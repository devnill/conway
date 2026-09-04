//! End-to-end acceptance tests for `conway_runtime::runway` (the fix for a
//! real dogfooding incident: a long-running `keep_alive` session silently
//! hit its step budget mid-real-work, with zero warning, and the operator
//! lost the ability to continue a real conversation with no idea why).
//!
//! Mirrors `runtime_api.rs`'s own harness shape (`Runtime` built entirely
//! from `conway-testkit` fakes plus a local `Capabilities` fixture) rather
//! than `agent_loop_e2e.rs`'s raw `AgentLoop`/`LoopDeps` construction --
//! `keep_alive` needs `RootSpec::keep_alive`, a `Runtime`-level concern this
//! file's own tests genuinely exercise (`agent_loop_e2e.rs`'s own harness
//! hardcodes `keep_alive: false`).
//!
//! **Shown to fail first (P-15):** before `conway_runtime::runway` existed,
//! `AgentLoop::run_inner` never wrote a `LogRecord::SystemNote { reason:
//! "runway", .. }` at all -- every assertion below (`notes.len() == 1`,
//! `"4 of 5"`, "no window note when the window is unknown") fails against
//! the unmodified loop: `runway_notes` always returns an empty `Vec`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{AgentKnobs, Budget, PermissionDecision};
use conway_core::capabilities::{
    CacheMode, Capabilities, HeadroomPolicy, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec, Usage,
};
use conway_core::error::ToolError;
use conway_core::event::Event;
use conway_core::ids::{
    AgentId, BackendId, ModelId, ModelRef, RoleAlias, SeqRange, SessionId, ToolName,
};
use conway_core::log::LogRecord;
use conway_core::ports::{
    Backend, Plugin, PluginManifest, Router, SessionStore, Tool, ToolCtx, ToolOutput,
};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use futures::StreamExt;

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn caps(max_context_tokens: u32) -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::NonStreamingOnly,
        cache: CacheMode::None,
        parallel_tool_calls: false,
        structured_output: StructuredOutput::None,
        max_context_tokens,
        reasoning: false,
        reliability_tier: ReliabilityTier::Unknown,
    }
}

// ---------------------------------------------------------------------
// A tool that returns a large, fixed-size text blob -- the "growing tool
// result" the incident this item fixes is about.
// ---------------------------------------------------------------------

const BLOB_CHARS: usize = 10_000;

fn schema_any_object() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
}

struct BigTool;

#[async_trait]
impl Tool for BigTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("big"),
            description: "test-only tool returning a large blob".into(),
            schema: schema_any_object(),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "x".repeat(BLOB_CHARS),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// A trivial, cheap-to-render tool -- gives a turn a tool-call step without
/// contributing meaningfully to context size, for the budget-dimension
/// tests below (which need many steps, not a large window).
struct ProbeTool;

#[async_trait]
impl Tool for ProbeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("probe"),
            description: "test-only trivial probe tool".into(),
            schema: schema_any_object(),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "ok".to_string(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

struct FixtureToolsPlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl Plugin for FixtureToolsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test.runway-fixtures".to_string(),
            version: "0.0.0".to_string(),
            tools: self.tools.iter().map(|t| t.spec().name).collect(),
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

fn tool_call(call_id: &str, tool: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: serde_json::json!({}),
        }],
        stop: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------

/// Builds a `Runtime` whose `RuntimeDeps` is constructed entirely from
/// `conway-testkit` fakes plus a `FakeRouter`/the given backend/plugins/
/// headroom policy -- mirrors `runtime_api.rs`'s own `build_runtime`,
/// parameterized on `headroom` (this file needs a small/zero headroom so a
/// small `max_context_tokens` fixture is actually reachable).
fn build_runtime(
    backend: Arc<ScriptedBackend>,
    plugins: Vec<Arc<dyn Plugin>>,
    headroom: HeadroomPolicy,
) -> (Arc<Runtime>, Arc<dyn SessionStore>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn conway_core::ports::Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let runtime = Runtime::new(RuntimeDeps {
        store: store.clone(),
        path_store: Arc::new(conway_testkit::FakePathStore::new()),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins,
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        instructions: Vec::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(headroom),
        session_discovery: Arc::new(conway_testkit::FakeSessionDiscoveryHost::new()),
        capabilities: Arc::new(conway_core::ports::CapabilityRegistry::default()),
    });
    (runtime, store)
}

