//! CLI-level acceptance test for board item `01M0XRCAFD7DD7N64RNRM3P8W9`:
//! naming a directory in `[plugins].claude_compat[]` gets its translated
//! `commands/*.md` files ACTUALLY REACHABLE from a running `conway`
//! process -- not merely translated in-memory by `conway-plugin-claude`'s
//! own library-level tests. Mirrors `tests/claude_compat_hooks.rs`'s own
//! "break-the-guard" shape (the real compiled binary via
//! `assert_cmd::cargo::cargo_bin("conway")`, an assertion that could not
//! pass by accident) applied to the command half of the same crate rather
//! than the hook half.
//!
//! **The joint this test closes, precisely.** Before this item,
//! `crates/conway-cli/src/claude_compat_plugins.rs::install` never called
//! `ClaudeCompatReport::command_registrations()` at all -- but even calling
//! it from `install` would not have been enough: `install` only ever
//! touches the one `ConwayBuilder` `main.rs::build_conway` carries, and
//! `conway::plugin::Plugin::commands()` has no reader anywhere inside the
//! facade (`ConwayBuilder::build` never looks at it). The one and only
//! reader is `conway_cli::tui::commands::CommandRegistry::build`, fed --
//! for BOTH its production call sites, the TUI and the `conway
//! <plugin-id>.<command>` external subcommand -- by
//! `first_party_plugins::installed_plugins`, which RE-DERIVES its plugin
//! list from `conway.config()` rather than reading back whatever
//! `ConwayBuilder::with_plugin` calls happened at build time. This test
//! drives that exact external-subcommand path through the compiled binary
//! (the TUI's own `/`-prefixed dispatch needs a live terminal this suite
//! does not drive; the `registry_wiring` module below exercises the
//! identical `CommandRegistry`/`parse` machinery the TUI shares, in-process,
//! for the parts a subprocess spawn cannot observe).

mod common;

use common::mock_backend::{MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};
use conway_core::log::LogRecord;
use conway_core::provenance::Provenance;

const PLUGIN_NAME: &str = "acme-tools";
const COMMAND_PROMPT: &str = "Review the diff for obvious problems.";

