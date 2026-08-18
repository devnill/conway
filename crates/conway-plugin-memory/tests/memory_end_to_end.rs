//! THE ACCEPTANCE TEST (board item 01M090JY3KYHQQMKCZZM1Y6EDZ): a real
//! store, a real `ConwayBuilder`, and a real turn -- proving INTENT.md
//! §5e's load-bearing requirement directly, not by construction of the
//! selection type alone: session A is labelled `"memory"` and holds a real
//! record; session B is NOT a descendant of A (an unrelated root session);
//! B takes a real turn; A's record content reaches the `GenerateRequest`
//! the backend actually received for B's turn.
//!
//! Same shape as `conway-plugin-skeleton`'s own
//! `tests/skeleton_end_to_end.rs` and `conway-plugin-history`'s own
//! `tests/history_end_to_end.rs`: `ConwayBuilder` + the fakes family (no
//! live provider, no network), [`MemoryPlugin`] attached the way a library
//! embedder attaches any plugin, via `ConwayBuilder::with_plugin` -- the
//! curator installs through the SAME channel `Plugin::curators` documents
//! (GP-03).
//!
//! **Why session A is seeded directly against the store rather than driven
//! through `conway.new_session()` + `.prompt()`.** `SessionSpec::labels`
//! (`conway::session_handle`) is, as of this item, a DISCLOSED FACADE GAP:
//! `Conway::new_session`'s own doc says outright that `RootSpec` (the
//! `conway-runtime` type it builds) "has no field for `SessionSpec::labels`
//! ... so neither reaches the created session/agent through this method."
//! Independently confirmed here: `conway-runtime`'s `runtime/root.rs` and
//! `subagent.rs` both hard-code `labels: Vec::new()` at session creation,
//! never reading `RootSpec`/`SubagentSpec` for a caller-supplied value. So
//! there is currently NO facade-only way to make `conway.new_session`
//! produce a labelled session at all -- `SessionSpec::labels` is inert. This
//! is a REPORTABLE FINDING (see this crate's own completion report), not
//! something this test works around by reaching into `conway-core`
//! production code: session A's log is still a REAL record in the REAL
//! `SessionStore` this `Conway` is built over (the same store B's own turn
//! reads through `ctx.store`), seeded via the store's own `SessionStore`
//! port directly -- the same "construct sessions directly against the
//! store" pattern `conway-runtime`'s own `curator_stage.rs` test seeding
//! helper already uses for exactly this reason. Session B, in contrast,
//! goes through the full real `Conway`/`SessionHandle`/turn path
//! end-to-end, which is what the acceptance criterion is actually about:
//! that a REAL turn's REAL assembled request carries a foreign record.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::Plugin as _;
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::{LogRecord, SessionMeta};
use conway_core::ports::{GenerateResponse, PluginConfig, SessionStore};
use conway_core::provenance::Provenance;
use conway_testkit::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

use conway_plugin_memory::{MemoryConfig, MemoryPlugin, DEFAULT_LABEL, PLUGIN_ID};

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
        // Deliberately empty, same as the skeleton/history end-to-end
        // tests: `[plugins].install` is read by whatever BINARY links this
        // crate; a library embedder attaches directly via `with_plugin`.
        tools: ToolsConfig::default(),
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

/// Seeds a session directly against `store` -- a REAL record in the REAL
/// `SessionStore` `Conway` is built over -- labelled `label`, holding one
/// `UserTurn` + one plain-text `Assistant` reply (both content records,
/// neither carrying a tool block, so both are recallable per R5). Returns
/// the new session's id. See this file's own module doc for why session A
/// is seeded this way rather than through `conway.new_session`.
async fn seed_labelled_session(
    store: &FakeStore,
    label: &str,
    user_text: &str,
    assistant_reply: &str,
) -> SessionId {
    let id = SessionId::new();
    store
        .create(SessionMeta {
            id,
            agent_id: AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![label.to_string()],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig::default(),
        })
        .await
        .expect("create session A");
    let seq = store.head(&id).await.expect("head");
    store
        .append(
            &id,
            LogRecord::UserTurn {
                seq,
                ts: Utc::now(),
                text: user_text.to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .expect("append A's user turn");
    let seq = store.head(&id).await.expect("head");
    store
        .append(
            &id,
            LogRecord::Assistant {
                seq,
                ts: Utc::now(),
                content: vec![ContentBlock::Text {
                    text: assistant_reply.to_string(),
                }],
                model: "anthropic/claude-sonnet-4-6".parse().unwrap(),
                route_reason: serde_json::json!({"AliasPrimary": {"alias": "default"}}),
                usage: Usage::default(),
                stop: StopReason::EndTurn,
            },
        )
        .await
        .expect("append A's assistant reply");
    id
}

/// A real, fully-faked `Conway` (no network, no live provider) with
/// [`MemoryPlugin`] attached exactly the way a library embedder would --
/// `ConwayBuilder::with_plugin`, the identical call
/// `crates/conway-cli/src/first_party_plugins.rs` would make for a bundled
/// first-party plugin. `store`/`backend` are handed back too, so the test
/// can seed a session directly and inspect every `GenerateRequest` the
/// backend actually received.
fn build_conway(
    backend: Arc<ScriptedBackend>,
    store: Arc<FakeStore>,
    install_memory: bool,
) -> Conway {
    let gate: Arc<dyn conway::PermissionGate> =
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }));
    let builder = ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(router);
    let builder = if install_memory {
        builder.with_plugin(Arc::new(MemoryPlugin::new(MemoryConfig::default())))
    } else {
        builder
    };
    builder
        .build()
        .expect("build should succeed with every port injected")
}

