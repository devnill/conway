//! Two, deliberately distinct, things live in this one file, both about
//! `conway`'s command line surface for plugins:
//!
//! 1. **Plugin-CONTRIBUTED subcommands** ([`run`], unchanged by board item
//!    `01M1FSDRF20E2EGHCG3RK28DKH`): anything typed on the command line that
//!    is not a built-in subcommand (`sessions`, `routes`, `tools`, `plugin`
//!    itself) falls through clap's own `external_subcommand` catch-all
//!    (`cli::Command::External`, `cli.rs`'s own doc on that variant) and is
//!    resolved here, against every installed plugin's own [`conway::plugin::
//!    Plugin::commands`] -- `<plugin-id>.<command-name>`, the identical
//!    namespacing scheme the TUI's `/`-prefixed dispatch already uses and
//!    proves live (`conway-plugin-history`'s `/conway.history.rewind`,
//!    `tui::commands::CommandRegistry`).
//! 2. **`conway plugin list|install|remove`** ([`run_admin`], THIS item): a
//!    BUILT-IN clap subcommand (`cli::Command::Plugin`, matched before
//!    `External` ever sees the word `plugin`) that lists, turns on, and
//!    turns off compiled-in first-party plugins -- the headless equivalent
//!    of the interactive `/plugin` command
//!    (`tui::view::plugins`/`tui::app::plugin_toggle`). See [`PluginArgs`]'s
//!    own doc for the three actions.
//!
//! Kept in one file ("beside the existing external-command resolver", this
//! item's own binding note) because both are `conway plugin`-shaped surface
//! for the SAME underlying thing (a plugin), even though one resolves
//! plugin-declared subcommands and the other administers the compiled-in
//! bundle -- a reader looking for "what does `conway plugin ...` do" has
//! one file to open, not two.
//!
//! ## `run`: plugin-contributed subcommands
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
//!
//! ## `run_admin`: `conway plugin list|install|remove`
//!
//! [`PluginAction`]'s own variant docs are deliberately terse (clap renders
//! them as `--help` text) -- the full contract lives here instead:
//!
//! - **Same rows, same writer, same file as `/plugin`.** `list` calls
//!   `crate::plugin_rows::rows_from_plugin_browser` (a private module --
//!   plain code span, not a doc link, on purpose) -- the identical row
//!   builder the TUI's `/plugin` command renders, never a second,
//!   independently-worded listing. `install`/`remove` call
//!   [`conway::config::set_plugin_installed`], targeting
//!   `~/.conway/settings.json` (or `$CONWAY_CONFIG_DIR/settings.json`) via
//!   [`conway::config::discovery::user_config_path`] -- the SAME function,
//!   and the SAME file, `/plugin`'s own compiled-in toggle writes
//!   (`tui::app::plugin_toggle`).
//! - **No project-scope target.** There is no flag to write
//!   `.conway/settings.json` instead -- this command writes exactly the
//!   file `/plugin` writes, nothing else (recorded in
//!   `docs/scripting.md`'s own `conway plugin` section as the current
//!   scope).
//! - **Restart to apply.** A write here never touches this process's own
//!   running config -- the next `conway` invocation picks it up, exactly
//!   like every other `/plugin` toggle.
//! - **`--defaults` installs [`crate::first_party_plugins::
//!   DEFAULT_OPINION_SET`]** -- the same six ids guided first-run setup
//!   installs unprompted the moment it verifies a working provider
//!   (`first_run::apply_opinion_set`).
//! - **An id this binary does not link is a usage error (exit 2)**, naming
//!   every id it does -- the same known-id listing
//!   `ConwayBuilder::install_selected`'s own `plugins.install names unknown
//!   id ...` config error already produces, narrowed to the compiled-in
//!   half of it (this command never touches router/backend factory ids).

use std::collections::HashMap;
use std::sync::Arc;

use clap::{Args, Subcommand};
use conway::plugin::{CommandCtx, CommandOutcome, MemoryStore};
use conway::{Conway, ForkSpec, SessionSpec};

use crate::diag;
use crate::exit::ExitCode;
use crate::first_party_plugins;
use crate::tui::commands::CommandRegistry;
use crate::tui::state::PluginBrowserEntry;

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

// ---------------------------------------------------------------------
// `conway plugin list|install|remove` -- board item
// `01M1FSDRF20E2EGHCG3RK28DKH`. See this module's own top doc, section 2,
// for how this differs from `run` above.
// ---------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct PluginArgs {
    #[command(subcommand)]
    pub action: PluginAction,
}

