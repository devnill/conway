//! Reads a Claude Code plugin directory that is already on disk (board item
//! `01M0VR89FB1F3Q4FQ8852K2A5E`): the translation layer that lets conway
//! read such a directory an operator already has on their machine, and
//! surface what it finds alongside conway's own plugins. **No downloading**
//! -- the operator points conway at a directory, this crate reads it.
//!
//! ## Question 1: read-at-runtime, not translate-and-write
//!
//! **Read-at-runtime, chosen.** [`discover`] re-parses the directory every
//! time it is called; nothing this crate does ever writes to the
//! operator's own `settings.json` or any other file. Two arguments, and
//! they agree:
//!
//! - **Provenance.** `/plugin` (`crates/conway-cli/src/tui/view/plugins.rs`)
//!   must show a Claude-format plugin's own origin, honestly, alongside a
//!   native one. A translate-and-write approach that copied entries into
//!   the operator's config would leave those entries indistinguishable from
//!   ones the operator wrote by hand -- "nothing records where this came
//!   from" is a direct conflict with the very origin requirement that
//!   motivates this item.
//! - **No config writer to build one on.** Translate-and-write needs a real
//!   array-entry config writer. `crates/conway/src/config/writer.rs`
//!   deliberately has none -- it patches one id in `plugins.install` via a
//!   hand-rolled text edit, chosen SPECIFICALLY to preserve the operator's
//!   own formatting rather than parse-and-reserialize. Building a real
//!   writer is a materially bigger, separate item.
//!
//! Read-at-runtime keeps the artifact -- the directory the operator already
//! has -- intact, reversible (delete the `[plugins].claude_compat[]` entry
//! and the translation vanishes; nothing was ever written anywhere), and
//! traceable to its own origin every time it is read.
//!
//! ## Question 2: foreign frontmatter, parsed permissively
//!
//! Every file this crate reads goes through `serde_json::Value` and pulls
//! only the fields it uses (see [`manifest`], [`mcp`], [`hooks`]) rather
//! than a `#[serde(deny_unknown_fields)]` struct -- an unrecognized Claude
//! Code key is simply never looked at, never a hard parse failure. This is
//! DELIBERATELY NOT how `crates/conway/src/skills.rs`/`agents.rs` parse
//! conway's OWN `.conway/skills`/`.conway/agents` frontmatter, and that
//! difference is the point: that strictness catches an operator's own typo
//! in a file conway itself defines the shape of, which this file is not.
//!
//! ## Question 3: skills and agents are OUT OF SCOPE, named and reasoned
//!
//! Neither `skills/<name>/SKILL.md` nor `agents/*.md` is imported by this
//! item. `crates/conway/src/skills.rs` hardcodes a single `.conway/skills`
//! root (`builder.rs`); `AgentsConfig::dir` is a single `PathBuf`, not a
//! list. Making either multi-rooted is a real, separate change (widening a
//! public config field, touching the loader, its own test coverage) --
//! GP-04's own "thin demonstrable slices" argues for scoping this item down
//! to what a directory-read layer can prove out cleanly rather than
//! shipping a fourth translated kind that is only partially wired. Every
//! `skills/<name>/SKILL.md` and `agents/*.md` found is still NAMED, in
//! [`ClaudeCompatReport::unsupported`] -- never silently skipped.
//!
//! **`commands/*.md` was ALSO out of scope, for a different, narrower
//! reason -- corrected here, by board item `01M0X1G29EZSFEWB1YAG40SE69`.**
//! `conway_core::ports::CommandOutcome::SubmitPrompt` existed (a *later*
//! item than the one this crate's own spec was originally filed against)
//! but wiring a Claude Code command file to it was explicitly deferred to a
//! separate follow-up belonging to neither item alone. **That follow-up is
//! this one:** `commands::read_commands` translates most `commands/*.md`
//! files into real, invokable [`conway_core::ports::Command`]s
//! ([`commands::ClaudeCommand`]) -- see that module's own doc for the full
//! "best effort, two things survive the relaxation" appetite (unsupported
//! frontmatter keys named, `allowed-tools` above all; a raw `$ARGUMENTS`
//! placeholder refused, never submitted verbatim). A `commands/*.md` file
//! is now named in `unsupported` only when it did NOT translate --
//! [`UnsupportedKind::CommandFrontmatterKey`] separately names an ignored
//! frontmatter key even on a file that otherwise translated successfully.
//!
//! ## What this crate does NOT do
//!
//! - No network access of any kind (C-04/acceptance 7) -- every read in
//!   this crate is local disk I/O (`fsutil::read_bounded`).
//! - **No new `conway_core::ports::plugin` surface, and no new
//!   `CommandOutcome` variant.** This crate now DOES depend on
//!   `conway-core` in production code -- [`commands::ClaudeCommand`]
//!   implements `conway_core::ports::Command`, returning the ALREADY
//!   EXISTING `CommandOutcome::SubmitPrompt` -- correcting an earlier
//!   version of this bullet's "this crate does not depend on `conway-core`
//!   at all" (true only under the earlier hooks/MCP-only scope). Still
//!   never `conway`, the facade: `Command`/`CommandOutcome` live one layer
//!   below it, the same foundational tier `conway-plugin-mcp` itself sits
//!   on (no cycle risk).
//! - **No `HooksConfig` mutation BY THIS CRATE, ever** -- this crate never
//!   holds, reads, or writes a `conway::config::schema::HooksConfig` of any
//!   kind (it does not depend on `conway` in production code at all, the
//!   identical reason [`mcp::TranslatedMcpServer`] hands back a
//!   `conway_plugin_mcp::McpPluginSpec` rather than reaching into `conway`
//!   itself). Board item `01M0X1FCQ80C9ET97HENXSAW2K` corrected the
//!   NARROWER claim this bullet used to make ("name-level match only,
//!   nothing is ever wired to dispatch"):
//!   [`hooks::HookTranslation::registration`],
//!   [`ClaudeCompatReport::hook_registrations`] now produce
//!   real, dispatchable `[hooks].rules[]`-shaped registrations for every
//!   `Mapped` rule -- a CALLER (an embedder, or a future `conway-cli`
//!   wiring point) appends them into ITS OWN `HooksConfig` before
//!   `ConwayBuilder::build`; this crate still never touches one itself.
//!   See [`hooks`]'s own module doc for the full "dispatches, but is not
//!   the same claim as behaves identically to Claude Code" disclosure.

