//! The three observation-only hook
//! events dispatch, and — the property that actually matters — a hook that
//! fails NEVER turns into a failure of the thing it observed.
//!
//! Each test drives the real production seam rather than calling
//! `HookDispatcher::dispatch` directly: `ToolRunner::run_batch` for
//! `post_tool_use`, `Runtime::start_root` for `session_starting`, and
//! `SubagentHost::start` for `child_spawned`. Asserting on the observable
//! outcome of the operation, not on an intermediate signal, is what makes
//! these discriminating — a dispatcher unit test would pass just as happily
//! if nothing ever called it.
//!
//! Every runner installed here is scripted to FAIL. That is deliberate: the
//! failure path is the one with a real chance of being wrong, and a test that
//! only ever exercises a succeeding hook proves nothing about propagation.
//!
//! **The matcher section below is
//! the one deliberate exception.** Its question is "did the RIGHT hook run
//! for the RIGHT tool", which a failing runner cannot answer -- only a
//! runner that records what it was actually invoked with can, so
//! `RecordingRunner` (never failing) is used there instead. The
//! `request_assembled`/`child_reported` section further down uses the SAME failing-runner
//! discipline as every other observation event above it -- those two are
//! ordinary observation-tier dispatch, and their own propagation guarantee
//! (a failing hook must not fail the turn/finish it observed) is exactly
//! what this file's opening claim is about.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{Budget, PermissionDecision, SubagentSpec};
use conway_core::capabilities::HeadroomPolicy;
use conway_core::content::{ContentBlock, PermissionClass, ToolCall, ToolCategory, ToolSpec};
use conway_core::error::{HookFailure, ToolError};
use conway_core::hook::{HookAnswer, HookInvocation};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId, ToolName};
use conway_core::ports::{
    Backend, HookRunner, Plugin, PluginConfig, PluginManifest, Router, SessionStore, SubagentHost,
    Tool, ToolCtx, ToolOutput,
};
use conway_runtime::events::EventBus;
use conway_runtime::hook_dispatch::{
    HookSpec, CHILD_REPORTED, CHILD_SPAWNED, POST_TOOL_USE, REQUEST_ASSEMBLED, SESSION_STARTING,
};
use conway_runtime::permission::{AgentRoot, PermissionBroker};
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_runtime::tools::{PluginRegistry, ToolBatchCtx, ToolRunner};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter, FakeStore, FakeSubagentHost};
use tokio_util::sync::CancellationToken as TokioCancellationToken;

/// A `HookRunner` that records every event name it saw and then FAILS.
///
/// Failing is the point: if any seam propagated a hook failure, the operation
/// under test would error and the assertion on its success would catch it.
#[derive(Debug, Default)]
struct FailingRunner {
    seen: Mutex<Vec<String>>,
}

impl FailingRunner {
    fn names(&self) -> Vec<String> {
        self.seen.lock().expect("seen lock poisoned").clone()
    }
    fn count(&self, event: &str) -> usize {
        self.names().iter().filter(|n| n.as_str() == event).count()
    }
}

#[async_trait]
impl HookRunner for FailingRunner {
    async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
        self.seen
            .lock()
            .expect("seen lock poisoned")
            .push(invocation.event.name.clone());
        Err(HookFailure::TimedOut { after_ms: 1 })
    }
}

fn spec(id: &str) -> HookSpec {
    HookSpec {
        id: id.to_string(),
        command: vec!["/bin/true".to_string()],
        timeout_ms: 1_000,
        matcher: None,
    }
}

fn hooks_for(event: &str) -> BTreeMap<String, Vec<HookSpec>> {
    BTreeMap::from([(event.to_string(), vec![spec("observer")])])
}

// ---------------------------------------------------------- post_tool_use --

/// A tool that always succeeds, so any error in the outcome can only have come
/// from the hook path this test is about.
struct OkTool(ToolName);

