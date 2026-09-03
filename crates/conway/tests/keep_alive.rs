//! Acceptance tests for `SessionSpec::keep_alive` (opt-in multi-turn
//! sessions): the confirmed bug this item fixes is that a live session's
//! agent task terminates after ONE prompt-to-completion turn
//! (`AgentLoop::run_inner`'s natural-completion branch), so a SECOND
//! `SessionHandle::prompt` on the same handle -- as `conway-cli`'s TUI issues
//! for every chat message -- silently appends a `UserTurn` nobody ever reads.
//!
//! **A session started without an initial prompt (`spec.prompt: None`,
//! which is what `Conway::new_session` always passes) starts IDLE**
//! (`Runtime::start_root`'s own doc): no placeholder turn is ever written or
//! run, and the agent loop gates until the first `prompt()` call arrives. So
//! `new_session` followed only by a `sleep` observes ZERO backend calls, and
//! a test's FIRST `prompt()` drives the FIRST turn -- not a second one. Some
//! tests below still `sleep` (`SETTLE`) after `new_session`, not to wait out
//! a turn (a prompt-less session never runs one) but to give the idle agent
//! loop a moment to actually reach its idle-await gate before the test's own
//! `prompt()` call races it (an adaptation of `resume.rs`'s own settle-
//! before-asserting idiom to an idling, rather than a completing, task).
//!
//! `keep_alive_true_session_runs_a_genuine_second_turn_in_the_same_process`
//! is the headline regression test: it would time out against pre-fix `main`
//! (the second `TurnHandle::text()` never observes a `TurnFinished` because
//! no second turn ever runs).
//!
//! `TurnHandle::result()` is deliberately NOT used against a `keep_alive`
//! handle here: per `AgentLoop`'s own doc, a keep-alive turn's completion
//! does not emit `Event::AgentFinished` (that would end the task) -- a
//! keep-alive session is consumed turn-by-turn via `TurnFinished`
//! (`TurnHandle::text()`/`events()`), and `AgentFinished` only ever arrives
//! once, at the session's real end (cancel/deadline/budget).

use std::sync::Arc;
use std::time::Duration;

use conway::test_support::{base_config, build_conway, test_builder};
use conway::{Plugin, SessionSpec, Tool};
use conway_core::agent::{Budget, ResultStatus};
use conway_core::content::ContentBlock;
use conway_core::event::Event;
use conway_core::ids::{BackendId, RoleAlias, SeqRange};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, SessionStore};
use conway_testkit::{
    text_response, text_response_with_stub_usage, FakeStore, ScriptedBackend, ScriptedTurn,
};

/// How long a test sleeps after `new_session` to give an idle keep_alive
/// session's agent loop a moment to actually reach its idle-await gate
/// before the test's own `prompt()` call races it (see this file's module
/// doc) -- not to wait out any turn, since a prompt-less session never runs
/// one.
const SETTLE: Duration = Duration::from_millis(100);

/// Mirrors `resume.rs`/`ask.rs`'s own identical helper -- this crate has no
/// shared fixture module for it (each integration test binary is its own
/// crate root).
/// Mirrors `ask.rs`'s own identical helper -- builds a `GenerateResponse`
/// carrying exactly one tool call, no text content.
fn tool_call_response(call_id: &str, tool: &str, args: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![conway_core::content::ToolCall {
            call_id: call_id.to_string(),
            name: conway_core::ids::ToolName::new(tool),
            arguments: args,
        }],
        stop: conway_core::content::StopReason::ToolUse,
        usage: conway_core::content::Usage::default(),
    }
}

fn schema_any_object() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
}

/// A trivial tool that always succeeds -- only its invocability (giving a
/// keep-alive turn a tool-call step to take), not its output, matters for
/// the tests below that need multi-step turns.
struct ProbeTool;

