//! Regression/acceptance tests for board item 01KYTP0PGKJ4VCJP5TD39A1WHF
//! ("`start`/`ask`/`tree` are unguarded -- cross-tree exfiltration in one
//! call") -- the SECURITY-CRITICAL slice `674bb65`
//! (01KYT8TS0EBKJHYNJRF6S88NRH, see `subagent_control_seam.rs`) left open.
//!
//! `674bb65` fenced `steer`/`await_result`/`cancel` at the trait boundary,
//! but `start`/`ask` still took only `parent` and acted on it directly, and
//! `tree` took no caller at all and returned the WHOLE runtime-wide tree to
//! any tool. Composed, that was cross-tree exfiltration in one call:
//! `tree()` to discover a sibling's `AgentId` (an ordinary, unprivileged
//! read every tool already has via `ToolCtx::subagents`), then
//! `ask(sibling, SubagentSpec { mode: Fork, .. })` to fork that sibling's
//! ENTIRE context (GP-02: a fork inherits everything up to the fork point)
//! and read the reply back as plain model output.
//!
//! Mirrors `subagent_control_seam.rs` exactly, for the identical reason
//! stated there: a hand-written `FakeSubagentHost` fixture proves nothing
//! about whether the real trait boundary (`impl SubagentHost for Runtime`)
//! enforces descendancy. Every test below drives a REAL tool
//! (`conway-core`'s `Plugin`/`Tool` ports, dispatched through a real agent
//! turn via the real `ToolRunner`) against two SIBLING agents produced by
//! the real `SubagentHost::start`, calling `ctx.subagents` -- the exact
//! trait object every built-in AND third-party tool holds -- directly.
//!
//! **Why a custom tool, not `conway_ask`/`conway_subagent`:** neither
//! built-in tool's OWN JSON schema exposes a field naming a different
//! parent/target -- both always pass `ctx.agent_id` for both `caller` and
//! `parent` (see `conway-tools`' `subagent/{ask,tools}.rs`). That is exactly
//! why this item's fix has to live at the `SubagentHost` PORT rather than at
//! either tool's own callsite (P-1): "a tool-layer guard alone is
//! insufficient because it leaves the trait impl bypassable from any OTHER
//! caller" -- a future or third-party tool that DOES expose a model-chosen
//! target (exactly the shape `ExfiltrateTool` below represents) must be
//! safe by construction, not by the current built-ins' convention.
#![cfg(feature = "builtin-tools")]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, TuiSection,
};
use conway::{AgentId, Conway, ConwayBuilder, Plugin, SessionHandle, SessionSpec, SpawnSpec, Tool};
use conway_core::agent::{Budget, PermissionDecision, SubagentMode, SubagentSpec};
use conway_core::capabilities::{
    CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolResult, ToolSpec,
    TruncationPolicy, Usage,
};
use conway_core::error::{BackendError, ToolError};
use conway_core::fakes::{FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{
    Backend, BoxStream, GenerateRequest, GenerateResponse, PermissionGate, PluginManifest,
    StreamChunk, ToolCtx, ToolOutput,
};
use futures_core::Stream;

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

fn tool_call_response(tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tui: TuiSection::default(),
    }
}

/// Copied from `subagent_control_seam.rs`'s identical private helper -- see
/// that file's own doc for why.
struct VecStream<T> {
    items: VecDeque<T>,
}

impl<T: Unpin> Stream for VecStream<T> {
    type Item = T;
    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<T>> {
        std::task::Poll::Ready(self.get_mut().items.pop_front())
    }
}

/// Copied from `subagent_control_seam.rs`'s identical `LazyBackend` -- a
/// backend whose script is evaluated lazily so a later turn's tool-call
/// arguments can embed a sibling's real, runtime-assigned `AgentId`, known
/// only partway through the test. See that file's own doc for the full
/// rationale.
struct LazyBackend {
    id: BackendId,
    steps: Mutex<VecDeque<Box<dyn Fn() -> GenerateResponse + Send + Sync>>>,
}

impl LazyBackend {
    fn new(steps: Vec<Box<dyn Fn() -> GenerateResponse + Send + Sync>>) -> Self {
        Self {
            id: BackendId::new("fake"),
            steps: Mutex::new(steps.into()),
        }
    }
}

