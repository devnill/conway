//! `ToolRunner`: owns tool dispatch (WI-079, architecture §4.2, §8) —
//! name resolution, argument validation, permission gating, bounded
//! concurrent execution, cancellation, truncation enforcement, and per-call
//! event emission.
//!
//! ## A documented interpretation gap: `rendered`
//!
//! Architecture §4.3 describes `PermissionRequest::rendered` as "a
//! human-readable one-liner from the tool", implying `Tool` itself renders
//! its own call. The committed `conway_core::ports::Tool` trait (WI-061)
//! has no such method — only `spec()` and `invoke()`. Since extending that
//! trait is out of this crate's file scope, [`render_call`] synthesizes a
//! generic one-liner from the tool name and canonicalized arguments
//! instead. This should be raised against `MODULE:conway-core`/`conway-tools`
//! as a request for a per-tool renderer; until then every tool call renders
//! identically regardless of what it does.
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
    CancellationToken as CoreCancellationToken, EventSinkHandle, PluginConfig, SubagentHost,
    ToolCtx, ToolOutput,
};
use futures::FutureExt;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

use crate::events::{BusSink, EventBus};
use crate::permission::{AuthorizedCall, PermissionBroker, PermissionCtx, PermissionOutcome};

use super::registry::PluginRegistry;

/// Everything one call to [`ToolRunner::run_batch`] needs beyond the calls
/// themselves: the requesting agent's identity/position, cancellation, and
/// the shared dependencies each invoked tool needs in its `ToolCtx`.
pub struct ToolBatchCtx {
    pub agent_id: AgentId,
    pub agent_path: Vec<AgentId>,
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub cancel: TokioCancellationToken,
    pub subagents: Arc<dyn SubagentHost>,
    pub plugin_config: Arc<PluginConfig>,
    pub max_parallel_tools: usize,
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
        Self {
            call_id,
            tool,
            blocks: vec![ContentBlock::Text {
                text: message.into(),
            }],
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
        }
    }

    /// Dispatches every call in `calls`, bounding concurrent tool
    /// *invocation* (not resolution/permission-checking) to
    /// `ctx.max_parallel_tools`. Returns one [`ToolOutcome`] per input call,
    /// in input order, regardless of completion order.
    pub async fn run_batch(&self, ctx: &ToolBatchCtx, calls: Vec<ToolCall>) -> Vec<ToolOutcome> {
        let total = calls.len();
        let semaphore = Arc::new(Semaphore::new(ctx.max_parallel_tools.max(1)));
        let mut set = tokio::task::JoinSet::new();

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
            let cwd = ctx.cwd.clone();
            let subagents = ctx.subagents.clone();
            let plugin_config = ctx.plugin_config.clone();
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
                    semaphore,
                    batch_cancel,
                    agent_id,
                    agent_path,
                    session_id,
                    cwd,
                    subagents,
                    plugin_config,
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
    semaphore: Arc<Semaphore>,
    batch_cancel: TokioCancellationToken,
    agent_id: AgentId,
    agent_path: Vec<AgentId>,
    session_id: SessionId,
    cwd: PathBuf,
    subagents: Arc<dyn SubagentHost>,
    plugin_config: Arc<PluginConfig>,
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
        rendered: render_call(&call),
    };
    let perm_ctx = PermissionCtx {
        agent_id,
        agent_path,
        session: session_id,
        cwd: cwd.clone(),
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
            // task per dispatched call (cycle-1 review C1).
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
                cancel: core_cancel,
                events: Arc::new(BusSink::new(bus.clone(), session_id, agent_id)) as EventSinkHandle,
                subagents: subagents.clone(),
                config: plugin_config.clone(),
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

    bus.emit(
        session_id,
        agent_id,
        Event::ToolCallFinished {
            call_id: call_id.clone(),
            is_error: outcome.is_error,
            preview: preview_text(&outcome.blocks),
        },
    );

    outcome
}

/// A generic one-liner rendering of a proposed call — see the module doc
/// comment's "documented interpretation gap" section.
fn render_call(call: &ToolCall) -> String {
    format!("{}({})", call.name, call.arguments)
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
        TruncationPolicy::None | TruncationPolicy::Artifact => return None,
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
    // bytes, inflating the audit record (cycle-1 review S1).
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
