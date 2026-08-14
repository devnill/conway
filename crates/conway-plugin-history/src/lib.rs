//! `conway-plugin-history`: `/conway.history.rewind`, a first-party plugin
//! (board item 01KZY8Q1CMMNVSF54CTC270N3H) proving `/rewind` genuinely IS a
//! plugin, per the owner's ruling: "features like /rewind, /checkout, etc
//! are to be plugins, to fit into the philosophy; they are not core
//! functionality." Not installed by default -- see
//! [`docs/getting-started.md`](../../../docs/getting-started.md#installing-a-first-party-plugin)
//! for the `[plugins].install` opt-in, mirroring
//! `crates/conway-plugin-skeleton`'s own pattern exactly.
//!
//! **What this crate proves.** `CommandOutcome::ForkSession` (board item
//! 01KZYH37WNDKDWSMWQQPRFKKXC) closed the ONE gap that used to block
//! `/rewind` from being a plugin at all: a command could not fork or
//! retarget the session driving it, because `conway-core` (where `Plugin`/
//! `Command` live) structurally cannot depend on `conway` (the facade,
//! where the fork capability lives) without a cycle. This crate is that
//! gap's first real consumer, written entirely against `conway::plugin` --
//! the identical public surface a third-party plugin author gets, exactly
//! like `conway-plugin-skeleton`. `/conway.history.rewind 42` forks the
//! CALLING session at seq 42 and hands the TUI its child, with the
//! parent's own append-only log never mutated (`Conway::fork_from`'s own
//! zero-copy-by-reference contract) --
//! `crates/conway-cli/tests/rewind_history_plugin.rs` proves this end to
//! end against the real compiled dispatch path, not merely this crate's own
//! trait-level tests.
//!
//! **What this crate deliberately does NOT build, and why.** [`CommandCtx`]
//! grants no transcript-read capability (`docs/plugins/hooks.md` point 15's
//! own disclosure) -- a command can read `ctx.args`, `ctx.session_id`,
//! `ctx.focused_agent`, `ctx.root_agent`, and nothing live beyond that. So
//! `/conway.history.rewind <seq>` -- an EXPLICIT sequence number the
//! operator already typed -- is the whole of what this crate builds.
//! Resolving free text ("go back three turns", "before the bad edit")
//! would require RESOLVING that request against the session's own history
//! first, and there is nothing on `CommandCtx` this command could read to
//! do that with. That is not a gap this crate papers over with a private
//! door into `conway-core`/`conway-cli` internals (the module doc of
//! `conway::plugin` forbids exactly that shortcut, and a first-party
//! plugin earns no exemption from it) -- it is a disclosed, separately-
//! justified capability this item does not build ahead of a real need
//! (YAGNI), the same restraint `docs/plugins/hooks.md`'s own "Forking the
//! calling session" subsection names for this exact command.
//!
//! **Is `/conway.history.rewind <seq>` genuinely usable, given that limit?**
//! Only if a seq is something an operator can actually see -- a command
//! that takes a number nobody is ever shown is a demo, not a capability.
//! Before this item, no TUI surface showed one at all (the live event
//! stream's own `Envelope::seq` is a per-connection renumbering, NOT the
//! persisted `LogSeq` `Conway::fork_from` accepts -- see that field's own
//! doc for why using it here would have been actively misleading). This
//! item closes that gap too, using data the TUI ALREADY has and with no
//! new port: the status line's existing `session <id>` field
//! (`crates/conway-cli/src/tui/view/status.rs`) now reads `session
//! <id>@<seq>`, the exact `<session-id>[@<seq>]` syntax
//! `crates/conway-cli/src/session_ref.rs`'s `--fork-from` flag already
//! established, kept authoritative via `Conway::session_head` (the same
//! facade call `SlashCommand::Resume`'s own reset path already uses) at
//! session start, after every root-agent turn boundary, and immediately
//! after a fork (where the new head is simply `at_seq` itself -- no extra
//! round trip needed). With that, an operator can watch the number grow as
//! they work and type `/conway.history.rewind <n>` to undo back to a turn
//! boundary they saw pass -- real, if coarse (turn-boundary, not
//! `keystroke`, granularity): a capability, not merely a mechanism proven
//! to exist.

use std::sync::Arc;

use conway::plugin::{
    async_trait, Command, CommandCtx, CommandOutcome, CommandSpec, Plugin, PluginManifest,
};
use conway::LogSeq;

/// This plugin's manifest id: the string an operator names in
/// `[plugins].install` (`settings.json`) or a caller matches by hand before
/// calling `ConwayBuilder::with_plugin`.
pub const PLUGIN_ID: &str = "conway.history";

/// The bare name [`RewindCommand`] registers under -- reachable in the TUI
/// as `/{PLUGIN_ID}.{COMMAND_NAME}`, i.e. `/conway.history.rewind`
/// (`conway_cli::tui::commands::CommandRegistry::build` prefixes it with
/// this plugin's own manifest id).
pub const COMMAND_NAME: &str = "rewind";

