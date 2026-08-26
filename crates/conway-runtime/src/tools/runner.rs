//! `ToolRunner`: owns tool dispatch (architecture §4.2, §8) —
//! name resolution, argument validation, permission gating, bounded
//! concurrent execution, cancellation, truncation enforcement, and per-call
//! event emission.
//!
//! ## `rendered`: per-tool, via `Tool::render`
//!
//! Architecture §4.3 describes `PermissionRequest::rendered` as "a
//! human-readable one-liner from the tool" — `conway_core::ports::Tool` now
//! has exactly that method (`Tool::render`, with a default reproducing the
//! old generic `name(args)` shape for any tool that doesn't need something
//! more specific). `render_call` calls it on the resolved tool instance
//! and sanitizes the result (`sanitize_rendered`) before it becomes
//! `AuthorizedCall::rendered` — the single seam every consumer of
//! `rendered` (the permission prompt, `Event::PermissionRequested`,
//! `PatternRule` prefix matching) shares. Previously this synthesized a
//! generic one-liner unconditionally, which made every `PatternRule` grant
//! permanently inert (`bash`'s rendering always carried the JSON
//! metacharacters `(){}`, which `PatternRule::matches`'s hard gate rejects
//! by design) — see `bash`'s `Tool::render` override, which fixes that by
//! rendering the bare command string instead of a JSON dump.
//!
//! ## Cancellation bridging
//!
//! [`ToolBatchCtx::cancel`] is a `tokio_util::sync::CancellationToken` (the
//! async-awaitable kind), per `conway_core::ports::CancellationToken`'s own
//! doc comment, which names this crate as the place such a bridge belongs.
//! Each call derives a `child_token()` from it and races that token's
//! `cancelled()` against the tool's `invoke` future via `tokio::select!` —
//! so a stuck or uncooperative tool is abandoned promptly (the losing
//! `select!` branch is dropped) rather than relying solely on the tool
//! polling `ToolCtx::cancel`. The same signal is *also* forwarded into a
//! fresh `conway_core::ports::CancellationToken` handed to the tool via
//! `ToolCtx`, so well-behaved tools still see the cooperative flag.

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;

use conway_core::content::{Artifact, ContentBlock, ToolCall, TruncationPolicy, TruncationRecord};
use conway_core::error::ToolError;
use conway_core::event::Event;
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::{
    CancellationToken as CoreCancellationToken, CapabilityCallHandle, CapabilityHost,
    ContextPathHandle, ContextPathHost, CwdHandle, EventSinkHandle, PluginConfig,
    PluginEventEmitter, PluginEventHandle, SessionDiscoveryHandle, SessionDiscoveryHost,
    SubagentHandle, SubagentHost, Tool, ToolCtx, ToolOutput,
};
use futures::FutureExt;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

use crate::events::{BusSink, EventBus};
use crate::hook_dispatch::{self, HookDispatcher};
use crate::permission::{
    AgentRoot, AuthorizedCall, PermissionBroker, PermissionCtx, PermissionOutcome,
};

use super::registry::PluginRegistry;

