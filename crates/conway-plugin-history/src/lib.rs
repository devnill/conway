//! `conway-plugin-history`: `/conway.history.rewind`,
//! `/conway.history.mask`, and `/conway.history.checkout` -- a first-party
//! plugin proving `/rewind`/`/checkout`/`ContextMask` genuinely ARE
//! plugins, per the owner's ruling: "features like /rewind, /checkout, etc
//! are to be plugins, to fit into the philosophy; they are not core
//! functionality." Not installed by default -- see
//! [`docs/getting-started.md`](../../../docs/getting-started.md#installing-a-first-party-plugin)
//! for the `[plugins].install` opt-in, mirroring
//! `crates/conway-plugin-skeleton`'s own pattern exactly.
//!
//! **`/conway.history.mask`/`/conway.history.checkout` (board item
//! 01KZY8QRAVVVKCRBZ6HAEGW3GG, "`/checkout` and a reachable `ContextMask`")
//! are this plugin's second and third commands**, added beside `/rewind`
//! rather than as a parallel mechanism -- both reuse `/rewind`'s own
//! established shape (an explicit, operator-typed identifier; parse or
//! return a named [`CommandOutcome::Error`]; never a panic) and both close
//! gaps `/rewind`'s own item deliberately left open:
//!
//! - **`/conway.history.mask <seq> [unmask]`** returns
//!   [`CommandOutcome::MaskRecord`], the producer `LogRecord::ContextMask`
//!   never had before this item -- see that variant's own doc
//!   (`conway_core::log`) for the full contract and the scope decision
//!   ("still fork-prefix-only") this item settled.
//! - **`/conway.history.checkout <session-id>`** returns
//!   [`CommandOutcome::Checkout`]: forks `<session-id>` at ITS OWN head and
//!   drives the child -- the one case `ForkSession` structurally cannot
//!   express (it can only ever fork the CALLING session). `/checkout`
//!   always forks rather than attaching to the live session directly
//!   (`PHILOSOPHY.md` §1: a finished session is forkable at any point,
//!   which keeps `/checkout` append-only the same way `/rewind` already
//!   is).
//!
//! **What this crate proves.** `CommandOutcome::ForkSession` closed the ONE gap that used to block
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
    async_trait, Command, CommandCtx, CommandOutcome, CommandSpec, Plugin, PluginDescription,
    PluginManifest,
};
use conway::{LogSeq, SessionId};

/// This plugin's manifest id: the string an operator names in
/// `[plugins].install` (`settings.json`) or a caller matches by hand before
/// calling `ConwayBuilder::with_plugin`.
pub const PLUGIN_ID: &str = "conway.history";

/// The bare name `RewindCommand` registers under -- reachable in the TUI
/// as `/{PLUGIN_ID}.{COMMAND_NAME_REWIND}`, i.e. `/conway.history.rewind`
/// (`conway_cli::tui::commands::CommandRegistry::build` prefixes it with
/// this plugin's own manifest id).
pub const COMMAND_NAME_REWIND: &str = "rewind";

/// The bare name `MaskCommand` registers under -- reachable as
/// `/conway.history.mask`. This plugin's second command (module doc).
pub const COMMAND_NAME_MASK: &str = "mask";

/// The bare name `CheckoutCommand` registers under -- reachable as
/// `/conway.history.checkout`. This plugin's third command (module doc).
pub const COMMAND_NAME_CHECKOUT: &str = "checkout";

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
            name: COMMAND_NAME_REWIND.to_string(),
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
                "usage: /{PLUGIN_ID}.{COMMAND_NAME_REWIND} <seq> -- expected a non-negative \
                 integer sequence number, got {trimmed:?}"
            )),
        }
    }
}

/// `/conway.history.mask <seq> [unmask]`: parses `ctx.args` as a bare,
/// non-negative integer [`LogSeq`] plus an optional trailing `unmask`
/// token, and asks the host to append a `LogRecord::ContextMask` for it
/// against the CALLING session -- this plugin's second command (module
/// doc), and the first real producer `LogRecord::ContextMask` has ever had
/// (`conway_core::log::LogRecord::ContextMask`'s own doc). With no second
/// token, the record excludes `<seq>`; with `unmask`, it reverses a
/// previous exclusion (`CommandOutcome::MaskRecord::excluded`'s own doc).
/// Anything else -- non-numeric, empty, extra tokens, or a second token
/// that is not exactly `unmask` -- is a named [`CommandOutcome::Error`],
/// mirroring [`RewindCommand`]'s own discipline.
struct MaskCommand;

