//! Acceptance tests for `SessionHandle::ask` (the `/ask` fork-ask slice,
//! slice A): forks the caller's agent at its current head into an
//! ephemeral, catalog-hidden child, then drives that child's first turn
//! with the question.
//!
//! Four properties, each its own test below (not folded into one, so a
//! regression in any single one fails loudly and specifically):
//! - `ask_child_is_hidden_from_default_listing_but_visible_with_include_ephemeral`
//!   -- catalog hiding, both via `Conway::sessions`/`SessionFilter` and via
//!   `SessionStore::children`.
//! - `ask_never_appends_to_the_parent_and_does_not_leak_into_a_resumed_continuation`
//!   -- parent isolation: the parent's own head is unchanged across `ask`,
//!   and a subsequent real `prompt` (via `Conway::resume`, mirroring
//!   `resume.rs`'s own restart-simulation idiom -- a root's live task runs
//!   exactly one prompt-to-completion cycle, so continuing it for real
//!   always goes through resume) never sees the ask's question text.
//! - `ask_child_inherits_the_parents_prior_turn_text` -- inheritance: the
//!   child's own backend request carries the parent's prior turn text.
//! - `ask_child_can_invoke_a_tool_the_parent_session_had` -- tool
//!   inheritance: a tool restricted via a named `agent_def` the parent used
//!   is still invocable by the child.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig,
};
use conway::{Conway, ConwayBuilder, Plugin, SessionSpec, Tool};
use conway_core::agent::PermissionDecision;
use conway_core::content::{
    ContentBlock, PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy,
};
use conway_core::error::ToolError;
use conway_core::fakes::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, SeqRange, ToolName};
use conway_core::log::{LogRecord, SessionFilter};
use conway_core::ports::{
    Backend, GenerateResponse, PluginManifest, SessionStore, ToolCtx, ToolOutput,
};

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
        stop: conway_core::content::StopReason::EndTurn,
        usage: conway_core::content::Usage::default(),
    }
}

fn tool_call_response(call_id: &str, tool: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: serde_json::json!({}),
        }],
        stop: conway_core::content::StopReason::ToolUse,
        usage: conway_core::content::Usage::default(),
    }
}

/// The concatenated text of every `ContentBlock::Text` in `req`'s segments
/// -- mirrors `resume.rs`'s own private helper of the same name (each
/// integration test binary is a separate crate, so this is not shared).
fn request_text(req: &conway_core::ports::GenerateRequest) -> String {
    req.segments
        .iter()
        .flat_map(|seg| seg.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
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
    }
}

fn build_conway_with_backend(store: Arc<dyn SessionStore>, backend: Arc<dyn Backend>) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected")
}

// ---------------------------------------------------------------------
// Catalog hiding
// ---------------------------------------------------------------------

#[tokio::test]
async fn ask_child_is_hidden_from_default_listing_but_visible_with_include_ephemeral() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    let ask_turn = handle
        .ask("what is 2+2?")
        .await
        .expect("ask should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let default_listing = conway
        .sessions(SessionFilter::default())
        .await
        .expect("sessions() should succeed");
    assert_eq!(
        default_listing.len(),
        1,
        "the ephemeral ask child must stay out of the default (exclude-ephemeral) listing"
    );
    assert_eq!(default_listing[0].id, handle.id());

    let with_ephemeral = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() with include_ephemeral should succeed");
    assert_eq!(
        with_ephemeral.len(),
        2,
        "include_ephemeral: true must surface the ask child alongside the parent"
    );
    let child_meta = with_ephemeral
        .iter()
        .find(|m| m.id != handle.id())
        .expect("the ask child must be present when ephemeral sessions are included");
    assert!(
        child_meta.ephemeral,
        "the ask child's own header must be marked ephemeral"
    );
    assert_eq!(
        child_meta.origin.as_ref().map(|o| o.parent),
        Some(handle.id()),
        "the ask child's origin must name the parent session"
    );

    let children = store
        .children(&handle.id())
        .await
        .expect("children() should succeed");
    assert!(
        children.is_empty(),
        "SessionStore::children must also hide the ephemeral ask child, got: {children:?}"
    );
}

