//! Acceptance tests for board item `01M129QW0GV90QTQS6B3BY3DAR`: a plugin
//! registers a hook through [`conway_core::ports::Plugin::hooks`], and that
//! registration reaches [`conway_runtime::permission::PermissionBroker::
//! decide`] at the SAME tier a config-declared `[hooks].rules[]` entry
//! always has -- before the mode gate, the cache, pattern allows, and
//! `AutoAllow` -- and is DISTINGUISHABLE from an operator-authored rule once
//! it gets there.
//!
//! Drives the REAL production seam end to end: a real `ConwayBuilder::build`
//! (not a hand-built `PermissionBroker` fixture -- that would only prove the
//! broker's own tier ordering, unchanged by this item, not that a plugin's
//! `hooks()` actually reaches it), the real `bash` tool, and a real
//! `ProcessHookRunner` spawning a real `/bin/sh` process -- the same
//! discipline `hook_revoke_seam.rs` already establishes for a
//! config-declared hook.
#![cfg(feature = "builtin-tools")]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{Plugin, PluginHookRule, PluginManifest, Tool};
use conway::test_support::{scripted_backend, test_builder};
use conway::{Conway, PluginSelection};
use conway_core::agent::{PermissionDecision, PermissionRequest};
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::ids::{RoleAlias, ToolName};
use conway_core::log::LogRecord;
use conway_core::permission_mode::PermissionMode;
use conway_core::ports::{GenerateResponse, PermissionGate};
use conway_testkit::{text_response, ScriptedTurn};
use tempfile::TempDir;

fn bash_call_response(command: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new("bash"),
            arguments: serde_json::json!({ "command": command }),
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

fn base_config(cwd: &Path) -> ConwayConfig {
    let mut roles = std::collections::BTreeMap::new();
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
        cwd: cwd.to_path_buf(),
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
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// A minimal plugin whose ONLY contribution is one `pre_tool_use` hook rule
/// -- a real command that always exits non-zero, so a real `ProcessHookRunner`
/// fail-closes it to `HookOnFailure::Deny` exactly like `hook_revoke_seam.rs`'s
/// own `denying_pre_tool_use_rule` does for a config-declared rule.
struct DenyingHookPlugin {
    manifest_id: &'static str,
}

impl Plugin for DenyingHookPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.manifest_id.to_string(),
            version: "0.0.0".to_string(),
            tools: Vec::new(),
            required_host_caps: Vec::new(),
            optional_host_caps: Vec::new(),
            requires: Vec::new(),
            optional: Vec::new(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn hooks(&self) -> Vec<PluginHookRule> {
        vec![PluginHookRule {
            id: "deny-bash".to_string(),
            event: "pre_tool_use".to_string(),
            match_tool: None,
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 1".to_string(),
            ],
            timeout_ms: 5_000,
            enabled: true,
            on_failure: Default::default(),
            spawn_only: false,
        }]
    }
}

/// A plugin whose single hook rule is fully caller-specified -- the id and
/// the event are what each collision/coverage test below actually varies,
/// so they vary exactly one thing rather than each carrying a near-copy of
/// [`DenyingHookPlugin`].
struct ConfigurableHookPlugin {
    manifest_id: &'static str,
    rule_id: &'static str,
    event: &'static str,
}

impl Plugin for ConfigurableHookPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.manifest_id.to_string(),
            version: "0.0.0".to_string(),
            tools: Vec::new(),
            required_host_caps: Vec::new(),
            optional_host_caps: Vec::new(),
            requires: Vec::new(),
            optional: Vec::new(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn hooks(&self) -> Vec<PluginHookRule> {
        vec![PluginHookRule {
            id: self.rule_id.to_string(),
            event: self.event.to_string(),
            match_tool: None,
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 1".to_string(),
            ],
            timeout_ms: 5_000,
            enabled: true,
            on_failure: Default::default(),
            spawn_only: false,
        }]
    }
}