fn root_spec(prompt: &str, budget: Budget, keep_alive: bool) -> RootSpec {
    RootSpec {
        session: None,
        knobs: AgentKnobs {
            agent_def: None,
            role: Some(RoleAlias::new("planner")),
            model: None,
            tools: None,
            budget: budget,
            result_contract: None,
            keep_alive: keep_alive,
        },
        cwd: PathBuf::from("/tmp"),
        root: None,
        prompt: Some(prompt.to_string()),
        system_prompt_override: None,
        labels: Vec::new(),
    }
}

fn session_of(runtime: &Runtime, agent: AgentId) -> SessionId {
    runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == agent)
        .expect("agent present in tree")
        .session
}

/// Waits for `Event::AgentFinished` -- every scenario below drives the
/// agent to a real terminal state (`Completed`, `Failed` on an exhausted
/// script or an admission refusal, or `BudgetExceeded`), so this is the one
/// wait every test needs.
async fn wait_for_agent_finished(
    stream: &mut conway_runtime::events::EventStream,
    agent: AgentId,
) -> conway_core::agent::AgentResult {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent {
                if let Event::AgentFinished { result, .. } = envelope.event {
                    return result;
                }
            }
        }
    })
    .await
    .expect("agent never finished")
}

/// Waits for either `Event::AgentFinished` or `Event::TurnAborted` --
/// board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`: a `keep_alive` root's
/// `max_steps`/`max_tool_calls` trip no longer finishes the agent at all,
/// it aborts just the current turn (see `agent_loop.rs`'s `check_budget`).
/// The `max_steps` scenarios below only need the run to reach a stable
/// turn boundary -- either shape counts -- so this is the wait THEY use
/// instead of `wait_for_agent_finished`, which would otherwise hang
/// forever waiting for an `AgentFinished` that a turn-scoped trip no
/// longer produces.
async fn wait_for_agent_finished_or_turn_aborted(
    stream: &mut conway_runtime::events::EventStream,
    agent: AgentId,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent
                && matches!(
                    envelope.event,
                    Event::AgentFinished { .. } | Event::TurnAborted { .. }
                )
            {
                return;
            }
        }
    })
    .await
    .expect("agent never reached a stable turn boundary")
}

async fn runway_notes(store: &dyn SessionStore, session: SessionId) -> Vec<String> {
    store
        .read(&session, SeqRange::full())
        .await
        .expect("read session records")
        .into_iter()
        .filter_map(|r| match r {
            LogRecord::SystemNote { text, reason, .. } if reason == "runway" => Some(text),
            _ => None,
        })
        .collect()
}

/// Parses the `N` out of `"runway: context window N% full (..."` --
/// `str::parse` over the digits between "window " and "% full".
fn window_pct(note: &str) -> u32 {
    let after = note
        .split("context window ")
        .nth(1)
        .expect("window note names a percentage");
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().expect("percentage is a plain integer")
}

// ---------------------------------------------------------------------
// Acceptance 1: window-fill note, exactly once, threshold not repeated.
// ---------------------------------------------------------------------

#[tokio::test]
async fn window_fill_note_fires_once_and_a_second_identical_result_does_not_repeat_it() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call("call-1", "big")),
            ScriptedTurn::Respond(tool_call("call-2", "big")),
        ])
        .with_capabilities(caps(4_000)),
    );
    let headroom = HeadroomPolicy {
        default_headroom_tokens: 0,
        per_role: Default::default(),
    };
    let (runtime, store) = build_runtime(
        backend,
        vec![Arc::new(FixtureToolsPlugin {
            tools: vec![Arc::new(BigTool)],
        })],
        headroom,
    );
    let mut stream = runtime.subscribe();

    let agent_id = runtime
        .start_root(root_spec("go", Budget::default(), false))
        .await
        .unwrap();
    let session = session_of(&runtime, agent_id);

    // The run ends in `Failed` (the second 10k-char blob, on top of the
    // first, overflows the 4_000-token window -- `ContextTooLarge`) rather
    // than `Completed` -- irrelevant to this test, which only asserts on
    // the durable `SystemNote`s a real, budget-tripping-free run left
    // behind before that point.
    let _ = wait_for_agent_finished(&mut stream, agent_id).await;

    let notes: Vec<String> = runway_notes(store.as_ref(), session)
        .await
        .into_iter()
        .filter(|t| t.contains("context window"))
        .collect();
    assert_eq!(
        notes.len(),
        1,
        "exactly one window-fill note, never repeated for the second identical-size result: {notes:?}"
    );
    let pct = window_pct(&notes[0]);
    assert!(
        pct >= 50,
        "note must report the window at 50% or higher: {} ({pct}%)",
        notes[0]
    );
}