/// Everything one call to [`ToolRunner::run_batch`] needs beyond the calls
/// themselves: the requesting agent's identity/position, cancellation, and
/// the shared dependencies each invoked tool needs in its `ToolCtx`.
pub struct ToolBatchCtx {
    pub agent_id: AgentId,
    pub agent_path: Vec<AgentId>,
    pub session_id: SessionId,
    /// S1: the durable "cd" cell, owned by `AgentLoop` and cloned into every
    /// turn's batch. `run_batch` reads [`CwdHandle::current`] exactly once,
    /// at its own top, before spawning anything -- see that function's own
    /// doc for why the snapshot is taken there rather than here (the caller
    /// building this struct) even though both are synchronous and therefore
    /// equivalent in practice: the snapshot's home is documented at the one
    /// place that actually matters for the no-race guarantee.
    pub chdir: CwdHandle,
    pub cancel: TokioCancellationToken,
    pub subagents: Arc<dyn SubagentHost>,
    /// The context-path composition capability every dispatched tool's
    /// `ToolCtx::context_path` is narrowed from (decision
    /// `01M0K4QT6MBXPD6PXMBBBD2P7B`; [`ContextPathHost`]'s own module doc) --
    /// mirrors [`Self::subagents`] exactly: one runtime-wide `Arc`, narrowed
    /// to this batch's own `session_id` per call, below.
    pub context_path_host: Arc<dyn ContextPathHost>,
    /// The cross-session discovery capability every dispatched tool's
    /// `ToolCtx::session_discovery` is built from (board item
    /// `01M0PS8J3AK7Z7253Z3E3RD3GY`) -- mirrors [`Self::context_path_host`]
    /// exactly: one runtime-wide `Arc`, cross-session by construction so
    /// (unlike `context_path_host`) nothing narrows it per call.
    pub session_discovery_host: Arc<dyn SessionDiscoveryHost>,
    /// Edge B's plugin -> plugin capability CALL channel (board item
    /// `01M0XXWV3BVDM6Y646WMEBTYT1`; `conway_core::ports::capability`'s own
    /// module doc) -- mirrors [`Self::context_path_host`]/
    /// [`Self::session_discovery_host`] exactly: one runtime-wide `Arc`
    /// (`RuntimeDeps::capabilities`, via `LoopDeps::capabilities`), narrowed
    /// per call below into a [`CapabilityCallHandle`] bound to THAT call's
    /// own resolved tool's declaring plugin id -- never a registry-wide
    /// identity, so provenance stays per-call exactly like
    /// `plugin_events`'s `caller_plugin_id` already does.
    pub capability_host: Arc<dyn CapabilityHost>,
    pub plugin_config: Arc<PluginConfig>,
    pub max_parallel_tools: usize,
    /// S5: this agent's confinement root, reconstructed exactly once per
    /// agent (`AgentLoop::run_inner`, alongside `chdir`'s own cell) and
    /// cloned in unchanged here for every call in this batch -- see
    /// `AgentRoot`'s own doc for why cloning it is cheap and why
    /// reconstructing it more often than once per agent would be wrong
    /// (hazard #8 in this slice's own inventory: a TOCTOU widening, and
    /// slow).
    pub root: AgentRoot,
}

/// The outcome of one dispatched tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutcome {
    pub call_id: String,
    pub tool: ToolName,
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    pub truncation: Option<TruncationRecord>,
    pub artifacts: Vec<Artifact>,
}

impl ToolOutcome {
    fn error(call_id: String, tool: ToolName, message: impl Into<String>) -> Self {
        // F3: sanitize at construction. A deny reason or error message can
        // carry attacker-influenced content (a filename, a tool argument
        // echoed back), and this `Text` block flows straight into model
        // context. A raw control character here is an injection surface into
        // the transcript (an ANSI escape rendered by the TUI, a `\n` that
        // splits a tool result into what a model parses as two turns), so
        // every runner-synthesized error path is covered here at construction
        // rather than by each caller remembering to call the sanitizer. See
        // `conway_core::text::sanitize_control_chars`.
        //
        // Scope -- this is the SYNTHESIZED-string surface only. It covers the
        // error strings the runner itself builds: preflight denies, an
        // `invoke` error, a panic. A tool's OWN output (the `Ok(output)` arm
        // above, including `is_error: true` from e.g. a non-zero `bash` exit)
        // is a different surface and is passed through verbatim: that is data
        // the model reads, where `\n`/`\t` are legitimate structure (bash's
        // `stdout:\n...\nstderr:\n...`), not a harness-authored string.
        // Replacing control chars there would corrupt the data. Whether
        // tool-produced output warrants selective handling (e.g. stripping
        // ANSI escapes while keeping whitespace structure) is a separate
        // question, out of scope for this item.
        let text = conway_core::text::sanitize_control_chars(&message.into());
        Self {
            call_id,
            tool,
            blocks: vec![ContentBlock::Text { text }],
            is_error: true,
            truncation: None,
            artifacts: Vec::new(),
        }
    }
}

