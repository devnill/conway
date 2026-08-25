//! End-to-end acceptance for `conway.path` (board item
//! `01M0PEFMG96SVBBD5D2E06H34A`): a real, fully-faked `Conway` (no network,
//! no live provider) with a REAL, disk-backed `FsPathStore` under a tempdir
//! -- `SessionStore` is faked (`conway_testkit::FakeStore`), but the path
//! store is deliberately NOT faked, so `write_head`/`resolve_default_path`
//! run against the actual production implementation this tool composes
//! through, the same as a real `conway` binary would.
//!
//! Every test drives the `compose_context_path` tool through a REAL model
//! turn (`ScriptedBackend`), never `Tool::invoke` called directly -- proving
//! the tool is reachable exactly the way a model reaches it, and asserting
//! on the ACTUAL wire request a later turn sends, which is what proves a
//! composition "survives the next turn" (acceptance criterion 1) rather
//! than merely returning the right value from one call.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::backend::{BackendId, GenerateRequest, GenerateResponse, StopReason, Usage};
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{ContentBlock, Event, ToolCall, ToolName};
use conway::{Conway, EventStream, RoleAlias, SessionSpec};
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

use conway::test_support::test_builder;
use conway_plugin_path::{PathPlugin, COMPOSE_TOOL_NAME};

fn base_config(cwd: std::path::PathBuf) -> ConwayConfig {
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
        cwd,
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

fn tool_call_response(call_id: &str, tool: &str, args: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: args,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// A real, fully-faked `Conway` -- `PathPlugin` attached exactly the way a
/// library embedder would (`ConwayBuilder::with_plugin`), the same call
/// `crates/conway-cli/src/first_party_plugins.rs` makes internally once
/// `conway.path` is named in `[plugins].install`. `cwd` is a fresh tempdir
/// so the real `FsPathStore`/`JsonlSessionStore` (session store faked, path
/// store not) `ConwayBuilder::build` constructs by default never touches
/// the repository tree.
/// The `PathPlugin`-only case, which is most of this file.
fn path_conway(
    cwd: std::path::PathBuf,
    backend: Arc<ScriptedBackend>,
    store: Arc<FakeStore>,
) -> Conway {
    conway_with_plugins(cwd, backend, store, vec![Arc::new(PathPlugin)])
}

/// `conway::test_support::test_builder`'s wiring plus an arbitrary plugin
/// list -- the one axis this file varies.
fn conway_with_plugins(
    cwd: std::path::PathBuf,
    backend: Arc<ScriptedBackend>,
    store: Arc<FakeStore>,
    plugins: Vec<Arc<dyn conway::plugin::Plugin>>,
) -> Conway {
    let mut builder = test_builder(base_config(cwd))
        .with_backend(backend)
        .with_session_store(store);
    for plugin in plugins {
        builder = builder.with_plugin(plugin);
    }
    builder
        .build()
        .expect("build should succeed with every port injected")
}

/// A session prompted more than once needs `keep_alive: true` -- a live
/// session's agent task otherwise terminates after its first
/// prompt-to-completion turn (`SessionSpec::keep_alive`'s own doc), so a
/// second `prompt()` would silently never be picked up by any running task.
/// Every multi-turn session in this file uses this helper;
/// `SessionSpec::default()` (single turn) is used only for the one-shot
/// "mint an id" sessions.
fn multi_turn_spec() -> SessionSpec {
    SessionSpec {
        keep_alive: true,
        ..SessionSpec::default()
    }
}

/// Drains `stream` for exactly `steps` occurrences of `Event::TurnFinished`
/// -- NOT `TurnHandle::text()`/`::result()`. `Event::TurnFinished` fires once
/// per internal model generation (`conway_runtime::agent_loop`'s own module
/// doc: "a turn is one model generation"), so a user prompt whose scripted
/// response calls a tool needs TWO steps drained (the tool-call generation,
/// then the follow-up) before the harness is ready for the next prompt --
/// `TurnHandle::text()` alone stops at the first step, and `::result()`
/// resolves only on the session's ONE whole-session `AgentFinished` once
/// `keep_alive: true` (`crates/conway/tests/keep_alive.rs`'s own
/// `drain_n_turn_finished`, the identical fixture this mirrors). The caller
/// must know the exact step count up front, true of every call below since
/// each test's backend script is fully known.
async fn drain_n_turn_finished(stream: &mut EventStream, steps: usize) {
    use futures_core::Stream as _;
    let mut seen = 0usize;
    loop {
        let envelope = tokio::time::timeout(
            Duration::from_secs(5),
            std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)),
        )
        .await
        .expect("event stream must not hang")
        .expect("event stream must not end mid-session");
        if let Event::TurnFinished { .. } = envelope.event {
            seen += 1;
            if seen == steps {
                return;
            }
        }
    }
}