/// Writes a Claude Code plugin directory, inside `fixture.dir` (so it lives
/// exactly as long as the fixture itself), declaring one `commands/greet.md`
/// file with a frontmatter `description` and a body containing no
/// `$ARGUMENTS` placeholder -- the `Ready` shape `commands::read_commands`
/// translates into a real, invokable `Command`.
fn write_claude_compat_plugin_dir(fixture: &Fixture) -> std::path::PathBuf {
    let plugin_dir = fixture.dir.path().join("acme-claude-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).expect("create .claude-plugin");
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        format!(r#"{{"name":"{PLUGIN_NAME}"}}"#),
    )
    .expect("write plugin.json");
    std::fs::create_dir_all(plugin_dir.join("commands")).expect("create commands dir");
    std::fs::write(
        plugin_dir.join("commands").join("greet.md"),
        format!("---\ndescription: Greets the operator\n---\n\n{COMMAND_PROMPT}\n"),
    )
    .expect("write greet.md");
    plugin_dir
}

/// `write_fixture`'s rendered config, patched with a `[plugins].
/// claude_compat[]` entry naming the directory `write_claude_compat_plugin_dir`
/// wrote -- the identical "patch the parsed JSON in place" pattern
/// `claude_compat_hooks.rs`'s own `write_fixture_with_claude_compat_entry`
/// already uses.
fn write_fixture_with_claude_compat_entry(mock: &common::mock_backend::MockHandle) -> Fixture {
    let fixture = write_fixture(mock, 5);
    let plugin_dir = write_claude_compat_plugin_dir(&fixture);
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

/// Scans `fixture`'s session store for the single session file the
/// `conway <plugin-id>.<command>` dispatch path creates -- inlined rather
/// than shared, mirroring `claude_compat_hooks.rs::only_session_records`'s
/// own doc on why (each `tests/*.rs` integration file compiles
/// independently).
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

/// VERIFICATION ANCHOR: a `commands/*.md` file in a Claude Code plugin
/// directory named by `[plugins].claude_compat[]` is genuinely invokable
/// through the compiled `conway` binary -- driven here via
/// `commands::plugin::run`'s `conway <plugin-id>.<command>` external
/// subcommand (`tests/plugin_subcommand.rs`'s own established real-plugin
/// dispatch shape, applied to a CLAUDE-COMPAT-TRANSLATED command rather
/// than a first-party one) -- and invoking it submits its own body as a
/// real, persisted turn: a `LogRecord::UserTurn` naming the translated
/// command's own full name in its `Provenance::CommandPrompt`, not merely
/// "some prompt was submitted by *something*."
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_translated_command_is_invokable_through_the_real_binary_and_submits_its_prompt() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture_with_claude_compat_entry(&mock);
    let full_name = format!("{PLUGIN_NAME}.greet");

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
        .find(|r| matches!(r, LogRecord::UserTurn { text, .. } if text == COMMAND_PROMPT))
        .unwrap_or_else(|| {
            panic!("expected a UserTurn record carrying the translated command's own body: {records:?}")
        });
    match submitted {
        LogRecord::UserTurn { prov, .. } => match prov {
            Provenance::CommandPrompt { command } => {
                assert_eq!(
                    command, &full_name,
                    "LOAD-BEARING: the persisted record must name THIS translated command's own \
                     full name -- a submission by any other mechanism would not carry it"
                );
            }
            other => panic!(
                "expected Provenance::CommandPrompt, got {other:?} -- a translated command's \
                 turn must never be stamped as if the operator typed it"
            ),
        },
        _ => unreachable!(),
    }
}

/// A directory with NO matching `[plugins].claude_compat[]` entry (i.e. the
/// translated command's namespace was never installed) is the same
/// "unknown subcommand" outcome `plugin_subcommand.rs`'s own
/// `unknown_without_the_plugin_installed_exits_usage_and_names_the_word`
/// pins for a first-party plugin -- proving this dispatch path resolves a
/// translated command through the SAME resolver, not a special-cased one
/// that would always succeed regardless of configuration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_claude_compat_entry_the_translated_command_is_unknown() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 5);
    // Deliberately no `[plugins].claude_compat[]` entry at all.

    let unknown_full_name = format!("{PLUGIN_NAME}.greet");
    let out = run_conway(&[unknown_full_name.as_str()], &fixture);

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("{PLUGIN_NAME}.greet")),
        "stderr must name the unresolved subcommand: {stderr}"
    );
}