#[async_trait::async_trait]
impl Tool for ProbeTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        conway_core::content::ToolSpec {
            name: conway_core::ids::ToolName::new("probe"),
            description: "test-only probe tool".into(),
            schema: schema_any_object(),
            category: conway_core::content::ToolCategory::Read,
            permission: conway_core::content::PermissionClass::Safe,
        }
    }

    async fn invoke(
        &self,
        _call: conway_core::content::ToolCall,
        _ctx: conway_core::ports::ToolCtx,
    ) -> Result<conway_core::ports::ToolOutput, conway_core::error::ToolError> {
        Ok(conway_core::ports::ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "probed".to_string(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// `ConwayBuilder::build` already registers the real `report` tool by
/// default (`conway-tools`' `ReportTool`, via `conway.report`'s builtin
/// plugin) -- registering a second one under the same name is rejected as a
/// duplicate (`RuntimeDeps` construction), so the `report`-calling test
/// below drives that real tool directly instead of faking one.
struct FixtureToolsPlugin;

impl Plugin for FixtureToolsPlugin {
    fn manifest(&self) -> conway_core::ports::PluginManifest {
        conway_core::ports::PluginManifest {
            id: "test.fixture-tools".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![conway_core::ids::ToolName::new("probe")],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(ProbeTool)]
    }
}

/// Drains `stream` until it yields `Event::AgentFinished`, returning its
/// `AgentResult` -- for the cancel/deadline/session-lifetime-budget tests
/// below, which (unlike the keep-alive-turn tests) DO expect a real
/// terminal event, since a genuine end (cancel/deadline/`max_tokens`) is
/// exactly when a `keep_alive` session's one and only `AgentFinished` is
/// emitted. Board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`: a `max_steps`/
/// `max_tool_calls` trip is NO LONGER one of those genuine ends for a
/// `keep_alive` agent -- see `next_turn_aborted`, immediately below, for
/// that case instead.
async fn next_agent_finished(
    stream: &mut conway::EventStream,
) -> Option<conway_core::agent::AgentResult> {
    use futures_core::Stream as _;
    loop {
        let envelope =
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)).await?;
        if let Event::AgentFinished { result, .. } = envelope.event {
            return Some(result);
        }
    }
}

/// Drains `stream` until it yields `Event::TurnAborted`, returning its
/// `limit`/`steps_this_turn` fields. Board item
/// `01M1FSP1QJFCHA7H8QPYZ9GG1P`: the live signal that a `keep_alive`
/// agent's turn-scoped budget dimension (`max_steps`/`max_tool_calls`)
/// tripped and ended just the current turn -- the session itself is still
/// alive and returns to idling for the next prompt, so callers must NOT
/// also wait for `Event::AgentFinished` here (it never comes for this
/// trip).
async fn next_turn_aborted(stream: &mut conway::EventStream) -> Option<(String, u32)> {
    use futures_core::Stream as _;
    loop {
        let envelope =
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)).await?;
        if let Event::TurnAborted {
            limit,
            steps_this_turn,
            ..
        } = envelope.event
        {
            return Some((limit, steps_this_turn));
        }
    }
}

