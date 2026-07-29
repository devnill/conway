//! The `Plugin`/`Tool` ports (architecture §4.2) and the `CancellationToken`
//! used to interrupt an in-flight tool call.
//!
//! **There is exactly one extension mechanism (GP-03).** Built-in
//! read/write/edit/bash and the subagent tool are `Plugin` implementations
//! registered by default in `ConwayBuilder`; nothing about them is
//! privileged. MVP plugins are in-process `Arc<dyn Plugin>` (Tension T-8).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::content::{Artifact, ContentBlock, ToolCall, ToolSpec, TruncationPolicy};
use crate::error::{PluginError, ToolError};
use crate::ids::{AgentId, ModelRef, SessionId, ToolName};
use crate::ports::{EventSinkHandle, SubagentHost};
use crate::segment::PromptSegment;

/// A source of tools: a plugin declares its identity, the tools it provides,
/// and an optional one-time initialization hook.
pub trait Plugin: Send + Sync + 'static {
    /// This plugin's static identity: id, semver, provided tools, required
    /// host capabilities.
    fn manifest(&self) -> PluginManifest;

    fn tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Called once at startup. The default no-op is correct for plugins that
    /// need no setup. No default method on this trait may perform I/O; a
    /// concrete `on_init` implementation may, but that is the implementer's
    /// responsibility, not this trait's contract.
    fn on_init(&self, _ctx: &PluginInitCtx) -> Result<(), PluginError> {
        Ok(())
    }
}

/// One invocable tool: aligned with ACP's tool-call categories (`ToolCategory`
/// in `content.rs`) for free future compatibility, zero present cost.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// This tool's name, description, JSON Schema, category, and permission
    /// class.
    fn spec(&self) -> ToolSpec;

    /// Invoke the tool.
    ///
    /// PRE: `call.arguments` has already been validated against
    /// `self.spec().schema`. PRE: permission has already been granted for
    /// `(agent, tool, arguments)`. POST: honors `ctx.cancel`; returns within
    /// the runtime's deadline or `Err(ToolError::Cancelled)`. POST: declares
    /// a `TruncationPolicy` on the returned `ToolOutput`; the runtime applies
    /// it and records the truncation in the log (architecture §8).
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;

    /// Renders this proposed call as a single human-readable line: the text
    /// behind `PermissionRequest::rendered` (permission prompt display,
    /// `Event::PermissionRequested`, and any future audit log), and --
    /// for a tool whose rendering is a shell-command-shaped string -- the
    /// text `conway_core::permission_pattern::PatternRule` prefix-matches
    /// against.
    ///
    /// PRE: `args` has already been validated against `self.spec().schema`
    /// by the caller. It is nonetheless UNTRUSTED, model-supplied content
    /// (P-10): an implementation MUST NOT panic on any `serde_json::Value`
    /// shape (no `unwrap`/`expect`/indexing into `args`), since a caller
    /// that skips validation, or a future validator bug, must not turn a
    /// bad render into a crash. Callers additionally sanitize the returned
    /// string for control bytes before display -- see
    /// `conway_runtime::tools::runner`'s render seam -- so an implementation
    /// need not do that itself, only avoid panicking.
    ///
    /// The default reproduces this trait's original, pre-per-tool-render
    /// behavior: a generic `name(args)` one-liner. It is correct for any
    /// tool whose call has no natural single command-string representation
    /// (`read`, `edit`, the subagent tools, ...). A tool whose call IS
    /// meaningfully a shell command -- `bash` -- overrides this to return
    /// that bare command string instead, so `PatternRule`'s prefix matching
    /// (designed against a shell command, not a JSON debug dump) has
    /// something legible to operate on.
    fn render(&self, args: &serde_json::Value) -> String {
        format!("{}({})", self.spec().name, args)
    }
}

/// A plugin's static identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub tools: Vec<ToolName>,
    pub required_host_caps: Vec<String>,
}

/// Context passed to `Plugin::on_init`.
#[derive(Clone, Debug)]
pub struct PluginInitCtx {
    pub config: Arc<PluginConfig>,
    pub cwd: PathBuf,
}

