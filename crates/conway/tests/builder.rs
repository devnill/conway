//! Acceptance tests for `ConwayBuilder`/`Conway` assembly (WI-100).

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionMode,
    PermissionsConfig, RoleEntry, RoutingSection, SessionConfig, TuiSection,
};
use conway::config::schema::{BackendEntry, BackendKind};
use conway::{Conway, ConwayBuilder, ConwayError, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{StopReason, Usage};
use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use conway_core::ids::{BackendId, RoleAlias};
use conway_core::ports::{GenerateResponse, SessionStore};
#[cfg(feature = "builtin-tools")]
use conway_core::ports::{Plugin, PluginManifest, Tool};

/// `Conway` deliberately does not derive `Debug` (it wraps `Arc<Runtime>`,
/// which does not either), so `Result::expect_err`/`unwrap_err` (which both
/// require `T: Debug`) cannot be used on a `Result<Conway, _>` here.
fn expect_build_err(result: Result<Conway, ConwayError>, msg: &str) -> ConwayError {
    match result {
        Err(err) => err,
        Ok(_) => panic!("{msg}"),
    }
}

fn caps() -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::Grammar,
        max_context_tokens: 100_000,
        reasoning: true,
        reliability_tier: ReliabilityTier::Verified,
    }
}

/// An injected router that trivially satisfies `ConwayBuilder::with_router`,
/// used by tests whose `ConwayConfig` carries an empty-chain role.
/// `conway_routing::DeclarativeRouter::new` (built when no router is
/// injected) runs its own, stricter `conway_routing::config::validate`,
/// which rejects an empty chain -- a check `crate::config::merge::validate`
/// (the facade's own, already-run validation) does not perform. Injecting a
/// `FakeRouter` sidesteps that stricter check for tests that are not
/// exercising routing behavior.
fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::new(vec![]))
}

fn fake_backend(id: &str) -> Arc<dyn conway_core::ports::Backend> {
    Arc::new(FakeBackend::new(
        BackendId::new(id),
        caps(),
        GenerateResponse {
            content: vec![],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
        },
    ))
}

/// A minimal, no-op `Plugin` test double, used only to exercise the
/// duplicate-manifest-id rejection path (this crate has no `FakePlugin`).
#[cfg(feature = "builtin-tools")]
struct DummyPlugin(&'static str);

#[cfg(feature = "builtin-tools")]
impl Plugin for DummyPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.0.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

/// A minimal config: one role with an empty chain (so `merge::validate`'s
/// chain/backend-existence check is trivially satisfied), no backends, and
/// otherwise-default sections. `cwd = "."`, so `agents.dir`
/// (`.conway/agents`, relative) resolves to a directory that does not exist
/// under the test process's cwd -- `agents::load_agent_defs` treats a
/// missing directory as `Ok(empty)`, not an error.
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
        default_role: conway_core::ids::RoleAlias::new("default"),
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
    }
}

#[tokio::test]
async fn end_to_end_from_parts_with_fakes_succeeds_with_no_network_or_fs() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store.clone())
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    // A prompt-less session starts IDLE: `new_session` creates the session
    // but writes no initial turn record, and the agent does not run until the
    // first `prompt`. (Previously an empty placeholder `UserTurn` was written
    // and run immediately, making the agent "explore" before any prompt --
    // that was a bug; see `Runtime::start_root`.)
    assert_eq!(
        store.total_record_count(),
        0,
        "a prompt-less session writes no initial turn record; it idles until the first prompt"
    );
    // `id()`/`root()` are populated (non-panicking access is the assertion;
    // ULIDs are always non-nil).
    let _ = handle.id();
    let _ = handle.root();
}

/// `SessionSpec::default()`'s `None` fields resolve, at `new_session` call
/// time, to `config.default_role`/`config.cwd` -- checked here against the
/// session actually recorded by the store rather than merely the
/// `new_session` call succeeding (`FakeStore::meta` returns the
/// `SessionMeta` `Runtime::start_root` built from the resolved `RootSpec`).
/// `budget`/`labels` have no store-side inspection point yet (`SessionMeta`
/// carries no budget field, and `RootSpec` has no field for
/// `SessionSpec::labels` at all -- see `conway.rs`'s own disclosed gap), so
/// asserting those two is deferred to WI-101.
#[tokio::test]
async fn new_session_with_default_spec_resolves_role_and_cwd_from_config() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store.clone())
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    let meta = store
        .meta(&handle.id())
        .await
        .expect("the session created by new_session must be readable back from the store");

    assert_eq!(meta.role, Some(RoleAlias::new("default")));
    assert_eq!(meta.cwd, std::path::PathBuf::from("."));
}

