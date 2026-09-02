//! Plugin-contributed subcommands: anything typed on the command line that
//! is not a built-in subcommand (`sessions`, `routes`, `tools`) falls through
//! clap's own `external_subcommand` catch-all (`cli::Command::External`,
//! `cli.rs`'s own doc on that variant) and is resolved here, against every
//! installed plugin's own [`conway::plugin::Plugin::commands`] --
//! `<plugin-id>.<command-name>`, the identical namespacing scheme the TUI's
//! `/`-prefixed dispatch already uses and proves live (`conway-plugin-
//! history`'s `/conway.history.rewind`, `tui::commands::CommandRegistry`).
//!
//! **Reused, not reinvented.** [`crate::tui::commands::CommandRegistry::
//! build`] already implements the exact resolution this needs (namespacing,
//! `conway::plugin::validate_command_name`, and duplicate-full-name
//! rejection) -- this module calls it directly rather than restating any of
//! that logic, per this item's own binding note to prefer reading and
//! reusing the TUI's existing command plumbing over adding a second copy of
//! it.
//!
//! **What is different from the TUI's own dispatch, and why.** The TUI
//! always has a live, already-focused session to build a [`conway::plugin::
//! CommandCtx`] from; a bare `conway <plugin-id>.<command>` invocation has
//! none yet, so [`run`] starts one fresh, prompt-less session purely to
//! have real `focused_agent`/`root_agent`/`session_id` values to hand the
//! command -- the same shape `crate::oneshot::resolve_session`'s
//! flag-free arm already creates for one-shot mode. A
//! [`conway::plugin::CommandOutcome::ForkSession`] outcome is honored for
//! real (the fork actually happens, against the real store) but, unlike the
//! TUI, there is no follow-on interactive loop to hand the child to -- this
//! prints the child's session id instead, which `conway sessions show
//! <id>`/`conway -p --resume <id> ...` can pick up from there.
//! [`conway::plugin::CommandOutcome::SubmitPrompt`] (board item
//! `01M0VSMF71S6VXX81YRAAF5S8Q`) is honored the same way: the prompt is
//! genuinely submitted (a real `UserTurn` record, real `Provenance::
//! CommandPrompt` attribution), but this function does not stay attached
//! to drive the resulting turn -- see that arm's own comment for why.

use std::sync::Arc;

use conway::plugin::{CommandCtx, CommandOutcome, MemoryStore};
use conway::{Conway, ForkSpec, SessionSpec};

use crate::diag;
use crate::exit::ExitCode;
use crate::first_party_plugins;
use crate::tui::commands::CommandRegistry;