/// Prompts `session` and drains exactly `steps` internal generations before
/// returning -- see [`drain_n_turn_finished`]'s own doc for why a plain
/// `text()`/`result()` call is not enough for a keep-alive, tool-calling
/// turn.
async fn run_turn(
    session: &conway::SessionHandle,
    events: &mut EventStream,
    prompt: &str,
    steps: usize,
) {
    session.prompt(prompt).await.expect("prompt");
    drain_n_turn_finished(events, steps).await;
}

fn all_text(req: &GenerateRequest) -> String {
    let mut out = String::new();
    for segment in &req.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
                out.push('\n');
            }
        }
    }
    out
}

#[test]
fn manifest_id_matches_the_published_constant() {
    use conway::plugin::Plugin as _;
    assert_eq!(PathPlugin.manifest().id, conway_plugin_path::PLUGIN_ID);
}

/// Acceptance 1: composing a cross-session `include` survives the NEXT
/// turn, and (the `covers_upto` trap's default resolution) this session's
/// own earlier content is STILL present -- proven on the wire request a
/// later, independently-scripted turn actually sends, not on the tool's own
/// return value alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn included_foreign_record_and_own_tail_both_survive_the_next_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());

    // First pass: mint session B's id (a compose call needs a real id to
    // name, and `ScriptedBackend`'s script is fixed at construction time,
    // so there is no way to know it before this session exists).
    let mint_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("b replies"))])
            .with_id(BackendId::new("fake")),
    );
    let mint_conway = path_conway(tmp.path().to_path_buf(), mint_backend, store.clone());
    let session_b = mint_conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session b");
    let mut b_events = session_b.events();
    run_turn(&session_b, &mut b_events, "unique-marker-from-session-b", 1).await;
    let b_id = session_b.id();

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("a's own first reply")),
            ScriptedTurn::Respond(tool_call_response(
                "tc_compose",
                COMPOSE_TOOL_NAME,
                serde_json::json!({
                    "include": [{"session": b_id.to_string(), "seq": 0}],
                }),
            )),
            ScriptedTurn::Respond(text_response("composed")),
            ScriptedTurn::Respond(text_response("proof turn reply")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = path_conway(tmp.path().to_path_buf(), backend.clone(), store.clone());

    let session_a = conway
        .new_session(multi_turn_spec())
        .await
        .expect("new_session a");
    let mut a_events = session_a.events();
    run_turn(&session_a, &mut a_events, "a's own first prompt", 1).await;
    run_turn(
        &session_a,
        &mut a_events,
        "please bring in what session b said",
        2, // tool-call generation, then the follow-up "composed"
    )
    .await;
    run_turn(&session_a, &mut a_events, "proof turn", 1).await;

    let calls = backend.calls();
    let proof_request = calls.last().expect("at least one call recorded");
    let text = all_text(proof_request);
    assert!(
        text.contains("unique-marker-from-session-b"),
        "the composed foreign record must be in the proof turn's context: {text}"
    );
    assert!(
        text.contains("a's own first prompt"),
        "the own tail must survive by default (no drop_own_tail): {text}"
    );
}

