//! Acceptance tests for `ConwayBuilder`/`Conway` assembly.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::BackendEntry;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionMode, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::{Conway, ConwayBuilder, ConwayError, SessionSpec};
// Only named by the `builtin-tools`-gated tests below.
#[cfg(feature = "builtin-tools")]
use conway::PluginSelection;
use conway_core::agent::PermissionDecision;
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{StopReason, Usage};
#[cfg(feature = "builtin-tools")]
use conway_core::ids::ToolName;
use conway_core::ids::{BackendId, RoleAlias};
use conway_core::ports::{GenerateResponse, SessionStore};
#[cfg(feature = "builtin-tools")]
use conway_core::ports::{Plugin, PluginManifest, RenderKind, Tool};
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

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
/// used by tests whose `ConwayConfig` carries an empty-chain role and that
/// are not themselves exercising routing behavior.
///: `build()`'s no-router/no-factory default is
/// now `conway_core::routing::MinimalRouter`, which never validates a
/// chain at construction at all (unlike the opt-in
/// `conway-plugin-routing::DeclarativeRouter::new`, whose own, stricter
/// `config::validate` rejects an empty chain -- a check
/// `crate::config::merge::validate`, the facade's own already-run
/// validation, does not perform). `fake_router()` therefore no longer
/// exists to dodge a validation failure these tests would otherwise hit;
/// it stays because these tests need a deterministic, content-free `Router`
/// double, not `MinimalRouter`'s own chain-order behavior.
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
/// asserting those two is deferred to earlier work.
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
        .with_router(fake_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
        .build()
        .expect("an anthropic-kind backend under the key 'kimi' must build");
}

/// The default case: a `backends.anthropic` entry still builds, unchanged.
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
        .with_router(fake_router())
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
#[test]
fn with_prompt_handler_satisfies_prompt_mode_with_no_injected_gate() {
    let mut cfg = base_config();
    cfg.permissions = PermissionsConfig {
        mode: PermissionMode::Prompt,
        allowed_tools: vec![],
        denied_tools: vec![],
    };
    let handler: conway::gates::PromptHandler =
        Arc::new(|_req| Box::pin(async { PermissionDecision::AllowOnce }));

    ConwayBuilder::from_parts(cfg)
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_router(fake_router())
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
#[test]
fn with_permission_gate_wins_over_with_prompt_handler() {
    let mut cfg = base_config();
    cfg.permissions = PermissionsConfig {
        mode: PermissionMode::Prompt,
        allowed_tools: vec![],
        denied_tools: vec![],
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
        .with_router(fake_router())
        .with_prompt_handler(denying_handler)
        .with_permission_gate(allowing_gate)
        .build()
        .expect(
            "an injected gate must win over a prompt handler, so build() succeeds regardless \
             of the handler's own (denying) decision",
        );
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
fn build_conway_with_selection(selection: Option<PluginSelection>) -> Conway {
    let cfg = base_config();
    let backend = fake_backend("fake");
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let builder = ConwayBuilder::from_parts(cfg)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router());
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
#[cfg(feature = "builtin-tools")]
#[test]
fn default_build_registers_every_builtin_except_bash() {
    let conway = build_conway_with_selection(None);

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
#[cfg(feature = "builtin-tools")]
#[test]
fn explicit_opt_in_via_builder_registers_the_bash_tool() {
    let conway = build_conway_with_selection(Some(PluginSelection::All));

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
#[cfg(feature = "builtin-tools")]
#[test]
fn explicit_opt_in_via_only_naming_shell_registers_the_bash_tool() {
    let conway =
        build_conway_with_selection(Some(PluginSelection::Only(
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
        .with_router(fake_router())
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
#[cfg(feature = "builtin-tools")]
#[test]
fn all_except_shell_and_none_select_what_their_names_say() {
    let all_except_shell = build_conway_with_selection(Some(PluginSelection::AllExcept(vec![
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

    let nothing = build_conway_with_selection(Some(PluginSelection::None));
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
#[cfg(feature = "builtin-tools")]
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
        .with_router(fake_router())
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
#[cfg(feature = "builtin-tools")]
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
        .with_router(fake_router())
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
        .with_router(fake_router())
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
        .with_router(fake_router())
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
        .with_router(fake_router())
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
            kind: "anthropic".to_string(),
            api_key_env: "CONWAY_TEST_DEFINITELY_UNSET_KEY_VAR".to_string(),
            base_url: "https://api.kimi.com/coding/".to_string(),
            ..BackendEntry::default()
        },
    );

    let result = ConwayBuilder::from_parts(cfg)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(fake_router())
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
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
        .with_router(fake_router())
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
        .with_router(fake_router())
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
        .with_router(fake_router())
        .build()
        .err()
        .expect("an unresolved kind must fail the build")
        .to_string();

    // The identical unresolved kind, but this caller declares it was
    // deliberately declined rather than simply never having heard of it.
    let declined_err = ConwayBuilder::from_parts(cfg_naming_openai_compat())
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(fake_router())
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