/// Owns tool dispatch for a whole runtime: name resolution against a
/// [`PluginRegistry`], permission gating via a [`PermissionBroker`], and
/// event emission through an [`EventBus`]. Cheap to clone-share (every
/// field is an `Arc`).
pub struct ToolRunner {
    registry: Arc<PluginRegistry>,
    broker: Arc<PermissionBroker>,
    bus: Arc<EventBus>,
    /// `post_tool_use` dispatch.
    /// Constructed here rather than taken as a parameter so `new` keeps its
    /// arity -- five test call sites build a `ToolRunner` directly -- and
    /// read back by `Runtime::new` via [`Self::hooks`] so the runtime
    /// and this runner share one interior-mutable dispatcher.
    hooks: Arc<HookDispatcher>,
}

impl ToolRunner {
    pub fn new(
        registry: Arc<PluginRegistry>,
        broker: Arc<PermissionBroker>,
        bus: Arc<EventBus>,
    ) -> Self {
        Self {
            registry,
            broker,
            bus,
            hooks: Arc::new(HookDispatcher::new()),
        }
    }

    /// The `post_tool_use` dispatcher this runner will consult, so
    /// `Runtime::new` can hold the same one and wire config onto it. Until
    /// something injects a runner into it, every dispatch is a no-op.
    pub fn hooks(&self) -> Arc<HookDispatcher> {
        self.hooks.clone()
    }

    /// Dispatches every call in `calls`, bounding concurrent tool
    /// *invocation* (not resolution/permission-checking) to
    /// `ctx.max_parallel_tools`. Returns one [`ToolOutcome`] per input call,
    /// in input order, regardless of completion order.
    pub async fn run_batch(&self, ctx: &ToolBatchCtx, calls: Vec<ToolCall>) -> Vec<ToolOutcome> {
        let total = calls.len();
        let semaphore = Arc::new(Semaphore::new(ctx.max_parallel_tools.max(1)));
        let mut set = tokio::task::JoinSet::new();

        // S1: the no-race guarantee. Snapshot the cwd cell exactly ONCE,
        // here, before any call in this batch is spawned -- every task
        // below shares this same `PathBuf`, so a `cd` proposed by one call
        // in this batch can never be observed by another call in the SAME
        // batch, regardless of dispatch/completion order. It takes effect
        // starting the next `run_batch` invocation, which reads
        // `ctx.chdir.current()` fresh. Unix has the identical constraint
        // (threads share one process cwd); this mirrors it deliberately
        // rather than emulating a per-task cwd around it.
        let cwd_snapshot = ctx.chdir.current();

        for (index, call) in calls.into_iter().enumerate() {
            let registry = self.registry.clone();
            let broker = self.broker.clone();
            let bus = self.bus.clone();
            let bus_for_panic = self.bus.clone();
            let semaphore = semaphore.clone();
            let batch_cancel = ctx.cancel.clone();
            let agent_id = ctx.agent_id;
            let agent_path = ctx.agent_path.clone();
            let session_id = ctx.session_id;
            let cwd = cwd_snapshot.clone();
            let chdir = ctx.chdir.clone();
            let subagents = ctx.subagents.clone();
            let context_path_host = ctx.context_path_host.clone();
            let session_discovery_host = ctx.session_discovery_host.clone();
            let capability_host = ctx.capability_host.clone();
            let plugin_config = ctx.plugin_config.clone();
            let root = ctx.root.clone();
            let hooks = self.hooks.clone();
            let call_id_for_panic = call.call_id.clone();
            let tool_for_panic = call.name.clone();

            set.spawn(async move {
                // A panicking `Tool::invoke` must yield an error outcome,
                // never abort the batch or the process (architecture §8).
                // `catch_unwind` here — rather than relying on the JoinSet's
                // own `JoinError::is_panic()` path — means every branch of
                // `set.join_next()` below is `Ok`, so index/outcome pairing
                // never has to be reconstructed from a lost task.
                let outcome = AssertUnwindSafe(execute_one(
                    registry,
                    broker,
                    bus,
                    hooks,
                    semaphore,
                    batch_cancel,
                    agent_id,
                    agent_path,
                    session_id,
                    cwd,
                    chdir,
                    subagents,
                    context_path_host,
                    session_discovery_host,
                    capability_host,
                    plugin_config,
                    root,
                    call,
                ))
                .catch_unwind()
                .await
                .unwrap_or_else(|payload| {
                    let detail = panic_message(payload);
                    let outcome = ToolOutcome::error(
                        call_id_for_panic.clone(),
                        tool_for_panic.clone(),
                        format!("tool `{tool_for_panic}` panicked: {detail}"),
                    );
                    bus_for_panic.emit(
                        session_id,
                        agent_id,
                        Event::ToolCallFinished {
                            call_id: call_id_for_panic,
                            is_error: true,
                            preview: preview_text(&outcome.blocks),
                        },
                    );
                    outcome
                });
                (index, outcome)
            });
        }

        let mut results: Vec<Option<ToolOutcome>> = (0..total).map(|_| None).collect();
        while let Some(joined) = set.join_next().await {
            let (index, outcome) =
                joined.expect("execute_one is wrapped in catch_unwind; its task cannot panic");
            results[index] = Some(outcome);
        }
        results
            .into_iter()
            .map(|outcome| outcome.expect("every spawned index yields exactly one result"))
            .collect()
    }
}

