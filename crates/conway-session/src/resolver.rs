//! `TranscriptResolver`: an agent's effective transcript, computed by
//! walking the ancestry chain and applying `origin.at_seq` truncation, with
//! allocations shared across siblings (architecture §5.1, §7 "Module:
//! conway-session").
//!
//! ## Algorithm
//!
//! `resolve(sid)` resolves to `resolve_prefix(sid, store.head(sid))`.
//! Every bound in this module — `ForkOrigin.at_seq` and each recursion
//! level's `upto` — is a LOCAL index into that session's OWN records (the
//! same units as `store.head` and `fork`'s range check), never an index
//! into the effective transcript. The inherited prefix always flows
//! through in full (a fork inherits the forker's ENTIRE context up
//! to the fork point). Conceptually:
//!
//! ```text
//! prefix(sid, upto_local) =
//!     (if origin(sid): prefix(origin.parent, origin.at_seq) else [])
//!     ++ own_records(sid)[0..upto_local]
//! ```
//!
//! Cycle-1 review (Critical, F-049-1): an earlier draft interpreted these
//! bounds as effective-transcript indexes while `fork` range-checked them
//! as local counts — the units conflation made it impossible for a fork to
//! capture a non-root parent's true tip (silent truncation of the parent's
//! own records). Local units everywhere resolves it: `fork(parent,
//! store.head(parent))` now inherits the parent's full effective
//! transcript by construction, at every tree depth.
//!
//! Rather than recursing (an `async fn` cannot recurse without boxing every
//! frame), `resolve_prefix` first walks the ancestry chain **upward**,
//! collecting `(sid, upto)` pairs, stopping at the first of: a cache hit, a
//! root session (`origin == None`), or a cycle/depth-limit violation. It
//! then folds **downward** from that stopping point, computing and
//! memoizing each level's `Arc<[LogRecord]>` in turn — so a shared ancestor
//! is resolved (and memoized) exactly once, and every descendant that reuses
//! it receives a clone of the same `Arc` (same backing allocation,
//! `Arc::ptr_eq`-equal).
//!
//! ## Memoization
//!
//! Cache key: `(SessionId, LogSeq)`, where the `LogSeq` is the *exclusive upper
//! bound* of the resolved prefix, measured in LOCAL units over the session's
//! OWN records (matching `store.head`). A full resolve of `sid` at local head
//! `H` is exactly the prefix entry `(sid, H)` — one keyspace serves both "full
//! transcript" and "prefix" lookups; there is no separate "full" sentinel.
//! Entries are immutable snapshots and are never invalidated by a parent's
//! later appends: appending to `sid` only makes new, higher-bound keys
//! reachable, so an already-memoized `(sid, at_seq)` stays correct forever
//! (this is what makes the snapshot invariant free).
//!
//! ## Cycle / depth guard
//!
//! While walking upward, a `SessionId` repeated in the current walk, or a
//! walk exceeding `MAX_ANCESTRY_DEPTH` hops, is reported as corrupt
//! ancestry rather than looping or overflowing the stack. `conway-core`'s
//! `StoreError` has no `CorruptAncestry` variant (the spec names one that
//! was never added to the core error enum); this is reported instead as
//! `StoreError::Corrupt { session, line: 0, detail }` — `line: 0` is a
//! reconciliation stand-in (there is no single offending line; the corrupt
//! *ancestry link*, not a line, is the defect) and `detail` names the cycle
//! or the depth bound.
//!
//! ## Context mask (WI-125)
//!
//! `LogRecord::ContextMask { target_seq, excluded, .. }` is a persisted
//! overlay, not a deletion: `target_seq` names another record in the SAME
//! session (local units, as above), and the latest `ContextMask` for a given
//! `target_seq` — by append order — decides whether that record is included
//! in the effective transcript. `apply_context_mask` filters each level's `own`
//! slice by this rule *before* it is combined with the (already-filtered)
//! inherited prefix, so the mask is folded into the same memoized
//! `Arc<[LogRecord]>` as everything else in this module: a fork's inherited
//! prefix is masked exactly as of the parent's state at the fork point, and
//! the parent's later mask/un-mask appends only ever affect keys strictly
//! above what the fork already captured — the same "snapshot invariant" the
//! Memoization section above describes for ordinary appends.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use lru::LruCache;