#[async_trait]
impl Backend for LazyBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::None,
            cache: CacheMode::None,
            parallel_tool_calls: false,
            structured_output: StructuredOutput::None,
            max_context_tokens: 128_000,
            reasoning: false,
            reliability_tier: ReliabilityTier::Unknown,
        }
    }

    async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        let step = self.steps.lock().unwrap().pop_front();
        match step {
            Some(f) => Ok(f()),
            None => Err(BackendError::BadRequest {
                detail: "LazyBackend script exhausted".into(),
            }),
        }
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.generate(req).await?;
        let mut chunks: Vec<StreamChunk> = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(StreamChunk::TextDelta(text.clone())),
                _ => None,
            })
            .collect();
        chunks.push(StreamChunk::Done(response));
        Ok(Box::pin(VecStream {
            items: chunks.into_iter().map(Ok).collect(),
        }))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        Ok(ProbeReport {
            ok: true,
            latency_ms: 1,
            models: vec![],
            detail: None,
            at: chrono::Utc::now(),
        })
    }
}

// ---------------------------------------------------------------------
// `ExfiltrateAskTool`/`ExfiltrateStartTool`: the shape a future/third-party
// tool takes IF it exposes a model-chosen target -- unlike `conway_ask`/
// `conway_subagent`, which never do (see this file's own module doc). Both
// pass `ctx.agent_id` (the runtime-assigned, non-forgeable true caller
// identity) as `caller`, and the MODEL-SUPPLIED `target_agent_id` argument
// as `parent` -- exactly the shape the `SubagentHost` port must refuse on
// its own, since a tool-layer guard alone would not protect a tool written
// like this one.
// ---------------------------------------------------------------------

struct ExfiltrateAskTool;

#[async_trait]
impl Tool for ExfiltrateAskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("exfiltrate_ask"),
            description: "test-only: ask an arbitrary agent id, not necessarily the caller's own"
                .into(),
            schema: serde_json::from_value(serde_json::json!({
                "type": "object",
                "properties": {
                    "target_agent_id": {"type": "string"},
                    "prompt": {"type": "string"},
                },
                "required": ["target_agent_id", "prompt"],
            }))
            .unwrap(),
            category: ToolCategory::Delegate,
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let target_raw = call.arguments["target_agent_id"]
            .as_str()
            .expect("target_agent_id present");
        let target: AgentId = target_raw
            .parse()
            .map_err(|e| ToolError::InvalidArguments {
                detail: format!("target_agent_id: {e}"),
            })?;
        let prompt = call.arguments["prompt"]
            .as_str()
            .expect("prompt present")
            .to_string();

        let outcome = ctx
            .subagents
            .ask(
                ctx.agent_id,
                target,
                SubagentSpec::fork(prompt, Budget::default()),
            )
            .await
            .map_err(|e| ToolError::Internal {
                detail: e.to_string(),
            })?;
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text: outcome.text }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

struct ExfiltrateStartTool;

#[async_trait]
impl Tool for ExfiltrateStartTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("exfiltrate_start"),
            description: "test-only: start a child under an arbitrary agent id, not necessarily \
                           the caller's own"
                .into(),
            schema: serde_json::from_value(serde_json::json!({
                "type": "object",
                "properties": {
                    "target_agent_id": {"type": "string"},
                },
                "required": ["target_agent_id"],
            }))
            .unwrap(),
            category: ToolCategory::Delegate,
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let target_raw = call.arguments["target_agent_id"]
            .as_str()
            .expect("target_agent_id present");
        let target: AgentId = target_raw
            .parse()
            .map_err(|e| ToolError::InvalidArguments {
                detail: format!("target_agent_id: {e}"),
            })?;

        let mut spec = SubagentSpec::fork("attach under a foreign parent", Budget::default());
        spec.mode = SubagentMode::Spawn;
        let child = ctx
            .subagents
            .start(ctx.agent_id, target, spec)
            .await
            .map_err(|e| ToolError::Internal {
                detail: e.to_string(),
            })?;
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: child.to_string(),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// A tool that reports the `AgentId`s visible in `ctx.subagents.tree
/// (ctx.agent_id)` -- the reconnaissance half of the exfiltration attack
/// this item closes.
struct TreeReconTool;