/// A real `Conway` build succeeds with this plugin installed.
#[test]
fn conway_builds_with_memory_plugin_installed() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi"))])
            .with_id(BackendId::new("fake")),
    );
    let _conway = build_conway(backend, Arc::new(FakeStore::new()), true);
}

/// The plugin's manifest id matches the published constant a config author
/// resolves `[plugins].install` entries against.
#[test]
fn manifest_id_matches_the_published_constant() {
    let plugin = MemoryPlugin::new(MemoryConfig::default());
    assert_eq!(plugin.manifest().id, PLUGIN_ID);
}

/// **THE ACCEPTANCE TEST.** Session A is labelled `"memory"` and holds a
/// real user turn plus a real assistant reply. Session B is a SEPARATE root
/// session -- not forked from A, not a descendant of A in any way -- so
/// nothing in B's own ancestry could ever surface A's content through
/// ordinary prefix inheritance. B then takes its own real turn through a
/// real `Conway`/`SessionHandle`, and the assertion is on the ACTUAL
/// `GenerateRequest` the fake backend received for that turn: A's own
/// content is present as rendered segments. This is the INTENT.md §5e
/// requirement checked directly -- a foreign record, from outside the
/// calling session's ancestry, reaching the assembled request -- not merely
/// that `derive_with` PERMITS a cross-tree `Include`.
#[tokio::test]
async fn a_foreign_labelled_sessions_record_reaches_a_different_sessions_assembled_context() {
    let store = Arc::new(FakeStore::new());
    seed_labelled_session(
        &store,
        DEFAULT_LABEL,
        "Remember this: the deploy secret lives in a vault path.",
        "Noted: the deploy secret lives in vault path secret/data/prod-deploy.",
    )
    .await;

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("done"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend.clone(), store, true);

    // Session B: an UNRELATED root session -- no fork/spawn relationship to
    // A whatsoever, and not itself labelled.
    let session_b = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session for B should succeed");
    let turn_b = session_b
        .prompt("What should I check before deploying?")
        .await
        .expect("prompt B");
    tokio::time::timeout(Duration::from_secs(5), turn_b.result())
        .await
        .expect("B's result() must not hang")
        .expect("B's result() should succeed");

    // The assertion: B's OWN request to the backend carries A's content.
    let calls = backend.calls();
    let b_request = calls
        .last()
        .expect("the backend must have received B's own request");

    let mut all_text = String::new();
    for segment in &b_request.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                all_text.push_str(text);
                all_text.push('\n');
            }
        }
    }

    assert!(
        all_text.contains("Remember this: the deploy secret lives in a vault path."),
        "session A's own user turn must reach session B's assembled request; got segments: \
         {all_text:?}"
    );
    assert!(
        all_text.contains("Noted: the deploy secret lives in vault path secret/data/prod-deploy."),
        "session A's own assistant reply must ALSO reach session B's assembled request (both are \
         plain-text content records, neither carries a tool block); got segments: {all_text:?}"
    );
}

/// The negative complement: with the plugin NOT installed, a labelled
/// session's content never reaches an unrelated session's request -- the
/// zero-cost pass-through the curator stage's own doc promises, and the
/// property that makes `context_golden`'s own 11/11-unregenerated gate
/// meaningful.
#[tokio::test]
async fn without_the_plugin_installed_a_labelled_sessions_content_never_reaches_another_session() {
    let store = Arc::new(FakeStore::new());
    seed_labelled_session(
        &store,
        DEFAULT_LABEL,
        "the secret marker text prompt",
        "the secret marker text reply",
    )
    .await;

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("done"))])
            .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend.clone(), store, false);

    let session_b = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session for B should succeed");
    let turn_b = session_b.prompt("hello").await.expect("prompt B");
    tokio::time::timeout(Duration::from_secs(5), turn_b.result())
        .await
        .expect("B's result() must not hang")
        .expect("B's result() should succeed");

    let calls = backend.calls();
    let b_request = calls.last().expect("backend received B's request");
    let mut all_text = String::new();
    for segment in &b_request.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                all_text.push_str(text);
            }
        }
    }
    assert!(
        !all_text.contains("the secret marker text prompt"),
        "with no curator installed, A's content must never reach B's request: {all_text:?}"
    );
}
