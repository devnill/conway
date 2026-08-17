//! The `Curator` port (DESIGN-context-path §11.3, §11.4, §11.5, §11.6).
//!
//! A **curator** is the selection-layer curation capability: a plugin that
//! chooses which records belong in this turn's context, *before* assembly
//! renders them. It is a second, separate port alongside [`ContextHook`](crate::ports::ContextHook),
//! and the two do not compete because they operate at different layers:
//!
//! | | `ContextHook` | what a curator needs |
//! | --- | --- | --- |
//! | Runs | **after** assembly | **before** assembly |
//! | Operates on | `Vec<PromptSegment>` -- rendered | `ValidatedPath` -- references |
//! | Sees | bytes, per-model | records, model-free |
//! | Its edit is | a rewrite the harness cannot validate | a `derive` the harness validates |
//!
//! (Table adapted from DESIGN §11.3.) Every advantage the path mechanism
//! claims for mechanical cherry-picking -- byte-identical records, knowable
//! cache cost, refusal instead of silent repair, structural predicates -- is
//! available at the selection layer and NOT at the segment layer, which is
//! why curation gets its own port rather than riding `ContextHook`.
//!
//! ## Shape (§11.4)
//!
//! A curator never constructs a path directly: it calls
//! [`ValidatedPath::derive`]/[`ValidatedPath::derive_reordered`], which is
//! where refusal and `offers` live (§4.1). [`CurateOutcome::Derived`] can
//! only be built from a [`Derivation`], so **an unvalidated path cannot reach
//! the runtime**: the same "make it unrepresentable" move as
//! `GuardedContextHook`, one layer up. No separate `GuardedCurator`
//! re-validation layer is needed -- the `Derivation`-only construction IS the
//! guard.
//!
//! ## Read surface (§11.5)
//!
//! [`CurateCtx`] carries the store and resolver a curator needs to read
//! records. A curator may reference **any** record in the store
//! (INTENT.md §5e): a sibling's, another project's, an unrelated tree's.
//! That is what makes a memory plugin expressible here rather than as a
//! separate subsystem (§11.7).
//!
//! ## GP-03
//!
//! Like [`Plugin::tools`]/[`Plugin::context_hooks`], curators install through
//! the SAME `with_plugin`/`install_selected` surface every other plugin
//! capability uses -- no privileged first-party channel. See
//! [`Plugin::curators`] for the full GP-03 argument.
//!
//! ## Why `Arc<dyn SessionStore>` is cycle-safe
//!
//! `SessionStore` is a `conway-core` port (`crate::ports::session`), NOT the
//! `conway` facade type the forbidden-types doc names (see `ToolCtx`'s own
//! "never will reach a live `Conway`/`SessionHandle`" passage in
//! `plugin.rs`). Only `Conway`/`SessionHandle` are forbidden -- they live in
//! the `conway` facade and would reopen the `conway-core -> conway` cycle.
//! `SessionStore` carries no such risk, so a `CurateCtx` holding
//! `Arc<dyn SessionStore>` plus the resolver is GP-03-compliant and does NOT
//! reopen the crate cycle. The handle-based alternative `CommandOutcome`'s
//! "weighed and rejected" passage describes is rejected precisely because a
//! live handle WOULD. (Both cited by anchor rather than line number: the
//! earlier line numbers rotted the moment `Plugin::curators` was inserted
//! above them.)

use std::sync::Arc;

use async_trait::async_trait;

use crate::ids::{AgentId, ModelId, SessionId};
use crate::path::{Derivation, ValidatedPath};
use crate::ports::SessionStore;
use crate::transcript::TranscriptResolver;

/// The selection-layer curation port (§11.4). A curator receives the
/// turn's resolved path as a [`ValidatedPath`] base and returns one of:
///
/// - [`CurateOutcome::Unchanged`] -- the overwhelmingly common case; cheap,
///   and the stage passes the original path through untouched.
/// - [`CurateOutcome::Derived`] -- a validated alternative path, produced by
///   `base.derive(...)` / `base.derive_reordered(...)`. The harness already
///   validated it; the stage adopts it.
/// - [`CurateOutcome::Failed`] -- recorded, non-fatal (§11.6); the stage
///   proceeds on the uncurated path.
///
/// Object-safe by construction (no generic params, `#[async_trait]`), so the
/// runtime can hold `Arc<dyn Curator>`.
#[async_trait]
pub trait Curator: Send + Sync + 'static {
    /// Curate this turn's path. `base` is the harness-resolved path
    /// (prefix-expanded, records already read); `ctx` carries the read
    /// surface (store + resolver) for any cross-session reach (§11.5).
    async fn curate(&self, ctx: &CurateCtx, base: &ValidatedPath) -> CurateOutcome;
}

