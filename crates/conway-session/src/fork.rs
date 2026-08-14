//! O(1) fork-by-reference (architecture §5.1, §8).
//!
//! `fork_impl` writes exactly one header line for the child and copies zero
//! parent records. `JsonlSessionStore::fork` delegates here
//! verbatim.
//!
//! ## Cost contract
//!
//! The only parent I/O `fork_impl` performs is a `store.head(parent)` call:
//! `NotFound` if the parent file does not exist, else the parent's current
//! head via its per-session handle. When that handle is already warm (the
//! runtime's actual usage — a fork always follows the parent agent having
//! already appended through the same live `JsonlSessionStore`), this is a
//! mutex lock and an `Arc` clone, not a file read. A *cold* parent handle
//! still requires `get_or_open_handle` to scan the file to recover its
//! records and head — that cost belongs to handle acquisition (amortized
//! store behavior every method pays once per session), not to `fork`
//! itself. `JsonlSessionStore::lines_scanned` instruments exactly this
//! distinction: it only advances on a cold-open scan, so a test that
//! pre-warms the parent handle and then calls `fork` can assert the counter
//! is unchanged.
//!
//! ## Error naming
//!
//! The work-item spec names the range-violation error `InvalidRange{ at,
//! head }`. `conway-core::error::StoreError` has no such variant; the
//! existing `SeqOutOfRange{ requested, head }` covers the identical case
//! (a requested seq beyond the head) and is used here instead — no new
//! `StoreError` variant is introduced for this item.
//!
//! ## Immutability semantics
//!
//! A fork is a snapshot, not a live view: records at `seq < at` are frozen
//! from the child's perspective forever, and parent appends at `seq >= at`
//! (including everything appended after the fork) are never visible to the
//! child. `fork_impl` enforces this simply by never reading or copying any
//! parent record — the child's own file starts, and stays, empty of them.

use conway_core::error::StoreError;
use conway_core::ids::{LogSeq, SessionId};
use conway_core::log::{ForkOrigin, SessionMeta, SubagentMode};
use conway_core::ports::SessionStore;

use crate::store::JsonlSessionStore;

/// Creates `child` as a fork of `parent` at `at`, writing exactly one
/// header line and copying zero parent records.
///
/// Procedure:
/// 1. `store.head(parent)` — `NotFound` if `parent` doesn't exist; O(1) in
///    parent size when the parent handle is already warm (see the
///    module-level cost contract).
/// 2. `at > head` → `StoreError::SeqOutOfRange{ requested: at, head }`, no
///    file created.
/// 3. Normalize `meta.origin` to `Some(ForkOrigin{ parent, at_seq: at,
///    mode })`: `mode` is the caller-supplied origin's mode when
///    `meta.origin` was `Some(..)`, else it defaults to
///    `SubagentMode::Fork` (this function backs the `fork`, not `spawn`,
///    path — a caller that omits an origin altogether is asking for a
///    plain fork). Any `parent`/`at_seq` the caller supplied in
///    `meta.origin` is discarded in favor of this call's own arguments.
/// 4. Delegate to `store.create`, the same header-writing path `create`
///    uses directly — one line, unconditional fsync regardless of
///    `FsyncPolicy` (satisfying "child header fsynced before fork returns
///    under all three policies"), zero records.
///
/// `SessionIndex::record_header` is deliberately not called here:
/// the delegation to `store.create` in step 4 already records the child's
/// header in the index (that call is `create`'s single wiring point), so
/// fork children are indexed with no edit to this file.
///
/// ## Serialization against `remove`
///
/// The store's `lifecycle` mutex (lock order documented on
/// `JsonlSessionStore`) is held across the head-check (step 1) AND the
/// create (step 4). `remove` holds the same mutex across its own
/// guard-check-plus-delete, so a `remove(parent)` racing this fork either
/// completes first — the head check then fails `NotFound` on the removal
/// tombstone — or starts after this fork returns, in which case the
/// remove's children check sees the new child and refuses. The pair can
/// never produce an orphaned child with dangling provenance (review
/// F-1). `create_inner` is called directly because `SessionStore::create`
/// would re-take the non-reentrant `lifecycle` mutex and self-deadlock.
///
/// Stall note: `lifecycle` is also held across the step-1 head check, so
/// a COLD parent handle — whose `get_or_open_handle` performs a full-file
/// scan (and possibly a `set_len` repair write) — blocks all concurrent
/// create/fork/remove store-wide for the scan's duration. In practice
/// forks follow warm parents (the parent agent just appended through the
/// same store), making this a cold-path-only cost.
pub(crate) async fn fork_impl(
    store: &JsonlSessionStore,
    parent: &SessionId,
    at: LogSeq,
    mut meta: SessionMeta,
) -> Result<SessionId, StoreError> {
    let _lifecycle = store.lifecycle.lock().await;

    let head = store.head(parent).await?;
    if at.0 > head.0 {
        return Err(StoreError::SeqOutOfRange {
            requested: at,
            head,
        });
    }

    let mode = meta
        .origin
        .as_ref()
        .map(|o| o.mode)
        .unwrap_or(SubagentMode::Fork);
    meta.origin = Some(ForkOrigin {
        parent: *parent,
        at_seq: at,
        mode,
    });

    let child = meta.id;
    store.create_inner(meta).await?;
    Ok(child)
}
