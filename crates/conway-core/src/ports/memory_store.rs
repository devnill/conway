//! The `MemoryStore` port (board item `01M09P2T8E5M292WMSMS64CVC4`, a
//! REWORK of the shipped `conway.memory` plugin).
//!
//! ## Why this exists: the seam the label-based design turned out not to be
//!
//! The plugin this port now backs used to be a [`Curator`](crate::ports::
//! Curator): `SessionMeta.labels` marked a whole session recallable, and
//! `MemoryCurator::curate` recalled its records verbatim via
//! `ValidatedPath::derive_with`. That worked only because `derive_with` can
//! reference records that already exist -- and freeform memory text does
//! not exist as a record anywhere. `CurateOutcome` is `{ Unchanged,
//! Derived(Derivation), Failed }`, and a `Derivation` can only be built
//! from `ValidatedPath::derive`/`derive_reordered`, which can only
//! reference nodes already on a resolved path. There is no variant that
//! can carry authored text with no backing record: the type system is not
//! an obstacle here, it is confirmation that path SELECTION was the wrong
//! mechanism for memory. Selection reorders/filters what already happened;
//! memory INJECTS content that did not come from any one record.
//!
//! The right seam is [`crate::ports::ContextHook::before_request`], which
//! runs POST-assembly over already-rendered [`crate::segment::
//! PromptSegment`]s and can add one carrying arbitrary authored text --
//! exactly what [`crate::provenance::Provenance::AgentDef`]/[`crate::
//! provenance::Provenance::Skill`] already do for a system prompt / skill
//! fragment that also never came from a logged record. This is a
//! deliberate departure from `crate::ports::curator`'s own module doc,
//! which argues `ContextHook` is the WRONG seam *for curation* -- and it
//! still is: curation edits *which records* end up on the path. Memory is
//! not curation; it is injection of content that was never a record in the
//! first place. The two module docs do not contradict each other; they are
//! about two different operations that happen to share a `Plugin`.
//!
//! ## Layering, mirrored from [`crate::ports::PathStore`] -- NOT reused
//!
//! Same two-tier shape `PathStore` established: a port here, a concrete
//! implementation in `conway-session` (`FsMemoryStore`, mirroring
//! `FsPathStore`'s directory-per-object layout). But `PathStore` itself is
//! the WRONG type to reuse, not merely a different one: it is write-once
//! and content-addressed over an expanded node list -- two properties a
//! memory has neither of. A memory is caller-assigned-id, freely mutable
//! (this port's whole point is that removal is first-class -- see
//! [`MemoryStore::remove`]), and never a selection over path nodes at all.
//!
//! ## Relationship to the record logs: NOT a cache of them
//!
//! Mirrors [`crate::ports::PathStore`]'s own disclosure: a `MemoryStore` is
//! a SEPARATE, independent store, not a derived index over
//! `SessionStore`'s append-only logs and not a rewrite of them. Record-log
//! immutability is completely untouched by this port -- nothing here ever
//! calls `SessionStore::append`/mutates a session's own records. A
//! [`Memory`] MAY carry a [`MemoryProvenance`] naming a source session (see
//! that type's own doc), but that is a REFERENCE for later lookup, not a
//! liveness dependency and not a cache entry: a memory whose named source
//! session was later purged is still a fully valid, independently stored
//! [`Memory`] -- [`MemoryStore::get`]/[`MemoryStore::list`] never consult
//! `SessionStore` at all, so there is nothing to fail even if the session
//! is gone.
//!
//! ## Scoping (open question 1, decided): global, not per-project/per-agent/tagged
//!
//! One `MemoryStore` instance is process-wide (or embedder-wide, however
//! the caller wires it) -- [`MemoryStore::list`] takes no scoping
//! parameter at all. `SessionMeta.labels`-style per-session marking already
//! proved the wrong unit of granularity (a whole 200-turn session is not
//! "one memory"); reintroducing scope as "per-project" or "per-agent"
//! would repeat the same mistake one level up -- an arbitrary CONTAINER
//! boundary standing in for the thing that actually varies, which is the
//! CONTENT of one memory. A tag lives on future work as a per-memory field
//! (this port's [`Memory`] struct does not add one), not a per-store
//! partition: the caller wiring `MemoryPlugin` decides how many
//! `MemoryStore` instances to construct and route to (one globally shared
//! one is the common case; an embedder wanting per-project isolation
//! constructs one `FsMemoryStore` per project root and installs a separate
//! `MemoryPlugin` per project's `ConwayBuilder`, no port change required).
//! Global-scope-by-default is deliberately narrow FIRST, not because
//! "everything I ever remembered, in every session" has no unbounded-
//! context problem (it does -- see [`crate::ports::plugin::ContextPayload`]'s
//! consumer, `conway-plugin-memory`'s own injection budget, for where that
//! cap actually lives), but because the injection-time BUDGET is the right
//! place to bound total injected text, not a storage-time partition that
//! would make "which memories exist" depend on which agent asked.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::MemoryStoreError;
use crate::ids::{MemoryId, SeqRange, SessionId};

