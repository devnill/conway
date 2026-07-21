//! `TranscriptResolver`: an agent's effective transcript, computed by
//! walking the ancestry chain and applying `origin.at_seq` truncation, with
//! allocations shared across siblings (architecture §5.1, §7 "Module:
//! conway-session").
//!
//! This file is a skeleton only. WI-049 implements the bounded-LRU
//! memoized resolution algorithm.

use std::sync::Arc;

use conway_core::error::StoreError;
use conway_core::ids::SessionId;
use conway_core::log::LogRecord;

use crate::store::JsonlSessionStore;

/// Computes and memoizes effective transcripts. Cheap to clone (`Arc`
/// interior, per WI-049); `JsonlSessionStore` owns one instance and exposes
/// it via `store.resolver()`.
#[derive(Debug)]
pub struct TranscriptResolver;

impl TranscriptResolver {
    /// Builds a resolver with an LRU cache of `capacity` entries (entry
    /// count, not bytes). Implemented by WI-049.
    pub fn new(_capacity: usize) -> Self {
        todo!("WI-049: TranscriptResolver::new")
    }

    /// Resolves `sid`'s effective transcript: for a root session, its own
    /// records in seq order; for a fork child, the parent's resolved
    /// prefix (`0..origin.at_seq`) concatenated with its own records,
    /// applied recursively up the ancestry chain. Implemented by WI-049.
    pub async fn resolve(
        &self,
        _store: &JsonlSessionStore,
        _sid: &SessionId,
    ) -> Result<Arc<[LogRecord]>, StoreError> {
        todo!("WI-049: TranscriptResolver::resolve")
    }
}