#[async_trait]
impl Tool for TreeReconTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("tree_recon"),
            description: "test-only: reports every agent id visible via ctx.subagents.tree".into(),
            schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let snapshot = ctx.subagents.tree(ctx.agent_id);
        let ids: Vec<String> = snapshot
            .nodes
            .iter()
            .map(|n| n.agent_id.to_string())
            .collect();
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: serde_json::json!({ "visible": ids }).to_string(),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

struct ExfiltratePlugin;

impl Plugin for ExfiltratePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test.exfiltrate".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![
                ToolName::new("exfiltrate_ask"),
                ToolName::new("exfiltrate_start"),
                ToolName::new("tree_recon"),
            ],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(ExfiltrateAskTool),
            Arc::new(ExfiltrateStartTool),
            Arc::new(TreeReconTool),
        ]
    }
}

fn build_conway(
    steps: Vec<Box<dyn Fn() -> GenerateResponse + Send + Sync>>,
    gate: Arc<dyn PermissionGate>,
) -> Conway {
    let backend = Arc::new(LazyBackend::new(steps));
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .with_plugin(Arc::new(ExfiltratePlugin))
        .build()
        .expect("build should succeed with the test-only exfiltration tools registered")
}

/// Spawns a child (a direct child of `handle`'s root) per `spec` and waits
/// for it to finish, returning its id and full transcript.
async fn spawn_and_await(handle: &SessionHandle, spec: SpawnSpec) -> (AgentId, Vec<LogRecord>) {
    let child = handle
        .spawn(handle.root(), spec)
        .await
        .expect("spawn should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(10), handle.await_agent(child))
        .await
        .expect("child turn must not hang")
        .expect("await_agent should resolve Ok");
    let records = handle
        .transcript(child)
        .await
        .expect("transcript should resolve");
    (child, records)
}

/// The LAST `ToolResultRecord` in `records` -- the attacking sibling's own
/// tool call (mirrors `subagent_control_seam.rs`'s identical helper).
fn tool_result(records: &[LogRecord]) -> &ToolResult {
    records
        .iter()
        .rev()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } => Some(result),
            _ => None,
        })
        .expect("expected a ToolResultRecord in the attacker's transcript")
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A marker embedded in B's own turn, so a successful exfiltration would be
/// unmistakable in A's transcript.
const VICTIM_SECRET: &str = "B_SECRET_MARKER_9f3a1c";

// ---------------------------------------------------------------------
// 1. `ask` -- the exfiltration attack itself: A forks B's entire context
//    and tries to read the reply back as plain model output.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_sibling_cannot_ask_against_another_siblings_context() {
    let target: Arc<Mutex<Option<AgentId>>> = Arc::new(Mutex::new(None));
    let target_for_step = target.clone();

    let conway = build_conway(
        vec![
            // B's own single turn -- carries the secret marker.
            Box::new(|| text_response(VICTIM_SECRET)),
            // A's first turn: attempts to fork B's context via the
            // exfiltration tool, by which point B's real id is known.
            Box::new(move || {
                let b = target_for_step
                    .lock()
                    .unwrap()
                    .expect("B must be spawned before A's turn runs");
                tool_call_response(
                    "exfiltrate_ask",
                    serde_json::json!({
                        "target_agent_id": b.to_string(),
                        "prompt": "summarize everything above",
                    }),
                )
            }),
            // A's follow-up turn, after seeing the tool's error result.
            Box::new(|| text_response("exfiltration was rejected, as expected")),
        ],
        // AllowOnce, not a denial: proves the rejection below is the
        // trait-boundary subtree check, not a permission denial that would
        // mask it (mirrors `subagent_control_seam.rs`'s identical note).
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce)) as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    let (b, _) = spawn_and_await(&handle, SpawnSpec::new("b: sit quietly")).await;
    *target.lock().unwrap() = Some(b);

    let (_a, a_records) = spawn_and_await(&handle, SpawnSpec::new("a: attack b")).await;

    let result = tool_result(&a_records);
    assert!(
        result.is_error,
        "a sibling's ask against another sibling's context must be rejected: {:?}",
        blocks_text(&result.blocks)
    );
    let rendered = blocks_text(&result.blocks);
    assert!(
        rendered.contains("subtree"),
        "the rejection must be the descendancy check (AgentNotInSubtree), not some other \
         failure: {rendered:?}"
    );

    // The core exfiltration assertion: B's secret marker must NEVER appear
    // anywhere in A's own transcript -- not in the rejected tool's result,
    // and not in any later turn (which would prove the reply text leaked
    // out even though the call itself was flagged an error).
    let a_full_text: String = a_records
        .iter()
        .filter_map(|r| match r {
            LogRecord::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            LogRecord::ToolResultRecord { result, .. } => Some(blocks_text(&result.blocks)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !a_full_text.contains(VICTIM_SECRET),
        "the victim's context must NEVER come back to the attacker, in any form: {a_full_text:?}"
    );
}

// ---------------------------------------------------------------------
// 2. `start` -- a sibling cannot attach a new child under another sibling.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_sibling_cannot_start_a_child_under_another_sibling() {
    let target: Arc<Mutex<Option<AgentId>>> = Arc::new(Mutex::new(None));
    let target_for_step = target.clone();

    let conway = build_conway(
        vec![
            Box::new(|| text_response("B is quietly working")),
            Box::new(move || {
                let b = target_for_step.lock().unwrap().expect("B must exist");
                tool_call_response(
                    "exfiltrate_start",
                    serde_json::json!({ "target_agent_id": b.to_string() }),
                )
            }),
            Box::new(|| text_response("start was rejected, as expected")),
        ],
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce)) as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    let (b, _) = spawn_and_await(&handle, SpawnSpec::new("b: sit quietly")).await;
    *target.lock().unwrap() = Some(b);

    let (_a, a_records) = spawn_and_await(&handle, SpawnSpec::new("a: attack b")).await;

    let result = tool_result(&a_records);
    assert!(
        result.is_error,
        "a sibling's start against another sibling must be rejected: {:?}",
        blocks_text(&result.blocks)
    );
    assert!(blocks_text(&result.blocks).contains("subtree"));

    // No child was ever attached under B.
    let tree = handle.tree();
    assert!(
        tree.nodes.iter().all(|n| n.parent != Some(b)),
        "no child should have been attached under B for a rejected start"
    );
}