/// One stored memory: freeform text plus optional provenance (board item
/// `01M09P2T8E5M292WMSMS64CVC4`, R1/R2).
///
/// **R1 -- freeform, no imposed structure.** `text` is an opaque `String`.
/// This port has no opinion about how it was produced -- a model-written
/// summary, a verbatim turn copied by a caller, a hand-typed operator note,
/// or another tool's output are all equally valid; distinguishing them is
/// WORKFLOW (`PHILOSOPHY.md`: "conway holds no opinions about how you
/// should work"), not something a memory's own shape encodes. There is no
/// summarisation anywhere behind this port -- no model call, no imposed
/// template.
///
/// **R2 -- provenance is optional and carried, never required.**
/// [`Self::provenance`] MAY name a source session (and, within it,
/// optionally a record range) so a caller can look up more context later.
/// It is `Option` because a hand-authored memory genuinely has none --
/// requiring one would silently reintroduce the record-backing constraint
/// this port exists to remove (see this module's own doc). A dangling
/// reference (the named session later purged) does not invalidate the
/// memory: provenance is a lookup hint, never a liveness dependency.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub id: MemoryId,
    pub text: String,
    pub created: DateTime<Utc>,
    pub provenance: Option<MemoryProvenance>,
}

/// Where a [`Memory`] came from, if the caller chose to record it (R2).
/// `range` further narrows to a specific record or span within `session`;
/// `None` means "somewhere in this session," not "the whole log verbatim"
/// -- this struct is a POINTER for later lookup, not a live selection, so
/// it never re-derives or re-validates against `SessionStore` on its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProvenance {
    pub session: SessionId,
    pub range: Option<SeqRange>,
}

/// A mutable, addressable, removable store of [`Memory`] (board item
/// `01M09P2T8E5M292WMSMS64CVC4`, R3). Object-safe by construction (no
/// generic params, `#[async_trait]`), so a plugin can hold `Arc<dyn
/// MemoryStore>` exactly as `CurateCtx` holds `Arc<dyn SessionStore>`.
///
/// **Removal is first-class, not an afterthought.** The label-based design
/// this port replaces had no removal at all and instead capped recall at
/// 8 records / 8192 bytes -- documented as "bounded by construction," which
/// in hindsight was the growth problem wearing a virtue's clothes: a cap
/// with no removal only ever discards the OLDEST content, never the
/// content a caller actually decided no longer mattered. [`Self::remove`]
/// is this port's answer: a caller (typically a `forget`-shaped tool) can
/// retire exactly one memory by its own [`MemoryId`], no bulk truncation
/// implied.
#[async_trait]
pub trait MemoryStore: Send + Sync + 'static {
    /// Store `memory` under its own `memory.id`, assigned by the caller
    /// before calling this (mirrors `SessionStore::create(meta)`, which
    /// likewise takes an already-`SessionId`-bearing value rather than
    /// generating one itself). A second `put` reusing an existing id is
    /// [`MemoryStoreError::AlreadyExists`] -- `put` never silently
    /// overwrites; a caller that wants to replace a memory's text removes
    /// the old id and puts a new one, keeping "what changed" inspectable
    /// as an explicit remove+put pair rather than a hidden mutation.
    async fn put(&self, memory: Memory) -> Result<(), MemoryStoreError>;

    /// Fetch the memory stored under `id`. Absent ->
    /// [`MemoryStoreError::NotFound`].
    async fn get(&self, id: &MemoryId) -> Result<Memory, MemoryStoreError>;

    /// Every currently-stored memory, in NO particular guaranteed order --
    /// a caller that needs a specific order (e.g. deterministic,
    /// budget-respecting injection) sorts the result itself. See
    /// `conway-plugin-memory`'s own injection hook for the oldest-first,
    /// cap-stopping walk this port's own ordering-agnosticism leaves to the
    /// caller.
    async fn list(&self) -> Result<Vec<Memory>, MemoryStoreError>;

    /// Retire the memory stored under `id`. Absent ->
    /// [`MemoryStoreError::NotFound`] (not a silent no-op): a caller asking
    /// to forget a specific id that turns out not to exist is a fact worth
    /// surfacing, not swallowing.
    async fn remove(&self, id: &MemoryId) -> Result<(), MemoryStoreError>;
}
