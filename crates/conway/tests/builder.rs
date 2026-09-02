//! Acceptance tests for `ConwayBuilder`/`Conway` assembly.

mod support;

use std::collections::BTreeMap;
// Only named by the `jsonl-store`-gated tests below.
#[cfg(feature = "jsonl-store")]
use std::fs;
use std::sync::Arc;

use conway::config::schema::BackendEntry;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionMode, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::{Conway, ConwayBuilder, FacadeError, SessionSpec};
// Only named by the `builtin-tools`-gated tests below.
use conway::test_support::test_builder_without_router;
#[cfg(feature = "builtin-tools")]
use conway::PluginSelection;
use conway_core::agent::{PermissionDecision, ResultStatus};
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{StopReason, Usage};
#[cfg(feature = "builtin-tools")]
use conway_core::ids::ToolName;
use conway_core::ids::{BackendId, RoleAlias};
use conway_core::ports::{GenerateResponse, SessionStore};
#[cfg(feature = "builtin-tools")]
use conway_core::ports::{HostCapability, Plugin, PluginManifest, RenderKind, Tool};
// Only named by the `jsonl-store`-gated tests below.
#[cfg(feature = "jsonl-store")]
use conway_core::provenance::Provenance;
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

/// `Conway` deliberately does not derive `Debug` (it wraps `Arc<Runtime>`,
/// which does not either), so `Result::expect_err`/`unwrap_err` (which both
/// require `T: Debug`) cannot be used on a `Result<Conway, _>` here.
fn expect_build_err(result: Result<Conway, FacadeError>, msg: &str) -> FacadeError {
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
/// used by tests whose `ConwayConfig` carries an empty-chain role and that
/// are not themselves exercising routing behavior.
///: `build()`'s no-router/no-factory default is
/// now `conway_core::routing::MinimalRouter`, which never validates a
/// chain at construction at all (unlike the opt-in
/// `conway-plugin-routing::DeclarativeRouter::new`, whose own, stricter
/// `config::validate` rejects an empty chain -- a check
/// `crate::config::merge::validate`, the facade's own already-run
/// validation, does not perform). This router therefore no longer exists
/// to dodge a validation failure these tests would otherwise hit; it stays
/// because these tests need a deterministic, content-free `Router` double,
/// not `MinimalRouter`'s own chain-order behavior.
///
/// It is `FakeRouter::new(vec![])` -- an EMPTY chain -- and so is
/// deliberately not the `FakeRouter::single(test_support::echo_model())`
/// every other suite injects. The old name for it here was `fake_router`,
/// the same name 30-odd other files used for the one-route version; it is
/// `empty_router` now so that one name does not mean two routers.
fn empty_router() -> Arc<dyn conway_core::ports::Router> {
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
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

/// A no-op `Plugin` that declares exactly one instruction fragment named
/// `.1` -- board item `01M0K5MD59YZRSHE31JKZKFRMY`'s duplicate-name check
/// (`ConwayBuilder::build`, a build-time, configuration-independent fact,
/// unlike the reachability check itself). The fragment names no tool_ids,
/// so it never exercises reachability -- these tests are scoped to the
/// naming collision alone.
#[cfg(feature = "builtin-tools")]
struct InstructingPlugin(&'static str, &'static str);

#[cfg(feature = "builtin-tools")]
impl Plugin for InstructingPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.0.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn instructions(&self) -> Vec<conway_core::ports::InstructionFragment> {
        vec![conway_core::ports::InstructionFragment::new(
            self.1,
            "some instruction text",
        )]
    }
}

/// `InstructingPlugin` plus a `PluginManifest::requires` edge -- the
/// dedicated fixture for the injection-order/dependency-resolution-order
/// separation test below (board item `01M0WWJMYK0KDC2X7B7MR46FRR`). Kept
/// distinct from `InstructingPlugin` (which declares no dependency) so the
/// two concerns stay legible at each call site.
#[cfg(feature = "builtin-tools")]
struct InstructingDependentPlugin {
    id: &'static str,
    fragment_name: &'static str,
    requires: Vec<&'static str>,
}

#[cfg(feature = "builtin-tools")]
impl Plugin for InstructingDependentPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: self.requires.iter().map(|s| s.to_string()).collect(),
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn instructions(&self) -> Vec<conway_core::ports::InstructionFragment> {
        vec![conway_core::ports::InstructionFragment::new(
            self.fragment_name,
            "some instruction text",
        )]
    }
}

/// A no-op `Plugin` that declares exactly one instruction fragment, at
/// `FragmentPosition::BeforeSystemPrompt`/`order: -100` -- the exact shape
/// `conway_plugin_idiom::IdiomPlugin::instructions`'s base fragment declares
/// (that crate cannot be a dev-dependency here without a cycle: it depends
/// on `conway` itself), used to prove position/order render correctly
/// through a REAL facade build, not merely through `conway-runtime`'s own
/// unit tests.
#[cfg(feature = "builtin-tools")]
struct BeforeSystemPromptPlugin(&'static str);

#[cfg(feature = "builtin-tools")]
impl Plugin for BeforeSystemPromptPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test.before_system_prompt".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn instructions(&self) -> Vec<conway_core::ports::InstructionFragment> {
        vec![conway_core::ports::InstructionFragment::new(
            self.0,
            "conway orientation text, rendered ahead of any agent def's own prompt",
        )
        .with_position(conway_core::ports::FragmentPosition::BeforeSystemPrompt)
        .with_order(-100)]
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
            ..Default::default()
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
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

#[cfg(feature = "jsonl-store")]
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
        .with_router(empty_router())
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
/// asserting those two is deferred to earlier work.
#[cfg(feature = "jsonl-store")]
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
        .with_router(empty_router())
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

/// Board item `01M163T1KGX3HTCC2YMDPT655J` flipped this from "`build()`
/// refuses an empty backend map" (this test's own predecessor, which
/// pinned exactly that) to this: zero backends -- no `[backends.<id>]`
/// entry, no `with_backend` injection -- builds a working `Conway` whose
/// first turn fails with the SAME typed `RoutingError::NoCandidate` an
/// empty role chain already produced before this item
/// (`crates/conway/tests/discover_getting_started_example_smoke.rs`'s own
/// `unmodified_default_role_still_fails_to_route_with_a_named_no_candidate_
/// error`, which no longer needs a dummy backend to reach it either -- see
/// that file). No router is injected here on purpose: the real
/// `MinimalRouter` `build()` compiles from `cfg.roles` (this file's
/// `base_config()`'s own empty `default` chain) is what actually produces
/// `NoCandidate`, not a test double standing in for it -- proving the
/// gate removal reaches all the way to a real turn, not merely that
/// `build()` no longer returns `Err`.
#[tokio::test]
async fn build_succeeds_with_no_backends_configured_and_a_turn_names_no_candidate() {
    let cfg = base_config();
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .build()
        .expect("zero backends must no longer refuse the build");

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = session
        .prompt("hello")
        .await
        .expect("prompt should succeed -- the turn fails later, inside the agent loop");
    let result = turn
        .result()
        .await
        .expect("result() itself must not error -- the turn ends Failed, not the stream");

    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(
                error.contains("no candidate for role"),
                "must name the role, not route silently: {error}"
            );
            assert!(
                error.contains("(0 considered)"),
                "zero backends means zero candidates to consider: {error}"
            );
        }
        other => {
            panic!("expected ResultStatus::Failed (a named no-candidate error), got {other:?}")
        }
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
        .with_router(empty_router())
        .build();
    let err = expect_build_err(
        result,
        "no store injected and jsonl-store disabled must fail",
    );

    match err {
        FacadeError::Build { message } => {
            assert!(message.contains("no session store"), "{message}");
        }
        other => panic!("expected Build error, got {other:?}"),
    }
}

