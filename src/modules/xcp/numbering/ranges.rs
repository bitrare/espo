use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Endpoints are inclusive and directional. [5, 3] means the stack [5, 4, 3].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub first: u64,
    pub last: u64,
}
impl Run {
    pub fn len(&self) -> u64 {
        self.first.abs_diff(self.last) + 1
    }
    pub fn contains(&self, serial: u64) -> bool {
        serial >= self.first.min(self.last) && serial <= self.first.max(self.last)
    }
    fn at(&self, offset: u64) -> u64 {
        if self.last >= self.first { self.first + offset } else { self.first - offset }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stack(pub Vec<Run>);
impl Stack {
    pub fn len(&self) -> u64 {
        self.0.iter().map(Run::len).sum()
    }
    pub fn append(&mut self, other: Self) {
        for run in other.0 {
            if let Some(tail) = self.0.last_mut() {
                let up = tail.last >= tail.first
                    && run.last >= run.first
                    && tail.last.checked_add(1) == Some(run.first);
                let down = tail.last <= tail.first
                    && run.last <= run.first
                    && run.first.checked_add(1) == Some(tail.last);
                if up || down {
                    tail.last = run.last;
                    continue;
                }
            }
            self.0.push(run);
        }
    }
    pub fn take(&mut self, quantity: u64, reverse: bool) -> Result<Self> {
        ensure!(
            quantity <= self.len(),
            "stack underflow: requested {quantity}, available {}",
            self.len()
        );
        let mut left = quantity;
        let mut result = Vec::new();
        while left > 0 {
            let mut run = self.0.pop().expect("length checked");
            let take = left.min(run.len());
            let first = run.at(run.len() - take);
            result.push(Run { first, last: run.last });
            if take < run.len() {
                run.last = run.at(run.len() - take - 1);
                self.0.push(run);
            }
            left -= take;
        }
        result.reverse();
        if reverse {
            result.reverse();
            for run in &mut result {
                std::mem::swap(&mut run.first, &mut run.last);
            }
        }
        Ok(Self(result))
    }
    pub fn api(&self) -> serde_json::Value {
        serde_json::json!(self.0.iter().map(|r| serde_json::json!({"first":r.first.to_string(),"last":r.last.to_string(),"quantity":r.len().to_string()})).collect::<Vec<_>>())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn expanded(s: &Stack) -> Vec<u64> {
        s.0.iter().flat_map(|r| (0..r.len()).map(|i| r.at(i))).collect()
    }
    #[test]
    fn split_reverse_matches_vector_model() {
        for seed in 1..30u64 {
            let mut s = Stack(vec![Run { first: 1, last: 1_000 }]);
            let mut v: Vec<u64> = (1..=1_000).collect();
            for i in 0..100 {
                let q = ((seed * (i + 1) * 17) % (v.len() as u64 + 1)) as usize;
                let reverse = i % 3 != 0;
                let mut tail = v.split_off(v.len() - q);
                if reverse {
                    tail.reverse();
                }
                let taken = s.take(q as u64, reverse).unwrap();
                assert_eq!(expanded(&taken), tail);
                assert_eq!(expanded(&s), v);
                s.append(taken);
                v.extend(tail);
                assert_eq!(expanded(&s), v);
            }
        }
    }
    #[test]
    fn enormous_supply_stays_compressed_and_underflow_is_atomic() {
        let mut s = Stack(vec![Run { first: 1, last: 9_000_000_000_000_000_000 }]);
        let before = s.clone();
        assert!(s.take(9_000_000_000_000_000_001, true).is_err());
        assert_eq!(s, before);
        assert_eq!(
            s.take(2, true).unwrap().0,
            vec![Run { first: 9_000_000_000_000_000_000, last: 8_999_999_999_999_999_999 }]
        );
        assert_eq!(s.0.len(), 1);
    }
}