/// Runs one call end to end: resolve → validate → propose → authorize →
/// (start → invoke → truncate) → finish. Free function (not a method) so it
/// owns everything it needs and can be spawned as an independent, `'static`
/// task per call.
#[allow(clippy::too_many_arguments)]
async fn execute_one(
    registry: Arc<PluginRegistry>,
    broker: Arc<PermissionBroker>,
    bus: Arc<EventBus>,
    hooks: Arc<HookDispatcher>,
    semaphore: Arc<Semaphore>,
    batch_cancel: TokioCancellationToken,
    agent_id: AgentId,
    agent_path: Vec<AgentId>,
    session_id: SessionId,
    cwd: PathBuf,
    chdir: CwdHandle,
    subagents: Arc<dyn SubagentHost>,
    context_path_host: Arc<dyn ContextPathHost>,
    session_discovery_host: Arc<dyn SessionDiscoveryHost>,
    capability_host: Arc<dyn CapabilityHost>,
    plugin_config: Arc<PluginConfig>,
    root: AgentRoot,
    call: ToolCall,
) -> ToolOutcome {
    let call_id = call.call_id.clone();
    let tool_name = call.name.clone();

    let Some(resolved) = registry.resolve(&tool_name) else {
        return ToolOutcome::error(
            call_id,
            tool_name.clone(),
            format!("unknown tool `{tool_name}`"),
        );
    };

    // Schema-invalid arguments never reach `ToolCallProposed`/permission/
    // `invoke` — the model's call is malformed before it is meaningfully a
    // "proposal" at all.
    if let Err(message) = resolved.validate(&call.arguments) {
        return ToolOutcome::error(call_id, tool_name, message);
    }

    bus.emit(
        session_id,
        agent_id,
        Event::ToolCallProposed {
            call_id: call_id.clone(),
            tool: tool_name.clone(),
            args: call.arguments.clone(),
        },
    );

    let authorized = AuthorizedCall {
        call_id: call_id.clone(),
        tool: tool_name.clone(),
        category: resolved.spec.category,
        arguments: call.arguments.clone(),
        rendered: render_call(resolved.tool.as_ref(), &call.arguments),
        // S5: a cheap, static, call-independent property of the resolved
        // tool -- this is the seam that gets the broker's root check
        // `Tool::path_args` without duplicating tool resolution at the
        // broker's own decision point (which has no `PluginRegistry`
        // access, and must not gain one just for this).
        path_args: resolved.tool.path_args(),
        // same seam, same reasoning,
        // for the metacharacter gate's applicability rather than the root
        // check's.
        render_kind: resolved.tool.render_kind(),
    };
    let perm_ctx = PermissionCtx {
        agent_id,
        // Cloned rather than moved: `post_tool_use`'s payload below names the
        // same path, and the two consumers are on opposite sides of the call.
        agent_path: agent_path.clone(),
        session: session_id,
        cwd: cwd.clone(),
        root,
    };

    match broker.decide(&perm_ctx, &authorized).await {
        PermissionOutcome::Allow => {}
        // A denial is model-visible feedback, never an abort: no
        // `ToolCallStarted` is emitted and `invoke` is never called.
        PermissionOutcome::Deny { rendered_error } => {
            return ToolOutcome::error(call_id, tool_name, rendered_error);
        }
    }

    let call_cancel = batch_cancel.child_token();
    let outcome = tokio::select! {
        biased;
        _ = call_cancel.cancelled() => {
            ToolOutcome::error(call_id.clone(), tool_name.clone(), ToolError::Cancelled.to_string())
        }
        outcome = async {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("tool semaphore is never closed");

            bus.emit(
                session_id,
                agent_id,
                Event::ToolCallStarted {
                    call_id: call_id.clone(),
                },
            );

            // Bridge the async-awaitable cancellation this function races
            // against into the polling-style token `ToolCtx` carries, so a
            // cooperative tool observes the same signal. The watcher's
            // handle is aborted once `invoke` returns: on the ordinary
            // (non-cancelled) path `watch.cancelled()` never resolves, and
            // an unaborted watcher would outlive the call — one leaked live
            // task per dispatched call.
            let core_cancel = CoreCancellationToken::new();
            let bridge = {
                let core_cancel = core_cancel.clone();
                let watch = call_cancel.clone();
                tokio::spawn(async move {
                    watch.cancelled().await;
                    core_cancel.cancel();
                })
            };

            let tool_ctx = ToolCtx {
                agent_id,
                session_id,
                cwd: cwd.clone(),
                chdir: chdir.clone(),
                cancel: core_cancel,
                events: Arc::new(BusSink::new(bus.clone(), session_id, agent_id)) as EventSinkHandle,
                // The agent's OWN id is baked into the handle here -- the
                // one place a `ToolCtx` is built for real tool dispatch
                // (`conway-testkit` fakes and `conway-tools` test doubles
                // are the only other construction sites C1). No
                // tool this handle reaches can ever act as a different
                // agent (structural -- see `SubagentHandle`'s own
                // doc).
                subagents: SubagentHandle::new(subagents.clone(), agent_id),
                // Narrowed to THIS call's own session -- mirrors
                // `subagents` immediately above exactly (a caller-bound
                // handle, never the raw runtime-wide host).
                context_path: ContextPathHandle::new(context_path_host.clone(), session_id),
                // Cross-session by construction -- no session to bind, see
                // `SessionDiscoveryHandle`'s own doc.
                session_discovery: SessionDiscoveryHandle::new(session_discovery_host.clone()),
                // bound to THIS
                // call's own resolved tool's declaring plugin id, never a
                // different one -- `hooks` (this runner's own
                // `HookDispatcher`, already shared with `Runtime::
                // set_observation_hooks`) is the SAME fan-out layer
                // `post_tool_use` etc. dispatch through (`impl
                // PluginEventEmitter for HookDispatcher`,
                // `hook_dispatch.rs`) -- "one dispatch path", not a second
                // one built just for this.
                plugin_events: PluginEventHandle::new(
                    hooks.clone() as Arc<dyn PluginEventEmitter>,
                    resolved.plugin_id.to_string(),
                ),
                config: plugin_config.clone(),
                // Edge B (`docs/vision/DESIGN-plugin-dependencies.md` §2):
                // the plugin -> plugin capability call channel
                // (`conway_core::ports::capability`'s own module doc). Bound
                // to `capability_host` -- the REAL, runtime-wide
                // `CapabilityRegistry` `RuntimeDeps::capabilities` threads in
                // via `LoopDeps`/`ToolBatchCtx` (board item
                // `01M0XXWV3BVDM6Y646WMEBTYT1`; `01M0WWNHQQYN1EVTH8WPZ33EBF`
                // built the channel and its build-time resolution check but
                // left every live dispatched call unconditionally refused) --
                // paired with THIS call's own resolved tool's declaring
                // plugin id for caller provenance, mirroring `plugin_events`
                // immediately above exactly: the handle's identity is
                // per-call, never the registry's own.
                capabilities: CapabilityCallHandle::new(
                    capability_host.clone(),
                    resolved.plugin_id.to_string(),
                ),
            };

            let invoked = resolved.tool.invoke(call.clone(), tool_ctx).await;
            bridge.abort();
            match invoked {
                Ok(mut output) => {
                    let truncation = apply_truncation(&mut output);
                    ToolOutcome {
                        call_id: call_id.clone(),
                        tool: tool_name.clone(),
                        blocks: output.blocks,
                        is_error: output.is_error,
                        truncation,
                        artifacts: output.artifacts,
                    }
                }
                Err(err) => ToolOutcome::error(call_id.clone(), tool_name.clone(), err.to_string()),
            }
        } => outcome,
    };

    let preview = preview_text(&outcome.blocks);

    bus.emit(
        session_id,
        agent_id,
        Event::ToolCallFinished {
            call_id: call_id.clone(),
            is_error: outcome.is_error,
            preview: preview.clone(),
        },
    );

    // `post_tool_use`, dispatched at
    // the same seam that emits `ToolCallFinished` because that is the point
    // which already knows the call finished and what it produced.
    //
    // OBSERVATION ONLY, and structurally so: `dispatch` returns `()`, so a
    // hook that fails or times out cannot turn into a failure of the call it
    // observed -- `outcome` below is returned unchanged either way. There is
    // deliberately no denial path here; the call has already run, so there is
    // nothing left to deny.
    if hooks.will_dispatch(hook_dispatch::POST_TOOL_USE) {
        hooks
            .dispatch(
                hook_dispatch::POST_TOOL_USE,
                serde_json::json!({
                    "call_id": call_id,
                    "tool": tool_name.as_str(),
                    "is_error": outcome.is_error,
                    "preview": preview,
                    "agent_id": agent_id,
                    "agent_path": agent_path,
                    "session": session_id,
                }),
            )
            .await;
    }

    outcome
}