/// The `covers_upto` trap, pinned as a CHOICE: `drop_own_tail: true`
/// deliberately drops this session's own earlier turns from the next
/// composition -- proven the same way, on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_own_tail_is_a_deliberate_reset_not_an_accident() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());

    // First pass: just to mint session B's id.
    let mint_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("b replies"))])
            .with_id(BackendId::new("fake")),
    );
    let mint_conway = path_conway(tmp.path().to_path_buf(), mint_backend, store.clone());
    let session_b = mint_conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session b");
    let mut b_events = session_b.events();
    run_turn(&session_b, &mut b_events, "session-b-content", 1).await;
    let b_id = session_b.id();

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response(
                "session-a-own-content-that-should-be-dropped",
            )),
            ScriptedTurn::Respond(tool_call_response(
                "tc_compose",
                COMPOSE_TOOL_NAME,
                serde_json::json!({
                    "include": [{"session": b_id.to_string(), "seq": 0}],
                    "drop_own_tail": true,
                }),
            )),
            ScriptedTurn::Respond(text_response("composed")),
            ScriptedTurn::Respond(text_response("proof turn reply")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = path_conway(tmp.path().to_path_buf(), backend.clone(), store.clone());
    let session_a = conway
        .new_session(multi_turn_spec())
        .await
        .expect("new_session a");
    let mut a_events = session_a.events();
    run_turn(
        &session_a,
        &mut a_events,
        "own-prompt-that-should-be-dropped",
        1,
    )
    .await;
    run_turn(
        &session_a,
        &mut a_events,
        "reset to only session b, please",
        2,
    )
    .await;
    run_turn(&session_a, &mut a_events, "proof turn", 1).await;

    let calls = backend.calls();
    let proof_request = calls.last().expect("at least one call recorded");
    let text = all_text(proof_request);
    assert!(
        text.contains("session-b-content"),
        "the foreign include must still be present: {text}"
    );
    assert!(
        !text.contains("own-prompt-that-should-be-dropped"),
        "drop_own_tail: true must actually drop this session's earlier own \
         content, not just claim to: {text}"
    );
}

/// Coherence: a `WouldOrphan` refusal is surfaced (`is_error: true`, the
/// orphan named) and persists NOTHING -- a later turn's context is
/// unaffected by the refused attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_would_orphan_composition_is_refused_and_persists_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());

    // Session A: turn 1 makes a real tool call (a probe tool this fixture
    // registers) so its own log has a ToolUse/ToolResultRecord pair to try
    // (and fail) to split.
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "tc_probe",
                "probe",
                serde_json::json!({}),
            )),
            ScriptedTurn::Respond(text_response("probed, done")),
            // The compose attempt: exclude the seq carrying the ToolUse
            // but not its result -- must be refused.
            ScriptedTurn::Respond(tool_call_response(
                "tc_compose",
                COMPOSE_TOOL_NAME,
                serde_json::json!({ "exclude": [1] }),
            )),
            ScriptedTurn::Respond(text_response("after refusal")),
            ScriptedTurn::Respond(text_response("proof turn reply")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = conway_with_plugins(
        tmp.path().to_path_buf(),
        backend.clone(),
        store.clone(),
        vec![Arc::new(PathPlugin), Arc::new(ProbePlugin)],
    );
    let session_a = conway
        .new_session(multi_turn_spec())
        .await
        .expect("new_session a");
    let mut a_events = session_a.events();
    run_turn(&session_a, &mut a_events, "call probe", 2).await;
    run_turn(
        &session_a,
        &mut a_events,
        "now try an incoherent exclude",
        2,
    )
    .await;
    run_turn(&session_a, &mut a_events, "proof turn", 1).await;

    let calls = backend.calls();
    let proof_request = calls.last().expect("at least one call recorded");
    let text = all_text(proof_request);
    assert!(
        text.contains("probed, done") || text.contains("call probe"),
        "a refused composition must leave the session's ordinary content \
         reachable exactly as before the attempt: {text}"
    );

    // The refusal itself: `derive_with` surfaced `PathError::WouldOrphan`
    // as an `is_error` tool result naming the orphan, never a silent
    // patch -- read straight off the session's own log (`SessionStore`'s
    // "immutable, append-only" contract means this record is exactly what
    // the model saw).
    use conway::plugin::SeqRange;
    use conway::SessionStore;
    let records = store
        .read(&session_a.id(), SeqRange::full())
        .await
        .expect("read session a's log");
    let compose_result = records
        .iter()
        .find_map(|r| match r {
            conway::LogRecord::ToolResultRecord { result, .. }
                if result.call_id == "tc_compose" =>
            {
                Some(result)
            }
            _ => None,
        })
        .expect("the compose call must have a logged tool result");
    assert!(
        compose_result.is_error,
        "an incoherent exclude must be refused (is_error), not silently patched"
    );
    let refusal_text: String = compose_result
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        refusal_text.contains("tc_probe") || refusal_text.to_lowercase().contains("orphan"),
        "the refusal must name the orphaned call, not just fail generically: {refusal_text}"
    );

    // Nothing was persisted: no `ContextPathSet` head was ever written to
    // session A's log, proving the refusal happened BEFORE `set_head`, not
    // after a partial write.
    let head_written = records
        .iter()
        .any(|r| matches!(r, conway::LogRecord::ContextPathSet { .. }));
    assert!(
        !head_written,
        "a refused composition must persist nothing, not even a partial head"
    );
}