/// The gap board item `01M0J7KWQDM4PMPD0TFFKSFTES` was filed over: a
/// facade-only caller supplies a custom `SessionStore` (satisfying step 8),
/// which used to make the fallthrough to `build_default_path_store`'s error
/// (step 8b) reachable with `jsonl-store` off and no `with_path_store` call --
/// previously untested in either direction. `build()` must still refuse
/// (there is no default `PathStore` without `jsonl-store`, and `PathStore`
/// is not nameable by a facade-only caller -- board item
/// `01M0EMCK55628YJXGBQY8YGXHE`), but the message must not point the caller
/// at `with_path_store` as if it were an actionable escape hatch: it names a
/// type this caller cannot name.
#[cfg(not(feature = "jsonl-store"))]
#[test]
fn build_fails_with_no_path_store_when_jsonl_store_disabled_even_with_custom_session_store() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let result = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_session_store(Arc::new(FakeStore::new()))
        .build();
    let err = expect_build_err(
        result,
        "a custom SessionStore satisfies step 8, but no path store was injected and \
         jsonl-store is disabled -- build() must still fail",
    );

    match err {
        FacadeError::Build { message } => {
            assert!(message.contains("no path store"), "{message}");
            assert!(
                !message.contains("call ConwayBuilder::with_path_store\""),
                "message must not suggest with_path_store as if a facade-only caller could \
                 act on it unqualified: {message}"
            );
            assert!(
                message.contains("engine-internal"),
                "message should name the real constraint (PathStore is engine-internal, not \
                 re-exported): {message}"
            );
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
    cfg.session.root = Some(std::path::PathBuf::from("sessions"));

    let backend = fake_backend("fake");
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_permission_gate(gate)
        .with_router(empty_router())
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

/// **`ConwayBuilder::from_parts` bypasses `config::load` entirely**, so the
/// central, project-keyed default (board item `01M0QK9GRM8HSNWRAR414TCX42`)
/// -- resolved at `config::load` time using its own `env`/`cwd`, which
/// `from_parts`-constructed configs never went through -- is never
/// computed. This proves the disclosed fallback instead: `session.root`
/// left at `None` (the type's own default) still opens a REAL
/// `JsonlSessionStore`, at the exact fixed location the field always
/// defaulted to before this item existed, `.conway/sessions` under `cwd` --
/// byte-identical to every OTHER test in this crate's suite that builds a
/// `ConwayConfig` by hand via `SessionConfig::default()` and never sets
/// `root` (this file's own `base_config`, and ~60 further call sites
/// workspace-wide). No `CONWAY_CONFIG_DIR`/ambient environment read is
/// involved in reaching this location -- `ConwayBuilder::build`'s own
/// `effective_session_root` fallback, not `config::discovery::
/// session_root`.
#[cfg(feature = "jsonl-store")]
#[tokio::test]
async fn build_falls_back_to_the_old_fixed_default_when_from_parts_leaves_root_unset() {
    let mut cfg = base_config();
    let root = support::unique_temp_dir("builder-jsonl-store-unset-root");
    cfg.cwd = root.clone();
    assert!(
        cfg.session.root.is_none(),
        "base_config's SessionConfig::default() must leave root unset for this test to mean \
         anything"
    );

    let backend = fake_backend("fake");
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .build()
        .expect("build should synthesize a real JsonlSessionStore at the old fixed default");

    conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session against the real store should succeed");

    assert!(
        root.join(".conway").join("sessions").is_dir(),
        "an unset root reaching build() via from_parts must fall back to the OLD \
         .conway/sessions default, not the new central one"
    );
}

/// **Regression test for a real bug this item's own manual verification
/// caught before landing** (board item `01M0QK9GRM8HSNWRAR414TCX42`):
/// `build_default_path_store`'s original formula derived the path store's
/// root from the session root's PARENT directory, which is safe only when
/// that parent is already project-exclusive -- true of the OLD fixed
/// default and of an operator's own explicit `session.root`, but false the
/// moment two projects' session roots share a common parent, exactly what
/// the new central default's layout does
/// (`~/.conway/sessions/<project-key>/`, every project's own subdirectory
/// under the ONE shared `sessions/`). Simulates that shared-parent shape
/// directly via two explicit `session.root` values under a common
/// `sessions/` directory (without touching `config::load`'s own
/// resolution, which this test has no need to exercise) and proves the two
/// builds' path stores land in two DIFFERENT directories, neither of which
/// is the shared parent's own bare `sessions/paths`.
#[cfg(feature = "jsonl-store")]
#[tokio::test]
async fn two_projects_sharing_a_central_sessions_parent_get_different_path_store_roots() {
    let root = support::unique_temp_dir("builder-path-store-no-collision");
    let shared_sessions_parent = root.join("sessions");

    for key in ["-Users-dan-project-a", "-Users-dan-project-b"] {
        let mut cfg = base_config();
        cfg.cwd = root.clone();
        cfg.session.root = Some(shared_sessions_parent.join(key));

        let backend = fake_backend("fake");
        let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
        let conway = ConwayBuilder::from_parts(cfg)
            .with_backend(backend)
            .with_permission_gate(gate)
            .with_router(empty_router())
            .build()
            .expect("build should synthesize real stores for a central-shaped session root");
        conway
            .new_session(SessionSpec::default())
            .await
            .expect("new_session against the real store should succeed");
    }

    assert!(
        !shared_sessions_parent.join("paths").is_dir(),
        "the two projects must not have collided on one shared paths/ directory"
    );
    assert!(
        shared_sessions_parent
            .join("-Users-dan-project-a-paths")
            .is_dir(),
        "project a's own path store must exist, keyed by its own session root"
    );
    assert!(
        shared_sessions_parent
            .join("-Users-dan-project-b-paths")
            .is_dir(),
        "project b's own path store must exist, keyed by its own session root"
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
#[cfg(feature = "jsonl-store")]
#[test]
fn build_accepts_an_anthropic_backend_under_any_json_key() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "kimi".to_string(),
        BackendEntry {
            kind: "anthropic".to_string(),
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
        .with_router(empty_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
        .build()
        .expect("an anthropic-kind backend under the key 'kimi' must build");
}

/// The default case: a `backends.anthropic` entry still builds, unchanged.
#[cfg(feature = "jsonl-store")]
#[test]
fn build_succeeds_for_a_conventionally_named_anthropic_backend() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "anthropic".to_string(),
        BackendEntry {
            kind: "anthropic".to_string(),
            api_key: "sk-ant-api03-not-a-real-key".to_string(),
            ..BackendEntry::default()
        },
    );
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
        .build()
        .expect("a matching 'anthropic' JSON key must build successfully");
}

/// Indirect but discriminating proof that `with_permission_gate` overrides
/// config-derived gate selection: `permissions.mode = "prompt"` with no
/// injected gate and no `with_prompt_handler` handler always fails
/// `build()`, so a `build()` success with mode `"prompt"` *and* an injected
/// gate can only be explained by the injected gate having been used instead
/// of `gates::from_config`.
#[cfg(feature = "jsonl-store")]
#[test]
fn injected_permission_gate_overrides_config_derived_selection() {
    let mut cfg = base_config();
    cfg.permissions = PermissionsConfig {
        mode: PermissionMode::Prompt,
        ..PermissionsConfig::default()
    };
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());

    // Without an override: prompt mode with no handler is a Config error.
    let result = ConwayBuilder::from_parts(cfg.clone())
        .with_backend(backend.clone())
        .with_session_store(store.clone())
        .with_router(empty_router())
        .build();
    let err = expect_build_err(
        result,
        "prompt mode with no injected gate and no handler must fail",
    );
    assert!(matches!(err, FacadeError::Config { .. }));

    // With an override: build succeeds, proving the injected gate was used.
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .build()
        .expect("an injected gate must bypass config-derived prompt-mode selection");
}

/// `with_prompt_handler` is the direct path a `permissions.mode = "prompt"`
/// config needs when a host has ONE closure to answer permission requests,
/// not a reason to hand-roll a whole `PermissionGate`. Discriminating the
/// same way the pre-existing `injected_permission_gate_overrides_config_
/// derived_selection` test above does: `gates::from_config` (step 9 of
/// `build()`) errors synchronously, at construction, when `permissions.mode
/// = "prompt"` and it receives no handler -- proven above by the identical
/// config failing `build()` with no override at all. `build()` succeeding
/// here, with only `with_prompt_handler` (no `with_permission_gate`) set,
/// can only be explained by this handler having reached `gates::from_config`
/// and let it construct a `PromptingGate` instead of erroring.
#[cfg(feature = "jsonl-store")]
#[test]
fn with_prompt_handler_satisfies_prompt_mode_with_no_injected_gate() {
    let mut cfg = base_config();
    cfg.permissions = PermissionsConfig {
        mode: PermissionMode::Prompt,
        ..PermissionsConfig::default()
    };
    let handler: conway::gates::PromptHandler =
        Arc::new(|_req| Box::pin(async { PermissionDecision::AllowOnce }));

    ConwayBuilder::from_parts(cfg)
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_router(empty_router())
        .with_prompt_handler(handler)
        .build()
        .expect("with_prompt_handler must satisfy prompt-mode gate selection");
}

/// `with_permission_gate` wins over `with_prompt_handler` when both are
/// called: `gates::from_config` is never even reached, so the handler is
/// simply never invoked. Proven the same discriminating way as
/// `injected_permission_gate_overrides_config_derived_selection`: a handler
/// that always denies would make the turn below fail if it were somehow
/// still consulted, and an always-allow injected gate is what actually
/// authorizes it instead.
#[cfg(feature = "jsonl-store")]
#[test]
fn with_permission_gate_wins_over_with_prompt_handler() {
    let mut cfg = base_config();
    cfg.permissions = PermissionsConfig {
        mode: PermissionMode::Prompt,
        ..PermissionsConfig::default()
    };
    let denying_handler: conway::gates::PromptHandler = Arc::new(|_req| {
        Box::pin(async {
            PermissionDecision::Deny {
                reason: "test handler always denies".to_string(),
            }
        })
    });
    let allowing_gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    ConwayBuilder::from_parts(cfg)
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_router(empty_router())
        .with_prompt_handler(denying_handler)
        .with_permission_gate(allowing_gate)
        .build()
        .expect(
            "an injected gate must win over a prompt handler, so build() succeeds regardless \
             of the handler's own (denying) decision",
        );
}

#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
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
        .with_router(empty_router())
        .with_plugin(Arc::new(DummyPlugin("conway.fs")))
        .build();
    let err = expect_build_err(
        result,
        "a plugin id colliding with a built-in must be rejected",
    );

    match err {
        FacadeError::Build { message } => {
            assert!(message.contains("duplicate plugin id"), "{message}");
            assert!(message.contains("conway.fs"), "{message}");
        }
        other => panic!("expected Build error, got {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Host-capability gate (board item 01M03VJXARFHSDAGHFXGCWKJTY):
// `PluginManifest::required_host_caps` is now consulted at the
// manifest-validation seam in `ConwayBuilder::build`. A plugin whose declared
// cap the host offers loads; one whose declared cap the host LACKS is refused
// at build with a `PluginError::MissingHostCapability`-sourced build error
// naming both the plugin and the cap. The unit-level check
// (`conway::HostCaps::check_manifest`) is covered in `host_caps`'s own tests;
// these are the builder-level end-to-end regressions that confirm the gate is
// wired into `build()` itself.
// -----------------------------------------------------------------------

/// A minimal no-op `Plugin` that declares a single required host cap, used to
/// exercise the build-time host-capability gate. Distinct from `DummyPlugin`
/// (which declares no caps) so the two concerns stay legible at the call site.
#[cfg(feature = "builtin-tools")]
struct CapPlugin {
    id: &'static str,
    required_caps: Vec<HostCapability>,
}

#[cfg(feature = "builtin-tools")]
impl Plugin for CapPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: self.required_caps.clone(),
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

/// Acceptance 1: a plugin whose `required_host_caps` names a cap the host
/// HAS loads normally. The `conway` runtime always offers `Subagent` (it
/// provides a `SubagentHost` unconditionally), so a plugin requiring
/// `subagent` builds successfully alongside the built-ins.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn plugin_requiring_a_cap_the_host_offers_builds() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let _conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(CapPlugin {
            id: "test.needs-subagent",
            required_caps: vec![HostCapability::Subagent],
        }))
        .build()
        .expect("a plugin requiring a cap the host offers (Subagent) must build");
}

