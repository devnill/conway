//! Edge B's capability CALL channel (board item `01M0WWNHQQYN1EVTH8WPZ33EBF`,
//! `docs/vision/DESIGN-plugin-dependencies.md` §2), exercised through the
//! REAL `ConwayBuilder::build()` facade -- distinct from
//! `crates/conway-core/src/ports/capability.rs`'s own test module, which
//! exercises the channel primitives (`CapabilityCallHandle`,
//! `CapabilityRegistry`, `CapabilityProvider`) directly, and from
//! `crates/conway/src/builder.rs`'s own `plugin_dependency_resolution_tests`
//! module, which exercises the free resolution functions
//! (`missing_required_dependency`/`missing_optional_dependencies`/
//! `provided_capability_names`) at the graph-algorithm level. This file is
//! the facade-level proof that acceptance 2/3 hold through `build()` itself,
//! not merely through the private functions that implement them --
//! mirroring `install_selected.rs`'s own split for the plugin-id case
//! (`PluginManifest::requires`/`::optional`).
//!
//! Board item `01M0XXWV3BVDM6Y646WMEBTYT1` (this file's own headline suite,
//! below the pre-existing `build()`-only tests above) extends this file to
//! the RUNTIME half of the same channel: a real dispatched tool call,
//! through the real `conway_runtime::tools::runner`, reaching a real
//! provider registered by a DIFFERENT installed plugin -- the gap that
//! item's own report names (`runner.rs`'s call site was bound to
//! `CapabilityCallHandle::noop`, so a `requires` edge that resolved as
//! satisfied at `build()` time still refused every live call).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::plugin::{
    CapabilityCallError, CapabilityError, CapabilityProvider, CapabilityRegistration, ContentBlock,
    HostCapability, PermissionClass, PluginManifest, Tool, ToolCategory, ToolCtx, ToolError,
    ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};
use conway::test_support::{echo_model, scripted_backend};
use conway::{Conway, ConwayBuilder, FacadeError, Plugin};
use conway_core::agent::PermissionDecision;
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{StopReason, ToolCall, Usage};
use conway_core::ids::{BackendId, RoleAlias};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, Router};
use conway_testkit::{text_response, FakeBackend, FakeGate, FakeRouter, FakeStore, ScriptedTurn};

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

/// Mirrors `install_selected.rs`'s own `empty_router` exactly (same name,
/// same reason): these tests only need a `Router` present so `build()`
/// proceeds, never a resolved route.
fn empty_router() -> Arc<dyn Router> {
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
        backends: BTreeMap::<String, BackendEntry>::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig {
            install: vec![],
            default_backends: vec![],
            subprocess: vec![],
            mcp: vec![],
            claude_compat: vec![],
            // Board item 01M0X500861X9035QJEA82F94K: `PluginsConfig` grew
            // this field after this literal was written -- `..Default::
            // default()` is deliberately not used here (this literal
            // predates that field and every field above it was already
            // spelled out explicitly), so the new field is spelled out too
            // rather than silently inheriting a default this test never
            // chose.
        },
        hooks: HooksConfig::default(),
    }
}

/// `Conway` deliberately does not derive `Debug`, mirroring
/// `install_selected.rs`'s own `expect_build_err`.
fn expect_build_err(result: Result<Conway, FacadeError>, msg: &str) -> FacadeError {
    match result {
        Err(err) => err,
        Ok(_) => panic!("{msg}"),
    }
}

/// A fixture provider that answers every call with its input, unchanged.
struct EchoProvider;

#[async_trait::async_trait]
impl CapabilityProvider for EchoProvider {
    async fn call(&self, payload: serde_json::Value) -> Result<serde_json::Value, CapabilityError> {
        Ok(payload)
    }
}

/// A `Plugin` fake that registers one live capability provider under
/// `capability` (its own `PluginManifest::provides`-equivalent runtime
/// registration -- see `Plugin::capabilities`'s own doc for why this is a
/// trait method, not a manifest field).
struct ProvidingPlugin {
    id: &'static str,
    capability: &'static str,
}