// ---------------------------------------------------------------------
// Parent isolation
// ---------------------------------------------------------------------

/// A root agent's live task runs exactly one prompt-to-completion cycle
/// before `run_inner` returns for good (`conway-runtime`'s `agent_loop.rs`:
/// a text-only completion is a `return`, not a loop-back) -- so a "real
/// subsequent prompt" on the same session is exercised the same way every
/// other continuation test in this crate does it
/// (`resume.rs`'s `resumed_handle_prompt_succeeds_and_continues_the_
/// transcript`): drop the live handle/`Conway` (simulating a process
/// restart), keeping only the persisted `store`, then `Conway::resume` and
/// prompt again. This is what proves the property against the *durable*
/// transcript, not just an in-memory one.
#[tokio::test]
async fn ask_never_appends_to_the_parent_and_does_not_leak_into_a_resumed_continuation() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let sid;
    let head_before;
    let head_after_ask;
    {
        let backend = Arc::new(
            ScriptedBackend::new(vec![
                ScriptedTurn::Respond(text_response("parent ack")),
                ScriptedTurn::Respond(text_response("ask answer")),
            ])
            .with_id(BackendId::new("fake")),
        );
        let conway = build_conway_with_backend(store.clone(), backend);

        let handle = conway
            .new_session(SessionSpec::default())
            .await
            .expect("new_session should succeed");
        let turn = handle.prompt("parent turn one").await.expect("prompt");
        let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
            .await
            .expect("result must not hang")
            .expect("result should succeed");

        head_before = store.head(&handle.id()).await.expect("head should succeed");
        sid = handle.id();

        let ask_turn = handle
            .ask("SUPER_SECRET_ASK_QUESTION_TOKEN")
            .await
            .expect("ask should succeed");
        let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
            .await
            .expect("ask result must not hang")
            .expect("ask result should succeed");

        head_after_ask = store.head(&handle.id()).await.expect("head should succeed");
        // `conway`/`handle` drop here -- only `store` survives, simulating a
        // process restart against the same persisted store.
    }

    assert_eq!(
        head_before, head_after_ask,
        "ask must not append anything to the parent's own log"
    );

    let backend2 = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            "parent continues",
        ))])
        .with_id(BackendId::new("fake")),
    );
    let conway2 = build_conway_with_backend(store.clone(), backend2.clone());
    let resumed = conway2
        .resume(sid)
        .await
        .expect("resume should succeed after the simulated restart");

    let turn2 = resumed
        .prompt("parent turn two")
        .await
        .expect("prompt on the resumed handle should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn2.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    let calls = backend2.calls();
    assert_eq!(
        calls.len(),
        1,
        "resumed continuation should make exactly one backend call, calls: {calls:?}"
    );
    let text = request_text(&calls[0]);
    assert!(
        !text.contains("SUPER_SECRET_ASK_QUESTION_TOKEN"),
        "the resumed parent's effective transcript must never contain the ask's question text, \
         got: {text}"
    );
}

// ---------------------------------------------------------------------
// `resolve_agent_session(include_ephemeral: true)` load-bearing check
// ---------------------------------------------------------------------