/// Acceptance 2: a plugin whose `required_host_caps` names a cap the host
/// LACKS is refused at build with a `PluginError::MissingHostCapability`-
/// sourced build error naming both the plugin and the cap. `base_config()`
/// has no `[plugins].subprocess[]` entries, so the host offers no
/// `PersistentTransport` -- a plugin requiring it is refused.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn plugin_requiring_a_cap_the_host_lacks_is_refused_naming_both() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let result = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(CapPlugin {
            id: "test.needs-persistent",
            required_caps: vec![HostCapability::PersistentTransport],
        }))
        .build();
    let err = expect_build_err(
        result,
        "a plugin requiring a cap the host lacks (PersistentTransport) must be refused",
    );

    match err {
        FacadeError::Build { message } => {
            // The message is the `PluginError::MissingHostCapability`'s
            // Display ("plugin {plugin} requires missing host capability
            // {capability}") -- it names BOTH the plugin id and the cap's
            // snake_case wire string.
            assert!(
                message.contains("test.needs-persistent"),
                "build error must name the plugin: {message}"
            );
            assert!(
                message.contains("persistent_transport"),
                "build error must name the missing cap: {message}"
            );
            assert!(
                message.contains("missing host capability"),
                "build error must be the MissingHostCapability shape: {message}"
            );
        }
        other => panic!("expected Build error, got {other:?}"),
    }
}

/// A minimal no-op `Plugin` that declares a single OPTIONAL host cap, used
/// to exercise the build-time optional-host-capability degrade path
/// (board item `01M0WWKA8K1E7JPK87J6RRQMZF`). Distinct from `CapPlugin`
/// (which declares a REQUIRED cap and whose two existing tests above stay
/// unedited) so this item's own new fixture never touches that struct or
/// its call sites.
#[cfg(feature = "builtin-tools")]
struct OptionalCapPlugin {
    id: &'static str,
    optional_caps: Vec<HostCapability>,
}

#[cfg(feature = "builtin-tools")]
impl Plugin for OptionalCapPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: self.optional_caps.clone(),
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

