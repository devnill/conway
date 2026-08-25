//! Regression/acceptance tests for
//! ("`start`/`ask`/`tree` are unguarded -- cross-tree exfiltration in one
//! call") -- the SECURITY-CRITICAL slice `674bb65`
//! (see `subagent_control_seam.rs`) left open.
//!
//! `674bb65` fenced `steer`/`await_result`/`cancel` at the trait boundary,
//! but `start`/`ask` still took only `parent` and acted on it directly, and
//! `tree` took no caller at all and returned the WHOLE runtime-wide tree to
//! any tool. Composed, that was cross-tree exfiltration in one call:
//! `tree()` to discover a sibling's `AgentId` (an ordinary, unprivileged
//! read every tool already has via `ToolCtx::subagents`), then
//! `ask(sibling, SubagentSpec { mode: Fork, .. })` to fork that sibling's
//! ENTIRE context (: a fork inherits everything up to the fork point)
//! and read the reply back as plain model output.
//!
//! Mirrors `subagent_control_seam.rs` exactly, for the identical reason
//! stated there: a hand-written `FakeSubagentHost` fixture proves nothing
//! about whether the real trait boundary (`impl SubagentHost for Runtime`)
//! enforces descendancy. Every test below drives a REAL tool
//! (`conway-core`'s `Plugin`/`Tool` ports, dispatched through a real agent
//! turn via the real `ToolRunner`) against two SIBLING agents produced by
//! the real `SubagentHost::start`, calling `ctx.subagents` -- the exact
//! capability every built-in AND third-party tool holds -- directly.
//!
//! ** C1 superseded most of this file's original premise, in the
//! strongest possible direction.** This file predates `SubagentHandle`:
//! `ctx.subagents` used to be a raw `Arc<dyn SubagentHost>`, so a
//! third-party tool COULD, syntactically, pass a model-chosen `target`/
//! `parent` distinct from its own `caller` -- the whole point of the
//! `Exfiltrate*`/`TreeRecon` tools below was to prove that the `SubagentHost`
//! trait boundary rejects such a call at RUNTIME even though nothing at the
//! `ToolCtx` layer stopped a tool from ATTEMPTING it. Since C1,
//! `ctx.subagents` is a `SubagentHandle` with the calling agent's own id
//! baked in: `SubagentHandle::start`/`steer`/`await_result`/`cancel`/`ask`/
//! `tree` have NO `caller`/`parent`/`target`-as-a-different-agent parameter
//! for any tool -- hostile or not -- to supply. The exact three attack
//! shapes this file probes (`ask` a sibling's context, `start` a child
//! under a sibling, `tree()` a foreign branch) no longer TYPE-CHECK against
//! `ctx.subagents` at all; see `conway_core::ports::subagent`'s own test
//! module (`start_and_ask_pass_the_handles_own_agent_id_as_both_caller_and_parent`,
//! `steer_await_cancel_and_tree_always_pass_the_handles_own_agent_id_as_caller`)
//! for that fact proven directly, at the type's own definition, without a
//! facade/runtime round trip.
//!
//! This file is kept, rewritten rather than deleted, because the RUNTIME
//! trait boundary these tools originally exercised is UNCHANGED (C1
//! deliberately does not touch `SubagentHost`/`RuntimeError` -- the D2 tier
//! line) and still deserves live coverage through a real `Tool`/`ToolCtx`
//! round trip, not only a unit test against the handle in isolation. Each
//! tool below keeps its ORIGINAL "hostile shape" -- a `target_agent_id`
//! argument a model could supply -- but its `invoke` can no longer thread
//! that argument anywhere `ctx.subagents` would honor it; each test now
//! asserts the STRUCTURALLY GUARANTEED outcome (the call always acts on the
//! tool's own caller, never the named target) rather than a runtime
//! rejection, since there is no longer a rejection to observe -- the call
//! simply succeeds, harmlessly, against the caller's own subtree.
//!
//! **Why a custom tool, not `conway_ask`/`conway_fork`/`conway_spawn`:** neither
//! built-in tool's OWN JSON schema exposes a field naming a different
//! parent/target -- both always pass `ctx.agent_id` for both `caller` and
//! `parent` (see `conway-tools`' `subagent/{ask,tools}.rs`). That was already
//! true before C1; what C1 adds is that even a tool willing to accept and
//! try to use such a field (exactly the shape `Exfiltrate*` below
//! represents) now has no way to make it matter.
#![cfg(feature = "builtin-tools")]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::test_support::test_builder;
use conway::{AgentId, Plugin, SessionHandle, SessionSpec, SpawnSpec, Tool};
use conway_core::agent::{Budget, PermissionDecision, SubagentMode, SubagentSpec};
use conway_core::capabilities::{
    CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolResult, ToolSpec,
    TruncationPolicy, Usage,
};
use conway_core::error::{BackendError, ToolError};
use conway_core::ids::{BackendId, ModelId, RoleAlias, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{
    Backend, BoxStream, GenerateRequest, GenerateResponse, PermissionGate, PluginManifest,
    StreamChunk, ToolCtx, ToolOutput,
};
use conway_testkit::{text_response, FakeGate};
use futures_core::Stream;

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
            ..Default::default()
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
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
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
// `conway_fork`/`conway_spawn`, which never do (see this file's own module doc). Both
// still accept a MODEL-SUPPLIED `target_agent_id` argument (the hostile
// shape), but since C1, `SubagentHandle::ask`/`start` have no
// `caller`/`parent` parameter at all for either tool to thread it through
// -- `target_agent_id` is parsed (proving a real, well-formed foreign id
// was genuinely supplied) and then provably unused: whatever id a model
// names, the call can only ever act on `ctx.agent_id` itself.
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
        // Parsed only to prove a real, well-formed foreign agent id was
        // genuinely supplied by the "model" here -- `SubagentHandle::ask`
        // (below) has no parameter to receive it through, so it is never
        // used for anything else. This is the structural guarantee under
        // test: however hostile this tool's OWN code is, it cannot make
        // this id matter.
        let _target: AgentId = target_raw
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
            .ask(SubagentSpec::fork(prompt, Budget::default()))
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
        // See `ExfiltrateAskTool::invoke`'s identical note: parsed to prove
        // a real foreign id was supplied, then provably unused --
        // `SubagentHandle::start` has no `parent` parameter to receive it
        // through.
        let _target: AgentId = target_raw
            .parse()
            .map_err(|e| ToolError::InvalidArguments {
                detail: format!("target_agent_id: {e}"),
            })?;

        let mut spec = SubagentSpec::fork("attach under a foreign parent", Budget::default());
        spec.mode = SubagentMode::Spawn;
        let child = ctx
            .subagents
            .start(spec)
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

/// A tool that reports the `AgentId`s visible in `ctx.subagents.tree()` --
/// the reconnaissance half of the exfiltration attack this item closes.
/// Since C1, `SubagentHandle::tree` takes no `caller` argument
/// at all (it was always `ctx.agent_id` in practice here; now there is no
/// parameter through which this -- or any -- tool could supply anything
/// else).
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
        let snapshot = ctx.subagents.tree();
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
// 1. `ask` -- the exfiltration attack itself: A tries to fork B's entire
//    context and read the reply back as plain model output. Since board
//    item C1, this call can no longer even NAME B as the fork target --
//    `SubagentHandle::ask` always forks the caller (A) itself, so the call
//    SUCCEEDS, harmlessly, against A's own context, instead of being
//    rejected. See this file's own module doc for why the assertions below
//    changed from "rejected with an AgentNotInSubtree message" to
//    "succeeded, but never touched B".
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_hostile_ask_tool_can_only_ever_ask_its_own_caller_never_a_named_target() {
    let target: Arc<Mutex<Option<AgentId>>> = Arc::new(Mutex::new(None));
    let target_for_step = target.clone();
    const SELF_FORK_REPLY: &str = "A_SELF_FORK_REPLY_5c2e9b";

    let conway = test_builder(base_config())
        .with_backend(Arc::new(LazyBackend::new(vec![
            // B's own single turn -- carries the secret marker.
            Box::new(|| text_response(VICTIM_SECRET)),
            // A's first turn: calls the hostile tool naming B as
            // `target_agent_id`, by which point B's real id is known.
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
            // The ephemeral child `SubagentHandle::ask` forks -- structurally
            // always a fork of A itself, never of B, so this is the ONLY
            // turn that can possibly run here regardless of what
            // `target_agent_id` named.
            Box::new(|| text_response(SELF_FORK_REPLY)),
            // A's follow-up turn, after seeing the tool's (successful) result.
            Box::new(|| text_response("noted")),
        ])))
        .with_permission_gate(
            Arc::new(FakeGate::new(PermissionDecision::AllowOnce)) as Arc<dyn PermissionGate>
        )
        .with_plugin(Arc::new(ExfiltratePlugin))
        .build()
        .expect("build should succeed with the test-only exfiltration tools registered");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    let (b, _) = spawn_and_await(&handle, SpawnSpec::new("b: sit quietly")).await;
    *target.lock().unwrap() = Some(b);

    let (_a, a_records) = spawn_and_await(&handle, SpawnSpec::new("a: attack b")).await;

    let result = tool_result(&a_records);
    assert!(
        !result.is_error,
        "the call always succeeds -- there is no longer a foreign target to reject, since \
         SubagentHandle::ask has no parameter to name one: {:?}",
        blocks_text(&result.blocks)
    );
    assert_eq!(
        blocks_text(&result.blocks),
        SELF_FORK_REPLY,
        "the ask must have forked A itself (its own reply comes back), never B"
    );

    // The core exfiltration assertion, unchanged in spirit: B's secret
    // marker must NEVER appear anywhere in A's own transcript. This is now
    // guaranteed structurally (there is no code path left, in this tool or
    // any other reachable through `ctx.subagents`, that could produce a
    // fork of B), not merely because the attempt happened to be rejected.
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
// 2. `start` -- a sibling tries to attach a new child under another
//    sibling. Since C1, `SubagentHandle::start` has no `parent`
//    parameter at all -- the call always attaches the new child under the
//    CALLER (A) itself, so it SUCCEEDS instead of being rejected; the
//    assertion that matters is that the attached child's parent is A,
//    never B, regardless of what `target_agent_id` named.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_hostile_start_tool_can_only_ever_attach_under_its_own_caller() {
    let target: Arc<Mutex<Option<AgentId>>> = Arc::new(Mutex::new(None));
    let target_for_step = target.clone();

    let conway = test_builder(base_config())
        .with_backend(Arc::new(LazyBackend::new(vec![
            Box::new(|| text_response("B is quietly working")),
            Box::new(move || {
                let b = target_for_step.lock().unwrap().expect("B must exist");
                tool_call_response(
                    "exfiltrate_start",
                    serde_json::json!({ "target_agent_id": b.to_string() }),
                )
            }),
            // Two more turns are needed once `start` SUCCEEDS: A's own
            // follow-up turn (after seeing the tool result) AND the newly
            // spawned grandchild's single turn (it runs to completion as
            // its own background task -- see this test's own comment
            // below for why `start`'s return already guarantees correct
            // attachment regardless of which of these two runs first).
            // Identical, innocuous content on purpose: nothing here
            // depends on which agent consumes which of these two.
            Box::new(|| text_response("ok")),
            Box::new(|| text_response("ok")),
        ])))
        .with_permission_gate(
            Arc::new(FakeGate::new(PermissionDecision::AllowOnce)) as Arc<dyn PermissionGate>
        )
        .with_plugin(Arc::new(ExfiltratePlugin))
        .build()
        .expect("build should succeed with the test-only exfiltration tools registered");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");

    let (b, _) = spawn_and_await(&handle, SpawnSpec::new("b: sit quietly")).await;
    *target.lock().unwrap() = Some(b);

    let (a, a_records) = spawn_and_await(&handle, SpawnSpec::new("a: attack b")).await;

    let result = tool_result(&a_records);
    assert!(
        !result.is_error,
        "the call always succeeds -- there is no longer a foreign parent to reject, since \
         SubagentHandle::start has no parameter to name one: {:?}",
        blocks_text(&result.blocks)
    );

    // `SubagentHost::start`'s own contract attaches the child to the tree
    // SYNCHRONOUSLY, before returning the new `AgentId` (only the child's
    // OWN turn execution is backgrounded) -- so this check is race-free
    // immediately after `spawn_and_await` returns, without needing to wait
    // for the grandchild's turn to finish.
    let tree = handle.tree();
    let started_child = tree
        .nodes
        .iter()
        .find(|n| n.parent == Some(a))
        .expect("exfiltrate_start's child must be attached under A");
    assert_ne!(
        started_child.agent_id, b,
        "the attached child must be a NEW agent, not B itself"
    );
    assert!(
        tree.nodes.iter().all(|n| n.parent != Some(b)),
        "no child was ever attached under B -- target_agent_id had no effect"
    );
}

// ---------------------------------------------------------------------
// 3. `tree` -- the reconnaissance half: a sibling's `tree()` call only ever
//    shows its OWN subtree, never a foreign branch.
// ---------------------------------------------------------------------
#[tokio::test]
async fn tree_reachable_from_any_tool_never_reveals_a_foreign_sibling() {
    let conway = test_builder(base_config())
        .with_backend(Arc::new(LazyBackend::new(vec![
            Box::new(|| text_response("B is quietly working")),
            Box::new(|| tool_call_response("tree_recon", serde_json::json!({}))),
            Box::new(|| text_response("recon complete")),
        ])))
        .with_permission_gate(
            Arc::new(FakeGate::new(PermissionDecision::AllowOnce)) as Arc<dyn PermissionGate>
        )
        .with_plugin(Arc::new(ExfiltratePlugin))
        .build()
        .expect("build should succeed with the test-only exfiltration tools registered");

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
//    ITSELF (the ordinary `conway_ask`/`conway_fork`/`conway_spawn` shape) still works
//    end to end through the real facade.
// ---------------------------------------------------------------------
#[tokio::test]
async fn the_root_can_still_fork_its_own_child_through_the_real_facade() {
    let conway = test_builder(base_config())
        .with_backend(Arc::new(LazyBackend::new(vec![Box::new(|| {
            text_response("child's only turn")
        })])))
        .with_permission_gate(
            Arc::new(FakeGate::new(PermissionDecision::AllowOnce)) as Arc<dyn PermissionGate>
        )
        .with_plugin(Arc::new(ExfiltratePlugin))
        .build()
        .expect("build should succeed with the test-only exfiltration tools registered");
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