/// What a curator returns from [`Curator::curate`].
///
/// `Derived` can only be built from a [`Derivation`] -- the validated,
/// cost-estimated output of `ValidatedPath::derive` -- so an unvalidated
/// path cannot reach the runtime (§11.4). There is deliberately no
/// "hand-rolled `ValidatedPath`" variant: the only way in is through the
/// refusing constructors.
#[derive(Debug)]
pub enum CurateOutcome {
    /// The curator declines to act; the stage uses the original path.
    Unchanged,
    /// The curator produced a validated alternative. The stage adopts this
    /// derivation's path.
    Derived(Derivation),
    /// The curator failed (e.g. a `derive` refused its ops, or the plugin
    /// hit an internal error). Recorded non-fatally (§11.6); the stage
    /// proceeds on the uncurated path. Fail-open is justified because a
    /// curator is an *optimization*, not a correctness requirement, and the
    /// consequence of not curating is caught downstream by admission.
    Failed { reason: String },
}

/// The read surface a curator operates against (§11.5). Plain public-field
/// struct, deliberately NOT `#[non_exhaustive]`, mirroring
/// [`ContextHookCtx`](crate::ports::ContextHookCtx)/[`ToolCtx`](crate::ports::ToolCtx):
/// the facade re-exports it, a curator author names it, and a test fixture
/// constructs it with ordinary struct-literal syntax.
///
/// **`model: Option<ModelId>`, not `Option<ModelRef>`** -- the curator stage
/// runs BEFORE routing/assembly, so the only model information available is
/// the `AgentSpec::pin`'s `ModelId` hint (or `"unrouted"` when unpinned). A
/// routed `ModelRef` does not exist yet at this point in the turn. This is
/// the model-dependent fact §11.5 permits a curator to *read*; what it
/// *produces* stays model-free.
///
/// `Debug` is manual (not derived) for the SAME reason [`ToolCtx`]'s is:
/// `Arc<dyn SessionStore>` does not implement `Debug`, so the trait-object
/// field is rendered as an opaque placeholder -- mirroring [`ToolCtx`]'s
/// `&"<dyn EventSink>"` rendering of its own `Arc<dyn EventSink>` field.
///
/// [`ToolCtx`]: crate::ports::ToolCtx
#[derive(Clone)]
pub struct CurateCtx {
    /// The agent whose context is being curated.
    pub agent_id: AgentId,
    /// The session the turn belongs to.
    pub session_id: SessionId,
    /// The current turn number.
    pub turn: u32,
    /// The pinned model id, if `AgentSpec::pin` set one, else the literal
    /// `"unrouted"` sentinel. Model-free curation ignores this; a
    /// window-aware compactor reads it.
    ///
    /// In practice the runtime always passes `Some`: the curator stage runs
    /// before routing, and the unpinned case is carried as
    /// `Some(ModelId::new("unrouted"))` rather than `None`. The field stays
    /// `Option` so a test fixture (or a future non-turn caller) can express
    /// "no model information at all", which is distinct from "unrouted".
    pub model: Option<ModelId>,
    /// Cross-session record read (§11.5): a curator may read ANY session's
    /// records, not just `session_id`'s. `Arc<dyn SessionStore>` is
    /// cycle-safe -- see the module doc.
    pub store: Arc<dyn SessionStore>,
    /// Memoised effective-transcript resolution (§11.5): `resolve`/`
    /// resolve_prefix` over any session, cached. `Arc` because
    /// `TranscriptResolver` is not `Clone` (it holds a `Mutex`-backed LRU).
    pub resolver: Arc<TranscriptResolver>,
}

impl std::fmt::Debug for CurateCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CurateCtx")
            .field("agent_id", &self.agent_id)
            .field("session_id", &self.session_id)
            .field("turn", &self.turn)
            .field("model", &self.model)
            .field("store", &"<dyn SessionStore>")
            .field("resolver", &self.resolver)
            .finish()
    }
}