/// The single seam where every proposed call's `rendered` text is produced
/// — see the module doc comment. Defers to the resolved tool's own
/// [`Tool::render`] (rather than synthesizing a generic form here) and
/// sanitizes the result before it becomes `AuthorizedCall::rendered`.
fn render_call(tool: &dyn Tool, args: &serde_json::Value) -> String {
    sanitize_rendered(&tool.render(args))
}

/// A tool's rendering is derived from model-supplied arguments and is
/// therefore UNTRUSTED. Replaces every Unicode control character (`Cc`:
/// `\x00`-`\x1F`, `\x7F`, and the C1 controls `\x80`-`\x9F`) with the
/// Unicode replacement character, so a model-supplied argument containing
/// e.g. an ANSI escape sequence (`\x1b[...`) cannot reach the TUI's
/// permission prompt (or any other consumer of `rendered`) as raw
/// terminal-control bytes. Applied once, here, rather than by each `Tool`
/// implementation, so the guarantee holds for the default rendering AND
/// every override without each one needing to know about it.
///
/// Delegates to `conway_core::text::sanitize_control_chars` -- the single
/// shared home for the replace-semantics sanitizer -- so this seam and the
/// permission gate's laundering-recognition cannot drift apart. See that
/// module's doc for why a `filter` variant is deliberately NOT provided
/// there.
fn sanitize_rendered(rendered: &str) -> String {
    conway_core::text::sanitize_control_chars(rendered)
}