pub mod commands;
pub mod error;
mod fsutil;
pub mod hooks;
pub mod manifest;
pub mod mcp;
pub mod unsupported;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use commands::{ClaudeCommand, CommandMapOutcome, CommandTranslation};
pub use error::ClaudeCompatError;
pub use hooks::{HookMapOutcome, HookRegistration, HookTranslation};
pub use manifest::ClaudePluginManifest;
pub use mcp::TranslatedMcpServer;
pub use unsupported::{UnsupportedItem, UnsupportedKind};

/// The full translation result for one Claude Code plugin directory --
/// everything [`discover`] found, split into what conway can use and what
/// it cannot (acceptance 5: nothing unusable is silently dropped).
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeCompatReport {
    /// This plugin's identity: `.claude-plugin/plugin.json`'s own `name`
    /// when present, else `source_dir`'s own directory name.
    pub id: String,
    pub source_dir: PathBuf,
    pub manifest: Option<ClaudePluginManifest>,
    /// Every `.mcp.json` server entry that translated cleanly -- ready for
    /// [`TranslatedMcpServer::into_spec`]/`conway_plugin_mcp::McpPlugin::
    /// discover`. This is the ONLY kind acceptance 2 requires to actually
    /// work end to end.
    pub mcp_servers: Vec<TranslatedMcpServer>,
    /// Every `hooks/hooks.json` rule, name-level-mapped or not -- see
    /// [`hooks`]'s own module doc for why a `Mapped` outcome here is not a
    /// claim that the rule runs.
    pub hooks: Vec<HookTranslation>,
    /// Every `commands/*.md` file, translated or refused -- see
    /// [`commands`]'s own module doc. [`Self::command_registrations`] is
    /// the ready-to-append real-`Command` form of the `Ready` subset.
    pub commands: Vec<CommandTranslation>,
    /// Everything found and not used: every unimported skill/agent, every
    /// unmapped hook event, every malformed `.mcp.json` entry, every
    /// `commands/*.md` file that did not translate, and every ignored
    /// `commands/*.md` frontmatter key (even on a file that DID translate)
    /// -- named, with a reason, never silently dropped.
    pub unsupported: Vec<UnsupportedItem>,
}

