//! Edge B's plugin-to-plugin capability CALL channel
//! (`docs/vision/DESIGN-plugin-dependencies.md` §2), exercised for the
//! OUT-OF-PROCESS leg -- board item `01M0XXXX3HK8914NE418P5GNRY`. Distinct
//! from `crates/conway/tests/capability_channel.rs`, which proves the
//! IN-PROCESS half of the same channel through the real runner; this file
//! proves a subprocess plugin can now stand on the PROVIDING side of that
//! exact channel, reached by a DIFFERENT in-process plugin's own tool call
//! through a real `ConwayBuilder::build()` and a real turn -- the acceptance
//! Edge B's own item (`01M0WWNHQQYN1EVTH8WPZ33EBF`) could only prove
//! "nearly": its own round-trip test spoke to a real child process directly,
//! never through `CapabilityCallHandle::call` from a SEPARATE plugin.
//!
//! `WireManifest::optional_host_caps`/`::provides` parsing itself (both
//! `#[serde(default)]` directions, the fail-closed/well-formed-unknown
//! boundary) is unit-tested in `crate::wire`'s own `#[cfg(test)] mod tests`
//! (`src/wire.rs`) -- this file is the end-to-end proof the parsed fields
//! actually DO something, not a restatement of that parsing coverage.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway::plugin::{
    CapabilityCallError, CapabilityProvider, ContentBlock, HostCapability, PermissionClass, Plugin,
    PluginManifest, Tool, ToolCall, ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput,
    ToolSpec, TruncationPolicy,
};
use conway::test_support::{scripted_backend, test_builder};
use conway::Conway;
use conway_core::content::{StopReason, ToolCall as GenToolCall, Usage};
use conway_core::log::LogRecord;
use conway_core::ports::GenerateResponse;
use conway_plugin_subprocess::{SubprocessPlugin, SubprocessTransport};
use conway_testkit::{text_response, ScriptedTurn};

fn base_config() -> conway::config::schema::ConwayConfig {
    use conway::config::schema::{
        AgentsConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig, PermissionsConfig,
        PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
    };
    let mut roles = std::collections::BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    conway::config::schema::ConwayConfig {
        default_role: conway_core::ids::RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: std::collections::BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        // No `[plugins].subprocess[]` entries -- this test attaches the
        // subprocess plugin the library-embedder way (`with_plugin`), the
        // same reason `end_to_end.rs`'s own `base_config` stays empty here.
        // Load-bearing for acceptance criterion 2: it means `HostCaps::
        // from_config` never offers `persistent_transport`, regardless of
        // which transport the ATTACHED `SubprocessPlugin` itself uses.
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// A `Tool` whose `invoke` calls straight through `ctx.capabilities` with a
/// FIXED payload -- mirrors `crates/conway/tests/capability_channel.rs`'s
/// own `CapabilityCallingTool`, generalized over the payload so one fixture
/// covers both the success (echo) and declared-failure (boom) cases this
/// file's own tests need.
struct CapabilityCallingTool {
    capability: &'static str,
    payload: serde_json::Value,
}

#[async_trait]
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
        let result = ctx
            .capabilities
            .call(self.capability, self.payload.clone())
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
        };
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: Vec::new(),
        })
    }
}

/// A `Plugin` whose sole tool is [`CapabilityCallingTool`] -- the calling
/// side, a plugin DIFFERENT from whichever `SubprocessPlugin` is providing
/// the capability under test.
struct CallingPlugin {
    id: &'static str,
    capability: &'static str,
    payload: serde_json::Value,
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
            payload: self.payload.clone(),
        })]
    }
}

