//! The `SessionStore` port (architecture §4.4).
//!
//! MVP impl: `JsonlSessionStore` in `conway-session` — one `.jsonl` per
//! session, first line the header. Debuggable with `jq`, greppable,
//! diffable, and trivially inspectable by a human (decision 9).

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::StoreError;
use crate::ids::{LogSeq, SeqRange, SessionId};
use crate::log::{LogRecord, SessionFilter, SessionMeta};

/// Cross-process liveness marker for a session-store directory — which
/// process is currently using this store, and when it last said so (S1
/// follow-up to B5's `sweep_stale_modal_asks`).
///
/// The store directory is a shared resource: nothing stops two TUI processes
/// from pointing at the same `root`. B5's startup sweep decides "not live"
/// by checking only THIS process's `Runtime::tree()`, so a second process
/// starting against a store the first is actively using would purge the
/// first's open modal-ask child as "residue". This marker closes that gap:
/// the sweep first asks the store whether ANOTHER process owns it, and
/// defers entirely (reaps nothing) while a fresh owner is present.
///
/// `pid` is the owning process's OS pid (diagnostic — the liveness decision
/// is made from `heartbeat` freshness, not from a `kill(0)` check; see the
/// `liveness_rule` note on `SessionStore::live_owner`). `heartbeat` is the
/// wall-clock time the owner last refreshed the marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveOwner {
    pub pid: u32,
    pub heartbeat: DateTime<Utc>,
}

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
    /// mandatory provenance retention, so implementations
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
    ///   purge-eligible scratchpad.
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

    /// Reads this store directory's cross-process liveness marker — the
    /// [`LiveOwner`] a process published via [`touch_live_owner`], or `None`
    /// when no marker is present (no process ever claimed this store, or the
    /// last owner cleared it on shutdown). Implementations MUST return `None`
    /// — not an error — for a missing marker AND for a marker that fails to
    /// decode (a half-written / corrupt sidecar): a corrupt liveness file
    /// is not a corrupt session, and the safest reading of "I can't tell
    /// whether anyone is alive" is "behave as if nobody is" (reap residue,
    /// the same as a cold-started store with no marker). IO errors other than
    /// "file not found" surface as [`StoreError::Io`].
    ///
    /// **liveness_rule:** this method returns the RAW marker; it does NOT
    /// decide freshness. The decision `now - heartbeat <= THRESHOLD` belongs
    /// to the caller (the sweep), so the threshold lives in one place and the
    /// store stays free of clock policy. A `kill(0)` pid-alive check is
    /// deliberately NOT part of this contract: freshness plus clean-shutdown
    /// removal already cover crash recovery and pid-reuse (a dead process
    /// stops heartbeating, so its marker goes stale regardless of whether
    /// its pid was later reused), and avoiding it keeps the port free of
    /// platform-specific process introspection.
    ///
    /// [`touch_live_owner`]: SessionStore::touch_live_owner
    async fn live_owner(&self) -> Result<Option<LiveOwner>, StoreError>;

    /// Publishes or refreshes THIS process's liveness marker — writes
    /// `{ pid, heartbeat: now }` to the store directory's sidecar. Called at
    /// TUI startup (AFTER the sweep, so the sweep never sees this process's
    /// own marker) and periodically by a heartbeat task. Idempotent and
    /// cheap (a small file, no transcript copy — deliberately a sidecar
    /// rather than a `SessionMeta` header field, which would require the
    /// heavy crash-atomic header rewrite per beat and contend with appends).
    async fn touch_live_owner(&self, pid: u32) -> Result<(), StoreError>;

    /// Removes THIS process's liveness marker — called on clean TUI shutdown
    /// so a subsequent cold start knows immediately that no owner is live.
    /// Best-effort: a missing marker (cleared already, or never written) is
    /// `Ok`, not `NotFound` — the marker is a cache of liveness, not a
    /// durable record, so its absence is the desired end state.
    async fn clear_live_owner(&self) -> Result<(), StoreError>;
}