#[test]
fn build_fails_with_no_backends_configured() {
    let cfg = base_config();
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .build();
    let err = expect_build_err(result, "no backend injected and none configured must fail");

    match err {
        ConwayError::Build { message } => {
            assert!(message.contains("no backends"), "{message}");
        }
        other => panic!("expected Build error, got {other:?}"),
    }
}

#[cfg(not(feature = "jsonl-store"))]
#[test]
fn build_fails_with_no_session_store_when_jsonl_store_disabled() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let result = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build();
    let err = expect_build_err(
        result,
        "no store injected and jsonl-store disabled must fail",
    );

    match err {
        ConwayError::Build { message } => {
            assert!(message.contains("no session store"), "{message}");
        }
        other => panic!("expected Build error, got {other:?}"),
    }
}

#[cfg(feature = "jsonl-store")]
#[tokio::test]
async fn build_constructs_default_jsonl_store_when_none_injected() {
    let mut cfg = base_config();
    let root = support::unique_temp_dir("builder-jsonl-store");
    cfg.cwd = root.clone();
    cfg.session.root = std::path::PathBuf::from("sessions");

    let backend = fake_backend("fake");
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should synthesize a real JsonlSessionStore");

    conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session against the real store should succeed");

    assert!(
        root.join("sessions").is_dir(),
        "JsonlSessionStore::open should have created the session root directory"
    );
}

/// An `anthropic`-kind backend may be named anything: `AnthropicConfig`
/// carries an `id`, set from the `backends.<id>` JSON key, and
/// `AnthropicBackend::id()` returns it. This is what lets an
/// Anthropic-compatible third-party endpoint be named for what it is
/// (`kimi`) instead of squatting the key `"anthropic"`.
///
/// This previously errored: the id was hardcoded, so a non-`"anthropic"`
/// key was rejected at `build()` time to avoid a routing panic. Now the
/// backend map and `config::merge::validate`'s chain-ref namespace agree by
/// construction, so no such guard is needed.
#[cfg(feature = "anthropic")]
#[test]
fn build_accepts_an_anthropic_backend_under_any_json_key() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "kimi".to_string(),
        BackendEntry {
            kind: BackendKind::Anthropic,
            api_key: "any-non-empty-key".to_string(),
            base_url: "https://api.kimi.com/coding/".to_string(),
            ..BackendEntry::default()
        },
    );
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("an anthropic-kind backend under the key 'kimi' must build");
}

/// The default case: a `backends.anthropic` entry still builds, unchanged.
#[cfg(feature = "anthropic")]
#[test]
fn build_succeeds_for_a_conventionally_named_anthropic_backend() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "anthropic".to_string(),
        BackendEntry {
            kind: BackendKind::Anthropic,
            api_key: "sk-ant-api03-not-a-real-key".to_string(),
            ..BackendEntry::default()
        },
    );
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("a matching 'anthropic' JSON key must build successfully");
}

#[cfg(not(feature = "anthropic"))]
#[test]
fn build_reports_unsupported_feature_for_anthropic_backend_kind() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "anthropic".to_string(),
        BackendEntry {
            kind: BackendKind::Anthropic,
            api_key: "sk-ant-api03-not-a-real-key".to_string(),
            ..BackendEntry::default()
        },
    );
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .build();
    let err = expect_build_err(result, "anthropic feature is disabled");

    match err {
        ConwayError::UnsupportedFeature { feature, message } => {
            assert_eq!(feature, "anthropic");
            assert!(message.contains("anthropic"), "{message}");
        }
        other => panic!("expected UnsupportedFeature error, got {other:?}"),
    }
}