// `conway plugin` with no subcommand prints clap's own generated help and
// exits 2 (clap's ordinary behavior for a required `#[command(subcommand)]`
// left unset -- nothing here has to construct that path by hand).
//
// Doc comments below are kept short deliberately: clap renders them
// verbatim as `--help` text, and every longer rationale (which file gets
// written, why there's no project-scope target, why an unknown id is exit
// 2) belongs in this module's own top doc or in a `//` comment instead, not
// stretched across the terminal every time an operator asks for help.
#[derive(Subcommand, Debug)]
pub enum PluginAction {
    /// List every compiled-in plugin this binary links: `[x]`/`[ ]`, id,
    /// and a one-line summary -- the same table `/plugin` shows in the TUI.
    List {
        /// Show only this one plugin's full "you get / you lose / costs"
        /// breakdown instead of the whole table.
        id: Option<String>,
        /// Show the full breakdown for every plugin, not just its summary.
        #[arg(long)]
        verbose: bool,
    },
    /// Turn one or more plugin ids on (writes ~/.conway/settings.json).
    Install {
        /// Plugin ids to install, e.g. conway.memory.
        #[arg(conflicts_with = "defaults")]
        ids: Vec<String>,
        /// Install conway's own default opinion set instead of naming ids.
        #[arg(long)]
        defaults: bool,
    },
    /// Turn one or more plugin ids off (writes ~/.conway/settings.json).
    Remove {
        /// Plugin ids to remove.
        ids: Vec<String>,
    },
}

/// Dispatches [`PluginArgs::action`]. `conway`'s the SAME `Conway` every
/// other built-in subcommand receives (`main.rs`'s single `build_conway`
/// choke point) -- `list`/`install` read `conway.config().cwd`/`.plugins.
/// install` off it purely to compute rows/validate ids, never anything that
/// touches the running session. `memory_store` is the SAME `Arc` `main.rs`
/// resolved once for this process (board item `01M09V3S2AQYB2VK6MANFRH1JM`)
/// -- forwarded to `first_party_plugins::all_bundle_plugins`, never
/// re-opened, mirroring [`run`]'s own identical need one section up. `env`
/// is `main.rs`'s own resolved-once process environment, forwarded to
/// `conway::config::discovery::user_config_path` so `install`/`remove`
/// honour `CONWAY_CONFIG_DIR` exactly like `/plugin`'s own toggle does.
pub async fn run_admin(
    args: &PluginArgs,
    conway: &Conway,
    memory_store: Arc<dyn MemoryStore>,
    env: &HashMap<String, String>,
) -> conway::Result<ExitCode> {
    match &args.action {
        PluginAction::List { id, verbose } => {
            list(conway, memory_store, env, id.as_deref(), *verbose)
        }
        PluginAction::Install { ids, defaults } => {
            install(conway, memory_store, env, ids, *defaults)
        }
        PluginAction::Remove { ids } => remove(conway, memory_store, env, ids),
    }
}

/// Exactly `tui::app::startup`'s own `state.plugin_browser` construction
/// (that call site's own comment cites this as the read to mirror) --
/// every compiled-in candidate `all_bundle_plugins` links, each tagged
/// `installed` by membership in `conway.config().plugins.install`. Kept as
/// its own function since both [`list`] and [`install`]'s unknown-id check
/// need the identical candidate set.
fn browser_entries(
    conway: &Conway,
    memory_store: Arc<dyn MemoryStore>,
    env: &HashMap<String, String>,
) -> Vec<PluginBrowserEntry> {
    let cwd = conway.config().cwd.clone();
    let install_ids = &conway.config().plugins.install;
    first_party_plugins::all_bundle_plugins(&cwd, memory_store, env)
        .iter()
        .map(|p| {
            let manifest = p.manifest();
            PluginBrowserEntry {
                installed: install_ids.contains(&manifest.id),
                id: manifest.id,
                version: manifest.version,
                description: p.description(),
            }
        })
        .collect()
}

/// The same "unknown id" phrasing `ConwayBuilder::install_selected`'s own
/// `plugins.install names unknown id ...` config error uses (this item's
/// own binding note: "the same known-id listing the config error already
/// produces") -- narrowed to the compiled-in-plugin half of that error
/// (this command never touches router/backend factory ids at all).
fn unknown_id_message(id: &str, entries: &[PluginBrowserEntry]) -> String {
    let known: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    format!(
        "plugin id '{id}' is not among this binary's linked plugins: [{}] -- see `conway plugin \
         list`",
        known.join(", ")
    )
}