impl Plugin for ProvidingPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
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

    fn capabilities(&self) -> Vec<CapabilityRegistration> {
        vec![CapabilityRegistration::new(
            HostCapability::named(self.capability).unwrap(),
            "1.0.0",
            Arc::new(EchoProvider) as Arc<dyn CapabilityProvider>,
        )]
    }
}

/// A `Plugin` fake carrying configurable `PluginManifest::requires`/
/// `::optional` -- mirrors `install_selected.rs`'s own `DependentPlugin`.
struct DependentPlugin {
    id: &'static str,
    requires: Vec<String>,
    optional: Vec<String>,
}

impl Plugin for DependentPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: self.requires.clone(),
            optional: self.optional.clone(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

/// Acceptance 2 (positive control): a `requires` entry satisfied by a
/// PROVIDED capability -- not a plugin id -- builds cleanly, no different
/// from a satisfied plugin-id `requires` edge.
#[test]
fn a_requires_edge_satisfied_by_a_provided_capability_builds_cleanly() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.ui".to_string(), "acme.consumer".to_string()];

    ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![
                Arc::new(ProvidingPlugin {
                    id: "acme.ui",
                    capability: "acme.ui.checkbox",
                }) as Arc<dyn Plugin>,
                Arc::new(DependentPlugin {
                    id: "acme.consumer",
                    requires: vec!["acme.ui.checkbox".to_string()],
                    optional: vec![],
                }) as Arc<dyn Plugin>,
            ],
            vec![],
            vec![],
        )
        .expect("both ids resolve against the supplied bundle")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
        .expect("a requires entry satisfied by a PROVIDED capability must build cleanly");
}

/// Acceptance 2: a `requires` entry naming a capability NOTHING installed
/// provides fails at `build()`, naming the consumer and the unprovided
/// capability -- the SAME `FacadeError::Build` shape a missing plugin id
/// already produces.
#[test]
fn a_requires_edge_naming_an_unprovided_capability_fails_build_naming_both_sides() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.consumer".to_string()];

    let result = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![Arc::new(DependentPlugin {
                id: "acme.consumer",
                requires: vec!["acme.ui.checkbox".to_string()],
                optional: vec![],
            }) as Arc<dyn Plugin>],
            vec![],
            vec![],
        )
        .expect("install_selected must not itself refuse a missing required dependency")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build();

    let err = expect_build_err(
        result,
        "a requires entry naming a capability nothing installed provides must fail build()",
    );
    match err {
        FacadeError::Build { message } => {
            assert!(message.contains("acme.consumer"), "{message}");
            assert!(message.contains("acme.ui.checkbox"), "{message}");
        }
        other => panic!("expected FacadeError::Build, got {other:?}"),
    }
}

/// Acceptance 3: an `optional` capability nothing provides degrades (never
/// fails `build()`) and is announced via `Conway::warnings()`
/// (`WarningCode::OptionalPluginDependencyMissing`) -- the SAME two-channel
/// announcement a missing optional plugin-id dependency already gets.
#[test]
fn an_optional_capability_nothing_provides_degrades_and_is_announced() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.consumer".to_string()];

    let conway = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![Arc::new(DependentPlugin {
                id: "acme.consumer",
                requires: vec![],
                optional: vec!["acme.ui.checkbox".to_string()],
            }) as Arc<dyn Plugin>],
            vec![],
            vec![],
        )
        .expect("install_selected must succeed: no REQUIRED dependency is missing")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
        .expect("a missing OPTIONAL capability must never fail build()");

    let warnings = conway.warnings();
    let degraded: Vec<_> = warnings
        .iter()
        .filter(|w| w.code == conway::config::WarningCode::OptionalPluginDependencyMissing)
        .collect();
    assert_eq!(
        degraded.len(),
        1,
        "exactly one degradation warning must be recorded: {warnings:?}"
    );
    assert!(degraded[0].message.contains("acme.consumer"));
    assert!(degraded[0].message.contains("acme.ui.checkbox"));
}