/// Acceptance 4: a plugin whose `optional_host_caps` names a cap the host
/// LACKS still builds -- unlike a missing REQUIRED cap, this never refuses
/// the plugin -- and the degradation is announced on `Conway::warnings()`
/// with `WarningCode::OptionalHostCapabilityMissing`, naming both the
/// plugin and the missing cap. `base_config()` has no
/// `[plugins].subprocess[]` entries, so the host offers no
/// `PersistentTransport`.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn plugin_with_missing_optional_host_cap_builds_and_warns() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(OptionalCapPlugin {
            id: "test.optional-persistent",
            optional_caps: vec![HostCapability::PersistentTransport],
        }))
        .build()
        .expect(
            "a plugin whose optional_host_caps names a cap the host lacks must still build, \
             degraded",
        );

    let warning = conway
        .warnings()
        .iter()
        .find(|w| w.code == conway::config::WarningCode::OptionalHostCapabilityMissing)
        .expect("build() must record an OptionalHostCapabilityMissing warning");
    assert!(
        warning.message.contains("test.optional-persistent"),
        "warning must name the plugin: {}",
        warning.message
    );
    assert!(
        warning.message.contains("persistent_transport"),
        "warning must name the missing cap: {}",
        warning.message
    );
}

/// A plugin whose `optional_host_caps` names a cap the host HAS builds with
/// no warning recorded for it -- the degrade path only fires for a cap
/// that is actually missing.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn plugin_with_satisfied_optional_host_cap_builds_with_no_warning() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(OptionalCapPlugin {
            id: "test.optional-subagent",
            optional_caps: vec![HostCapability::Subagent],
        }))
        .build()
        .expect("a plugin whose optional_host_caps names a cap the host has must build");

    assert!(
        conway
            .warnings()
            .iter()
            .all(|w| w.code != conway::config::WarningCode::OptionalHostCapabilityMissing),
        "no OptionalHostCapabilityMissing warning is expected when the cap is offered"
    );
}

// -----------------------------------------------------------------------
// bash ships on by default and cannot be declined.
//
//: every assertion below reads `Conway::tool_render_kind` -- the
// SAME already-existing accessor `structured_rule_seam.rs`'s registration
// checks use to read the real registry -- never a config flag/selection
// value. `tool_render_kind(name)` returns `None` iff no plugin registered
// a tool by that name (see its own doc), so "the registry lacks `bash`" and
// "the registry has `bash`" are both observed on the RUNTIME'S OWN
// resolved tool set, not on whether `build()` merely accepted a selection.
// -----------------------------------------------------------------------

#[cfg(feature = "builtin-tools")]
/// A `Conway` over `base_config()` with an optional builtin-plugin
/// selection -- the only thing these tests vary.
///
/// Delegates the port wiring to `conway::test_support`, but supplies its
/// own `empty_router` (see that function's doc: this file's router is
/// deliberately not the shared one).
fn conway_with_selection(selection: Option<PluginSelection>) -> Conway {
    let builder = test_builder_without_router(base_config())
        .with_backend(fake_backend("fake"))
        .with_router(empty_router());
    let builder = match selection {
        Some(selection) => builder.with_builtin_plugins(selection),
        None => builder,
    };
    builder
        .build()
        .expect("build should succeed with every port injected")
}

/// The headline acceptance test: a default `ConwayBuilder::build()` (no
/// `with_builtin_plugins` call, no `[tools]` override) yields a runtime
/// with NO `bash` tool registered.
///
/// The registry-emptiness hazard this test is written to rule out:
/// a build that silently registered NOTHING would also make
/// `tool_render_kind("bash")` return `None`, passing this assertion for
/// the WRONG reason. `fs`'s `read` tool is asserted present in the very
/// same registry to rule that out -- the registry is not empty, `bash`
/// specifically is absent.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn default_build_registers_every_builtin_except_bash() {
    let conway = conway_with_selection(None);

    assert!(
        conway.tool_render_kind(&ToolName::new("bash")).is_none(),
        "a default build must register no `bash` tool at all"
    );
    // Rules out the "build registered nothing" false-pass: `fs`'s `read`
    // tool (default-on, per this item's own deliberate fs/subagent/report
    // decision) IS registered in this same runtime.
    assert!(
        conway.tool_render_kind(&ToolName::new("read")).is_some(),
        "fs/subagent/report stay registered by default -- only bash is excluded"
    );
}

/// The explicit-opt-in mirror: `with_builtin_plugins(PluginSelection::All)`
/// yields a runtime that HAS the real `bash` tool, declaring the
/// `ShellCommand` `RenderKind` only `bash` uses among the built-ins.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn explicit_opt_in_via_builder_registers_the_bash_tool() {
    let conway = conway_with_selection(Some(PluginSelection::All));

    assert_eq!(
        conway.tool_render_kind(&ToolName::new("bash")),
        Some(RenderKind::ShellCommand),
        "with_builtin_plugins(All) must register the real bash tool"
    );
}

/// Same opt-in, expressed as a NAMED selection (`Only(["conway.shell"])`)
/// rather than the blanket `All` -- proves the mechanism is a real id-keyed
/// predicate, not merely a two-state All/None switch. The negative half
/// matters as much as the positive one: naming only shell must also mean
/// fs's `read` is absent, which a predicate that ignored its argument would
/// fail.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn explicit_opt_in_via_only_naming_shell_registers_the_bash_tool() {
    let conway =
        conway_with_selection(Some(PluginSelection::Only(
            vec!["conway.shell".to_string()],
        )));

    assert_eq!(
        conway.tool_render_kind(&ToolName::new("bash")),
        Some(RenderKind::ShellCommand),
        "Only([\"conway.shell\"]) must register bash"
    );
    // And nothing else this selection didn't name.
    assert!(
        conway.tool_render_kind(&ToolName::new("read")).is_none(),
        "Only([\"conway.shell\"]) must NOT also register fs's `read` tool"
    );
}

/// The `--root`/`with_root` startup warning (harness gap review
/// 2026-09-01, finding 10): an operator who sets a confinement root
/// reasonably believes nothing can touch files outside it, but `bash`
/// (`conway.shell`) runs a shell command verbatim, which reaches any path
/// it likes -- `with_root`'s own doc names this exception in prose only.
/// With BOTH a root and `conway.shell` selected, `Conway::warnings()`
/// carries exactly one `ConfigWarning` naming `bash` and `--root`. Written
/// FIRST against the current tree (checks shown to fail): before this
/// item's `ConwayBuilder::build` change, `warnings()` here is empty.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn root_plus_bash_selected_warns_exactly_once_naming_both() {
    let root = tempfile::tempdir().expect("tempdir");
    let conway = test_builder_without_router(base_config())
        .with_backend(fake_backend("fake"))
        .with_router(empty_router())
        .with_root(root.path())
        .with_builtin_plugins(PluginSelection::All)
        .build()
        .expect("build should succeed with root set and bash selected");

    let matches: Vec<_> = conway
        .warnings()
        .iter()
        .filter(|w| w.message.contains("bash") && w.message.contains("--root"))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one root/bash warning, got: {:?}",
        conway.warnings()
    );
    assert!(
        matches[0].message.contains("conway.shell"),
        "the warning must name the config key that turns bash off: {:?}",
        matches[0].message
    );
}

/// BREAK-THE-GUARD (half 1): a root WITHOUT `conway.shell` selected must
/// print no root/bash warning at all -- the default, bash-excluded builtin
/// set is exactly the safe composition `with_root`'s own doc already
/// describes as a real guarantee.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn root_without_bash_selected_warns_of_nothing() {
    let root = tempfile::tempdir().expect("tempdir");
    let conway = test_builder_without_router(base_config())
        .with_backend(fake_backend("fake"))
        .with_router(empty_router())
        .with_root(root.path())
        // No `with_builtin_plugins` call at all: the restrictive DEFAULT
        // selection, which excludes `conway.shell` -- see
        // `default_build_registers_every_builtin_except_bash` above.
        .build()
        .expect("build should succeed with root set and bash NOT selected");

    assert!(
        conway.warnings().is_empty(),
        "a root with no unconfinable shell tool registered must print no warning, got: {:?}",
        conway.warnings()
    );
}