/// Records every `PermissionRequest` it receives and always answers Allow
/// -- the headline signal is WHETHER the gate is reached AT ALL, mirroring
/// `hook_revoke_seam.rs`'s own `RecordingAllowGate` exactly.
struct RecordingAllowGate {
    requests: Mutex<Vec<PermissionRequest>>,
}

impl RecordingAllowGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
        })
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl PermissionGate for RecordingAllowGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        self.requests.lock().unwrap().push(req);
        PermissionDecision::AllowOnce
    }
}

async fn run_one_bash_call(conway: &Conway) -> Vec<LogRecord> {
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

fn bash_tool_result_text(records: &[LogRecord]) -> Option<String> {
    records.iter().find_map(|r| match r {
        LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "bash" => {
            result.blocks.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        }
        _ => None,
    })
}

/// **ACCEPTANCE 2 -- the tier, not merely that the hook ran.** Permission
/// mode is `AutoAllow`: with NO hook installed, `PermissionBroker::decide`
/// returns `Allow` directly (`decide()`'s own `mode == PermissionMode::
/// AutoAllow` arm) WITHOUT EVER CONSULTING THE GATE -- so a call that still
/// ends up denied, under this mode, can only mean something fired BEFORE
/// that shortcut. A plugin-registered `pre_tool_use` hook is checked at
/// EXACTLY that point (the same step `deny_matches` and an operator's own
/// `[hooks].rules[]` entry are checked at) -- before the mode gate, the
/// cache, pattern allows, and `AutoAllow` itself.
///
/// **The discriminating observable, named per P-15 before writing this
/// test:** the bash tool's own `ToolResultRecord` text -- `"denied"` present
/// (the hook fired ahead of AutoAllow, correct tier) versus absent (AutoAllow
/// allowed the call because the hook was wired at, or after, its own tier --
/// the defect this test exists to catch). A call-count assertion alone
/// cannot distinguish these two outcomes here: `RecordingAllowGate.
/// request_count()` is `0` in EITHER case, since `AutoAllow` bypasses the
/// gate regardless of whether a hook denied first -- that is exactly the
/// P-15 trap ("a correct refusal and a silent full bypass both produce zero
/// gate calls"), which is why the tool-result TEXT, not the gate's call
/// count, is this test's real assertion.
#[tokio::test]
async fn a_plugin_registered_pre_tool_use_hook_outranks_auto_allow() {
    let cwd = TempDir::new().expect("tempdir");
    let gate = RecordingAllowGate::new();
    let conway = test_builder(base_config(cwd.path()))
        .with_backend(scripted_backend(vec![
            ScriptedTurn::Respond(bash_call_response("echo hi")),
            ScriptedTurn::Respond(text_response("done")),
        ]))
        .with_permission_gate(gate.clone() as Arc<dyn PermissionGate>)
        .with_builtin_plugins(PluginSelection::All)
        .with_plugin(Arc::new(DenyingHookPlugin {
            manifest_id: "acme-hooks",
        }))
        .with_default_hook_runner()
        .build()
        .expect("build should succeed with the real builtin `bash` tool, hook runner, and plugin");

    // The one mode with NO human in the loop: absent a hook, every call is
    // allowed with the gate never consulted at all.
    conway.set_permission_mode(PermissionMode::AutoAllow);

    let records = run_one_bash_call(&conway).await;
    let text = bash_tool_result_text(&records).expect("a bash ToolResultRecord must exist");
    assert!(
        text.contains("denied"),
        "a plugin-registered pre_tool_use hook must outrank AutoAllow, exactly like an \
         operator-authored one already does: {text}"
    );
    assert!(
        text.contains("deny-bash"),
        "the denial must name the hook that fired: {text}"
    );
    assert_eq!(
        gate.request_count(),
        0,
        "AutoAllow never consults the gate either way -- this assertion alone would pass even \
         if the hook were silently bypassed; it is here only to document that fact, not to \
         carry this test's real claim"
    );
}

/// **ACCEPTANCE 6 -- provenance, made structural, checked directly.** A
/// plugin-registered hook's row on the SAME `/settings` review surface an
/// operator-authored rule already appears on
/// (`Conway::active_deny_capable_hook_rules`) is DISTINGUISHABLE from one:
/// its `origin` names the declaring plugin, never the operator-authored
/// label, and its `id` carries the host-applied namespace prefix no
/// operator-authored id gets.
#[tokio::test]
async fn a_plugin_registered_hook_is_distinguishable_from_an_operator_authored_one() {
    use conway::config::schema::HookEntry;

    let cwd = TempDir::new().expect("tempdir");
    let mut config = base_config(cwd.path());
    config.hooks = HooksConfig {
        rules: vec![HookEntry {
            id: "operator-rule".to_string(),
            event: "pre_tool_use".to_string(),
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 1".to_string(),
            ],
            ..Default::default()
        }],
    };
    let gate = RecordingAllowGate::new();
    let conway = test_builder(config)
        .with_backend(scripted_backend(vec![]))
        .with_permission_gate(gate as Arc<dyn PermissionGate>)
        .with_builtin_plugins(PluginSelection::All)
        .with_plugin(Arc::new(DenyingHookPlugin {
            manifest_id: "acme-hooks",
        }))
        .with_default_hook_runner()
        .build()
        .expect("build should succeed");

    let rows = conway.active_deny_capable_hook_rules();
    assert_eq!(rows.len(), 2, "{rows:?}");

    let operator_row = rows
        .iter()
        .find(|r| r.id == "operator-rule")
        .unwrap_or_else(|| panic!("no row for the operator-authored rule: {rows:?}"));
    assert_eq!(operator_row.origin, "settings.json (merged config)");

    // The plugin's own bare id ("deny-bash") is namespaced by
    // `ConwayBuilder::build` with its declaring plugin's manifest id
    // ("acme-hooks") before it ever reaches this list -- never the bare id
    // the plugin itself returned.
    let plugin_row = rows
        .iter()
        .find(|r| r.id.contains("deny-bash"))
        .unwrap_or_else(|| panic!("no row for the plugin-registered rule: {rows:?}"));
    assert_ne!(
        plugin_row.id, "deny-bash",
        "a plugin-registered hook's dispatched id must be namespaced, never the plugin's own \
         bare id: {}",
        plugin_row.id
    );
    assert!(
        plugin_row.id.contains("acme-hooks"),
        "namespaced by the declaring plugin's own manifest id: {}",
        plugin_row.id
    );
    assert_eq!(
        plugin_row.origin, "plugin 'acme-hooks'",
        "a plugin-registered hook's origin must name its own plugin, never the operator-authored \
         label: {plugin_row:?}"
    );
    assert_ne!(
        plugin_row.origin, operator_row.origin,
        "the two must be genuinely distinguishable, not just differently-named the same way"
    );
}