/// Positive control beside the optional-degrade test above: the SAME
/// `optional` capability, but a provider IS installed this time -- builds
/// cleanly with no degradation warning at all.
#[test]
fn an_optional_capability_something_provides_produces_no_warning() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.ui".to_string(), "acme.consumer".to_string()];

    let conway = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![
                Arc::new(ProvidingPlugin {
                    id: "acme.ui",
                    capability: "acme.ui.checkbox",
                }) as Arc<dyn Plugin>,
                Arc::new(DependentPlugin {
                    id: "acme.consumer",
                    requires: vec![],
                    optional: vec!["acme.ui.checkbox".to_string()],
                }) as Arc<dyn Plugin>,
            ],
            vec![],
            vec![],
        )
        .expect("both ids resolve against the supplied bundle")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build()
        .expect("an optional capability satisfied by a real provider must build cleanly");

    assert!(
        conway
            .warnings()
            .iter()
            .all(|w| w.code != conway::config::WarningCode::OptionalPluginDependencyMissing),
        "a satisfied optional capability must not produce a degradation warning: {:?}",
        conway.warnings()
    );
}

// ---- board item 01M0XXWV3BVDM6Y646WMEBTYT1: the RUNTIME half ----
//
// Everything below drives a REAL dispatched tool call through
// `ConwayBuilder::build()` and a real session/prompt/turn -- never a
// hand-built `ToolRunner`/`ToolBatchCtx` -- so these tests fail if the
// production wiring (`RuntimeDeps::capabilities` ->
// `conway_runtime::tools::runner`'s `execute_one`) regresses back to a
// `CapabilityCallHandle::noop`, even though every primitive it is built
// from is already covered elsewhere (this file's own tests above, and
// `conway-core`'s `ports::capability` test module).

/// A provider that always fails with a fixed message -- Acceptance 2's own
/// fixture: proves a provider's own [`CapabilityError`] reaches the caller
/// as [`CapabilityCallError::Provider`], distinguishable from
/// [`CapabilityCallError::NotProvided`] (Acceptance 3's regression case,
/// exercised below with NO provider installed at all).
struct FailingProvider;

#[async_trait::async_trait]
impl CapabilityProvider for FailingProvider {
    async fn call(
        &self,
        _payload: serde_json::Value,
    ) -> Result<serde_json::Value, CapabilityError> {
        Err(CapabilityError::new("acme.fixture.fail always fails"))
    }
}

/// A provider that requires the payload's `"caller_plugin_id"` field to
/// equal `expected_caller` -- Acceptance 5's own fixture. `caller_plugin_id`
/// is the field this item's own spec calls out as most likely to be wired
/// to the REGISTRY's owner (this plugin's own id, `"acme.ui"` below) by
/// mistake rather than the per-call CALLER's id (`"acme.consumer"`): either
/// mistake fails LOUDLY here as `CapabilityCallError::Provider`, never a
/// silent pass.
struct AssertingProvider {
    expected_caller: &'static str,
}

#[async_trait::async_trait]
impl CapabilityProvider for AssertingProvider {
    async fn call(&self, payload: serde_json::Value) -> Result<serde_json::Value, CapabilityError> {
        let actual = payload
            .get("caller_plugin_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if actual == self.expected_caller {
            Ok(serde_json::json!({ "ok": true }))
        } else {
            Err(CapabilityError::new(format!(
                "expected caller_plugin_id '{}', got '{actual}'",
                self.expected_caller
            )))
        }
    }
}

/// A `Plugin` fake that registers a live provider built from `make_provider`
/// -- the SAME shape `ProvidingPlugin` (above) is, generalized over which
/// provider it registers so [`FailingProvider`]/[`AssertingProvider`] don't
/// each need their own bespoke `Plugin` impl.
struct ProviderPlugin<F> {
    id: &'static str,
    capability: &'static str,
    make_provider: F,
}

impl<F> Plugin for ProviderPlugin<F>
where
    F: Fn() -> Arc<dyn CapabilityProvider> + Send + Sync + 'static,
{
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
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

    fn capabilities(&self) -> Vec<CapabilityRegistration> {
        vec![CapabilityRegistration::new(
            HostCapability::named(self.capability).unwrap(),
            "1.0.0",
            (self.make_provider)(),
        )]
    }
}

