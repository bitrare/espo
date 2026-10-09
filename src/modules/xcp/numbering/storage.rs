use anyhow::{Result, ensure};
use rocksdb::{BlockBasedOptions, Cache, DB, IteratorMode, Options, WriteBatch, WriteOptions};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, path::Path};

pub fn open(path: &Path) -> Result<DB> {
    // A marker prevents accidentally opening an existing Espo/Core database.
    std::fs::create_dir_all(path)?;
    let marker = path.join("XCP_HISTORY_ONLY");
    if !marker.exists() {
        ensure!(
            std::fs::read_dir(path)?.next().is_none(),
            "history directory must be empty or contain XCP_HISTORY_ONLY"
        );
        std::fs::write(&marker, "espo-xcp-history schema 1\n")?;
    }
    ensure!(
        std::fs::read_to_string(&marker)? == "espo-xcp-history schema 1\n",
        "unsupported history schema; rebuild XCP in a new directory"
    );
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.set_max_background_jobs(2);
    opts.set_write_buffer_size(32 * 1024 * 1024);
    opts.set_max_write_buffer_number(2);
    let mut table = BlockBasedOptions::default();
    table.set_block_cache(&Cache::new_lru_cache(128 * 1024 * 1024));
    opts.set_block_based_table_factory(&table);
    Ok(DB::open(&opts, path)?)
}
pub fn read<T: DeserializeOwned>(db: &DB, key: &str) -> Result<Option<T>> {
    db.get(key)?.map(|v| serde_json::from_slice(&v).map_err(Into::into)).transpose()
}
pub fn put<T: Serialize>(batch: &mut WriteBatch, key: &str, value: &T) -> Result<()> {
    batch.put(key, serde_json::to_vec(value)?);
    Ok(())
}
pub fn commit(db: &DB, batch: WriteBatch) -> Result<()> {
    let mut opts = WriteOptions::default();
    opts.set_sync(true);
    db.write_opt(batch, &opts)?;
    Ok(())
}
/// A complete projected block is a transaction including its checkpoint and undo.
pub struct BlockTx<'a> {
    pub db: &'a DB,
    pub changes: BTreeMap<String, Option<Vec<u8>>>,
}
impl<'a> BlockTx<'a> {
    pub fn new(db: &'a DB) -> Self {
        Self { db, changes: BTreeMap::new() }
    }
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.changes.get(key) {
            Some(v) => {
                v.as_ref().map(|v| serde_json::from_slice(v).map_err(Into::into)).transpose()
            }
            None => read(self.db, key),
        }
    }
    pub fn set<T: Serialize>(&mut self, key: String, value: &T) -> Result<()> {
        self.changes.insert(key, Some(serde_json::to_vec(value)?));
        Ok(())
    }
    pub fn delete(&mut self, key: String) {
        self.changes.insert(key, None);
    }
    pub fn prefix(&self, prefix: &str) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        for entry in self
            .db
            .iterator(IteratorMode::From(prefix.as_bytes(), rocksdb::Direction::Forward))
        {
            let (k, v) = entry?;
            if !k.starts_with(prefix.as_bytes()) {
                break;
            }
            out.insert(String::from_utf8(k.to_vec())?, v.to_vec());
        }
        for (k, v) in self.changes.range(prefix.to_string()..) {
            if !k.starts_with(prefix) {
                break;
            }
            match v {
                Some(v) => {
                    out.insert(k.clone(), v.clone());
                }
                None => {
                    out.remove(k);
                }
            }
        }
        Ok(out)
    }
    pub fn commit(self, height: u64) -> Result<()> {
        let mut batch = WriteBatch::default();
        let mut undo = BTreeMap::new();
        for (key, value) in self.changes {
            undo.insert(key.clone(), self.db.get(&key)?);
            match value {
                Some(value) => batch.put(key, value),
                None => batch.delete(key),
            }
        }
        put(&mut batch, &format!("undo/{height:010}"), &undo)?;
        if height > 288 {
            batch.delete(format!("undo/{:010}", height - 288));
        }
        commit(self.db, batch)
    }
}
pub fn rollback(db: &DB, height: u64) -> Result<()> {
    let undo: BTreeMap<String, Option<Vec<u8>>> = read(db, &format!("undo/{height:010}"))?
        .ok_or_else(|| {
            anyhow::anyhow!("reorg beyond retained undo; rebuild XCP projection only")
        })?;
    let mut batch = WriteBatch::default();
    for (k, v) in undo {
        match v {
            Some(v) => batch.put(k, v),
            None => batch.delete(k),
        }
    }
    batch.delete(format!("undo/{height:010}"));
    commit(db, batch)
}

#[cfg(test)]
mod tests {
    #[test]
    fn refuses_to_open_an_existing_non_history_database() {
        let dir = tempfile::TempDir::new().unwrap();
        let sentinel = dir.path().join("CURRENT");
        std::fs::write(&sentinel, b"existing Espo database").unwrap();
        assert!(super::open(dir.path()).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"existing Espo database");
        assert!(!dir.path().join("XCP_HISTORY_ONLY").exists());
    }
}
