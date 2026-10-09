//! Isolated Counterparty history: no Espo runtime, shared MDB, or indexing globals.
pub mod archive;
pub mod engine;
pub mod ranges;
pub mod service;
pub mod storage;
pub const RULESET: &str = "nakamoto-stack-v1";
pub const SOURCE_COMMIT: &str = "cd827e0fafe62b3166b2a186749e4191e1897170";
pub const GENESIS: u64 = 278270;