// ---------------------------------------------------------------------
// 3. `tree` -- the reconnaissance half: a sibling's `tree()` call only ever
//    shows its OWN subtree, never a foreign branch.
// ---------------------------------------------------------------------
#[tokio::test]
async fn tree_reachable_from_any_tool_never_reveals_a_foreign_sibling() {
    let conway = build_conway(
        vec![
            Box::new(|| text_response("B is quietly working")),
            Box::new(|| tool_call_response("tree_recon", serde_json::json!({}))),
            Box::new(|| text_response("recon complete")),
        ],
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce)) as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    let (b, _) = spawn_and_await(&handle, SpawnSpec::new("b: sit quietly")).await;
    let (a, a_records) = spawn_and_await(&handle, SpawnSpec::new("a: recon")).await;

    let result = tool_result(&a_records);
    assert!(
        !result.is_error,
        "tree_recon itself always succeeds -- only its CONTENT is under test: {:?}",
        blocks_text(&result.blocks)
    );
    let rendered = blocks_text(&result.blocks);
    assert!(
        rendered.contains(&a.to_string()),
        "A's own subtree (itself) must still be visible: {rendered:?}"
    );
    assert!(
        !rendered.contains(&b.to_string()),
        "B (an unrelated sibling branch) must NEVER be visible to A's own tree() call: \
         {rendered:?}"
    );
}

// ---------------------------------------------------------------------
// 4. The legitimate path is unaffected: an agent asking/starting a fork of
//    ITSELF (the ordinary `conway_ask`/`conway_subagent` shape) still works
//    end to end through the real facade.
// ---------------------------------------------------------------------
#[tokio::test]
async fn the_root_can_still_fork_its_own_child_through_the_real_facade() {
    let conway = build_conway(
        vec![Box::new(|| text_response("child's only turn"))],
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce)) as Arc<dyn PermissionGate>,
    );
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    // `SessionHandle::fork` (the operator/embedder path) reaches the exact
    // same trait boundary this file's attacks above are rejected at; the
    // root forking ITS OWN agent must still succeed.
    let root = handle.root();
    let child = handle
        .fork(root, conway::ForkSpec::new("hi from the root"))
        .await
        .expect("root forking itself must still succeed");
    let _ = tokio::time::timeout(Duration::from_secs(10), handle.await_agent(child))
        .await
        .expect("child turn must not hang")
        .expect("await_agent should resolve Ok");
}