/// A plugin's untyped configuration values, as loaded and handed down by the
/// facade. This crate does no config loading itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginConfig {
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// Per-invocation context handed to `Tool::invoke`.
///
/// `Clone` (every field is an `Arc`, `Copy`, or otherwise cheap to clone).
/// **Not** `Serialize` — it holds trait objects (`events`, `subagents`).
/// This is the known T-8 limitation: `ToolCall` and `ToolOutput` are fully
/// serializable, so a future subprocess/RPC plugin transport only needs an
/// RPC-shaped form of `ToolCtx`, not this one.
#[derive(Clone)]
pub struct ToolCtx {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    /// Progress reporting; see [`EventSinkHandle`].
    pub events: EventSinkHandle,
    /// The cycle-breaker for the fork/spawn tool: the same trait object the
    /// developer API (`SessionHandle::fork`/`spawn`) calls.
    pub subagents: Arc<dyn SubagentHost>,
    pub config: Arc<PluginConfig>,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("agent_id", &self.agent_id)
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd)
            .field("cancel", &self.cancel)
            .field("events", &"<dyn EventSink>")
            .field("subagents", &"<dyn SubagentHost>")
            .field("config", &self.config)
            .finish()
    }
}

/// The outcome of a tool invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    /// The tool declares how it wants oversized output handled; the runtime
    /// enforces the policy and records the truncation in the log.
    pub truncation: TruncationPolicy,
    pub artifacts: Vec<Artifact>,
}

/// The outgoing request payload a [`ContextHook`] may transform: the
/// assembled prompt segments (in send order, including the `ToolRegistry`
/// segment) and the tool set announced to the model for this turn.
///
/// **Tool announcement vs. execution (WI-126):** `tools` here is what the
/// model is TOLD it may call -- distinct from [`PermissionGate`], which
/// governs whether a call the model actually makes is allowed to run.
/// Narrowing `tools` hides a tool from the model entirely (it can never
/// propose calling it this turn); `PermissionGate` still gates every
/// proposed call regardless of what was announced. A tool a hook filters
/// out here was never a `PermissionGate` bypass -- it is simply never
/// offered.
#[derive(Clone, Debug, Default)]
pub struct ContextPayload {
    pub segments: Vec<PromptSegment>,
    pub tools: Vec<ToolSpec>,
}

/// Read-only identity/sizing context for one [`ContextHook`] invocation.
/// `estimated_tokens` reflects whatever payload is being transformed by
/// *this* call (the freshly built assembly for [`ContextHook::before_request`],
/// or the still-too-large one for [`ContextHook::on_overflow`]) -- a hook
/// does not need to recompute it itself.
#[derive(Clone, Debug)]
pub struct ContextHookCtx {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub turn: u32,
    /// The model this request is routed toward, if known yet. `None` for
    /// `before_request` on an unpinned role (routing hasn't run); `Some` for
    /// `before_request` when `AgentSpec::pin` fixes the model regardless of
    /// routing, and always `Some` for `on_overflow` (a specific route was
    /// already chosen and found to overflow by the time that fires).
    pub model: Option<ModelRef>,
    pub estimated_tokens: u32,
}

/// Why [`ContextHook::on_overflow`] fired: the same shortfall accounting as
/// `conway_core::error::RoutingError::ContextTooLarge`, so a hook can decide
/// how much to trim without the runtime recomputing anything the T-1 gate
/// already worked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverflowInfo {
    pub max_context_tokens: u32,
    pub headroom_tokens: u32,
    /// `estimated_tokens + headroom_tokens`, saturating.
    pub required_tokens: u32,
    /// `required_tokens - max_context_tokens`, saturating.
    pub shortfall_tokens: u32,
}