/// The id-collision guard, which was previously pinned by inspection only.
/// A plugin cannot silently overwrite -- or silently duplicate-dispatch --
/// an id an operator already wrote in `[hooks].rules[]`. `build()` refuses,
/// and the message names the offending plugin so the operator knows which
/// one to remove.
///
/// The fixture is deliberately exact: `seen_hook_ids` is seeded from the
/// config's own rule ids and the plugin's bare id is namespaced as
/// `{manifest_id}.{rule_id}`, so the ONLY config id that can collide is the
/// full namespaced string. A fixture using the bare id would pass against a
/// broken guard, because it would never collide in the first place.
#[tokio::test]
async fn a_plugin_hook_id_colliding_with_an_operator_authored_one_is_refused_by_name() {
    use conway::config::schema::HookEntry;

    let cwd = TempDir::new().expect("tempdir");
    let mut config = base_config(cwd.path());
    config.hooks = HooksConfig {
        rules: vec![HookEntry {
            id: "acme-hooks.deny-bash".to_string(),
            event: "pre_tool_use".to_string(),
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 1".to_string(),
            ],
            ..Default::default()
        }],
    };
    let built = test_builder(config)
        .with_backend(scripted_backend(vec![]))
        .with_builtin_plugins(PluginSelection::All)
        .with_plugin(Arc::new(DenyingHookPlugin {
            manifest_id: "acme-hooks",
        }))
        .with_default_hook_runner()
        .build();
    let err = match built {
        Ok(_) => panic!(
            "a colliding hook id must fail the build, never silently win or duplicate-dispatch"
        ),
        Err(e) => e,
    };

    let message = err.to_string();
    assert!(
        message.contains("acme-hooks.deny-bash"),
        "the refusal must name the colliding id: {message}"
    );
    assert!(
        message.contains("acme-hooks"),
        "the refusal must name the offending plugin: {message}"
    );
}