/// A trivial always-succeeds tool, giving the refusal test a real
/// `ToolUse`/`ToolResultRecord` pair to try (and fail) to split.
struct ProbePlugin;

impl conway::plugin::Plugin for ProbePlugin {
    fn manifest(&self) -> conway::plugin::PluginManifest {
        conway::plugin::PluginManifest {
            id: "test.probe".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![ToolName::new("probe")],
            required_host_caps: vec![],
        }
    }
    fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
        vec![Arc::new(ProbeTool)]
    }
}

struct ProbeTool;

#[conway::plugin::async_trait]
impl conway::plugin::Tool for ProbeTool {
    fn spec(&self) -> conway::plugin::ToolSpec {
        conway::plugin::ToolSpec {
            name: ToolName::new("probe"),
            description: "test-only probe tool".into(),
            schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
            category: conway::plugin::ToolCategory::Read,
            permission: conway::plugin::PermissionClass::Safe,
        }
    }
    async fn invoke(
        &self,
        _call: conway::plugin::ToolCall,
        _ctx: conway::plugin::ToolCtx,
    ) -> Result<conway::plugin::ToolOutput, conway::plugin::ToolError> {
        Ok(conway::plugin::ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "probed".to_string(),
            }],
            is_error: false,
            truncation: conway::plugin::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// The anchor must survive an `exclude` list that also names it.
///
/// Regression for a defect found in review: the anchor that keeps
/// `covers_upto` off `LogSeq::ZERO` was originally chosen inside the
/// `drop_own_tail` branch alone, so an `exclude` list naming that same seq
/// omitted it anyway. Every own node then went, `covers_upto` fell to zero,
/// and zero means "own tail = my whole log, read live" — so the very next
/// own append resurrected everything the call had just dropped. That is the
/// exact failure board finding `01M0P50E04EY3BHQJHZX74HSSC` exists to
/// prevent, reintroduced by the fix for it.
///
/// This drives a real later turn and asserts on the wire request, because
/// the whole point is what the NEXT turn sends — an assertion on any
/// intermediate value would have passed against the broken version.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exclude_naming_the_anchor_cannot_resurrect_the_dropped_tail() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());

    let mint_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("b replies"))])
            .with_id(BackendId::new("fake")),
    );
    let mint_conway = path_conway(tmp.path().to_path_buf(), mint_backend, store.clone());
    let session_b = mint_conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session b");
    let mut b_events = session_b.events();
    run_turn(&session_b, &mut b_events, "session-b-content", 1).await;
    let b_id = session_b.id();

    // `drop_own_tail` AND an `exclude` enumerating a wide span of own seqs,
    // deliberately covering whichever seq the anchor would land on. A model
    // "making sure everything is gone" has an ordinary reason to do this.
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response(
                "session-a-own-content-that-should-be-dropped",
            )),
            ScriptedTurn::Respond(tool_call_response(
                "tc_compose",
                COMPOSE_TOOL_NAME,
                serde_json::json!({
                    "include": [{"session": b_id.to_string(), "seq": 0}],
                    "exclude": [0, 1, 2, 3, 4, 5, 6, 7, 8],
                    "drop_own_tail": true,
                }),
            )),
            ScriptedTurn::Respond(text_response("composed")),
            ScriptedTurn::Respond(text_response("proof turn reply")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = path_conway(tmp.path().to_path_buf(), backend.clone(), store.clone());
    let session_a = conway
        .new_session(multi_turn_spec())
        .await
        .expect("new_session a");
    let mut a_events = session_a.events();
    run_turn(
        &session_a,
        &mut a_events,
        "own-prompt-that-should-be-dropped",
        1,
    )
    .await;
    run_turn(&session_a, &mut a_events, "drop all of it please", 2).await;
    run_turn(&session_a, &mut a_events, "proof turn", 1).await;

    let calls = backend.calls();
    let proof_request = calls.last().expect("at least one call recorded");
    let text = all_text(proof_request);
    assert!(
        !text.contains("own-prompt-that-should-be-dropped"),
        "an `exclude` list that also names the anchor seq must NOT be able to \
         drop every own node and reset covers_upto to zero — doing so \
         resurrects the whole own log on the next turn, which is the exact \
         opposite of what the call asked for: {text}"
    );
    assert!(
        text.contains("session-b-content"),
        "the foreign include must still survive: {text}"
    );
}
