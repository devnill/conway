//! End-to-end acceptance tests for the `stop: max_tokens` transcript notice
//! (board item `01M23JRDAM480SRXFGM46GBVA6`) -- the fix for a real
//! dogfooding incident: a turn spent its entire output budget inside a
//! `Thinking` block, emitted no `Text` and no `ToolUse` block, and conway
//! showed the operator nothing at all. The operator had to type "what just
//! happened?" to find out; conway's own answer, reading back its own log,
//! was that it had no record of its own turn either.
//!
//! **Shown to fail first (P-15):** before `AgentLoop::run_inner` grew the
//! `stop == StopReason::MaxTokens` check this file exercises, a turn shaped
//! exactly like the incident's own (one `Thinking` block, `stop:
//! max_tokens`, no `Text`, no `ToolUse`) left behind no
//! `LogRecord::SystemNote` naming that at all --
//! `max_tokens_with_only_thinking_emits_the_silent_notice` fails against
//! the unmodified loop. Paired with a turn that hit the same cap WITH text
//! already emitted (asserting the OTHER wording --
//! `max_tokens_with_text_already_emitted_gets_the_truncated_wording`) and
//! an ordinary completed turn (asserting silence from the whole mechanism
//! -- `an_ordinary_completed_turn_gets_no_max_tokens_notice`), so the
//! notice cannot have been made to fire on every turn and still pass.
//!
//! Mirrors `runway_notes.rs`'s own harness (`Runtime` built entirely from
//! `conway-testkit` fakes) -- the identical shape as that file's own
//! `LogRecord::SystemNote`-reading acceptance tests, just filtered on this
//! item's own two `reason` strings instead of `"runway"`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conway_core::agent::{AgentKnobs, Budget, PermissionDecision};
use conway_core::capabilities::HeadroomPolicy;
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::{Backend, GenerateResponse, Router, SessionStore};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use futures::StreamExt;

// ---------------------------------------------------------------------
// Harness -- trimmed down from `runway_notes.rs`'s own `build_runtime`/
// `root_spec`/`session_of`/`wait_for_agent_finished`: no tools, no
// `keep_alive`, and default (128k) capabilities are all this file's three
// single-turn scenarios need.
// ---------------------------------------------------------------------

fn build_runtime(backend: Arc<ScriptedBackend>) -> (Arc<Runtime>, Arc<dyn SessionStore>) {
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
        plugins: Vec::new(),
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
            keep_alive: false,
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

/// Every `LogRecord::SystemNote` this session's own log holds whose
/// `reason` is exactly `reason` -- the durable half of this item's own
/// acceptance criteria (the live `Event::AgentProgress` twin is asserted
/// separately, in `max_tokens_with_only_thinking_also_emits_a_live_notice`
/// below).
async fn system_notes_by_reason(
    store: &dyn SessionStore,
    session: SessionId,
    reason: &str,
) -> Vec<String> {
    store
        .read(&session, SeqRange::full())
        .await
        .expect("read session records")
        .into_iter()
        .filter_map(|r| match r {
            LogRecord::SystemNote {
                text,
                reason: found,
                ..
            } if found == reason => Some(text),
            _ => None,
        })
        .collect()
}

fn silent_response() -> GenerateResponse {
    GenerateResponse {
        // Shaped exactly like the incident's own seq 14: one `Thinking`
        // block, nothing else.
        content: vec![ContentBlock::Thinking {
            text: "enumerating every claimable item and starting to plan \
                    which to claim first..."
                .to_string(),
            signature: None,
        }],
        tool_calls: vec![],
        stop: StopReason::MaxTokens,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 8_192,
            ..Default::default()
        },
    }
}

fn truncated_response() -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: "here is the start of an answer that never gets to finish".to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::MaxTokens,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 8_192,
            ..Default::default()
        },
    }
}

fn ordinary_response() -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: "all done".to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------
// P-15: a turn shaped exactly like the incident (one `Thinking` block,
// `stop: max_tokens`, no `Text`, no `ToolUse`) gets the "said nothing"
// notice naming the exhausted budget.
// ---------------------------------------------------------------------