/// Drains `stream` for exactly `turn_finished_count` occurrences of
/// `Event::TurnFinished`, concatenating every `Event::TextDelta` seen along
/// the way. Needed instead of `TurnHandle::text()` for a keep-alive turn
/// that takes more than one internal step (e.g. a tool call followed by a
/// natural-completion text response): `TurnFinished` is emitted once per
/// internal step (`conway_runtime::agent_loop`'s own module doc: "a turn is
/// one model generation"), so `TurnHandle::text()` alone would stop
/// draining at the FIRST internal step -- before the turn's real, final
/// response ever arrives -- not at the whole user turn's end. The caller
/// must know the exact per-turn step count up front (true of every test
/// below: the backend script is fully known).
async fn drain_n_turn_finished(
    stream: &mut conway::EventStream,
    turn_finished_count: usize,
) -> String {
    use futures_core::Stream as _;
    let mut text = String::new();
    let mut seen = 0usize;
    loop {
        let envelope = std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx))
            .await
            .expect("event stream must not end mid-session");
        match envelope.event {
            Event::TextDelta { text: delta } => text.push_str(&delta),
            Event::TurnFinished { .. } => {
                seen += 1;
                if seen == turn_finished_count {
                    return text;
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// The fix: a second `prompt()` in the same process runs a genuine turn.
// ---------------------------------------------------------------------

/// The headline regression test. Pre-fix (no `keep_alive` gate at the end of
/// a completed turn), the root agent's task terminates the instant its first
/// turn completes with no tool calls -- so `turn2.text()` below would never
/// observe a `TurnFinished` and the `timeout` would fire. This test proves
/// the opposite: a SECOND `prompt()` on the SAME live session, in the SAME
/// process, drives a genuine additional backend call (`ScriptedBackend`'s
/// own call log is the ground truth -- not just "the record was appended").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_true_session_runs_a_genuine_second_turn_in_the_same_process() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("first-response")),
            ScriptedTurn::Respond(text_response("second-response")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend.clone(), store);

    let spec = SessionSpec {
        keep_alive: true,
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");

    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        backend.calls().len(),
        0,
        "a keep_alive session idles until the first prompt -- no spontaneous turn runs"
    );

    let turn1 = handle
        .prompt("first turn text")
        .await
        .expect("first prompt should succeed");
    let text1 = tokio::time::timeout(Duration::from_secs(5), turn1.text())
        .await
        .expect("first turn's text() must not hang")
        .expect("first turn's text() should succeed");
    assert_eq!(text1, "first-response");
    assert_eq!(
        backend.calls().len(),
        1,
        "the first explicit prompt must have driven exactly one new backend call"
    );

    let turn2 = handle
        .prompt("second turn text")
        .await
        .expect("second prompt on the SAME live session should succeed");
    let text2 = tokio::time::timeout(Duration::from_secs(5), turn2.text())
        .await
        .expect(
            "second turn's text() must not hang -- this is exactly the fix: a keep_alive \
             session's task must still be alive to run a genuine second turn",
        )
        .expect("second turn's text() should succeed");
    assert_eq!(text2, "second-response");
    assert_eq!(
        backend.calls().len(),
        2,
        "the second explicit prompt must ALSO have driven a new, second backend call -- proving a \
         real second turn ran, not just that the UserTurn record was appended: {:?}",
        backend.calls()
    );
}

// ---------------------------------------------------------------------
// Default unchanged: non-keep-alive sessions are untouched by this item.
// See also `crates/conway/tests/session_handle_subagent.rs`'s
// `await_agent`-based fork/spawn tests (e.g.
// `same-session await_agent must not hang`, and
// `fork_produces_a_child_with_mapped_fields_and_an_inherited_prefix`/
// `spawn_produces_a_child_with_mapped_fields_and_no_inherited_prefix`), which
// already prove a spawned/forked child (always `keep_alive: false`,
// `subagent.rs`) terminates and a parent awaiting its `AgentResult` does not
// hang -- unmodified by this item, still green.
// ---------------------------------------------------------------------

/// Mirrors `crates/conway/tests/session_handle.rs`'s own already-green
/// `turn_handle_text_then_result_does_not_deadlock` -- `SessionSpec::default()`
/// (`keep_alive: false`) must still resolve `result()` normally. This test
/// does NOT pre-settle past the spontaneous initial turn (unlike every
/// `keep_alive: true` test above): for a non-keep_alive session that
/// spontaneous turn's own completion is itself the proof this path is
/// unaffected -- see `SessionHandle::prompt`'s own doc ("subscribe before
/// append" -- no gap either turn's `AgentFinished` could fall through).
#[tokio::test]
async fn keep_alive_false_default_session_still_terminates_after_one_turn() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("done"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend, store);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect(
            "result() must resolve for a non-keep_alive session -- SessionSpec::default() must \
             be byte-for-byte unaffected by this item",
        )
        .expect("result() should succeed");
    assert!(matches!(result.status, ResultStatus::Completed));
}

// ---------------------------------------------------------------------
// ask()'s ephemeral child is never keep-alive (slice A, unaffected).
// ---------------------------------------------------------------------

/// Light, item-local confirmation alongside `tests/ask.rs`'s own existing
/// coverage (every test there already awaits `ask_turn.result()`
/// successfully, e.g. `ask_child_is_hidden_from_default_listing_...`) that
/// `/ask`'s fork-ask child is NOT keep-alive: `SessionHandle::ask` builds
/// its `SubagentSpec` with `keep_alive: false` (post-B2 it goes through
/// `SubagentHost::start`; pre-B2 the `resume_root` path hard-coded the
/// same). If the child were keep-alive, `result()` below would hang instead
/// of resolving.
#[tokio::test]
async fn ask_child_is_not_keep_alive_and_its_result_resolves() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    // Mirrors `tests/ask.rs`'s own identical two-response script for the
    // "parent turn, then ask" shape -- no settle needed, same as that file.
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend, store);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result must not hang")
        .expect("parent result should succeed");

    let ask_turn = handle.ask("a question").await.expect("ask should succeed");
    let result = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask child's result() must not hang -- it must not be keep_alive")
        .expect("ask child's result() should succeed");
    assert!(matches!(result.status, ResultStatus::Completed));
}