fn print_row(row: &crate::plugin_rows::PluginRow, verbose: bool) {
    let box_glyph = if row.active { "x" } else { " " };
    println!("[{box_glyph}] {} -- {}", row.id, row.contributes);
    if verbose {
        if let Some(description) = &row.description {
            println!(
                "    you get   {}",
                crate::plugin_rows::non_empty_or(&description.you_get, "(none given)")
            );
            println!(
                "    you lose  {}",
                crate::plugin_rows::non_empty_or(&description.you_lose, "(none given)")
            );
            println!(
                "    costs     {}",
                crate::plugin_rows::non_empty_or(&description.costs, "none")
            );
        }
    }
}

fn list(
    conway: &Conway,
    memory_store: Arc<dyn MemoryStore>,
    env: &HashMap<String, String>,
    only_id: Option<&str>,
    verbose: bool,
) -> conway::Result<ExitCode> {
    let entries = browser_entries(conway, memory_store, env);

    if let Some(id) = only_id {
        let Some(entry) = entries.iter().find(|e| e.id == id) else {
            diag::error(unknown_id_message(id, &entries));
            return Ok(ExitCode::Usage);
        };
        // `rows_from_plugin_browser` -- the SAME builder `/plugin` calls,
        // reused rather than re-derived (this module's own top doc, and
        // this item's own binding note) -- over a single-element slice, so
        // the printed row is byte-for-byte what the TUI's detail panel
        // would show for this same id.
        let rows = crate::plugin_rows::rows_from_plugin_browser(std::slice::from_ref(entry));
        print_row(&rows[0], true);
        return Ok(ExitCode::Completed);
    }

    for row in crate::plugin_rows::rows_from_plugin_browser(&entries) {
        print_row(&row, verbose);
    }
    Ok(ExitCode::Completed)
}

fn install(
    conway: &Conway,
    memory_store: Arc<dyn MemoryStore>,
    env: &HashMap<String, String>,
    ids: &[String],
    defaults: bool,
) -> conway::Result<ExitCode> {
    let targets: Vec<String> = if defaults {
        first_party_plugins::DEFAULT_OPINION_SET
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        ids.to_vec()
    };
    if targets.is_empty() {
        diag::error("conway plugin install: name at least one <id>, or pass --defaults");
        return Ok(ExitCode::Usage);
    }

    let entries = browser_entries(conway, memory_store, env);
    for id in &targets {
        if !entries.iter().any(|e| &e.id == id) {
            diag::error(unknown_id_message(id, &entries));
            return Ok(ExitCode::Usage);
        }
    }

    let Some(path) = conway::config::discovery::user_config_path(env) else {
        diag::error("could not resolve a home directory to write settings.json into");
        return Ok(ExitCode::Usage);
    };
    for id in &targets {
        match conway::config::set_plugin_installed(&path, id, true) {
            Ok(true) => println!(
                "{id}: installed -- written to {} (restart to apply; a currently running conway \
                 process is unaffected until then)",
                path.display()
            ),
            Ok(false) => println!("{id}: already installed"),
            Err(e) => {
                diag::error(format!("{id}: {e}"));
                return Ok(ExitCode::Usage);
            }
        }
    }
    Ok(ExitCode::Completed)
}

fn remove(
    conway: &Conway,
    memory_store: Arc<dyn MemoryStore>,
    env: &HashMap<String, String>,
    ids: &[String],
) -> conway::Result<ExitCode> {
    if ids.is_empty() {
        diag::error("conway plugin remove: name at least one <id>");
        return Ok(ExitCode::Usage);
    }

    // Same validation `install`/`list` already do (this module's own doc
    // says "an id this binary does not link is a usage error" for the
    // whole surface): without it, an unknown or mistyped id here is a
    // silent, exit-0 no-op ("was not installed") indistinguishable from
    // successfully removing an id that was, in fact, never installed --
    // an operator who typos a real id could believe it's gone.
    let entries = browser_entries(conway, memory_store, env);
    for id in ids {
        if !entries.iter().any(|e| &e.id == id) {
            diag::error(unknown_id_message(id, &entries));
            return Ok(ExitCode::Usage);
        }
    }

    let Some(path) = conway::config::discovery::user_config_path(env) else {
        diag::error("could not resolve a home directory to write settings.json into");
        return Ok(ExitCode::Usage);
    };
    for id in ids {
        match conway::config::set_plugin_installed(&path, id, false) {
            Ok(true) => println!(
                "{id}: removed -- written to {} (restart to apply; a currently running conway \
                 process is unaffected until then)",
                path.display()
            ),
            Ok(false) => println!("{id}: was not installed"),
            Err(e) => {
                diag::error(format!("{id}: {e}"));
                return Ok(ExitCode::Usage);
            }
        }
    }
    Ok(ExitCode::Completed)
}