/// Indirect but discriminating proof that `with_permission_gate` overrides
/// config-derived gate selection: `permissions.mode = "prompt"` with no
/// injected gate always fails `build()` (no `ConwayBuilder` method supplies
/// a prompt handler -- see `builder.rs`'s module doc), so a `build()`
/// success with mode `"prompt"` *and* an injected gate can only be explained
/// by the injected gate having been used instead of `gates::from_config`.
#[test]
fn injected_permission_gate_overrides_config_derived_selection() {
    let mut cfg = base_config();
    cfg.permissions = PermissionsConfig {
        mode: PermissionMode::Prompt,
        allowed_tools: vec![],
        denied_tools: vec![],
    };
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());

    // Without an override: prompt mode with no handler is a Config error.
    let result = ConwayBuilder::from_parts(cfg.clone())
        .with_backend(backend.clone())
        .with_session_store(store.clone())
        .with_router(fake_router())
        .build();
    let err = expect_build_err(
        result,
        "prompt mode with no injected gate and no handler must fail",
    );
    assert!(matches!(err, ConwayError::Config { .. }));

    // With an override: build succeeds, proving the injected gate was used.
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("an injected gate must bypass config-derived prompt-mode selection");
}

#[cfg(feature = "builtin-tools")]
#[test]
fn duplicate_injected_plugin_id_is_rejected() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    // "conway.fs" collides with the built-in fs plugin's manifest id.
    let result = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .with_plugin(Arc::new(DummyPlugin("conway.fs")))
        .build();
    let err = expect_build_err(
        result,
        "a plugin id colliding with a built-in must be rejected",
    );

    match err {
        ConwayError::Build { message } => {
            assert!(message.contains("duplicate plugin id"), "{message}");
            assert!(message.contains("conway.fs"), "{message}");
        }
        other => panic!("expected Build error, got {other:?}"),
    }
}

#[cfg(feature = "openai-compat")]
#[test]
fn injected_backend_replaces_config_derived_backend_with_same_id() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "local".to_string(),
        BackendEntry {
            kind: BackendKind::OpenaiCompat,
            dialect: Some("ollama".to_string()),
            base_url: "http://localhost:11434/v1".to_string(),
            ..BackendEntry::default()
        },
    );
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    // Distinguish the injected fake from the real config-derived backend by
    // its declared capabilities (a real `OpenAiCompatBackend`'s
    // `max_context_tokens` comes from dialect defaults, never 100_000).
    let injected = fake_backend("local");
    let conway = ConwayBuilder::from_parts(cfg)
        .with_backend(injected)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed");

    // There is no public accessor for the backend map (Conway's own
    // surface deliberately does not expose it), so this test only proves
    // `build()` accepted the injection without erroring; the mechanical
    // "same key -> replaced" guarantee is exercised structurally by
    // `builder.rs`'s own construction order (config-derived insert, then
    // injected insert, into the same `HashMap` keyed by `Backend::id()`).
    let _ = conway;
}

/// Whether a TCP connection has landed in `listener`'s accept backlog.
/// Non-blocking, single-shot: a completed handshake is already queued by the
/// kernel by the time this is called (called only after `build()` has
/// already returned, so any network attempt `build()` made has either
/// already happened or never will).
#[cfg(feature = "openai-compat")]
fn connection_was_accepted(listener: &std::net::TcpListener) -> bool {
    listener.set_nonblocking(true).expect("set_nonblocking");
    listener.accept().is_ok()
}