#[async_trait]
impl Command for MaskCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_MASK.to_string(),
            summary: "masks (or, with a trailing `unmask`, un-masks) a sequence number out of \
                      what a FUTURE fork of this session inherits, e.g. \
                      `/conway.history.mask 42` / `/conway.history.mask 42 unmask` -- never \
                      affects this session's own later turns"
                .to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let trimmed = ctx.args.trim();
        let mut tokens = trimmed.split_whitespace();
        let usage = || {
            CommandOutcome::Error(format!(
                "usage: /{PLUGIN_ID}.{COMMAND_NAME_MASK} <seq> [unmask] -- expected a \
                 non-negative integer sequence number and an optional trailing `unmask`, got \
                 {trimmed:?}"
            ))
        };
        let Some(seq_token) = tokens.next() else {
            return usage();
        };
        let Ok(seq) = seq_token.parse::<u64>() else {
            return usage();
        };
        let excluded = match tokens.next() {
            None => true,
            Some("unmask") => false,
            Some(_) => return usage(),
        };
        if tokens.next().is_some() {
            return usage();
        }
        CommandOutcome::MaskRecord {
            target_seq: LogSeq(seq),
            excluded,
        }
    }
}

/// `/conway.history.checkout <session-id>`: parses `ctx.args` as a bare
/// [`SessionId`] (the same ULID form `--fork-from`/`--resume` accept) and
/// asks the host to check it out -- this plugin's third command (module
/// doc). Any argument that does not parse as a `SessionId`, or is empty, is
/// a named [`CommandOutcome::Error`], mirroring [`RewindCommand`]'s own
/// discipline. This command does not, and cannot, validate that the
/// session actually exists -- it performs no I/O (see [`Command`]'s own
/// doc); an unknown id surfaces as a host-side error when the fork is
/// attempted, exactly like an out-of-range `/rewind` seq does.
struct CheckoutCommand;

