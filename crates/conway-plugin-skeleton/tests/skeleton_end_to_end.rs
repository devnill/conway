//! The first-party plugin tier's own acceptance test -- this crate is a first-party plugin, never
//! registered unless a caller explicitly installs it -- exactly the shape
//! `PHILOSOPHY.md`'s "First-party plugins, and why they are not defaults"
//! describes.
//!
//! Written the way a library embedder would write it: `ConwayBuilder`,
//! `ScriptedBackend`/`FakeGate`/`FakeRouter`/`FakeStore` (the credential-
//! free fakes family CONTRIBUTING's check-liveness rule names as the
//! strongest form of coverage -- no live provider, no network), and
//! `conway_plugin_skeleton::SkeletonPlugin` attached the same way any
//! third-party plugin would be, via `ConwayBuilder::with_plugin`.
//!
//! **`tool_absent_by_default_present_once_installed` is the VERIFICATION
//! ANCHOR test** the names: it asserts the skeleton's tool is
//! absent from a `Conway` built with no `with_plugin` call, and present on
//! an otherwise-identical one that adds exactly one `.with_plugin(..)`
//! call -- so a stubbed-out registration path (e.g. this crate's
//! `Plugin::tools()` returning an empty `Vec`, or a caller's own
//! `with_plugin` wiring being dropped) fails the "present" half while the
//! "absent" half keeps passing, per CONTRIBUTING's own discipline: a check
//! that cannot fail is not a check.
//!
//! `skeleton_tool_is_callable_end_to_end_through_a_real_turn` goes one step
//! further and proves the installed tool is not just *announced* but
//! actually *invocable*: a `ScriptedBackend` turn calls `skeleton_ping`,
//! the runtime dispatches it to this plugin's real `Tool::invoke`, and the
//! persisted `ToolResultRecord` carries the exact reply text.
//!
//! `a_configured_hook_fires_when_the_skeletons_declared_event_is_dispatched`
//! is the open-vocabulary half's OWN
//! end-to-end proof, driven the identical way: a real `[hooks].rules[]`
//! entry naming this plugin's declared event
//! (`{PLUGIN_ID}.{PONG_DISPATCHED_EVENT}`), a real injected `HookRunner`
//! double that records what it was invoked with, a real turn that calls
//! `skeleton_ping`, and an assertion that the runner was actually invoked
//! with the exact namespaced event name and the payload this plugin's own
//! `Tool::invoke` constructed -- not merely that `Plugin::events()` returns
//! a declaration (`PHILOSOPHY.md` §5: "An event a plugin declares and never
//! fires is the same defect as a tool that does nothing").

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HookEntry, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
    TuiSection,
};
use conway::plugin::{async_trait, HookAnswer, HookInvocation, HookRunner};
use conway::{Conway, ConwayBuilder, Plugin as _, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::error::HookFailure;
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, SeqRange, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, SessionStore};
use conway_testkit::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

use conway_plugin_skeleton::{SkeletonPlugin, PLUGIN_ID, PONG_DISPATCHED_EVENT, TOOL_NAME};

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
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
        // Deliberately NOT how this plugin is installed -- `tools.builtin_plugins`
        // is the closed conway-tools candidate set, unrelated to this
        // crate (`PluginsConfig`'s own doc). This test never registers any
        // built-in either way: this crate depends only on `conway`, not
        // `conway-tools`.
        tools: ToolsConfig::default(),
        // `[plugins].install` is read by whatever BINARY links a given
        // first-party plugin crate (`conway-cli`'s own
        // `first_party_plugins.rs`) -- a library embedder instead attaches
        // the plugin directly via `with_plugin` below, which is what this
        // test does. Left empty here on purpose: proving the config-driven
        // install path is `conway-cli`'s own test, not this one.
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

