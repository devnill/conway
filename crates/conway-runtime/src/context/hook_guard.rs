//! One seam enforcing tool-call/result coherence on whatever a
//! `ContextHook` returns -- `docs/vision/INTENT.md` §8.6: "an invariant
//! belongs to the seam, not to its call sites." `ContextBuilder::build`
//! already guarantees a rendered context never carries a tool call without
//! its result (see `super::builder`'s module doc, "Tool-call/result
//! pairing"); nothing re-checked that guarantee once a `ContextHook` ran, so
//! a hook that dropped (or otherwise orphaned) half a call/result pair
//! shipped a request every provider rejects outright -- surfacing as an
//! opaque backend error with no indication the harness assembled it that
//! way (board item `01M00RGARPESWXYAVY960KDE7S`).
//!
//! [`GuardedContextHook`] is that seam. A hook enters this runtime through
//! exactly one place -- `Runtime::set_context_hook` -- which wraps it, and
//! `LoopDeps::context_hook` stores the wrapped type, never a bare
//! `Arc<dyn ContextHook>`. Both call sites in `crate::agent_loop` therefore
//! just call the guard and propagate its `Result`; neither performs a check,
//! because neither has anything left to forget.
//!
//! **A third `ContextHook` method added later inherits the check by
//! construction**, not by its new call site remembering to run one. That
//! distinction is the whole item: an earlier revision routed both call sites
//! through [`ensure_hook_payload_coherent`] directly, which covered both
//! methods that existed but left the next one exactly as exposed as
//! `on_overflow` had been.
//!
//! **Refuse, not repair** (settled by the board item this module implements):
//! a hook's edit is a deliberate act, so an incoherent result is reported as
//! a typed error naming what it orphaned and which method produced it --
//! never silently patched up, which would delete part of a deliberate
//! choice (`INTENT.md` §5b; `docs/vision/DESIGN-context-path.md` §4.1).
//!
//! **Naming "the responsible hook":** `conway_core::ports::plugin::
//! ContextHook` is held as `Arc<dyn ContextHook>` with no `name()`/`Debug`
//! bound of its own -- adding one is `conway-core` scope, out of this
//! item's. [`HookMethod`] is the closest available identity: which of the
//! trait's two methods produced the payload. Combined with the
//! `agent_id`/`session_id`/`turn` [`HookCoherenceError`] also carries (from
//! the very `ContextHookCtx` the hook itself was called with), that is
//! enough to name exactly which turn's which hook call broke coherence.

use std::sync::Arc;

use conway_core::error::{HookOrphan, RuntimeError};
use conway_core::ids::{AgentId, SessionId};
use conway_core::ports::{ContextHook, ContextHookCtx, ContextPayload, OverflowInfo};

// [`HookMethod`] moved to `conway_core::error` (board item
// `01M014W91NWBP35139YFWCB88J`) -- it had exactly one caller in this crate
// (this module) and no other user, so it relocates cleanly rather than
// being duplicated behind a stringly-typed field on
// [`RuntimeError::ContextHookIncoherent`]. Re-exported under this module's
// own name below, so nothing else in this crate has to know it moved.
pub(crate) use conway_core::error::HookMethod;

use super::builder::{check_tool_call_coherence, ToolCallOrphan};

/// The typed error [`ensure_hook_payload_coherent`] returns: which hook
/// method produced the incoherent payload, which agent/session/turn it
/// happened on, and every orphaned `call_id` in either direction (see
/// [`ToolCallOrphan`]). Never constructed for a repair -- see this module's
/// doc, "Refuse, not repair" -- only ever to name the cause of a refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HookCoherenceError {
    pub method: HookMethod,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub turn: u32,
    pub orphans: Vec<ToolCallOrphan>,
}

impl std::fmt::Display for HookCoherenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} on agent {} (session {}, turn {}) returned an incoherent context ({} orphan{}): ",
            self.method,
            self.agent_id,
            self.session_id,
            self.turn,
            self.orphans.len(),
            if self.orphans.len() == 1 { "" } else { "s" },
        )?;
        for (index, orphan) in self.orphans.iter().enumerate() {
            if index > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{orphan}")?;
        }
        Ok(())
    }
}

impl std::error::Error for HookCoherenceError {}