use conway_core::error::StoreError;
use conway_core::ids::{LogSeq, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::SessionStore;

/// Ancestry walks longer than this are reported as corrupt rather than
/// followed indefinitely — see the module-level "Cycle / depth guard" docs.
const MAX_ANCESTRY_DEPTH: usize = 256;

/// Memoization cache key: a session and the exclusive upper bound (measured
/// over its *effective* transcript) of the resolved prefix — see the
/// module-level "Memoization" docs.
type CacheKey = (SessionId, LogSeq);

/// Computes and memoizes effective transcripts. Cheap to clone is *not*
/// provided (there is exactly one instance per store in practice); the type
/// itself is `Send + Sync` so it can be shared behind an `Arc` by callers
/// that need to.
pub struct TranscriptResolver {
    cache: Mutex<LruCache<CacheKey, Arc<[LogRecord]>>>,
}

impl std::fmt::Debug for TranscriptResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptResolver").finish_non_exhaustive()
    }
}

fn corrupt_ancestry(session: SessionId, detail: impl Into<String>) -> StoreError {
    StoreError::Corrupt {
        session,
        line: 0,
        detail: detail.into(),
    }
}

/// Applies WI-125's context-exclusion mask to one session's own records
/// (never the inherited prefix -- see the call site): `ContextMask::target_seq`
/// is local to the session that owns both the mask and its target, the same
/// units this module uses everywhere, so masking only ever needs to look
/// within `own`, not across the ancestry.
///
/// Later records win when a `target_seq` is masked more than once (`own` is
/// already in seq order, so a linear scan suffices). `ContextMask` records
/// themselves are left in place -- same precedent as `Header`-adjacent kinds
/// like `ContextReportRecord`, which already flows through `resolve_prefix`
/// unfiltered and is dropped downstream (context/builder.rs, WI-126) by kind
/// rather than by the resolver.
fn apply_context_mask(own: Vec<LogRecord>) -> Vec<LogRecord> {
    let mut excluded: HashSet<LogSeq> = HashSet::new();
    for rec in &own {
        if let LogRecord::ContextMask {
            target_seq,
            excluded: is_excluded,
            ..
        } = rec
        {
            if *is_excluded {
                excluded.insert(*target_seq);
            } else {
                excluded.remove(target_seq);
            }
        }
    }
    if excluded.is_empty() {
        return own;
    }
    own.into_iter()
        .filter(|rec| !matches!(rec.seq(), Some(seq) if excluded.contains(&seq)))
        .collect()
}