#[tokio::test]
async fn max_tokens_with_only_thinking_emits_the_silent_notice() {
    let backend = Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        silent_response(),
    )]));
    let (runtime, store) = build_runtime(backend);
    let mut stream = runtime.subscribe();

    let agent_id = runtime.start_root(root_spec("go")).await.unwrap();
    let session = session_of(&runtime, agent_id);
    let _ = wait_for_agent_finished(&mut stream, agent_id).await;

    let notes = system_notes_by_reason(store.as_ref(), session, "max_tokens_silent").await;
    assert_eq!(
        notes.len(),
        1,
        "exactly one silent max_tokens notice, got: {notes:?}"
    );
    let note = &notes[0];
    assert!(
        note.contains("max_tokens") && note.contains("budget"),
        "notice must name the exhausted output budget: {note}"
    );
    assert!(
        note.to_lowercase().contains("no visible answer")
            || note.to_lowercase().contains("no text"),
        "notice must say plainly that no answer was produced: {note}"
    );
    assert!(
        note.to_lowercase().contains("reason") || note.to_lowercase().contains("preserved"),
        "notice must point at the reasoning already in the log: {note}"
    );
    assert!(
        note.to_lowercase().contains("retry")
            && note.to_lowercase().contains("raise")
            && note.to_lowercase().contains("narrower"),
        "notice must offer retry / raise the cap / ask narrower, deciding none of them: {note}"
    );

    // No truncated-wording note also fired for this turn -- the two
    // wordings are mutually exclusive per turn.
    let truncated = system_notes_by_reason(store.as_ref(), session, "max_tokens_truncated").await;
    assert!(
        truncated.is_empty(),
        "a silent turn must not also get the mid-answer wording: {truncated:?}"
    );
}

// ---------------------------------------------------------------------
// Pair (a): the SAME cap, but text was already emitted before it hit --
// the "cut off mid-answer" wording, not the "said nothing" one.
// ---------------------------------------------------------------------

#[tokio::test]
async fn max_tokens_with_text_already_emitted_gets_the_truncated_wording() {
    let backend = Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        truncated_response(),
    )]));
    let (runtime, store) = build_runtime(backend);
    let mut stream = runtime.subscribe();

    let agent_id = runtime.start_root(root_spec("go")).await.unwrap();
    let session = session_of(&runtime, agent_id);
    let _ = wait_for_agent_finished(&mut stream, agent_id).await;

    let notes = system_notes_by_reason(store.as_ref(), session, "max_tokens_truncated").await;
    assert_eq!(
        notes.len(),
        1,
        "exactly one truncated max_tokens notice, got: {notes:?}"
    );
    let note = &notes[0];
    assert!(
        note.contains("max_tokens") && note.to_lowercase().contains("mid-answer"),
        "notice must name the mid-answer truncation: {note}"
    );

    // The silent wording never fires for a turn that had text.
    let silent = system_notes_by_reason(store.as_ref(), session, "max_tokens_silent").await;
    assert!(
        silent.is_empty(),
        "a turn with text already emitted must not get the silent wording: {silent:?}"
    );
}

// ---------------------------------------------------------------------
// Pair (b): an ordinary completed turn (`stop: end_turn`) gets neither
// wording -- proves the notice cannot be firing on every turn.
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_ordinary_completed_turn_gets_no_max_tokens_notice() {
    let backend = Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        ordinary_response(),
    )]));
    let (runtime, store) = build_runtime(backend);
    let mut stream = runtime.subscribe();

    let agent_id = runtime.start_root(root_spec("go")).await.unwrap();
    let session = session_of(&runtime, agent_id);
    let result = wait_for_agent_finished(&mut stream, agent_id).await;
    assert!(
        matches!(result.status, conway_core::agent::ResultStatus::Completed),
        "sanity: the scripted turn must complete normally, got {:?}",
        result.status
    );

    let silent = system_notes_by_reason(store.as_ref(), session, "max_tokens_silent").await;
    let truncated = system_notes_by_reason(store.as_ref(), session, "max_tokens_truncated").await;
    assert!(
        silent.is_empty() && truncated.is_empty(),
        "an ordinary completed turn must get neither max_tokens notice: \
         silent={silent:?} truncated={truncated:?}"
    );
}

// ---------------------------------------------------------------------
// The live twin: `Event::AgentProgress` fires alongside the persisted
// `SystemNote`, so a live TUI subscriber sees the notice the instant the
// turn ends -- not only on the next resume's backfill (this item's whole
// point: "the operator never has to ask").
// ---------------------------------------------------------------------

#[tokio::test]
async fn max_tokens_with_only_thinking_also_emits_a_live_notice() {
    let backend = Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        silent_response(),
    )]));
    let (runtime, _store) = build_runtime(backend);
    let mut stream = runtime.subscribe();

    let agent_id = runtime.start_root(root_spec("go")).await.unwrap();

    let saw_live_notice = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent != agent_id {
                continue;
            }
            if let Event::AgentProgress { note } = &envelope.event {
                if note.contains("max_tokens") {
                    return true;
                }
            }
            if matches!(envelope.event, Event::AgentFinished { .. }) {
                return false;
            }
        }
    })
    .await
    .expect("event stream never reached a decision");

    assert!(
        saw_live_notice,
        "expected a live Event::AgentProgress naming max_tokens before AgentFinished"
    );
}