// ---------------------------------------------------------------------
// Acceptance 2: max_steps warning at 80% of the limit, keep_alive root.
// ---------------------------------------------------------------------

async fn run_max_steps_scenario(max_steps: u32) -> Vec<String> {
    let backend = Arc::new(ScriptedBackend::new(vec![
        ScriptedTurn::Respond(tool_call("s1", "probe")),
        ScriptedTurn::Respond(tool_call("s2", "probe")),
        ScriptedTurn::Respond(tool_call("s3", "probe")),
        ScriptedTurn::Respond(tool_call("s4", "probe")),
        ScriptedTurn::Respond(tool_call("s5", "probe")),
    ]));
    let (runtime, store) = build_runtime(
        backend,
        vec![Arc::new(FixtureToolsPlugin {
            tools: vec![Arc::new(ProbeTool)],
        })],
        HeadroomPolicy::default(),
    );
    let mut stream = runtime.subscribe();

    let budget = Budget {
        max_steps,
        ..Budget::default()
    };
    let agent_id = runtime
        .start_root(root_spec("go", budget, true))
        .await
        .unwrap();
    let session = session_of(&runtime, agent_id);

    // `keep_alive: true` -- board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`: a
    // `max_steps` trip now aborts just the turn (`Event::TurnAborted`),
    // never finishing the agent, so this waits for either shape rather
    // than the `AgentFinished` this scenario can no longer produce.
    wait_for_agent_finished_or_turn_aborted(&mut stream, agent_id).await;

    runway_notes(store.as_ref(), session).await
}

#[tokio::test]
async fn max_steps_warning_fires_after_step_four_of_five() {
    let notes: Vec<String> = run_max_steps_scenario(5)
        .await
        .into_iter()
        .filter(|t| t.contains("max_steps"))
        .collect();
    assert_eq!(
        notes.len(),
        1,
        "exactly one max_steps runway note: {notes:?}"
    );
    assert!(notes[0].contains("4 of 5"), "{}", notes[0]);
    assert!(notes[0].contains("max_steps"), "{}", notes[0]);
}

#[tokio::test]
async fn max_steps_zero_never_warns() {
    let notes: Vec<String> = run_max_steps_scenario(0)
        .await
        .into_iter()
        .filter(|t| t.contains("max_steps"))
        .collect();
    assert!(
        notes.is_empty(),
        "max_steps=0 (unlimited) must never produce a runway note: {notes:?}"
    );
}

// ---------------------------------------------------------------------
// Acceptance 3: an Unverified (unknown) window never gets a window note;
// budget notes still fire.
// ---------------------------------------------------------------------

#[tokio::test]
async fn unverified_window_writes_no_window_note_but_budget_notes_still_fire() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call("b1", "big")),
            ScriptedTurn::Respond(tool_call("b2", "big")),
            ScriptedTurn::Respond(tool_call("b3", "big")),
            ScriptedTurn::Respond(tool_call("b4", "big")),
        ])
        // `u32::MAX`: `crate::runway`'s honestly-scoped stand-in for "this
        // model's real window is not known" -- see `AttemptOutcome::
        // max_context_tokens`'s own doc.
        .with_capabilities(caps(u32::MAX)),
    );
    let headroom = HeadroomPolicy {
        default_headroom_tokens: 0,
        per_role: Default::default(),
    };
    let (runtime, store) = build_runtime(
        backend,
        vec![Arc::new(FixtureToolsPlugin {
            tools: vec![Arc::new(BigTool)],
        })],
        headroom,
    );
    let mut stream = runtime.subscribe();

    let budget = Budget {
        max_steps: 4,
        ..Budget::default()
    };
    let agent_id = runtime
        .start_root(root_spec("go", budget, true))
        .await
        .unwrap();
    let session = session_of(&runtime, agent_id);

    // `keep_alive: true`, same reasoning as `run_max_steps_scenario` above:
    // the `max_steps=4` trip now aborts the turn instead of finishing the
    // agent.
    wait_for_agent_finished_or_turn_aborted(&mut stream, agent_id).await;

    let notes = runway_notes(store.as_ref(), session).await;
    assert!(
        notes.iter().all(|t| !t.contains("context window")),
        "an unknown window must never produce a window-fill note, even though real context \
         growth (four 10k-char tool results) would have crossed every threshold if the window \
         were known: {notes:?}"
    );
    assert!(
        notes.iter().any(|t| t.contains("max_steps")),
        "a budget note must still fire even though the window is unknown: {notes:?}"
    );
}