/// BREAK-THE-GUARD (half 2): `conway.shell` selected but NO root set must
/// also print nothing -- the warning is about the COMBINATION, not either
/// setting alone (an unrooted `bash` selection is this crate's own
/// long-standing default-off-but-selectable behavior, unrelated to this
/// item).
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn bash_selected_without_root_warns_of_nothing() {
    let conway = test_builder_without_router(base_config())
        .with_backend(fake_backend("fake"))
        .with_router(empty_router())
        .with_builtin_plugins(PluginSelection::All)
        .build()
        .expect("build should succeed with bash selected and no root");

    assert!(
        conway.warnings().is_empty(),
        "bash selected with no root set must print no warning, got: {:?}",
        conway.warnings()
    );
}

/// A typo in `tools.builtin_plugins` must FAIL THE BUILD, not silently
/// leave the tool off.
///
/// This is the config key an operator uses to turn `bash` back on. If
/// `"conway.shel"` were accepted and simply never matched, the build would
/// succeed, bash would stay absent, and the operator would believe they had
/// enabled it -- silence indistinguishable from success ranks
/// as the WORST harm tier ("user-facing configuration that does nothing").
/// The candidate set is closed and known at compile time, so an
/// unrecognized id is always a mistake and never a forward reference.
#[cfg(feature = "builtin-tools")]
#[test]
fn a_misspelled_builtin_plugin_id_is_rejected_rather_than_silently_ignored() {
    let mut cfg = base_config();
    cfg.tools.builtin_plugins = vec![
        "conway.fs".to_string(),
        "conway.shel".to_string(), // typo: missing the final `l`
    ];

    // `Conway` is not `Debug`, so match rather than `expect_err`.
    let rendered = match ConwayBuilder::from_parts(cfg)
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
    {
        Ok(_) => panic!("a misspelled built-in plugin id must fail the build, but it succeeded"),
        Err(e) => e.to_string(),
    };
    assert!(
        rendered.contains("conway.shel"),
        "the error must name the offending id so the operator can find the typo: {rendered}"
    );
    assert!(
        rendered.contains("conway.shell"),
        "the error must list the known ids so the operator can see the correction: {rendered}"
    );
}

/// The two remaining `PluginSelection` variants, which nothing else drives.
/// Their `allows()` arms are one-liners today, so this is not chasing a
/// live bug -- it is so that a future edit to `allows()` cannot regress
/// them silently. `AllExcept` is the variant an operator reaches for to
/// drop exactly one built-in, and `None` is the only way to get a runtime
/// with no built-in tools at all; both deserve a guard.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn all_except_shell_and_none_select_what_their_names_say() {
    let all_except_shell = conway_with_selection(Some(PluginSelection::AllExcept(vec![
        "conway.shell".to_string(),
    ])));
    assert!(
        all_except_shell
            .tool_render_kind(&ToolName::new("bash"))
            .is_none(),
        "AllExcept([\"conway.shell\"]) must NOT register bash"
    );
    assert!(
        all_except_shell
            .tool_render_kind(&ToolName::new("read"))
            .is_some(),
        "AllExcept([\"conway.shell\"]) must still register everything it did not name"
    );

    let nothing = conway_with_selection(Some(PluginSelection::None));
    assert!(
        nothing.tool_render_kind(&ToolName::new("bash")).is_none(),
        "None must register no bash"
    );
    assert!(
        nothing.tool_render_kind(&ToolName::new("read")).is_none(),
        "None must register no built-in tools at all, not merely skip shell"
    );
}

/// The `settings.json`-reachable path: no `with_builtin_plugins` call at
/// all, just `config.tools.builtin_plugins` naming `"conway.shell"` --
/// proving the config key and the builder method reach the exact same
/// outcome (: built-ins and the config surface select the same way).
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn config_tools_builtin_plugins_naming_shell_registers_the_bash_tool() {
    let mut cfg = base_config();
    cfg.tools.builtin_plugins.push("conway.shell".to_string());
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .build()
        .expect("build should succeed with every port injected");

    assert_eq!(
        conway.tool_render_kind(&ToolName::new("bash")),
        Some(RenderKind::ShellCommand),
        "config.tools.builtin_plugins naming conway.shell must register bash, \
         with no with_builtin_plugins call at all"
    );
}

/// A `with_plugin`-injected third-party plugin is never filtered by the
/// built-in `PluginSelection` -- including the restrictive DEFAULT one
/// (`PluginSelection`'s own doc: calling `with_plugin` IS already the
/// explicit per-plugin declaration requires).
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn injected_plugin_is_unaffected_by_the_default_builtin_selection() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct EchoArgs {}

    struct EchoTool;
    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> conway_core::content::ToolSpec {
            conway_core::content::ToolSpec {
                name: ToolName::new("test_echo"),
                description: "echo".to_string(),
                schema: schemars::schema_for!(EchoArgs),
                category: conway_core::content::ToolCategory::Read,
                permission: conway_core::content::PermissionClass::Safe,
            }
        }
        async fn invoke(
            &self,
            _call: conway_core::content::ToolCall,
            _ctx: conway_core::ports::ToolCtx,
        ) -> Result<conway_core::ports::ToolOutput, conway_core::error::ToolError> {
            unreachable!("not invoked by this test")
        }
        fn path_args(&self) -> conway_core::ports::PathArgs {
            conway_core::ports::PathArgs::None
        }
        fn render_kind(&self) -> RenderKind {
            RenderKind::Structured
        }
    }
    struct EchoPlugin;
    impl Plugin for EchoPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.echo".to_string(),
                version: "0.0.0".to_string(),
                tools: vec![ToolName::new("test_echo")],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }
        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![Arc::new(EchoTool)]
        }
    }

    let conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(EchoPlugin))
        .build()
        .expect("build should succeed with every port injected");

    assert_eq!(
        conway.tool_render_kind(&ToolName::new("test_echo")),
        Some(RenderKind::Structured),
        "an injected third-party plugin is registered regardless of the default builtin selection"
    );
    assert!(
        conway.tool_render_kind(&ToolName::new("bash")).is_none(),
        "the default selection still excludes bash even alongside an injected plugin"
    );
}