/// `SessionHandle::resolve_agent_session` (private, session_handle.rs)
/// passes `include_ephemeral: true` so an ephemeral child stays resolvable
/// by agent id through a handle whose root is a DIFFERENT agent -- the
/// parent's own handle, not the child's. This test drives exactly that
/// path via `handle.transcript(child_agent)`: `handle.root()` is the
/// parent's root agent, `child_agent` belongs to the ask child (ephemeral),
/// so `resolve_agent_session` must fall through its `agent == self.root`
/// fast path and hit the `store.list` lookup below it. Without
/// `include_ephemeral: true` there, that lookup would miss the child
/// entirely (it is ephemeral) and this call would fail with
/// `RuntimeError::AgentNotFound`.
#[tokio::test]
async fn transcript_resolves_the_ephemeral_ask_child_by_agent_id_via_the_parents_handle() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    let ask_turn = handle
        .ask("SENTINEL_ASK_QUESTION_FOR_TRANSCRIPT_LOOKUP")
        .await
        .expect("ask should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let with_ephemeral = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() with include_ephemeral should succeed");
    let child_meta = with_ephemeral
        .iter()
        .find(|m| m.id != handle.id())
        .expect("the ask child must be present when ephemeral sessions are included");
    assert_ne!(
        child_meta.agent_id,
        handle.root(),
        "the child agent must differ from the parent handle's own root -- otherwise this test \
         would trivially hit `resolve_agent_session`'s `agent == self.root` fast path instead of \
         the ephemeral-inclusive lookup it is meant to exercise"
    );

    let child_transcript = handle.transcript(child_meta.agent_id).await.expect(
        "transcript(child_agent) must resolve the ephemeral child by agent id through \
             `handle` -- a SessionHandle whose own root is the PARENT, not the child",
    );
    let saw_ask_question = child_transcript.iter().any(|record| match record {
        LogRecord::UserTurn { text, .. } => {
            text.contains("SENTINEL_ASK_QUESTION_FOR_TRANSCRIPT_LOOKUP")
        }
        _ => false,
    });
    assert!(
        saw_ask_question,
        "resolved transcript must be the child's own (containing the ask question), \
         got: {child_transcript:?}"
    );
}

// ---------------------------------------------------------------------
// Inheritance
// ---------------------------------------------------------------------

#[tokio::test]
async fn ask_child_inherits_the_parents_prior_turn_text() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend.clone());

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle
        .prompt("DISTINCTIVE_PARENT_PHRASE_77821")
        .await
        .expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    let ask_turn = handle
        .ask("what about that?")
        .await
        .expect("ask should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let calls = backend.calls();
    let child_call = calls
        .last()
        .expect("the child turn should have called the backend");
    let text = request_text(child_call);
    assert!(
        text.contains("DISTINCTIVE_PARENT_PHRASE_77821"),
        "the ask child must inherit the parent's prior turn text, got: {text}"
    );
}

// ---------------------------------------------------------------------
// Tool inheritance
// ---------------------------------------------------------------------

fn schema_any_object() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
}

/// A trivial tool that always succeeds -- only its invocability (not its
/// output) matters for this test.
struct MarkerTool;

#[async_trait]
impl Tool for MarkerTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("marker"),
            description: "test-only marker tool".into(),
            schema: schema_any_object(),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "marked".into(),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

struct MarkerPlugin;

impl Plugin for MarkerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test.marker".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![ToolName::new("marker")],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(MarkerTool)]
    }
}

/// Writes a minimal agent-def fixture (matching `agent_defs.rs`'s own
/// front-matter format) restricting tools to exactly `marker`, so that a
/// successful invocation through the ask child proves genuine tool-set
/// inheritance rather than both sessions merely defaulting to "all tools".
fn write_asker_agent_def(dir: &std::path::Path) {
    std::fs::write(
        dir.join("asker.md"),
        "---\nname: asker\ntools: [marker]\n---\nAsker system prompt.\n",
    )
    .expect("write agent def fixture");
}