/// Pluggable per-call context/tool curation (WI-126, architecture's
/// unifying hook primitive): invoked before every LLM request, with an
/// optional second invocation if the first invocation's output still
/// overflows the routed model's window.
///
/// **No hook registered is the whole contract for "default behavior
/// unchanged":** the runtime holds this as `Option<Arc<dyn ContextHook>>`
/// and never invokes anything when it is `None` -- not even a no-op
/// pass-through call. `conway-core` ships no implementation and no built-in
/// curation policy; every consumer (CLI/IDE/embedder) that wants masking,
/// system-prompt instrumentation, tool-announcement narrowing, or
/// overflow-time compaction supplies its own.
///
/// **One trait, three transforms:** `before_request`'s `ContextPayload`
/// bundles segments and announced tools together because the runtime treats
/// them as one outgoing request -- a hook can edit/drop a segment (e.g. the
/// `AgentDef`-provenance segment, to augment the system prompt; or any
/// segment, to apply an ad hoc exclusion mirroring WI-125's persisted
/// `ContextMask`) and/or narrow `tools` (announcement filtering) in the same
/// call. Async so an inference-driven hook can issue its own LLM call to
/// decide (criterion: "hooks may be pure scripts OR issue their own LLM
/// call").
///
/// **Overflow is a distinct, optional method, not a flag on
/// `before_request`:** `on_overflow` only fires when the *already-hooked*
/// payload still doesn't fit the routed model's window (the runtime's T-1
/// gate). Its default returns `None`, which the runtime treats identically
/// to no hook being registered at all: a hard `ContextTooLarge`. This
/// preserves "no hook registered -> today's behavior exactly" as a
/// per-method guarantee, not just a per-trait one -- a consumer can
/// implement curation (`before_request`) without accidentally also
/// suppressing the hard overflow error.
#[async_trait]
pub trait ContextHook: Send + Sync + 'static {
    /// Invoked once per assembled request, before it is routed/sent.
    /// Returning `payload` unchanged is always a valid implementation.
    async fn before_request(&self, ctx: &ContextHookCtx, payload: ContextPayload)
        -> ContextPayload;

    /// Invoked only when `before_request`'s output still exceeds the routed
    /// model's window. `Some(payload)` gives the runtime a smaller/edited
    /// payload to re-estimate and retry -- bounded by the runtime's own
    /// re-assembly loop, never by this trait. `None` (the default) falls
    /// through to the hard `ContextTooLarge` error.
    async fn on_overflow(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
        overflow: OverflowInfo,
    ) -> Option<ContextPayload> {
        let _ = (ctx, payload, overflow);
        None
    }
}

/// A minimal, serialization-free cancellation flag.
///
/// `conway-core` cannot depend on `tokio`, so this is a small
/// `Arc<AtomicBool>`-based token rather than `tokio_util::sync::
/// CancellationToken`. Downstream crates that need an async cancellation
/// *await* (rather than a poll of `is_cancelled`) bridge this token to
/// `tokio_util`'s token themselves — see `conway-runtime`.
///
/// `child()` produces a token that observes both its own cancellation and
/// every ancestor's, to arbitrary depth: internally each child holds a
/// shared handle to its parent (rather than the parent's raw flag alone), so
/// cancelling a root token cancels every descendant transitively.
#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
    parent: Option<Arc<CancellationToken>>,
}

impl std::fmt::Debug for CancellationToken {
    // Manual impl: a derived Debug would walk (and print) the entire ancestor
    // chain, which is unbounded in a deep agent tree.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .field("has_parent", &self.parent.is_some())
            .finish()
    }
}

impl CancellationToken {
    /// A fresh, uncancelled, parentless token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks this token cancelled. Every token derived from it via
    /// [`Self::child`] (to any depth) observes this immediately.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// `true` if this token, or any ancestor it was derived from, has been
    /// cancelled. Iterative: walks the ancestor chain without recursion, so
    /// arbitrarily deep agent trees cannot overflow the stack.
    pub fn is_cancelled(&self) -> bool {
        let mut current = self;
        loop {
            if current.flag.load(Ordering::SeqCst) {
                return true;
            }
            match current.parent.as_deref() {
                Some(parent) => current = parent,
                None => return false,
            }
        }
    }