/// The first 200 chars of the first `Text` block, or empty if there is
/// none — the `ToolCallFinished.preview` contract.
fn preview_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.chars().take(200).collect()),
            _ => None,
        })
        .unwrap_or_default()
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Enforces `output.truncation` against its own declared byte budget,
/// mutating `output.blocks` in place and returning the record to persist,
/// or `None` if no truncation was needed.
///
/// Scope note: this only truncates when every block is `ContentBlock::Text`
/// (concatenated as one logical string) — the shape every built-in tool
/// produces (`conway-tools`' `common::text_output`). A `ToolOutput` mixing
/// text with other block kinds is left untouched; extending byte-accounting
/// to mixed content is not exercised by this item's criteria.
fn apply_truncation(output: &mut ToolOutput) -> Option<TruncationRecord> {
    let policy = output.truncation;
    let (limit, mode) = match policy {
        TruncationPolicy::None => return None,
        TruncationPolicy::Head { max_bytes } => (max_bytes, TruncMode::Head),
        TruncationPolicy::Tail { max_bytes } => (max_bytes, TruncMode::Tail),
        TruncationPolicy::HeadTail {
            head_bytes,
            tail_bytes,
        } => (
            head_bytes.saturating_add(tail_bytes),
            TruncMode::HeadTail(head_bytes, tail_bytes),
        ),
        // `TruncationPolicy` is `#[non_exhaustive]`: a future variant this
        // crate doesn't know how to size is treated like `None` — leave the
        // content untouched rather than guessing a budget.
        _ => return None,
    };

    if output.blocks.is_empty()
        || !output
            .blocks
            .iter()
            .all(|block| matches!(block, ContentBlock::Text { .. }))
    {
        return None;
    }
    let text: String = output
        .blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.as_str(),
            _ => unreachable!("checked above"),
        })
        .collect();

    let original_bytes = text.len() as u64;
    if original_bytes <= limit {
        return None;
    }

    // `kept_bytes` counts bytes retained FROM THE ORIGINAL content — the
    // rendered string's length would also include the elision marker's own
    // bytes, inflating the audit record.
    let (truncated, kept_bytes) = match mode {
        TruncMode::Head => truncate_head(&text, limit as usize),
        TruncMode::Tail => truncate_tail(&text, limit as usize),
        TruncMode::HeadTail(head, tail) => truncate_head_tail(&text, head as usize, tail as usize),
    };
    output.blocks = vec![ContentBlock::Text { text: truncated }];
    Some(TruncationRecord {
        policy,
        original_bytes,
        kept_bytes,
    })
}