impl HookCoherenceError {
    /// The one conversion point into [`RuntimeError`], the currency every
    /// call site in `agent_loop.rs` must eventually return.
    ///
    /// Maps onto [`RuntimeError::ContextHookIncoherent`] -- a dedicated
    /// variant, not a fold into `BackendError::BadRequest`'s `detail`
    /// string (board item `01M014W91NWBP35139YFWCB88J`; an earlier item,
    /// `01M00RGARPESWXYAVY960KDE7S`, folded it there instead, having
    /// identified but correctly declined to make this `conway-core`-scoped
    /// change from within its own narrower scope). Every fact this typed
    /// error names -- `method`, `agent_id`/`session_id`/`turn`, and every
    /// orphaned `call_id` in either direction -- travels into the variant's
    /// own fields, not just its rendered `Display`: a caller matches on the
    /// variant directly and never has to string-match `detail` to identify
    /// this cause.
    pub(crate) fn into_runtime_error(self) -> RuntimeError {
        RuntimeError::ContextHookIncoherent {
            agent: self.agent_id,
            session: self.session_id,
            turn: self.turn,
            method: self.method,
            orphans: self.orphans.iter().map(orphan_to_core).collect(),
        }
    }
}

/// Maps this module's own [`ToolCallOrphan`] onto `conway_core::error::
/// HookOrphan`, the boundary-crossing counterpart
/// [`HookCoherenceError::into_runtime_error`] needs. Not a relocation: see
/// [`HookOrphan`]'s own doc for why `ToolCallOrphan` itself stays
/// `pub(crate)` here (`ContextBuilder::build`'s own mutating pass and this
/// crate's own test suite both use it well beyond this one error path).
fn orphan_to_core(orphan: &ToolCallOrphan) -> HookOrphan {
    match orphan {
        ToolCallOrphan::UnansweredCall { call_id } => HookOrphan::UnansweredCall {
            call_id: call_id.clone(),
        },
        ToolCallOrphan::OrphanedResult { call_id } => HookOrphan::OrphanedResult {
            call_id: call_id.clone(),
        },
    }
}

/// The one wrapper both hook call sites route through (see this module's
/// own doc). Runs [`check_tool_call_coherence`] -- the check-only sibling of
/// `ContextBuilder::build`'s own `drop_unanswered_tool_calls` -- on
/// `payload.segments` and refuses (rather than repairs) if it finds
/// anything: see this module's doc, "Refuse, not repair."
///
/// Callers pass the payload ALREADY unwrapped from whatever shape their
/// method returns it in (`before_request` always returns one;
/// `on_overflow`'s `Some(payload)` arm already has one -- its `None` arm
/// means "hook declined," never reaches here, and is unaffected by this
/// function existing at all).
pub(crate) fn ensure_hook_payload_coherent(
    method: HookMethod,
    ctx: &ContextHookCtx,
    payload: ContextPayload,
) -> Result<ContextPayload, HookCoherenceError> {
    let orphans = check_tool_call_coherence(&payload.segments);
    if orphans.is_empty() {
        Ok(payload)
    } else {
        Err(HookCoherenceError {
            method,
            agent_id: ctx.agent_id,
            session_id: ctx.session_id,
            turn: ctx.turn,
            orphans,
        })
    }
}

/// Makes an unwrapped hook unrepresentable, per `INTENT.md` §8.6: rather
/// than trusting every current and future call site to remember to call
/// `ensure_hook_payload_coherent` (the exact shape of the defect this
/// module exists to retire -- `on_overflow` went unguarded while
/// `before_request` was the one being discussed), the check is installed
/// ONCE, at construction, and the resulting type is what
/// `LoopDeps::context_hook` (`crate::agent_loop`) actually stores --
/// `Arc<GuardedContextHook>`, never a bare `Arc<dyn ContextHook>`. There is
/// no second constructor and no public field: the only way to produce one is
/// [`Self::new`], and the only place that is called is
/// `crate::runtime::Runtime::set_context_hook` -- the one seam a hook enters
/// this runtime through.
///
/// **This type deliberately does NOT implement [`ContextHook`].** It is not a
/// hook; it is a checked *caller* of one, and its `before_request`/
/// `on_overflow` are inherent, `Result`-returning methods that can express
/// refusal where the trait's fixed return types cannot.
///
/// An earlier revision did implement the trait — delegating, checking, and
/// panicking on refusal because the signature left it no other way — as
/// "defense in depth" against a caller erasing a wrapped hook back to
/// `&dyn ContextHook` and reaching the unchecked path. That reasoning is
/// circular: the impl was the only thing that made such a view obtainable in
/// the first place, so it manufactured the hazard it then defended against.
/// It also made the guarantee rest on Rust resolving inherent methods before
/// same-named trait methods — true, but a subtle rule to hang a safety
/// property on, and one that reads as infinite recursion at a glance.
///
/// Without the impl, a `GuardedContextHook` **cannot** be erased to
/// `&dyn ContextHook` at all. There is no unchecked path to defend, no
/// name-resolution subtlety, and no panic where `INTENT.md` §8.3 asks for a
/// typed refusal. Removing code was the stronger guarantee.
pub struct GuardedContextHook {
    inner: Arc<dyn ContextHook>,
}

