//! Board item A5.6 -- the load-bearing test the item's own spec demands:
//! ONE real child, run under a real `Runtime`, with a tight budget, proving
//! all three observable things together rather than three separate,
//! individually-satisfiable checks:
//!
//! 1. the child's OWN session log holds the model-facing wrap-up notice
//!    (`LogRecord::SystemNote { reason: "runway", .. }`) BEFORE its
//!    terminal record -- fired at 80% of `max_steps`, i.e. at step 4 of 5;
//! 2. the PARENT's own session log holds the forwarded crossing notice
//!    (`LogRecord::SystemNote { reason: "child_budget", .. }`), observed
//!    the same way `docs/agents.md`'s "a parent sees a child's completion
//!    on its own next turn" mechanism already works for `ChildResultRecord`
//!    (`mailbox.rs`'s own module doc) -- and the live event stream carries
//!    `Event::BudgetWarning` for the same crossing, the signal `/agents`
//!    marks a row from;
//! 3. the child's terminal `AgentResult` names the tool call that was
//!    genuinely in flight when its deadline killed it.
//!
//! Deliberately does NOT use `supervisor::DEFAULT_GRACE`'s full 2 seconds:
//! the scripted tool (`RacyTool`, below) races `ToolCtx::cancel` against a
//! long sleep and returns the moment cancellation fires, so the owning
//! `AgentLoop` observes `self.cancel.is_cancelled()` and reaches its own
//! `finish_cancelled` well within `grace` -- this test's wall-clock cost is
//! bounded by the deadline (a few hundred ms), not by `grace`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{AgentKnobs, Budget, PermissionDecision, ResultStatus, SubagentSpec};
use conway_core::capabilities::HeadroomPolicy;
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::error::ToolError;
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::{
    Backend, PluginManifest, Router, SessionStore, SubagentHost, Tool, ToolCtx, ToolOutput,
};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{
    text_response_with_stub_usage as text_response, FakeGate, FakeHealth, FakeRouter, FakeStore,
    ScriptedBackend, ScriptedTurn,
};
use futures::StreamExt;

fn build_runtime(backend: Arc<ScriptedBackend>) -> (Arc<Runtime>, Arc<dyn SessionStore>) {
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let runtime = Runtime::new(RuntimeDeps {
        store: store.clone(),
        path_store: Arc::new(conway_testkit::FakePathStore::new()),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![Arc::new(RacyPlugin)],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        instructions: Vec::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
        tool_result_bound: Arc::new(conway_core::capabilities::ToolResultBoundPolicy::default()),
        session_discovery: Arc::new(conway_testkit::FakeSessionDiscoveryHost::new()),
        capabilities: Arc::new(conway_core::ports::CapabilityRegistry::default()),
    });
    (runtime, store)
}

/// `keep_alive: true` -- deliberately, unlike this crate's other test
/// files' own `root_spec` helpers. This test needs the ROOT to still be a
/// live, listening agent (idling at its own resume gate) while the child
/// runs, so that a later `runtime.prompt(root, ..)` wakes the SAME task
/// back up to run `AgentLoop::drain_inbox` -- the only way the child's
/// `AgentMessage::BudgetNotice`, queued into the root's in-memory mailbox
/// while the root was idling, ever becomes a persisted `SystemNote` on the
/// root's own log. A non-`keep_alive` root's task exits for good after its
/// one turn, and nothing in this workspace revives an exited task's mailbox
/// from a second, independent `Runtime` instance (mailboxes are
/// per-process, in-memory state -- never persisted, unlike the session
/// log).
fn root_spec(prompt: &str) -> RootSpec {
    RootSpec {
        session: None,
        knobs: AgentKnobs {
            agent_def: None,
            role: Some(RoleAlias::new("planner")),
            model: None,
            tools: None,
            budget: Budget::default(),
            result_contract: None,
            keep_alive: true,
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

/// A slow tool -- the deterministic stand-in for a real, slow verification
/// command (`bash cargo test`, ...) that would otherwise make this test's
/// timing depend on real subprocess scheduling. Deliberately does NOT
/// itself watch `ctx.cancel` (unlike a real `bash` tool's own kill-group
/// handling): `ToolRunner::run_batch` (`tools/runner.rs`'s own module doc)
/// already races EVERY dispatched call's future against cancellation via
/// `tokio::select!`, structurally, independent of whether the tool
/// cooperates -- so a batch containing this call still returns promptly
/// once the agent's own token trips, exactly as it would for a real tool
/// that ignored the signal. Mirrors `subagent_fork_spawn.rs`'s own
/// `SlowTool`, just held long enough (5s, far past this test's own
/// deadline) that it is unambiguously still "in flight" from
/// `AgentTree::in_flight_tools`'s point of view when that happens.
struct RacyTool;

fn racy_tool_spec() -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name: conway_core::ids::ToolName::new("racy"),
        description: "races cancellation against a long sleep".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: conway_core::content::ToolCategory::Read,
        permission: conway_core::content::PermissionClass::Safe,
    }
}

#[async_trait]
impl Tool for RacyTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        racy_tool_spec()
    }

    async fn invoke(
        &self,
        _call: conway_core::content::ToolCall,
        _ctx: ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "should never reach here".into(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// A tool that returns immediately -- this loop's structure (`agent_loop.
/// rs`'s own `run_inner`) only continues a non-`keep_alive` agent's turn
/// count past a text-only reply's NATURAL completion when a turn dispatches
/// at least one tool call, so the four turns this test needs BEFORE the
/// slow, budget-killed one must each carry one too. Kept fast and
/// deliberately trivial: these four turns exist only to climb
/// `steps_this_turn` to 4 (80% of `max_steps: 5`) before the fifth,
/// `RacyTool`-carrying turn ever starts.
struct FastTool;

fn fast_tool_spec() -> conway_core::content::ToolSpec {
    conway_core::content::ToolSpec {
        name: conway_core::ids::ToolName::new("fast"),
        description: "returns immediately".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: conway_core::content::ToolCategory::Read,
        permission: conway_core::content::PermissionClass::Safe,
    }
}

#[async_trait]
impl Tool for FastTool {
    fn spec(&self) -> conway_core::content::ToolSpec {
        fast_tool_spec()
    }

    async fn invoke(
        &self,
        _call: conway_core::content::ToolCall,
        _ctx: ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "step done".into(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

fn fast_tool_call_response(call_id: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![],
        tool_calls: vec![conway_core::content::ToolCall {
            call_id: call_id.into(),
            name: conway_core::ids::ToolName::new("fast"),
            arguments: serde_json::json!({}),
        }],
        stop: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 5,
            output_tokens: 2,
            ..Default::default()
        },
    }
}

struct RacyPlugin;

impl conway_core::ports::Plugin for RacyPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "racy".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![racy_tool_spec().name, fast_tool_spec().name],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(RacyTool), Arc::new(FastTool)]
    }
}