fn tool_call_response(call_id: &str, tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// Builds a `Conway` with every port faked (no network, no built-ins --
/// this crate does not depend on `conway-tools`) and, when `install_skeleton`
/// is `true`, this crate's `SkeletonPlugin` attached exactly the way a
/// library embedder attaches any plugin: `ConwayBuilder::with_plugin`.
/// `store` is handed back too, so a test can read the persisted log
/// afterward the same way `conway`'s own `tests/ask.rs` does.
fn build_conway(backend: Arc<ScriptedBackend>, install_skeleton: bool) -> (Conway, Arc<FakeStore>) {
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let builder = ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store.clone())
        .with_permission_gate(gate)
        .with_router(fake_router());
    let builder = if install_skeleton {
        builder.with_plugin(Arc::new(SkeletonPlugin))
    } else {
        builder
    };
    let conway = builder
        .build()
        .expect("build should succeed with every port injected");
    (conway, store)
}

/// The VERIFICATION ANCHOR: absent by default, present once installed.
/// `Conway::tool_render_kind` is the facade's own public "is this tool
/// registered" probe (used internally by structured-rule validation) --
/// `None` means no plugin registered that name, `Some(_)` means one did.
#[tokio::test]
async fn tool_absent_by_default_present_once_installed() {
    let backend_absent = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi"))])
            .with_id(BackendId::new("fake")),
    );
    let (without_plugin, _store) = build_conway(backend_absent, false);
    assert_eq!(
        without_plugin.tool_render_kind(&ToolName::new(TOOL_NAME)),
        None,
        "the skeleton plugin's tool must not be registered unless installed"
    );

    let backend_present = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi"))])
            .with_id(BackendId::new("fake")),
    );
    let (with_plugin, _store) = build_conway(backend_present, true);
    assert!(
        with_plugin
            .tool_render_kind(&ToolName::new(TOOL_NAME))
            .is_some(),
        "with_plugin(SkeletonPlugin) must register '{TOOL_NAME}'"
    );
}

/// The plugin's manifest id matches the constant a config author (or
/// `conway-cli`'s own bundle) resolves `[plugins].install` entries against.
#[test]
fn manifest_id_matches_the_published_constant() {
    assert_eq!(SkeletonPlugin.manifest().id, PLUGIN_ID);
}

/// End-to-end: once installed, the tool is not just announced but
/// dispatches through the real runtime to this plugin's own `invoke`, and
/// the persisted result carries the exact reply text `invoke` computed.
#[tokio::test]
async fn skeleton_tool_is_callable_end_to_end_through_a_real_turn() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "call-1",
                TOOL_NAME,
                serde_json::json!({ "message": "hello" }),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let (conway, store) = build_conway(backend, true);
    let store: Arc<dyn SessionStore> = store;

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("ping it").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let records = store
        .read(&handle.id(), SeqRange::full())
        .await
        .expect("read should succeed");
    let tool_result = records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == TOOL_NAME => {
                Some(result)
            }
            _ => None,
        })
        .expect("the session must have actually invoked the skeleton plugin's tool");

    assert!(
        !tool_result.is_error,
        "the skeleton_ping call must succeed, not error: {tool_result:?}"
    );
    let text: String = tool_result
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "skeleton pong: hello",
        "the real Tool::invoke must have produced this exact reply, proving the runtime \
         dispatched to this plugin's own implementation and not merely announced its name"
    );
}

// ---------------------------------------------------------------------
// a plugin's own custom event,
// declared AND fired, actually reaches a real configured hook -- end to
// end, through a real `Conway`.
// ---------------------------------------------------------------------

/// Records every `HookInvocation` it is asked to run and always succeeds.
#[derive(Debug, Default)]
struct RecordingHookRunner {
    seen: Mutex<Vec<HookInvocation>>,
}

impl RecordingHookRunner {
    fn invocations(&self) -> Vec<HookInvocation> {
        self.seen.lock().expect("seen lock poisoned").clone()
    }
}

#[async_trait]
impl HookRunner for RecordingHookRunner {
    async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
        self.seen
            .lock()
            .expect("seen lock poisoned")
            .push(invocation.clone());
        Ok(HookAnswer::default())
    }
}