/// A `Tool` whose `invoke` calls straight through `ctx.capabilities` -- the
/// REAL production seam this item wires (`conway_runtime::tools::runner`'s
/// `execute_one`), never a hand-built `CapabilityRegistry`/
/// `CapabilityCallHandle` pair. Embeds `ctx.capabilities.caller_plugin_id()`
/// into the outgoing payload so a provider fixture ([`AssertingProvider`])
/// can assert on it, and renders the call's outcome as plain text so a test
/// can read it straight off the resulting `ToolResultRecord` -- one branch
/// per [`CapabilityCallError`] variant, so a wrong variant is as visible as
/// a wrong value. `CapabilityCallError` carries no `#[non_exhaustive]` and
/// this match has no wildcard arm, so that "one branch per variant" claim
/// is not just a description -- rustc's own exhaustiveness check (`E0004`)
/// enforces it: a fifth variant added to that enum without a matching arm
/// here fails this crate's own test build, not just a code review.
struct CapabilityCallingTool {
    capability: &'static str,
}

#[async_trait::async_trait]
impl Tool for CapabilityCallingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("acme_call_capability"),
            description: "calls a capability through ctx.capabilities".into(),
            schema: serde_json::from_value(serde_json::json!({"type": "object"}))
                .expect("valid RootSchema JSON"),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let caller = ctx.capabilities.caller_plugin_id().to_string();
        let result = ctx
            .capabilities
            .call(
                self.capability,
                serde_json::json!({ "caller_plugin_id": caller }),
            )
            .await;
        let text = match result {
            Ok(value) => format!("ok:{value}"),
            Err(CapabilityCallError::NotProvided { capability }) => {
                format!("not_provided:{capability}")
            }
            Err(CapabilityCallError::Provider { capability, error }) => {
                format!("provider_error:{capability}:{}", error.message)
            }
            Err(CapabilityCallError::MalformedName { capability, reason }) => {
                format!("malformed:{capability}:{reason}")
            }
            Err(CapabilityCallError::VersionMismatch {
                capability,
                required,
                available,
                available_declared: _,
            }) => {
                format!("version_mismatch:{capability}:{required}:{available}")
            }
        };
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: Vec::new(),
        })
    }
}

/// A provider whose `call` panics -- board item `01M12XRY8MZRG8Q88E0WMSGBF4`'s
/// own fixture. Proves a panic inside an IN-PROCESS provider, invoked from a
/// DIFFERENT plugin's tool, is contained by `conway_runtime::tools::runner`'s
/// `catch_unwind` (`runner.rs:265`, wrapping `execute_one` -- the same
/// future a capability call runs inside) and reported as THIS TOOL's
/// (`acme_call_capability`'s) own error, never a silent `NotProvided` and
/// never an aborted process. The subprocess tier's equivalent case (a
/// provider PROCESS dying mid-call, as opposed to an in-process panic) is
/// already covered by
/// `conway-plugin-subprocess/tests/capability_channel.rs`'s own
/// `a_dead_child_mid_capability_call_produces_a_typed_error_not_a_hang` --
/// that transport's dead-session path, not `catch_unwind`, is what contains
/// it, so it is a distinct guard and out of scope here.
struct PanickingProvider;

#[async_trait::async_trait]
impl CapabilityProvider for PanickingProvider {
    async fn call(
        &self,
        _payload: serde_json::Value,
    ) -> Result<serde_json::Value, CapabilityError> {
        panic!("acme.fixture.panic always panics");
    }
}

/// A `Plugin` whose sole tool is [`CapabilityCallingTool`] -- mirrors
/// `ProvidingPlugin`/`ProviderPlugin` one edge over: this one CALLS a
/// capability rather than providing one.
struct CallingPlugin {
    id: &'static str,
    capability: &'static str,
}

impl Plugin for CallingPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![ToolName::new("acme_call_capability")],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(CapabilityCallingTool {
            capability: self.capability,
        })]
    }
}

/// A single scripted turn that calls `acme_call_capability`, followed by a
/// plain-text turn so the session finishes cleanly.
fn call_capability_tool_script() -> Vec<ScriptedTurn> {
    vec![
        ScriptedTurn::Respond(GenerateResponse {
            content: vec![],
            tool_calls: vec![ToolCall {
                call_id: "call_1".to_string(),
                name: ToolName::new("acme_call_capability"),
                arguments: serde_json::json!({}),
            }],
            stop: StopReason::ToolUse,
            usage: Usage::default(),
        }),
        ScriptedTurn::Respond(text_response("done")),
    ]
}