fn racy_tool_call_response() -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![],
        tool_calls: vec![conway_core::content::ToolCall {
            call_id: "c1".into(),
            name: conway_core::ids::ToolName::new("racy"),
            arguments: serde_json::json!({"cmd": "run the full verification suite"}),
        }],
        stop: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

/// Waits for `agent`'s NEXT `Event::TurnFinished` -- unlike
/// `Event::AgentFinished`, emitted unconditionally at the end of every
/// round-trip, `keep_alive` or not (`AgentLoop::run_inner`'s own per-round
/// emission) -- so this is the right "this turn's work landed" signal for
/// a `keep_alive` root, which never produces `AgentFinished` at all.
async fn wait_for_turn_finished(stream: &mut conway_runtime::events::EventStream, agent: AgentId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent {
                if let Event::TurnFinished { .. } = envelope.event {
                    return;
                }
            }
        }
    })
    .await
    .expect("agent's turn never finished")
}

/// Board item A5.6's own load-bearing test, asserting all three criteria
/// together against ONE real child (a `conway_fork`-shaped
/// `SubagentHost::start`) -- see this file's own module doc.
#[tokio::test]
async fn a_child_near_its_budget_deadline_warns_before_dying_names_its_interrupted_tool_and_tells_its_parent(
) {
    // Root: `keep_alive: true` (`root_spec`'s own doc explains why this
    // test needs that) -- one ordinary scripted turn, then it idles at its
    // own resume gate rather than exiting, so it is still there, later, to
    // `drain_inbox` whatever the child sent it while it was idling.
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("ok")), // root's own FIRST turn
            // Child: four cheap `fast`-tool-calling turns (to climb
            // `steps_this_turn` to 4 -- 80% of `max_steps: 5` -- BEFORE the
            // fifth turn ever dispatches its slow one). A text-only reply
            // would end the run right there as a NATURAL completion
            // (`agent_loop.rs`'s own `run_inner`: a non-`keep_alive` agent's
            // turn count only advances past a tool-call turn), so these
            // four must each carry a call too, not just be idle filler.
            ScriptedTurn::Respond(fast_tool_call_response("f1")),
            ScriptedTurn::Respond(fast_tool_call_response("f2")),
            ScriptedTurn::Respond(fast_tool_call_response("f3")),
            ScriptedTurn::Respond(fast_tool_call_response("f4")),
            ScriptedTurn::Respond(racy_tool_call_response()),
            ScriptedTurn::Respond(text_response("checked")), // root's SECOND turn
        ])
        .with_id(BackendId::new("b")),
    );
    let (runtime, store) = build_runtime(backend);
    let mut stream = runtime.subscribe();

    let root = runtime.start_root(root_spec("hi")).await.unwrap();
    let root_session = session_of(&runtime, root);
    // `keep_alive`, so this root never produces `Event::AgentFinished` --
    // `Event::TurnFinished` is the "this turn's work landed, the agent is
    // back to idling" signal instead (`AgentLoop::run_inner`'s own
    // unconditional per-round `TurnFinished` emission, keep-alive or not).
    wait_for_turn_finished(&mut stream, root).await;

    // A deadline generous enough that four near-instant, in-memory turns
    // can never plausibly blow through it, but tight enough that the
    // fifth turn's `RacyTool` call (which would otherwise sleep 5s) is
    // unambiguously still in flight when it elapses.
    let deadline = Utc::now() + chrono::Duration::milliseconds(300);
    let budget = Budget {
        max_steps: 5,
        deadline: Some(deadline),
        max_tokens: None,
        max_tool_calls: None,
    };
    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        SubagentSpec::fork("please verify the change", budget),
    )
    .await
    .unwrap();
    let child_session = session_of(&runtime, child);

    // Criterion (2), live half: `/agents` marks a row from THIS event --
    // it must arrive before the child's own `AgentFinished`.
    let mut saw_budget_warning_before_finish = false;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent != child {
                continue;
            }
            match envelope.event {
                Event::BudgetWarning { ref limit, .. } if limit.starts_with("max_steps") => {
                    saw_budget_warning_before_finish = true;
                }
                Event::AgentFinished { result, .. } => return result,
                _ => {}
            }
        }
    })
    .await
    .expect("child never finished");

    assert!(
        saw_budget_warning_before_finish,
        "Event::BudgetWarning for max_steps must be observed before the child's own \
         AgentFinished -- this is what lets `/agents` mark the row while the child is \
         still alive, not only after"
    );

    // ---- Criterion (3): the terminal result names the interrupted call ----
    assert!(
        matches!(result.status, ResultStatus::Cancelled { .. }),
        "expected a deadline-mid-tool-call termination to still be Cancelled (no \
         reclassification -- A5.6's own \"no change to what trips a budget\" constraint), \
         got: {:?}",
        result.status
    );
    if let ResultStatus::Cancelled { reason } = &result.status {
        assert!(
            reason.starts_with("budget: deadline="),
            "a deadline-elapsed-mid-tool-call cancellation must say so, not read as an \
             ordinary external cancel, got reason: {reason:?}"
        );
    }
    assert!(
        result.summary.contains("racy("),
        "terminal summary must name the interrupted tool, got: {:?}",
        result.summary
    );
    assert!(
        result.summary.contains("verification suite"),
        "terminal summary must carry a short args summary of the interrupted call, got: {:?}",
        result.summary
    );

    // ---- Criterion (1): the child's OWN log holds the wrap-up notice, ----
    // ---- strictly BEFORE its terminal record.                        ----
    let child_log = store.read(&child_session, SeqRange::full()).await.unwrap();

    let wrap_up_seq = child_log.iter().find_map(|r| match r {
        LogRecord::SystemNote {
            seq, reason, text, ..
        } if reason == "runway" => {
            assert!(
                text.contains("4 of 5") && text.contains("max_steps"),
                "expected the max_steps wrap-up note's exact text, got: {text:?}"
            );
            Some(*seq)
        }
        _ => None,
    });
    let terminal_seq = child_log.iter().find_map(|r| match r {
        LogRecord::AgentResultRecord { seq, .. } => Some(*seq),
        _ => None,
    });
    let wrap_up_seq = wrap_up_seq.expect("child's own log must hold the runway wrap-up note");
    let terminal_seq = terminal_seq.expect("child's own log must hold its terminal record");
    assert!(
        wrap_up_seq < terminal_seq,
        "the wrap-up notice (seq {wrap_up_seq:?}) must precede the terminal record \
         (seq {terminal_seq:?}) -- the child must be warned BEFORE it dies, not after"
    );

    // ---- Criterion (2), durable half: the PARENT's own log holds the ----
    // ---- forwarded crossing notice.                                  ----
    // `AgentMessage::BudgetNotice` was queued into the root's mailbox
    // while the root itself was idling (`keep_alive`, at its own resume
    // gate) -- exactly like `ChildResultRecord`'s own documented "observed
    // on the parent's very next turn" mechanism (`mailbox.rs`'s module
    // doc). Waking the SAME still-alive root task with a fresh prompt
    // (rather than a second `Runtime`/`resume_root` -- a mailbox is
    // per-process, in-memory state that a persisted-log-only "process
    // restart" could never see in the first place) makes its very next
    // `run_inner` iteration `drain_inbox` -- persisting the queued
    // `child_budget` notice (and the child's own terminal `Result`) --
    // BEFORE it ever reassembles context for this new turn.
    runtime
        .prompt(root, "check on the child".to_string())
        .await
        .unwrap();
    wait_for_turn_finished(&mut stream, root).await;

    let root_log = store.read(&root_session, SeqRange::full()).await.unwrap();
    let crossing = root_log.iter().find_map(|r| match r {
        LogRecord::SystemNote { reason, text, .. } if reason == "child_budget" => {
            Some(text.clone())
        }
        _ => None,
    });
    let crossing = crossing.expect(
        "the parent's own log must hold a SystemNote{reason: \"child_budget\"} record \
         forwarded from the child's crossing",
    );
    assert!(
        crossing.contains("max_steps"),
        "the forwarded notice must name the crossed dimension, got: {crossing:?}"
    );
    assert!(
        crossing.contains(&child.to_string()),
        "the forwarded notice must name which child crossed it, got: {crossing:?}"
    );
}
