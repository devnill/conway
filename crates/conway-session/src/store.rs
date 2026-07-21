//! `JsonlSessionStore`: append-only, one-file-per-session backing storage
//! (architecture §4.4, §7 "Module: conway-session").
//!
//! This file is a skeleton only. WI-047 fills in `open`/`open_with`, the
//! `SessionStore` trait impl (`create`/`append`/`read`/`head`/`meta`), the
//! fsync policy, and crash-tolerant reads; it delegates `fork` to
//! [`crate::fork::fork_impl`] and `children`/`list` to
//! [`crate::index::SessionIndex`] without needing to touch this file again
//! for those two methods' bodies.
//!
//! The exact signatures below are fixed by WI-046 so that WI-047 is the
//! only downstream item that edits this file.

use std::path::PathBuf;

use conway_core::error::StoreError;

/// One `.jsonl`-per-session, append-only session store. Implemented by
/// WI-047.
#[derive(Debug)]
pub struct JsonlSessionStore;

/// Store-wide configuration: fsync policy and the `TranscriptResolver`
/// LRU capacity. Default (WI-047) is `{ fsync: Interval(200ms), lru_capacity: 64 }`.
#[derive(Debug)]
pub struct StoreConfig;

/// Durability policy for header writes and `append`. Fully defined by
/// WI-047 as `Always | Interval(Duration) | Never`, serde `snake_case`,
/// with `interval` carrying a humantime duration string.
#[derive(Debug)]
pub enum FsyncPolicy {}

impl JsonlSessionStore {
    /// Opens `root`, creating it recursively if absent. Implemented by
    /// WI-047.
    pub async fn open(_root: PathBuf) -> Result<Self, StoreError> {
        todo!("WI-047: JsonlSessionStore::open")
    }

    /// As [`open`](Self::open), with an explicit [`StoreConfig`].
    /// Implemented by WI-047.
    pub async fn open_with(_root: PathBuf, _cfg: StoreConfig) -> Result<Self, StoreError> {
        todo!("WI-047: JsonlSessionStore::open_with")
    }
}