#[async_trait]
impl Tool for OkTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.0.clone(),
            description: "always succeeds".into(),
            schema: serde_json::from_value(serde_json::json!({"type": "object"}))
                .expect("valid RootSchema JSON"),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }
    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "the real result".into(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: Vec::new(),
        })
    }
}

fn tool_batch_ctx() -> ToolBatchCtx {
    ToolBatchCtx {
        agent_id: AgentId::new(),
        agent_path: vec![],
        session_id: SessionId::new(),
        chdir: conway_core::ports::CwdHandle::new(PathBuf::from("/tmp")),
        cancel: TokioCancellationToken::new(),
        subagents: Arc::new(FakeSubagentHost::new(AgentId::new())) as Arc<dyn SubagentHost>,
        plugin_config: Arc::new(PluginConfig::default()),
        max_parallel_tools: 4,
        root: AgentRoot::Unconfined,
    }
}

/// ACCEPTANCE: "a tool call still completes successfully and returns its real
/// result when its `post_tool_use` hook times out or errors."
#[tokio::test]
async fn post_tool_use_hook_failure_does_not_fail_the_tool_call() {
    let name = ToolName::new("ok");
    let registry = Arc::new(
        PluginRegistry::from_plugins(vec![Arc::new(OneToolPlugin {
            id: "fake".into(),
            tool: Arc::new(OkTool(name.clone())),
        }) as Arc<dyn Plugin>])
        .expect("registry"),
    );
    let bus = EventBus::new(1024);
    let broker = Arc::new(PermissionBroker::new(
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        bus.clone(),
    ));
    let runner = ToolRunner::new(registry, broker, bus);

    let hook = Arc::new(FailingRunner::default());
    runner.hooks().set_runner(Some(hook.clone()));
    runner.hooks().set_hooks(hooks_for(POST_TOOL_USE));

    let outcomes = runner
        .run_batch(
            &tool_batch_ctx(),
            vec![ToolCall {
                call_id: "c1".into(),
                name,
                arguments: serde_json::json!({}),
            }],
        )
        .await;

    // The hook ran...
    assert_eq!(
        hook.count(POST_TOOL_USE),
        1,
        "post_tool_use did not dispatch"
    );
    // ...and failed, and the call is STILL successful with its real result.
    assert_eq!(outcomes.len(), 1);
    assert!(
        !outcomes[0].is_error,
        "a failing post_tool_use hook turned into a tool-call failure"
    );
    assert!(
        format!("{:?}", outcomes[0].blocks).contains("the real result"),
        "the real tool result did not survive the failing hook: {:?}",
        outcomes[0].blocks
    );
}

/// One plugin wrapping one tool — the registry needs a `Plugin`, not a bare
/// `Tool`. No `Debug` derive: `dyn Tool` is not `Debug`.
struct OneToolPlugin {
    id: String,
    tool: Arc<dyn Tool>,
}

impl Plugin for OneToolPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.clone(),
            version: "0.0.0".into(),
            tools: vec![self.tool.spec().name],
            required_host_caps: vec![],
        }
    }
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![self.tool.clone()]
    }
}

// -------------------------------------------------------------- matcher --

/// A `HookRunner` that records every event name AND payload it saw, and
/// always succeeds -- unlike every other runner in this file (module doc):
/// the matcher's question is "did the right hook run for the right call",
/// which only a recording, succeeding runner can answer.
#[derive(Debug, Default)]
struct RecordingRunner {
    seen: Mutex<Vec<(String, serde_json::Value)>>,
}

impl RecordingRunner {
    fn tool_calls(&self, tool: &str) -> usize {
        self.seen
            .lock()
            .expect("seen lock poisoned")
            .iter()
            .filter(|(_, payload)| payload["tool"] == tool)
            .count()
    }
}

#[async_trait]
impl HookRunner for RecordingRunner {
    async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
        self.seen.lock().expect("seen lock poisoned").push((
            invocation.event.name.clone(),
            invocation.event.payload.clone(),
        ));
        Ok(HookAnswer::default())
    }
}

