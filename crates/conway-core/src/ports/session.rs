//! The `SessionStore` port (architecture §4.4).
//!
//! MVP impl: `JsonlSessionStore` in `conway-session` — one `.jsonl` per
//! session, first line the header. Debuggable with `jq`, greppable,
//! diffable, and trivially inspectable by a human (decision 9).

use async_trait::async_trait;

use crate::error::StoreError;
use crate::ids::{LogSeq, SeqRange, SessionId};
use crate::log::{LogRecord, SessionFilter, SessionMeta};

#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, StoreError>;

    async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, StoreError>;

    async fn read(&self, sid: &SessionId, range: SeqRange) -> Result<Vec<LogRecord>, StoreError>;

    async fn head(&self, sid: &SessionId) -> Result<LogSeq, StoreError>;

    /// Writes exactly one header line; copies zero records. O(1) in parent
    /// transcript size regardless of how many records the parent holds —
    /// this is what makes tournament patterns (one fork → N spawned
    /// children) affordable (architecture §5.1, §8).
    async fn fork(
        &self,
        parent: &SessionId,
        at: LogSeq,
        meta: SessionMeta,
    ) -> Result<SessionId, StoreError>;

    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, StoreError>;

    async fn children(&self, sid: &SessionId) -> Result<Vec<SessionId>, StoreError>;

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>, StoreError>;

    /// Permanently deletes a session and its backing storage — the purge
    /// primitive consumed by `/ask` modal discard, pull-in, and NL-intent
    /// classification. Purge is the single user-explicit exception to
    /// mandatory provenance retention (P-2/GP-10), so implementations
    /// enforce a narrow guard matrix:
    ///
    /// - REFUSES (`StoreError::NotRemovable`) unless the session's header
    ///   marks it `ephemeral` — purge exists for ephemeral sessions only.
    /// - REFUSES when the session has ANY children, ephemeral ones
    ///   included. The check must go through
    ///   `list(SessionFilter { parent: Some(sid), include_ephemeral: true,
    ///   .. })`, never `children()`, which hides ephemeral children and
    ///   would orphan them.
    /// - The FACADE layer additionally refuses when the session is live in
    ///   `Runtime::tree()`. That check deliberately does NOT live at this
    ///   layer (the store has no view of the runtime's live tree); callers
    ///   in `conway`/`conway-cli` enforce it before calling `remove`.
    ///
    /// Concurrency contract: implementations serialize `remove` against
    /// `fork`/`create` such that a racing fork of the removed session can
    /// never produce an orphaned child with dangling provenance — the
    /// remove either sees the new child and refuses, or completes first
    /// and the fork fails `NotFound`. An `append` in flight across the
    /// removal must not report success after `remove` has returned (a
    /// record acknowledged as stored must never be silently discarded).
    /// In-flight `read`/`head`/`meta` calls that already hold the session
    /// handle may still complete with pre-removal data (they linearize at
    /// handle acquisition); only `append` is guaranteed to fail once
    /// `remove` has returned.
    ///
    /// Returns `StoreError::NotFound` if the session does not exist.
    async fn remove(&self, sid: &SessionId) -> Result<(), StoreError>;

    /// Flips a session header's `ephemeral` flag — the store primitive
    /// behind the facade's ephemeral→persistent promote (the `/ask` modal's
    /// "keep" fate, B3). This is the ONE sanctioned exception to the
    /// session file's write-once header discipline, and it is narrow:
    ///
    /// - REFUSES (`StoreError::NotPromotable`) any `ephemeral: true`
    ///   request. Demotion (persistent→ephemeral) does not exist: promotion
    ///   is one-way, so a persistent record can never silently become
    ///   purge-eligible scratchpad (P-2).
    /// - REFUSES (`NotPromotable`) when the session is not currently
    ///   ephemeral — a false→false no-op would silently mask a double
    ///   promote or a caller bug.
    /// - Returns `StoreError::NotFound` if the session does not exist.
    ///
    /// On success, the persisted header AND every derived view the
    /// implementation maintains (in-memory meta, `children`/`list`
    /// accelerators) reflect the flip before this returns — a successful
    /// return means a previously catalog-hidden session is now visible to
    /// default `list`/`children` queries.
    ///
    /// Concurrency contract: implementations serialize `set_ephemeral`
    /// against `create`/`fork`/`remove` with the same lifecycle
    /// serialization `remove`'s contract describes, so a promote and a
    /// purge of the same session can never both succeed — one linearizes
    /// first (the purge then fails `NotFound`, or the promote lands first
    /// and the purge fails `NotRemovable` on the flipped header).
    ///
    /// Note the FACADE-layer live check is NOT part of this contract: the
    /// store has no view of the runtime's live tree, exactly as `remove`'s
    /// guard matrix documents for its own (inverse) live check. `conway`'s
    /// `Conway::promote` requires the agent to be present in
    /// `Runtime::tree()` before calling this; the store-level primitive
    /// itself also works on a cold (not currently live) session.
    async fn set_ephemeral(&self, sid: &SessionId, ephemeral: bool) -> Result<(), StoreError>;
}