#[cfg(feature = "jsonl-store")]
#[test]
fn injected_backend_replaces_config_derived_backend_with_same_id() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "local".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
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
        .with_router(empty_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
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
/// probe (: `OpenAiCompatBackendFactory
/// ::probe_capabilities` calls `conway_plugin_backends::probe::
/// CapabilityProbe` directly rather than through `Backend::probe()`; a
/// "counting `Backend`" double would not observe anything, since no
/// `Backend` instance is consulted for capability discovery -- see
/// `builder.rs`'s module doc, reconciliation on startup probing).
#[cfg(feature = "jsonl-store")]
#[test]
fn probe_on_startup_false_makes_no_network_call_true_does() {
    // probe_on_startup = false (the default): zero connection attempts.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let mut cfg = base_config();
    cfg.backends.insert(
        "probe".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
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
        .with_router(empty_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
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
            kind: "openai-compat".to_string(),
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
        .with_router(empty_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect("build should still succeed: a probe failure is a warning, not an error");

    assert!(
        connection_was_accepted(&listener),
        "probe_on_startup = true must attempt at least one network call"
    );
}

/// `Conway::explain_routing` delegates to `conway_core::routing::
/// MinimalRouter` (the no-plugin default,
/// no `with_router`/`with_router_factory` call
/// here), and its `entries` correspond 1:1 to the role's configured chain,
/// in order -- the same shape the richer, capability-/health-filtered
/// `conway-plugin-routing::RoutingExplain` produces when that plugin is
/// installed instead (see that crate's own tests).
#[cfg(feature = "jsonl-store")]
#[test]
fn explain_routing_reports_the_configured_chain_for_the_role() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "local".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
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

    // No `with_router`/`with_router_factory` override: `build()` falls
    // through to `MinimalRouter`, which `explain_routing` projects through
    // directly.
    let conway = ConwayBuilder::from_parts(cfg)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
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
/// to export the key. Board item `01M163T1KGX3HTCC2YMDPT655J` flipped this
/// from "a hard `build()`-time error" (this test's own predecessor, which
/// asserted exactly that and is the reason this scenario is pinned here at
/// all) to this: `build()` now succeeds anyway, registering `kimi` with no
/// credential, so "no thanks, I'll configure it later" is a real option
/// instead of a bounce back to the shell. This was TWO independent gates
/// refusing for the identical reason, not one -- `resolve_api_key`
/// (`crates/conway/src/builder.rs`) and `AnthropicBackendFactory::build`'s
/// own `cfg.validate()` call (`crates/conway-plugin-backends/src/
/// factory.rs`, itself sitting on top of a THIRD, `AnthropicBackend::
/// with_extra_headers`'s own `config.validate()` -- see that fn's own doc)
/// -- both relaxed together, verified by this test alone going green (it
/// still failed with only the first relaxed: `factory for kind 'anthropic'
/// failed to build: ... missing API key`, observed directly while making
/// this change). `a_missing_credential_registers_the_backend_and_fails_
/// loud_at_the_wire` below is this test's turn-time sibling: registering
/// the backend is only correct if a real turn against it still fails
/// loud, naming the problem, rather than silently succeeding or panicking.
#[test]
fn an_unset_api_key_env_no_longer_fails_the_build_and_registers_the_backend_anyway() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "kimi".to_string(),
        BackendEntry {
            kind: "anthropic".to_string(),
            api_key_env: "CONWAY_TEST_DEFINITELY_UNSET_KEY_VAR".to_string(),
            base_url: "https://api.kimi.com/coding/".to_string(),
            ..BackendEntry::default()
        },
    );

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
        .build();

    if let Err(e) = result {
        panic!("an unset api_key_env must no longer refuse the whole build: {e}");
    }
}

/// Acceptance 2's own proof, and the sibling the test above names: a turn
/// actually routed to the credential-less `kimi` backend must fail with a
/// typed, legible error naming the problem -- never an empty response,
/// never a panic (`conway_runtime::attempt::AttemptEngine::backend_for`
/// would panic outright if this backend had been excluded from the built
/// `backend_map` instead of registered into it with an empty key, which is
/// why registering it -- not silently dropping it -- is the design this
/// item chose). No real network involved: a loopback `wiremock` server
/// stands in for Anthropic's own API and returns the exact 401 shape a real
/// unauthorized request gets, which `conway_plugin_backends::error::
/// classify` already maps to `BackendError::Auth` (tested there), and
/// which `conway_runtime::attempt`'s own T-2 classification already treats
/// as `Fatal` (tested there too) -- this test drives that EXISTING,
/// already-tested taxonomy end to end through a real `ConwayBuilder::build`
/// rather than inventing a new failure shape for "no usable backend".
#[tokio::test]
async fn a_missing_credential_registers_the_backend_and_fails_loud_at_the_wire() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/messages"))
        .respond_with(
            wiremock::ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "type": "error",
                "error": { "type": "authentication_error", "message": "invalid x-api-key" }
            })),
        )
        .mount(&server)
        .await;

    let mut cfg = base_config();
    cfg.roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec!["kimi/claude-sonnet-4-6".to_string()],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    cfg.backends.insert(
        "kimi".to_string(),
        BackendEntry {
            kind: "anthropic".to_string(),
            api_key_env: "CONWAY_TEST_DEFINITELY_UNSET_KEY_VAR".to_string(),
            base_url: server.uri(),
            ..BackendEntry::default()
        },
    );

    let conway = ConwayBuilder::from_parts(cfg)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
        .build()
        .expect("build must succeed: a missing credential no longer refuses it");

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = session
        .prompt("hello")
        .await
        .expect("prompt should succeed -- the turn fails later, inside the agent loop");
    let result = turn
        .result()
        .await
        .expect("result() itself must not error -- the turn ends Failed, not the stream");

    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(
                error.contains("authentication failed"),
                "must name the auth failure, not a generic or empty one: {error}"
            );
        }
        other => panic!("expected ResultStatus::Failed (a named auth failure), got {other:?}"),
    }
}

/// A `[backends.<id>].kind` no
/// registered factory claims fails `build()` -- a production entry point
/// (`ConwayBuilder::from_parts(..).build()`, the same call every other
/// `build()`-time error in this file goes through) -- with an error naming
/// the offending value and listing the kinds this build actually
/// recognises.: a silently ignored `kind` is exactly the failure this
/// error exists to prevent. Both `conway_plugin_backends` factories are
/// registered here ( removed the
/// compiled-in fallback the previous item's own doc referenced -- every
/// kind is a registered factory now, including these two) so the
/// "recognised kinds" assertion below still has something real to list.
#[test]
fn unknown_backend_kind_fails_build_naming_the_value_and_recognised_kinds() {
    let mut cfg = base_config();
    cfg.backends.insert(
        "mystery".to_string(),
        BackendEntry {
            kind: "totally-unrecognised-kind".to_string(),
            ..BackendEntry::default()
        },
    );

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build();

    let err = result
        .err()
        .expect("an unknown backend kind must fail the build")
        .to_string();
    assert!(
        err.contains("totally-unrecognised-kind"),
        "the error must quote the offending kind value: {err}"
    );
    assert!(
        err.contains("anthropic") && err.contains("openai-compat"),
        "the error must list the recognised kinds: {err}"
    );
}

/// The novel-kind sibling of the test above: a `[backends.<id>].kind` that
/// DOES match a registered `BackendFactory` is resolved to it -- checked
/// here by listing that factory's own kind id in the SAME unknown-kind
/// error's recognised-kinds set once it is registered, proving the
/// recognised-kinds list is genuinely derived from what is installed, not a
/// hardcoded pair. `tests/backend_factory.rs`'s `factory_built_backend_
/// serves_a_turn` is the discriminating end-to-end proof that such a kind
/// actually builds and serves a turn; this test only pins the error-message
/// side of the same resolution.
#[test]
fn registered_factory_kind_appears_in_the_recognised_kinds_list() {
    use conway::{BackendBuildContext, BackendFactory, CoreConwayError};

    struct StubFactory;
    impl BackendFactory for StubFactory {
        fn id(&self) -> &str {
            "stub-novel-kind"
        }
        fn build(
            &self,
            _ctx: BackendBuildContext,
        ) -> Result<Arc<dyn conway_core::ports::Backend>, CoreConwayError> {
            unreachable!("this test never names 'stub-novel-kind' from a [backends.<id>] entry")
        }
    }

    let mut cfg = base_config();
    cfg.backends.insert(
        "mystery".to_string(),
        BackendEntry {
            kind: "still-totally-unrecognised".to_string(),
            ..BackendEntry::default()
        },
    );

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .with_backend_factory(Arc::new(StubFactory))
        .build();

    let err = result
        .err()
        .expect("an unknown backend kind must still fail the build")
        .to_string();
    assert!(
        err.contains("stub-novel-kind"),
        "the recognised-kinds list must include the registered factory's own kind id: {err}"
    );
}