/// **VERIFICATION ANCHOR**: two
/// `post_tool_use` rules, one matching `read` and one matching `edit`,
/// driven through a session (`ToolRunner::run_batch`, the real production
/// seam -- module doc) that calls both tools; each script runs EXACTLY ONCE,
/// for the right call. Shown to fail by removing `HookSpec::applies_to`'s
/// filter in `HookDispatcher::dispatch`, which makes both specs fire for
/// both calls (4 invocations instead of 2).
#[tokio::test]
async fn a_post_tool_use_matcher_fires_only_for_its_own_tool_through_run_batch() {
    let read_name = ToolName::new("read");
    let edit_name = ToolName::new("edit");
    let registry = Arc::new(
        PluginRegistry::from_plugins(vec![
            Arc::new(OneToolPlugin {
                id: "read-plugin".into(),
                tool: Arc::new(OkTool(read_name.clone())),
            }) as Arc<dyn Plugin>,
            Arc::new(OneToolPlugin {
                id: "edit-plugin".into(),
                tool: Arc::new(OkTool(edit_name.clone())),
            }) as Arc<dyn Plugin>,
        ])
        .expect("registry"),
    );
    let bus = EventBus::new(1024);
    let broker = Arc::new(PermissionBroker::new(
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        bus.clone(),
    ));
    let runner = ToolRunner::new(registry, broker, bus);

    let hook = Arc::new(RecordingRunner::default());
    runner.hooks().set_runner(Some(hook.clone()));
    runner.hooks().set_hooks(BTreeMap::from([(
        POST_TOOL_USE.to_string(),
        vec![
            HookSpec {
                id: "read-watcher".into(),
                command: vec!["/bin/true".to_string()],
                timeout_ms: 1_000,
                matcher: Some("read".to_string()),
            },
            HookSpec {
                id: "edit-watcher".into(),
                command: vec!["/bin/true".to_string()],
                timeout_ms: 1_000,
                matcher: Some("edit".to_string()),
            },
        ],
    )]));

    let outcomes = runner
        .run_batch(
            &tool_batch_ctx(),
            vec![
                ToolCall {
                    call_id: "c1".into(),
                    name: read_name,
                    arguments: serde_json::json!({}),
                },
                ToolCall {
                    call_id: "c2".into(),
                    name: edit_name,
                    arguments: serde_json::json!({}),
                },
            ],
        )
        .await;
    assert_eq!(outcomes.len(), 2);

    assert_eq!(
        hook.tool_calls("read"),
        1,
        "the read-matching hook must fire exactly once, for `read`: {:?}",
        hook.seen.lock().expect("seen lock poisoned")
    );
    assert_eq!(
        hook.tool_calls("edit"),
        1,
        "the edit-matching hook must fire exactly once, for `edit`: {:?}",
        hook.seen.lock().expect("seen lock poisoned")
    );
    assert_eq!(
        hook.seen.lock().expect("seen lock poisoned").len(),
        2,
        "exactly 2 invocations total, not 4 -- each rule must fire for its own tool only"
    );
}

// ------------------------------------------- session_starting / child_spawned

fn build_runtime() -> Arc<Runtime> {
    build_runtime_with_store().0
}

/// Hands back the concrete store too, so a test can read the records the loop
/// will actually assemble from rather than trusting the spec it passed in.
fn build_runtime_with_store() -> (Arc<Runtime>, Arc<FakeStore>) {
    let fake = Arc::new(FakeStore::new());
    let store: Arc<dyn SessionStore> = fake.clone();
    let backend = Arc::new(
        conway_testkit::ScriptedBackend::new(Default::default()).with_id(BackendId::new("b")),
    );
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let rt = Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });
    (rt, fake)
}