// ---------------------------------------------------------------------
// Termination: cancel/deadline still end an idle keep-alive session.
// ---------------------------------------------------------------------

/// While idle-awaiting between turns (the exact state a keep-alive session
/// sits in most of its life), a cancel must still terminate the task --
/// `keep_alive` must never turn into an un-cancellable session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_ends_an_idle_keep_alive_session() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("spontaneous-ack")),
            ScriptedTurn::Respond(text_response("ack")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend, store);

    let spec = SessionSpec {
        keep_alive: true,
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");
    tokio::time::sleep(SETTLE).await;

    let mut events = handle.events();
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.text())
        .await
        .expect("text() must not hang")
        .expect("text() should succeed");

    // The turn is done and (per the fix) the task is now idle-awaiting the
    // next prompt rather than having exited -- give the scheduler a moment
    // to actually land there before cancelling, though the top-of-loop
    // cancel check would also catch it if this raced.
    tokio::time::sleep(SETTLE).await;

    handle
        .cancel(handle.root(), "test cancel")
        .await
        .expect("cancel should succeed against a live agent");

    let result = tokio::time::timeout(Duration::from_secs(5), next_agent_finished(&mut events))
        .await
        .expect("cancelling an idle keep_alive session must not hang")
        .expect("the event stream must yield AgentFinished before ending");
    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );
}

/// A deadline reached while idle-awaiting between turns must also terminate
/// the session -- keep-alive spans the deadline across turns rather than
/// resetting or ignoring it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deadline_ends_an_idle_keep_alive_session() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("spontaneous-ack")),
            ScriptedTurn::Respond(text_response("ack")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend, store);

    let spec = SessionSpec {
        keep_alive: true,
        budget: Some(Budget {
            max_steps: 40,
            deadline: Some(chrono::Utc::now() + chrono::Duration::milliseconds(400)),
            max_tokens: None,
            max_tool_calls: None,
        }),
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");

    let mut events = handle.events();
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.text())
        .await
        .expect("text() must not hang")
        .expect("text() should succeed");

    // Do NOT prompt again -- the session must end on its own once the
    // deadline passes while idle-awaiting, never by idling forever.
    let result = tokio::time::timeout(Duration::from_secs(5), next_agent_finished(&mut events))
        .await
        .expect("an idle keep_alive session must terminate once its deadline passes")
        .expect("the event stream must yield AgentFinished before ending");
    // Disclosed, pre-existing race (not introduced by keep-alive, and not
    // this item's to fix): `runtime.rs`'s `supervise` wraps every agent task
    // with its OWN independent deadline enforcement (`supervisor.rs`'s
    // `deadline_sleep` + `cancel.cancel()`), racing the exact same
    // `CancellationToken` this idle wait's `biased` `tokio::select!` also
    // watches (`agent_loop.rs`'s `ResumeGate` branch, first in the biased
    // order). Whichever notices the deadline first -- this loop's own
    // `tokio::time::sleep(remaining)` (-> `BudgetExceeded`) or the
    // supervisor's external cancel (-> this loop's own `finish_cancelled`,
    // status `Cancelled`) -- wins; both are a real, non-synthesized result
    // from the SAME task (`conway-runtime/tests/supervisor.rs`'s own
    // `deadline_elapsed_while_blocked_resolves_budget_exceeded` sidesteps
    // this by using a task that never observes cancellation at all). Either
    // outcome proves what this test is actually asserting: the idle session
    // terminated on its own once the deadline passed, rather than idling
    // forever.
    assert!(
        matches!(
            result.status,
            ResultStatus::BudgetExceeded { .. } | ResultStatus::Cancelled { .. }
        ),
        "expected BudgetExceeded or Cancelled (deadline), got {:?}",
        result.status
    );
}

