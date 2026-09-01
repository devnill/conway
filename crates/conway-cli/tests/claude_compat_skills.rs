//! CLI-level acceptance test for board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`:
//! `skills/<name>/SKILL.md` translation, exercised against a REAL Claude
//! Code plugin's own layout rather than a synthetic one-skill fixture --
//! this item's own trap list names the reason explicitly: "a test that
//! avoids the path under test proves nothing," and a synthetic fixture
//! cannot exercise the cross-reference case (a skill's own body naming a
//! SIBLING file "relative to the plugin root"), which is the one most
//! likely to break.
//!
//! `tests/fixtures/claude_compat_ideate/` is a checked-in COPY of
//! `ideate` 3.2.2's own real `skills/`/`agents/` directories (not a live
//! read of the operator's own `~/.conway/plugins/...` -- that path is
//! machine-specific and this suite must run anywhere), taken verbatim so
//! this test proves out against the actual corpus the operator's own
//! trigger report (`/ideate:refine` -> "unknown command") named.
//!
//! Mirrors `claude_compat_commands.rs`'s own "break-the-guard" shape (the
//! real compiled binary via `run_conway`, an assertion that could not pass
//! by accident) applied to the skill half rather than the command half.

mod common;

use common::mock_backend::{MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};
use conway_core::log::LogRecord;
use conway_core::provenance::Provenance;

const PLUGIN_NAME: &str = "ideate";

/// The checked-in fixture directory, absolute, resolved against this
/// crate's own manifest dir so the test runs correctly regardless of the
/// process's own current working directory.
fn fixture_plugin_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_compat_ideate")
}

/// `write_fixture`'s rendered config, patched with a `[plugins].
/// claude_compat[]` entry naming the checked-in real `ideate` fixture --
/// mirrors `claude_compat_commands.rs::write_fixture_with_claude_compat_
/// entry`'s own "patch the parsed JSON in place" pattern exactly.
fn write_fixture_with_ideate_entry(mock: &common::mock_backend::MockHandle) -> Fixture {
    let fixture = write_fixture(mock, 5);
    let plugin_dir = fixture_plugin_dir();
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    value["plugins"] = serde_json::json!({
        "claude_compat": [
            { "id": PLUGIN_NAME, "dir": plugin_dir.display().to_string(), "timeout_ms": 5_000 }
        ]
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize fixture config"),
    )
    .expect("rewrite fixture config");
    fixture
}

fn only_session_records(fixture: &Fixture) -> Vec<LogRecord> {
    let dir = common::session_dir(fixture);
    let mut found: Option<std::path::PathBuf> = None;
    for entry in std::fs::read_dir(&dir).expect("read sessions dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "index" {
            continue;
        }
        assert!(
            found.is_none(),
            "expected exactly one session file in {}, also found {stem}",
            dir.display()
        );
        found = Some(path);
    }
    let path = found.unwrap_or_else(|| panic!("no session file found in {}", dir.display()));
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read session jsonl at {}: {e}", path.display()));
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<LogRecord>(line)
                .unwrap_or_else(|e| panic!("parse LogRecord: {e}; line: {line}"))
        })
        .collect()
}