/// Runs one prompt/turn to completion and returns the session's transcript
/// -- mirrors `hook_revoke_seam.rs`'s own `run_one_bash_call` exactly, one
/// tool over.
async fn run_one_turn(conway: &Conway) -> Vec<LogRecord> {
    let handle = conway
        .new_session(conway::SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let root = handle.root();
    let turn = handle.prompt("do the thing").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    handle.transcript(root).await.expect("transcript")
}

/// The `acme_call_capability` tool's own rendered result text, read straight
/// off the transcript's `ToolResultRecord`.
fn capability_call_result_text(records: &[LogRecord]) -> Option<String> {
    records.iter().find_map(|r| match r {
        LogRecord::ToolResultRecord { result, .. }
            if result.tool.as_str() == "acme_call_capability" =>
        {
            result.blocks.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        }
        _ => None,
    })
}

/// Like [`capability_call_result_text`], but also returns the record's own
/// `is_error` -- needed to distinguish a contained panic (reported through
/// `ToolOutcome::error`, which always sets `is_error: true`) from every
/// outcome [`CapabilityCallingTool::invoke`] can produce on its own
/// (`ok:`/`not_provided:`/`provider_error:`/`malformed:`, all of which it
/// returns with `is_error: false` -- see its own `Ok(ToolOutput { is_error:
/// false, .. })`).
fn capability_call_result(records: &[LogRecord]) -> Option<(String, bool)> {
    records.iter().find_map(|r| match r {
        LogRecord::ToolResultRecord { result, .. }
            if result.tool.as_str() == "acme_call_capability" =>
        {
            let text = result.blocks.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })?;
            Some((text, result.is_error))
        }
        _ => None,
    })
}

/// A `ConwayBuilder` for this section's own tests: `plugins`, driven through
/// a scripted backend that calls `acme_call_capability` once, with the
/// port doubles every other test in this file already uses.
fn build_calling_conway(plugins: Vec<Arc<dyn Plugin>>, install: Vec<&str>) -> Conway {
    let mut cfg = base_config();
    cfg.plugins.install = install.into_iter().map(str::to_string).collect();

    ConwayBuilder::from_parts(cfg)
        .install_selected(plugins, vec![], vec![])
        .expect("every listed id must resolve against the supplied bundle")
        .with_backend(scripted_backend(call_capability_tool_script()))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(Arc::new(FakeRouter::single(echo_model())))
        .build()
        .expect("build should succeed with every installed plugin resolving cleanly")
}

/// Acceptance 1: a tool invoked through the REAL runner (via
/// `ConwayBuilder::build`, a real session, a real prompt/turn -- never a
/// hand-built `ToolRunner`) reaches a provider registered by a DIFFERENT
/// installed plugin, and gets its answer.
#[tokio::test]
async fn a_tool_invoked_through_the_real_runner_reaches_a_different_plugins_provider() {
    let conway = build_calling_conway(
        vec![
            Arc::new(ProvidingPlugin {
                id: "acme.ui",
                capability: "acme.ui.checkbox",
            }) as Arc<dyn Plugin>,
            Arc::new(CallingPlugin {
                id: "acme.consumer",
                capability: "acme.ui.checkbox",
            }) as Arc<dyn Plugin>,
        ],
        vec!["acme.ui", "acme.consumer"],
    );

    let records = run_one_turn(&conway).await;
    let text = capability_call_result_text(&records)
        .expect("a ToolResultRecord for acme_call_capability must exist");
    assert!(
        text.starts_with("ok:"),
        "the call must reach acme.ui's real EchoProvider and succeed: {text}"
    );
    assert!(
        text.contains("acme.consumer"),
        "the echoed payload must carry the caller's own id: {text}"
    );
}