impl GuardedContextHook {
    /// The only constructor. Nothing about `inner` is inspected or altered
    /// here -- wrapping installs the check at every SUBSEQUENT call, not at
    /// registration time (an already-coherent hook costs nothing extra
    /// either way, since `check_tool_call_coherence`'s common-case cost is
    /// a `HashSet` build and an empty `Vec`).
    ///
    /// `pub`, unlike this module's other internals: `LoopDeps::context_hook`
    /// (`crate::agent_loop`) is a fully public field, and every test fixture
    /// that constructs a `LoopDeps` directly (bypassing `Runtime::
    /// set_context_hook`, this constructor's one PRODUCTION caller) needs to
    /// reach this from outside the crate to install a hook at all -- see
    /// `crates/conway-runtime/tests/agent_loop_e2e.rs`'s `build_loop_inner`.
    pub fn new(inner: Arc<dyn ContextHook>) -> Self {
        Self { inner }
    }

    /// Same name as [`ContextHook::before_request`], but `Result`-returning
    /// -- see this type's own doc, "Two method names, same name twice."
    /// Delegates to the wrapped hook, then runs the identical
    /// [`ensure_hook_payload_coherent`] the free function's own unit tests
    /// already cover.
    pub(crate) async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> Result<ContextPayload, HookCoherenceError> {
        let transformed = self.inner.before_request(ctx, payload).await;
        ensure_hook_payload_coherent(HookMethod::BeforeRequest, ctx, transformed)
    }

    /// Same name as [`ContextHook::on_overflow`], but `Result`-returning --
    /// see this type's own doc. `None` from the wrapped hook ("this hook
    /// declines to shrink the payload further") passes straight through as
    /// `Ok(None)`: there is nothing to check, exactly like
    /// [`ensure_hook_payload_coherent`]'s own doc on its callers.
    pub(crate) async fn on_overflow(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
        overflow: OverflowInfo,
    ) -> Result<Option<ContextPayload>, HookCoherenceError> {
        match self.inner.on_overflow(ctx, payload, overflow).await {
            None => Ok(None),
            Some(transformed) => {
                ensure_hook_payload_coherent(HookMethod::OnOverflow, ctx, transformed).map(Some)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::content::{ContentBlock, Role, ToolSpec};
    use conway_core::ids::ToolName;
    use conway_core::provenance::Provenance;
    use conway_core::segment::PromptSegment;

    use conway_core::ports::ArtifactWriteHandle;

    fn ctx(agent_id: AgentId, session_id: SessionId, turn: u32) -> ContextHookCtx {
        ContextHookCtx {
            agent_id,
            agent_path: vec![agent_id],
            session_id,
            turn,
            model: None,
            estimated_tokens: 0,
            artifacts: ArtifactWriteHandle::noop(agent_id),
            tag: None,
        }
    }

    fn tool_use_segment(call_id: &str) -> PromptSegment {
        PromptSegment::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                call_id: call_id.to_string(),
                name: ToolName::new("read"),
                arguments: serde_json::json!({}),
            }],
            Provenance::SystemNote {
                reason: "assistant_turn".to_string(),
            },
        )
    }

    fn tool_result_segment(call_id: &str) -> PromptSegment {
        PromptSegment::new(
            Role::ToolResult,
            vec![ContentBlock::ToolResultBlock {
                call_id: call_id.to_string(),
                blocks: vec![ContentBlock::Text {
                    text: "contents".to_string(),
                }],
                is_error: false,
            }],
            Provenance::ToolResult {
                call_id: call_id.to_string(),
                tool: ToolName::new("read"),
            },
        )
    }