/// **VERIFICATION ANCHOR (acceptance 1 and 2, together).** `ideate`'s real
/// `skills/refine/SKILL.md` -- the operator's OWN named trigger
/// (`/ideate:refine`) -- resolves and runs through the compiled binary
/// (`conway ideate.refine`, this crate's own bare-name + host-namespacing
/// scheme), and the SUBMITTED prompt still carries the skill's own
/// cross-reference to `skills/shared/human-presentation.md` "relative to
/// the plugin root" -- together with enough information (this plugin's own
/// absolute root directory) that the reference actually resolves to a real
/// file, proving the cross-reference survives translation rather than
/// merely surviving a parse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ideates_real_refine_skill_resolves_runs_and_keeps_its_cross_reference() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture_with_ideate_entry(&mock);
    let full_name = format!("{PLUGIN_NAME}.refine");

    let out = run_conway(&[full_name.as_str()], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("submitted a prompt"),
        "stdout should report the real submitted-prompt outcome: {stdout}"
    );

    let records = only_session_records(&fixture);
    let submitted = records
        .iter()
        .find_map(|r| match r {
            LogRecord::UserTurn { text, prov, .. } => Some((text, prov)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a UserTurn record: {records:?}"));
    let (text, prov) = submitted;

    // The real skill's own heading -- proves the ACTUAL SKILL.md body was
    // submitted, not a placeholder.
    assert!(text.contains("# ideate:refine"), "{text}");
    // The literal cross-reference survives, untouched.
    assert!(
        text.contains("skills/shared/human-presentation.md"),
        "{text}"
    );
    // And the plugin's own absolute root is present, so joining the two
    // resolves to a REAL, existing file.
    let plugin_dir = fixture_plugin_dir();
    assert!(
        text.contains(&plugin_dir.display().to_string()),
        "the plugin's own absolute root must be named so the reference resolves: {text}"
    );
    let resolved = plugin_dir.join("skills/shared/human-presentation.md");
    assert!(
        resolved.is_file(),
        "the referenced sibling must actually exist on disk"
    );

    match prov {
        Provenance::CommandPrompt { command } => {
            assert_eq!(command, &full_name);
        }
        other => panic!("expected Provenance::CommandPrompt, got {other:?}"),
    }
}

/// In-process coverage for the parts a subprocess spawn cannot observe --
/// mirrors `claude_compat_commands.rs::registry_wiring`'s own split.
mod registry_wiring {
    use std::collections::BTreeMap;

    use conway::config::schema::{
        AgentsConfig, ClaudeCompatPluginEntry, ConwayConfig, HealthSection, HooksConfig,
        LimitsConfig, ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection,
        SessionConfig, ToolsConfig,
    };
    use conway_cli::claude_compat_plugins::command_plugins;
    use conway_cli::tui::commands::{parse, CommandRegistry, SlashCommand};
    use conway_core::ids::RoleAlias;

    use super::{fixture_plugin_dir, PLUGIN_NAME};

    fn config_with_ideate_entry() -> ConwayConfig {
        let mut roles = BTreeMap::new();
        roles.insert(
            "default".to_string(),
            RoleEntry {
                chain: vec![],
                headroom_tokens: None,
                ..Default::default()
            },
        );
        let mut config = ConwayConfig {
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
            tools: ToolsConfig::default(),
            plugins: PluginsConfig::default(),
            hooks: HooksConfig::default(),
        };
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: PLUGIN_NAME.to_string(),
            dir: fixture_plugin_dir(),
            timeout_ms: 5_000,
        });
        config
    }

    /// Acceptance 1, restated precisely: the operator's OWN typed form
    /// (`/ideate:refine`, Claude Code's own separator) parses into the
    /// SAME full name the registry resolves under.
    #[test]
    fn the_operators_own_typed_colon_form_resolves_to_the_registered_full_name() {
        let config = config_with_ideate_entry();
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        let registry = CommandRegistry::build(&plugins).expect("registry build must succeed");

        let parsed = parse("/ideate:refine").expect("parse must accept the colon form");
        let SlashCommand::Plugin { full_name, .. } = parsed else {
            panic!("expected SlashCommand::Plugin, got {parsed:?}");
        };
        assert_eq!(full_name, "ideate.refine");
        assert!(
            registry.resolve(&full_name).is_some(),
            "the colon-typed form must resolve to a real, registered command"
        );
    }

    /// Every real, translatable skill (`autopilot`, `execute`, `init`,
    /// `refine`, `review` -- `shared` is not a skill: it has no `SKILL.md`
    /// of its own) is registered, namespaced, and distinct from every
    /// built-in.
    #[test]
    fn every_real_ideate_skill_is_registered_and_namespaced() {
        let config = config_with_ideate_entry();
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        let registry = CommandRegistry::build(&plugins).expect("registry build must succeed");

        let entries = registry.palette_entries();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        for skill in ["autopilot", "execute", "init", "refine", "review"] {
            let full = format!("/ideate.{skill}");
            assert!(names.contains(&full.as_str()), "{names:?}");
        }
        assert!(
            !names.contains(&"/ideate.shared"),
            "`shared` has no SKILL.md of its own and must not be registered: {names:?}"
        );
    }
}
