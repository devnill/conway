//! End-to-end acceptance for `conway.ui`'s `ask_question` tool (harness gap
//! review 2026-09-01, finding 11): a real, fully-faked `Conway` (no
//! network, no live provider) with `ConwayUiPlugin` attached exactly the
//! way a library embedder would (`ConwayBuilder::with_plugin`), driving a
//! model-issued `ask_question` call through a REAL turn (`ScriptedBackend`)
//! and reading the actual `LogRecord::ToolResultRecord` the model received
//! back -- rather than calling `Tool::invoke` directly the way this crate's
//! own `src/lib.rs` unit tests already do.
//!
//! Two shapes, mirroring the crate's own module doc's "the path is model ->
//! tool -> operator, never plugin -> plugin -> operator":
//!
//!   1. A surface IS wired in (`ConwayUiPlugin::new(Some(surface))`): the
//!      tool result carries the surface's chosen option and nothing else --
//!      `"operator selected: <selected>"`, verbatim.
//!   2. No surface is wired in (`ConwayUiPlugin::new(None)`, the
//!      construction every non-TUI dispatch target uses in production): the
//!      tool result carries the plugin's own typed degrade sentence, never
//!      a panic and never an empty success.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{async_trait, ContentBlock, SeqRange, ToolCall, ToolName};
use conway::test_support::test_builder;
use conway::{
    backend::{BackendId, GenerateResponse, StopReason, Usage},
    Conway, RoleAlias, SessionSpec, SessionStore,
};
use conway_plugin_ui::{
    AskSelectAnswer, AskSelectRequest, ConwayUiPlugin, FormSurface, FormSurfaceError,
    ASK_QUESTION_TOOL_NAME,
};
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

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

/// A real, fully-faked `Conway` with `ConwayUiPlugin` attached exactly the
/// way a library embedder would.
fn ui_conway(
    cwd: std::path::PathBuf,
    backend: Arc<ScriptedBackend>,
    store: Arc<FakeStore>,
    plugin: ConwayUiPlugin,
) -> Conway {
    test_builder(base_config(cwd))
        .with_backend(backend)
        .with_session_store(store)
        .with_plugin(Arc::new(plugin))
        .build()
        .expect("build should succeed with every port injected")
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

/// A local `FormSurface` fixture that always answers with a fixed choice --
/// mirrors this crate's own private `FixedAnswerSurface` in `src/lib.rs`
/// (not reachable from an integration test binary, which is a separate
/// compilation unit; a fresh, minimal copy here is the ordinary shape,
/// never a new `conway-testkit` fake -- C-04).
struct FixedAnswerSurface {
    answer: String,
}

#[async_trait]
impl FormSurface for FixedAnswerSurface {
    async fn ask_select(
        &self,
        _request: AskSelectRequest,
    ) -> Result<AskSelectAnswer, FormSurfaceError> {
        Ok(AskSelectAnswer {
            selected: self.answer.clone(),
        })
    }
}

fn ask_question_call(call_id: &str) -> GenerateResponse {
    tool_call_response(
        call_id,
        ASK_QUESTION_TOOL_NAME,
        serde_json::json!({
            "prompt": "which color?",
            "options": ["red", "green", "blue"],
        }),
    )
}

async fn tool_result_text(store: &FakeStore, session: &conway::SessionId, call_id: &str) -> String {
    let records = store
        .read(session, SeqRange::full())
        .await
        .expect("read back session log");
    let result = records
        .iter()
        .find_map(|r| match r {
            conway::LogRecord::ToolResultRecord { result, .. } if result.call_id == call_id => {
                Some(result)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no ToolResultRecord for call_id {call_id} in {records:#?}"));
    result
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A surface IS wired in: the tool result carries the surface's chosen
/// option and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wired_surface_answers_with_its_chosen_option_and_nothing_else() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(ask_question_call("tc1")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let plugin = ConwayUiPlugin::new(Some(Arc::new(FixedAnswerSurface {
        answer: "blue".to_string(),
    })));
    let conway = ui_conway(tmp.path().to_path_buf(), backend.clone(), store.clone(), plugin);

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = session.prompt("ask which color").await.expect("prompt");
    turn.result().await.expect("turn completes naturally");

    let text = tool_result_text(&store, &session.id(), "tc1").await;
    assert_eq!(
        text, "operator selected: blue",
        "the tool result must carry the surface's chosen option and nothing else"
    );
}

/// No surface is wired in: the tool result carries the plugin's own typed
/// degrade sentence -- never a panic, never an empty success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_surface_degrades_to_the_typed_no_surface_error_text() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(ask_question_call("tc1")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let plugin = ConwayUiPlugin::new(None);
    let conway = ui_conway(tmp.path().to_path_buf(), backend.clone(), store.clone(), plugin);

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = session.prompt("ask which color").await.expect("prompt");
    turn.result().await.expect("turn completes naturally");

    let text = tool_result_text(&store, &session.id(), "tc1").await;
    assert_eq!(
        text,
        "no answer available: no interactive surface is available in this host to ask the \
         operator",
        "no surface must degrade with the plugin's own typed sentence, never an empty success \
         or a generic message"
    );

    // Not a tool error either -- the plugin's own posture is to degrade the
    // reply, never fail the call, when nobody could answer it.
    let records = store
        .read(&session.id(), SeqRange::full())
        .await
        .expect("read back session log");
    let result = records
        .iter()
        .find_map(|r| match r {
            conway::LogRecord::ToolResultRecord { result, .. } if result.call_id == "tc1" => {
                Some(result)
            }
            _ => None,
        })
        .expect("tc1 must have a logged tool result");
    assert!(
        !result.is_error,
        "no interactive surface must degrade the reply, not fail the tool call"
    );
}