/// Acceptance 2: an error returned by the provider reaches the caller as
/// `CapabilityCallError::Provider`, distinguishable from `NotProvided`.
#[tokio::test]
async fn an_error_from_the_provider_reaches_the_caller_as_capabilitycallerror_provider() {
    let conway = build_calling_conway(
        vec![
            Arc::new(ProviderPlugin {
                id: "acme.ui",
                capability: "acme.ui.checkbox",
                make_provider: || Arc::new(FailingProvider) as Arc<dyn CapabilityProvider>,
            }) as Arc<dyn Plugin>,
            Arc::new(CallingPlugin {
                id: "acme.consumer",
                capability: "acme.ui.checkbox",
            }) as Arc<dyn Plugin>,
        ],
        vec!["acme.ui", "acme.consumer"],
    );

    let records = run_one_turn(&conway).await;
    let text = capability_call_result_text(&records)
        .expect("a ToolResultRecord for acme_call_capability must exist");
    assert!(
        text.starts_with("provider_error:acme.ui.checkbox:"),
        "a provider failure must surface as Provider, naming the capability: {text}"
    );
    assert!(
        text.contains("acme.fixture.fail always fails"),
        "the provider's own error message must reach the caller: {text}"
    );
}

/// Acceptance 3 (regression): a call naming a capability nothing installed
/// provides still gets `NotProvided` through the REAL runner -- the no-op's
/// one correct behaviour must survive its replacement by a real registry.
#[tokio::test]
async fn a_call_naming_an_unprovided_capability_still_gets_not_provided() {
    let conway = build_calling_conway(
        vec![Arc::new(CallingPlugin {
            id: "acme.consumer",
            capability: "acme.ui.checkbox",
        }) as Arc<dyn Plugin>],
        vec!["acme.consumer"],
    );

    let records = run_one_turn(&conway).await;
    let text = capability_call_result_text(&records)
        .expect("a ToolResultRecord for acme_call_capability must exist");
    assert_eq!(
        text, "not_provided:acme.ui.checkbox",
        "nothing installed provides this capability -- must be NotProvided, never a panic \
         or a silent success: {text}"
    );
}

/// Acceptance 4: two plugins registering the same capability name fail
/// `build()` with an error naming both plugins and the capability -- the
/// duplicate-provider refusal `CapabilityRegistry::from_registrations`
/// itself returns MUST reach `build()`, not be swallowed while constructing
/// the registry.
#[test]
fn two_plugins_registering_the_same_capability_fail_build_naming_both_and_the_capability() {
    let mut cfg = base_config();
    cfg.plugins.install = vec!["acme.ui.one".to_string(), "acme.ui.two".to_string()];

    let result = ConwayBuilder::from_parts(cfg)
        .install_selected(
            vec![
                Arc::new(ProvidingPlugin {
                    id: "acme.ui.one",
                    capability: "acme.ui.checkbox",
                }) as Arc<dyn Plugin>,
                Arc::new(ProvidingPlugin {
                    id: "acme.ui.two",
                    capability: "acme.ui.checkbox",
                }) as Arc<dyn Plugin>,
            ],
            vec![],
            vec![],
        )
        .expect("both ids resolve against the supplied bundle")
        .with_backend(fake_backend("fake"))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_router(empty_router())
        .build();

    let err = expect_build_err(
        result,
        "two providers for the same capability name must fail build(), never silently pick one",
    );
    match err {
        FacadeError::Build { message } => {
            assert!(message.contains("acme.ui.one"), "{message}");
            assert!(message.contains("acme.ui.two"), "{message}");
            assert!(message.contains("acme.ui.checkbox"), "{message}");
        }
        other => panic!("expected FacadeError::Build, got {other:?}"),
    }
}

/// Acceptance 5: `caller_plugin_id` is the CALLING tool's own declaring
/// plugin (`acme.consumer`), verified by a provider that asserts on it --
/// not the registry's own owner (`acme.ui`, the providing plugin), which is
/// the mistake this field is most likely wired to.
#[tokio::test]
async fn caller_plugin_id_is_the_calling_tools_declaring_plugin() {
    let conway = build_calling_conway(
        vec![
            Arc::new(ProviderPlugin {
                id: "acme.ui",
                capability: "acme.ui.checkbox",
                make_provider: || {
                    Arc::new(AssertingProvider {
                        expected_caller: "acme.consumer",
                    }) as Arc<dyn CapabilityProvider>
                },
            }) as Arc<dyn Plugin>,
            Arc::new(CallingPlugin {
                id: "acme.consumer",
                capability: "acme.ui.checkbox",
            }) as Arc<dyn Plugin>,
        ],
        vec!["acme.ui", "acme.consumer"],
    );

    let records = run_one_turn(&conway).await;
    let text = capability_call_result_text(&records)
        .expect("a ToolResultRecord for acme_call_capability must exist");
    assert!(
        text.starts_with("ok:"),
        "the provider must see caller_plugin_id == 'acme.consumer', not the registry's own \
         owner 'acme.ui' or anything else: {text}"
    );
}