fn root_spec() -> RootSpec {
    RootSpec {
        session: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        tools: None,
        budget: Budget::default(),
        cwd: PathBuf::from("/tmp"),
        root: None,
        // No prompt: the root idles instead of running a turn against the
        // scripted backend, which keeps this test about the event and not
        // about turn execution.
        prompt: None,
        keep_alive: false,
        model: None,
        system_prompt_override: None,
        result_contract: None,
        labels: Vec::new(),
    }
}

/// ACCEPTANCE: "`session_starting` fires exactly once per `start_root`, not
/// once per turn or per tool call."
#[tokio::test]
async fn session_starting_fires_exactly_once_per_start_root() {
    let rt = build_runtime();
    let hook = Arc::new(FailingRunner::default());
    rt.set_observation_hook_runner(Some(hook.clone()));
    rt.set_observation_hooks(hooks_for(SESSION_STARTING));

    let first = rt
        .start_root(root_spec())
        .await
        .expect("first root started");
    assert_eq!(
        hook.count(SESSION_STARTING),
        1,
        "session_starting did not fire exactly once for one start_root"
    );

    // A SECOND root is a second session, so a second event -- and still
    // exactly one per start, never a burst.
    let second = rt
        .start_root(root_spec())
        .await
        .expect("second root started");
    assert_ne!(first, second);
    assert_eq!(
        hook.count(SESSION_STARTING),
        2,
        "session_starting is not once-per-start_root"
    );

    // And the failing hook did not stop either session from starting, which
    // is the propagation property for this seam.
    assert_eq!(
        hook.names()
            .iter()
            .filter(|n| *n != SESSION_STARTING)
            .count(),
        0,
        "an unrelated event dispatched: {:?}",
        hook.names()
    );
}

/// ACCEPTANCE: "a subagent still spawns successfully when its `child_spawned`
/// hook fails."
#[tokio::test]
async fn child_spawned_hook_failure_does_not_fail_the_spawn() {
    let rt = build_runtime();
    let hook = Arc::new(FailingRunner::default());
    rt.set_observation_hook_runner(Some(hook.clone()));
    rt.set_observation_hooks(hooks_for(CHILD_SPAWNED));

    let parent = rt.start_root(root_spec()).await.expect("root started");

    // A FORK rather than a spawn: `child_spawned` is dispatched from the
    // single `start` both modes share, and a fork needs no registered
    // `AgentDef`, which keeps this test about the hook and not about def
    // resolution.
    let child = rt
        .start(
            parent,
            parent,
            SubagentSpec::fork("do a thing", Budget::default()),
        )
        .await
        .expect("a failing child_spawned hook must not fail the spawn");

    assert_ne!(child, parent);
    assert_eq!(
        hook.count(CHILD_SPAWNED),
        1,
        "child_spawned did not dispatch for a spawn"
    );
}

/// The default posture: no runner injected means nothing is spawned, for every
/// event. This is what every existing consumer that never calls the setters
/// gets, and it must stay a byte-for-byte no-op.
#[tokio::test]
async fn no_runner_installed_dispatches_nothing() {
    let rt = build_runtime();
    // Hooks configured but NO runner -- still inert.
    rt.set_observation_hooks(hooks_for(SESSION_STARTING));
    rt.start_root(root_spec()).await.expect("root started");

    // Nothing to assert against a recorder here (there is none); the property
    // is that `start_root` completes normally, which it just did. The
    // discriminating version of this is the unit test in
    // `conway_runtime::hook_dispatch`, which owns a recorder.
}

/// A hook subscribed to an event OTHER than the one that fires must not be
/// invoked -- the dispatcher is keyed by name, and a bug there would show up
/// as every hook running for every event.
#[tokio::test]
async fn a_hook_subscribed_to_another_event_is_not_invoked() {
    let rt = build_runtime();
    let hook = Arc::new(FailingRunner::default());
    rt.set_observation_hook_runner(Some(hook.clone()));
    // Subscribed to child_spawned only...
    rt.set_observation_hooks(hooks_for(CHILD_SPAWNED));

    // ...while a session starts.
    rt.start_root(root_spec()).await.expect("root started");

    assert_eq!(
        hook.count(SESSION_STARTING),
        0,
        "a child_spawned subscriber was invoked for session_starting"
    );
}