/// `/conway.history.rewind <seq>`: parses `ctx.args` as a bare, non-negative
/// integer [`LogSeq`] and asks the host to fork the CALLING session there
/// (see this crate's own module doc for the full "why only an explicit
/// seq" reasoning). Any non-numeric or empty argument is a
/// [`CommandOutcome::Error`] naming exactly what was typed, never a panic.
struct RewindCommand;

#[async_trait]
impl Command for RewindCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME.to_string(),
            summary: "forks this session at an explicit sequence number, e.g. \
                      `/conway.history.rewind 42` -- see the `session <id>@<seq>` status line \
                      field for the current head"
                .to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let trimmed = ctx.args.trim();
        match trimmed.parse::<u64>() {
            Ok(n) => CommandOutcome::ForkSession {
                at_seq: LogSeq(n),
                // Undirected: this command carries no free-text resolution
                // capability to build one from (module doc, above) -- the
                // child simply resumes from `at_seq` with no extra
                // instruction, exactly [`CommandOutcome::ForkSession::
                // directive`]'s own documented "empty is legal" case.
                directive: String::new(),
            },
            Err(_) => CommandOutcome::Error(format!(
                "usage: /{PLUGIN_ID}.{COMMAND_NAME} <seq> -- expected a non-negative integer \
                 sequence number, got {trimmed:?}"
            )),
        }
    }
}

/// The plugin itself. `Default` so a caller (this crate's own tests,
/// `conway-cli`'s first-party bundle) constructs it with no arguments,
/// matching every built-in's own zero-config construction.
#[derive(Default)]
pub struct HistoryPlugin;

impl Plugin for HistoryPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            // Versioned WITH the workspace -- see this crate's own
            // Cargo.toml doc comment.
            version: env!("CARGO_PKG_VERSION").to_string(),
            // No tools: this plugin's entire surface is the one TUI slash
            // command below. `PluginManifest::tools` names only what
            // `Plugin::tools()` actually returns -- an empty `Vec` here,
            // never a stub.
            tools: vec![],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
        Vec::new()
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        vec![Arc::new(RewindCommand)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(args: &str) -> CommandCtx {
        CommandCtx {
            focused_agent: conway::AgentId::new(),
            root_agent: conway::AgentId::new(),
            session_id: conway::SessionId::new(),
            args: args.to_string(),
        }
    }

    #[tokio::test]
    async fn rewind_parses_a_bare_seq_into_a_fork_session_outcome() {
        let command = RewindCommand;
        let outcome = command.invoke(ctx("7")).await;
        assert_eq!(
            outcome,
            CommandOutcome::ForkSession {
                at_seq: LogSeq(7),
                directive: String::new(),
            }
        );
    }

    /// Leading/trailing whitespace (e.g. a stray trailing space from typing)
    /// must not turn a well-formed seq into a parse error.
    #[tokio::test]
    async fn rewind_trims_surrounding_whitespace() {
        let command = RewindCommand;
        let outcome = command.invoke(ctx("  12  ")).await;
        assert_eq!(
            outcome,
            CommandOutcome::ForkSession {
                at_seq: LogSeq(12),
                directive: String::new(),
            }
        );
    }

    #[tokio::test]
    async fn rewind_zero_is_a_valid_seq() {
        let command = RewindCommand;
        let outcome = command.invoke(ctx("0")).await;
        assert_eq!(
            outcome,
            CommandOutcome::ForkSession {
                at_seq: LogSeq(0),
                directive: String::new(),
            }
        );
    }

    /// **The discriminating half of this command's own contract**: anything
    /// that is not a bare non-negative integer -- free text, a negative
    /// number, empty input -- is a named [`CommandOutcome::Error`], never a
    /// panic and never a silent fallback to some default seq. This is the
    /// direct consequence of `CommandCtx` granting no capability to resolve
    /// free text against the session's own history (module doc).
    #[tokio::test]
    async fn rewind_rejects_non_numeric_arguments_with_a_named_error() {
        let command = RewindCommand;
        let outcome = command.invoke(ctx("before the bad edit")).await;
        match outcome {
            CommandOutcome::Error(message) => {
                assert!(message.contains("before the bad edit"), "{message}");
                assert!(message.contains("usage"), "{message}");
            }
            other => panic!("expected CommandOutcome::Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rewind_rejects_empty_arguments() {
        let command = RewindCommand;
        let outcome = command.invoke(ctx("")).await;
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    #[tokio::test]
    async fn rewind_rejects_negative_numbers() {
        let command = RewindCommand;
        let outcome = command.invoke(ctx("-1")).await;
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    #[test]
    fn manifest_id_matches_the_published_constant() {
        assert_eq!(HistoryPlugin.manifest().id, PLUGIN_ID);
    }

    #[test]
    fn plugin_declares_the_rewind_command_under_its_bare_name_and_no_tools() {
        let plugin = HistoryPlugin;
        let commands = plugin.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].spec().name, COMMAND_NAME);
        assert!(
            plugin.manifest().tools.is_empty(),
            "this plugin ships one TUI command and no tools"
        );
    }
}