    /// A new token that is independently cancellable but also observes this
    /// token's (and its ancestors') cancellation.
    pub fn child(&self) -> CancellationToken {
        CancellationToken {
            flag: Arc::new(AtomicBool::new(false)),
            parent: Some(Arc::new(self.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_is_observed() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn child_observes_parent_cancellation() {
        let parent = CancellationToken::new();
        let child = parent.child();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_can_be_cancelled_independently_of_parent() {
        let parent = CancellationToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn grandchild_observes_root_cancellation() {
        let root = CancellationToken::new();
        let child = root.child();
        let grandchild = child.child();
        assert!(!grandchild.is_cancelled());
        root.cancel();
        assert!(grandchild.is_cancelled());
    }

    #[test]
    fn plugin_manifest_round_trips() {
        let manifest = PluginManifest {
            id: "builtin.fs".into(),
            version: "0.1.0".into(),
            tools: vec![ToolName::new("read"), ToolName::new("write")],
            required_host_caps: vec![],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn tool_output_round_trips() {
        let out = ToolOutput {
            blocks: vec![ContentBlock::Text { text: "ok".into() }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        };
        let json = serde_json::to_string(&out).unwrap();
        let back: ToolOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(out, back);
    }

    fn hook_ctx() -> ContextHookCtx {
        ContextHookCtx {
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            turn: 0,
            model: Some(ModelRef {
                backend: crate::ids::BackendId::new("anthropic"),
                model: crate::ids::ModelId::new("claude-sonnet-4-6"),
            }),
            estimated_tokens: 100,
        }
    }

    fn segment(text: &str) -> PromptSegment {
        PromptSegment::new(
            crate::content::Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            crate::provenance::Provenance::UserPrompt,
        )
    }

    /// A hook that drops every segment whose text contains "secret" and
    /// otherwise passes the payload through unchanged -- exercises the
    /// mask-like "drop a segment" transform (criterion 1a) without any
    /// dependency on WI-125's persisted `ContextMask`.
    struct DropSecretsHook;

    #[async_trait]
    impl ContextHook for DropSecretsHook {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            mut payload: ContextPayload,
        ) -> ContextPayload {
            payload.segments.retain(|s| {
                !s.content.iter().any(|b| match b {
                    ContentBlock::Text { text } => text.contains("secret"),
                    _ => false,
                })
            });
            payload
        }
    }

    #[test]
    fn before_request_can_drop_a_segment() {
        let hook: Arc<dyn ContextHook> = Arc::new(DropSecretsHook);
        let payload = ContextPayload {
            segments: vec![segment("hello"), segment("the secret plan")],
            tools: vec![],
        };
        let out = block_on(hook.before_request(&hook_ctx(), payload));
        assert_eq!(out.segments.len(), 1);
    }

    /// The default `on_overflow` -- what every hook gets unless it opts in
    /// by overriding it -- must return `None`, which the runtime treats
    /// identically to no hook being registered (hard `ContextTooLarge`).
    struct BeforeRequestOnlyHook;

    #[async_trait]
    impl ContextHook for BeforeRequestOnlyHook {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            payload: ContextPayload,
        ) -> ContextPayload {
            payload
        }
    }

    #[test]
    fn default_on_overflow_is_none() {
        let hook = BeforeRequestOnlyHook;
        let payload = ContextPayload {
            segments: vec![segment("hi")],
            tools: vec![],
        };
        let overflow = OverflowInfo {
            max_context_tokens: 100,
            headroom_tokens: 10,
            required_tokens: 200,
            shortfall_tokens: 100,
        };
        let out = block_on(hook.on_overflow(&hook_ctx(), payload, overflow));
        assert!(out.is_none());
    }

    /// Dependency-free async-test helper (`conway-core` has no `tokio`/
    /// `futures-executor` dependency, even in dev-deps): every hook exercised
    /// by this module's tests does no real `.await`ing internally, so a
    /// single poll with a no-op waker always resolves `Ready` -- this is not
    /// a general-purpose executor, just enough to drive `async_trait`'s
    /// synchronous-bodied futures to completion in a unit test.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);

        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
                return val;
            }
        }
    }

    /// Object-safety proof (mirrors this module's own `_assert_object_safe`
    /// pattern in `ports/mod.rs`): `RuntimeDeps` needs to hold this as a
    /// trait object.
    #[test]
    fn context_hook_is_object_safe() {
        fn assert_object_safe(_: &dyn ContextHook) {}
        let hook = BeforeRequestOnlyHook;
        assert_object_safe(&hook);
    }

    // ---- Tool::render's default implementation ----

    /// A tool that accepts the trait's default `render` untouched -- proves
    /// a third-party `Tool` implementor (`ConwayBuilder::with_plugin`, GP-03)
    /// keeps compiling without implementing the new method (the widening
    /// this trait underwent to fix the "pattern grants are inert" bug).
    struct DefaultRenderTool;

    #[async_trait]
    impl Tool for DefaultRenderTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: crate::ids::ToolName::new("probe"),
                description: "test".into(),
                schema: schemars::schema_for!(serde_json::Value),
                category: crate::content::ToolCategory::Read,
                permission: crate::content::PermissionClass::Safe,
            }
        }

        async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
            unreachable!("not exercised by this test")
        }
    }

    #[test]
    fn default_render_reproduces_the_pre_widening_name_args_shape() {
        let tool = DefaultRenderTool;
        let rendered = tool.render(&serde_json::json!({"a": 1}));
        assert_eq!(rendered, "probe({\"a\":1})");
    }

    /// `Tool` must remain object-safe: `PluginRegistry`/third-party plugin
    /// consumers hold it as `Arc<dyn Tool>`.
    #[test]
    fn tool_is_object_safe() {
        fn assert_object_safe(_: &dyn Tool) {}
        let tool = DefaultRenderTool;
        assert_object_safe(&tool);
    }
}