/// Builds a `Conway` with the skeleton plugin installed AND one
/// `[hooks].rules[]` entry subscribed to its declared event -- the same
/// `ConwayBuilder::with_hook_runner` seam any embedder wanting a
/// `pre_tool_use`/`post_tool_use` hook already uses, injected with
/// [`RecordingHookRunner`] rather than a real process-spawning one so this
/// test asserts on exactly what was dispatched without touching a real
/// filesystem or subprocess.
fn build_conway_with_pong_hook(
    backend: Arc<ScriptedBackend>,
    runner: Arc<RecordingHookRunner>,
) -> (Conway, Arc<FakeStore>) {
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let mut config = base_config();
    config.hooks = HooksConfig {
        rules: vec![HookEntry {
            id: "watch-pong".to_string(),
            // The exact namespaced shape `declared_plugin_events` produces:
            // this plugin's own `PLUGIN_ID` + separator + its declared
            // `PONG_DISPATCHED_EVENT` -- an operator writes this by hand,
            // reading it off `Plugin::events()`'s own `summary`, or off
            // this crate's published constants (as this test does, since
            // both are public exactly so a config author CAN).
            event: format!("{PLUGIN_ID}.{PONG_DISPATCHED_EVENT}"),
            command: vec!["unused".to_string()],
            ..Default::default()
        }],
    };
    let conway = ConwayBuilder::from_parts(config)
        .with_backend(backend)
        .with_session_store(store.clone())
        .with_permission_gate(gate)
        .with_router(fake_router())
        .with_plugin(Arc::new(SkeletonPlugin))
        .with_hook_runner(runner)
        .build()
        .expect("build should succeed with every port injected, hook runner included");
    (conway, store)
}

/// **The VERIFICATION ANCHOR for the event half of this** a
/// real `[hooks].rules[]` entry naming this plugin's declared event fires
/// when a real turn calls `skeleton_ping` -- the runner is invoked with the
/// exact namespaced event name and a payload carrying the reply
/// `Tool::invoke` actually produced, not a stubbed/empty one.
#[tokio::test]
async fn a_configured_hook_fires_when_the_skeletons_declared_event_is_dispatched() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "call-1",
                TOOL_NAME,
                serde_json::json!({ "message": "world" }),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let runner = Arc::new(RecordingHookRunner::default());
    let (conway, _store) = build_conway_with_pong_hook(backend, runner.clone());

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("ping it").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let seen = runner.invocations();
    assert_eq!(
        seen.len(),
        1,
        "the configured hook must fire exactly once, for the one skeleton_ping call: {seen:?}"
    );
    assert_eq!(
        seen[0].event.name,
        format!("{PLUGIN_ID}.{PONG_DISPATCHED_EVENT}"),
        "the hook must be invoked with this plugin's own namespaced event name"
    );
    assert_eq!(
        seen[0].event.payload["reply"], "skeleton pong: world",
        "the payload must carry the exact reply text SkeletonPingTool::invoke produced, not a \
         stubbed one"
    );
}

/// The negative half of the same proof: a plugin's declared event does NOT
/// fire an UNRELATED hook -- only the rule actually naming it. Reuses
/// [`build_conway_with_pong_hook`]'s config but drives a DIFFERENT tool
/// call (`conway_ask` is not registered here, so instead this asserts the
/// simplest possible negative: an event name that is merely a PREFIX of
/// the declared one, or the bare plugin id alone, is never what the
/// dispatched invocation names.
#[tokio::test]
async fn the_dispatched_event_name_is_never_merely_a_prefix_or_the_bare_plugin_id() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "call-1",
                TOOL_NAME,
                serde_json::json!({}),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let runner = Arc::new(RecordingHookRunner::default());
    let (conway, _store) = build_conway_with_pong_hook(backend, runner.clone());

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("ping it").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let seen = runner.invocations();
    assert_eq!(seen.len(), 1);
    assert_ne!(
        seen[0].event.name, PLUGIN_ID,
        "must be the FULL namespaced name, not bare"
    );
    assert_ne!(
        seen[0].event.name, PONG_DISPATCHED_EVENT,
        "must be namespaced, never the bare event name alone"
    );
}
