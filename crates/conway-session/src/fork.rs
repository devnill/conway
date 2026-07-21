//! O(1) fork-by-reference (architecture §5.1, §8).
//!
//! This file is a skeleton only. WI-048 implements `fork_impl`: a single
//! header write that references the parent by `(parent, at_seq, mode)` and
//! copies nothing. `JsonlSessionStore::fork` (WI-047) delegates to this
//! function verbatim.

use conway_core::error::StoreError;
use conway_core::ids::{LogSeq, SessionId};
use conway_core::log::SessionMeta;

use crate::store::JsonlSessionStore;

/// Creates `child` as a fork of `parent` at `at`, writing exactly one
/// header line. Implemented by WI-048.
///
/// `#[allow(dead_code)]`: nothing calls this yet — `JsonlSessionStore::fork`
/// (WI-047) is itself a not-yet-written trait method that will delegate
/// here. Remove the attribute when that call site lands.
#[allow(dead_code)]
pub(crate) async fn fork_impl(
    _store: &JsonlSessionStore,
    _parent: &SessionId,
    _at: LogSeq,
    _meta: SessionMeta,
) -> Result<SessionId, StoreError> {
    todo!("WI-048: fork_impl")
}