/// In-process coverage for the two acceptance points a subprocess spawn
/// cannot observe directly (there is no PTY harness in this suite to type
/// into a running TUI): the translated command's own palette entry, and
/// the structural guarantee that it can never shadow a built-in. Both are
/// checked against the EXACT `conway_cli::tui::commands` machinery the TUI
/// itself calls (`CommandRegistry::build`/`parse`/`resolve`), fed by the
/// SAME `claude_compat_plugins::command_plugins` the real-binary test above
/// exercises indirectly through `first_party_plugins::installed_plugins` --
/// never a second, parallel implementation of either.
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

    use super::{COMMAND_PROMPT, PLUGIN_NAME};

    /// A minimal, valid `ConwayConfig` naming one `[plugins].claude_compat[]`
    /// entry -- `command_plugins` only ever reads `config.plugins.
    /// claude_compat`, but a real `ConwayConfig` value is still required to
    /// call it, mirroring `claude_compat_plugins.rs`'s own `minimal_config`
    /// test helper (duplicated here rather than shared, since each
    /// `tests/*.rs` integration file compiles independently).
    fn config_with_claude_compat_entry(dir: &std::path::Path, entry_id: &str) -> ConwayConfig {
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
            id: entry_id.to_string(),
            dir: dir.to_path_buf(),
            timeout_ms: 5_000,
        });
        config
    }

    fn write_greet_command(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin").join("plugin.json"),
            format!(r#"{{"name":"{PLUGIN_NAME}"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("commands")).unwrap();
        std::fs::write(
            dir.join("commands").join("greet.md"),
            format!("---\ndescription: Greets the operator\n---\n\n{COMMAND_PROMPT}\n"),
        )
        .unwrap();
    }

    /// Acceptance point 3: the translated command appears in the
    /// `/help`-palette-shaped projection, namespaced (`view::palette::
    /// CommandSpec::name`'s own leading-`/` convention), and distinguishable
    /// from every built-in (none of which carry the namespace separator).
    /// Acceptance point 1, restated at this layer: typing its full,
    /// namespaced name resolves and invoking it submits its own body.
    #[tokio::test]
    async fn a_translated_command_appears_in_the_palette_and_invokes_through_the_tui_registry() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_greet_command(dir.path());

        let config = config_with_claude_compat_entry(dir.path(), PLUGIN_NAME);
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        let registry = CommandRegistry::build(&plugins).expect("registry build must succeed");

        let full_name = format!("{PLUGIN_NAME}.greet");
        let entries = registry.palette_entries();
        assert!(
            entries.iter().any(|e| e.name == format!("/{full_name}")),
            "the translated command must appear in the palette, namespaced: {entries:?}"
        );
        assert!(
            entries.iter().all(|e| !e.name.contains("/help")),
            "must not be confusable with the built-in /help: {entries:?}"
        );

        let parsed =
            parse(&format!("/{full_name}")).expect("parse must accept the namespaced form");
        let SlashCommand::Plugin {
            full_name: parsed_name,
            ..
        } = parsed
        else {
            panic!("expected SlashCommand::Plugin, got {parsed:?}");
        };
        assert_eq!(parsed_name, full_name);

        let command = registry
            .resolve(&parsed_name)
            .expect("registry must resolve the translated command");
        let ctx = conway::plugin::CommandCtx {
            focused_agent: conway_core::ids::AgentId::new(),
            root_agent: conway_core::ids::AgentId::new(),
            session_id: conway_core::ids::SessionId::new(),
            args: String::new(),
        };
        let outcome = command.invoke(ctx).await;
        assert_eq!(
            outcome,
            conway::plugin::CommandOutcome::SubmitPrompt {
                text: COMMAND_PROMPT.to_string()
            }
        );
    }

    /// Acceptance point 4, adversarial: a translated command whose OWN bare
    /// name is a built-in's word (`help`) cannot shadow it -- typing the
    /// bare built-in word still resolves to the built-in, and the
    /// translated command is reachable ONLY at its namespaced full name.
    /// Structural (per `CommandRegistry::build`'s own doc: no built-in word
    /// contains the namespace separator), checked directly rather than only
    /// asserted in prose.
    #[tokio::test]
    async fn a_translated_command_named_like_a_built_in_cannot_shadow_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(".claude-plugin").join("plugin.json"),
            format!(r#"{{"name":"{PLUGIN_NAME}"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("commands")).unwrap();
        std::fs::write(
            dir.path().join("commands").join("help.md"),
            "Not the real /help.\n",
        )
        .unwrap();

        let config = config_with_claude_compat_entry(dir.path(), PLUGIN_NAME);
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        let registry = CommandRegistry::build(&plugins).expect("registry build must succeed");

        // Typing the bare built-in word still resolves to the built-in.
        assert_eq!(parse("/help"), Ok(SlashCommand::Help));

        // The translated command is reachable ONLY at its namespaced form.
        assert!(registry.resolve("help").is_none());
        assert!(registry.resolve(&format!("{PLUGIN_NAME}.help")).is_some());
    }
}