/// Direct verification of the "`build()` performs no network I/O" /
/// "`CapabilityProbe` is only invoked when `probe_on_startup = true`"
/// criterion: a real `openai-compat` backend entry pointed at a local
/// `TcpListener` (bound to an ephemeral port, never actually speaking
/// HTTP) lets this test observe, at the TCP level, whether `build()` ever
/// attempted a connection -- independent of whatever mechanism performs the
/// probe (this crate's `probe_openai_compat_backends` calls
/// `conway_backends::probe::CapabilityProbe` directly rather than through
/// `Backend::probe()`; a "counting `Backend`" double would not observe
/// anything, since no `Backend` instance is consulted for capability
/// discovery -- see `builder.rs`'s module doc, reconciliation on startup
/// probing).
#[cfg(feature = "openai-compat")]
#[test]
fn probe_on_startup_false_makes_no_network_call_true_does() {
    // probe_on_startup = false (the default): zero connection attempts.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let mut cfg = base_config();
    cfg.backends.insert(
        "probe".to_string(),
        BackendEntry {
            kind: BackendKind::OpenaiCompat,
            dialect: Some("openai".to_string()),
            base_url: format!("http://{addr}/v1"),
            ..BackendEntry::default()
        },
    );
    assert!(
        !cfg.models.probe_on_startup,
        "probe_on_startup must default to false"
    );
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed: construction never contacts the backend");

    assert!(
        !connection_was_accepted(&listener),
        "probe_on_startup = false (default) must perform zero network calls"
    );

    // probe_on_startup = true: build() attempts a real connection (the
    // request itself is doomed -- the listener never speaks HTTP -- but
    // `CapabilityProbe` treats every failure as "found nothing", never an
    // error, so `build()` still succeeds; see `CapabilityProbe::discover`).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let mut cfg = base_config();
    cfg.backends.insert(
        "probe".to_string(),
        BackendEntry {
            kind: BackendKind::OpenaiCompat,
            dialect: Some("openai".to_string()),
            base_url: format!("http://{addr}/v1"),
            ..BackendEntry::default()
        },
    );
    cfg.models.probe_on_startup = true;
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should still succeed: a probe failure is a warning, not an error");

    assert!(
        connection_was_accepted(&listener),
        "probe_on_startup = true must attempt at least one network call"
    );
}

/// `Conway::explain_routing` delegates to `conway_routing::RoutingExplain`
/// over the concrete `DeclarativeRouter` this `Conway` compiled itself
/// (no `with_router` override here), and its `entries` correspond 1:1 to
/// the role's configured chain, in order -- mirrors the shape
/// `conway-routing`'s own `RoutingExplain` tests assert.
#[cfg(feature = "openai-compat")]
#[test]
fn explain_routing_reports_the_configured_chain_for_the_role() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "local".to_string(),
        BackendEntry {
            kind: BackendKind::OpenaiCompat,
            dialect: Some("ollama".to_string()),
            base_url: "http://localhost:11434/v1".to_string(),
            ..BackendEntry::default()
        },
    );
    cfg.roles
        .get_mut("default")
        .expect("default role present")
        .chain = vec!["local/model-a".to_string(), "local/model-b".to_string()];

    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    // No `with_router` override: `build()` compiles its own `DeclarativeRouter`,
    // which `explain_routing` needs to project through.
    let conway = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .build()
        .expect("build should succeed with a real, non-empty chain");

    let role = conway_core::ids::RoleAlias::new("default");
    let report = conway.explain_routing(&role);

    assert_eq!(report.role, role);
    assert_eq!(
        report.entries.len(),
        2,
        "one entry per chain candidate, regardless of selected/skipped outcome"
    );
    assert_eq!(
        report.entries[0].model_ref,
        "local/model-a"
            .parse::<conway_core::ids::ModelRef>()
            .unwrap()
    );
    assert_eq!(
        report.entries[1].model_ref,
        "local/model-b"
            .parse::<conway_core::ids::ModelRef>()
            .unwrap()
    );
    assert_eq!(report.entries[0].chain_position, Some(0));
    assert_eq!(report.entries[1].chain_position, Some(1));
}

/// The first thing a new Kimi user hits if they follow the docs but forget
/// to export the key. `api_key_env` is resolved from the live process
/// environment at `build()` time, so a missing variable must fail loudly
/// and name the variable -- not panic, and (since `d27b5c0`) not misdirect
/// the user to another vendor's console.
#[test]
fn unset_api_key_env_fails_naming_the_variable() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "kimi".to_string(),
        BackendEntry {
            kind: BackendKind::Anthropic,
            api_key: String::new(),
            api_key_env: "CONWAY_TEST_DEFINITELY_UNSET_KEY_VAR".to_string(),
            base_url: "https://api.kimi.com/coding/".to_string(),
            dialect: None,
            stream_tools: None,
        },
    );

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(fake_router())
        .build();

    let err = result
        .err()
        .expect("an unset api_key_env must be a hard error")
        .to_string();
    assert!(
        err.contains("CONWAY_TEST_DEFINITELY_UNSET_KEY_VAR"),
        "the error must name the missing variable: {err}"
    );
    assert!(
        !err.contains("console.anthropic.com"),
        "must not misdirect a third-party-endpoint user to Anthropic's console: {err}"
    );
}