// ---- board item 01M12XRY8MZRG8Q88E0WMSGBF4: an IN-PROCESS provider panic
// crossing the plugin boundary ----
//
// Edge B lets a capability call reach a DIFFERENT plugin's own code. Before
// that channel existed, a plugin's panic surfaced inside its own call stack;
// now it surfaces inside a caller who never chose that code. The general
// `catch_unwind` at `conway_runtime::tools::runner.rs:265` (wrapping
// `execute_one`, the same future a capability call runs inside) is
// pre-existing machinery built for a different purpose -- this test proves
// it actually holds across THIS boundary rather than inheriting the claim
// by reading the source.

/// A panic inside an IN-PROCESS provider, invoked from a DIFFERENT plugin's
/// tool through the REAL runner, is contained and reported as the CALLING
/// tool's own error -- distinguishable both from a clean answer and from a
/// silently-swallowed `NotProvided` (the shape a call that never reached the
/// provider, or a guard that ate the panic without reporting it, would
/// produce). Per P-15, this test is required to FAIL if `catch_unwind` at
/// `runner.rs:265` is neutralised -- verified manually for this item (see
/// its own worker report) rather than automated in-tree, since disabling
/// production containment from a test would defeat the guard for every
/// OTHER test in the same binary.
#[tokio::test]
async fn a_panicking_provider_is_contained_and_reported_as_the_calling_tools_own_error() {
    let conway = build_calling_conway(
        vec![
            Arc::new(ProviderPlugin {
                id: "acme.ui",
                capability: "acme.ui.checkbox",
                make_provider: || Arc::new(PanickingProvider) as Arc<dyn CapabilityProvider>,
            }) as Arc<dyn Plugin>,
            Arc::new(CallingPlugin {
                id: "acme.consumer",
                capability: "acme.ui.checkbox",
            }) as Arc<dyn Plugin>,
        ],
        vec!["acme.ui", "acme.consumer"],
    );

    let records = run_one_turn(&conway).await;
    let (text, is_error) = capability_call_result(&records).expect(
        "a ToolResultRecord for acme_call_capability must exist -- a call that never reached \
         the provider, or a batch that never finished, would leave no record at all, which is \
         a DIFFERENT failure than a contained-and-reported panic",
    );

    // The discriminating observable, named before this assertion was
    // written: `CapabilityCallingTool::invoke` (this file's own fixture)
    // never sets `is_error` itself -- every outcome it can produce on its
    // own (`ok:`/`not_provided:`/`provider_error:`/`malformed:`) comes back
    // as `is_error: false`. So `is_error: true` here can only have been
    // synthesized by the runner's OWN panic path (`ToolOutcome::error` at
    // `runner.rs:269`), never by the tool's own logic and never by a
    // swallowed call reported as an ordinary answer.
    assert!(
        is_error,
        "a contained panic must surface as an ERROR on the calling tool's own result, not as \
         one of its ordinary (is_error: false) outcomes: {text}"
    );
    assert!(
        text.contains("panicked"),
        "the outcome must name the failure as a panic -- an empty or default answer would not: \
         {text}"
    );
    assert!(
        text.contains("acme_call_capability"),
        "the panic must be reported against the CALLING tool itself, not swallowed or \
         misattributed to some other name: {text}"
    );
    assert!(
        text.contains("acme.fixture.panic always panics"),
        "the provider's own panic message must reach the caller -- this is what separates a \
         contained-and-reported panic from a silent no-answer, both of which leave the process \
         alive: {text}"
    );
    assert_ne!(
        text, "not_provided:acme.ui.checkbox",
        "a call that never reached the panicking provider (or a guard that swallowed it) must \
         not be mistaken for a contained-and-reported panic: {text}"
    );
}