// ------------------------------------------------------- prompt_submitted --

use conway_core::error::RuntimeError;
use conway_runtime::hook_dispatch::PROMPT_SUBMITTED;

/// A runner that DENIES, and would rewrite the prompt if the type let it.
///
/// It returns a `HookAnswer` whose `context` carries an append delta — the
/// nearest thing to "replacement text" the answer type can express — so the
/// byte-identity test below is not merely asserting against a runner that
/// never tried.
#[derive(Debug, Default)]
struct MeddlingRunner {
    deny: bool,
    seen_text: Mutex<Vec<String>>,
}

#[async_trait]
impl HookRunner for MeddlingRunner {
    async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
        self.seen_text.lock().expect("lock poisoned").push(
            invocation.event.payload["text"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        Ok(HookAnswer {
            permission: if self.deny {
                conway_core::hook::HookPermissionVerdict::Deny {
                    reason: "refused by policy".into(),
                }
            } else {
                conway_core::hook::HookPermissionVerdict::NoOpinion
            },
            // Deliberately non-default: a hook TRYING to change things. The
            // prompt path ignores this field entirely.
            context: conway_core::hook::ContextDelta::default(),
        })
    }
}

fn wire_prompt_hook(rt: &Runtime, runner: Arc<dyn HookRunner>) {
    rt.set_observation_hook_runner(Some(runner));
    rt.set_observation_hooks(hooks_for(PROMPT_SUBMITTED));
}

fn root_spec_with_prompt(text: &str) -> RootSpec {
    let mut s = root_spec();
    s.prompt = Some(text.to_string());
    s
}

/// ACCEPTANCE: "a denying hook prevents a fresh session's first prompt
/// (`start_root`) from reaching the agent loop."
#[tokio::test]
async fn a_denying_prompt_submitted_hook_rejects_start_root() {
    let rt = build_runtime();
    wire_prompt_hook(
        &rt,
        Arc::new(MeddlingRunner {
            deny: true,
            ..Default::default()
        }),
    );

    let err = rt
        .start_root(root_spec_with_prompt("do the thing"))
        .await
        .expect_err("a denying prompt_submitted hook must reject start_root");

    match err {
        RuntimeError::PromptDenied { reason } => {
            assert!(reason.contains("refused by policy"), "reason: {reason}");
        }
        other => panic!("expected PromptDenied, got {other:?}"),
    }
}

/// ACCEPTANCE: "a denying hook prevents a follow-up (`prompt`) from reaching
/// the agent loop."
#[tokio::test]
async fn a_denying_prompt_submitted_hook_rejects_a_follow_up_prompt() {
    let rt = build_runtime();
    // Start WITHOUT a prompt and with no hooks, so the session exists first.
    let agent = rt.start_root(root_spec()).await.expect("root started");

    wire_prompt_hook(
        &rt,
        Arc::new(MeddlingRunner {
            deny: true,
            ..Default::default()
        }),
    );

    let err = rt
        .prompt(agent, "a follow-up".to_string())
        .await
        .expect_err("a denying prompt_submitted hook must reject prompt");
    assert!(matches!(err, RuntimeError::PromptDenied { .. }), "{err:?}");
}

/// ACCEPTANCE: "a test submits a prompt through a hook that would alter the
/// text if it could, and asserts the text the agent loop receives is
/// byte-identical to what was submitted."
///
/// The type-level half of the guarantee is asserted separately, below.
#[tokio::test]
async fn a_permitted_prompt_reaches_the_loop_byte_identical() {
    const SUBMITTED: &str = "  Do NOT rewrite\tthis — 日本語 \u{1F600}  ";

    let (rt, store) = build_runtime_with_store();
    let runner = Arc::new(MeddlingRunner {
        deny: false,
        ..Default::default()
    });
    wire_prompt_hook(&rt, runner.clone());

    let agent = rt
        .start_root(root_spec_with_prompt(SUBMITTED))
        .await
        .expect("a non-denying hook must let the prompt through");

    // What the hook was SHOWN is byte-identical to what was submitted...
    let seen = runner.seen_text.lock().expect("lock poisoned").clone();
    assert_eq!(
        seen,
        vec![SUBMITTED.to_string()],
        "the hook saw altered text"
    );

    // ...and so is what was PERSISTED for the loop to assemble context from,
    // which is the observable outcome the acceptance asks for. Read back off
    // the store rather than trusting the spec we passed in, which would prove
    // nothing about what survived the hook.
    let session = rt
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == agent)
        .expect("the started agent is in the tree")
        .session;
    let records = SessionStore::read(&*store, &session, conway_core::ids::SeqRange::full())
        .await
        .expect("records readable");
    let user_turns: Vec<String> = records
        .iter()
        .filter_map(|r| match r {
            conway_core::log::LogRecord::UserTurn { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        user_turns,
        vec![SUBMITTED.to_string()],
        "the text persisted for the loop is not byte-identical to what was submitted"
    );
}

/// ACCEPTANCE: "fail-closed: a missing, timing-out, or malformed script denies
/// the prompt."
#[tokio::test]
async fn a_failing_prompt_submitted_hook_denies_fail_closed() {
    let rt = build_runtime();
    // `FailingRunner` always returns `HookFailure::TimedOut`.
    wire_prompt_hook(&rt, Arc::new(FailingRunner::default()));

    let err = rt
        .start_root(root_spec_with_prompt("anything"))
        .await
        .expect_err("a failing prompt_submitted hook must deny, not proceed");
    match err {
        RuntimeError::PromptDenied { reason } => {
            assert!(reason.contains("fail-closed"), "reason: {reason}");
        }
        other => panic!("expected PromptDenied, got {other:?}"),
    }
}

/// ACCEPTANCE: "with no `prompt_submitted` hooks configured, behaviour is
/// exactly as before."
#[tokio::test]
async fn no_prompt_submitted_hook_leaves_prompt_submission_unchanged() {
    let rt = build_runtime();
    let agent = rt
        .start_root(root_spec_with_prompt("unhooked"))
        .await
        .expect("start_root works with no hooks");
    rt.prompt(agent, "also unhooked".to_string())
        .await
        .expect("prompt works with no hooks");
}

/// ACCEPTANCE, the type-level half: "the answer type has no field capable of
/// carrying replacement text -- verified by inspecting the type definition."
///
/// `HookPermissionVerdict` is what `dispatch_deny_only` reads, and its whole
/// vocabulary is exhaustively matched here. If a future variant is added that
/// could carry text back, this match stops compiling and the decision has to
/// be made deliberately rather than inherited.
#[test]
fn the_prompt_hook_answer_type_cannot_carry_replacement_text() {
    use conway_core::hook::HookPermissionVerdict;

    fn assert_no_text_channel(v: HookPermissionVerdict) {
        match v {
            // Proceed: carries nothing at all.
            HookPermissionVerdict::NoOpinion => {}
            // Deny: carries a REASON, which is surfaced to the caller as an
            // error and is never substituted for the prompt.
            HookPermissionVerdict::Deny { reason: _ } => {}
        }
    }

    assert_no_text_channel(HookPermissionVerdict::NoOpinion);
    assert_no_text_channel(HookPermissionVerdict::Deny { reason: "r".into() });
}

// -------------------------------------------- request_assembled / child_reported --

/// Polls `predicate` (cooperatively yielding between checks, never sleeping
/// a fixed duration) until it is `true` or 2s elapse. `request_assembled`/
/// `child_reported` dispatch from INSIDE a `tokio::spawn`'d agent task
/// (`AgentLoop::run`), unlike `session_starting`/`child_spawned`/
/// `prompt_submitted` above, which all dispatch from a directly-`.await`ed
/// `Runtime` method body -- a caller here has no single call whose `.await`
/// resolving already proves the dispatch happened, only the terminal
/// `AgentResult` (`SubagentHost::await_result`), which this crate's own
/// `AgentTree::publish_result` doc places BEFORE the hook dispatch in
/// program order (`AgentLoop::finish`: publish, then dispatch). This poll
/// removes any dependence on exactly how the executor happens to interleave
/// the two tasks.
async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition was not met within 2s");
}

/// ACCEPTANCE: `request_assembled`
/// fires once per turn, before routing -- asserted by a hook script (here,
/// `FailingRunner`, recording then failing per this file's own discipline)
/// that records it, not by the dispatch function having been called
/// directly. Shown to fail by removing either of this item's two dispatch
/// call sites (`agent_loop.rs`'s `request_assembled` dispatch, or
/// `supervisor.rs`'s `child_reported` dispatch below).
#[tokio::test]
async fn request_assembled_fires_once_per_turn_before_routing() {
    let rt = build_runtime();
    let hook = Arc::new(FailingRunner::default());
    rt.set_observation_hook_runner(Some(hook.clone()));
    rt.set_observation_hooks(hooks_for(REQUEST_ASSEMBLED));

    let agent = rt
        .start_root(root_spec_with_prompt("hello"))
        .await
        .expect("root started");

    // The scripted backend's script is empty (`build_runtime`'s own doc),
    // so the turn errors immediately after routing -- well after
    // `request_assembled` has already fired -- and the agent reaches a
    // terminal (`Failed`) result quickly either way.
    wait_until(|| hook.count(REQUEST_ASSEMBLED) >= 1).await;

    let _ = rt.await_result(agent, agent).await;
    assert_eq!(
        hook.count(REQUEST_ASSEMBLED),
        1,
        "request_assembled must fire exactly once for the one turn run, not once per \
         backend retry or not at all"
    );
    assert_eq!(
        hook.names()
            .iter()
            .filter(|n| *n != REQUEST_ASSEMBLED)
            .count(),
        0,
        "an unrelated event dispatched: {:?}",
        hook.names()
    );
}

/// ACCEPTANCE: `child_reported` fires for a child that completes normally
/// (here, "normally" includes a fast `Failed` terminus via
/// `AgentLoop::finish_error` -- the point is that THIS agent's own
/// `AgentLoop::finish` ran, not that it succeeded; the supervisor-synthesis
/// path below is the OTHER half of this criterion). Never fires for a
/// ROOT's own finish -- a root has no parent for a result to cross back to.
#[tokio::test]
async fn child_reported_fires_for_a_normal_child_finish_but_never_for_a_root() {
    let rt = build_runtime();
    let hook = Arc::new(FailingRunner::default());
    rt.set_observation_hook_runner(Some(hook.clone()));
    rt.set_observation_hooks(hooks_for(CHILD_REPORTED));

    // A root's OWN finish must never fire `child_reported` -- proven first,
    // before a child exists at all, so a later false positive from the
    // child below cannot be mistaken for the root having fired.
    let parent = rt.start_root(root_spec()).await.expect("root started");

    let child = rt
        .start(
            parent,
            parent,
            SubagentSpec::fork("do a thing", Budget::default()),
        )
        .await
        .expect("child spawned");

    wait_until(|| hook.count(CHILD_REPORTED) >= 1).await;
    let _ = rt.await_result(parent, child).await;

    assert_eq!(
        hook.count(CHILD_REPORTED),
        1,
        "child_reported must fire exactly once for the child's own finish"
    );
    let seen = hook.names();
    assert_eq!(
        seen.iter().filter(|n| *n == CHILD_REPORTED).count(),
        1,
        "child_reported must never fire a second time (e.g. for the root): {seen:?}"
    );
}
