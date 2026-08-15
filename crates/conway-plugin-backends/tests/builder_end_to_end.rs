//! The first-party plugin tier's own acceptance test for this crate (board
//! item), the backend-side sibling of
//! `crates/conway-plugin-skeleton/tests/skeleton_end_to_end.rs`: written the
//! way a library embedder would write it, `ConwayBuilder` +
//! `ConwayBuilder::with_backend_factory` called directly, no
//! `conway-cli`/`first_party_plugins` involved at all -- proving
//! ("every mode reachable") for the library-embedder path the way
//! `crates/conway-cli/tests/first_party_plugins.rs`'s
//! `default_backends_attach_with_no_plugins_install_entry_and_complete_a_
//! one_shot_prompt` proves it for the CLI-binary path.
//!
//! Unlike the skeleton crate's own end-to-end test (fully in-memory,
//! `ScriptedBackend`), this one drives the REAL
//! `OpenAiCompatBackendFactory::build` -> real `OpenAiCompatBackend` ->
//! real HTTP request, against a loopback `wiremock` server -- no
//! credentials, no network beyond that loopback listener. This is
//! the discriminating proof that `ConwayBuilder::with_backend_factory` is
//! not merely accepted but genuinely reaches the wire: a builder that
//! silently dropped the registered factory, or built a backend that never
//! actually dialed the configured `base_url`, would leave the mock server
//! unreached and this test hanging/failing rather than completing.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::ids::RoleAlias;
use conway_plugin_backends::OpenAiCompatBackendFactory;
use conway_testkit::{FakeGate, FakeStore};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A real, validated `ConwayConfig`: one role naming a model on a real
/// `openai-compat`/`openai`-dialect backend pointed at `base_url` -- the
/// exact shape a hand-authored `settings.json` would carry, never a
/// `with_backend` injection standing in for it (that would prove nothing
/// about `with_backend_factory` itself).
fn config(base_url: String) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec!["mock/echo-model".to_string()],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    let mut backends = BTreeMap::new();
    backends.insert(
        "mock".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
            base_url,
            dialect: Some("openai".to_string()),
            ..BackendEntry::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("coder"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends,
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

/// Wires a `Conway` exactly as a library embedder linking this crate
/// directly would: `ConwayBuilder::with_backend_factory(Arc::new(
/// OpenAiCompatBackendFactory))`, the same call `conway-cli`'s own
/// `first_party_plugins::backend_bundle` makes internally for the shipped
/// binary -- no `.with_backend` injection anywhere in this file.
fn build_conway(config: ConwayConfig) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(config)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_backend_factory(Arc::new(OpenAiCompatBackendFactory))
        .build()
        .expect(
            "build should succeed: a real config-derived openai-compat backend, resolved \
             through the registered OpenAiCompatBackendFactory",
        )
}

/// Renders `events` as an SSE body, one `data:` line per event, terminated
/// by `data: [DONE]` -- mirrors `tests/openai_compat_stream.rs`'s own
/// `sse_body` helper. Real SSE, not a single JSON document, is required
/// here: the `"openai"` profile's `tool_calling` default
/// (`Streaming{validated:true}`) means `AttemptEngine` always selects the
/// streaming path, tools present or not (`fanout_prefix_sharing.rs`'s own
/// module doc explains the identical requirement for the Anthropic
/// dialect).
fn sse_body(events: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

/// The end-to-end proof: a config naming `kind = "openai-compat"` with no
/// injected `Backend` anywhere resolves, through the registered factory
/// alone, to a real backend that reaches the mock server and completes a
/// turn with the model's own reply text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn factory_installed_backend_completes_a_real_turn_against_a_loopback_server() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({"choices": [{"delta": {"content": "hello from the factory-built backend"}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let conway = build_conway(config(server.uri()));

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let text = tokio::time::timeout(Duration::from_secs(10), turn.text())
        .await
        .expect("turn must not hang")
        .expect("turn should succeed");

    assert_eq!(
        text, "hello from the factory-built backend",
        "the turn's own text must be the real backend's reply, proving the registered \
         BackendFactory constructed a working backend that genuinely reached the wire"
    );
    server.verify().await;
}