/// Board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`, acceptance criterion 3:
/// session-lifetime dimensions are UNCHANGED by this item -- only
/// `max_steps`/`max_tool_calls` (turn-scoped) became turn-aborting;
/// `max_tokens` (session-lifetime, like `deadline`) still ends the whole
/// `keep_alive` agent, exactly as before. `state.usage` accrues across
/// every turn and is never reset at a turn boundary (`LoopState::usage`'s
/// own doc), so `check_budget`'s `max_tokens` check trips at the very top
/// of the next loop iteration -- immediately after the first turn
/// naturally completes, before this test ever issues a second prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_session_max_tokens_exceeded_still_ends_the_whole_session() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response_with_stub_usage(
            "hi there",
        ))])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend, store);

    let spec = SessionSpec {
        keep_alive: true,
        budget: Some(Budget {
            max_steps: 0,
            deadline: None,
            // `text_response_with_stub_usage` accrues 10 + 5 = 15 tokens --
            // comfortably over this ceiling after just the one turn below.
            max_tokens: Some(10),
            max_tool_calls: None,
        }),
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");

    let mut events = handle.events();
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.text())
        .await
        .expect("text() must not hang")
        .expect("text() should succeed");

    // Do NOT prompt again -- a session-lifetime trip must end the agent on
    // its own, the same as it always has, unlike the turn-scoped trips this
    // item changes.
    let result = tokio::time::timeout(Duration::from_secs(5), next_agent_finished(&mut events))
        .await
        .expect("a max_tokens trip must still terminate the whole keep_alive session")
        .expect("the event stream must yield AgentFinished before ending");
    match &result.status {
        ResultStatus::BudgetExceeded { limit } => {
            assert_eq!(
                limit, "max_tokens=10",
                "max_tokens carries no turn/session scope suffix -- it is only ever \
                 session-lifetime, unlike max_steps/max_tool_calls"
            );
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Critical fix: `max_steps` is a PER-USER-TURN runaway guard for a
// keep_alive session, not a session-lifetime total.
// ---------------------------------------------------------------------

/// The critical-fix headline regression test. `max_steps` is set to 3 --
/// smaller than even one of the six turns below would need if the budget
/// were still a session-lifetime total (each turn takes 2 steps: a tool
/// call, then a final text response; 6 turns * 2 steps = 12 total steps,
/// far exceeding `max_steps=3`). Pre-fix, `check_budget` gates on
/// `state.turn` -- a monotonic, whole-session counter -- so this session
/// dies partway through turn 2 (`2 [turn 1] + 1 [turn 2] = 3 >=
/// max_steps`), and `turn.text()` for turn 2 onward never observes the
/// real per-turn response. Post-fix, `check_budget` gates a `keep_alive`
/// agent on `state.turn_steps` (reset at every turn boundary), so every
/// turn gets its own fresh 3-step allowance and the session survives all
/// six turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_session_survives_many_turns_whose_total_steps_exceed_max_steps() {
    const TURNS: usize = 6;

    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let mut script = Vec::new();
    for i in 1..=TURNS {
        script.push(ScriptedTurn::Respond(tool_call_response(
            &format!("tc_{i}"),
            "probe",
            serde_json::json!({}),
        )));
        script.push(ScriptedTurn::Respond(text_response(&format!(
            "turn-{i}-response"
        ))));
    }
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_plugin(Arc::new(FixtureToolsPlugin))
        .build()
        .expect("build should succeed with every port injected");

    let spec = SessionSpec {
        keep_alive: true,
        budget: Some(Budget {
            max_steps: 3,
            deadline: None,
            max_tokens: None,
            max_tool_calls: None,
        }),
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");

    tokio::time::sleep(SETTLE).await;
    assert_eq!(
        backend.calls().len(),
        0,
        "a keep_alive session idles until the first prompt -- no spontaneous turn runs"
    );

    // Subscribed once, before the first prompt, so no turn's events can be
    // missed by a subscribe-after-append race (mirrors `SessionHandle::
    // prompt`'s own doc). Each turn below takes exactly 2 internal steps
    // (tool call, then a natural-completion text response), so
    // `drain_n_turn_finished(&mut events, 2)` drains exactly one whole user
    // turn per call.
    let mut events = handle.events();

    for i in 1..=TURNS {
        let _turn = handle
            .prompt(format!("turn {i} text"))
            .await
            .unwrap_or_else(|_| panic!("prompt for turn {i} should succeed"));
        let text = tokio::time::timeout(
            Duration::from_secs(5),
            drain_n_turn_finished(&mut events, 2),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "turn {i} must not hang -- this is exactly the critical fix: max_steps must \
                     gate PER USER TURN, not the whole session's total step count ({TURNS} \
                     turns * 2 steps/turn = {} total steps, far exceeding max_steps=3)",
                TURNS * 2
            )
        });
        assert_eq!(
            text,
            format!("turn-{i}-response"),
            "turn {i} must still receive its own genuine response, not an empty/budget-cut-off \
             one"
        );
    }

    assert_eq!(
        backend.calls().len(),
        TURNS * 2,
        "every one of the {TURNS} turns must have driven its own 2 backend calls -- {} total \
         steps, far exceeding max_steps=3 -- proving the budget is scoped per user turn, not per \
         session",
        TURNS * 2
    );
}