impl ClaudeCompatReport {
    /// The count of `hooks/hooks.json` rules whose event has a same-named
    /// conway counterpart -- see [`Self::hook_registrations`] for the
    /// actual, dispatchable form of these same rules.
    pub fn mapped_hook_count(&self) -> usize {
        self.hooks
            .iter()
            .filter(|h| matches!(h.outcome, HookMapOutcome::Mapped { .. }))
            .count()
    }

    /// The count of `hooks/hooks.json` rules with no conway counterpart at
    /// all -- named individually in [`Self::unsupported`].
    pub fn unmapped_hook_count(&self) -> usize {
        self.hooks.len() - self.mapped_hook_count()
    }

    /// Every `Mapped` `hooks/hooks.json` rule, as a ready-to-append
    /// `[hooks].rules[]`-shaped [`HookRegistration`] -- see
    /// [`hooks::HookTranslation::registration`]'s own doc for the shell
    /// wrapping and `${CLAUDE_PLUGIN_ROOT}` substitution every registration
    /// here already carries. `Unmapped` rules contribute nothing (already
    /// named in [`Self::unsupported`]).
    ///
    /// Each registration's own `id` is
    /// `"claude_compat:<self.id>:<claude_event>:<n>"`, `n` a stable
    /// per-event ordinal (this rule's own position among that SAME Claude
    /// Code event's rules in `hooks.json`) -- unique within one report, and
    /// namespaced by this plugin's own `id` so two
    /// `[plugins].claude_compat[]` entries can never collide even if both
    /// name the identical Claude Code event.
    pub fn hook_registrations(&self) -> Vec<HookRegistration> {
        let mut seen_for_event: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        self.hooks
            .iter()
            .filter_map(|translation| {
                let ordinal = seen_for_event
                    .entry(translation.claude_event.as_str())
                    .or_insert(0);
                let id = format!(
                    "claude_compat:{}:{}:{ordinal}",
                    self.id, translation.claude_event
                );
                *ordinal += 1;
                translation.registration(id, &self.source_dir)
            })
            .collect()
    }

    /// Every `Ready` `commands/*.md` translation, as a real, invokable
    /// `conway_core::ports::Command` -- ready to fold into a
    /// `conway_core::ports::Plugin::commands()` implementation, mirroring
    /// [`Self::hook_registrations`]'s own "the ready-to-append shape"
    /// precedent for the command surface instead of the hook one. A
    /// `Refused` translation contributes nothing here -- it already named
    /// itself in [`Self::unsupported`].
    pub fn command_registrations(&self) -> Vec<Arc<dyn conway_core::ports::Command>> {
        self.commands
            .iter()
            .filter_map(CommandTranslation::command)
            .collect()
    }
}