/// `args` is exactly clap's `external_subcommand` payload: `args[0]` is the
/// unrecognized subcommand word itself (e.g. `"acme.greet"`), `args[1..]`
/// is everything typed after it, verbatim, joined with single spaces into
/// [`CommandCtx::args`] -- mirroring `conway_cli::tui::commands::parse`'s
/// own "consume the remainder verbatim, no re-tokenization" rule for a
/// plugin command's free-text argument (that module's own doc,
/// `CommandCtx::args`'s doc).
///
/// `memory_store` is `main.rs`'s `build_conway`/`dispatch` forwarding the
/// SAME `Arc<dyn MemoryStore>` `first_party_plugins::install` resolved for
/// this process (board item `01M09V3S2AQYB2VK6MANFRH1JM`) -- handed straight
/// through to [`first_party_plugins::installed_plugins`] below, never
/// re-resolved here, so this call site cannot open a second `FsMemoryStore`
/// over the same root.
///
/// `agent_names` is the identical arrangement for `conway.names`'s own
/// store (board item `01M0TV5BSE98S16SFYECG9G9WP`): forwarded, never
/// re-resolved, so `conway conway.names.rename ...` run as a one-shot
/// subcommand writes into the SAME file the TUI reads names out of.
///
/// `env` is `main.rs`'s own process-environment map, resolved once at that
/// binary's single entry point and forwarded through `dispatch` -- passed to
/// [`first_party_plugins::installed_plugins`] so the `conway.idiom` plugin's
/// operator-global instructions file honours `CONWAY_CONFIG_DIR` here too
/// (board item `01M0W5Q569F0T97HSEP6F0MPCR`), never re-read ambiently in
/// this function.
pub async fn run(
    args: &[String],
    conway: &Conway,
    memory_store: Arc<dyn MemoryStore>,
    agent_names: Arc<dyn conway_plugin_names::AgentNames>,
    env: &std::collections::HashMap<String, String>,
) -> conway::Result<ExitCode> {
    let Some(full_name) = args.first() else {
        // Unreachable through clap's own `external_subcommand`, which never
        // fires with zero captured tokens -- kept as an explicit usage
        // error rather than a `panic!`/`unreachable!` so a future clap
        // change that *did* make this reachable fails softly, at exit 2,
        // not with a crash.
        diag::error("no subcommand given");
        return Ok(ExitCode::Usage);
    };
    let rest = args[1..].join(" ");

    let plugins = first_party_plugins::installed_plugins(conway, memory_store, agent_names, env)?;
    let registry = match CommandRegistry::build(&plugins) {
        Ok(registry) => registry,
        Err(e) => {
            // A registration collision (two installed plugins landing on
            // the same full name) is a configuration problem, not this
            // particular invocation's fault -- still a usage error (exit
            // 2), matching every other "before a single agent turn ever
            // starts" failure mode `oneshot::resolve_session` establishes.
            diag::error(format!("plugin command registration: {e}"));
            return Ok(ExitCode::Usage);
        }
    };

    let Some(command) = registry.resolve(full_name) else {
        diag::error(format!(
            "unknown subcommand `{full_name}`: not a built-in subcommand (`sessions`, \
             `routes`, `tools`), and no installed plugin declares it"
        ));
        return Ok(ExitCode::Usage);
    };

    // No live session yet -- start a fresh, prompt-less one purely for real
    // agent/session ids (module doc). Never prompted, so this never
    // consults the permission gate or reaches a model.
    let handle = conway.new_session(SessionSpec::default()).await?;
    let ctx = CommandCtx {
        focused_agent: handle.root(),
        root_agent: handle.root(),
        session_id: handle.id(),
        args: rest,
    };

    match command.invoke(ctx).await {
        CommandOutcome::Output(lines) => {
            for line in lines {
                println!("{line}");
            }
            Ok(ExitCode::Completed)
        }
        CommandOutcome::Error(message) => {
            diag::error(format!("{full_name}: {message}"));
            Ok(ExitCode::AgentFailed)
        }
        CommandOutcome::ForkSession { at_seq, directive } => {
            match conway
                .fork_from(handle.id(), at_seq, ForkSpec::new(directive))
                .await
            {
                Ok(child) => {
                    println!(
                        "{full_name}: forked session {} at seq {} -- `conway sessions show {}` \
                         to inspect it, or `conway -p --resume {}` to continue it",
                        child.id(),
                        at_seq.0,
                        child.id(),
                        child.id(),
                    );
                    Ok(ExitCode::Completed)
                }
                Err(e) => {
                    diag::error(format!("{full_name}: fork failed: {e}"));
                    Ok(ExitCode::AgentFailed)
                }
            }
        }
        // `/conway.history.mask`'s own capability (board item
        // 01KZY8QRAVVVKCRBZ6HAEGW3GG) -- appends against the fresh,
        // prompt-less session this function created above (module doc: no
        // live session exists yet in this bare-invocation path), the same
        // `handle.id()` `ForkSession` above resolves against.
        CommandOutcome::MaskRecord {
            target_seq,
            excluded,
        } => match conway.mask_record(handle.id(), target_seq, excluded).await {
            Ok(_seq) => {
                let verb = if excluded { "masked" } else { "un-masked" };
                println!(
                    "{full_name}: {verb} seq {} on session {}",
                    target_seq.0,
                    handle.id()
                );
                Ok(ExitCode::Completed)
            }
            Err(e) => {
                diag::error(format!("{full_name}: mask failed: {e}"));
                Ok(ExitCode::AgentFailed)
            }
        },
        // `/conway.history.checkout`'s own capability -- forks `target` at
        // its own head; unlike the TUI there is no follow-on interactive
        // loop to hand the child to, so (mirroring `ForkSession` above) this
        // prints the child's session id instead.
        CommandOutcome::Checkout { target } => {
            let head = match conway.session_head(target).await {
                Ok(head) => head,
                Err(e) => {
                    diag::error(format!("{full_name}: checkout failed: {e}"));
                    return Ok(ExitCode::AgentFailed);
                }
            };
            match conway
                .fork_from(target, head, ForkSpec::new(String::new()))
                .await
            {
                Ok(child) => {
                    println!(
                        "{full_name}: checked out session {target} at seq {} into new session \
                         {} -- `conway sessions show {}` to inspect it, or `conway -p --resume \
                         {}` to continue it ({target} is untouched)",
                        head.0,
                        child.id(),
                        child.id(),
                        child.id(),
                    );
                    Ok(ExitCode::Completed)
                }
                Err(e) => {
                    diag::error(format!("{full_name}: checkout failed: {e}"));
                    Ok(ExitCode::AgentFailed)
                }
            }
        }
        // `CommandOutcome::SubmitPrompt`'s own capability (board item
        // `01M0VSMF71S6VXX81YRAAF5S8Q`) -- the FIRST arm in this function
        // that actually reaches a model, unlike every arm above it (module
        // doc: "never prompted, so this never consults the permission gate
        // or reaches a model"). Mirrors `ForkSession`/`Checkout` above in
        // NOT staying attached to drive the resulting turn interactively --
        // that would need this function to grow the same event-loop/
        // renderer machinery `oneshot::run` already owns, which is real,
        // separate scope this smaller v1 does not take on. Submits for
        // real (a genuine `LogRecord::UserTurn` stamped `Provenance::
        // CommandPrompt`, appended against the fresh session `resolve_
        // session`'s sibling above created) and prints a pointer, the same
        // shape `ForkSession`'s own "`conway -p --resume <id> ...` to
        // continue it" message already uses.
        CommandOutcome::SubmitPrompt { text } => {
            match handle
                .prompt_command(handle.root(), text, full_name.clone())
                .await
            {
                Ok(_turn) => {
                    println!(
                        "{full_name}: submitted a prompt to session {} -- `conway -p --resume {} \
                         ...` to see the response",
                        handle.id(),
                        handle.id(),
                    );
                    Ok(ExitCode::Completed)
                }
                Err(e) => {
                    diag::error(format!("{full_name}: could not submit prompt: {e}"));
                    Ok(ExitCode::AgentFailed)
                }
            }
        }
    }
}