/// Board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`, acceptance criterion 1. Runaway
/// protection is preserved -- a SINGLE keep-alive turn whose tool loop never
/// naturally completes still gets cut off once ITS OWN step count hits
/// `max_steps`, exactly like the non-keep-alive path always has -- but
/// cutting it off now ends only that ONE turn, not the whole session:
/// `Event::TurnAborted` fires (not `Event::AgentFinished`), a
/// `LogRecord::SystemNote { reason: "budget_turn_aborted", .. }` tells the
/// model why, and a SUBSEQUENT prompt on the same handle is processed and
/// completes normally.
///
/// **Shown to fail first (P-15):** before this item, `check_budget` called
/// `self.finish(ResultStatus::BudgetExceeded, ..)` for this exact trip,
/// ending the whole agent -- so against the unmodified loop this test's
/// first `tokio::time::timeout(.., next_turn_aborted(&mut events))` times
/// out (no `Event::TurnAborted` variant existed at all; the agent instead
/// emits `Event::AgentFinished` and the task then exits, so the SECOND
/// prompt this test issues afterward finds no live task left to notify).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_single_turn_runaway_tool_loop_aborts_the_turn_and_the_session_survives() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let store_check = store.clone();
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response("tc_1", "probe", serde_json::json!({}))),
            ScriptedTurn::Respond(tool_call_response("tc_2", "probe", serde_json::json!({}))),
            ScriptedTurn::Respond(tool_call_response("tc_3", "probe", serde_json::json!({}))),
            // Consumed by the SECOND prompt below, proving the session
            // survived the trip: a plain text reply, no tool call. If the
            // budget trip regresses to NOT aborting the turn (the old,
            // unfixed behavior), the loop would try to consume a 4th
            // script entry here and ScriptedBackend's own exhaustion
            // panic reports that clearly, rather than this test silently
            // asserting against the wrong response.
            ScriptedTurn::Respond(text_response("recovered")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_plugin(Arc::new(FixtureToolsPlugin))
        .build()
        .expect("build should succeed with every port injected");

    let spec = SessionSpec {
        keep_alive: true,
        budget: Some(Budget {
            max_steps: 3,
            deadline: None,
            max_tokens: None,
            max_tool_calls: None,
        }),
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");
    tokio::time::sleep(SETTLE).await;

    let mut events = handle.events();
    let _turn = handle
        .prompt("go wild")
        .await
        .expect("prompt should succeed");

    let (limit, steps_this_turn) =
        tokio::time::timeout(Duration::from_secs(5), next_turn_aborted(&mut events))
            .await
            .expect("a runaway single-turn tool loop must still trip max_steps and abort")
            .expect("the event stream must yield TurnAborted before ending");
    assert_eq!(
        limit, "max_steps=3",
        "the bare turn-scoped limit, no session-ended framing"
    );
    assert_eq!(
        steps_this_turn, 3,
        "the runaway loop's own step count at the moment it tripped"
    );
    assert_eq!(
        backend.calls().len(),
        3,
        "exactly 3 in-turn steps must have run before the per-turn budget tripped, without ever \
         reaching tc_4/tc_5"
    );

    // The durable half of the fix: the model was told why, in the log.
    let records = store_check
        .read(&handle.id(), SeqRange::full())
        .await
        .expect("read session records");
    let note = records
        .into_iter()
        .find_map(|r| match r {
            LogRecord::SystemNote { text, reason, .. } if reason == "budget_turn_aborted" => {
                Some(text)
            }
            _ => None,
        })
        .expect("a budget_turn_aborted SystemNote must be appended");
    assert!(note.contains("max_steps=3"), "{note}");
    assert!(note.contains("3 steps this turn"), "{note}");

    // The session survived: a SECOND prompt on the same handle is processed
    // and completes normally -- this is exactly the "notice, not the
    // conversation" fix.
    let _turn2 = handle
        .prompt("continue")
        .await
        .expect("second prompt should succeed -- the session must still be alive");
    let text = tokio::time::timeout(
        Duration::from_secs(5),
        drain_n_turn_finished(&mut events, 1),
    )
    .await
    .expect("the recovery turn must not hang -- the session survived the turn-scoped trip");
    assert_eq!(text, "recovered");
}

/// Board item `01M1FS8R09PGF9HHV95B7A6RMH` established that a keep_alive
/// trip's `limit` is turn-scoped, not session-scoped, and that
/// `AgentResult` carries both `steps_taken` (session-lifetime) and
/// `steps_this_turn`. Board item `01M1FSP1QJFCHA7H8QPYZ9GG1P` goes further:
/// that trip no longer produces an `AgentResult` at all for a `keep_alive`
/// agent -- it aborts just the turn (`Event::TurnAborted`, carrying `limit`/
/// `steps_this_turn` directly) and the session survives. This test proves
/// the trip is STILL scoped to the runaway turn's own step count alone (two
/// ordinary turns run to completion first, accruing `NORMAL_TURNS * 2 = 4`
/// steps of session-lifetime history that must NOT count toward the third
/// turn's own `max_steps=3` ceiling), and that the session survives the
/// trip well enough to process a fourth prompt normally afterward.
///
/// **Shown to fail first (P-15):** before this item, this exact trip called
/// `self.finish(ResultStatus::BudgetExceeded, ..)` and ended the whole
/// agent -- `Event::TurnAborted` did not exist, so this test does not even
/// compile against the unfixed tree, and the fourth prompt below would find
/// no live task left to notify even if it did.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_max_steps_trip_is_turn_scoped_and_the_session_survives_it() {
    const NORMAL_TURNS: usize = 2;

    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let store_check = store.clone();
    let mut script = Vec::new();
    for i in 1..=NORMAL_TURNS {
        script.push(ScriptedTurn::Respond(tool_call_response(
            &format!("tc_{i}"),
            "probe",
            serde_json::json!({}),
        )));
        script.push(ScriptedTurn::Respond(text_response(&format!(
            "turn-{i}-response"
        ))));
    }
    // The third prompt: a runaway tool loop that never completes naturally.
    // Exactly 3 of these are consumed if the per-turn budget trips where it
    // should (max_steps=3) -- ScriptedBackend serves entries strictly in
    // call order with no notion of a "turn," so any extra entries here
    // would be consumed by the FOURTH prompt below instead of a fresh
    // regression-only exhaustion, silently shifting `turn-4-response` out
    // from under it.
    for i in 1..=3 {
        script.push(ScriptedTurn::Respond(tool_call_response(
            &format!("runaway_{i}"),
            "probe",
            serde_json::json!({}),
        )));
    }
    // The fourth, final prompt: proves the session survived the trip. If
    // the budget trip regresses to NOT aborting the turn, the runaway loop
    // would try to consume a 4th script entry here and ScriptedBackend's
    // own exhaustion panic reports that clearly.
    script.push(ScriptedTurn::Respond(text_response("turn-4-response")));
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_plugin(Arc::new(FixtureToolsPlugin))
        .build()
        .expect("build should succeed with every port injected");

    let spec = SessionSpec {
        keep_alive: true,
        budget: Some(Budget {
            max_steps: 3,
            deadline: None,
            max_tokens: None,
            max_tool_calls: None,
        }),
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");
    tokio::time::sleep(SETTLE).await;

    let mut events = handle.events();
    for i in 1..=NORMAL_TURNS {
        let _turn = handle
            .prompt(format!("turn {i} text"))
            .await
            .unwrap_or_else(|_| panic!("prompt for turn {i} should succeed"));
        let text = tokio::time::timeout(
            Duration::from_secs(5),
            drain_n_turn_finished(&mut events, 2),
        )
        .await
        .unwrap_or_else(|_| panic!("turn {i} must not hang"));
        assert_eq!(text, format!("turn-{i}-response"));
    }

    let _turn = handle
        .prompt("go wild")
        .await
        .expect("third prompt should succeed");
    let (limit, steps_this_turn) =
        tokio::time::timeout(Duration::from_secs(5), next_turn_aborted(&mut events))
            .await
            .expect("the runaway third turn must still trip max_steps and abort")
            .expect("the event stream must yield TurnAborted before ending");

    assert_eq!(
        limit, "max_steps=3",
        "a keep_alive trip names the bare turn-scoped limit"
    );
    assert_eq!(
        steps_this_turn,
        3,
        "the THIRD turn's own runaway loop must have taken exactly max_steps=3 steps of ITS \
         OWN before aborting -- NOT the {} steps the two prior turns already accrued \
         session-lifetime",
        (NORMAL_TURNS as u32) * 2
    );

    let records = store_check
        .read(&handle.id(), SeqRange::full())
        .await
        .expect("read session records");
    assert!(
        records.iter().any(|r| matches!(
            r,
            LogRecord::SystemNote { reason, .. } if reason == "budget_turn_aborted"
        )),
        "a budget_turn_aborted SystemNote must be appended"
    );

    // The session survives the trip: a fourth prompt is processed normally.
    let _turn4 = handle
        .prompt("turn 4 text")
        .await
        .expect("fourth prompt should succeed -- the session must still be alive");
    let text = tokio::time::timeout(
        Duration::from_secs(5),
        drain_n_turn_finished(&mut events, 1),
    )
    .await
    .expect("the fourth turn must not hang -- the session survived the turn-scoped trip");
    assert_eq!(text, "turn-4-response");
}

// ---------------------------------------------------------------------
// Significant fix: the terminal `AgentResult` reflects the LAST turn, not
// whole-session-accumulated `report`/tool-artifact history.
// ---------------------------------------------------------------------

/// A keep_alive session calls `report` on an early turn, then several
/// unrelated plain-text turns run, and the session is finally cancelled
/// while idle. Pre-fix, `result_builder`'s `last_report` is a whole-run
/// accumulator never reset at a turn boundary, so the stale early `report`
/// call still wins at the finish boundary. Post-fix, the keep-alive turn
/// boundary resets `result_builder` every time a turn completes naturally,
/// so by the time the session ends several turns later, that early report
/// is long gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_terminal_result_does_not_leak_a_stale_early_report() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            // Turn 1: calls `report` with a distinctive summary, then a
            // trailing text response completes the turn naturally.
            ScriptedTurn::Respond(tool_call_response(
                "tc_report",
                "report",
                serde_json::json!({"summary": "EARLY REPORT MUST NOT SURVIVE"}),
            )),
            ScriptedTurn::Respond(text_response("turn-1-response")),
            // Turns 2 and 3: plain, unrelated text turns -- no `report`
            // call.
            ScriptedTurn::Respond(text_response("turn-2-response")),
            ScriptedTurn::Respond(text_response("turn-3-response")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = test_builder(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_plugin(Arc::new(FixtureToolsPlugin))
        .build()
        .expect("build should succeed with every port injected");

    let spec = SessionSpec {
        keep_alive: true,
        ..SessionSpec::default()
    };
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");
    tokio::time::sleep(SETTLE).await;

    // Subscribed once, before the first prompt (mirrors `SessionHandle::
    // prompt`'s own subscribe-before-append discipline), and reused all the
    // way through the final cancel below -- one continuous live stream.
    // Turn 1 takes 2 internal steps (the `report` tool call, then a
    // natural-completion text response); turns 2 and 3 are plain
    // one-step text turns.
    let mut events = handle.events();

    for (i, expected, steps) in [
        (1, "turn-1-response", 2usize),
        (2, "turn-2-response", 1),
        (3, "turn-3-response", 1),
    ] {
        let _turn = handle
            .prompt(format!("turn {i} text"))
            .await
            .unwrap_or_else(|_| panic!("prompt for turn {i} should succeed"));
        let text = tokio::time::timeout(
            Duration::from_secs(5),
            drain_n_turn_finished(&mut events, steps),
        )
        .await
        .unwrap_or_else(|_| panic!("turn {i} must not hang"));
        assert_eq!(text, expected);
    }

    tokio::time::sleep(SETTLE).await;
    handle
        .cancel(handle.root(), "test cancel")
        .await
        .expect("cancel should succeed against a live agent");
    let result = tokio::time::timeout(Duration::from_secs(5), next_agent_finished(&mut events))
        .await
        .expect("cancelling an idle keep_alive session must not hang")
        .expect("the event stream must yield AgentFinished before ending");

    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        result.status
    );
    assert!(
        !result.summary.contains("EARLY REPORT MUST NOT SURVIVE"),
        "the terminal AgentResult must not reflect turn 1's stale `report` call after three \
         later, unrelated turns have run -- got summary: {:?}",
        result.summary
    );
}