impl TranscriptResolver {
    /// Builds a resolver with an LRU cache of `capacity` entries (entry
    /// count, not bytes). `capacity` is clamped to at least 1.
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::new(1).unwrap());
        Self {
            cache: Mutex::new(LruCache::new(cap)),
        }
    }

    fn cache_get(&self, sid: SessionId, upto: LogSeq) -> Option<Arc<[LogRecord]>> {
        self.cache.lock().unwrap().get(&(sid, upto)).cloned()
    }

    fn cache_put(&self, sid: SessionId, upto: LogSeq, value: Arc<[LogRecord]>) {
        self.cache.lock().unwrap().put((sid, upto), value);
    }

    /// Test-only accessor: returns the memoized prefix for `(sid, at_seq)`
    /// without computing it, and without disturbing LRU recency (backed by
    /// `LruCache::peek`). Used to assert sibling sharing via `Arc::ptr_eq`.
    #[doc(hidden)]
    pub fn peek_prefix(&self, sid: &SessionId, at_seq: LogSeq) -> Option<Arc<[LogRecord]>> {
        self.cache.lock().unwrap().peek(&(*sid, at_seq)).cloned()
    }

    /// Resolves `sid`'s effective transcript at its current head: for a
    /// root session, its own records in seq order; for a fork child, the
    /// parent's resolved prefix (`0..origin.at_seq`) concatenated with its
    /// own records, applied recursively up the ancestry chain.
    ///
    /// Generic over any [`SessionStore`] implementation — including through
    /// a `&dyn SessionStore`, since the trait is object-safe — rather than
    /// tied to `JsonlSessionStore` specifically, so non-store callers (e.g.
    /// the runtime) can resolve against whatever store they were handed.
    /// `&JsonlSessionStore` satisfies this bound directly, so store-owning
    /// callers pay no extra syntax at the call site.
    ///
    /// `upto` for a full resolve is `store.head(sid)` — the session's own
    /// local record count. Bounds are local units everywhere in this
    /// module (see the module docs and F-049-1): the inherited prefix
    /// flows through in full at each level, so the effective transcript's
    /// length emerges from the recursion rather than being computed here.
    pub async fn resolve<S>(
        &self,
        store: &S,
        sid: &SessionId,
    ) -> Result<Arc<[LogRecord]>, StoreError>
    where
        S: SessionStore + ?Sized,
    {
        let upto = store.head(sid).await?;
        self.resolve_prefix(store, sid, upto).await
    }

    /// Resolves `sid`'s effective transcript up to (not including) the
    /// LOCAL bound `upto` — `sid`'s own records past `upto` are never read.
    /// `resolve` is just this method called with `upto = store.head(sid)`.
    ///
    /// Exposed publicly (WI-119) for a caller that needs a session's
    /// inherited-only prefix as it stood at a specific ancestor bound,
    /// distinct from `resolve`'s "full effective transcript at the current
    /// head" — e.g. resolving a fork child's `InheritedPrefix` when the
    /// child already has run turns of its own (so `resolve(child)` would
    /// fold the child's own records into the result, double-counting them
    /// against an `AgentLoop` that also reads its own session's records
    /// separately every turn). Calling `resolve_prefix(store, &origin.parent,
    /// origin.at_seq)` for such a child returns exactly the parent's prefix,
    /// with none of the child's own records mixed in — the same value
    /// `subagent.rs`'s live fork path gets via `resolve(store, &child)` at
    /// the one moment (immediately after `store.fork`, before the child's
    /// own head record is appended) that shortcut is valid.
    pub async fn resolve_prefix<S>(
        &self,
        store: &S,
        sid: &SessionId,
        upto: LogSeq,
    ) -> Result<Arc<[LogRecord]>, StoreError>
    where
        S: SessionStore + ?Sized,
    {
        // Walk the ancestry upward, collecting the (sid, upto) pairs that
        // still need to be computed, stopping at the first cache hit, root
        // session, or corrupt-ancestry condition.
        let mut chain: Vec<(SessionId, LogSeq)> = Vec::new();
        let mut visited: HashSet<SessionId> = HashSet::new();
        let mut cur_sid = *sid;
        let mut cur_upto = upto;

        let base: Arc<[LogRecord]> = loop {
            if let Some(hit) = self.cache_get(cur_sid, cur_upto) {
                break hit;
            }
            if !visited.insert(cur_sid) {
                return Err(corrupt_ancestry(
                    cur_sid,
                    format!("cycle detected in fork ancestry (revisited session {cur_sid})"),
                ));
            }
            if visited.len() > MAX_ANCESTRY_DEPTH {
                return Err(corrupt_ancestry(
                    cur_sid,
                    format!("fork ancestry exceeds max depth ({MAX_ANCESTRY_DEPTH})"),
                ));
            }

            let meta = store.meta(&cur_sid).await?;
            chain.push((cur_sid, cur_upto));
            match meta.origin {
                Some(origin) => {
                    cur_sid = origin.parent;
                    cur_upto = origin.at_seq;
                }
                None => break Arc::from(Vec::new()),
            }
        };

        // Fold downward: `chain` is target-first / root-last, so folding in
        // reverse computes the topmost pending level first and the
        // originally requested (sid, upto) last.
        let mut prefix = base;
        for (level_sid, level_upto) in chain.into_iter().rev() {
            // Local units (F-049-1): the whole inherited prefix, then this
            // level's own records up to the local bound. Never slice the
            // prefix — the inheritance boundary is the parent's at_seq,
            // already applied one level up.
            let result: Arc<[LogRecord]> = if level_upto == LogSeq::ZERO {
                Arc::clone(&prefix)
            } else {
                let own = store
                    .read(&level_sid, SeqRange::new(LogSeq::ZERO, Some(level_upto)))
                    .await?;
                let own = apply_context_mask(own);
                let mut combined = Vec::with_capacity(prefix.len() + own.len());
                combined.extend(prefix.iter().cloned());
                combined.extend(own);
                Arc::from(combined)
            };
            self.cache_put(level_sid, level_upto, Arc::clone(&result));
            prefix = result;
        }

        Ok(prefix)
    }
}