enum TruncMode {
    Head,
    Tail,
    HeadTail(u64, u64),
}

/// The largest index `<= max_bytes` that lands on a UTF-8 char boundary.
fn char_boundary_at_most(s: &str, max_bytes: usize) -> usize {
    let mut idx = max_bytes.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// The smallest index `>= min_bytes_from_start` that lands on a UTF-8 char
/// boundary.
fn char_boundary_at_least(s: &str, min_bytes_from_start: usize) -> usize {
    let mut idx = min_bytes_from_start.min(s.len());
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn truncate_head(text: &str, max_bytes: usize) -> (String, u64) {
    let boundary = char_boundary_at_most(text, max_bytes);
    let omitted = text.len() - boundary;
    (
        format!("{}\n… ({omitted} bytes omitted)", &text[..boundary]),
        boundary as u64,
    )
}

fn truncate_tail(text: &str, max_bytes: usize) -> (String, u64) {
    let start = char_boundary_at_least(text, text.len().saturating_sub(max_bytes));
    let omitted = start;
    (
        format!("… ({omitted} bytes omitted)\n{}", &text[start..]),
        (text.len() - start) as u64,
    )
}

fn truncate_head_tail(text: &str, head_bytes: usize, tail_bytes: usize) -> (String, u64) {
    let head_boundary = char_boundary_at_most(text, head_bytes);
    let tail_start_min = text.len().saturating_sub(tail_bytes).max(head_boundary);
    let tail_start = char_boundary_at_least(text, tail_start_min);
    let omitted = tail_start - head_boundary;
    (
        format!(
            "{}\n… ({omitted} bytes omitted)\n{}",
            &text[..head_boundary],
            &text[tail_start..]
        ),
        (head_boundary + (text.len() - tail_start)) as u64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rendered_is_a_no_op_on_ordinary_text() {
        assert_eq!(
            sanitize_rendered("git status --short"),
            "git status --short"
        );
    }

    /// The concrete threat this exists for — a model-supplied
    /// argument smuggling an ANSI escape sequence into the permission
    /// prompt must not reach the terminal as a raw ESC byte.
    #[test]
    fn sanitize_rendered_neutralizes_ansi_escape_sequences() {
        let sanitized = sanitize_rendered("git status\x1b[31m; rm -rf /\x1b[0m");
        assert!(!sanitized.contains('\x1b'), "{sanitized:?}");
        assert!(sanitized.contains('\u{FFFD}'), "{sanitized:?}");
    }

    #[test]
    fn sanitize_rendered_neutralizes_other_control_bytes() {
        for raw in ["a\0b", "a\nb", "a\rb", "a\tb", "a\x07b", "a\x7fb"] {
            let sanitized = sanitize_rendered(raw);
            assert!(
                sanitized.chars().all(|c| !c.is_control()),
                "{raw:?} -> {sanitized:?}"
            );
        }
    }

    /// A minimal `Tool` whose `render` is the trait's default, to prove
    /// `render_call` reaches the tool instance rather than re-implementing
    /// the rendering itself.
    struct ProbeTool;

    #[async_trait::async_trait]
    impl Tool for ProbeTool {
        fn spec(&self) -> conway_core::content::ToolSpec {
            conway_core::content::ToolSpec {
                name: ToolName::new("probe"),
                description: "test".into(),
                schema: schemars::schema_for!(serde_json::Value),
                category: conway_core::content::ToolCategory::Read,
                permission: conway_core::content::PermissionClass::Safe,
            }
        }

        async fn invoke(
            &self,
            _call: ToolCall,
            _ctx: ToolCtx,
        ) -> Result<ToolOutput, conway_core::error::ToolError> {
            unreachable!("not exercised by this test")
        }
    }

    #[test]
    fn render_call_uses_the_resolved_tool_and_sanitizes_its_output() {
        let rendered = render_call(&ProbeTool, &serde_json::json!({"a": "x\x1by"}));
        assert!(rendered.starts_with("probe("), "{rendered:?}");
        assert!(!rendered.contains('\x1b'), "{rendered:?}");
    }

    /// F3: `ToolOutcome::error` constructs a `Text` block that flows straight
    /// into model context, and a deny reason / error message can carry
    /// attacker-influenced content. Every raw control character in the input
    /// must be rewritten (to `U+FFFD`, the same placeholder the `rendered`
    /// seam produces), never dropped and never passed through. This is
    /// applied at construction so callers cannot forget to call it.
    #[test]
    fn tool_outcome_error_strips_raw_control_characters_from_model_context() {
        // Carries a newline, a carriage return, and a full ANSI SGR escape
        // sequence (`\x1b[31m...\x1b[0m`) -- the three vectors the spec names.
        let message = "denied: path 'p\x1b[31m/evil\x1b[0m'\nsecond line\rfinal";
        let outcome = ToolOutcome::error("call_x".into(), ToolName::new("bash"), message);
        let text = match outcome.blocks.as_slice() {
            [ContentBlock::Text { text }] => text.clone(),
            other => panic!("expected one Text block, got {other:?}"),
        };
        assert!(
            text.chars().all(|c| !c.is_control()),
            "error output must contain no raw control characters: {text:?}"
        );
        // Replace, not drop: each control char becomes evidence (U+FFFD),
        // so the gate's laundering-recognition still sees it if this text
        // is ever fed back through a render/match path.
        assert!(
            text.contains('\u{FFFD}'),
            "must replace, not drop: {text:?}"
        );
        // No raw ESC, no raw newline, no raw CR survived.
        assert!(!text.contains('\x1b'), "no raw ESC: {text:?}");
        assert!(!text.contains('\n'), "no raw newline: {text:?}");
        assert!(!text.contains('\r'), "no raw CR: {text:?}");
    }
}
