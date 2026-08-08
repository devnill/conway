//! Acceptance tests for `SessionHandle::ask` (the `/ask` fork-ask slice,
//! slice A; re-attached by board item B2): forks the caller's agent at its
//! current head into an ephemeral, catalog-hidden child -- attached as a
//! proper fork child of the asker, `AgentSpawned { kind: Fork,
//! parent: Some(asker), ephemeral: true, inherited_upto: Some(_) }` emitted
//! -- then drives that child's first turn with the question.
//!
//! Properties, each its own test below (not folded into one, so a
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
//! - `ask_child_attaches_as_ephemeral_fork_child_of_the_asker` (B2) -- the
//!   child attaches under the asker (not as a root): `AgentSpawned` on the
//!   live stream carries `kind: Fork`, `parent: Some(asker)`,
//!   `ephemeral: true`, `inherited_upto: Some(<fork-point seq>)`, and the
//!   `tree()` snapshot shows the child node under the asker (P-2:
//!   ephemeral children stay attached and visible to provenance).
//! - `ask_child_emits_agent_finished_with_ephemeral_true` -- the child's
//!   terminal event is stamped ephemeral too.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    PluginsConfig,
    RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
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
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
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
    // Post-B2 the question lands as the child's `ForkDirective` head record
    // (the runtime's own fork-attach path), not the `UserTurn` the old
    // `fork_child` -> `resume_root` -> `prompt` sequence appended.
    let saw_ask_question = child_transcript.iter().any(|record| match record {
        LogRecord::ForkDirective { text, .. } => {
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
// Board item 01KZGX1RR0VXN2YH3P75SBE9SA: a def-declared `result_contract`
// must never govern an ask child.
// ---------------------------------------------------------------------

/// A def whose `tools` selector omits `report` (so the ask child can never
/// satisfy any contract -- `structured` is populated ONLY by a successful
/// `report` call, `conway-runtime`'s `result.rs`) AND declares a
/// `result_contract` of its own. `SessionHandle::ask` already inherits this
/// def (`parent_meta.agent_def.map(AgentDefRef)`); before the fix,
/// `conway-runtime`'s `subagent.rs::start` also sourced `result_contract`
/// from this SAME def as a fallback, so the ask child had a contract it
/// structurally could not satisfy -- one retry, then `Rejected`.
fn write_contract_asker_agent_def(dir: &std::path::Path) {
    let content = concat!(
        "---\n",
        "name: contract_asker\n",
        "tools: [marker]\n",
        "result_contract:\n",
        "  type: object\n",
        "  required: [ok]\n",
        "  properties:\n",
        "    ok: { type: boolean }\n",
        "---\n",
        "Contract asker system prompt.\n",
    );
    std::fs::write(dir.join("contract_asker.md"), content).expect("write agent def fixture");
}

/// **Part 1 guard (board item 01KZGX1RR0VXN2YH3P75SBE9SA), shown to fail
/// before the fix.** The asker's own def declares a `result_contract` the
/// child cannot possibly satisfy (its `tools` selector omits `report`, so
/// `structured` always resolves to `null`). Drives the exact production
/// path the TUI's modal `/ask` uses (`SessionHandle::ask`, which already
/// inherits `agent_def` -- see that method's own doc). Before this item's
/// fix, `subagent.rs::start` sourced `result_contract` from the SAME
/// inherited def as a second, lower-precedence source (WI-086's original
/// precedence chain, meant for an ordinary `conway_fork`/`conway_spawn`
/// child that CAN call `report`), so this ask child failed validation on
/// its first turn, spent its one corrective retry (consuming a SECOND
/// scripted backend turn), and terminated `Rejected` -- never `Completed`.
/// `AskOutcome` (the `conway_ask` tool path) and `TurnHandle::result`'s
/// `AgentResult` (this facade path) are the two things a caller can
/// actually observe; `AskOutcome` in particular carries no `structured`
/// field at all, so a contract governing an ask child could only ever
/// break a good answer, never validate one.
#[tokio::test]
async fn ask_child_completes_with_prose_despite_a_def_declared_result_contract_it_cannot_satisfy()
{
    let agents_dir = support::unique_temp_dir("ask-contract-carveout");
    write_contract_asker_agent_def(&agents_dir);

    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("parent ack")),
            // The child's one (and, post-fix, ONLY) turn: plain prose, no
            // `report` call. A second scripted turn is queued too, so a
            // pre-fix run (contract enforced -> corrective retry) has a
            // response to consume instead of exhausting the script and
            // masking the real failure behind an unrelated backend error.
            ScriptedTurn::Respond(text_response("ask prose answer")),
            ScriptedTurn::Respond(text_response("ask prose answer, retry turn")),
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
        .build()
        .expect("build should succeed");

    let handle = conway
        .new_session(SessionSpec {
            agent_def: Some("contract_asker".to_string()),
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
        .ask("what's the answer?")
        .await
        .expect("ask should succeed");
    let result = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed (the outer Result -- distinct from AgentResult::status, asserted below)");

    assert_eq!(
        result.status,
        conway_core::agent::ResultStatus::Completed,
        "an ask child must complete with prose -- a def-declared result_contract must never \
         govern it, since AskOutcome/TurnHandle expose no `structured` field a contract could \
         ever satisfy. Got: {:?}",
        result.status
    );
    assert_eq!(
        result.summary, "ask prose answer",
        "the child's prose reply, unmolested by contract validation"
    );
}

// ---------------------------------------------------------------------
// Ephemeral flag on the live event stream (board item b)
// ---------------------------------------------------------------------

/// The facade `/ask` child is born with `SessionMeta::ephemeral = true`
/// (post-B2: via `SubagentSpec::ephemeral` -> `SubagentHost::start`, which
/// threads it into the forked header and the attached `AgentNode`
/// verbatim). The live `Event::AgentFinished` for that child must therefore
/// carry `ephemeral: true` (stamped by `agent_loop.rs`/`supervisor.rs` via
/// `AgentTree::ephemeral_of`). The matching `Event::AgentSpawned`
/// assertions live in `ask_child_attaches_as_ephemeral_fork_child_of_the_asker`
/// below; the runtime-level stamping itself is covered by
/// `crates/conway-runtime/tests/ephemeral_events.rs`.
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

// ---------------------------------------------------------------------
// B2: the ask child attaches as a proper ephemeral fork child of the asker
// ---------------------------------------------------------------------

/// Board item B2's acceptance shape: `SessionHandle::ask` must attach its
/// ephemeral child as a FORK CHILD of the asker -- not (as the pre-B2
/// `fork_child` -> `resume_root` path did) as a `kind: None` root with no
/// `AgentSpawned` event at all. Two observation points, both asserted here:
///
/// 1. The live stream carries `Event::AgentSpawned { kind: Fork,
///    parent: Some(asker), ephemeral: true, inherited_upto: Some(fork
///    point) }` for the child -- this is what the post-A1 TUI tree view
///    renders the node from.
/// 2. `SessionHandle::tree()`'s snapshot keeps the child attached UNDER
///    the asker (P-2: runtime provenance keeps ephemeral children attached
///    -- never-attach was REJECTED, decision
///    01KYFS1W7CJ1HW7N30H56B1VZZ) with `mode: Some(Fork)` and
///    `ephemeral: true` projected.
#[tokio::test]
async fn ask_child_attaches_as_ephemeral_fork_child_of_the_asker() {
    use conway_core::agent::SubagentMode;
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

    // The fork point: the parent's head at the moment of `ask` -- what
    // `AgentSpawned::inherited_upto` must name.
    let fork_point = store
        .head(&handle.id())
        .await
        .expect("head should succeed");

    // Subscribe BEFORE `ask` so the child's `AgentSpawned` cannot race past
    // the subscriber. `handle.events()` is session-scoped to the parent, but
    // `EventStream::accept` bypasses the session filter for lifecycle events
    // (`AgentSpawned`/`AgentFinished`) -- see `event_stream.rs`.
    let mut events = handle.events();

    let ask_turn = handle
        .ask("attach me properly")
        .await
        .expect("ask should succeed");

    // 1. The live `AgentSpawned` for the child, with the full B2 field set.
    let (child_agent, spawned) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope =
                std::future::poll_fn(|cx| std::pin::Pin::new(&mut events).poll_next(cx))
                    .await
                    .expect("event stream open");
            if let Event::AgentSpawned {
                kind,
                parent,
                ephemeral,
                inherited_upto,
                ..
            } = envelope.event
            {
                // The only fork child spawned in this test is the ask
                // child; the root itself never emits `AgentSpawned`
                // (`kind: None` roots are re-started, not spawned).
                return (
                    envelope.agent,
                    (kind, parent, ephemeral, inherited_upto),
                );
            }
        }
    })
    .await
    .expect("timed out waiting for the ask child's AgentSpawned");

    assert_ne!(
        child_agent,
        handle.root(),
        "the AgentSpawned must be the child's, not the asker's"
    );
    assert_eq!(
        spawned.0,
        SubagentMode::Fork,
        "the ask child must attach as kind: Fork (P-1: ask is the fork primitive, not a new one)"
    );
    assert_eq!(
        spawned.1,
        Some(handle.root()),
        "the ask child's parent must be the asking agent"
    );
    assert!(
        spawned.2,
        "the ask child's AgentSpawned must carry ephemeral: true"
    );
    assert_eq!(
        spawned.3,
        Some(fork_point),
        "inherited_upto must name the fork point (the parent's head at ask time)"
    );

    // 2. The tree snapshot keeps the child attached under the asker.
    let tree = handle.tree();
    let node = tree
        .nodes
        .iter()
        .find(|n| n.agent_id == child_agent)
        .expect("the ephemeral ask child must stay attached in the tree snapshot (P-2)");
    assert_eq!(
        node.parent,
        Some(handle.root()),
        "the tree node must hang under the asker, not off the root set"
    );
    assert_eq!(
        node.mode,
        Some(SubagentMode::Fork),
        "the tree node must record the fork mode"
    );
    assert!(
        node.ephemeral,
        "the tree node must project ephemeral: true"
    );

    // Drive the child's turn to completion so the test leaves no live task
    // running past its assertions (and proves the returned TurnHandle still
    // resolves under the new attach path).
    let _ = tokio::time::timeout(Duration::from_secs(5), ask_turn.result())
        .await
        .expect("ask result must not hang")
        .expect("ask result should succeed");
}