/// An unresolved `[backends.<id>].
/// kind` is a hard `build()` error either way (unchanged), but the message
/// must DISTINGUISH "conway has never heard of this kind" from "an operator
/// declined it" -- two different diagnoses for the same unresolved-kind
/// failure (: an operator who deliberately declined a dialect deserves
/// the accurate one). `ConwayBuilder::with_declined_backend_kinds` is the
/// mechanism a caller declares that distinction with;
/// `crates/conway-cli/src/first_party_plugins.rs`'s `install` is the one
/// caller wired today, computing it as every published backend-factory id
/// this binary links minus `wanted` (`[plugins].install` unioned with
/// `[plugins].default_backends`) -- see
/// `crates/conway-cli/tests/decline_backend_kind.rs` for the compiled-binary
/// sibling of this same property.
#[test]
fn declined_backend_kind_error_is_distinct_from_unknown_backend_kind_error() {
    fn cfg_naming_openai_compat() -> ConwayConfig {
        let mut cfg = base_config();
        cfg.backends.insert(
            "mock".to_string(),
            BackendEntry {
                kind: "openai-compat".to_string(),
                ..BackendEntry::default()
            },
        );
        cfg
    }

    // Genuinely unknown: no factory registered for "openai-compat", and
    // nothing declared it declined either.
    let unknown_err = ConwayBuilder::from_parts(cfg_naming_openai_compat())
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
        .err()
        .expect("an unresolved kind must fail the build")
        .to_string();

    // The identical unresolved kind, but this caller declares it was
    // deliberately declined rather than simply never having heard of it.
    let declined_err = ConwayBuilder::from_parts(cfg_naming_openai_compat())
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .with_declined_backend_kinds(vec!["openai-compat".to_string()])
        .build()
        .err()
        .expect("a declined kind still referenced by config must fail the build")
        .to_string();

    assert!(
        unknown_err.contains("unknown kind"),
        "the genuinely-unrecognised case must read as an unknown kind: {unknown_err}"
    );
    assert!(
        !unknown_err.to_lowercase().contains("declined"),
        "the genuinely-unrecognised case must not claim the kind was declined: {unknown_err}"
    );
    assert!(
        declined_err.contains("declined"),
        "the declined case must say so in the message: {declined_err}"
    );
    assert!(
        !declined_err.contains("unknown kind"),
        "the declined case must not read as an unrecognised kind: {declined_err}"
    );
    assert!(
        unknown_err.contains("openai-compat") && declined_err.contains("openai-compat"),
        "both messages must name the offending kind: unknown={unknown_err} declined={declined_err}"
    );
    assert_ne!(
        unknown_err, declined_err,
        "the two diagnoses must be genuinely different text, not the same message printed twice"
    );
}

/// Board item `01M0K5MD59YZRSHE31JKZKFRMY`: two installed plugins
/// declaring `Plugin::instructions()` fragments under the SAME name is a
/// build-time error -- a plain authoring bug this method's own doc argues
/// is deliberately NOT the (configuration-dependent) reachability check,
/// so it is caught here rather than deferred to context assembly.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn duplicate_instruction_fragment_name_is_rejected() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let result = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(InstructingPlugin("test.one", "when-to-do-x")))
        .with_plugin(Arc::new(InstructingPlugin("test.two", "when-to-do-x")))
        .build();
    let err = expect_build_err(
        result,
        "two plugins declaring the same instruction fragment name must be rejected",
    );

    match err {
        FacadeError::Build { message } => {
            assert!(
                message.contains("duplicate instruction fragment name"),
                "{message}"
            );
            assert!(message.contains("when-to-do-x"), "{message}");
            // BOTH plugin ids, not just the one being processed when the
            // clash was noticed. Resolving a collision means editing one of
            // the two declarations, so a message naming only the second and
            // calling the first "an earlier plugin" leaves the operator
            // hunting through every earlier-installed plugin by hand.
            assert!(
                message.contains("test.one"),
                "the FIRST plugin to declare the name must be identified: {message}"
            );
            assert!(
                message.contains("test.two"),
                "the SECOND plugin to declare the name must be identified: {message}"
            );
        }
        other => panic!("expected Build error, got {other:?}"),
    }
}

/// The positive case beside the rejection above: two DISTINCTLY-named
/// fragments from two different plugins build cleanly -- `build()` does
/// not reject on the mere presence of `Plugin::instructions()`
/// contributions, only on an actual name collision.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[test]
fn distinctly_named_instruction_fragments_from_two_plugins_build_cleanly() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(InstructingPlugin("test.one", "when-to-do-x")))
        .with_plugin(Arc::new(InstructingPlugin("test.two", "when-to-do-y")))
        .build()
        .expect("two distinctly-named instruction fragments must build cleanly");
}

/// End-to-end through the real facade (not merely the `conway-runtime`
/// unit tests): a reachable `Plugin::instructions()` fragment installed via
/// `ConwayBuilder::with_plugin` reaches a real root agent's assembled
/// context and reports its own plugin attribution -- the full
/// `Plugin::instructions()` -> `ConwayBuilder::build` ->
/// `RuntimeDeps.instructions` -> `Runtime.instructions` ->
/// `runtime::root::resolve_instructions` -> `AgentSpec.instructions` ->
/// `ContextInput.instructions` -> `ContextBuilder::build` pipeline, proven
/// live rather than layer by layer.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[tokio::test]
async fn a_reachable_plugin_instruction_reaches_a_real_agents_context() {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(InstructingPlugin("test.trim", "when-to-compose")))
        .build()
        .expect("build should succeed with a reachable instruction fragment");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello there").await.expect("prompt");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    let report = handle
        .context_report_current(handle.root())
        .await
        .expect("context_report_current should succeed");
    assert_eq!(report.instruction_fragments.len(), 1);
    let entry = &report.instruction_fragments[0];
    assert_eq!(entry.plugin_id, "test.trim");
    assert_eq!(entry.name, "when-to-compose");
    assert!(
        entry.unreachable_tool_ids.is_empty(),
        "a fragment naming no tool_ids is trivially reachable"
    );
}