#[async_trait]
impl Command for CheckoutCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_CHECKOUT.to_string(),
            summary: "checks out another session: forks it at its own head and drives the \
                      child, e.g. `/conway.history.checkout <session-id>` -- the session \
                      checked out from is left untouched and still listed"
                .to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let trimmed = ctx.args.trim();
        match trimmed.parse::<SessionId>() {
            Ok(target) => CommandOutcome::Checkout { target },
            Err(_) => CommandOutcome::Error(format!(
                "usage: /{PLUGIN_ID}.{COMMAND_NAME_CHECKOUT} <session-id> -- expected a valid \
                 session id (ULID), got {trimmed:?}"
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
            // No tools: this plugin's entire surface is the three TUI
            // slash commands below. `PluginManifest::tools` names only
            // what `Plugin::tools()` actually returns -- an empty `Vec`
            // here, never a stub.
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "rewind, mask, and check out session history".to_string(),
            you_get: format!(
                "3 commands: /{PLUGIN_ID}.rewind (fork this session at a past turn), \
                 /{PLUGIN_ID}.mask (hide a turn from future context without deleting it), \
                 /{PLUGIN_ID}.checkout (fork a DIFFERENT session by id)"
            ),
            you_lose: "nothing else -- history stays append-only and readable either way"
                .to_string(),
            costs: "none beyond the commands themselves".to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
        Vec::new()
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        vec![
            Arc::new(RewindCommand),
            Arc::new(MaskCommand),
            Arc::new(CheckoutCommand),
        ]
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

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default.
    #[test]
    fn description_is_non_empty() {
        let description = HistoryPlugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
    }

    #[test]
    fn plugin_declares_all_three_commands_under_their_bare_names_and_no_tools() {
        let plugin = HistoryPlugin;
        let commands = plugin.commands();
        assert_eq!(commands.len(), 3);
        let names: Vec<String> = commands.iter().map(|c| c.spec().name).collect();
        assert_eq!(
            names,
            vec![
                COMMAND_NAME_REWIND.to_string(),
                COMMAND_NAME_MASK.to_string(),
                COMMAND_NAME_CHECKOUT.to_string(),
            ]
        );
        assert!(
            plugin.manifest().tools.is_empty(),
            "this plugin ships three TUI commands and no tools"
        );
    }

    // -----------------------------------------------------------------
    // `/conway.history.mask` -- `CommandOutcome::MaskRecord`
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn mask_with_no_second_token_excludes() {
        let command = MaskCommand;
        let outcome = command.invoke(ctx("7")).await;
        assert_eq!(
            outcome,
            CommandOutcome::MaskRecord {
                target_seq: LogSeq(7),
                excluded: true,
            }
        );
    }

    #[tokio::test]
    async fn mask_with_trailing_unmask_un_excludes() {
        let command = MaskCommand;
        let outcome = command.invoke(ctx("7 unmask")).await;
        assert_eq!(
            outcome,
            CommandOutcome::MaskRecord {
                target_seq: LogSeq(7),
                excluded: false,
            }
        );
    }

    #[tokio::test]
    async fn mask_trims_surrounding_whitespace() {
        let command = MaskCommand;
        let outcome = command.invoke(ctx("  12  unmask  ")).await;
        assert_eq!(
            outcome,
            CommandOutcome::MaskRecord {
                target_seq: LogSeq(12),
                excluded: false,
            }
        );
    }

    #[tokio::test]
    async fn mask_zero_is_a_valid_seq() {
        let command = MaskCommand;
        let outcome = command.invoke(ctx("0")).await;
        assert_eq!(
            outcome,
            CommandOutcome::MaskRecord {
                target_seq: LogSeq(0),
                excluded: true,
            }
        );
    }

    #[tokio::test]
    async fn mask_rejects_empty_arguments() {
        let command = MaskCommand;
        let outcome = command.invoke(ctx("")).await;
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    #[tokio::test]
    async fn mask_rejects_non_numeric_seq() {
        let command = MaskCommand;
        let outcome = command.invoke(ctx("nope")).await;
        match outcome {
            CommandOutcome::Error(message) => {
                assert!(message.contains("nope"), "{message}");
                assert!(message.contains("usage"), "{message}");
            }
            other => panic!("expected CommandOutcome::Error, got {other:?}"),
        }
    }

    /// **The discriminating half of the second token's own contract**:
    /// anything other than exactly `unmask` -- a typo, a third token -- is
    /// a named error, never silently treated as either mask or unmask.
    #[tokio::test]
    async fn mask_rejects_an_unrecognized_second_token() {
        let command = MaskCommand;
        let outcome = command.invoke(ctx("7 delete")).await;
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    #[tokio::test]
    async fn mask_rejects_a_third_token() {
        let command = MaskCommand;
        let outcome = command.invoke(ctx("7 unmask extra")).await;
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    // -----------------------------------------------------------------
    // `/conway.history.checkout` -- `CommandOutcome::Checkout`
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn checkout_parses_a_valid_session_id() {
        let command = CheckoutCommand;
        let target = conway::SessionId::new();
        let outcome = command.invoke(ctx(&target.to_string())).await;
        assert_eq!(outcome, CommandOutcome::Checkout { target });
    }

    #[tokio::test]
    async fn checkout_trims_surrounding_whitespace() {
        let command = CheckoutCommand;
        let target = conway::SessionId::new();
        let outcome = command.invoke(ctx(&format!("  {target}  "))).await;
        assert_eq!(outcome, CommandOutcome::Checkout { target });
    }

    #[tokio::test]
    async fn checkout_rejects_empty_arguments() {
        let command = CheckoutCommand;
        let outcome = command.invoke(ctx("")).await;
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    /// **The discriminating half of this command's own contract**: garbage
    /// that is not a valid ULID is a named error, never a panic and never
    /// silently coerced into some default session.
    #[tokio::test]
    async fn checkout_rejects_a_malformed_session_id_with_a_named_error() {
        let command = CheckoutCommand;
        let outcome = command.invoke(ctx("not-a-session-id")).await;
        match outcome {
            CommandOutcome::Error(message) => {
                assert!(message.contains("not-a-session-id"), "{message}");
                assert!(message.contains("usage"), "{message}");
            }
            other => panic!("expected CommandOutcome::Error, got {other:?}"),
        }
    }
}