/// The empty-id guard, likewise previously pinned by inspection only. An
/// empty bare id would namespace to a bare `"<plugin>."` prefix and become
/// an un-nameable rule on the review surface, so it is refused outright.
#[tokio::test]
async fn a_plugin_hook_rule_with_an_empty_id_is_refused_by_name() {
    let cwd = TempDir::new().expect("tempdir");
    let built = test_builder(base_config(cwd.path()))
        .with_backend(scripted_backend(vec![]))
        .with_builtin_plugins(PluginSelection::All)
        .with_plugin(Arc::new(ConfigurableHookPlugin {
            manifest_id: "acme-hooks",
            rule_id: "",
            event: "pre_tool_use",
        }))
        .with_default_hook_runner()
        .build();
    let err = match built {
        Ok(_) => panic!("an empty hook id must fail the build"),
        Err(e) => e,
    };

    let message = err.to_string();
    assert!(
        message.contains("acme-hooks"),
        "the refusal must name the offending plugin: {message}"
    );
    assert!(
        message.contains("empty"),
        "the refusal must say what was wrong with the id: {message}"
    );
}

/// **`prompt_submitted`, not just `pre_tool_use`.** Conway has TWO
/// deny-capable events, and a `prompt_submitted` hook can refuse every
/// prompt the operator types. P-14's own worked example is this exact event
/// being lost: `claude_compat_plugins.rs` once hardcoded the deny-capable
/// set as a single `"pre_tool_use"` literal, so a translated
/// `UserPromptSubmit` rule was misclassified observation-only. Coverage
/// that stops at `pre_tool_use` is how that recurs.
///
/// Asserts a plugin-registered `prompt_submitted` rule reaches the SAME
/// operator-facing review surface, correctly attributed -- so an operator
/// inspecting what a downloaded plugin can do to them sees it (P-12: a rule
/// applied but not inspectable is a trap, not a policy).
#[tokio::test]
async fn a_plugin_registered_prompt_submitted_hook_is_deny_capable_and_attributed() {
    let cwd = TempDir::new().expect("tempdir");
    let conway = test_builder(base_config(cwd.path()))
        .with_backend(scripted_backend(vec![]))
        .with_builtin_plugins(PluginSelection::All)
        .with_plugin(Arc::new(ConfigurableHookPlugin {
            manifest_id: "acme-hooks",
            rule_id: "deny-prompts",
            event: "prompt_submitted",
        }))
        .with_default_hook_runner()
        .build()
        .expect("build should succeed");

    let rows = conway.active_deny_capable_hook_rules();
    let row = rows
        .iter()
        .find(|r| r.id.contains("deny-prompts"))
        .unwrap_or_else(|| {
            panic!(
                "a plugin-registered prompt_submitted rule must appear on the deny-capable \
                 review surface -- it can refuse every prompt the operator types: {rows:?}"
            )
        });
    assert_eq!(
        row.event, "prompt_submitted",
        "the row must carry its real event, not be collapsed into pre_tool_use: {row:?}"
    );
    assert_eq!(
        row.origin, "plugin 'acme-hooks'",
        "a plugin-registered prompt_submitted hook must be attributed to its plugin, never \
         reported as operator-authored: {row:?}"
    );
    assert!(
        row.id.contains("acme-hooks"),
        "namespaced by its declaring plugin, exactly like the pre_tool_use path: {}",
        row.id
    );
}