    fn payload(segments: Vec<PromptSegment>) -> ContextPayload {
        ContextPayload {
            segments,
            tools: Vec::<ToolSpec>::new(),
        }
    }

    /// The settled case -- a hook that changes nothing, or edits without
    /// touching any tool call/result pairing -- passes through untouched.
    #[test]
    fn a_coherent_payload_passes_through_unchanged() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let hook_ctx = ctx(agent_id, session_id, 3);
        let input = payload(vec![tool_use_segment("a"), tool_result_segment("a")]);

        let result =
            ensure_hook_payload_coherent(HookMethod::BeforeRequest, &hook_ctx, input.clone());

        let output = result.expect("a coherent payload must not be refused");
        assert_eq!(output.segments.len(), input.segments.len());
    }

    /// The regression this item exists for: a `before_request` hook drops
    /// the `ToolResultBlock` segment out of an otherwise-answered pair.
    /// `drop_unanswered_tool_calls` never sees this list again (assembly
    /// already ran) -- this function is the only thing left that can catch
    /// it.
    #[test]
    fn a_hook_dropping_a_tool_result_segment_is_refused_naming_the_orphan_and_method() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let hook_ctx = ctx(agent_id, session_id, 5);
        // The hook received both segments but returned only the call.
        let dropped_result = payload(vec![tool_use_segment("orphan")]);

        let err =
            ensure_hook_payload_coherent(HookMethod::BeforeRequest, &hook_ctx, dropped_result)
                .expect_err("a call with no answering result must be refused");

        assert_eq!(err.method, HookMethod::BeforeRequest);
        assert_eq!(err.agent_id, agent_id);
        assert_eq!(err.session_id, session_id);
        assert_eq!(err.turn, 5);
        assert_eq!(
            err.orphans,
            vec![ToolCallOrphan::UnansweredCall {
                call_id: "orphan".to_string()
            }]
        );
        let message = err.to_string();
        assert!(message.contains("orphan"), "message: {message}");
        assert!(
            message.contains("ContextHook::before_request"),
            "message: {message}"
        );
    }

    /// The other direction: an `on_overflow` hook drops the `ToolUse`
    /// segment, leaving its result stranded. `drop_unanswered_tool_calls`
    /// never handled this direction even before hooks existed (see its own
    /// corrected doc) -- this is the case that makes the "could not be made
    /// to fail" claim false.
    #[test]
    fn a_hook_dropping_a_tool_use_segment_is_refused_naming_the_orphan_and_method() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let hook_ctx = ctx(agent_id, session_id, 7);
        let dropped_call = payload(vec![tool_result_segment("stranded")]);

        let err = ensure_hook_payload_coherent(HookMethod::OnOverflow, &hook_ctx, dropped_call)
            .expect_err("a result with no surviving call must be refused");

        assert_eq!(err.method, HookMethod::OnOverflow);
        assert_eq!(
            err.orphans,
            vec![ToolCallOrphan::OrphanedResult {
                call_id: "stranded".to_string()
            }]
        );
        let message = err.to_string();
        assert!(message.contains("stranded"), "message: {message}");
        assert!(
            message.contains("ContextHook::on_overflow"),
            "message: {message}"
        );
    }

    /// Board item `01M09P2T8E5M292WMSMS64CVC4`: `conway.memory`'s
    /// `ContextHook` injects a plain-text `Provenance::Memory` segment with
    /// no `ToolUse`/`ToolResultBlock` content at all. "Verify, do not
    /// assume" -- this proves the exact shape that plugin's hook produces
    /// passes `check_tool_call_coherence` (and therefore
    /// `ensure_hook_payload_coherent`) unmodified, rather than merely
    /// arguing it must from `check_tool_call_coherence`'s own definition.
    #[test]
    fn an_injected_memory_segment_does_not_trip_the_coherence_guard() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let hook_ctx = ctx(agent_id, session_id, 1);
        let memory_segment = PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: "the deploy secret lives in vault".to_string(),
            }],
            Provenance::Memory {
                id: conway_core::ids::MemoryId::new(),
            },
        );
        let input = payload(vec![memory_segment]);

        let result =
            ensure_hook_payload_coherent(HookMethod::BeforeRequest, &hook_ctx, input.clone());

        let output = result.expect("an injected memory segment must never be refused");
        assert_eq!(output.segments.len(), 1);
        assert!(matches!(
            output.segments[0].provenance,
            Provenance::Memory { .. }
        ));
    }

    /// The typed error becomes its own dedicated
    /// [`conway_core::error::RuntimeError::ContextHookIncoherent`] variant,
    /// not a fold into `BackendError::BadRequest`'s `detail` string (board
    /// item `01M014W91NWBP35139YFWCB88J`). Asserts on the variant's own
    /// fields via a `let else` on the exact variant -- not on any
    /// substring-matched text -- so this fails equally whether the fold
    /// comes back OR the variant's fields silently drop information the
    /// typed error carries: an assertion that only checked
    /// `.to_string().contains(...)` would pass against either shape and
    /// would not be testing this change (see `P-15`).
    #[test]
    fn into_runtime_error_produces_the_dedicated_variant_naming_every_field() {
        let agent_id = AgentId::new();
        let session_id = SessionId::new();
        let hook_ctx = ctx(agent_id, session_id, 5);
        let dropped_result = payload(vec![tool_use_segment("lost")]);

        let err =
            ensure_hook_payload_coherent(HookMethod::BeforeRequest, &hook_ctx, dropped_result)
                .expect_err("must be refused");

        let runtime_err = err.into_runtime_error();
        let RuntimeError::ContextHookIncoherent {
            agent,
            session,
            turn,
            method,
            orphans,
        } = runtime_err
        else {
            panic!("expected RuntimeError::ContextHookIncoherent, got {runtime_err:?}");
        };
        assert_eq!(agent, agent_id);
        assert_eq!(session, session_id);
        assert_eq!(turn, 5);
        assert_eq!(method, HookMethod::BeforeRequest);
        assert_eq!(
            orphans,
            vec![HookOrphan::UnansweredCall {
                call_id: "lost".to_string()
            }]
        );
    }
}