/// A single scripted turn that calls `acme_call_capability`, followed by a
/// plain-text turn so the session finishes cleanly -- mirrors
/// `crates/conway/tests/capability_channel.rs`'s own
/// `call_capability_tool_script`.
fn call_capability_tool_script() -> Vec<ScriptedTurn> {
    vec![
        ScriptedTurn::Respond(GenerateResponse {
            content: vec![],
            tool_calls: vec![GenToolCall {
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

/// Runs one prompt/turn to completion and returns the session's transcript.
async fn run_one_turn(conway: &Conway) -> Vec<LogRecord> {
    let handle = conway
        .new_session(conway::SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let root = handle.root();
    let turn = handle.prompt("do the thing").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang")
        .expect("turn should succeed");
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

fn build_calling_conway(plugins: Vec<Arc<dyn Plugin>>) -> Conway {
    let cfg = base_config();
    let mut builder =
        test_builder(cfg).with_backend(scripted_backend(call_capability_tool_script()));
    for plugin in plugins {
        builder = builder.with_plugin(plugin);
    }
    builder
        .build()
        .expect("build should succeed with every attached plugin resolving cleanly")
}

// ---------------------------------------------------------------------
// Acceptance 3: a subprocess plugin declaring `provides` registers a real
// CapabilityProvider -- checked directly against `Plugin::capabilities()`,
// no turn required, mirroring how `tests/mechanism.rs` checks `Plugin::
// tools()` directly.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_subprocess_plugin_declaring_provides_registers_one_capability_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec =
        common::spec_for_warmed(dir.path(), "provider.py", common::PROVIDES_ECHO_PLUGIN).await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery against the real fixture must succeed");

    let registrations = Plugin::capabilities(&plugin);
    assert_eq!(
        registrations.len(),
        1,
        "exactly one provides entry was declared"
    );
    assert_eq!(
        registrations[0].capability,
        HostCapability::named("acme.provider.echo").unwrap()
    );
}

// ---------------------------------------------------------------------
// Acceptance 4: a real child process serves a capability call made by a
// DIFFERENT, in-process plugin, through a real `ConwayBuilder::build()` and
// a real turn, and the response reaches the caller.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_real_child_process_serves_a_capability_call_from_a_different_in_process_plugin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec =
        common::spec_for_warmed(dir.path(), "provider.py", common::PROVIDES_ECHO_PLUGIN).await;
    let provider_plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery against the real fixture must succeed");

    let calling_plugin = CallingPlugin {
        id: "acme.consumer",
        capability: "acme.provider.echo",
        payload: serde_json::json!({"hello": "wire"}),
    };

    let conway = build_calling_conway(vec![
        Arc::new(provider_plugin) as Arc<dyn Plugin>,
        Arc::new(calling_plugin) as Arc<dyn Plugin>,
    ]);

    let records = run_one_turn(&conway).await;
    let text = capability_call_result_text(&records)
        .expect("a ToolResultRecord for acme_call_capability must exist");
    assert_eq!(
        text, r#"ok:{"echoed":{"hello":"wire"}}"#,
        "the REAL child's own capability/1 answer must reach the caller verbatim, proving the \
         call crossed a genuine subprocess boundary and back: {text}"
    );
}

// ---------------------------------------------------------------------
// Acceptance 5: a provider error the child DECLARES reaches the caller as
// `CapabilityCallError::Provider`.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_declared_provider_error_from_the_child_reaches_the_caller_as_provider_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec =
        common::spec_for_warmed(dir.path(), "provider.py", common::PROVIDES_ECHO_PLUGIN).await;
    let provider_plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery against the real fixture must succeed");

    let calling_plugin = CallingPlugin {
        id: "acme.consumer",
        capability: "acme.provider.echo",
        payload: serde_json::json!({"boom": true}),
    };

    let conway = build_calling_conway(vec![
        Arc::new(provider_plugin) as Arc<dyn Plugin>,
        Arc::new(calling_plugin) as Arc<dyn Plugin>,
    ]);

    let records = run_one_turn(&conway).await;
    let text = capability_call_result_text(&records)
        .expect("a ToolResultRecord for acme_call_capability must exist");
    assert_eq!(
        text, "provider_error:acme.provider.echo:acme.provider.echo declined",
        "a declared child failure must surface as Provider, naming the capability and carrying \
         the child's own message verbatim: {text}"
    );
}

// ---------------------------------------------------------------------
// Acceptance 2: a subprocess plugin declaring an optional host cap the host
// does not grant degrades and is announced, matching
// `PluginManifest::optional_host_caps`' own behaviour.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_subprocess_plugin_with_an_unsatisfied_optional_host_cap_degrades_and_is_announced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::spec_for_warmed(
        dir.path(),
        "optional_cap.py",
        common::OPTIONAL_HOST_CAP_PLUGIN,
    )
    .await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery against the real fixture must succeed");

    let cfg = base_config();
    let conway = test_builder(cfg)
        .with_backend(scripted_backend(vec![]))
        .with_plugin(Arc::new(plugin))
        .build()
        .expect(
            "a subprocess plugin whose optional_host_caps names a cap the host lacks must still \
             build, degraded",
        );

    let warning = conway
        .warnings()
        .iter()
        .find(|w| w.code == conway::config::WarningCode::OptionalHostCapabilityMissing)
        .expect("build() must record an OptionalHostCapabilityMissing warning");
    assert!(
        warning.message.contains("acme.optional-cap"),
        "warning must name the plugin: {}",
        warning.message
    );
    assert!(
        warning.message.contains("persistent_transport"),
        "warning must name the missing cap: {}",
        warning.message
    );
}

// ---------------------------------------------------------------------
// Acceptance 6: a dead child mid-call, and a malformed response, both
// produce a stated, typed outcome -- never a hang. Exercised directly
// against the registered `CapabilityProvider`, the same "no turn required"
// shape the registration test above uses -- this is a transport-level
// property of `SubprocessCapabilityProvider::call`/`PersistentSession::
// capability_round_trip`, not something a full agent turn adds coverage
// for.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_dead_child_mid_capability_call_produces_a_typed_error_not_a_hang() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "die.py",
        common::PERSISTENT_PROVIDES_DIE_AFTER_ONE_PLUGIN,
    )
    .await;
    assert_eq!(spec.transport, SubprocessTransport::Persistent);
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery/handshake against the real fixture must succeed");

    let registrations = Plugin::capabilities(&plugin);
    assert_eq!(registrations.len(), 1);
    let provider = &registrations[0].provider;

    // First call: the child answers normally (still alive).
    let first = tokio::time::timeout(
        Duration::from_secs(10),
        provider.call(serde_json::json!({"n": 1})),
    )
    .await
    .expect("the first call must not hang")
    .expect("the first call must succeed -- the fixture answers before exiting");
    assert_eq!(first, serde_json::json!({"echoed": {"n": 1}}));

    // Second call: the child already exited after answering the first --
    // must fail with a typed error, bounded, never a hang.
    let second = tokio::time::timeout(
        Duration::from_secs(10),
        provider.call(serde_json::json!({"n": 2})),
    )
    .await
    .expect("the second call must not hang even though the session is dead")
    .expect_err("a dead session must fail the call, not silently succeed");
    let lower = second.message.to_lowercase();
    assert!(
        lower.contains("died") || lower.contains("session") || lower.contains("timed"),
        "the typed error should name the dead-session/timeout cause, never an opaque failure: {}",
        second.message
    );
}

#[tokio::test]
async fn a_malformed_capability_response_produces_a_typed_error_not_a_hang() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = common::persistent_spec_for_warmed(
        dir.path(),
        "badjson.py",
        common::PERSISTENT_PROVIDES_BAD_JSON_PLUGIN,
    )
    .await;
    let plugin = SubprocessPlugin::discover(spec)
        .await
        .expect("discovery/handshake against the real fixture must succeed");

    let registrations = Plugin::capabilities(&plugin);
    assert_eq!(registrations.len(), 1);
    let provider = &registrations[0].provider;

    let err = tokio::time::timeout(
        Duration::from_secs(10),
        provider.call(serde_json::json!({"n": 1})),
    )
    .await
    .expect("a malformed response must not hang the caller")
    .expect_err("invalid JSON on the wire must fail the call, not silently succeed");
    assert!(
        !err.message.is_empty(),
        "the typed error must carry a non-empty message naming the malformed answer"
    );
}