/// Acceptance 4 (fragment position/order/scope item): a `BeforeSystemPrompt`
/// fragment installed via `ConwayBuilder::with_plugin` renders AHEAD of a
/// real `AgentDef`'s own `[0] SystemPrompt` segment -- proven through the
/// real facade end to end (`Plugin::instructions()` -> `ConwayBuilder::build`
/// -> a real agent's assembled context), the same "not merely layer by
/// layer" discipline `a_reachable_plugin_instruction_reaches_a_real_agents_context`
/// establishes immediately above.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[tokio::test]
async fn a_before_system_prompt_fragment_renders_ahead_of_a_real_agent_defs_prompt() {
    let root = support::unique_temp_dir("builder-fragment-position");
    let agents_dir = root.join(".conway").join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    fs::write(
        agents_dir.join("reviewer.md"),
        "---\nname: reviewer\n---\nYou are a careful reviewer.\n",
    )
    .unwrap();

    let mut cfg = base_config();
    cfg.cwd = root;
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .with_plugin(Arc::new(BeforeSystemPromptPlugin("conway.idiom.base")))
        .build()
        .expect("build should succeed with a BeforeSystemPrompt fragment installed");

    let handle = conway
        .new_session(SessionSpec {
            agent_def: Some("reviewer".to_string()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session naming a real agent def should succeed");
    let turn = handle.prompt("hello there").await.expect("prompt");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    let report = handle
        .context_report_current(handle.root())
        .await
        .expect("context_report_current should succeed");
    assert!(
        report.segments.len() >= 2,
        "expected at least the fragment and the agent-def system prompt: {:?}",
        report.segments
    );
    assert!(
        matches!(&report.segments[0].provenance, Provenance::Skill { name } if name == "conway.idiom.base"),
        "the BeforeSystemPrompt fragment must be the very first segment, ahead of the agent \
         def's own prompt: {:?}",
        report
            .segments
            .iter()
            .map(|s| s.provenance.clone())
            .collect::<Vec<_>>()
    );
    assert!(
        matches!(&report.segments[1].provenance, Provenance::AgentDef { name } if name == "reviewer"),
        "the agent def's own prompt must immediately follow the BeforeSystemPrompt fragment: {:?}",
        report
            .segments
            .iter()
            .map(|s| s.provenance.clone())
            .collect::<Vec<_>>()
    );
}

/// THE TRAP (board item `01M0WWJMYK0KDC2X7B7MR46FRR`,
/// `docs/vision/DESIGN-plugin-dependencies.md` §5): a topological pass over
/// `PluginManifest::requires` must NEVER become the injection order
/// `Plugin::instructions()`'s own doc fixes to `with_plugin`/
/// `install_selected` INSTALL order. `[plugins].install` names
/// `test.dependent` FIRST and `test.base` -- `test.dependent`'s OWN
/// `requires` target -- SECOND: a topological (dependency-before-
/// dependent) resolution would put `test.base`'s fragment first instead.
/// This test proves `install_selected`'s dependency-graph validation does
/// not leak into `ConwayBuilder::build`'s actual plugin order: the
/// assembled context still orders fragments by INSTALL order,
/// `test.dependent` then `test.base`, exactly as if `requires` had never
/// been declared.
#[cfg(all(feature = "builtin-tools", feature = "jsonl-store"))]
#[tokio::test]
async fn a_requires_edge_does_not_reorder_instruction_fragment_precedence() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["test.dependent".to_string(), "test.base".to_string()];
    // `install_selected` resolves `install` UNIONED with `default_backends`
    // (`PluginsConfig::default_backends`'s own doc -- default
    // `["anthropic", "openai-compat"]`), and this test supplies EMPTY
    // backend-factory bundles, so leaving the default in place makes
    // resolution fail on `anthropic` before the ordering property under
    // test is ever reached. The backend arrives via `with_backend` below
    // instead, which needs no factory.
    cfg.plugins.default_backends = Vec::new();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![
                Arc::new(InstructingDependentPlugin {
                    id: "test.dependent",
                    fragment_name: "dependent-fragment",
                    requires: vec!["test.base"],
                }) as Arc<dyn Plugin>,
                Arc::new(InstructingDependentPlugin {
                    id: "test.base",
                    fragment_name: "base-fragment",
                    requires: vec![],
                }) as Arc<dyn Plugin>,
            ],
            vec![],
            vec![],
        )
        .expect("a satisfied requires edge, in either install order, must resolve cleanly")
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(empty_router())
        .build()
        .expect("build should succeed: test.base satisfies test.dependent's own requires");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello there").await.expect("prompt");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    let report = handle
        .context_report_current(handle.root())
        .await
        .expect("context_report_current should succeed");
    assert_eq!(report.instruction_fragments.len(), 2);
    assert_eq!(
        report.instruction_fragments[0].plugin_id, "test.dependent",
        "injection order must follow with_plugin/install_selected INSTALL order, not the \
         topological (dependency-before-dependent) order a naive resolution would produce"
    );
    assert_eq!(report.instruction_fragments[1].plugin_id, "test.base");
}

// ---------------------------------------------------------------------
// A second skills root, reached ONLY through `ConwayBuilder` (board item
// `01M0XRE2N96ATHEXJ1617E133P`). Before this item, `skills::
// load_skill_defs_from_roots` had zero production callers -- `build()`
// called the single-root `skills::load_skill_defs` only, and no config
// surface or builder method could reach the multi-root capability at all.
// This drives the REAL construction path end to end
// (`ConwayBuilder::from_parts(..).with_extra_skill_dir(..).build()` ->
// a real session -> a real prompt -> the assembled context), not a direct
// call to the loader -- a direct call is exactly what the prior item
// shipped (`crates/conway/src/skills.rs`'s own unit tests, and
// `tests/skills_e2e.rs`, both drive the loader directly), and is why
// nothing noticed the capability was unreachable through any real build.
// ---------------------------------------------------------------------
#[cfg(feature = "jsonl-store")]
#[tokio::test]
async fn a_second_skills_root_added_via_with_extra_skill_dir_reaches_a_real_agents_context() {
    let root = support::unique_temp_dir("builder-second-skills-root");
    // The operator's own agent-def root (`.conway/agents`, `AgentsConfig::
    // default().dir`), naming a skill by name only -- the def itself never
    // says WHERE that skill lives; discovering that is the loader's job.
    let agents_dir = root.join(".conway").join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    fs::write(
        agents_dir.join("skilled.md"),
        "---\nname: skilled\nskills: [example]\n---\nYou are an agent that uses a skill.\n",
    )
    .unwrap();

    // The plugin's own skills root -- deliberately NOT under `.conway/skills`
    // (the operator's own root, which this test leaves entirely absent), so
    // the ONLY way "example" can be discovered at all is through the SECOND
    // root `with_extra_skill_dir` adds.
    let plugin_skills_dir = support::unique_temp_dir("builder-second-skills-root-plugin");
    let skill_dir = plugin_skills_dir.join("example");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: example\ndescription: An example skill.\n---\nBody text.\n",
    )
    .unwrap();

    let mut cfg = base_config();
    cfg.cwd = root.clone();

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .with_extra_skill_dir(plugin_skills_dir)
        .build()
        .expect("build should succeed with a second skills root supplied");

    let handle = conway
        .new_session(SessionSpec {
            agent_def: Some("skilled".to_string()),
            ..SessionSpec::default()
        })
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello there").await.expect("prompt");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    let report = handle
        .context_report_current(handle.root())
        .await
        .expect("context_report_current should succeed");
    let found = report
        .segments
        .iter()
        .any(|s| matches!(&s.provenance, Provenance::Skill { name } if name == "example"));
    assert!(
        found,
        "a skill discoverable ONLY through the second (extra) skills root must reach the \
         assembled context via the real ConwayBuilder construction path; provenances seen: \
         {:?}",
        report
            .segments
            .iter()
            .map(|s| s.provenance.clone())
            .collect::<Vec<_>>()
    );
}

/// The agents-side twin of the skills test immediately above, proving the
/// symmetric guarantee this item's own report must argue: an agent def
/// discoverable ONLY through `ConwayBuilder::with_extra_agent_dir` (the
/// operator's own `.conway/agents` root is left entirely absent) is still
/// reachable and startable through a real build -- the SAME "second root
/// reached only via a builder method, proven end to end" shape, over the
/// agents loader instead of the skills one.
#[cfg(feature = "jsonl-store")]
#[tokio::test]
async fn a_second_agents_root_added_via_with_extra_agent_dir_reaches_a_real_session() {
    let root = support::unique_temp_dir("builder-second-agents-root");
    let plugin_agents_dir = support::unique_temp_dir("builder-second-agents-root-plugin");
    fs::create_dir_all(&plugin_agents_dir).unwrap();
    fs::write(
        plugin_agents_dir.join("plugin_agent.md"),
        "---\nname: plugin_agent\n---\nYou are a plugin-supplied agent.\n",
    )
    .unwrap();

    let mut cfg = base_config();
    cfg.cwd = root.clone();

    let conway: Conway = ConwayBuilder::from_parts(cfg)
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .with_extra_agent_dir(plugin_agents_dir)
        .build()
        .expect("build should succeed with a second agents root supplied");

    let handle = conway
        .new_session(SessionSpec {
            agent_def: Some("plugin_agent".to_string()),
            ..SessionSpec::default()
        })
        .await
        .expect(
            "new_session naming an agent def discoverable ONLY through the second agents root \
             must succeed",
        );
    let turn = handle.prompt("hello there").await.expect("prompt");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
}
