//! `SessionIndex`: the derived, rebuildable index that accelerates `list`,
//! `children`, and tree reconstruction — never a source of truth
//! (architecture §7 "Module: conway-session").
//!
//! This file is a skeleton only. WI-050 implements rebuild-by-scan, the
//! in-memory `by_id`/`children` maps, and the append-only `index.jsonl`
//! projection. `JsonlSessionStore::children`/`::list` (WI-047) delegate to
//! this type verbatim; `fork_impl` (WI-048) calls `record_header` as a
//! no-op today so WI-050 needs no edit to `fork.rs`.

use std::path::Path;

use conway_core::error::StoreError;
use conway_core::ids::SessionId;
use conway_core::log::{SessionFilter, SessionMeta};

/// The derived, rebuildable session index. Implemented by WI-050.
#[derive(Debug)]
pub struct SessionIndex;

impl SessionIndex {
    /// Loads `root/index.jsonl`, or rebuilds it by scanning `root/*.jsonl`
    /// (excluding `index.jsonl`) if it is absent, corrupt, or inconsistent
    /// with the session files on disk. Implemented by WI-050.
    ///
    /// `#[allow(dead_code)]`: not yet called by `JsonlSessionStore::open`
    /// (WI-047 owns that call site). Remove the attribute when it lands.
    #[allow(dead_code)]
    pub(crate) async fn load_or_rebuild(_root: &Path) -> Result<Self, StoreError> {
        todo!("WI-050: SessionIndex::load_or_rebuild")
    }

    /// Records a newly written header in the in-memory index and appends
    /// one line to `index.jsonl` (best-effort — index I/O errors are
    /// logged at WARN and never propagate). Implemented by WI-050.
    ///
    /// `#[allow(dead_code)]`: called by `fork_impl` (WI-048) once that item
    /// lands; unused until then.
    #[allow(dead_code)]
    pub(crate) fn record_header(&self, _meta: &SessionMeta) {
        todo!("WI-050: SessionIndex::record_header")
    }

    /// Fsyncs `index.jsonl`. Called on store drop and by the interval
    /// flusher (WI-047). Implemented by WI-050.
    #[allow(dead_code)]
    pub(crate) async fn flush(&self, _root: &Path) -> Result<(), StoreError> {
        todo!("WI-050: SessionIndex::flush")
    }

    /// Sessions whose header `origin.parent == sid`, ascending `created`
    /// order. Implemented by WI-050.
    #[allow(dead_code)]
    pub(crate) fn children(&self, _sid: &SessionId) -> Vec<SessionId> {
        todo!("WI-050: SessionIndex::children")
    }

    /// Sessions matching `f`, AND-composed across `parent`/`status`/`label`,
    /// `limit` applied after filtering and ordering, descending `created`
    /// with ties broken by ascending `id`. Implemented by WI-050.
    #[allow(dead_code)]
    pub(crate) fn list(&self, _f: &SessionFilter) -> Vec<SessionMeta> {
        todo!("WI-050: SessionIndex::list")
    }
}