/// Board item `01M00RGARPESWXYAVY960KDE7S`, round 2: proves the claim
/// [`GuardedContextHook`]'s own doc makes -- an unwrapped hook is
/// unrepresentable, and the coherence check is installed once, at
/// construction, rather than left for every call site to remember -- by
/// exercising it, not merely asserting it in prose.
#[cfg(test)]
mod context_hook_wrapping_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use conway_core::content::{ContentBlock, Role};
    use conway_core::ids::{AgentId, SessionId, ToolName};
    use conway_core::ports::{ArtifactWriteHandle, ContextHook, ContextHookCtx, ContextPayload};
    use conway_core::provenance::Provenance;
    use conway_core::segment::PromptSegment;

    use super::GuardedContextHook;

    fn ctx(agent_id: AgentId, session_id: SessionId, turn: u32) -> ContextHookCtx {
        ContextHookCtx {
            agent_id,
            agent_path: vec![agent_id],
            session_id,
            turn,
            model: None,
            estimated_tokens: 0,
            artifacts: ArtifactWriteHandle::noop(agent_id),
            tag: None,
        }
    }

    fn tool_use_segment(call_id: &str) -> PromptSegment {
        PromptSegment::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                call_id: call_id.to_string(),
                name: ToolName::new("read"),
                arguments: serde_json::json!({}),
            }],
            Provenance::SystemNote {
                reason: "assistant_turn".to_string(),
            },
        )
    }

    fn tool_result_segment(call_id: &str) -> PromptSegment {
        PromptSegment::new(
            Role::ToolResult,
            vec![ContentBlock::ToolResultBlock {
                call_id: call_id.to_string(),
                blocks: vec![ContentBlock::Text {
                    text: "contents".to_string(),
                }],
                is_error: false,
            }],
            Provenance::ToolResult {
                call_id: call_id.to_string(),
                tool: ToolName::new("read"),
            },
        )
    }

    /// An ordinary, un-self-checking `ContextHook` -- exactly what every
    /// real implementation looks like, since coherence-checking was never
    /// part of the trait's own contract. Strips every `ToolResultBlock`
    /// segment it is handed, from BOTH methods, so wrapping it in
    /// `GuardedContextHook` is the only thing standing between an unanswered
    /// `ToolUse` and a request no provider would accept.
    struct DropsEveryResult;

    fn strip_results(payload: ContextPayload) -> ContextPayload {
        ContextPayload {
            segments: payload
                .segments
                .into_iter()
                .filter(|s| {
                    !s.content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolResultBlock { .. }))
                })
                .collect(),
            tools: payload.tools,
        }
    }

    #[async_trait]
    impl ContextHook for DropsEveryResult {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            payload: ContextPayload,
        ) -> ContextPayload {
            strip_results(payload)
        }

        async fn on_overflow(
            &self,
            _ctx: &ContextHookCtx,
            payload: ContextPayload,
            _overflow: conway_core::ports::OverflowInfo,
        ) -> Option<ContextPayload> {
            Some(strip_results(payload))
        }
    }

    fn incoherent_payload() -> ContextPayload {
        ContextPayload {
            segments: vec![tool_use_segment("orphan"), tool_result_segment("orphan")],
            tools: vec![],
        }
    }

    /// The claim that matters most: calling `.before_request(...)` on the
    /// CONCRETE type `LoopDeps::context_hook` actually stores
    /// (`&GuardedContextHook`/`Arc<GuardedContextHook>`) reaches the checked
    /// inherent method, not `ContextHook`'s unchecked trait method of the
    /// same name -- proven by the fact that this returns `Err` at all
    /// (the trait method's return type has no `Err` to return; if this call
    /// had resolved to it instead, `DropsEveryResult`'s edit would have gone
    /// straight through as an ordinary `ContextPayload`, or this test would
    /// have hit the trait impl's panic -- see
    /// `via_a_dyn_context_hook_view_the_same_incoherence_panics_instead`
    /// below for that path exercised directly).
    #[tokio::test]
    async fn before_request_on_the_concrete_type_reaches_the_checked_inherent_method() {
        let guarded = GuardedContextHook::new(Arc::new(DropsEveryResult));
        let agent_id = AgentId::new();
        let hook_ctx = ctx(agent_id, SessionId::new(), 1);

        let err = guarded
            .before_request(&hook_ctx, incoherent_payload())
            .await
            .expect_err("DropsEveryResult orphans the call; must be refused");

        assert_eq!(err.orphans.len(), 1);
    }

    /// Same claim, `on_overflow` -- proving ONE mechanism (`GuardedContextHook`)
    /// covers BOTH methods identically, which is the whole point of enforcing
    /// at the seam rather than per call site (`INTENT.md` §8.6): there is
    /// exactly one place this behavior is implemented, exercised twice here,
    /// rather than two independent call-site checks that could drift (which
    /// is exactly how `on_overflow` went unguarded the first time).
    #[tokio::test]
    async fn on_overflow_on_the_concrete_type_reaches_the_checked_inherent_method() {
        let guarded = GuardedContextHook::new(Arc::new(DropsEveryResult));
        let agent_id = AgentId::new();
        let hook_ctx = ctx(agent_id, SessionId::new(), 1);
        let overflow = conway_core::ports::OverflowInfo {
            max_context_tokens: 100,
            headroom_tokens: 10,
            required_tokens: 200,
            shortfall_tokens: 100,
        };

        let err = guarded
            .on_overflow(&hook_ctx, incoherent_payload(), overflow)
            .await
            .expect_err("DropsEveryResult orphans the call; must be refused");

        assert_eq!(err.orphans.len(), 1);
    }

    /// A coherent edit still passes straight through both checked methods --
    /// the guard changes nothing about a hook that never orphans anything,
    /// matching every existing (unguarded-by-name) `ContextHook` test's
    /// expectations elsewhere in this crate.
    #[tokio::test]
    async fn a_coherent_edit_still_passes_through_both_checked_methods() {
        struct PassThrough;
        #[async_trait]
        impl ContextHook for PassThrough {
            async fn before_request(
                &self,
                _ctx: &ContextHookCtx,
                payload: ContextPayload,
            ) -> ContextPayload {
                payload
            }
        }

        let guarded = GuardedContextHook::new(Arc::new(PassThrough));
        let agent_id = AgentId::new();
        let hook_ctx = ctx(agent_id, SessionId::new(), 1);
        let coherent = ContextPayload {
            segments: vec![tool_use_segment("a"), tool_result_segment("a")],
            tools: vec![],
        };

        let out = guarded
            .before_request(&hook_ctx, coherent)
            .await
            .expect("a coherent payload must not be refused");
        assert_eq!(out.segments.len(), 2);
    }
}