/// Reads `dir` as a Claude Code plugin directory, translating what conway
/// can use and naming everything it cannot (acceptance 5). Fails closed on
/// a directory that does not exist, and on any file this crate reads that
/// is malformed (P-13: a directory that cannot be read correctly is a named
/// error, never a silently partial result) -- see [`ClaudeCompatError`] for
/// every failure mode.
///
/// A `.claude-plugin/plugin.json` is NOT required to be present: `dir` is
/// read permissively as "whatever is actually there," per question 2's own
/// posture.
pub fn discover(dir: &Path) -> Result<ClaudeCompatReport, ClaudeCompatError> {
    if !dir.is_dir() {
        return Err(ClaudeCompatError::NotADirectory(dir.to_path_buf()));
    }

    let manifest = manifest::read_manifest(dir)?;
    let id = manifest
        .as_ref()
        .and_then(|m| m.name.clone())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "claude-plugin".to_string());

    let (mcp_servers, mut unsupported) = mcp::read_mcp_servers(dir)?;
    let hooks = hooks::read_hooks(dir, &mut unsupported)?;
    let commands = commands::read_commands(dir, &mut unsupported);
    unsupported::scan_skills(dir, &mut unsupported);
    unsupported::scan_agents(dir, &mut unsupported);

    Ok(ClaudeCompatReport {
        id,
        source_dir: dir.to_path_buf(),
        manifest,
        mcp_servers,
        hooks,
        commands,
        unsupported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonexistent_directory_is_a_typed_error_not_a_panic() {
        let err = discover(Path::new("/does/not/exist/at/all")).unwrap_err();
        assert!(
            matches!(err, ClaudeCompatError::NotADirectory(_)),
            "{err:?}"
        );
    }

    #[test]
    fn a_plain_file_is_not_a_directory_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        let err = discover(&file).unwrap_err();
        assert!(
            matches!(err, ClaudeCompatError::NotADirectory(_)),
            "{err:?}"
        );
    }

    #[test]
    fn an_empty_directory_reports_nothing_and_no_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let report = discover(dir.path()).unwrap();
        assert!(report.mcp_servers.is_empty());
        assert!(report.hooks.is_empty());
        assert!(report.commands.is_empty());
        assert!(report.command_registrations().is_empty());
        assert!(report.unsupported.is_empty());
        // No manifest -- identity falls back to the directory's own name.
        assert!(report.manifest.is_none());
        assert!(!report.id.is_empty());
    }

    /// The full-directory composition: every kind this crate knows about,
    /// present at once, each landing in the right bucket. This is
    /// acceptance 5's own demonstration shape -- an operator reading one
    /// `ClaudeCompatReport` can tell exactly what worked and what did not.
    #[test]
    fn a_directory_with_every_kind_composes_correctly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"acme-tools","version":"2.0.0"}"#,
        )
        .unwrap();

        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"acme-search":{"command":"acme-mcp","args":["--stdio"]}}}"#,
        )
        .unwrap();

        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(
            root.join("hooks").join("hooks.json"),
            r#"{"hooks":{
                "PreToolUse": [{"matcher":"Bash","hooks":[{"type":"command","command":"echo pre"}]}],
                "Stop": [{"hooks":[{"type":"command","command":"echo stop"}]}]
            }}"#,
        )
        .unwrap();

        std::fs::create_dir_all(root.join("commands")).unwrap();
        // Empty body -- refuses to translate (still named, in
        // `unsupported`, exactly as the pre-wiring version of this test
        // already asserted).
        std::fs::write(root.join("commands").join("review.md"), "").unwrap();

        std::fs::create_dir_all(root.join("skills").join("triage")).unwrap();
        std::fs::write(root.join("skills").join("triage").join("SKILL.md"), "").unwrap();

        std::fs::create_dir_all(root.join("agents")).unwrap();
        std::fs::write(root.join("agents").join("reviewer.md"), "").unwrap();

        let report = discover(root).unwrap();
        assert_eq!(report.id, "acme-tools");
        assert_eq!(report.mcp_servers.len(), 1);
        assert_eq!(report.mcp_servers[0].name, "acme-search");
        assert_eq!(report.hooks.len(), 2);
        assert_eq!(report.mapped_hook_count(), 1);
        assert_eq!(report.unmapped_hook_count(), 1);
        assert_eq!(report.commands.len(), 1);
        assert!(
            report.command_registrations().is_empty(),
            "an empty command body must refuse to translate, not register a no-op command"
        );

        let unsupported_names: Vec<_> =
            report.unsupported.iter().map(|u| u.name.as_str()).collect();
        assert!(unsupported_names.contains(&"commands/review.md"));
        assert!(unsupported_names.contains(&"skills/triage"));
        assert!(unsupported_names.contains(&"agents/reviewer.md"));
        assert!(unsupported_names.contains(&"Stop"));
        // Exactly these four -- nothing else silently missing, nothing
        // spuriously extra.
        assert_eq!(report.unsupported.len(), 4);
    }

    #[test]
    fn an_id_falls_back_to_the_directory_name_when_no_manifest_names_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let named = dir.path().join("my-claude-plugin");
        std::fs::create_dir_all(&named).unwrap();
        let report = discover(&named).unwrap();
        assert_eq!(report.id, "my-claude-plugin");
    }

    /// `hook_registrations` produces exactly one [`HookRegistration`] per
    /// `Mapped` rule, skips every `Unmapped` one, and assigns each a
    /// unique, namespaced, per-event-ordinal `id`.
    #[test]
    fn hook_registrations_are_namespaced_by_plugin_id_and_per_event_ordinal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"acme-tools"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(
            root.join("hooks").join("hooks.json"),
            r#"{"hooks":{
                "PreToolUse": [
                    {"matcher":"Bash","hooks":[{"type":"command","command":"echo one"}]},
                    {"matcher":"Read","hooks":[{"type":"command","command":"echo two"}]}
                ],
                "Stop": [{"hooks":[{"type":"command","command":"echo bye"}]}]
            }}"#,
        )
        .unwrap();

        let report = discover(root).unwrap();
        let registrations = report.hook_registrations();
        // Two `PreToolUse` rules mapped; `Stop` contributes nothing.
        assert_eq!(registrations.len(), 2);
        assert!(registrations.iter().all(|r| r.event == "pre_tool_use"));

        let ids: Vec<&str> = registrations.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "claude_compat:acme-tools:PreToolUse:0",
                "claude_compat:acme-tools:PreToolUse:1",
            ],
            "each rule's own id must be unique and stable-ordered: {ids:?}"
        );
    }

    /// `command_registrations` produces exactly one real `Command` per
    /// `Ready` translation, skips a `Refused` sibling entirely, and every
    /// produced `Command`'s own `spec().name` is bare (see
    /// `commands`'s own module doc, "Namespacing").
    #[tokio::test]
    async fn command_registrations_are_the_ready_subset_with_bare_names() {
        // NOTE: this test carried a `use conway_core::ports::Command as _;`
        // on the reasoning that the trait must be in scope to call `spec()`
        // on an `Arc<dyn Command>`. It does not: the methods resolve through
        // the trait object itself, so the import was unused and tripped
        // `-D warnings`. Removed rather than `#[allow]`-ed.
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"acme-tools"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("commands")).unwrap();
        std::fs::write(
            root.join("commands").join("greet.md"),
            "---\ndescription: Greets the operator\n---\n\nSay a friendly hello.\n",
        )
        .unwrap();
        // Refused (empty body) -- must contribute nothing to
        // `command_registrations`.
        std::fs::write(root.join("commands").join("blank.md"), "").unwrap();

        let report = discover(root).unwrap();
        assert_eq!(report.commands.len(), 2);
        let registrations = report.command_registrations();
        assert_eq!(registrations.len(), 1);

        let spec = registrations[0].spec();
        assert_eq!(spec.name, "greet");
        assert_eq!(spec.summary, "Greets the operator");

        let ctx = conway_core::ports::CommandCtx {
            focused_agent: conway_core::ids::AgentId::new(),
            root_agent: conway_core::ids::AgentId::new(),
            session_id: conway_core::ids::SessionId::new(),
            args: String::new(),
        };
        let outcome = registrations[0].invoke(ctx).await;
        assert_eq!(
            outcome,
            conway_core::ports::CommandOutcome::SubmitPrompt {
                text: "Say a friendly hello.".to_string()
            }
        );
    }
}