#[tokio::test]
async fn ask_child_can_invoke_a_tool_the_parent_session_had() {
    let agents_dir = support::unique_temp_dir("ask-tool-inherit");
    write_asker_agent_def(&agents_dir);

    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(tool_call_response("call_1", "marker")),
            ScriptedTurn::Respond(text_response("ask done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let mut config = base_config();
    config.agents = AgentsConfig { dir: agents_dir };
    let conway = ConwayBuilder::from_parts(config)
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store.clone())
        .with_permission_gate(gate)
        .with_router(fake_router())
        .with_plugin(Arc::new(MarkerPlugin))
        .build()
        .expect("build should succeed");

    let handle = conway
        .new_session(SessionSpec {
            agent_def: Some("asker".to_string()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    let ask_turn = handle
        .ask("please use the marker tool")
        .await
        .expect("ask should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let with_ephemeral = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() should succeed");
    let child_id = with_ephemeral
        .iter()
        .find(|m| m.id != handle.id())
        .expect("the ask child must be present")
        .id;

    let records = store
        .read(&child_id, SeqRange::full())
        .await
        .expect("read should succeed");
    let tool_result = records.iter().find_map(|r| match r {
        LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "marker" => {
            Some(result)
        }
        _ => None,
    });
    let tool_result = tool_result.expect(
        "the ask child must have actually invoked the 'marker' tool it inherited from the parent",
    );
    assert!(
        !tool_result.is_error,
        "the inherited 'marker' tool call must succeed, not error"
    );
}

// ---------------------------------------------------------------------
// Ephemeral flag on the live event stream (board item b)
// ---------------------------------------------------------------------

/// The facade `/ask` child is born with `SessionMeta::ephemeral = true`
/// (`fork_child`), which `resume_root` stamps into `AgentNode::ephemeral`.
/// The live `Event::AgentFinished` for that child must therefore carry
/// `ephemeral: true` (stamped by `agent_loop.rs`/`supervisor.rs` via
/// `AgentTree::ephemeral_of`).
///
/// Disclosure (matches the spec's design, NOT a gap): `resume_root` attaches
/// the child with `kind: None` (a resumed root is re-started, not spawned --
/// see `tree.rs`'s module doc), so `AgentTree::attach` does NOT emit an
/// `Event::AgentSpawned` for the facade `/ask` path. Only `AgentFinished` is
/// observable on the live stream; the `ephemeral: true` flag still reaches
/// it via `AgentNode::ephemeral` -> `ephemeral_of`. The runtime-level
/// `AgentSpawned { ephemeral: true }` stamping is covered by
/// `crates/conway-runtime/tests/ephemeral_events.rs` (direct `AgentTree::attach`
/// with `kind: Some(Fork)`).
#[tokio::test]
async fn ask_child_emits_agent_finished_with_ephemeral_true() {
    use conway_core::event::Event;
    use futures_core::Stream as _;

    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            ScriptedTurn::Respond(text_response("ask answer")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway_with_backend(store.clone(), backend);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("parent turn").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("parent result must not hang")
        .expect("parent result should succeed");

    // Subscribe BEFORE `ask` so the child's finish cannot race past the
    // subscriber. `handle.events()` is session-scoped to the parent, but
    // `EventStream::accept` bypasses the session filter for lifecycle events
    // (`AgentSpawned`/`AgentFinished`) -- see `event_stream.rs` -- so the
    // child's `AgentFinished` reaches this stream.
    let mut events = handle.events();

    let ask_turn = handle
        .ask("(ephemeral) checking")
        .await
        .expect("ask should succeed");
    // Drive the child's turn to completion so its `AgentFinished` is emitted.
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");

    let child_id = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("sessions() should succeed")
        .into_iter()
        .find(|m| m.id != handle.id())
        .expect("the ask child must be present")
        .agent_id;

    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope =
                std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                    .await
                    .expect("event stream open");
            if let Event::AgentFinished { ephemeral, .. } = envelope.event {
                if envelope.agent == child_id {
                    return ephemeral;
                }
            }
        }
    })
    .await
    .expect("timed out waiting for the ask child's AgentFinished");

    assert!(
        finished,
        "the facade /ask child's AgentFinished must carry ephemeral: true"
    );
}
