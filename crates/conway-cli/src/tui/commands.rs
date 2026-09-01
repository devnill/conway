//! TUI slash-command parsing and dispatch.
//!
//! Two responsibilities, kept apart on purpose so [`parse`] stays a pure,
//! state-free function while resolution and dispatch get to see the live
//! session: [`parse`] turns one line of raw input into a [`SlashCommand`]
//! with every argument still a raw `String` (no agent-id resolution, no
//! facade call); [`execute`] resolves agent-id arguments against the
//! caller's [`AppState`] tree and performs the one facade call each command
//! maps to (module notes' table), through the [`Host`] seam so dispatch
//! logic is testable without a live `Runtime`.
//!
//! None of this reaches past `SessionHandle`/`Conway` -- [`Host`] is a
//! thin abstraction over exactly those two types' methods, and [`LiveHost`]
//! is a pure delegation to them.
//!
//! ## `/thinking` and `/timestamps` are REMOVED (V4)
//!
//! Both used to be intercepted directly in `app.rs::submit`, ahead of
//! [`parse`] (mirroring `/agents`'s own pattern) -- neither one ever reached
//! this module's parser at all. They are now gone entirely, not aliased:
//! [`SlashCommand::Settings`] (`/settings`) opens a menu
//! (`view/settings.rs`) covering the same two toggles plus a numeric
//! setting, on the reasoning that a dedicated slash command per single
//! toggle does not scale as more display preferences are added. `parse`
//! returns the ordinary "unknown command" [`ParseError`] for `/thinking`/
//! `/timestamps` now, the same as any other retired command name.
//!
//! ## `/settings`, `/trust`, `/agents`, `/ask` are ordinary commands now
//! (board item `01KZVZ5XV162XCQR96AQKCCCF7`)
//!
//! All four used to be intercepted in `app.rs::submit` by direct string
//! comparison, before [`parse`] ever ran -- see T9
//! (`crates/conway/tests/architecture_invariants.rs`). Each is now a
//! first-class [`SlashCommand`] variant, parsed here like any other command,
//! so `submit` calls [`parse`] exactly once and every command -- modal or
//! not -- is dispatched from its result. Concretely, this closes the "the
//! parser cannot express a command that takes over the surface or refreshes
//! state before rendering" gap the item's own spec asked to be checked
//! first:
//!
//! - [`SlashCommand::Agents`] toggles the `/agents` tree view -- a pure
//!   `AppState` flip, [`execute`]'s simplest arm.
//! - [`SlashCommand::Trust`] installs the operator's project permissions
//!   file (D4 §5/§9: an explicit action, never automatic) through the new
//!   [`Host::trust_permission_file`] -- routed through the SAME `Host` seam
//!   as every other facade call, so it is unit-testable against
//!   `tests::FakeHost` like `/steer`/`/fork`/etc.
//! - [`SlashCommand::Ask`] opens the B5 modal-ask flow. `execute` cannot
//!   itself spawn the async task that forks the ephemeral child and drains
//!   its turn (that needs the live `SessionHandle` plus `App`'s own
//!   `modal_ask_tx`, neither of which `Host` exposes) -- it validates
//!   (empty question, already-in-flight) and, on success, sets
//!   `state.ask_in_flight` and returns [`Effect::RunModalAsk`], mirroring
//!   [`Effect::RunPluginCommand`]'s own shape exactly: the caller
//!   (`App::submit`, via `App::spawn_modal_ask`) does the actual spawn. A
//!   second modal command can be added the SAME way -- a new `SlashCommand`
//!   variant, an `execute` arm that validates and returns a new `Effect::Run*`
//!   variant, and one new arm in `submit`'s existing `Effect` match -- with
//!   no new pre-parser interception and no change to `parse`'s dispatch
//!   shape.
//! - [`SlashCommand::Settings`] is the one case that stays partly outside
//!   `execute`: it refreshes eight `Conway`-backed mirrors (`permission_
//!   grants`, `structured_allow_rules`, `permission_mode`, `permission_
//!   denies`, `permission_prompts`, `structured_deny_rules`, `structured_
//!   prompt_rules`, `hook_rules`) so the settings menu builder stays a pure
//!   function of `AppState` -- see `app.rs::submit`'s own doc for exactly
//!   where that refresh now lives and why moving it into `execute` would
//!   break `tests::settings_opens_the_menu`'s existing "a pure `AppState`
//!   flip, no facade call at all" assertion, which this item must leave
//!   passing unedited. `execute`'s own `SlashCommand::Settings` arm is
//!   UNCHANGED: still exactly `state.open_settings(); Effect::None`.

use std::collections::HashMap;
use std::sync::Arc;

use conway::plugin::{Command, CommandCtx};
use conway::{
    AgentId, AgentIntent, ContextReport, Conway, Event, ForkSpec, ModelRef, PermissionScope,
    Provenance, RoleAlias, RoutingReason, SessionHandle, SessionId, SpawnSpec, SubagentMode,
    ToolSelector, TrustPermissionReport, TrustPreview, Usage,
};

use super::form::PendingFormAsk;
use super::state::{
    AppState, AskFate, Entry, IntentChoice, IntentConfirm, Mode, PluginCommandEntry, TrustDecision,
    TrustPreviewCard,
};

/// One parsed slash command. Agent/session identifiers are still raw
/// strings here -- prefix resolution against the live tree happens in
/// [`execute`], the only place with a tree to resolve against.
#[derive(Debug, Clone, PartialEq)]
pub enum SlashCommand {
    Steer {
        target: String,
        text: String,
    },
    /// `/cancel <agent> [<reason>]` (INTENT.md §7a: anything a model can do
    /// to the session's agents, the operator can do from the terminal with
    /// one typed command -- the model already has `conway_cancel`; the
    /// operator had no counterpart, only `/quit`, which loses the parent's
    /// in-flight work too). `target` resolves through the SAME
    /// `resolve_agent` every other agent-targeted command uses (full id,
    /// exact name, unique prefix) -- see that function's own doc; `/cancel`
    /// invents no new grammar. `reason` is optional free text; `execute`
    /// supplies a default ("cancelled by operator") when it is `None`,
    /// mirroring `conway_cancel`'s own tool-facing default. Always
    /// `CancelMode::Immediate` -- the operator surface exposes no `mode`
    /// argument (see [`execute`]'s own `Cancel` arm for why: this is a
    /// "possibly non-focused subagent" instant-stop control, not a second
    /// place to reach `Graceful`, which stays reachable only through the
    /// model-facing `conway_cancel` tool). Cancelling the session's own
    /// ROOT agent is refused by `execute` before any facade call -- doing
    /// so would end the whole session, which this command's acceptance
    /// ("without ending the session") forbids; `/quit` remains the way to
    /// end the session.
    Cancel {
        target: String,
        reason: Option<String>,
    },
    Tree,
    /// `agent` is `None` for a bare `/context` (board item
    /// `01M0RWKJD04JBR5NCVKBQXYHV4`: the only way to learn an id from the
    /// TUI was a wrong-on-purpose prefix guess) -- `execute` then resolves
    /// it to `AppState::focused_agent`, mirroring [`SlashCommand::Fork`]'s
    /// own bare-defaults-to-focused convention. `agent` is `Some` for the
    /// explicit `/context <agent>` form, resolved through `resolve_agent`
    /// exactly as before.
    Context {
        agent: Option<String>,
    },
    Why,
    /// `agent` is `None` for a BARE `/fork`/`/fork <directive>` (WI "bare
    /// /spawn & /fork open an interactive session"): the child is created
    /// as a fresh, interactive KEEP-ALIVE session forked from the FOCUSED
    /// agent -- see [`execute`]'s own `Fork` arm and `parse_fork` for the
    /// exact forms this covers. `agent` is `Some` only for the explicit-
    /// target form `/fork @<agent> <directive>` (this item's generalization
    /// of the pre-existing `/fork <agent> <directive>`, unchanged in
    /// substance: an autonomous, non-keep-alive fork of that SPECIFIC live
    /// agent). `directive` is `None` when the caller supplies no first
    /// message -- the interactive child then idles until prompted
    /// (`Effect::FocusNewSession`'s own doc); for the explicit-target form
    /// `directive` is always `Some` (required, exactly as it always was).
    Fork {
        agent: Option<String>,
        directive: Option<String>,
    },
    /// `agent_def` is `None` when the caller omits it (`/spawn <prompt>`) --
    /// the spawned child then inherits the parent session's role/model (see
    /// [`parse`]'s `/spawn` branch and `conway::SpawnSpec`'s own doc).
    /// `prompt` is `None` for a BARE `/spawn`/`/spawn @<agent_def>` (this
    /// item): the child is created as a fresh, interactive KEEP-ALIVE
    /// session with no first message -- it idles until prompted.
    Spawn {
        agent_def: Option<String>,
        prompt: Option<String>,
    },
    Resume {
        sid: String,
    },
    /// `/model [<backend/model>]` (INTENT.md §5c: "changing model
    /// mid-session is ordinary, and stays cheap"). `Some(model)` is still
    /// the raw, unparsed `--model`-spelled string (`ModelRef::from_str` runs
    /// in [`execute`], where a malformed value becomes a `Notice` like any
    /// other facade failure, not a [`ParseError`]) -- see [`execute`]'s own
    /// `Model` arm for the fork-based mechanism this drives.
    ///
    /// **Board item `01M1A35S609TZ613GAECPEHX8D`: `None` (bare `/model`) is
    /// no longer a [`ParseError`].** It used to be -- `/model` with nothing
    /// after it errored the same way `/resume` with no session id does.
    /// Now it lists the configured `"backend/model"` pairs instead (a menu,
    /// when `conway.ui` is installed; a text listing otherwise) -- see
    /// [`execute`]'s own `Model { model: None }` arm.
    Model {
        model: Option<String>,
    },
    /// `/role <alias>` -- the same mid-session-switch mechanism as
    /// [`SlashCommand::Model`], naming a role instead of pinning a model
    /// directly. `role` is the raw alias text; an alias the configured
    /// `[routing]` table does not recognize is not caught here (`parse`
    /// stays state-free) -- it surfaces the first time the switched-to
    /// child actually runs a turn, the same as any other roleless-fork
    /// misconfiguration.
    Role {
        role: String,
    },
    Help,
    /// V4: opens the `/settings` menu (`view/settings.rs`), replacing the
    /// standalone `/thinking`/`/timestamps` toggles -- both REMOVED, not
    /// aliased (see this module's own doc: a per-toggle command per
    /// setting doesn't scale). Mirrors [`SlashCommand::Help`]'s own shape
    /// exactly: a pure `AppState` flag flip, no facade call, no transcript
    /// mutation.
    Settings,
    /// `/trust permissions` (board item `01KZVZ5XV162XCQR96AQKCCCF7`,
    /// formerly intercepted in `app.rs::submit`): trusts the project-scoped
    /// permissions file (`AppState::permission_paths`' first entry) and
    /// installs its current allow rules for this session. `parse` accepts
    /// only the bare form or the literal `permissions` argument -- anything
    /// else is a [`ParseError`] naming `usage: /trust permissions`, exactly
    /// the notice text the old interception pushed by hand.
    Trust,
    /// `/agents` (formerly intercepted in `app.rs::submit`): toggles the
    /// `/agents` tree view. Takes no arguments -- `parse` rejects anything
    /// else with `usage: /agents (no arguments)`.
    Agents,
    /// `/ask <text>` (B5, formerly intercepted in `app.rs::submit`): opens
    /// the single-question modal-ask flow. `question` is required by
    /// `parse` (`usage: /ask <text>` on empty); the OTHER failure mode --
    /// an ask already in flight -- can only be known at `execute` time
    /// (`AppState::ask_in_flight`), so it is checked there, not here.
    Ask {
        question: String,
    },
    Quit,
    /// A plugin-declared command:
    /// `full_name` is the command word with its leading `/` AND leading
    /// whitespace stripped (e.g. `"acme.greet"` for `/acme.greet`), still
    /// unresolved against any registry -- [`parse`] recognizes only the
    /// SHAPE (a word containing [`conway::plugin::validate_command_name`]'s
    /// namespace separator, since no built-in command name ever does),
    /// staying pure/state-free exactly like every other arm; resolving
    /// whether `full_name` actually names an installed plugin command is
    /// [`execute`]'s job, via [`Host::resolve_command`]. `args` is
    /// everything after the command word, verbatim (module notes' own
    /// "consume the remainder verbatim" rule).
    Plugin {
        full_name: String,
        args: String,
    },
    /// `/plugin` (board item `01M0VR5RCCB8NDGG2JEQW8X7XR`), now widened by
    /// board item `01M0WB5W5DX844HSJQG3JP23X0` (Q2, determine-first): bare
    /// `/plugin` (`action: None`) still opens the `/plugin` listing
    /// (`view/plugins.rs`) exactly as before -- a pure `AppState` flip, no
    /// facade call. `action: Some(..)` is the new install/uninstall
    /// trigger the listing item scoped out; see [`PluginsAction`]'s own
    /// doc for the two forms and why this extends the EXISTING variant
    /// rather than adding a new one. **Still named `Plugins`, not
    /// `Plugin`** -- `Plugin` above already names a plugin-DECLARED
    /// command's dispatch shape, and `t10_every_slash_command_variant_
    /// reaches_the_palette` (`crates/conway/tests/
    /// architecture_invariants.rs`) filters that exact string out of its
    /// scan, so reusing it here would make this variant invisibly exempt
    /// from the guard rather than covered by it.
    Plugins {
        action: Option<PluginsAction>,
    },
}

/// `/plugin`'s optional action (board item `01M0WB5W5DX844HSJQG3JP23X0`,
/// Q2/Q3 determine-first). **One surface still owns plugin listing** (the
/// principle the listing item established): this extends
/// [`SlashCommand::Plugins`] with an argument rather than adding a
/// competing command or a second view, so `/plugin` stays the one place an
/// operator types to see or change what conway can run.
///
/// **Q3, smallest honest v1: a URL argument, not a browsable catalogue.**
/// `Install` takes the marketplace URL and the plugin id directly
/// (`/plugin install <url> <id>`) -- an operator must already know both
/// (from the marketplace's own listing page, a README, etc.); this cannot
/// browse a marketplace's plugin list from inside conway, only fetch a
/// named entry from it. A browsable catalogue (fetch the manifest first,
/// render its plugin list, let the operator pick) is a real, larger
/// follow-up, not built here.
///
/// **Q4: uninstall gets a trigger too.** An operator who can install from
/// here but must hand-edit `settings.json` to remove it again is a poor
/// trade (the spec's own words) -- `Uninstall` (`/plugin uninstall <id>`)
/// is the direct counterpart, reaching `App::apply_marketplace_uninstall`
/// the same way `Install` reaches `apply_marketplace_install` (see
/// [`Effect::RunMarketplaceInstall`]/[`Effect::RunMarketplaceUninstall`]'s
/// own doc for why neither can run inside `execute` itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginsAction {
    Install {
        marketplace_url: String,
        plugin_id: String,
    },
    Uninstall {
        plugin_id: String,
    },
}

/// One palette entry: a command's name (leading `/` included, matching
/// [`SlashCommand`]'s own convention), its usage form, and a one-line
/// description.
///
/// Board item `01M0RW29F2ATVGCV0R8H0GQEYH`: this is now the ONLY type
/// carrying that text -- `view::palette::COMMANDS` used to be a second,
/// independent hand-kept array (`view/palette.rs`'s own module doc called
/// this "disclosed duplication"), and it drifted: `/trust` and `/tree`
/// worked and were never listed, `/exit`/`/quit` were listed and a naive
/// grep over the parser's `match` arms looked like they were not (they
/// were -- see [`parse`]'s combined `"/quit" | "/exit"` arm; the grep just
/// cannot see a disjunctive pattern). See [`describe`] for the mechanism
/// that makes the missing-`/trust`-and-`/tree` half of that drift
/// impossible to reintroduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
}

/// Describes one built-in [`SlashCommand`] variant for the palette --
/// **the single declaration site this item's spec asked for**: name, usage
/// and description live nowhere else, and both the palette
/// (`view::palette::draw_overlay`, via [`builtin_commands`]) and this
/// module's own tests read it.
///
/// **Why a `match` with no catch-all arm, not a shared array `parse` and
/// the palette both consult.** A shared array is DATA: nothing stops a new
/// arm being added to [`parse`]'s own `match word { .. }` (which is CODE)
/// without a corresponding array entry -- exactly the class of drift this
/// item exists to close, since the two would still be two independently
/// maintained things a person has to remember to keep in sync. Matching
/// exhaustively on the real [`SlashCommand`] enum instead ties the
/// obligation to the TYPE: adding a new variant (the established way this
/// codebase adds a command -- see this module's own doc on `/trust`,
/// `/agents`, `/ask`, each "a new `SlashCommand` variant, an `execute` arm
/// ... with no new pre-parser interception") makes this `match` stop
/// compiling, everywhere in the crate, until a `CommandSpec` is given for
/// it -- a compile failure, not a runtime gap someone has to notice. This
/// crate already uses the identical idiom for behaviour: [`execute`]'s own
/// dispatch `match cmd { .. }` has no catch-all either, so a new variant
/// already fails to build there today; this extends the same guarantee to
/// the DESCRIPTION half.
///
/// [`SlashCommand::Plugin`] is deliberately excluded from ordinary
/// coverage and panics if reached: a plugin command's name/description are
/// resolved at TUI startup from whichever plugins are installed (never
/// known at compile time -- see `CommandRegistry::palette_entries`, the
/// mechanism that already stays correct, module notes' "Plugin commands
/// are the one entry this module does NOT hand-keep"), so nothing may ever
/// call `describe` with one; the panic is what makes that misuse loud
/// rather than returning made-up text (`tests::
/// describing_a_plugin_command_panics` is the direct proof).
pub fn describe(cmd: &SlashCommand) -> CommandSpec {
    match cmd {
        SlashCommand::Ask { .. } => CommandSpec {
            name: "/ask",
            usage: "/ask <text>",
            description: "ask an ephemeral fork a question (does not affect the live session)",
        },
        SlashCommand::Agents => CommandSpec {
            name: "/agents",
            usage: "/agents",
            description: "toggle the below-chat agent-tree panel",
        },
        SlashCommand::Settings => CommandSpec {
            name: "/settings",
            usage: "/settings",
            description: "open the settings menu (display preferences -- session only)",
        },
        SlashCommand::Plugins { .. } => CommandSpec {
            name: "/plugin",
            usage: "/plugin [install <manifest-url> <plugin-id> | uninstall <plugin-id>]",
            // Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR` ruled layers 1-4: a
            // real, published Claude Code marketplace now parses
            // (`conway_plugin_marketplace::manifest`, `owner`/`metadata`
            // tolerated, a `name`+`source` entry accepted) and its
            // `git-subdir`/`github` sources fetch via the system `git`
            // binary (`conway_plugin_marketplace::git_source`) -- so THIS
            // claim is no longer the false one the prior wording in this
            // spot warned against re-adding. What is still true, and still
            // worth stating rather than promising past it: no archive-
            // requiring source kind is fetched (refused by name), and no
            // browsable catalogue exists -- an operator still needs the
            // marketplace URL and the plugin's own id/name already in hand.
            description: "list every plugin conway can run today, or install/uninstall one from \
                          a conway or Claude Code marketplace manifest URL",
        },
        SlashCommand::Trust => CommandSpec {
            name: "/trust",
            usage: "/trust permissions",
            description: "preview and trust the project permissions file",
        },
        SlashCommand::Steer { .. } => CommandSpec {
            name: "/steer",
            usage: "/steer <agent> <text>",
            description: "send a steer message to a running agent",
        },
        SlashCommand::Cancel { .. } => CommandSpec {
            name: "/cancel",
            usage: "/cancel <agent> [<reason>]",
            description: "cancel a running agent immediately (does not end the session)",
        },
        SlashCommand::Context { .. } => CommandSpec {
            name: "/context",
            usage: "/context [<agent>]",
            description: "show an agent's assembled context (defaults to the focused agent)",
        },
        SlashCommand::Tree => CommandSpec {
            name: "/tree",
            usage: "/tree",
            description: "show the agent tree (same view /agents shows)",
        },
        SlashCommand::Why => CommandSpec {
            name: "/why",
            usage: "/why",
            description: "show the last routing decision",
        },
        SlashCommand::Fork { .. } => CommandSpec {
            name: "/fork",
            usage: "/fork [<text>] | @<agent> <directive>",
            description: "open an interactive fork of the focused agent (or fork a specific agent)",
        },
        SlashCommand::Spawn { .. } => CommandSpec {
            name: "/spawn",
            usage: "/spawn [@<agent_def>] [<prompt>]",
            description:
                "open an interactive spawned agent (inherits parent's role/model if no @agent_def)",
        },
        SlashCommand::Resume { .. } => CommandSpec {
            name: "/resume",
            usage: "/resume <session-id>",
            description: "resume a prior session",
        },
        SlashCommand::Model { .. } => CommandSpec {
            name: "/model",
            usage: "/model [<backend/model>]",
            description: "list configured models (menu, with conway.ui) or switch the focused \
                           agent to a pinned model (forks; see /why for the reason)",
        },
        SlashCommand::Role { .. } => CommandSpec {
            name: "/role",
            usage: "/role <alias>",
            description:
                "switch the focused agent to a different role (forks; see /why for the reason)",
        },
        SlashCommand::Help => CommandSpec {
            name: "/help",
            usage: "/help",
            description: "show this help",
        },
        SlashCommand::Quit => CommandSpec {
            name: "/quit",
            usage: "/quit",
            description: "exit",
        },
        SlashCommand::Plugin { .. } => panic!(
            "describe() called with SlashCommand::Plugin -- plugin commands are described \
             dynamically (CommandRegistry::palette_entries), never through this table; this \
             indicates a caller reaching for the wrong mechanism"
        ),
    }
}

/// One placeholder instance per describable [`SlashCommand`] variant
/// (everything [`describe`] covers -- i.e. everything except
/// [`SlashCommand::Plugin`], which is dynamic and has no fixed instance to
/// place here), in palette display order. Field values are arbitrary empty/
/// `None` placeholders: [`describe`] only inspects which VARIANT it
/// received, never the field values, since this list exists purely to
/// enumerate variants for the palette, not to represent real parsed input.
///
/// **What this does NOT protect against** (disclosed, module notes' own
/// habit): a new `SlashCommand` variant added without a matching sample
/// here would still compile -- [`describe`]'s own `match` is what forces a
/// *description* to exist; nothing at the type level forces this Vec's
/// membership too, since Rust has no built-in "list every variant of this
/// enum" reflection without a derive macro (C-04: no new dependency for
/// one). Two test-time guards close that gap between them, and the split
/// matters because each catches what the other cannot:
///
/// - `tests::builtin_commands_round_trips_through_parse` catches a STALE or
///   mis-ordered entry -- every row this function produces must parse back
///   into a `SlashCommand` whose own [`describe`] agrees. It cannot see an
///   entry that was never produced, because it only iterates what is here.
/// - `conway`'s `architecture_invariants::t10_every_slash_command_variant_reaches_the_palette`
///   catches a WHOLLY MISSING one. It reads this file's own source, extracts
///   every `SlashCommand` variant from the enum declaration, and asserts each
///   (except `Plugin`) appears in this list. Verified to fail: deleting the
///   `Tree` sample leaves the crate compiling cleanly and every other test
///   green, and T10 alone reports it by name.
fn builtin_variant_samples() -> Vec<SlashCommand> {
    vec![
        SlashCommand::Ask {
            question: String::new(),
        },
        SlashCommand::Agents,
        SlashCommand::Settings,
        SlashCommand::Plugins { action: None },
        SlashCommand::Trust,
        SlashCommand::Steer {
            target: String::new(),
            text: String::new(),
        },
        SlashCommand::Cancel {
            target: String::new(),
            reason: None,
        },
        SlashCommand::Context { agent: None },
        SlashCommand::Tree,
        SlashCommand::Why,
        SlashCommand::Fork {
            agent: None,
            directive: None,
        },
        SlashCommand::Spawn {
            agent_def: None,
            prompt: None,
        },
        SlashCommand::Resume { sid: String::new() },
        SlashCommand::Model { model: None },
        SlashCommand::Role {
            role: String::new(),
        },
        SlashCommand::Help,
        SlashCommand::Quit,
    ]
}

/// Every built-in command's palette entry, generated from [`describe`] --
/// the palette's whole built-in half (`view::palette::matches` merges
/// plugin commands in after this, unchanged). Display order matches
/// `builtin_variant_samples`'s own order, with `/exit` appended last.
/// (Plain code span, not an intra-doc link: that function is private and
/// this one is public, which the `-D warnings` rustdoc gate rejects.)
///
/// **`/exit` is the one entry not derived from a `SlashCommand` variant.**
/// It is a second accepted SPELLING of `/quit` -- [`parse`]'s own
/// `"/quit" | "/exit"` arm -- not a distinct variant, so it cannot come
/// from the exhaustive match [`describe`] performs. Represented explicitly
/// here instead of silently dropped (dropping it would make a working
/// command undiscoverable again, the exact defect class this item exists
/// to close); `tests::exit_and_quit_both_parse_to_the_same_described_variant`
/// proves the two spellings really do parse to the identical command, so
/// this hand-written row cannot silently drift from `parse`'s own alias.
pub fn builtin_commands() -> Vec<CommandSpec> {
    let mut specs: Vec<CommandSpec> = builtin_variant_samples().iter().map(describe).collect();
    specs.push(CommandSpec {
        name: "/exit",
        usage: "/exit",
        description: "alias for /quit",
    });
    specs
}

/// A malformed slash command. `Display` always names the expected form, so
/// it can be surfaced verbatim as a transcript [`Entry::Notice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// Parses one line of raw input (leading `/` included, e.g. `"/steer a7
/// hold on"`) into a [`SlashCommand`].
///
/// Parsing rule (module notes): split on the first whitespace for the
/// command word; a command with a trailing free-text argument consumes the
/// remainder after its first argument verbatim -- no re-tokenization, no
/// quote handling.
pub fn parse(input: &str) -> Result<SlashCommand, ParseError> {
    let (word, rest) = split_command(input);
    match word {
        "/steer" => {
            let (target, text) = parse_two_arg(rest, "/steer <agent> <text>")?;
            Ok(SlashCommand::Steer { target, text })
        }
        "/cancel" => {
            let (target, reason) = parse_cancel(rest, "/cancel <agent> [<reason>]")?;
            Ok(SlashCommand::Cancel { target, reason })
        }
        "/tree" => {
            // Item A3 introduced `/tree` as an alias for the `/agents`
            // panel's own view (`execute` renders it from `state.tree`, no
            // facade call). Board item `01M0RW29F2ATVGCV0R8H0GQEYH`
            // reverses A3's OTHER choice -- hiding it from the palette --
            // which was itself the exact defect class this item exists to
            // close: a working command a person could not discover by
            // typing `/`. `/tree` is now described like any other command
            // (see [`describe`]); T7's removal of the transcript-dump
            // `/help` listing it used to be excluded from is unrelated and
            // still stands.
            parse_no_arg(rest, "/tree")?;
            Ok(SlashCommand::Tree)
        }
        "/context" => {
            // Board item `01M0RWKJD04JBR5NCVKBQXYHV4`: the argument is now
            // OPTIONAL -- a bare `/context` is not a usage error, it
            // defers to `execute`'s focused-agent default (mirrors
            // `parse_fork`'s own bare form). An explicit argument is still
            // free-form (no `@` sigil needed, unlike `/fork`/`/spawn`:
            // `/context` never takes a second, free-text argument that a
            // leading agent token would need distinguishing from).
            let value = rest.trim();
            let agent = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            Ok(SlashCommand::Context { agent })
        }
        "/why" => {
            parse_no_arg(rest, "/why")?;
            Ok(SlashCommand::Why)
        }
        "/fork" => {
            let (agent, directive) = parse_fork(rest, "/fork [@<agent> <directive>] | [<text>]")?;
            Ok(SlashCommand::Fork { agent, directive })
        }
        "/spawn" => {
            let (agent_def, prompt) = parse_spawn(rest, "/spawn [@<agent_def>] [<prompt>]")?;
            Ok(SlashCommand::Spawn { agent_def, prompt })
        }
        "/resume" => {
            let sid = parse_one_arg(rest, "/resume <session-id>")?;
            Ok(SlashCommand::Resume { sid })
        }
        "/model" => {
            // Board item `01M1A35S609TZ613GAECPEHX8D`: bare `/model` (no
            // argument) is a valid parse now, not a `ParseError` -- unlike
            // `parse_one_arg`'s every other caller, empty `rest` means
            // "list what's configured", handled entirely in `execute`
            // (parse stays state-free either way -- it only tells apart
            // "an argument was given" from "none was").
            let trimmed = rest.trim();
            let model = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
            Ok(SlashCommand::Model { model })
        }
        "/role" => {
            let role = parse_one_arg(rest, "/role <alias>")?;
            Ok(SlashCommand::Role { role })
        }
        "/help" => {
            parse_no_arg(rest, "/help")?;
            Ok(SlashCommand::Help)
        }
        "/settings" => {
            parse_no_arg(rest, "/settings")?;
            Ok(SlashCommand::Settings)
        }
        "/plugin" => {
            let action = parse_plugins_action(
                rest,
                "/plugin [install <manifest-url> <plugin-id> | uninstall <plugin-id>]",
            )?;
            Ok(SlashCommand::Plugins { action })
        }
        "/trust" => {
            // Accepts the bare form or the one literal argument
            // `permissions` -- exactly the form the old `app.rs::submit`
            // interception checked by hand (`arg.is_empty() || arg ==
            // "permissions"`), so this `ParseError`'s text becomes the
            // IDENTICAL `Entry::Notice` the removed interception used to
            // push directly, once it reaches `submit`'s shared `Err(e)` arm.
            let arg = rest.trim();
            if arg.is_empty() || arg == "permissions" {
                Ok(SlashCommand::Trust)
            } else {
                Err(ParseError("usage: /trust permissions".to_string()))
            }
        }
        "/agents" => {
            parse_no_arg(rest, "/agents")?;
            Ok(SlashCommand::Agents)
        }
        "/ask" => {
            let question = parse_one_arg(rest, "/ask <text>")?;
            Ok(SlashCommand::Ask { question })
        }
        "/quit" | "/exit" => {
            parse_no_arg(rest, word)?;
            Ok(SlashCommand::Quit)
        }
        other => {
            // a plugin command's full
            // name is ALWAYS `plugin_id.command_name` (see `SlashCommand::
            // Plugin`'s own doc) -- no built-in command word above ever
            // contains the namespace separator, so recognizing the SHAPE
            // here is enough to route to plugin dispatch with zero risk of
            // ever mis-capturing a built-in as a plugin command, and no
            // registry lookup is needed to stay pure. Whether `full_name`
            // actually names something installed is `execute`'s job.
            // '.' here is the identical separator
            // `conway::plugin::validate_command_name` enforces (re-exported
            // from `conway_core::event_name::EVENT_NAMESPACE_SEPARATOR`) --
            // asserted equal, not merely commented, by
            // `plugin_shape_check_uses_the_same_separator_validate_command_name_enforces`
            // below, so the two can never silently desync.
            //
            // Board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`: a LEADING `:` is
            // ALSO accepted here, as a typed-input ALIAS for `.` -- real
            // Claude Code names a plugin skill/command `/<plugin-id>:
            // <name>` (a colon), and the operator's own trigger for this
            // item was typing exactly that (`/ideate:refine`) and getting
            // "unknown command". `EVENT_NAMESPACE_SEPARATOR` itself stays
            // `.` (unchanged, still load-bearing for `validate_command_name`
            // and every other plugin-declared event/command); only the
            // FIRST `:` in what the operator TYPED is translated to `.`
            // before it becomes `full_name`, so `CommandRegistry`'s own
            // lookup keys (always `.`-joined) need no change either. A `.`
            // and a `:` in the SAME typed word (an unusual plugin id
            // containing `.` of its own, spelled Claude Code's way) still
            // resolves correctly: only the first `:` is rewritten, so
            // `"foo.bar:baz"` becomes `"foo.bar.baz"`, the same full name a
            // plugin id containing `.` already produces today.
            let bare = other.strip_prefix('/').unwrap_or(other);
            let full_name = match bare.split_once(':') {
                Some((head, tail)) => format!("{head}.{tail}"),
                None => bare.to_string(),
            };
            if full_name.contains('.') {
                Ok(SlashCommand::Plugin {
                    full_name,
                    args: rest.to_string(),
                })
            } else {
                Err(ParseError(format!(
                    "unknown command `{other}` -- try /help"
                )))
            }
        }
    }
}

/// Splits `input` into its command word and the (left-trimmed) remainder,
/// on the first whitespace run.
fn split_command(input: &str) -> (&str, &str) {
    match input.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim_start()),
        None => (input, ""),
    }
}

fn parse_no_arg(rest: &str, form: &str) -> Result<(), ParseError> {
    if rest.trim().is_empty() {
        Ok(())
    } else {
        Err(ParseError(format!("usage: {form} (no arguments)")))
    }
}

fn parse_one_arg(rest: &str, form: &str) -> Result<String, ParseError> {
    let value = rest.trim();
    if value.is_empty() {
        Err(ParseError(format!("usage: {form}")))
    } else {
        Ok(value.to_string())
    }
}

/// Splits `rest` into its first whitespace-delimited token and everything
/// after the single separating whitespace char, verbatim (module notes:
/// "consume the remainder verbatim, no re-tokenization"). Errors when
/// either half is missing -- covers both "no arguments at all" and "first
/// argument but no free-text second argument" (e.g. `/fork a7` with no
/// directive) under the same message.
fn parse_two_arg(rest: &str, form: &str) -> Result<(String, String), ParseError> {
    match rest.split_once(char::is_whitespace) {
        Some((first, text)) if !text.trim().is_empty() => Ok((first.to_string(), text.to_string())),
        _ => Err(ParseError(format!("usage: {form}"))),
    }
}

/// Parses `/cancel`'s argument list: `<agent>` is required, `<reason>` is
/// NOT -- unlike [`parse_two_arg`] (`/steer`'s parser, which requires its
/// second argument), a cancel is well-formed with no reason at all; `execute`
/// supplies a default when this returns `None`, mirroring `conway_cancel`'s
/// own tool-facing `CancelArgs::reason` default. `<agent>` is still
/// unresolved here -- `execute`'s [`resolve_agent`] call is what turns it
/// into a live id.
fn parse_cancel(rest: &str, form: &str) -> Result<(String, Option<String>), ParseError> {
    let trimmed = rest.trim_start();
    if trimmed.is_empty() {
        return Err(ParseError(format!("usage: {form}")));
    }
    match trimmed.split_once(char::is_whitespace) {
        Some((agent, reason)) => {
            let reason = reason.trim();
            let reason = if reason.is_empty() {
                None
            } else {
                Some(reason.to_string())
            };
            Ok((agent.to_string(), reason))
        }
        None => Ok((trimmed.to_string(), None)),
    }
}

/// Parses `/spawn`'s argument list, where naming an `agent_def` is optional
/// (module notes / this item's own doc: no `agent_def` means the spawned
/// child inherits the parent session's role/model) AND -- since the "bare
/// /spawn & /fork open an interactive session" item -- the prompt itself is
/// now ALSO optional (a bare `/spawn`/`/spawn @<agent_def>` creates a fresh,
/// interactive keep-alive session with no first message; `execute` supplies
/// the first message later, via `Effect::FocusNewSession`, only if one was
/// given here). The unambiguous syntax for naming an `agent_def` is a
/// leading `@<agent_def>` token -- distinguishable from the prompt by the
/// `@` sigil with no positional guessing:
///
/// - `/spawn` -- no agent_def, no prompt: bare interactive spawn.
/// - `/spawn <prompt>` -- no agent_def, `prompt` is the first message.
/// - `/spawn @<agent_def>` -- names an agent_def, no first message.
/// - `/spawn @<agent_def> <prompt>` -- names an agent_def AND a first
///   message.
/// - `/spawn @@<prompt>` -- escape hatch: a prompt that must begin with a
///   literal `@` (no agent_def). Without this, a prompt like `@channel ...`
///   would be silently mis-split into an agent_def + a truncated prompt.
fn parse_spawn(rest: &str, _form: &str) -> Result<(Option<String>, Option<String>), ParseError> {
    if let Some(after_at_at) = rest.strip_prefix("@@") {
        // Literal-`@` prompt, no agent_def: re-attach the single `@` the
        // escape consumed and treat the whole thing as the prompt.
        let after = after_at_at.trim();
        let prompt = if after.is_empty() {
            None
        } else {
            Some(format!("@{after}"))
        };
        return Ok((None, prompt));
    }
    match rest.strip_prefix('@') {
        Some(after_at) => {
            let after_at = after_at.trim_start();
            match after_at.split_once(char::is_whitespace) {
                Some((agent_def, prompt)) if !agent_def.is_empty() => {
                    let prompt = prompt.trim();
                    let prompt = if prompt.is_empty() {
                        None
                    } else {
                        Some(prompt.to_string())
                    };
                    Ok((Some(agent_def.to_string()), prompt))
                }
                // No whitespace at all (or an empty leading token): the
                // entire remainder is the agent_def name, no prompt.
                _ if !after_at.is_empty() => Ok((Some(after_at.to_string()), None)),
                _ => Ok((None, None)),
            }
        }
        None => {
            let prompt = rest.trim();
            if prompt.is_empty() {
                Ok((None, None))
            } else {
                Ok((None, Some(prompt.to_string())))
            }
        }
    }
}

/// Parses `/fork`'s argument list. Generalizes the pre-existing explicit
/// two-argument form (`/fork <agent> <directive>`, forking a NAMED live
/// agent autonomously) to a leading `@<agent>` sigil -- mirroring
/// [`parse_spawn`]'s own `@` convention for the same reason (unambiguously
/// distinguishing "name a target" from free text, no positional guessing)
/// -- and adds the bare/optional-text forms the "bare /spawn & /fork open
/// an interactive session" item introduces:
///
/// - `/fork` -- no target, no directive: a bare interactive fork of the
///   FOCUSED agent (`execute` resolves it via `AppState::focused_agent`),
///   idling until prompted.
/// - `/fork <text>` -- no target; `text` (verbatim, however many words)
///   becomes the interactive child's first message.
/// - `/fork @<agent> <directive>` -- explicit target: forks that SPECIFIC
///   live agent with `directive` (both required, exactly like the
///   pre-this-item two-argument form did) -- `execute` keeps this
///   autonomous, NOT keep-alive.
/// - `/fork @@<text>` -- escape hatch: a first message that must begin with
///   a literal `@`, no explicit target.
fn parse_fork(rest: &str, form: &str) -> Result<(Option<String>, Option<String>), ParseError> {
    if rest.trim().is_empty() {
        return Ok((None, None));
    }
    if let Some(after_at_at) = rest.strip_prefix("@@") {
        let directive = parse_one_arg(&format!("@{after_at_at}"), form)?;
        return Ok((None, Some(directive)));
    }
    match rest.strip_prefix('@') {
        Some(after_at) => {
            let (agent, directive) = parse_two_arg(after_at, form)?;
            if agent.is_empty() {
                return Err(ParseError(format!("usage: {form}")));
            }
            Ok((Some(agent), Some(directive)))
        }
        None => {
            let directive = parse_one_arg(rest, form)?;
            Ok((None, Some(directive)))
        }
    }
}

/// Parses `/plugin`'s optional trailing action (board item
/// `01M0WB5W5DX844HSJQG3JP23X0`): bare/empty is `None` (open the listing,
/// unchanged); `install <marketplace-url> <plugin-id>` and `uninstall
/// <plugin-id>` are the two forms [`PluginsAction`] carries. **P-10:**
/// `marketplace_url` is only ever split as a whitespace-delimited token
/// here, never parsed as a URL -- the third-party RESPONSE it names is
/// untrusted, but the request text itself is not further validated at
/// this layer (no new dependency for a `Url` type this crate does not
/// otherwise need, C-04); a malformed URL is reported cleanly by
/// `conway_plugin_marketplace::fetch_marketplace` itself once dispatch
/// reaches it (already tested, that crate's own suite), never a panic
/// here.
fn parse_plugins_action(rest: &str, form: &str) -> Result<Option<PluginsAction>, ParseError> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let (word, action_rest) = split_command(trimmed);
    match word {
        "install" => {
            let (marketplace_url, plugin_id) = parse_two_arg(action_rest, form)?;
            Ok(Some(PluginsAction::Install {
                marketplace_url,
                plugin_id,
            }))
        }
        "uninstall" => {
            let plugin_id = parse_one_arg(action_rest, form)?;
            Ok(Some(PluginsAction::Uninstall { plugin_id }))
        }
        _ => Err(ParseError(format!("usage: {form}"))),
    }
}

/// The effect an executed command has on the caller's [`App`](super::app::App)
/// loop, beyond the [`AppState`] mutation `execute` already performed
/// directly.
pub enum Effect {
    /// Nothing further to do.
    None,
    /// `/quit` -- the app loop should exit.
    Quit,
    /// `/resume` succeeded -- the caller's active `SessionHandle` must be
    /// swapped for this one and its event stream resubscribed (`execute`
    /// cannot do either itself: both live in the app loop, not here).
    Resumed(SessionHandle),
    /// A bare/implicit `/spawn` or `/fork` succeeded (WI "bare /spawn &
    /// /fork open an interactive session"): `child` was created as a fresh,
    /// interactive KEEP-ALIVE session and must be auto-focused by the app
    /// loop (`app.rs` reuses the existing `Action::FocusAgent` path --
    /// `AppState::focus_agent` + re-subscribing `handle.agent_events(child)`
    /// -- neither of which `execute` can do itself: focus-switching needs
    /// the live facade, which only `app.rs` holds). `first_message`, when
    /// `Some` (the caller supplied `/spawn <text>`/`/fork <text>`), must
    /// then be delivered to `child` via `SessionHandle::prompt_agent` --
    /// again something only `app.rs` can do, since `execute` has no live
    /// handle either. Deliberately NOT baked into the `SpawnSpec`/
    /// `ForkSpec` that created `child`: those are always built with an
    /// EMPTY prompt/directive (`execute`'s own `Spawn`/`Fork` arms), so the
    /// child starts genuinely idle (`conway_runtime::subagent`'s own doc on
    /// `keep_alive` + an empty prompt) and `first_message` becomes the
    /// child's own first `UserTurn`, indistinguishable from any later
    /// message the user types once focused on it.
    FocusNewSession {
        child: AgentId,
        /// The agent `child` was spawned/forked under (root for `/spawn`, the
        /// focused agent for `/fork`). The app loop seeds `child`'s `/agents`
        /// tree node under this parent immediately (`AppState::
        /// ensure_agent_tracked`), rather than waiting for `child`'s
        /// `AgentSpawned` event -- which never arrives on the stream the app
        /// switches to. The app swaps its event subscription to
        /// `agent_events(child)` the SAME turn, and that stream's replay is
        /// `child`'s own records only (never its own spawn lifecycle event),
        /// while the live half subscribed only AFTER the spawn already fired.
        /// Without this seed the freshly created session is missing from the
        /// panel until some LATER tree event happens to redraw it.
        parent: AgentId,
        first_message: Option<String>,
    },
    /// A resolved plugin command is ready to run. **`execute` never calls
    /// `command.invoke` itself** -- it only resolves `full_name` against the
    /// registry and builds `ctx`, both synchronous, bounded-time operations;
    /// running the plugin's OWN async code is deferred to this effect so the
    /// caller (`app.rs::App::run`) can spawn it off the render/input loop
    /// (mirroring the existing `/ask` modal's own `tokio::spawn` +
    /// channel-reply shape, `run_modal_ask`). This is the load-bearing
    /// property behind this item's own hang/panic-safety acceptance
    /// criterion: since `execute` (the thing `App::run`'s `select!` loop
    /// DOES await directly) never runs a byte of plugin code, a hanging or
    /// panicking `Command::invoke` cannot block or crash the loop that
    /// awaits `execute` -- see `commands::tests::
    /// execute_never_awaits_a_hanging_plugin_command` for the direct proof.
    RunPluginCommand(PluginCommandInvocation),
    /// `/ask <question>` (B5) validated -- `state.ask_in_flight` is already
    /// set. **`execute` never spawns the task itself** -- the same reasoning
    /// as [`Self::RunPluginCommand`]'s own doc: forking the ephemeral child
    /// and draining its turn needs the live `SessionHandle` and `App`'s own
    /// `modal_ask_tx`, neither of which `Host`/`execute` has, and running it
    /// here would be exactly the kind of long-lived async work `execute`
    /// must never itself await (mirrors the hang-safety property that
    /// doc names). The caller (`App::submit`, via `App::spawn_modal_ask`)
    /// does the actual `tokio::spawn`.
    RunModalAsk { question: String },
    /// `/plugin install <url> <id>` validated -- **`execute` never fetches
    /// anything itself.** `App::apply_marketplace_install` needs `env`
    /// (to resolve `settings.json`'s path via `CONWAY_CONFIG_DIR`) and
    /// `cwd` (the project config layer, for its own honesty check) --
    /// neither of which `Host` carries (`Host`'s own doc: "a thin
    /// abstraction over exactly `SessionHandle`/`Conway`'s own methods";
    /// this needs neither) -- plus a real network fetch this crate's own
    /// dependency graph deliberately keeps out of `execute`'s reach
    /// (`conway-plugin-marketplace` is a dependency of `app::marketplace`
    /// only). Board item `01M0WB5W5DX844HSJQG3JP23X0`'s determine-first
    /// question 1 answer: the caller (`App::submit`) calls `App::
    /// apply_marketplace_install` directly, awaited inline -- **not**
    /// spawned off the loop the way `RunPluginCommand`/`RunModalAsk` are
    /// (see `app/marketplace.rs`'s own module doc for why that shape was
    /// considered and rejected here: it would have required splitting the
    /// already-tested method into two independently-shaped halves for a
    /// fetch already bounded by a 20s client timeout).
    RunMarketplaceInstall {
        marketplace_url: String,
        plugin_id: String,
    },
    /// `/plugin uninstall <id>` validated. Unlike `Install`, `App::
    /// apply_marketplace_uninstall` touches no network (its own doc:
    /// "Synchronous (no network): both steps are local filesystem
    /// operations") -- so there was never a spawn-vs-await question here,
    /// only the same `env`/`cwd` `execute` cannot reach. The caller
    /// (`App::submit`) calls it directly.
    RunMarketplaceUninstall { plugin_id: String },
}

/// What [`Effect::RunPluginCommand`] carries: the resolved command object,
/// its invocation context, and the full name to attribute output/errors to
/// once it completes.
pub struct PluginCommandInvocation {
    pub full_name: String,
    pub command: Arc<dyn Command>,
    pub ctx: CommandCtx,
}

/// The facade surface commands dispatch through -- abstracted behind a
/// trait so dispatch logic is unit-testable against a fake, with no live
/// `Runtime` (module notes: "headless, fake `SessionHandle` seam").
#[async_trait::async_trait]
pub trait Host {
    fn root(&self) -> AgentId;
    /// The CALLING session's own id:
    /// what [`SlashCommand::Plugin`]'s dispatch arm below stamps into
    /// [`conway::plugin::CommandCtx::session_id`] -- the one identity a
    /// `CommandOutcome::ForkSession` reply is ever resolved against (see
    /// that variant's own doc). `LiveHost::session_id` is a thin passthrough
    /// to `SessionHandle::id`, exactly like [`Self::root`]'s own passthrough
    /// to `SessionHandle::root`.
    fn session_id(&self) -> SessionId;
    /// A thin passthrough to `SessionHandle::context_report_current` (T3
    /// follow-up) -- NOT the plain `SessionHandle::context_report`: the
    /// `_current` variant closes that method's documented resumed-session
    /// gap (falls back to the most recently PERSISTED report when this
    /// process has no live one yet for `agent`), so every caller reached
    /// through this trait -- `/context` and `try_focus_agent`'s re-fetch
    /// alike -- gets the fallback for free rather than each needing to know
    /// to ask for it.
    async fn context_report(&self, agent: AgentId) -> conway::Result<ContextReport>;
    /// The focused agent's cumulative token spend -- a thin passthrough to
    /// `SessionHandle::session_usage`, reached through this trait -- like
    /// every other method here -- so `app.rs`'s status-line refresh logic
    /// stays unit-testable against a fake, with no live `Runtime`.
    async fn session_usage(&self, agent: AgentId) -> conway::Result<Usage>;
    /// T3 follow-up: a thin passthrough to `SessionHandle::last_model` --
    /// the model that served `agent`'s most recent completed turn, `None`
    /// if it has not completed one. `try_focus_agent`'s re-fetch is this
    /// trait's only caller today; routed through `Host` like every other
    /// method here so that re-fetch stays unit-testable against a fake.
    async fn last_model(&self, agent: AgentId) -> conway::Result<Option<ModelRef>>;
    async fn fork(&self, from: AgentId, spec: ForkSpec) -> conway::Result<AgentId>;
    async fn spawn(&self, from: AgentId, spec: SpawnSpec) -> conway::Result<AgentId>;
    async fn steer(&self, target: AgentId, text: String) -> conway::Result<()>;
    /// `/cancel <agent> [<reason>]`: a thin passthrough to
    /// `SessionHandle::cancel` (always `CancelMode::Immediate` -- see
    /// [`SlashCommand::Cancel`]'s own doc for why the operator surface
    /// exposes no `mode` argument), the SAME facade method the model-facing
    /// `conway_cancel` tool's convenience wrapper (`SubagentHandle::cancel`)
    /// reduces to internally -- both ultimately reach `SubagentHost::cancel`
    /// on the one shared `Runtime`, so an operator cancel and a model cancel
    /// of the same agent do the identical thing. Routed through this trait
    /// like every other facade call so `execute`'s `SlashCommand::Cancel`
    /// arm is unit-testable against `tests::FakeHost`.
    async fn cancel(&self, target: AgentId, reason: String) -> conway::Result<()>;
    async fn resume(&self, sid: SessionId) -> conway::Result<SessionHandle>;
    /// The `/ask` modal's three fates (B5) -- one facade op each: promote
    /// (B3, `[f]` keep), pull_in (B4, `[p]` merge into the parent), purge
    /// (`[esc]` discard, and the quit-with-modal-open fallback). Routed
    /// through this trait like every other facade call so the modal's fate
    /// dispatch (`apply_ask_fate`) is unit-testable against a fake (
    /// the TUI never reaches the store directly).
    async fn promote(&self, agent: AgentId) -> conway::Result<SessionId>;
    async fn pull_in(&self, child: AgentId) -> conway::Result<()>;
    async fn purge(&self, agent: AgentId) -> conway::Result<()>;
    /// C2: classifies a natural-language `/fork`/`/spawn` request via
    /// `Conway::classify_agent_intent` (C1) -- run as an EPHEMERAL one-turn
    /// session under the declarative `intent` role, then purged before
    /// returning. Routed through this trait like every other facade call
    /// so the free-text routing decision in `execute` is unit-testable
    /// against a fake (the TUI never reaches the store directly, only the facade).
    /// `default_recipe` is the CALLER's command default (`Fork` for
    /// `/fork`, `Spawn` for `/spawn`); every degraded path returns a
    /// verbatim passthrough `AgentIntent` carrying that recipe, the raw
    /// text, and no agent def (so a classifier failure can never break the
    /// command), while a real backend failure propagates as
    /// `FacadeError::IntentClassification` -- see `conway::intent`'s
    /// module doc for the full untrusted-output validation policy.
    async fn classify_agent_intent(
        &self,
        parent: AgentId,
        default_recipe: SubagentMode,
        text: &str,
    ) -> conway::Result<AgentIntent>;

    /// `/trust permissions` (board item `01KZVZ5XV162XCQR96AQKCCCF7`): a
    /// thin passthrough to `Conway::trust_permission_file`, reached through
    /// this trait like every other facade call so `execute`'s
    /// `SlashCommand::Trust` arm is unit-testable against a fake. Returns
    /// `std::io::Result`, NOT `conway::Result` -- deliberately: `Conway::
    /// trust_permission_file`'s own signature already returns `std::io::
    /// Result`, and converting through `conway::FacadeError::Io` would
    /// prepend that variant's `"io error: "` `Display` prefix onto every
    /// message the old `app.rs::submit` interception used to show verbatim
    /// (e.g. the unrecognized-top-level-key case's own message), which
    /// would be an observable wording regression this item's "must not
    /// regress" acceptance forbids.
    async fn trust_permission_file(
        &self,
        path: &std::path::Path,
        scope: PermissionScope,
        granting_agent: AgentId,
    ) -> std::io::Result<TrustPermissionReport>;

    /// `/trust permissions`'s read-only FIRST step (board item, split from
    /// `01KZHVFCN6ZEAXV7K5JHRQN1YB`'s `(kind, id, digest)`/plugin-subject
    /// generalisation, which this does not pre-empt): a thin passthrough to
    /// `Conway::preview_trust_target`, reached through this trait like
    /// every other facade call so `execute`'s `SlashCommand::Trust` arm
    /// (which now opens a preview card instead of trusting immediately) is
    /// unit-testable against a fake. Returns `std::io::Result`, matching
    /// [`Self::trust_permission_file`]'s own choice and for the identical
    /// reason: `Conway::preview_trust_target` already returns `std::io::
    /// Result`, and this is a pure read, so there is no separate class of
    /// facade-level error to fold in.
    async fn preview_trust_target(&self, path: &std::path::Path) -> std::io::Result<TrustPreview>;

    /// Resolves a plugin command's full name (e.g. `"acme.greet"`, the same
    /// string [`SlashCommand::Plugin::full_name`] carries) against the
    /// installed [`CommandRegistry`], or `None` if nothing is registered
    /// under that name. **Synchronous, not async** -- deliberately: a
    /// registry lookup is an in-memory `HashMap::get`, and keeping this off
    /// `async_trait` makes it visible at a glance that resolving a plugin
    /// command can never itself be the thing that blocks (running the
    /// resolved command is a SEPARATE step -- see [`Effect::
    /// RunPluginCommand`]'s own doc).
    fn resolve_command(&self, full_name: &str) -> Option<Arc<dyn Command>>;
}

/// The live [`Host`]: pure delegation to a `SessionHandle` + `Conway` pair
/// -- no logic of its own, per this item's own objective ("none of them may
/// reach past `SessionHandle`/`Conway`").
pub struct LiveHost<'a> {
    pub handle: &'a SessionHandle,
    pub conway: &'a Conway,
    /// The installed plugin commands
    /// -- `App` builds this once, at construction (`CommandRegistry::
    /// build`), from the SAME plugin list it was handed; `LiveHost` borrows
    /// it fresh per call, mirroring `handle`/`conway`'s own borrow shape.
    pub commands: &'a CommandRegistry,
}

#[async_trait::async_trait]
impl Host for LiveHost<'_> {
    fn root(&self) -> AgentId {
        self.handle.root()
    }

    fn session_id(&self) -> SessionId {
        self.handle.id()
    }

    async fn context_report(&self, agent: AgentId) -> conway::Result<ContextReport> {
        self.handle.context_report_current(agent).await
    }

    async fn session_usage(&self, agent: AgentId) -> conway::Result<Usage> {
        self.handle.session_usage(agent).await
    }

    async fn last_model(&self, agent: AgentId) -> conway::Result<Option<ModelRef>> {
        self.handle.last_model(agent).await
    }

    async fn fork(&self, from: AgentId, spec: ForkSpec) -> conway::Result<AgentId> {
        self.handle.fork(from, spec).await
    }

    async fn spawn(&self, from: AgentId, spec: SpawnSpec) -> conway::Result<AgentId> {
        self.handle.spawn(from, spec).await
    }

    async fn steer(&self, target: AgentId, text: String) -> conway::Result<()> {
        self.handle.steer(target, text).await
    }

    async fn cancel(&self, target: AgentId, reason: String) -> conway::Result<()> {
        self.handle.cancel(target, &reason).await
    }

    async fn resume(&self, sid: SessionId) -> conway::Result<SessionHandle> {
        self.conway.resume(sid).await
    }

    async fn promote(&self, agent: AgentId) -> conway::Result<SessionId> {
        self.conway.promote(agent).await
    }

    async fn pull_in(&self, child: AgentId) -> conway::Result<()> {
        self.conway.pull_in(child).await
    }

    async fn purge(&self, agent: AgentId) -> conway::Result<()> {
        self.conway.purge(agent).await
    }

    async fn classify_agent_intent(
        &self,
        parent: AgentId,
        default_recipe: SubagentMode,
        text: &str,
    ) -> conway::Result<AgentIntent> {
        self.conway
            .classify_agent_intent(parent, default_recipe, text)
            .await
    }

    async fn trust_permission_file(
        &self,
        path: &std::path::Path,
        scope: PermissionScope,
        granting_agent: AgentId,
    ) -> std::io::Result<TrustPermissionReport> {
        // Collected fresh per call, exactly like the old `app.rs::submit`
        // interception did -- `Conway::trust_permission_file`'s own
        // `TrustStore::trust` reads `env` for user config resolution.
        let env_vars: HashMap<String, String> = std::env::vars().collect();
        self.conway
            .trust_permission_file(&env_vars, path, scope, granting_agent)
    }

    async fn preview_trust_target(&self, path: &std::path::Path) -> std::io::Result<TrustPreview> {
        // Collected fresh per call, mirroring `trust_permission_file` just
        // above -- `Conway::preview_trust_target`'s own `TrustStore::load`
        // reads `env` for user config resolution.
        let env_vars: HashMap<String, String> = std::env::vars().collect();
        self.conway.preview_trust_target(&env_vars, path)
    }

    fn resolve_command(&self, full_name: &str) -> Option<Arc<dyn Command>> {
        self.commands.resolve(full_name)
    }
}

/// A registration-time defect:
/// [`CommandRegistry::build`] refuses to install a malformed or colliding
/// plugin command rather than silently dropping or overwriting it --
/// "a surfaced, named error at install time, not a silent win or a silent
/// loss" (this item's own acceptance). `App::new` propagates this as a
/// startup failure (`FacadeError::Config`), so a defect here stops the TUI
/// from starting with a clear message, the same severity every OTHER
/// startup misconfiguration (a malformed permissions file, an unknown
/// `[plugins].install` id) already gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRegistrationError(String);

impl std::fmt::Display for CommandRegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CommandRegistrationError {}

/// The resolved set of plugin-declared TUI commands -- built once, at TUI startup
/// (`CommandRegistry::build`), from the same installed plugin list
/// `conway-cli` fed to `ConwayBuilder`; consulted by [`LiveHost::
/// resolve_command`] on every `/`-prefixed submit and by `AppState::
/// plugin_commands` (a derived, `/help`-palette-shaped projection built
/// once alongside this registry -- see [`Self::palette_entries`]) for
/// discovery.
#[derive(Default)]
pub struct CommandRegistry {
    entries: HashMap<String, Arc<dyn Command>>,
    /// Declaration order (plugin order, then each plugin's own `commands()`
    /// order) -- kept separately from `entries` (a `HashMap`, unordered) so
    /// `/help`/the palette present a STABLE order across runs rather than
    /// whatever `HashMap` iteration happens to produce.
    order: Vec<String>,
}

impl std::fmt::Debug for CommandRegistry {
    // Manual impl: `Arc<dyn Command>` carries no `Debug` bound (adding one
    // to the `Command` trait would burden every plugin author for a
    // TEST-only convenience -- `unwrap_err`'s own bound is the only
    // consumer). Lists the registered full names only, in declaration
    // order, which is exactly what a test failure message needs to name.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandRegistry")
            .field("commands", &self.order)
            .finish()
    }
}

impl CommandRegistry {
    /// Builds a registry from every installed plugin's own [`conway::plugin::
    /// Plugin::commands`], namespacing each with its declaring plugin's
    /// [`conway::plugin::PluginManifest::id`] (`conway::plugin::
    /// validate_command_name` -- the SAME rule `conway_core::event_name::
    /// validate_event_name` already enforces for plugin-declared events,
    /// reused rather than reinvented; see that function's own doc).
    ///
    /// **Shadowing a built-in is impossible by construction, not merely
    /// checked here.** Every registered full name is `plugin_id` +
    /// [`conway::plugin`]'s namespace separator + the plugin's own bare
    /// command name, and no built-in `SlashCommand` word (`help`, `quit`,
    /// `fork`, ...) contains that separator -- so no plugin, however it
    /// names itself or its command, can ever produce a full name equal to a
    /// bare built-in's. `commands::tests::
    /// a_plugin_naming_its_command_help_does_not_shadow_the_built_in_help`
    /// proves this directly, adversarially, rather than leaving it as an
    /// assertion in prose.
    ///
    /// What CAN still collide under this scheme, and what this DOES check:
    /// two commands (from the same plugin, or two different ones) landing
    /// on the identical full name -- refused as a named
    /// [`CommandRegistrationError`], the collision the "not a silent win or
    /// a silent loss" acceptance language is actually about once bare-name
    /// shadowing is structurally excluded.
    pub fn build(
        plugins: &[Arc<dyn conway::plugin::Plugin>],
    ) -> Result<Self, CommandRegistrationError> {
        let mut entries: HashMap<String, Arc<dyn Command>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for plugin in plugins {
            let manifest = plugin.manifest();
            for command in plugin.commands() {
                let spec = command.spec();
                if spec.name.is_empty() || spec.name.chars().any(char::is_whitespace) {
                    return Err(CommandRegistrationError(format!(
                        "plugin '{}' declared an invalid command name '{}': a command name \
                         must be non-empty and contain no whitespace, or it can never be typed \
                         (`commands::parse` splits on the first whitespace run)",
                        manifest.id, spec.name
                    )));
                }
                let full_name = format!("{}.{}", manifest.id, spec.name);
                conway::plugin::validate_command_name(&full_name, Some(&manifest.id))
                    .map_err(CommandRegistrationError)?;
                if entries.contains_key(&full_name) {
                    return Err(CommandRegistrationError(format!(
                        "duplicate plugin command '/{full_name}' -- declared more than once \
                         (plugin '{}', command '{}')",
                        manifest.id, spec.name
                    )));
                }
                entries.insert(full_name.clone(), command);
                order.push(full_name);
            }
        }

        Ok(Self { entries, order })
    }

    /// Looks up a full name (e.g. `"acme.greet"`, no leading `/`) against
    /// this registry. `None` for anything not registered -- including a
    /// shape that merely LOOKS plugin-namespaced (see [`SlashCommand::
    /// Plugin`]'s own doc: `parse` recognizes the shape, not membership).
    pub fn resolve(&self, full_name: &str) -> Option<Arc<dyn Command>> {
        self.entries.get(full_name).cloned()
    }

    /// A `/help`-palette-shaped projection, in declaration order: one
    /// [`PluginCommandEntry`] per registered command, `name` already
    /// carrying its leading `/` (matching `view::palette::CommandSpec::
    /// name`'s own convention) so the view layer never has to remember to
    /// add it.
    pub fn palette_entries(&self) -> Vec<PluginCommandEntry> {
        self.order
            .iter()
            .map(|full_name| {
                let spec = self.entries[full_name].spec();
                PluginCommandEntry {
                    name: format!("/{full_name}"),
                    description: spec.summary,
                }
            })
            .collect()
    }

    /// Whether this registry has no declared commands at all -- `App::new`
    /// uses this only to decide whether constructing the fixture-carrying
    /// test harness below needs a non-default registry; no production call
    /// site needs it today (`palette_entries`'s own empty `Vec` already
    /// degrades correctly wherever it is consumed).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The tool profile a bare `/fork`/`/spawn`'s fresh, interactive keep-alive
/// child gets: the same "pure and
/// light" exclusion `App::new` gives the TUI root -- excludes `report`, since
/// an interactive keep-alive child (like the root) has no parent to report an
/// `AgentResult` to, and would otherwise hit the permission gate on a tool
/// call nothing downstream ever unblocks. `conway_fork`/`conway_spawn` and
/// every other builtin tool stay available. Deliberately NOT applied to the
/// explicit-target `/fork @<agent> <directive>` arm above -- that fork stays
/// autonomous (non-keep-alive) and keeps the default toolset, `report`
/// included, exactly as an autonomous `conway_fork`/`conway_spawn`-started
/// child does.
fn interactive_keep_alive_tools() -> ToolSelector {
    ToolSelector::Except(vec!["report".into()])
}

/// The bare/implicit `/fork` execution path (WI "bare /spawn & /fork open an
/// interactive session"): a fresh, interactive KEEP-ALIVE fork of `focused`
/// with an EMPTY directive (the child inherits context at head and idles);
/// `first_message`, when `Some` (the caller supplied `/fork <text>`), is
/// delivered to `child` via `Effect::FocusNewSession`, not baked into the
/// `ForkSpec` itself (see that variant's own doc). Factored out of `execute`'s
/// `Fork` arm so the C2 intent-classifier fallback (`IntentClassification` ->
/// manual flow with the raw text) reuses the exact same path, and the
/// `IntentChoice::Manual`/`Confirm` arms in [`execute_intent_confirm`] can
/// dispatch back through it via a synthetic `SlashCommand::Fork`.
async fn bare_fork<H: Host>(
    state: &mut AppState,
    host: &H,
    focused: AgentId,
    first_message: Option<String>,
) -> Effect {
    match host
        .fork(
            focused,
            ForkSpec::new("")
                .keep_alive(true)
                .tools(interactive_keep_alive_tools()),
        )
        .await
    {
        Ok(child) => Effect::FocusNewSession {
            child,
            parent: focused,
            first_message,
        },
        Err(e) => {
            notice(state, e.to_string());
            Effect::None
        }
    }
}

/// The `/model`/`/role` execution path (INTENT.md §5c: "changing model
/// mid-session is ordinary, and stays cheap"). Forks a fresh, interactive
/// keep-alive child off `focused` with `spec` (already carrying a `.model`
/// or `.role` override plus `.keep_alive(true)`/`.tools(..)`, built by each
/// caller) and an EMPTY directive -- the identical idiom [`bare_fork`] uses,
/// so the child inherits `focused`'s entire context (§5c: "a selection
/// survives a model change; only the rendering ... does not") and idles
/// rather than running a placeholder turn against blank input.
///
/// **Why fork, not a live mutation of `focused`'s own task.** `focused`'s
/// running `AgentLoop` (`conway_runtime::agent_loop`) reads its role/pin
/// from its own `AgentSpec`, fixed for that task's entire lifetime -- there
/// is no message kind or shared cell that lets anything outside the task
/// change it, and `AgentTree::attach` refuses a second registration under
/// the same id besides, so "cancel and re-resume in place" is not available
/// either (see `conway::Conway::resume_with`'s own doc for the
/// process-restart path that IS this shape). Forking is the mechanism that
/// already reaches a LIVE session without any of that: it is a wholly
/// separate, already-running task from the moment it starts, so there is no
/// existing task to mutate at all.
///
/// A malformed `--model`-shaped value or an admission refusal
/// (`RoutingError::ContextTooLarge`, when the new pin/role's chain cannot
/// take the inherited context) is never swallowed here: the malformed-value
/// case fails before this function is even called (`execute`'s own `Model`
/// arm), and an admission refusal cannot happen at fork time at all -- the
/// child starts idle and only runs its router-facing admission check on its
/// first REAL turn, which surfaces through the ordinary event stream
/// `App::run` already renders, exactly like any other turn's refusal.
///
/// Pushes a `Notice` naming the switch and its target BEFORE returning
/// `Effect::FocusNewSession`, so the transcript records the switch itself
/// even though the routing decision it causes has not happened yet (that
/// arrives later, as this child's own `Event::ModelDecision`, which `/why`
/// then reports against the switch this notice already logged).
async fn switch_session<H: Host>(
    state: &mut AppState,
    host: &H,
    focused: AgentId,
    describe: impl Into<String>,
    spec: ForkSpec,
) -> Effect {
    match host.fork(focused, spec).await {
        Ok(child) => {
            notice(state, format!("{}: {focused} -> {child}", describe.into()));
            Effect::FocusNewSession {
                child,
                parent: focused,
                first_message: None,
            }
        }
        Err(e) => {
            notice(state, e.to_string());
            Effect::None
        }
    }
}

/// Parses `model` and forks the focused agent onto it via `switch_session`
/// -- the ONE place a chosen `"backend/model"` string becomes a live switch
/// (steering P-14), called both by [`execute`]'s `Model { model: Some(_) }`
/// arm (typed `/model <backend/model>`) and by `run.rs`'s
/// `Action::UiFormDecision` arm once the operator answers `/model` bare's
/// own menu (board item `01M1A35S609TZ613GAECPEHX8D`) -- a pair shown by
/// that menu (or by the bare listing's text form) is, by construction,
/// already a valid `ModelRef` string (every entry comes straight from a
/// configured role's own `chain`, `AppState::configured_models`'s own doc),
/// but this still parses it the same way a hand-typed value would rather
/// than trusting the caller -- the SAME function either path goes through
/// can never accept one shape's input more leniently than the other's.
pub async fn apply_model_switch<H: Host>(model: String, state: &mut AppState, host: &H) -> Effect {
    let focused = state.focused_agent;
    match model.parse::<ModelRef>() {
        Ok(model_ref) => {
            let spec = ForkSpec::new(String::new())
                .keep_alive(true)
                .tools(interactive_keep_alive_tools())
                .model(model_ref.clone());
            switch_session(
                state,
                host,
                focused,
                format!("switched model to {model_ref}"),
                spec,
            )
            .await
        }
        Err(e) => {
            notice(state, format!("/model {model}: {e}"));
            Effect::None
        }
    }
}

/// The bare/implicit `/spawn` execution path (WI "bare /spawn & /fork open an
/// interactive session"): a fresh, interactive KEEP-ALIVE session with an
/// EMPTY prompt (the child idles until `first_message`, if given, is
/// delivered separately by the app loop via `Effect::FocusNewSession`),
/// attached under `root` exactly as every spawn always has been. `agent_def`,
/// when `Some`, sets the child's def; otherwise the child inherits the
/// parent session's role/model. Factored out of `execute`'s `Spawn` arm for
/// the same reason [`bare_fork`] is -- the C2 fallback and the
/// `IntentChoice::Manual`/`Confirm` arms reuse it via synthetic
/// `SlashCommand::Spawn`s.
async fn bare_spawn<H: Host>(
    state: &mut AppState,
    host: &H,
    root: AgentId,
    agent_def: Option<String>,
    first_message: Option<String>,
) -> Effect {
    let mut spec = SpawnSpec::new("")
        .keep_alive(true)
        .tools(interactive_keep_alive_tools());
    if let Some(def) = &agent_def {
        spec = spec.agent_def(def.clone());
    }
    match host.spawn(root, spec).await {
        Ok(child) => Effect::FocusNewSession {
            child,
            parent: root,
            first_message,
        },
        Err(e) => {
            notice(state, e.to_string());
            Effect::None
        }
    }
}

/// Executes one NL intent confirmation choice (C2 -- the trust gate for
/// classified `/fork`/`/spawn` intent, which is untrusted and validated),
/// driven by `app.rs`'s `Action::IntentConfirm` arm. Reads the parked
/// [`IntentConfirm`] from `state.mode` (a no-op when no card is open -- a stale
/// choice after the card already closed cannot double-apply), then:
///
/// - `Confirm`: closes the card and runs the CLASSIFIED recipe directly via
///   `bare_fork`/`bare_spawn` (NOT by re-entering `execute` with a synthetic
///   `SlashCommand`, which would re-classify the free text and loop). The
///   recipe may have been cross-classified (user typed `/fork`, classifier
///   said `spawn`); `intent.agent_def` is honored only for `Spawn`
///   (`bare_fork` builds its `ForkSpec` with `agent_def` always unset --
///   see that function's own body -- so a classifier-returned `agent_def`
///   on a `Fork` recipe is ignored, matching `AgentIntent`'s own doc: the
///   def is the OPTIONAL garnish; the child still inherits whatever def the
///   focused agent was itself running under, via `SubagentHost::start`'s
///   own Fork-only fallback).
///   `intent.prompt` becomes the first message. Reuses the existing
///   `bare_fork`/`bare_spawn` execution path (the `Effect::FocusNewSession`
///   machinery) -- no new facade ops.
/// - `Edit`: the key handler has already called
///   `AppState::begin_intent_confirm_edit` to drop `intent.prompt` into
///   `state.input` and close the card; this arm is a no-op (the user edits
///   and submits normally).
/// - `Manual`: closes the card and runs the ORIGINAL command's
///   `default_recipe` directly via `bare_fork`/`bare_spawn` with the user's
///   raw text (untouched) as the first message -- today's pre-classification
///   flow, verbatim (no re-classify).
///
/// `Confirm` and `Manual` both call `bare_fork`/`bare_spawn` directly, so the
/// effect they return is that path's `Effect::FocusNewSession` on success or
/// `Effect::None` on a facade failure (the failure is already pushed as a
/// `Notice` by `bare_fork`/`bare_spawn`).
pub async fn execute_intent_confirm<H: Host>(
    choice: IntentChoice,
    state: &mut AppState,
    host: &H,
) -> Effect {
    let card = match &state.mode {
        Mode::IntentConfirm(card) => card.clone(),
        _ => return Effect::None,
    };
    match choice {
        IntentChoice::Confirm => {
            state.close_intent_confirm();
            // Run the CLASSIFIED recipe directly via `bare_fork`/`bare_spawn`
            // -- NOT by re-entering `execute` with a synthetic SlashCommand,
            // which would re-classify the free text and loop. The recipe may
            // have been cross-classified (user typed /fork, classifier said
            // spawn); `intent.agent_def` is honored only for `Spawn`
            // (`bare_fork`, below, builds its `ForkSpec` with `agent_def`
            // always unset; the child still inherits whatever def the
            // focused agent was itself running under, via `SubagentHost::
            // start`'s own Fork-only fallback). `intent.prompt` becomes the
            // first message.
            let focused = state.focused_agent;
            match card.intent.recipe {
                SubagentMode::Fork => {
                    bare_fork(state, host, focused, Some(card.intent.prompt.clone())).await
                }
                SubagentMode::Spawn => {
                    let root = host.root();
                    bare_spawn(
                        state,
                        host,
                        root,
                        card.intent.agent_def.clone(),
                        Some(card.intent.prompt.clone()),
                    )
                    .await
                }
            }
        }
        IntentChoice::Manual => {
            state.close_intent_confirm();
            // Fall back to today's pre-classification flow with the ORIGINAL
            // command's `default_recipe` and the user's raw text (untouched)
            // -- verbatim. Also via `bare_fork`/`bare_spawn` directly (no
            // re-classify).
            let focused = state.focused_agent;
            match card.default_recipe {
                SubagentMode::Fork => {
                    bare_fork(state, host, focused, Some(card.raw_text.clone())).await
                }
                SubagentMode::Spawn => {
                    let root = host.root();
                    bare_spawn(state, host, root, None, Some(card.raw_text.clone())).await
                }
            }
        }
        IntentChoice::Edit => {
            // The key handler already dropped `intent.prompt` into
            // `state.input` and closed the card via
            // `AppState::begin_intent_confirm_edit`. Nothing further for
            // the facade -- the user edits and submits normally.
            Effect::None
        }
    }
}

/// Executes one parsed command against `host`, mutating `state` directly
/// (transcript entries, and -- for `/resume` -- a full state reset) and
/// returning whatever [`Effect`] the caller's app loop must additionally
/// carry out. Every command maps to exactly one `host` call except `/why`
/// (reads `state.last_model_decision`, no facade call at all), `/tree`
/// (item A3: renders `state.tree` directly, no facade call), and `/help`
/// (T7: flips `AppState::help_open`, no facade call and no transcript
/// mutation at all).
///
/// Never panics and never propagates a facade error: a failing command
/// becomes a `Notice` entry with the error's `Display` (module notes: "A
/// failing slash command must never terminate the TUI").
pub async fn execute<H: Host>(cmd: SlashCommand, state: &mut AppState, host: &H) -> Effect {
    match cmd {
        SlashCommand::Steer { target, text } => {
            match resolve_agent(state, &target) {
                Ok(agent) => match host.steer(agent, text).await {
                    Ok(()) => notice(state, format!("steer queued for {agent}")),
                    Err(e) => notice(state, e.to_string()),
                },
                Err(e) => notice(state, e),
            }
            Effect::None
        }
        SlashCommand::Cancel { target, reason } => {
            match resolve_agent(state, &target) {
                Ok(agent) if agent == state.root_agent() => notice(
                    state,
                    format!(
                        "cannot cancel {agent}: it is this session's own root agent, and \
                         cancelling it would end the session -- use /quit instead"
                    ),
                ),
                Ok(agent) => {
                    let reason = reason.unwrap_or_else(|| "cancelled by operator".to_string());
                    match host.cancel(agent, reason.clone()).await {
                        Ok(()) => notice(state, format!("cancelled {agent}: {reason}")),
                        Err(e) => notice(state, e.to_string()),
                    }
                }
                Err(e) => notice(state, e),
            }
            Effect::None
        }
        SlashCommand::Tree => {
            // Item A3: no facade call -- the alias renders from
            // `state.tree` (the panel's own view), so its labels, recipe
            // parts, indent and status match what `/agents` shows. The
            // agent id does NOT: board item `01M0TNCAP1HH4YNC5K9753YG26`
            // decided the two deliberately differ there. See
            // `render_tree_snapshot`'s own doc for why.
            render_tree_snapshot(state);
            Effect::None
        }
        SlashCommand::Context { agent } => {
            // A bare `/context` defaults to the FOCUSED agent -- the same
            // concept `/agents` already tags `(focused)` and `/fork`'s own
            // bare form already resolves against. No `resolve_agent` call
            // needed for the default case: `state.focused_agent` is
            // already a live `AgentId`, not a token to parse.
            let resolved = match &agent {
                Some(token) => resolve_agent(state, token),
                None => Ok(state.focused_agent),
            };
            match resolved {
                Ok(agent_id) => match host.context_report(agent_id).await {
                    Ok(report) => {
                        render_instruction_preamble(&report, state);
                        render_context_report(&report, state);
                    }
                    Err(e) => notice(state, e.to_string()),
                },
                Err(e) => notice(state, e),
            }
            Effect::None
        }
        SlashCommand::Why => {
            render_why(state);
            Effect::None
        }
        SlashCommand::Fork { agent, directive } => match agent {
            // Explicit target (`/fork @<agent> <directive>`): the
            // pre-existing autonomous, non-keep-alive fork-of-a-named-agent
            // behavior, unchanged in substance -- `parse_fork` guarantees
            // `directive` is `Some` whenever `agent` is. Explicit `@<agent>`
            // syntax skips inference entirely (C2: only FREE TEXT is
            // classified; the user already named the target).
            Some(token) => {
                let directive_text = directive.unwrap_or_default();
                match resolve_agent(state, &token) {
                    Ok(agent_id) => {
                        match host.fork(agent_id, ForkSpec::new(directive_text)).await {
                            Ok(child) => notice(state, format!("forked {agent_id} -> {child}")),
                            Err(e) => notice(state, e.to_string()),
                        }
                    }
                    Err(e) => notice(state, e),
                }
                Effect::None
            }
            // Bare/implicit (`/fork`, `/fork <text>`): a fresh, interactive
            // keep-alive fork of the FOCUSED agent. C2: when free text
            // follows the command (`directive` is `Some`) AND it does not
            // start with explicit `@<agent>` syntax (already excluded by
            // the `Some(token)` arm above -- `parse_fork` only sets
            // `agent: Some(..)` for a leading `@`), the facade classifier
            // runs and a confirmation card opens on `Ok` (including the
            // verbatim passthrough -- the user confirms the raw text as
            // the prompt). A propagated `FacadeError::IntentClassification`
            // (a real backend failure, NOT the passthrough) falls back to
            // today's manual flow with a notice; the card must not appear
            // for a hard error. Bare `/fork` (no text) is unchanged: no
            // classify, no card, inherited model.
            None => {
                let focused = state.focused_agent;
                match directive {
                    Some(text) => match host
                        .classify_agent_intent(focused, SubagentMode::Fork, &text)
                        .await
                    {
                        Ok(intent) => {
                            state.offer_intent_confirm(IntentConfirm {
                                intent,
                                default_recipe: SubagentMode::Fork,
                                raw_text: text,
                                parent: focused,
                            });
                            Effect::None
                        }
                        Err(e) => {
                            notice(
                                state,
                                format!(
                                    "intent classification failed: {e}; \
                                     falling back to manual"
                                ),
                            );
                            bare_fork(state, host, focused, Some(text)).await
                        }
                    },
                    None => bare_fork(state, host, focused, None).await,
                }
            }
        },
        SlashCommand::Spawn { agent_def, prompt } => {
            // Always a fresh, interactive keep-alive session (this item):
            // empty prompt (the child idles until `prompt`, if given, is
            // delivered separately by the app loop -- see `Effect::
            // FocusNewSession`'s own doc), attached under `host.root()`
            // exactly as every spawn always has been (spawn never named a
            // "from" agent). C2: when free text follows the command
            // (`prompt` is `Some`) AND no explicit `@<agent_def>` was
            // named (`agent_def` is `None`), the facade classifier runs
            // and a confirmation card opens on `Ok` (including the
            // verbatim passthrough). Explicit `@<agent_def>` syntax and
            // bare `/spawn` are unchanged: no classify, no card.
            let root = host.root();
            match (agent_def, prompt) {
                (Some(def), prompt) => bare_spawn(state, host, root, Some(def), prompt).await,
                (None, Some(text)) => {
                    let focused = state.focused_agent;
                    match host
                        .classify_agent_intent(focused, SubagentMode::Spawn, &text)
                        .await
                    {
                        Ok(intent) => {
                            state.offer_intent_confirm(IntentConfirm {
                                intent,
                                default_recipe: SubagentMode::Spawn,
                                raw_text: text,
                                parent: focused,
                            });
                            Effect::None
                        }
                        Err(e) => {
                            notice(
                                state,
                                format!(
                                    "intent classification failed: {e}; \
                                     falling back to manual"
                                ),
                            );
                            bare_spawn(state, host, root, None, Some(text)).await
                        }
                    }
                }
                (None, None) => bare_spawn(state, host, root, None, None).await,
            }
        }
        SlashCommand::Resume { sid } => match sid.parse::<SessionId>() {
            Ok(id) => match host.resume(id).await {
                Ok(handle) => {
                    // Module notes: "replace the active handle, resubscribe
                    // events, reset AppState from handle.transcript(root)".
                    // The full LogRecord -> Entry backfill is left out here
                    // (disclosed): no LogRecord -> Entry mapping exists
                    // anywhere in this crate today, and no criterion of
                    // this item exercises it -- `conway::SessionHandle`'s
                    // own `record_to_event` doc names the analogous
                    // LogRecord -> Event gap as unresolved for the same
                    // reason (mismatched cardinality). `state` is reset to
                    // a clean `AppState` scoped to the new root instead, so
                    // resumed browsing starts from a known-empty transcript
                    // rather than a stale one from the old session.
                    //
                    // the installed
                    // plugin command list is process-lifetime configuration
                    // (which plugins were installed at startup), not
                    // session-scoped state -- `AppState::new` seeds it empty
                    // (every OTHER field reset here genuinely IS
                    // session-scoped), so it is carried across the reset by
                    // hand, the one field `/resume` intentionally does not
                    // clear.
                    // The installed agent-name store is carried across
                    // for the identical reason (board item
                    // `01M0TV5BSE98S16SFYECG9G9WP`): which plugins this
                    // process installed is startup configuration, not
                    // session state, so `/resume` must not silently strip
                    // `/steer <name>` of its ability to resolve. The NAMES
                    // themselves are per-agent and the resumed session has
                    // new agents, so nothing stale carries over -- only the
                    // store handle does.
                    //
                    // The plugin status-contribution snapshot is carried
                    // across for the SAME reason as its two siblings above
                    // (board item `01M0XDEDBR5YDF71Q7ZRXYMT85`, closing the
                    // third link in the chain those two items opened):
                    // `Conway::plugin_status_contributions()` is a
                    // `Conway`-level, build-time value -- exactly as
                    // process-lifetime as `plugin_commands`/`agent_names`,
                    // not session-scoped state -- so `AppState::new`'s empty
                    // default is the wrong value to leave it at here. This
                    // does NOT make the snapshot live: it is still the same
                    // frozen, typically-empty value `App::new` copied once
                    // at TUI startup (see `AppState::
                    // plugin_status_contributions`'s own doc for the
                    // caveat, restated rather than silently dropped by this
                    // carry-across).
                    let agent_names = state.agent_names.clone();
                    let plugin_commands = state.plugin_commands.clone();
                    let plugin_status_contributions = state.plugin_status_contributions.clone();
                    *state = AppState::new(handle.root());
                    state.plugin_commands = plugin_commands;
                    state.agent_names = agent_names;
                    state.plugin_status_contributions = plugin_status_contributions;
                    notice(state, format!("resumed session {sid}"));
                    Effect::Resumed(handle)
                }
                Err(e) => {
                    notice(state, e.to_string());
                    Effect::None
                }
            },
            Err(e) => {
                notice(state, format!("invalid session id `{sid}`: {e}"));
                Effect::None
            }
        },
        // `/model <backend/model>` -- see [`switch_session`]'s own doc for
        // the fork-based mechanism and why it, not a live mutation of the
        // focused agent's own running task, is what actually reaches a LIVE
        // session (INTENT.md §5c). The actual switch is factored into
        // [`apply_model_switch`] -- the SAME function `run.rs`'s
        // `Action::UiFormDecision` arm calls once the operator answers
        // `/model` bare's own menu (board item
        // `01M1A35S609TZ613GAECPEHX8D`), so there is exactly ONE place that
        // turns a chosen `"backend/model"` string into a live switch,
        // whichever surface produced it (steering P-14).
        SlashCommand::Model { model: Some(model) } => apply_model_switch(model, state, host).await,
        // Board item `01M1A35S609TZ613GAECPEHX8D`: bare `/model` lists the
        // configured `"backend/model"` pairs rather than erroring -- see
        // `AppState::configured_models`'s own doc for where the list comes
        // from (`[roles]`'s own chains, refreshed just before this call by
        // `App::submit`'s own `/model`-bare seam -- never a live provider
        // API call: "what is configured", not a remote roster).
        //
        // **With `conway.ui` installed, this is a MENU**, not a text dump --
        // `AskSelectRequest { prompt, options }` is exactly the
        // pick-one-from-a-list shape board item `01M19NH39AE2D5AMJK0RZRQY86`
        // already built for a model-called `ask_question` tool; this is
        // that mechanism's SECOND real consumer, reusing `Mode::UiForm`/
        // `draw_ui_form`/`handle_ui_form_key` exactly as they already are.
        // The one thing a model-raised question never needs and this one
        // does: the app itself (not a blocked tool call) is the asker, so
        // `state.model_picker_active` is set here to tell `run.rs`'s
        // `Action::UiFormDecision` arm to run [`apply_model_switch`] once
        // answered -- see that field's own doc.
        //
        // **Without `conway.ui`, this is plain transcript text** -- the
        // degrade path, not a fallback bolted on afterward: `conway.ui` is
        // opt-in and absent by default (`docs/plugins/trust-and-security.md`),
        // so the text listing is the MAIN path most sessions take, and the
        // menu is the enhancement, not the reverse.
        SlashCommand::Model { model: None } => {
            if state.configured_models.is_empty() {
                notice(
                    state,
                    "no models are configured -- add a provider and a role chain first \
                     (see /settings)"
                        .to_string(),
                );
                return Effect::None;
            }
            let ui_available = state
                .plugin_browser
                .iter()
                .any(|entry| entry.id == "conway.ui" && entry.installed);
            if ui_available {
                let ask = PendingFormAsk::new_local(conway_plugin_ui::AskSelectRequest {
                    prompt: "select a model".to_string(),
                    options: state.configured_models.clone(),
                });
                state.offer_ui_form(ask);
                state.model_picker_active = true;
            } else {
                let focused = state.focused_model.clone();
                let lines: Vec<String> = state
                    .configured_models
                    .iter()
                    .map(|m| {
                        if focused.as_deref() == Some(m.as_str()) {
                            format!("  {m}  (active)")
                        } else {
                            format!("  {m}")
                        }
                    })
                    .collect();
                notice(state, format!("configured models:\n{}", lines.join("\n")));
            }
            Effect::None
        }
        // `/role <alias>` -- same mechanism as `Model` above, naming a role
        // instead of pinning a model directly.
        SlashCommand::Role { role } => {
            let focused = state.focused_agent;
            let spec = ForkSpec::new(String::new())
                .keep_alive(true)
                .tools(interactive_keep_alive_tools())
                .role(RoleAlias::new(role.clone()));
            switch_session(
                state,
                host,
                focused,
                format!("switched role to {role}"),
                spec,
            )
            .await
        }
        // T7: `/help` opens the keybinding overlay (`view/help.rs`) instead
        // of dumping a command list into the transcript -- `AppState::open_help`
        // is a pure flag flip, pushing zero `Entry::Notice` lines.
        SlashCommand::Help => {
            state.open_help();
            Effect::None
        }
        // V4: `/settings` opens the settings menu -- a pure `AppState::
        // open_settings` flag flip, exactly like `/help` just above (no
        // facade call, no transcript mutation).
        SlashCommand::Settings => {
            state.open_settings();
            Effect::None
        }
        // Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`: bare `/plugin` opens the
        // plugin listing -- a pure `AppState::open_plugins` flag flip,
        // mirroring `Settings` immediately above exactly. Board item
        // `01M0WB5W5DX844HSJQG3JP23X0` adds the two `action` forms: neither
        // can run here (see `Effect::RunMarketplaceInstall`/`Effect::
        // RunMarketplaceUninstall`'s own doc for exactly why `execute`
        // hands both off rather than doing the work itself), so this arm's
        // only job for either is to translate a parsed `PluginsAction`
        // into its matching `Effect` -- no validation to perform here that
        // `parse` has not already done (both string fields are simply
        // forwarded verbatim; the real validation -- does the marketplace
        // have this id, is settings.json writable -- can only happen once
        // the fetch/write actually runs).
        SlashCommand::Plugins { action } => match action {
            None => {
                state.open_plugins();
                Effect::None
            }
            Some(PluginsAction::Install {
                marketplace_url,
                plugin_id,
            }) => Effect::RunMarketplaceInstall {
                marketplace_url,
                plugin_id,
            },
            Some(PluginsAction::Uninstall { plugin_id }) => {
                Effect::RunMarketplaceUninstall { plugin_id }
            }
        },
        // Formerly intercepted in `app.rs::submit` (board item
        // `01KZVZ5XV162XCQR96AQKCCCF7`) -- see this module's own doc for
        // why. `parse` already rejected any argument other than the bare
        // form or `permissions`, so the only remaining case here is
        // "nothing to trust".
        //
        // Board item (split from `01KZHVFCN6ZEAXV7K5JHRQN1YB`'s `(kind,
        // id, digest)`/plugin-subject generalisation, which this does not
        // pre-empt): this arm used to call `Host::trust_permission_file`
        // directly, installing and trusting in one action with nothing
        // shown first. It now calls the read-only `Host::
        // preview_trust_target` and opens the trust-preview card
        // (`state.offer_trust_preview`) instead -- the actual trust call
        // happens only after an explicit confirm, in
        // [`apply_trust_decision`] below, driven by `app.rs`'s
        // `Action::TrustDecision` arm exactly the way `Action::AskFate`
        // drives [`apply_ask_fate`].
        SlashCommand::Trust => {
            match state.permission_paths.first().cloned() {
                None => {
                    notice(state, "no project permissions file is configured to trust");
                }
                Some(path) => match host.preview_trust_target(&path).await {
                    Ok(preview) => {
                        state.offer_trust_preview(TrustPreviewCard {
                            path,
                            contents: preview.contents,
                            status: preview.status,
                            error: None,
                        });
                    }
                    Err(e) => {
                        // A read failure here is promoted to `Entry::Error`,
                        // same severity `apply_trust_decision`'s own confirm
                        // failure uses below -- `/trust permissions` is an
                        // explicit operator action, so a failure to even
                        // show what would be trusted must never surface as
                        // a routine notice.
                        state.transcript.push(Entry::Error {
                            text: format!("could not read {}: {e}", path.display()),
                            fatal: false,
                        });
                    }
                },
            }
            Effect::None
        }
        // Formerly intercepted in `app.rs::submit` -- see this module's own
        // doc. A pure `AppState` flip, no facade call, mirroring `/help`'s
        // own shape.
        SlashCommand::Agents => {
            state.toggle_agent_view();
            Effect::None
        }
        // B5, formerly intercepted in `app.rs::submit` -- see this module's
        // own doc and `Effect::RunModalAsk`'s. `parse` already guarantees
        // `question` is non-empty; the only validation left for `execute`
        // is the one check that needs live `AppState`: the single-question
        // modal is already occupied.
        SlashCommand::Ask { question } => {
            if state.ask_in_flight {
                // B5: one ask at a time, never a pile-up competing for the
                // one `Mode::AskModal` slot.
                notice(state, "an /ask is already running -- wait for its answer");
                Effect::None
            } else {
                state.ask_in_flight = true;
                // Board item `01M0RWFH6V709B7WTAFRZGFKG3`: stamped at
                // submit time, not once `AskUpdate::Started` reports the
                // forked child (a small delay later) -- the status line's
                // elapsed figure should read from the moment the operator
                // actually asked, and this is the earliest point `AppState`
                // is touched at all for this ask.
                state.ask_started_at = Some(std::time::Instant::now());
                Effect::RunModalAsk { question }
            }
        }
        SlashCommand::Quit => Effect::Quit,
        SlashCommand::Plugin { full_name, args } => match host.resolve_command(&full_name) {
            Some(command) => Effect::RunPluginCommand(PluginCommandInvocation {
                full_name,
                command,
                ctx: CommandCtx {
                    focused_agent: state.focused_agent,
                    root_agent: host.root(),
                    session_id: host.session_id(),
                    args,
                },
            }),
            None => {
                notice(
                    state,
                    format!("unknown command `/{full_name}` -- try /help"),
                );
                Effect::None
            }
        },
    }
}

fn notice(state: &mut AppState, text: impl Into<String>) {
    state.transcript.push(Entry::Notice { text: text.into() });
}

/// Runs one `/ask` modal fate (B5) against the facade: exactly one `host`
/// call per fate (`Fork` -> `Conway::promote`, `PullIn` ->
/// `Conway::pull_in`, `Discard` -> `Conway::purge`), driven by `app.rs`'s
/// `Action::AskFate` arm.
///
/// **Forced choice, so the ask's fate is always recorded:** a SUCCESS closes the modal
/// (`AppState::close_ask_modal`, which also promotes any permission prompt
/// queued behind it) and records the outcome as a `Notice`; a FAILURE
/// keeps the modal OPEN with the error shown in-modal
/// (`AppState::fail_ask_modal`) -- the user still must choose a fate, and
/// a failed fate never silently falls through to another one (e.g. a
/// refused pull-in is NOT implicitly converted into a discard).
///
/// A no-op when no modal is open (a stale fate key after the modal already
/// closed cannot double-apply a fate).
///
/// **A failed fate's FULL text is also recorded in the transcript** (board
/// item `01M0TYRPF1ASGQ77AK04RB7H84`), through the same
/// `Entry::Error { fatal: false }` channel `apply_trust_decision`'s own
/// registration-error path already uses: the in-modal footer
/// (`view/mod.rs::draw_ask_modal`) grows past its normal fixed height to
/// show an error, but stays capped, so a `RuntimeError::PullInIncomplete`
/// -- whose `Display` names exactly how many of how many records merged
/// and which child session still holds the ask, and can run past what even
/// a grown footer shows -- is never simply lost. `modal.error` itself
/// still gets the FULL, untruncated `Display` text (unchanged from before
/// this item): deciding how much of it actually fits on screen is the view
/// layer's job, not this dispatch's.
pub async fn apply_ask_fate<H: Host>(fate: AskFate, state: &mut AppState, host: &H) {
    let child = match &state.mode {
        Mode::AskModal(modal) => modal.child,
        _ => return,
    };
    let result = match fate {
        AskFate::Fork => host
            .promote(child)
            .await
            .map(|sid| format!("ask kept -- forked session {sid} is now persistent")),
        AskFate::PullIn => host
            .pull_in(child)
            .await
            .map(|()| "ask pulled into the parent session".to_string()),
        AskFate::Discard => host
            .purge(child)
            .await
            .map(|()| "ask discarded".to_string()),
    };
    match result {
        Ok(message) => {
            state.close_ask_modal();
            notice(state, message);
        }
        Err(e) => {
            let full = e.to_string();
            state.transcript.push(Entry::Error {
                text: format!("ask fate failed: {full}"),
                fatal: false,
            });
            state.fail_ask_modal(full);
        }
    }
}

/// Carries out the trust-preview card's decision (board item, split from
/// `01KZHVFCN6ZEAXV7K5JHRQN1YB`): a no-op `notice` for [`TrustDecision::
/// Cancel`] (there is nothing to undo -- nothing was ever written), or the
/// SAME `Host::trust_permission_file` call this arm used to make
/// immediately (before this item added the preview step) for
/// [`TrustDecision::Confirm`]. Driven by `app.rs`'s `Action::TrustDecision`
/// arm, mirroring [`apply_ask_fate`]'s own dispatch shape exactly: reads
/// the open card from `state.mode`, is a no-op if none is open (the same
/// defensive shape `apply_ask_fate` uses -- a stale action delivered after
/// the card already closed some other way must never panic or act on
/// nothing).
pub async fn apply_trust_decision<H: Host>(
    decision: TrustDecision,
    state: &mut AppState,
    host: &H,
) {
    let path = match &state.mode {
        Mode::TrustPreview(card) => card.path.clone(),
        _ => return,
    };
    match decision {
        TrustDecision::Cancel => {
            state.close_trust_preview();
            notice(state, format!("not trusted: {}", path.display()));
        }
        TrustDecision::Confirm => {
            let root_agent = state.root_agent();
            match host
                .trust_permission_file(&path, PermissionScope::Session, root_agent)
                .await
            {
                Ok(report) => {
                    state.close_trust_preview();
                    // B3: surface each registration error through the SAME
                    // `Entry::Error { fatal: false }` channel
                    // `load_permission_files`'s own `registration_errors`
                    // uses.
                    for err in report.registration_errors {
                        state.transcript.push(Entry::Error {
                            text: format!(
                                "permission rule not installed: {} -- {}",
                                err.rule.describe(),
                                err.reason.describe()
                            ),
                            fatal: false,
                        });
                    }
                    // A4: surface each partial-inertness notice through the
                    // SAME `Entry::Notice` channel `load_permission_files`'s
                    // own `notices` uses.
                    for msg in report.notices {
                        notice(state, msg);
                    }
                    notice(
                        state,
                        format!(
                            "trusted {} -- {} allow rule(s) installed for this \
                             session, and will load automatically until its \
                             content next changes",
                            path.display(),
                            report.installed
                        ),
                    );
                }
                Err(e) => {
                    // The card STAYS OPEN with the error shown -- mirroring
                    // `apply_ask_fate`'s own failure path -- rather than
                    // silently falling through to "cancelled": the
                    // operator's belief ("I just confirmed trusting this
                    // file") diverging from reality ("nothing was
                    // recorded") must never be camouflaged as a routine
                    // notice.
                    state.fail_trust_preview(format!("could not trust {}: {e}", path.display()));
                }
            }
        }
    }
}

/// Resolves `token` to a live agent id, in three passes: a full ULID is
/// accepted outright (no membership check -- the facade call itself rejects
/// an agent outside this session); else an exact operator-chosen NAME, if
/// `conway.names` is installed; else `token` as a unique prefix against
/// `state.tree`'s known agent ids (module notes: "an ambiguous prefix is a
/// `ParseError` listing the candidates").
///
/// **The name pass is entirely new (board item
/// `01M0TV5BSE98S16SFYECG9G9WP`), and it is what makes `/steer scout ...`
/// work.** Every agent-targeted command already routes through this one
/// function, so accepting a name here is the whole of the payoff and costs
/// no new verbs. With `conway.names` uninstalled,
/// [`AppState::agent_names`] is `None`, this pass matches nothing, and the
/// two surrounding passes behave exactly as they did before this item.
///
/// **Ordering, decided: exact id, then exact name, then prefix.** Both of
/// the first two passes are EXACT matches on an identifier the operator
/// typed in full -- one canonical, one chosen -- while the third is an
/// approximate match on an abbreviation. Putting the name pass ahead of the
/// prefix pass means a name can never be shadowed by an accidental
/// prefix collision with some other agent's id, which is the genuinely
/// confusing case; putting it AFTER the full-id parse means this function's
/// oldest, most load-bearing behaviour is untouched (and
/// `conway_plugin_names::validate_name` refuses to store a name that is
/// itself a valid ULID, so nothing can hide behind that branch anyway).
///
/// **A name resolves only against agents in THIS session's tree**, the
/// same set the prefix pass ranges over. The store is flat and global
/// across projects (`conway_plugin_names`'s own module doc), so a name
/// belonging to an agent from another session must fall through to "no
/// agent matches" rather than resolve to something the operator cannot see.
///
/// **Two agents sharing a name is an ambiguity, not a refusal.** The store
/// deliberately allows duplicates (it cannot know which agents are on
/// screen), so this function reports them the SAME way it already reports
/// an ambiguous prefix -- one message shape, listing every candidate --
/// rather than inventing a second failure mode.
fn resolve_agent(state: &AppState, token: &str) -> Result<AgentId, String> {
    if let Ok(id) = token.parse::<AgentId>() {
        return Ok(id);
    }
    if let Some(names) = &state.agent_names {
        let named: Vec<AgentId> = state
            .tree
            .nodes
            .iter()
            .map(|n| n.agent_id)
            .filter(|id| names.get(id).as_deref() == Some(token))
            .collect();
        match named.as_slice() {
            // Not a name in this tree -- fall through to the prefix pass,
            // which owns the "nothing matched" message for both.
            [] => {}
            [id] => return Ok(*id),
            _ => {
                return Err(format!(
                    "ambiguous agent name `{token}`; candidates: {}",
                    named
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        }
    }
    let matches: Vec<AgentId> = state
        .tree
        .nodes
        .iter()
        .map(|n| n.agent_id)
        .filter(|id| id.to_string().starts_with(token))
        .collect();
    match matches.as_slice() {
        [] => Err(format!("no agent matches `{token}`")),
        [id] => Ok(*id),
        _ => Err(format!(
            "ambiguous agent prefix `{token}`; candidates: {}",
            matches
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Renders `state.tree` (the `/agents` panel's own `AgentTreeView`) into the
/// transcript as one `Notice` line per agent. Item A3: `/tree` is an
/// alias for the panel, so its text derives from the SAME nodes and
/// recipe labels (`view::agents::recipe_parts`) the panel draws -- never
/// from the runtime's `AgentTreeSnapshot`, so `execute` makes no facade
/// call for it at all. `TreeNode` carries no `steps`/`budget`/`role`, so a
/// line is exactly what the panel row shows (indent, label, recipe parts,
/// status).
///
/// **The agent id is deliberately NOT what the panel shows** (board item
/// `01M0TNCAP1HH4YNC5K9753YG26`, which found the two had drifted and this
/// doc comment was claiming a parity that no longer held). The panel row
/// prints [`super::view::agents::panel_agent_id`]'s screen-relative short
/// id -- an affordance sized to what is on screen right now. This line
/// prints `node.agent_id`'s full 26-character ULID via `Display`, because a
/// `/tree` line is transcript text: it can be scrolled back to, pasted into
/// a script, a commit message, or a bug report, arbitrarily far from the
/// moment it was printed and the row set that made a short prefix unique
/// then. A durable reference needs no "unique among what was on screen"
/// caveat; the full id is that reference, and `resolve_agent` accepts it
/// unchanged. Both forms resolve to the same agent either way -- this is a
/// choice between two valid identifiers, not a bug in either one.
///
/// Unlike the panel, the snapshot deliberately does NOT honor the
/// `AgentVisibility` filter: it shows ALL nodes, terminal ones included.
/// A transcript dump is a provenance/auditing artifact -- hiding
/// finished agents here would silently drop rows a copied transcript is
/// expected to keep.
///
/// Lines are composed in one immutable pass over `state.tree` and only then
/// pushed as notices: `notice` needs `&mut state`, so the depth walk
/// (`view::agents::ancestor_depth`, the panel's own helper, borrowed
/// immutably) cannot run interleaved with it.
fn render_tree_snapshot(state: &mut AppState) {
    let lines: Vec<String> = state
        .tree
        .nodes
        .iter()
        .map(|node| {
            let indent = "  ".repeat(super::view::agents::ancestor_depth(state, node.agent_id));
            let label = node
                .agent_def
                .clone()
                .unwrap_or_else(|| "agent".to_string());
            let parts = super::view::agents::recipe_parts(node);
            let recipe = if parts.is_empty() {
                String::new()
            } else {
                format!(" {}", parts.join(" "))
            };
            format!(
                "{indent}{} {label}{recipe} [{:?}]",
                node.agent_id, node.status
            )
        })
        .collect();
    for line in lines {
        notice(state, line);
    }
}

/// The preamble section (board item `01M0K5MD59YZRSHE31JKZKFRMY`):
/// renders `report.instruction_fragments` -- every plugin-declared
/// instruction fragment this turn's assembly considered, WITH the
/// (plugin_id, name) source attribution `Provenance::Skill` alone cannot
/// carry (see `ContextReport::instruction_fragments`'s own doc). Called
/// BEFORE [`render_context_report`] -- "instruction is the top of your
/// context" (decision `01M0K5K8DCRVR523P54DZF4BY3`) -- and only when
/// non-empty, so a session with no instruction-declaring plugin installed
/// renders byte-identically to before this item (the per-segment listing
/// below already shows the base idiom and any directory-authored skill by
/// name, via `Provenance::AgentDef`/`Provenance::Skill`; this section adds
/// only what that listing cannot: which PLUGIN a fragment came from, and
/// whether it was withheld).
///
/// **Not the full illustrative header/table `/context`'s board item
/// sketches** (`context · @root · claude-opus-5 · 11.8k / 200k`, a boxed
/// `path` section, etc.) -- that is a broader `/context` rendering
/// redesign the item's own text rules out ("`/context <agent>` already
/// exists -- do not build a new viewer"). This renders the SAME one-
/// notice-per-line shape [`render_context_report`] already uses, adding
/// the source/reachability columns that shape can express.
fn render_instruction_preamble(report: &ContextReport, state: &mut AppState) {
    if report.instruction_fragments.is_empty() {
        return;
    }
    let total_tokens: u32 = report
        .instruction_fragments
        .iter()
        .map(|f| f.tokens_est)
        .sum();
    notice(
        state,
        format!(
            "preamble: {} plugin-declared fragment{} · {total_tokens}tok",
            report.instruction_fragments.len(),
            if report.instruction_fragments.len() == 1 {
                ""
            } else {
                "s"
            },
        ),
    );
    for fragment in &report.instruction_fragments {
        if fragment.unreachable_tool_ids.is_empty() {
            notice(
                state,
                format!(
                    "  {}.{}  {}tok  <- {}",
                    fragment.plugin_id, fragment.name, fragment.tokens_est, fragment.plugin_id
                ),
            );
        } else {
            let missing = fragment
                .unreachable_tool_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            notice(
                state,
                format!(
                    "  {}.{}  {}tok  \u{26a0} names {missing} -- not installed",
                    fragment.plugin_id, fragment.name, fragment.tokens_est
                ),
            );
        }
    }
}

fn render_context_report(report: &ContextReport, state: &mut AppState) {
    if report.segments.is_empty() && report.dropped.is_empty() {
        notice(state, "empty context");
        return;
    }
    for entry in &report.segments {
        notice(
            state,
            format!(
                "{} {} {}tok",
                entry.segment,
                provenance_label(&entry.provenance),
                entry.tokens_est
            ),
        );
    }
    // Reported last and only when non-empty, so an ordinary turn's output is
    // byte-for-byte what it was. This is the one thing in the report that
    // describes what ISN'T in the context, which is exactly why it would
    // otherwise be unanswerable: the blocks are gone from `segments` by the
    // time anything renders them.
    if !report.dropped.is_empty() {
        notice(
            state,
            format!(
                "dropped {} unanswered tool call{}: {} \
                 (no result was recorded, so the request could not carry them; \
                 the model may re-issue them)",
                report.dropped.len(),
                if report.dropped.len() == 1 { "" } else { "s" },
                report.dropped.join(", "),
            ),
        );
    }
}

fn provenance_label(p: &Provenance) -> String {
    match p {
        Provenance::UserPrompt => "user prompt".to_string(),
        Provenance::AgentDef { name } => format!("agent def `{name}`"),
        Provenance::Skill { name } => format!("skill `{name}`"),
        Provenance::ToolRegistry { hash } => format!("tool registry {hash}"),
        Provenance::Inherited { from, seq_range } => {
            format!("inherited from {from} ({seq_range:?})")
        }
        Provenance::ForkDirective { by } => format!("fork directive by {by}"),
        Provenance::ParentSteer { from, parent_seq } => {
            format!("parent steer from {from} @{parent_seq:?}")
        }
        Provenance::ToolResult { call_id, tool } => format!("tool result {tool} ({call_id})"),
        Provenance::SystemNote { reason } => format!("system note: {reason}"),
        Provenance::MergedAsk { from } => format!("merged /ask from {from}"),
        Provenance::ChildResult { from } => format!("child result from {from}"),
        Provenance::CommandPrompt { command } => format!("submitted by /{command}"),
        _ => "unknown provenance".to_string(),
    }
}

/// `/why`: renders `state.last_model_decision` (populated by `app.rs` on
/// `Event::ModelDecision` -- this module never writes it). No facade call
/// at all (module notes: "reads cached state with no facade call").
///
/// **Shows what changed** (this item, INTENT.md §5c: "changing model
/// mid-session is ordinary"): when `state.previous_model_decision` is also
/// `Some` (i.e. this is at least the second routing decision this session
/// has seen -- ordinarily the child's first turn after a `/model`/`/role`
/// switch), a changed `role`/`chosen` is rendered as `X -> Y` instead of
/// bare `Y`, naming exactly what a `/model`/`/role` switch (or, equally, an
/// ordinary fallback the router itself chose) actually changed. A field
/// that did NOT change renders bare, unchanged from before this item --
/// there is nothing to contrast it against.
fn render_why(state: &mut AppState) {
    let Some(env) = state.last_model_decision.clone() else {
        notice(state, "no routing decision yet");
        return;
    };
    let Event::ModelDecision {
        role,
        chosen,
        reason,
        attempt,
    } = env.event
    else {
        // `last_model_decision` is only ever assigned an `Event::ModelDecision`
        // envelope (app.rs's own invariant) -- this arm exists so a future
        // widening of that invariant degrades to the same "nothing to show
        // yet" message rather than panicking.
        notice(state, "no routing decision yet");
        return;
    };
    // `previous_model_decision` is only ever assigned an `Event::
    // ModelDecision` envelope too (the SAME invariant as `last_model_
    // decision`, `app.rs`'s run loop) -- a mismatched shape there degrades
    // to "no previous decision to compare against" (`None`), same as a
    // genuinely absent one, rather than panicking.
    let previous = state.previous_model_decision.clone().and_then(|env| {
        if let Event::ModelDecision {
            role: prev_role,
            chosen: prev_chosen,
            ..
        } = env.event
        {
            Some((prev_role, prev_chosen))
        } else {
            None
        }
    });
    let role_text = match &previous {
        Some((prev_role, _)) if *prev_role != role => format!("{prev_role} -> {role}"),
        _ => role.to_string(),
    };
    let model_text = match &previous {
        Some((_, prev_chosen)) if *prev_chosen != chosen => format!("{prev_chosen} -> {chosen}"),
        _ => chosen.to_string(),
    };
    notice(state, format!("role: {role_text}"));
    notice(state, format!("model: {model_text}"));
    notice(state, format!("reason: {}", render_routing_reason(&reason)));
    notice(state, format!("attempt: {attempt}"));
}

fn render_routing_reason(reason: &RoutingReason) -> String {
    match reason {
        RoutingReason::PinnedByApi => "pinned by API".to_string(),
        RoutingReason::PinnedByAgentDef => "pinned by agent definition".to_string(),
        RoutingReason::AliasPrimary { alias } => format!("primary for role `{alias}`"),
        RoutingReason::Fallback { position, after } => format!(
            "fallback #{position} after: {}",
            after
                .iter()
                .map(|f| format!("{} ({})", f.model, f.error))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        RoutingReason::CapabilitySkip { skipped, missing } => {
            format!("skipped `{skipped}`: missing {}", missing.join(", "))
        }
        RoutingReason::HealthSkip { skipped, breaker } => {
            format!("skipped `{skipped}`: {breaker:?} breaker open")
        }
        _ => "unknown routing reason".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use conway::plugin::{CommandOutcome, CommandSpec};
    use conway::{AgentId, FacadeError, SessionId, SubagentMode};
    // Test-only: `ContextReportEntry`/`SegmentId` are not part of this
    // crate's curated `conway` re-export list -- constructing
    // `ContextReport` fixtures reaches into `conway-core` directly, exactly
    // as `exit.rs`/`oneshot.rs`/`render/*.rs`'s existing tests already do
    // (see this crate's `Cargo.toml` `[dev-dependencies]` comment).
    use conway_core::ids::{SegmentId, ToolName};
    use conway_core::provenance::{ContextReportEntry, InstructionFragmentEntry};

    use super::*;
    use crate::tui::state::{NodeStatus, PluginBrowserEntry, TreeNode};

    /// Wide enough that a rendered status line's `focused: <ulid>` suffix
    /// (a 26-char ULID, after every other status segment) is never itself
    /// clipped by the terminal width -- see the render/state tests near the
    /// bottom of this module.
    const RENDER_WIDTH: u16 = 200;

    // ---------------------------------------------------------------
    // parse()
    // ---------------------------------------------------------------

    #[test]
    fn steer_parses_and_preserves_internal_whitespace() {
        assert_eq!(
            parse("/steer a7 hold on"),
            Ok(SlashCommand::Steer {
                target: "a7".to_string(),
                text: "hold on".to_string(),
            })
        );
    }

    #[test]
    fn steer_missing_text_is_a_parse_error_naming_the_form() {
        let err = parse("/steer a7").unwrap_err();
        assert!(err.to_string().contains("/steer <agent> <text>"));
    }

    #[test]
    fn cancel_parses_agent_with_no_reason() {
        assert_eq!(
            parse("/cancel a7"),
            Ok(SlashCommand::Cancel {
                target: "a7".to_string(),
                reason: None,
            })
        );
    }

    #[test]
    fn cancel_parses_agent_and_preserves_internal_whitespace_in_the_reason() {
        assert_eq!(
            parse("/cancel a7 burning tokens, stop it"),
            Ok(SlashCommand::Cancel {
                target: "a7".to_string(),
                reason: Some("burning tokens, stop it".to_string()),
            })
        );
    }

    #[test]
    fn cancel_missing_agent_is_a_parse_error_naming_the_form() {
        let err = parse("/cancel").unwrap_err();
        assert!(err.to_string().contains("/cancel <agent> [<reason>]"));
    }

    #[test]
    fn tree_parses() {
        assert_eq!(parse("/tree"), Ok(SlashCommand::Tree));
    }

    #[test]
    fn tree_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/tree foo").unwrap_err();
        assert!(err.to_string().contains("/tree"));
    }

    #[test]
    fn context_parses() {
        assert_eq!(
            parse("/context a7"),
            Ok(SlashCommand::Context {
                agent: Some("a7".to_string()),
            })
        );
    }

    // Board item `01M0RWKJD04JBR5NCVKBQXYHV4`, acceptance 1: a bare
    // `/context` is no longer a usage error -- REPLACES
    // `context_missing_agent_is_a_parse_error_naming_the_form`, which used
    // to pin exactly the opposite of this. Deliberate, not a quiet
    // deletion: the operator's own report was that requiring an argument
    // nobody could discover was the defect, and `execute` now resolves the
    // bare form against `AppState::focused_agent` (see the execute-level
    // test below), mirroring `/fork`'s pre-existing bare-defaults-to-
    // focused convention.
    #[test]
    fn context_with_no_argument_parses_to_no_agent_not_an_error() {
        assert_eq!(parse("/context"), Ok(SlashCommand::Context { agent: None }));
    }

    #[test]
    fn why_parses() {
        assert_eq!(parse("/why"), Ok(SlashCommand::Why));
    }

    #[test]
    fn why_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/why now").unwrap_err();
        assert!(err.to_string().contains("/why"));
    }

    #[test]
    fn fork_at_agent_splits_agent_and_directive() {
        // Explicit target via `@` (this item's generalization of the old
        // `/fork <agent> <directive>` two-arg form).
        assert_eq!(
            parse("/fork @a7 review the diff"),
            Ok(SlashCommand::Fork {
                agent: Some("a7".to_string()),
                directive: Some("review the diff".to_string()),
            })
        );
    }

    #[test]
    fn fork_at_agent_missing_directive_is_a_parse_error_naming_the_form() {
        let err = parse("/fork @a7").unwrap_err();
        assert!(err.to_string().contains("/fork"));
    }

    #[test]
    fn bare_fork_parses_with_no_agent_and_no_directive() {
        // Bare `/fork` (this item): a fresh, interactive keep-alive fork of
        // the FOCUSED agent, idling until prompted.
        assert_eq!(
            parse("/fork"),
            Ok(SlashCommand::Fork {
                agent: None,
                directive: None,
            })
        );
    }

    #[test]
    fn fork_with_text_and_no_at_sigil_is_a_bare_fork_with_a_first_message() {
        // No `@` sigil -- the entire remainder (however many words) is the
        // interactive child's first message, not an explicit target.
        assert_eq!(
            parse("/fork please review this"),
            Ok(SlashCommand::Fork {
                agent: None,
                directive: Some("please review this".to_string()),
            })
        );
    }

    #[test]
    fn spawn_with_no_agent_def_treats_the_whole_remainder_as_the_prompt() {
        // No `@<agent_def>` token -- the entire remainder is the prompt and
        // `agent_def` is `None` (the spawned child inherits the parent's
        // role/model).
        assert_eq!(
            parse("/spawn reviewer review the diff"),
            Ok(SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("reviewer review the diff".to_string()),
            })
        );
    }

    #[test]
    fn spawn_with_at_agent_def_splits_agent_def_and_prompt() {
        assert_eq!(
            parse("/spawn @reviewer review the diff"),
            Ok(SlashCommand::Spawn {
                agent_def: Some("reviewer".to_string()),
                prompt: Some("review the diff".to_string()),
            })
        );
    }

    #[test]
    fn bare_spawn_parses_with_no_agent_def_and_no_prompt() {
        // Bare `/spawn` (this item): a fresh, interactive keep-alive spawn,
        // idling until prompted -- no longer a parse error.
        assert_eq!(
            parse("/spawn"),
            Ok(SlashCommand::Spawn {
                agent_def: None,
                prompt: None,
            })
        );
    }

    #[test]
    fn spawn_at_agent_def_with_no_prompt_parses_with_prompt_none() {
        // `/spawn @<agent_def>` (this item): names an agent_def with no
        // first message -- no longer a parse error.
        assert_eq!(
            parse("/spawn @reviewer"),
            Ok(SlashCommand::Spawn {
                agent_def: Some("reviewer".to_string()),
                prompt: None,
            })
        );
    }

    #[test]
    fn spawn_double_at_escapes_a_literal_at_prompt_with_no_agent_def() {
        // `@@` is the escape hatch for a prompt that must begin with `@`;
        // without it, `@channel ...` would be mis-split into an agent_def.
        assert_eq!(
            parse("/spawn @@channel please refactor the parser"),
            Ok(SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("@channel please refactor the parser".to_string()),
            })
        );
    }

    #[test]
    fn resume_parses() {
        assert_eq!(
            parse("/resume 01ARZ3NDEKTSV4RRFFQ69G5FAV"),
            Ok(SlashCommand::Resume {
                sid: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            })
        );
    }

    #[test]
    fn resume_missing_sid_is_a_parse_error_naming_the_form() {
        let err = parse("/resume").unwrap_err();
        assert!(err.to_string().contains("/resume <session-id>"));
    }

    #[test]
    fn model_parses() {
        assert_eq!(
            parse("/model anthropic/claude-sonnet-4-6"),
            Ok(SlashCommand::Model {
                model: Some("anthropic/claude-sonnet-4-6".to_string()),
            })
        );
    }

    /// **VERIFICATION ANCHOR, board item `01M1A35S609TZ613GAECPEHX8D`
    /// acceptance 4.** Bare `/model` used to be a `ParseError` naming the
    /// form -- it is now a valid parse carrying `model: None`, which
    /// `execute`'s own `Model { model: None }` arm turns into a listing
    /// rather than an error.
    #[test]
    fn model_with_no_argument_parses_as_a_bare_listing_request() {
        assert_eq!(parse("/model"), Ok(SlashCommand::Model { model: None }));
        // Bare whitespace after the command name is the same as none at all.
        assert_eq!(parse("/model   "), Ok(SlashCommand::Model { model: None }));
    }

    #[test]
    fn role_parses() {
        assert_eq!(
            parse("/role planner"),
            Ok(SlashCommand::Role {
                role: "planner".to_string(),
            })
        );
    }

    #[test]
    fn role_missing_value_is_a_parse_error_naming_the_form() {
        let err = parse("/role").unwrap_err();
        assert!(err.to_string().contains("/role <alias>"));
    }

    #[test]
    fn help_parses() {
        assert_eq!(parse("/help"), Ok(SlashCommand::Help));
    }

    #[test]
    fn help_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/help me").unwrap_err();
        assert!(err.to_string().contains("/help"));
    }

    #[test]
    fn settings_parses() {
        assert_eq!(parse("/settings"), Ok(SlashCommand::Settings));
    }

    #[test]
    fn settings_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/settings all").unwrap_err();
        assert!(err.to_string().contains("/settings"));
    }

    /// Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`.
    #[test]
    fn plugin_parses() {
        assert_eq!(parse("/plugin"), Ok(SlashCommand::Plugins { action: None }));
    }

    #[test]
    fn plugin_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/plugin all").unwrap_err();
        assert!(err.to_string().contains("/plugin"));
    }

    /// Board item `01M0WB5W5DX844HSJQG3JP23X0`: the install trigger this
    /// item adds. Both fields are forwarded verbatim, no re-tokenization
    /// (module notes' own rule) -- `plugin_id` here is deliberately the
    /// remainder after the URL, unsplit further.
    #[test]
    fn plugin_install_parses() {
        assert_eq!(
            parse("/plugin install https://example.com/marketplace.json acme-tools"),
            Ok(SlashCommand::Plugins {
                action: Some(PluginsAction::Install {
                    marketplace_url: "https://example.com/marketplace.json".to_string(),
                    plugin_id: "acme-tools".to_string(),
                }),
            })
        );
    }

    #[test]
    fn plugin_install_missing_plugin_id_is_a_parse_error_naming_the_form() {
        let err = parse("/plugin install https://example.com/marketplace.json").unwrap_err();
        assert!(err.to_string().contains("/plugin"));
        assert!(err.to_string().contains("install"));
    }

    #[test]
    fn plugin_uninstall_parses() {
        assert_eq!(
            parse("/plugin uninstall acme-tools"),
            Ok(SlashCommand::Plugins {
                action: Some(PluginsAction::Uninstall {
                    plugin_id: "acme-tools".to_string(),
                }),
            })
        );
    }

    #[test]
    fn plugin_uninstall_missing_plugin_id_is_a_parse_error_naming_the_form() {
        let err = parse("/plugin uninstall").unwrap_err();
        assert!(err.to_string().contains("/plugin"));
        assert!(err.to_string().contains("uninstall"));
    }

    /// An unrecognized action word (neither `install` nor `uninstall`) is a
    /// `ParseError`, not a silently-accepted third form -- shown to fail
    /// first (P-15): before this item's `parse_plugins_action` existed,
    /// `/plugin all` already failed this way via the old `parse_no_arg`
    /// path, so this and `plugin_with_trailing_argument_is_a_parse_error_
    /// naming_the_form` above both cover the identical case through the
    /// NEW parser, proving it did not regress.
    #[test]
    fn plugin_unknown_action_is_a_parse_error_naming_the_form() {
        let err = parse("/plugin browse").unwrap_err();
        assert!(err.to_string().contains("/plugin"));
    }

    // ---------------------------------------------------------------
    // /trust, /agents, /ask (board item `01KZVZ5XV162XCQR96AQKCCCF7`):
    // formerly intercepted in `app.rs::submit` by direct string comparison,
    // now parsed here like any other command -- see this module's own doc.
    // ---------------------------------------------------------------

    #[test]
    fn bare_trust_parses() {
        assert_eq!(parse("/trust"), Ok(SlashCommand::Trust));
    }

    #[test]
    fn trust_permissions_parses() {
        assert_eq!(parse("/trust permissions"), Ok(SlashCommand::Trust));
    }

    /// The exact wording the OLD `app.rs::submit` interception pushed as an
    /// `Entry::Notice` by hand -- now a `ParseError` whose `Display`
    /// produces the identical text, so `submit`'s shared `Err(e)` arm
    /// reproduces it with no special case.
    #[test]
    fn trust_with_an_unrecognized_argument_is_a_parse_error_naming_the_usage() {
        let err = parse("/trust nope").unwrap_err();
        assert_eq!(err.to_string(), "usage: /trust permissions");
    }

    #[test]
    fn agents_parses() {
        assert_eq!(parse("/agents"), Ok(SlashCommand::Agents));
    }

    /// The exact wording the OLD interception pushed by hand.
    #[test]
    fn agents_with_an_argument_is_a_parse_error_naming_the_usage() {
        let err = parse("/agents foo").unwrap_err();
        assert_eq!(err.to_string(), "usage: /agents (no arguments)");
    }

    #[test]
    fn ask_parses_with_the_question_as_a_single_field() {
        assert_eq!(
            parse("/ask is this safe to merge?"),
            Ok(SlashCommand::Ask {
                question: "is this safe to merge?".to_string(),
            })
        );
    }

    /// The exact wording the OLD interception pushed by hand.
    #[test]
    fn ask_with_no_question_is_a_parse_error_naming_the_usage() {
        let err = parse("/ask").unwrap_err();
        assert_eq!(err.to_string(), "usage: /ask <text>");
    }

    /// V4 acceptance: `/thinking` and `/timestamps` no longer parse at all
    /// -- they are REMOVED, not aliased to `/settings`, and both were never
    /// reachable through this parser in the first place (they used to be
    /// intercepted in `app.rs::submit`, now deleted -- see this module's
    /// own doc). Locks the removal down as an ordinary "unknown command".
    #[test]
    fn thinking_and_timestamps_no_longer_parse() {
        let thinking_err = parse("/thinking").unwrap_err();
        assert!(thinking_err.to_string().contains("unknown command"));
        let timestamps_err = parse("/timestamps").unwrap_err();
        assert!(timestamps_err.to_string().contains("unknown command"));
    }

    #[test]
    fn quit_parses() {
        assert_eq!(parse("/quit"), Ok(SlashCommand::Quit));
    }

    #[test]
    fn quit_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/quit now").unwrap_err();
        assert!(err.to_string().contains("/quit"));
    }

    #[test]
    fn exit_parses_as_an_alias_for_quit() {
        assert_eq!(parse("/exit"), Ok(SlashCommand::Quit));
    }

    #[test]
    fn exit_with_trailing_argument_is_a_parse_error_naming_the_form() {
        let err = parse("/exit now").unwrap_err();
        assert!(err.to_string().contains("/exit"));
    }

    #[test]
    fn bareword_exit_and_quit_do_not_parse_as_slash_commands() {
        // No leading `/` -- these must stay normal prompts sent to the
        // model, never intercepted as a slash command.
        assert!(parse("exit").is_err());
        assert!(parse("quit").is_err());
    }

    #[test]
    fn unknown_command_is_a_parse_error() {
        let err = parse("/nope").unwrap_err();
        assert!(err.to_string().contains("/nope"));
    }

    // ---------------------------------------------------------------
    // describe() / builtin_commands() (board item
    // `01M0RW29F2ATVGCV0R8H0GQEYH`): the palette's single source of truth.
    // ---------------------------------------------------------------

    /// The verification anchor's own measured drift, proven fixed: `/trust`
    /// and `/tree` -- both real, working commands -- were absent from the
    /// old hand-kept `view::palette::COMMANDS` table. They must now appear,
    /// generated automatically rather than by someone remembering to add a
    /// row.
    #[test]
    fn trust_and_tree_are_discoverable_in_builtin_commands() {
        let names: Vec<&str> = builtin_commands().iter().map(|c| c.name).collect();
        assert!(
            names.contains(&"/trust"),
            "/trust must be discoverable: {names:?}"
        );
        assert!(
            names.contains(&"/tree"),
            "/tree must be discoverable: {names:?}"
        );
    }

    /// `/ask` and `/agents` (the two the operator named explicitly) must
    /// still be listed -- and, per this item's own finding, the mechanism
    /// by which they are listed is no longer a special case at all: both
    /// are ordinary `SlashCommand` variants reached through the identical
    /// `describe` match every other built-in goes through (see this
    /// module's own top-of-file doc on `/settings`, `/trust`, `/agents`,
    /// `/ask` "are ordinary commands now" -- board item
    /// `01KZVZ5XV162XCQR96AQKCCCF7` already closed the pre-parser
    /// interception this item's spec describes as the reason they needed
    /// separate handling). There is no "handled elsewhere" case left to
    /// model for them.
    #[test]
    fn ask_and_agents_are_still_listed_as_ordinary_described_variants() {
        let names: Vec<&str> = builtin_commands().iter().map(|c| c.name).collect();
        assert!(names.contains(&"/ask"));
        assert!(names.contains(&"/agents"));
        // Both reach `describe` through the SAME exhaustive match as
        // `/steer`/`/trust`/every other built-in -- proven directly by
        // constructing each and calling `describe`, exactly like any other
        // variant, with no special-cased branch anywhere in this test.
        assert_eq!(
            describe(&SlashCommand::Ask {
                question: String::new()
            })
            .name,
            "/ask"
        );
        assert_eq!(describe(&SlashCommand::Agents).name, "/agents");
    }

    /// `describe` refuses to describe a plugin command -- plugin commands
    /// are resolved dynamically at TUI startup (`CommandRegistry::
    /// palette_entries`), never through this static table; this is the
    /// direct proof that misuse is loud, not a silently wrong description.
    #[test]
    #[should_panic(expected = "describe() called with SlashCommand::Plugin")]
    fn describing_a_plugin_command_panics() {
        let _ = describe(&SlashCommand::Plugin {
            full_name: "acme.greet".to_string(),
            args: String::new(),
        });
    }

    /// Every row [`builtin_commands`] produces really does parse (criterion
    /// 3: "nothing is advertised that does not work"), and the variant it
    /// parses into is described by `describe` with the SAME name the row
    /// itself carries -- catching a stale/renamed row even though (module
    /// notes on `builtin_variant_samples`) it cannot catch a wholly
    /// omitted one. Each row's minimal valid input is built from its own
    /// `usage` field's shape rather than hand-duplicated per command, so a
    /// changed `usage` string cannot silently desync from this test's own
    /// example.
    #[test]
    fn builtin_commands_round_trips_through_parse() {
        for row in builtin_commands() {
            let example = match row.name {
                "/steer" => "/steer a1 hello".to_string(),
                "/cancel" => "/cancel a1".to_string(),
                "/context" => "/context a1".to_string(),
                "/fork" => "/fork".to_string(),
                "/spawn" => "/spawn".to_string(),
                "/resume" => "/resume 01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                "/model" => "/model anthropic/claude".to_string(),
                "/role" => "/role reviewer".to_string(),
                "/trust" => "/trust".to_string(),
                "/ask" => "/ask is this safe?".to_string(),
                // Every other row is already valid input verbatim (bare
                // commands: `/agents`, `/settings`, `/tree`, `/why`,
                // `/help`, `/quit`, `/exit`).
                other => other.to_string(),
            };
            let cmd = parse(&example)
                .unwrap_or_else(|e| panic!("row {:?} example {example:?} failed: {e}", row.name));
            // `/resume`'s argument is only shape-validated by `parse`
            // itself (SessionId parsing happens in `execute`), so this
            // still reaches a real `SlashCommand::Resume` -- fine to
            // `describe` like everything else.
            if row.name == "/exit" {
                // The one row NOT derived from a `SlashCommand` variant --
                // see `builtin_commands`'s own doc. Proven separately,
                // below, that it round-trips to the same variant as
                // `/quit`.
                assert_eq!(cmd, SlashCommand::Quit);
                continue;
            }
            assert_eq!(
                describe(&cmd).name,
                row.name,
                "row {:?} parsed into a variant `describe` names differently",
                row.name
            );
        }
    }

    /// The one row [`builtin_commands`] carries that is NOT a distinct
    /// `SlashCommand` variant: `/exit` is a second accepted spelling of
    /// `/quit`. Proves the two spellings really do parse to the identical
    /// command, so the hand-written `/exit` row cannot silently drift from
    /// `parse`'s own `"/quit" | "/exit"` alias.
    #[test]
    fn exit_and_quit_both_parse_to_the_same_described_variant() {
        assert_eq!(parse("/quit"), parse("/exit"));
        assert_eq!(describe(&parse("/quit").unwrap()).name, "/quit");
    }

    /// No two rows share a name -- a duplicate would mean the same command
    /// listed twice (and would silently swallow one of them in the
    /// palette's prefix filter behaving oddly), never a legitimate state.
    #[test]
    fn builtin_commands_has_no_duplicate_names() {
        let names: Vec<&str> = builtin_commands().iter().map(|c| c.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "duplicate name in builtin_commands(): {names:?}"
        );
    }

    // ---------------------------------------------------------------
    // parse() -- plugin commands
    // ---------------------------------------------------------------

    #[test]
    fn plugin_shaped_command_parses_into_slash_command_plugin() {
        assert_eq!(
            parse("/acme.greet world"),
            Ok(SlashCommand::Plugin {
                full_name: "acme.greet".to_string(),
                args: "world".to_string(),
            })
        );
    }

    #[test]
    fn plugin_shaped_command_with_no_args_parses_with_empty_args() {
        assert_eq!(
            parse("/acme.greet"),
            Ok(SlashCommand::Plugin {
                full_name: "acme.greet".to_string(),
                args: String::new(),
            })
        );
    }

    /// Board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`'s own trigger, pinned
    /// directly: the operator typed `/ideate:refine` -- Claude Code's own
    /// separator -- and it must resolve to the SAME `full_name` the `.`
    /// form already does, not "unknown command".
    #[test]
    fn a_colon_separated_plugin_command_is_accepted_as_an_alias_for_the_dot_form() {
        assert_eq!(
            parse("/ideate:refine do the thing"),
            Ok(SlashCommand::Plugin {
                full_name: "ideate.refine".to_string(),
                args: "do the thing".to_string(),
            })
        );
        // Both spellings land on the IDENTICAL full_name.
        assert_eq!(parse("/ideate:refine"), parse("/ideate.refine"));
    }

    /// Only the FIRST `:` is rewritten -- a plugin id that itself contains
    /// `.` (already a supported, if unusual, shape --
    /// `plugin_shape_check_uses_the_same_separator_validate_command_name_
    /// enforces`'s own sibling coverage) still resolves correctly when
    /// typed Claude Code's way.
    #[test]
    fn only_the_first_colon_is_rewritten() {
        assert_eq!(
            parse("/foo.bar:baz"),
            Ok(SlashCommand::Plugin {
                full_name: "foo.bar.baz".to_string(),
                args: String::new(),
            })
        );
    }

    /// A bare word with neither `.` nor `:` is still "unknown command", not
    /// a plugin shape -- the colon alias narrows nothing about what
    /// already fails to parse.
    #[test]
    fn a_word_with_no_separator_at_all_is_still_unknown() {
        assert!(parse("/nonsense").is_err());
    }

    /// The shape check `parse` uses (a `.` in the command word) must agree
    /// with `conway::plugin::validate_command_name`'s own separator -- see
    /// `parse`'s own comment. Not a redundant assertion: it is what keeps
    /// the two from silently desyncing if the shared separator ever changes.
    #[test]
    fn plugin_shape_check_uses_the_same_separator_validate_command_name_enforces() {
        assert!(conway::plugin::validate_command_name("acme.greet", Some("acme")).is_ok());
        assert!(matches!(
            parse("/acme.greet"),
            Ok(SlashCommand::Plugin { .. })
        ));
    }

    #[test]
    fn a_word_with_no_dot_still_falls_through_to_unknown_command() {
        let err = parse("/nope").unwrap_err();
        assert!(err.to_string().contains("unknown command"));
    }

    // ---------------------------------------------------------------
    // execute() -- dispatch, via a fake Host
    // ---------------------------------------------------------------

    struct FakeHost {
        calls: Mutex<Vec<&'static str>>,
        root: AgentId,
        /// The fixed session id this
        /// fake `Host` reports -- lets a test assert `execute`'s
        /// `SlashCommand::Plugin` arm stamps THIS value (never a fresh one,
        /// never the focused/root agent) into `CommandCtx::session_id`.
        session: SessionId,
        context: Option<ContextReport>,
        /// The most recent `agent` `execute` actually passed to
        /// `context_report` -- lets a `/context` test assert a BARE
        /// command resolved against `AppState::focused_agent`, not just
        /// that "some" context_report call happened (board item
        /// `01M0RWKJD04JBR5NCVKBQXYHV4`).
        last_context_agent: Mutex<Option<AgentId>>,
        /// When `Some`, `fork`/`spawn` succeed with this child id instead of
        /// the default `fake_error()` -- lets a test exercise the
        /// `Effect::FocusNewSession` success path.
        fork_child: Option<AgentId>,
        spawn_child: Option<AgentId>,
        /// The most recent `ForkSpec`/`SpawnSpec` `execute` actually passed
        /// -- lets a test assert the bare/implicit paths build a
        /// `keep_alive(true)`, empty-prompt spec (module notes: never baked
        /// into the spec itself, see `Effect::FocusNewSession`'s own doc).
        last_fork_spec: Mutex<Option<ForkSpec>>,
        last_spawn_spec: Mutex<Option<SpawnSpec>>,
        /// When `true`, `promote`/`pull_in`/`purge` succeed (promote
        /// returns a fresh session id); otherwise they fail with
        /// `fake_error()` -- lets a fate test exercise both the close-modal
        /// and the keep-open-with-error paths of `apply_ask_fate`.
        fate_ok: bool,
        /// When `true`, `cancel` succeeds; otherwise it fails with
        /// `fake_error()` -- mirrors `fate_ok`'s own shape, so a `/cancel`
        /// test can exercise both the "it stops" success path and ordinary
        /// facade-error propagation.
        cancel_ok: bool,
        /// C2: when `Some`, `classify_agent_intent` succeeds with this
        /// intent; otherwise it fails with `FacadeError::IntentClassification`
        /// -- lets a free-text `/fork`/`/spawn` test exercise both the
        /// card-opens path (Ok, including a scripted passthrough) and the
        /// manual-fallback path (Err). The default (`None` -> Err) keeps
        /// the pre-C2 free-text tests' assertions closest to their old
        /// shape: they now also see one `classify_agent_intent` call
        /// before the `fork`/`spawn` they already asserted on.
        classify_intent: Option<conway::AgentIntent>,
        /// A registered-by-hand
        /// plugin-command table (keyed by full name), so `execute`'s
        /// `SlashCommand::Plugin` arm is testable with no live
        /// `CommandRegistry`/plugin at all -- `resolve_command` below is a
        /// plain lookup into this map, the same shape `LiveHost::
        /// resolve_command` delegates to `CommandRegistry::resolve`.
        plugin_commands: HashMap<String, Arc<dyn Command>>,
        /// `01KZVZ5XV162XCQR96AQKCCCF7`: when `Some`, `trust_permission_file`
        /// succeeds with this report; otherwise it fails with a fixed
        /// `std::io::Error` -- lets a `/trust` test exercise both the
        /// success path (installed rules, notices, registration errors) and
        /// the failure path (`Entry::Error`, not `Entry::Notice`).
        trust_result: Option<TrustPermissionReport>,
        /// Board item (split from `01KZHVFCN6ZEAXV7K5JHRQN1YB`): when
        /// `Some`, `preview_trust_target` succeeds with this preview;
        /// otherwise it fails with a fixed `std::io::Error` -- lets a
        /// trust-preview test exercise both the card-opens path (any of
        /// the three `TrustStatus` cases) and the read-failure path
        /// (`Entry::Error`, no card opened).
        preview_result: Option<TrustPreview>,
    }

    impl FakeHost {
        fn new(root: AgentId) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                root,
                session: SessionId::new(),
                context: None,
                last_context_agent: Mutex::new(None),
                fork_child: None,
                spawn_child: None,
                last_fork_spec: Mutex::new(None),
                last_spawn_spec: Mutex::new(None),
                fate_ok: false,
                cancel_ok: false,
                classify_intent: None,
                plugin_commands: HashMap::new(),
                trust_result: None,
                preview_result: None,
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn last_context_agent(&self) -> Option<AgentId> {
            *self.last_context_agent.lock().unwrap()
        }

        /// Registers `command` under `full_name`, for a test exercising
        /// `SlashCommand::Plugin` dispatch.
        fn with_plugin_command(mut self, full_name: &str, command: Arc<dyn Command>) -> Self {
            self.plugin_commands.insert(full_name.to_string(), command);
            self
        }

        /// Scripts `trust_permission_file` to succeed with `report` -- see
        /// that field's own doc.
        fn with_trust_result(mut self, report: TrustPermissionReport) -> Self {
            self.trust_result = Some(report);
            self
        }

        /// Scripts `preview_trust_target` to succeed with `preview` -- see
        /// that field's own doc.
        fn with_preview_result(mut self, preview: TrustPreview) -> Self {
            self.preview_result = Some(preview);
            self
        }

        /// Scripts `cancel` to succeed -- see the `cancel_ok` field's own doc.
        fn with_cancel_ok(mut self) -> Self {
            self.cancel_ok = true;
            self
        }
    }

    fn fake_error() -> FacadeError {
        FacadeError::Config {
            path: None,
            message: "fake error".to_string(),
        }
    }

    #[async_trait::async_trait]
    impl Host for FakeHost {
        fn root(&self) -> AgentId {
            self.root
        }

        fn session_id(&self) -> SessionId {
            self.session
        }

        async fn context_report(&self, agent: AgentId) -> conway::Result<ContextReport> {
            self.calls.lock().unwrap().push("context_report");
            *self.last_context_agent.lock().unwrap() = Some(agent);
            self.context.clone().ok_or_else(fake_error)
        }

        async fn session_usage(&self, _agent: AgentId) -> conway::Result<Usage> {
            self.calls.lock().unwrap().push("session_usage");
            Err(fake_error())
        }

        async fn last_model(&self, _agent: AgentId) -> conway::Result<Option<ModelRef>> {
            self.calls.lock().unwrap().push("last_model");
            Err(fake_error())
        }

        async fn fork(&self, _from: AgentId, spec: ForkSpec) -> conway::Result<AgentId> {
            self.calls.lock().unwrap().push("fork");
            *self.last_fork_spec.lock().unwrap() = Some(spec);
            self.fork_child.ok_or_else(fake_error)
        }

        async fn spawn(&self, _from: AgentId, spec: SpawnSpec) -> conway::Result<AgentId> {
            self.calls.lock().unwrap().push("spawn");
            *self.last_spawn_spec.lock().unwrap() = Some(spec);
            if let Some(child) = self.spawn_child {
                return Ok(child);
            }
            Err(fake_error())
        }

        async fn steer(&self, _target: AgentId, _text: String) -> conway::Result<()> {
            self.calls.lock().unwrap().push("steer");
            Err(fake_error())
        }

        async fn cancel(&self, _target: AgentId, _reason: String) -> conway::Result<()> {
            self.calls.lock().unwrap().push("cancel");
            if self.cancel_ok {
                Ok(())
            } else {
                Err(fake_error())
            }
        }

        async fn resume(&self, _sid: SessionId) -> conway::Result<SessionHandle> {
            self.calls.lock().unwrap().push("resume");
            // A live `SessionHandle` has no public constructor reachable
            // from this crate (`conway::SessionHandle::new` is
            // `pub(crate)` to the facade) -- this fake can only ever
            // exercise the call-count and error-propagation half of the
            // `/resume` criterion from outside `conway`, disclosed here
            // rather than silently skipped.
            Err(fake_error())
        }

        async fn promote(&self, _agent: AgentId) -> conway::Result<SessionId> {
            self.calls.lock().unwrap().push("promote");
            if self.fate_ok {
                Ok(SessionId::new())
            } else {
                Err(fake_error())
            }
        }

        async fn pull_in(&self, _child: AgentId) -> conway::Result<()> {
            self.calls.lock().unwrap().push("pull_in");
            if self.fate_ok {
                Ok(())
            } else {
                Err(fake_error())
            }
        }

        async fn purge(&self, _agent: AgentId) -> conway::Result<()> {
            self.calls.lock().unwrap().push("purge");
            if self.fate_ok {
                Ok(())
            } else {
                Err(fake_error())
            }
        }

        async fn classify_agent_intent(
            &self,
            _parent: AgentId,
            _default_recipe: SubagentMode,
            _text: &str,
        ) -> conway::Result<conway::AgentIntent> {
            self.calls.lock().unwrap().push("classify_agent_intent");
            match &self.classify_intent {
                Some(intent) => Ok(intent.clone()),
                None => Err(conway::FacadeError::IntentClassification {
                    message: "fake: intent role unconfigured".to_string(),
                }),
            }
        }

        async fn trust_permission_file(
            &self,
            _path: &std::path::Path,
            _scope: PermissionScope,
            _granting_agent: AgentId,
        ) -> std::io::Result<TrustPermissionReport> {
            self.calls.lock().unwrap().push("trust_permission_file");
            self.trust_result.clone().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "fake: trust failed")
            })
        }

        async fn preview_trust_target(
            &self,
            _path: &std::path::Path,
        ) -> std::io::Result<TrustPreview> {
            self.calls.lock().unwrap().push("preview_trust_target");
            self.preview_result.clone().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "fake: preview read failed")
            })
        }

        fn resolve_command(&self, full_name: &str) -> Option<Arc<dyn Command>> {
            self.calls.lock().unwrap().push("resolve_command");
            self.plugin_commands.get(full_name).cloned()
        }
    }

    // ---------------------------------------------------------------
    // execute() -- SlashCommand::Plugin
    // ---------------------------------------------------------------

    /// A fixture plugin command that echoes `ctx.args` back, prefixed --
    /// this module's own equivalent of the item's `/greet` verification
    /// fixture, used to prove `execute` reaches an installed plugin command
    /// end to end (not vacuously: `plugin_command_dispatch_is_unknown_when_
    /// not_registered` below shows the SAME input fails when the fixture is
    /// absent).
    struct GreetCommand;

    #[async_trait::async_trait]
    impl Command for GreetCommand {
        fn spec(&self) -> CommandSpec {
            CommandSpec {
                name: "greet".to_string(),
                summary: "echoes its argument".to_string(),
            }
        }

        async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
            CommandOutcome::Output(vec![format!("hello, {}!", ctx.args)])
        }
    }

    /// A fixture plugin command that never resolves -- `Command::invoke`'s
    /// own `Future` never completes. Used ONLY to prove `execute` never
    /// awaits a plugin's `invoke` itself (the structural property behind
    /// this item's hang-safety acceptance criterion); actually running
    /// this to completion would hang the test process, so no test here
    /// ever `.await`s the `CommandOutcome` this produces -- only that
    /// `execute` returns without needing to.
    struct HangingCommand;

    #[async_trait::async_trait]
    impl Command for HangingCommand {
        fn spec(&self) -> CommandSpec {
            CommandSpec {
                name: "hang".to_string(),
                summary: "never returns".to_string(),
            }
        }

        async fn invoke(&self, _ctx: CommandCtx) -> CommandOutcome {
            std::future::pending::<()>().await;
            unreachable!("pending() never resolves")
        }
    }

    #[tokio::test]
    async fn execute_dispatches_a_resolved_plugin_command_via_run_plugin_command_effect() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root).with_plugin_command("acme.greet", Arc::new(GreetCommand));

        let effect = execute(
            SlashCommand::Plugin {
                full_name: "acme.greet".to_string(),
                args: "world".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        let Effect::RunPluginCommand(invocation) = effect else {
            panic!("expected Effect::RunPluginCommand");
        };
        assert_eq!(invocation.full_name, "acme.greet");
        assert_eq!(invocation.ctx.args, "world");
        assert_eq!(invocation.ctx.root_agent, root);
        assert_eq!(invocation.ctx.focused_agent, state.focused_agent);
        assert_eq!(invocation.ctx.session_id, host.session_id());
        // `execute` resolved the command but did NOT run it -- proven
        // directly by actually invoking it now, outside `execute`'s own
        // call, and checking the fixture's own behavior fires.
        let outcome = invocation.command.invoke(invocation.ctx).await;
        assert_eq!(
            outcome,
            CommandOutcome::Output(vec!["hello, world!".to_string()])
        );
    }

    /// The verification anchor's negative half: the identical input fails
    /// as an unknown command when nothing is registered under that name --
    /// proves the positive test above is not vacuous.
    #[tokio::test]
    async fn plugin_command_dispatch_is_unknown_when_not_registered() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root); // no plugin commands registered

        let effect = execute(
            SlashCommand::Plugin {
                full_name: "acme.greet".to_string(),
                args: "world".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None));
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text.contains("unknown command") && text.contains("acme.greet")
        ));
    }

    /// **Hang-safety, direct proof.** `execute` must return promptly even
    /// when the resolved command's `invoke` would never complete --
    /// `execute` never calls `invoke` at all (only `Effect::
    /// RunPluginCommand`'s eventual consumer, `App::spawn_plugin_command`,
    /// does, off the render/input loop). Wrapped in a generous timeout so a
    /// regression that DID start awaiting the hang fails this test instead
    /// of hanging the whole suite.
    #[tokio::test]
    async fn execute_never_awaits_a_hanging_plugin_command() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root).with_plugin_command("acme.hang", Arc::new(HangingCommand));

        let effect = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            execute(
                SlashCommand::Plugin {
                    full_name: "acme.hang".to_string(),
                    args: String::new(),
                },
                &mut state,
                &host,
            ),
        )
        .await
        .expect("execute must return promptly, even for a command whose invoke() hangs forever");

        assert!(matches!(effect, Effect::RunPluginCommand(_)));
    }

    // ---------------------------------------------------------------
    // CommandRegistry::build
    // ---------------------------------------------------------------

    struct FixturePlugin {
        id: &'static str,
        command_names: Vec<&'static str>,
    }

    impl conway::plugin::Plugin for FixturePlugin {
        fn manifest(&self) -> conway::plugin::PluginManifest {
            conway::plugin::PluginManifest {
                id: self.id.to_string(),
                version: "0.1.0".to_string(),
                tools: vec![],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
            vec![]
        }

        fn commands(&self) -> Vec<Arc<dyn Command>> {
            self.command_names
                .iter()
                .map(|name| -> Arc<dyn Command> { Arc::new(NamedCommand(name.to_string())) })
                .collect()
        }
    }

    struct NamedCommand(String);

    #[async_trait::async_trait]
    impl Command for NamedCommand {
        fn spec(&self) -> CommandSpec {
            CommandSpec {
                name: self.0.clone(),
                summary: format!("fixture command '{}'", self.0),
            }
        }

        async fn invoke(&self, _ctx: CommandCtx) -> CommandOutcome {
            CommandOutcome::Output(vec![])
        }
    }

    #[test]
    fn command_registry_build_registers_a_namespaced_full_name() {
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(FixturePlugin {
            id: "acme",
            command_names: vec!["greet"],
        });
        let registry = CommandRegistry::build(&[plugin]).unwrap();
        assert!(registry.resolve("acme.greet").is_some());
        assert!(registry.resolve("greet").is_none());
    }

    /// The structural shadow-prevention proof: a plugin naming its own
    /// command "help" -- the exact attempt the acceptance criterion names
    /// ("a plugin declaring `/help` must not shadow the built-in") -- never
    /// produces a bare `"help"` entry. It registers cleanly as its own,
    /// separately reachable, correctly namespaced command instead: not an
    /// error (there is nothing wrong with a plugin having a command NAMED
    /// "help"), and not a shadow (the built-in `/help` is a different,
    /// untouched code path -- `SlashCommand::Help`, matched by `parse`
    /// BEFORE this shape check ever runs).
    #[test]
    fn a_plugin_naming_its_command_help_does_not_shadow_the_built_in_help() {
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(FixturePlugin {
            id: "acme",
            command_names: vec!["help"],
        });
        let registry = CommandRegistry::build(&[plugin]).unwrap();
        assert!(
            registry.resolve("help").is_none(),
            "the bare name 'help' must never resolve to a plugin command"
        );
        assert!(
            registry.resolve("acme.help").is_some(),
            "the plugin's own namespaced command must still register"
        );
        // And `parse` itself still routes bare `/help` to the untouched
        // built-in, never to anything plugin-shaped.
        assert_eq!(parse("/help"), Ok(SlashCommand::Help));
    }

    #[test]
    fn command_registry_build_rejects_a_duplicate_full_name_with_a_named_error() {
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(FixturePlugin {
            id: "acme",
            command_names: vec!["greet", "greet"],
        });
        let err = CommandRegistry::build(&[plugin]).unwrap_err();
        assert!(err.to_string().contains("acme.greet"));
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn command_registry_build_rejects_a_command_name_with_whitespace() {
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(FixturePlugin {
            id: "acme",
            command_names: vec!["not a valid name"],
        });
        let err = CommandRegistry::build(&[plugin]).unwrap_err();
        assert!(err.to_string().contains("acme"));
        assert!(err.to_string().contains("whitespace"));
    }

    #[test]
    fn command_registry_palette_entries_are_slash_prefixed_and_carry_the_summary() {
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(FixturePlugin {
            id: "acme",
            command_names: vec!["greet"],
        });
        let registry = CommandRegistry::build(&[plugin]).unwrap();
        let entries = registry.palette_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "/acme.greet");
        assert_eq!(entries[0].description, "fixture command 'greet'");
    }

    #[test]
    fn command_registry_build_with_no_plugins_is_empty() {
        let registry = CommandRegistry::build(&[]).unwrap();
        assert!(registry.is_empty());
        assert!(registry.palette_entries().is_empty());
    }

    #[tokio::test]
    async fn steer_maps_to_exactly_one_steer_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Steer {
                target: root.to_string(),
                text: "hold on".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None));
        assert_eq!(host.calls(), vec!["steer"]);
    }

    // ---------------------------------------------------------------
    // /cancel
    // ---------------------------------------------------------------

    /// Acceptance 1/2: cancelling a running, NON-focused subagent (mirrors
    /// `/steer`'s own targeting -- `resolve_agent` reaches an agent whether
    /// or not it is the one currently in view) stops it -- proved here by
    /// exactly one `cancel` facade call reaching `host` -- and the SESSION
    /// itself keeps working: `Effect::None`, never `Effect::Quit`, and the
    /// outcome is surfaced as a transcript `Notice` an operator watching the
    /// screen would see (acceptance 2's "visible to the operator" -- the
    /// AGENT's own tree-node status flip to `Cancelled`/the `-` marker is a
    /// separate, pre-existing mechanism: `AgentFinished { Cancelled }` ->
    /// `AppState::apply_agent_finished`, unrelated to this command's own
    /// dispatch and untouched by this item).
    #[tokio::test]
    async fn cancel_a_running_non_focused_subagent_stops_it_and_the_session_lives() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: None,
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        // `state.focused_agent` stays on `root` (`AppState::new`'s default)
        // -- `child` is deliberately NOT the focused agent.
        assert_ne!(state.focused_agent, child);
        let host = FakeHost::new(root).with_cancel_ok();

        let effect = execute(
            SlashCommand::Cancel {
                target: child.to_string(),
                reason: Some("burning tokens".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(
            matches!(effect, Effect::None),
            "the parent session must keep working, not quit"
        );
        assert_eq!(host.calls(), vec!["cancel"]);
        assert!(
            notice_lines(&state)
                .iter()
                .any(|line| line.contains(&child.to_string()) && line.contains("burning tokens")),
            "the cancellation must be visible in the transcript: {:?}",
            notice_lines(&state)
        );
    }

    /// Acceptance 5, half 2: cancelling an unknown ref is a typed error and
    /// NOTHING dies -- `resolve_agent` fails before `host.cancel` is ever
    /// reached (zero facade calls), and `execute` still returns
    /// `Effect::None`, exactly like an unresolvable `/steer` target already
    /// does.
    #[tokio::test]
    async fn cancel_an_unknown_ref_is_a_typed_error_and_nothing_dies() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Cancel {
                target: "not-a-real-agent".to_string(),
                reason: None,
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None));
        assert!(host.calls().is_empty(), "no facade call may be made");
        assert!(
            notice_lines(&state)
                .iter()
                .any(|line| line.contains("no agent matches")),
            "the failure must be a named, typed notice: {:?}",
            notice_lines(&state)
        );
    }

    /// Determination question 3: cancelling the session's own ROOT agent is
    /// refused loudly, before any facade call -- `CancelMode::Immediate`
    /// propagates to the whole subtree structurally, so cancelling root
    /// would end the entire session, which this command's acceptance
    /// ("without ending the session") forbids. `/quit` remains the way to
    /// end a session; this guard fires even when `target` is spelled out as
    /// root's own full id, matching `resolve_agent`'s full-id pass.
    #[tokio::test]
    async fn cancel_targeting_the_session_root_is_refused_before_any_facade_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root).with_cancel_ok();

        let effect = execute(
            SlashCommand::Cancel {
                target: root.to_string(),
                reason: None,
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None));
        assert!(
            host.calls().is_empty(),
            "the root guard must fire before host.cancel is ever called"
        );
        assert!(
            notice_lines(&state)
                .iter()
                .any(|line| line.contains("cannot cancel") && line.contains("/quit")),
            "the refusal must name the reason and point at /quit: {:?}",
            notice_lines(&state)
        );
    }

    // ---------------------------------------------------------------
    // B5: /ask modal fates -- each fate maps to exactly one facade op
    // ---------------------------------------------------------------

    fn modal_state() -> (AppState, AgentId) {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.offer_ask_modal(crate::tui::state::AskModal {
            question: "q".to_string(),
            child,
            answer: "the answer".to_string(),
            error: None,
        });
        (state, child)
    }

    #[tokio::test]
    async fn fork_fate_invokes_promote_and_closes_the_modal() {
        let (mut state, _child) = modal_state();
        let mut host = FakeHost::new(state.root_agent());
        host.fate_ok = true;

        apply_ask_fate(AskFate::Fork, &mut state, &host).await;

        assert_eq!(host.calls(), vec!["promote"]);
        assert!(
            matches!(state.mode, Mode::Normal),
            "a successful fate must close the modal, got: {:?}",
            state.mode
        );
        assert!(
            matches!(state.transcript.last(), Some(Entry::Notice { .. })),
            "the outcome is recorded as a Notice"
        );
    }

    #[tokio::test]
    async fn pull_in_fate_invokes_pull_in_and_closes_the_modal() {
        let (mut state, _child) = modal_state();
        let mut host = FakeHost::new(state.root_agent());
        host.fate_ok = true;

        apply_ask_fate(AskFate::PullIn, &mut state, &host).await;

        assert_eq!(host.calls(), vec!["pull_in"]);
        assert!(matches!(state.mode, Mode::Normal));
    }

    #[tokio::test]
    async fn discard_fate_invokes_purge_and_closes_the_modal() {
        let (mut state, _child) = modal_state();
        let mut host = FakeHost::new(state.root_agent());
        host.fate_ok = true;

        apply_ask_fate(AskFate::Discard, &mut state, &host).await;

        assert_eq!(host.calls(), vec!["purge"]);
        assert!(matches!(state.mode, Mode::Normal));
    }

    /// The forced-choice invariant: a FAILED fate (here: a refused pull-in)
    /// must keep the modal open with the error shown in-modal -- never
    /// close it, never fall through to another fate.
    #[tokio::test]
    async fn a_failed_fate_keeps_the_modal_open_with_the_error_shown() {
        let (mut state, child) = modal_state();
        let host = FakeHost::new(state.root_agent()); // fate_ok: false -> fails

        apply_ask_fate(AskFate::PullIn, &mut state, &host).await;

        assert_eq!(host.calls(), vec!["pull_in"]);
        match &state.mode {
            Mode::AskModal(modal) => {
                assert_eq!(modal.child, child, "the same ask is still open");
                assert!(
                    modal.error.is_some(),
                    "the failure must surface as an in-modal error"
                );
            }
            other => panic!("a failed fate must KEEP the modal open, got: {other:?}"),
        }
        assert!(
            !state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Notice { text } if text.contains("pulled in"))),
            "a failed fate must not record a success notice"
        );
    }

    /// Board item `01M0TYRPF1ASGQ77AK04RB7H84`: a failed fate's FULL error
    /// text must also land in the transcript (`Entry::Error { fatal: false
    /// }`), durable and scrollable there regardless of what the in-modal
    /// footer itself has room to show -- see `view/mod.rs`'s
    /// `draw_ask_modal`/`ASK_MODAL_MAX_ERROR_ROWS` for the rendering-layer
    /// half of this fix.
    #[tokio::test]
    async fn a_failed_fate_pushes_the_full_error_to_the_transcript() {
        let (mut state, _child) = modal_state();
        let host = FakeHost::new(state.root_agent()); // fate_ok: false -> fails

        apply_ask_fate(AskFate::PullIn, &mut state, &host).await;

        assert!(
            state.transcript.iter().any(
                |e| matches!(e, Entry::Error { text, fatal: false } if text.contains("fake error"))
            ),
            "the full error text must reach the transcript as a non-fatal \
             Entry::Error, not be dropped once it no longer fits the \
             footer: {:?}",
            state.transcript
        );
        // The in-modal error and the transcript entry carry the SAME
        // underlying text -- the transcript is a durable copy, not a
        // different (shorter or longer) message.
        match &state.mode {
            Mode::AskModal(modal) => {
                assert_eq!(modal.error.as_deref(), Some("fake error"));
            }
            other => panic!("expected the modal to stay open, got: {other:?}"),
        }
    }

    /// A stale fate key after the modal already closed must not double-apply
    /// a fate (no host call at all).
    #[tokio::test]
    async fn a_fate_with_no_modal_open_is_a_noop() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        apply_ask_fate(AskFate::Discard, &mut state, &host).await;

        assert!(host.calls().is_empty());
        assert!(state.transcript.is_empty());
    }

    // ---------------------------------------------------------------
    // C2: NL intent classification on free-text /fork and /spawn.
    // The routing decision (free-text vs `@def` vs bare) is the part of
    // the command handler reachable without a live `Conway` -- `Host::
    // classify_agent_intent` is the mockable seam. The facade call's
    // end-to-end effect is covered by the `conway` crate's own C1 tests.
    // ---------------------------------------------------------------

    fn scripted_intent(
        recipe: SubagentMode,
        agent_def: Option<&str>,
        prompt: &str,
    ) -> conway::AgentIntent {
        conway::AgentIntent {
            recipe,
            agent_def: agent_def.map(str::to_string),
            prompt: prompt.to_string(),
        }
    }

    #[tokio::test]
    async fn free_text_spawn_classifies_and_opens_the_card_on_ok() {
        // Free text, no `@agent_def`: classify runs and on Ok the card
        // opens; NO spawn is called yet (the card is the trust gate).
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.classify_intent = Some(scripted_intent(
            SubagentMode::Spawn,
            Some("reviewer"),
            "review the diff carefully",
        ));

        let effect = execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(
            host.calls(),
            vec!["classify_agent_intent"],
            "Ok -> only classify ran; the card gates the spawn"
        );
        assert!(
            matches!(effect, Effect::None),
            "the card opens, no spawn effect yet"
        );
        match &state.mode {
            Mode::IntentConfirm(card) => {
                assert_eq!(card.intent.recipe, SubagentMode::Spawn);
                assert_eq!(card.intent.agent_def.as_deref(), Some("reviewer"));
                assert_eq!(card.intent.prompt, "review the diff carefully");
                assert_eq!(card.raw_text, "review the diff");
                assert_eq!(card.default_recipe, SubagentMode::Spawn);
            }
            other => panic!("expected the card to be open, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn free_text_spawn_on_passthrough_opens_the_card_with_the_raw_text() {
        // The verbatim passthrough (unconfigured role etc.) is NOT an
        // error -- the card still opens, with the raw text as the prompt.
        // This is the spec's "pick and test" behavior.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.classify_intent = Some(scripted_intent(
            SubagentMode::Spawn,
            None,
            "review the diff", // passthrough: prompt == raw text, no def
        ));

        execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["classify_agent_intent"]);
        match &state.mode {
            Mode::IntentConfirm(card) => {
                assert_eq!(card.intent.prompt, "review the diff");
                assert!(card.intent.agent_def.is_none());
                assert_eq!(card.intent.recipe, SubagentMode::Spawn);
            }
            other => panic!("passthrough must still open the card, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn free_text_spawn_on_classify_error_falls_back_to_manual_with_a_notice() {
        // A propagated IntentClassification (a real backend failure, NOT
        // the passthrough) must NOT open the card -- today's manual flow
        // runs with the raw text, plus a notice.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.spawn_child = Some(AgentId::new());
        // classify_intent stays None -> FakeHost returns IntentClassification.

        let effect = execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(
            host.calls(),
            vec!["classify_agent_intent", "spawn"],
            "the manual fallback must still call spawn"
        );
        assert!(
            !matches!(state.mode, Mode::IntentConfirm(_)),
            "the card must NOT appear for a hard error"
        );
        assert!(
            state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Notice { text } if text.contains("intent classification failed"))),
            "the fallback notice must be present: {:?}",
            state.transcript
        );
        match effect {
            Effect::FocusNewSession { first_message, .. } => {
                assert_eq!(first_message, Some("review the diff".to_string()));
            }
            _ => panic!("the manual fallback must return FocusNewSession, got a different effect"),
        }
    }

    #[tokio::test]
    async fn free_text_fork_classifies_and_opens_the_card_on_ok() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.classify_intent = Some(scripted_intent(
            SubagentMode::Fork,
            None,
            "please review this carefully",
        ));

        let effect = execute(
            SlashCommand::Fork {
                agent: None,
                directive: Some("please review this".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["classify_agent_intent"]);
        assert!(matches!(effect, Effect::None));
        match &state.mode {
            Mode::IntentConfirm(card) => {
                assert_eq!(card.intent.recipe, SubagentMode::Fork);
                assert_eq!(card.intent.prompt, "please review this carefully");
                assert_eq!(card.raw_text, "please review this");
            }
            other => panic!("expected the card to be open, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn explicit_at_agent_def_spawn_skips_classify() {
        // Explicit `@<agent_def>` syntax skips inference entirely --
        // preserve current behavior. No classify call, spawn runs directly.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.spawn_child = Some(AgentId::new());

        execute(
            SlashCommand::Spawn {
                agent_def: Some("reviewer".to_string()),
                prompt: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(
            host.calls(),
            vec!["spawn"],
            "explicit @agent_def must skip classify"
        );
        assert!(!matches!(state.mode, Mode::IntentConfirm(_)));
    }

    #[tokio::test]
    async fn explicit_at_agent_fork_skips_classify() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Fork {
                agent: Some(root.to_string()),
                directive: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(
            host.calls(),
            vec!["fork"],
            "explicit @agent must skip classify"
        );
        assert!(!matches!(state.mode, Mode::IntentConfirm(_)));
    }

    #[tokio::test]
    async fn bare_spawn_skips_classify() {
        // Bare `/spawn` (no text) is unchanged: no classify, no card.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.spawn_child = Some(AgentId::new());

        execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: None,
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(
            host.calls(),
            vec!["spawn"],
            "bare /spawn must skip classify"
        );
        assert!(!matches!(state.mode, Mode::IntentConfirm(_)));
    }

    #[tokio::test]
    async fn bare_fork_skips_classify() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.fork_child = Some(AgentId::new());

        execute(
            SlashCommand::Fork {
                agent: None,
                directive: None,
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["fork"], "bare /fork must skip classify");
        assert!(!matches!(state.mode, Mode::IntentConfirm(_)));
    }

    // ---------------------------------------------------------------
    // C2: execute_intent_confirm -- the three choices' facade dispatch.
    // ---------------------------------------------------------------

    fn card_in_state(
        intent: conway::AgentIntent,
        default_recipe: SubagentMode,
        raw_text: &str,
    ) -> AppState {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(IntentConfirm {
            intent,
            default_recipe,
            raw_text: raw_text.to_string(),
            parent: AgentId::new(),
        });
        state
    }

    #[tokio::test]
    async fn confirm_runs_the_classified_recipe_and_closes_the_card() {
        // Classifier said `spawn` with agent_def `reviewer` and a rewritten
        // prompt -- Confirm re-dispatches through `execute` with exactly
        // those, ignoring the raw text and the original `/fork` default.
        let root = AgentId::new();
        let mut state = card_in_state(
            scripted_intent(
                SubagentMode::Spawn,
                Some("reviewer"),
                "review the diff carefully",
            ),
            SubagentMode::Fork, // user typed /fork, classifier cross-classified to spawn
            "review the diff",
        );
        let mut host = FakeHost::new(root);
        host.spawn_child = Some(AgentId::new());

        let effect = execute_intent_confirm(IntentChoice::Confirm, &mut state, &host).await;

        assert_eq!(
            host.calls(),
            vec!["spawn"],
            "Confirm runs the classified recipe (spawn), not the default (fork)"
        );
        assert!(
            matches!(state.mode, Mode::Normal),
            "Confirm must close the card, got: {:?}",
            state.mode
        );
        let spec = host
            .last_spawn_spec
            .lock()
            .unwrap()
            .clone()
            .expect("spawn should have been called");
        assert_eq!(
            spec.agent_def.as_deref(),
            Some("reviewer"),
            "the classified agent_def must reach the SpawnSpec"
        );
        match effect {
            Effect::FocusNewSession { first_message, .. } => {
                assert_eq!(
                    first_message,
                    Some("review the diff carefully".to_string()),
                    "the classified prompt becomes the first message"
                );
            }
            _ => panic!("Confirm must return FocusNewSession, got a different effect"),
        }
    }

    #[tokio::test]
    async fn confirm_fork_recipe_ignores_a_classifier_returned_agent_def() {
        // ForkSpec has no agent_def field -- a classifier-returned def on a
        // Fork recipe is ignored (a fork inherits the parent's def),
        // matching `AgentIntent`'s own doc.
        let root = AgentId::new();
        let mut state = card_in_state(
            scripted_intent(SubagentMode::Fork, Some("reviewer"), "go"),
            SubagentMode::Fork,
            "go",
        );
        let mut host = FakeHost::new(root);
        host.fork_child = Some(AgentId::new());

        let effect = execute_intent_confirm(IntentChoice::Confirm, &mut state, &host).await;

        assert_eq!(host.calls(), vec!["fork"]);
        match effect {
            Effect::FocusNewSession { first_message, .. } => {
                assert_eq!(first_message, Some("go".to_string()));
            }
            _ => panic!("expected FocusNewSession, got a different effect"),
        }
    }

    #[tokio::test]
    async fn manual_falls_back_to_the_default_recipe_with_the_raw_text() {
        // Manual uses the ORIGINAL command's default_recipe and the raw
        // text (untouched), not the classifier's rewrite -- today's
        // pre-classification flow, verbatim.
        let root = AgentId::new();
        let mut state = card_in_state(
            scripted_intent(
                SubagentMode::Spawn,
                Some("reviewer"),
                "review the diff carefully",
            ),
            SubagentMode::Fork, // user typed /fork
            "review the diff",  // raw text
        );
        let mut host = FakeHost::new(root);
        host.fork_child = Some(AgentId::new());

        let effect = execute_intent_confirm(IntentChoice::Manual, &mut state, &host).await;

        assert_eq!(
            host.calls(),
            vec!["fork"],
            "Manual uses the default_recipe (fork), not the classified recipe (spawn)"
        );
        assert!(matches!(state.mode, Mode::Normal));
        match effect {
            Effect::FocusNewSession { first_message, .. } => {
                assert_eq!(
                    first_message,
                    Some("review the diff".to_string()),
                    "Manual uses the RAW text, not the classified prompt"
                );
            }
            _ => panic!("Manual must return FocusNewSession, got a different effect"),
        }
    }

    #[tokio::test]
    async fn edit_is_a_noop_for_the_facade() {
        // The key handler has already dropped intent.prompt into state.input
        // and closed the card; execute_intent_confirm(Edit) does nothing.
        let root = AgentId::new();
        let mut state = card_in_state(
            scripted_intent(SubagentMode::Spawn, None, "review the diff carefully"),
            SubagentMode::Spawn,
            "review the diff",
        );
        // Simulate the key handler's Edit: drop prompt into input, close card.
        state.begin_intent_confirm_edit();
        assert_eq!(state.input, "review the diff carefully");
        assert!(matches!(state.mode, Mode::Normal));

        let host = FakeHost::new(root);
        let effect = execute_intent_confirm(IntentChoice::Edit, &mut state, &host).await;

        assert!(host.calls().is_empty(), "Edit must not call any facade op");
        assert!(matches!(effect, Effect::None));
        assert_eq!(
            state.input, "review the diff carefully",
            "the input line is untouched"
        );
    }

    #[tokio::test]
    async fn execute_intent_confirm_is_a_noop_when_no_card_is_open() {
        // A stale choice key after the card already closed cannot
        // double-apply (mirrors `apply_ask_fate`'s no-modal-open guard).
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute_intent_confirm(IntentChoice::Confirm, &mut state, &host).await;

        assert!(host.calls().is_empty());
        assert!(matches!(effect, Effect::None));
    }

    // ---------------------------------------------------------------
    // /tree (item A3: alias rendering `state.tree`)
    // ---------------------------------------------------------------

    /// Builds one `TreeNode` fixture directly (the fields are all `pub` --
    /// `view::agents`'s own tests construct fixtures the same way), so a
    /// `/tree` test composes `state.tree` by hand and never consults a
    /// runtime/host snapshot.
    fn tree_node(
        agent_id: AgentId,
        parent: Option<AgentId>,
        agent_def: Option<&str>,
        status: NodeStatus,
        kind: Option<SubagentMode>,
        inherited_upto: Option<conway::LogSeq>,
        ephemeral: bool,
    ) -> TreeNode {
        TreeNode {
            agent_id,
            parent,
            agent_def: agent_def.map(str::to_string),
            status,
            kind,
            inherited_upto,
            ephemeral,
        }
    }

    fn notice_lines(state: &AppState) -> Vec<&str> {
        state
            .transcript
            .iter()
            .filter_map(|entry| match entry {
                Entry::Notice { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Item A3: `/tree` is an alias for the `/agents` panel -- it
    /// makes NO facade call, and renders from `state.tree` alone. (The
    /// `Host` seam no longer even EXPOSES a runtime tree snapshot -- item
    /// A3 removed `Host::tree` once this became its only caller -- so
    /// "renders `state.tree` even when the runtime host tree would differ"
    /// holds by construction; the empty `host.calls()` assertion is what a
    /// regression back to a facade lookup would trip.)
    ///
    /// This is ALSO the pinning test for board item
    /// `01M0TNCAP1HH4YNC5K9753YG26`'s id-format decision: `child.to_string()`
    /// is the FULL 26-character ULID (`AgentId`'s `Display`, not
    /// `view::agents::panel_agent_id`'s screen-relative short form), and the
    /// assertion below is deliberately looking for that exact full string,
    /// not a prefix of it. `/tree` keeps printing full ids on purpose (see
    /// `render_tree_snapshot`'s doc for why); if a future change makes
    /// `/tree` print short ids instead, this assertion must be rewritten to
    /// say so, not silently relaxed to `starts_with`.
    #[tokio::test]
    async fn tree_makes_no_facade_call_and_renders_state_tree_not_the_host_snapshot() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.tree.nodes.push(tree_node(
            child,
            Some(root),
            Some("worker"),
            NodeStatus::Running,
            Some(SubagentMode::Spawn),
            None,
            false,
        ));
        let host = FakeHost::new(root);

        execute(SlashCommand::Tree, &mut state, &host).await;

        assert!(
            host.calls().is_empty(),
            "/tree must not consult the runtime host tree: {:?}",
            host.calls()
        );
        let lines = notice_lines(&state);
        assert_eq!(lines.len(), 2, "one rendered line per state.tree node");
        let child_line = lines
            .iter()
            .find(|line| line.contains(&child.to_string()))
            .expect("the state.tree child must have a rendered line");
        assert!(
            child_line.contains("worker") && child_line.contains("[Running]"),
            "the line renders label + status from the TreeNode: {child_line:?}"
        );
    }

    /// Item A3 (reworked onto `state.tree` from the MIN-3 runtime-snapshot
    /// test): the snapshot keeps ephemeral `/ask` children in the output
    /// (provenance is kept) and marks them with the panel's own plain-text
    /// `(ephemeral)` recipe part so they read distinctly from persistent
    /// subagents -- ASCII only, so a copied transcript line keeps the
    /// marker.
    #[test]
    fn render_tree_snapshot_marks_ephemeral_nodes_only() {
        let root = AgentId::new();
        let ephemeral_child = AgentId::new();
        let persistent_child = AgentId::new();
        let mut state = AppState::new(root);
        state.tree.nodes.push(tree_node(
            ephemeral_child,
            Some(root),
            None,
            NodeStatus::Running,
            Some(SubagentMode::Fork),
            Some(conway::LogSeq(7)),
            true,
        ));
        state.tree.nodes.push(tree_node(
            persistent_child,
            Some(root),
            None,
            NodeStatus::Running,
            Some(SubagentMode::Fork),
            Some(conway::LogSeq(7)),
            false,
        ));

        render_tree_snapshot(&mut state);

        let lines = notice_lines(&state);
        // Every node renders exactly one line -- the marker is added, never
        // a node filtered (the snapshot ignores the panel's visibility
        // filter by design, see `render_tree_snapshot`'s own doc).
        assert_eq!(lines.len(), 3, "one rendered line per state.tree node");
        let line_of = |id: AgentId| {
            lines
                .iter()
                .find(|line| line.contains(&id.to_string()))
                .unwrap_or_else(|| panic!("{id} must have a rendered line"))
        };
        assert!(
            line_of(ephemeral_child).contains("(ephemeral)"),
            "ephemeral child's line carries the marker: {:?}",
            line_of(ephemeral_child)
        );
        assert!(
            !line_of(persistent_child).contains("(ephemeral)"),
            "persistent child's line carries no marker: {:?}",
            line_of(persistent_child)
        );
        assert!(
            !line_of(root).contains("(ephemeral)"),
            "root's line carries no marker: {:?}",
            line_of(root)
        );
    }

    /// Item A3: every `/tree` line carries the panel's A2 recipe labels,
    /// derived from `state.tree` via the SAME `recipe_parts` the panel
    /// draws with -- `fork @seq N` for forks, `@agent_def` / `(inherit)`
    /// for spawns, `(ephemeral)` on top of either -- and indents children
    /// by ancestor depth exactly like the panel rows.
    #[test]
    fn render_tree_snapshot_includes_the_panels_recipe_labels_and_indent() {
        let root = AgentId::new();
        let fork = AgentId::new();
        let spawn_def = AgentId::new();
        let spawn_inherit = AgentId::new();
        let ask = AgentId::new();
        let mut state = AppState::new(root);
        state.tree.nodes.push(tree_node(
            fork,
            Some(root),
            None,
            NodeStatus::Running,
            Some(SubagentMode::Fork),
            Some(conway::LogSeq(42)),
            false,
        ));
        state.tree.nodes.push(tree_node(
            spawn_def,
            Some(root),
            Some("reviewer"),
            NodeStatus::Running,
            Some(SubagentMode::Spawn),
            None,
            false,
        ));
        state.tree.nodes.push(tree_node(
            spawn_inherit,
            Some(root),
            None,
            NodeStatus::Running,
            Some(SubagentMode::Spawn),
            None,
            false,
        ));
        state.tree.nodes.push(tree_node(
            ask,
            Some(root),
            None,
            NodeStatus::Running,
            Some(SubagentMode::Fork),
            Some(conway::LogSeq(7)),
            true,
        ));

        render_tree_snapshot(&mut state);

        let lines = notice_lines(&state);
        assert_eq!(lines.len(), 5, "one rendered line per state.tree node");
        let line_of = |id: AgentId| {
            lines
                .iter()
                .find(|line| line.contains(&id.to_string()))
                .unwrap_or_else(|| panic!("{id} must have a rendered line"))
        };
        assert!(
            line_of(fork).contains("fork @seq 42"),
            "fork recipe label: {:?}",
            line_of(fork)
        );
        assert!(
            line_of(spawn_def).contains("@reviewer"),
            "spawn @agent_def recipe label: {:?}",
            line_of(spawn_def)
        );
        assert!(
            line_of(spawn_inherit).contains("(inherit)"),
            "spawn-without-agent_def recipe label: {:?}",
            line_of(spawn_inherit)
        );
        let ask_line = line_of(ask);
        assert!(
            ask_line.contains("fork @seq 7") && ask_line.contains("(ephemeral)"),
            "an ephemeral fork carries both its recipe and the marker: {ask_line:?}"
        );
        let root_line = line_of(root);
        assert!(
            root_line.contains("agent [")
                && !root_line.contains("fork")
                && !root_line.contains('@')
                && !root_line.contains("(inherit)"),
            "the root/legacy node renders label + status with no recipe parts: {root_line:?}"
        );
        // Children of root indent one level, exactly like the panel rows.
        assert!(
            line_of(fork).starts_with("  ") && !line_of(root).starts_with("  "),
            "indent must follow ancestor depth"
        );
    }

    /// T7 acceptance: `/help` opens the keybinding overlay and pushes ZERO
    /// `Entry::Notice` lines -- the old transcript-dump behavior
    /// (`HELP_LINES`/`render_help`, both removed) is gone entirely, not just
    /// trimmed down.
    #[tokio::test]
    async fn help_opens_the_overlay_and_pushes_no_transcript_entries() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);
        assert!(!state.help_open);

        let effect = execute(SlashCommand::Help, &mut state, &host).await;

        assert!(state.help_open, "/help must open the keybinding overlay");
        assert!(
            state.transcript.is_empty(),
            "/help must push zero transcript entries (no `Entry::Notice` \
             dump), got {:?}",
            state.transcript
        );
        assert!(matches!(effect, Effect::None));
        // No facade call at all -- a pure state flip.
        assert!(host.calls().is_empty());
    }

    /// V4 acceptance: `/settings` opens the menu -- a pure `AppState` flip,
    /// mirroring `/help`'s own test exactly.
    #[tokio::test]
    async fn settings_opens_the_menu() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);
        assert!(!state.settings_open);

        let effect = execute(SlashCommand::Settings, &mut state, &host).await;

        assert!(state.settings_open, "/settings must open the menu");
        assert!(matches!(effect, Effect::None));
        assert!(
            host.calls().is_empty(),
            "no facade call at all -- a pure state flip"
        );
    }

    /// Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`: `/plugin` opens the
    /// listing -- a pure `AppState` flip, mirroring `/settings`'s own test
    /// exactly.
    #[tokio::test]
    async fn plugin_opens_the_listing() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);
        assert!(!state.plugins_open);

        let effect = execute(SlashCommand::Plugins { action: None }, &mut state, &host).await;

        assert!(state.plugins_open, "/plugin must open the listing");
        assert!(matches!(effect, Effect::None));
        assert!(
            host.calls().is_empty(),
            "no facade call at all -- a pure state flip"
        );
    }

    /// Board item `01M0WB5W5DX844HSJQG3JP23X0`: `/plugin install` cannot
    /// run inside `execute` (it needs `env`/`cwd`, neither of which `Host`
    /// carries, and a network fetch `execute`'s own dependency graph keeps
    /// out of reach) -- it must hand back `Effect::RunMarketplaceInstall`
    /// with both fields forwarded verbatim, no facade call and no
    /// `AppState` mutation of its own (unlike bare `/plugin` above).
    #[tokio::test]
    async fn plugin_install_hands_back_the_run_effect_untouched() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Plugins {
                action: Some(PluginsAction::Install {
                    marketplace_url: "https://example.com/marketplace.json".to_string(),
                    plugin_id: "acme-tools".to_string(),
                }),
            },
            &mut state,
            &host,
        )
        .await;

        match effect {
            Effect::RunMarketplaceInstall {
                marketplace_url,
                plugin_id,
            } => {
                assert_eq!(marketplace_url, "https://example.com/marketplace.json");
                assert_eq!(plugin_id, "acme-tools");
            }
            _ => panic!("expected Effect::RunMarketplaceInstall"),
        }
        assert!(!state.plugins_open, "an install must not open the listing");
        assert!(
            host.calls().is_empty(),
            "no facade call -- the actual fetch is the caller's job (Effect::RunMarketplaceInstall's own doc)"
        );
    }

    /// Mirrors `plugin_install_hands_back_the_run_effect_untouched` for
    /// `/plugin uninstall`.
    #[tokio::test]
    async fn plugin_uninstall_hands_back_the_run_effect_untouched() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Plugins {
                action: Some(PluginsAction::Uninstall {
                    plugin_id: "acme-tools".to_string(),
                }),
            },
            &mut state,
            &host,
        )
        .await;

        match effect {
            Effect::RunMarketplaceUninstall { plugin_id } => {
                assert_eq!(plugin_id, "acme-tools");
            }
            _ => panic!("expected Effect::RunMarketplaceUninstall"),
        }
        assert!(host.calls().is_empty(), "no facade call at all");
    }

    // ---------------------------------------------------------------
    // /trust, /agents, /ask -- the discriminating observable this item
    // exists to prove: each reaches its handler THROUGH `execute`, driven
    // off `SlashCommand`, not a pre-parser string comparison.
    // ---------------------------------------------------------------

    /// The headline property this item exists to prove: `/trust
    /// permissions` opens the preview card FIRST, showing the file's
    /// current content and status -- it must NOT call
    /// `trust_permission_file` (which would both install and trust in the
    /// same action) until an explicit confirm.
    #[tokio::test]
    async fn trust_opens_a_preview_card_before_trusting_anything() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let path = std::path::PathBuf::from("/tmp/permissions.json");
        state.permission_paths = vec![path.clone()];
        let host = FakeHost::new(root).with_preview_result(TrustPreview {
            contents: r#"{"allow":["bash:cargo test"]}"#.to_string(),
            status: conway::TrustStatus::New,
        });

        let effect = execute(SlashCommand::Trust, &mut state, &host).await;

        assert!(matches!(effect, Effect::None));
        assert_eq!(
            host.calls(),
            vec!["preview_trust_target"],
            "must read and show the content, and must NOT trust anything yet"
        );
        match &state.mode {
            Mode::TrustPreview(card) => {
                assert_eq!(card.path, path);
                assert_eq!(card.contents, r#"{"allow":["bash:cargo test"]}"#);
                assert_eq!(card.status, conway::TrustStatus::New);
                assert!(card.error.is_none());
            }
            other => panic!("expected Mode::TrustPreview, got {other:?}"),
        }
    }

    /// Confirming the open card is what actually calls
    /// `trust_permission_file` and records the installed count -- the SAME
    /// facade call and message shape `/trust permissions` used to produce
    /// immediately, before this item added the preview step.
    #[tokio::test]
    async fn confirming_the_trust_preview_installs_rules_and_records_the_installed_count() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let path = std::path::PathBuf::from("/tmp/permissions.json");
        state.offer_trust_preview(TrustPreviewCard {
            path: path.clone(),
            contents: "{}".to_string(),
            status: conway::TrustStatus::New,
            error: None,
        });
        let host = FakeHost::new(root).with_trust_result(TrustPermissionReport {
            installed: 3,
            registration_errors: Vec::new(),
            notices: Vec::new(),
        });

        apply_trust_decision(TrustDecision::Confirm, &mut state, &host).await;

        assert_eq!(host.calls(), vec!["trust_permission_file"]);
        assert!(
            matches!(state.mode, Mode::Normal),
            "a successful confirm must close the card"
        );
        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("trusted") && text.contains("3 allow rule")
            )),
            "the installed count must be surfaced: {:?}",
            state.transcript
        );
    }

    /// Cancelling the open card makes NO facade call at all -- there is
    /// nothing to undo when nothing was ever written.
    #[tokio::test]
    async fn cancelling_the_trust_preview_makes_no_facade_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let path = std::path::PathBuf::from("/tmp/permissions.json");
        state.offer_trust_preview(TrustPreviewCard {
            path: path.clone(),
            contents: "{}".to_string(),
            status: conway::TrustStatus::New,
            error: None,
        });
        let host = FakeHost::new(root);

        apply_trust_decision(TrustDecision::Cancel, &mut state, &host).await;

        assert!(
            host.calls().is_empty(),
            "cancelling must never call trust_permission_file"
        );
        assert!(
            matches!(state.mode, Mode::Normal),
            "cancel must close the card"
        );
        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("not trusted")
            )),
            "{:?}",
            state.transcript
        );
    }

    /// Nothing configured to trust: a plain notice, and -- the point of
    /// this test -- NO facade call at all (there is nothing to pass it).
    #[tokio::test]
    async fn trust_with_no_permission_paths_configured_is_a_notice_with_no_facade_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root); // permission_paths starts empty
        let host = FakeHost::new(root);

        let effect = execute(SlashCommand::Trust, &mut state, &host).await;

        assert!(matches!(effect, Effect::None));
        assert!(
            host.calls().is_empty(),
            "nothing to trust -- preview_trust_target must never be called"
        );
        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("no project permissions file")
            )),
            "{:?}",
            state.transcript
        );
    }

    /// A preview read failure is promoted to `Entry::Error`, never
    /// `Entry::Notice`, and opens no card -- `/trust permissions` is an
    /// explicit operator action, so a failure to even show what would be
    /// trusted must never be camouflaged as a routine notice.
    #[tokio::test]
    async fn trust_preview_read_failure_is_an_error_entry_not_a_notice() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.permission_paths = vec![std::path::PathBuf::from("/tmp/permissions.json")];
        let host = FakeHost::new(root); // preview_result: None -> fake io::Error

        let effect = execute(SlashCommand::Trust, &mut state, &host).await;

        assert!(matches!(effect, Effect::None));
        assert!(
            matches!(state.mode, Mode::Normal),
            "a read failure must not open a card"
        );
        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, fatal: false } if text.contains("could not read")
            )),
            "a facade failure must be an Entry::Error, not a Notice: {:?}",
            state.transcript
        );
    }

    /// A facade failure on CONFIRM keeps the card open with the error shown
    /// (mirroring `apply_ask_fate`'s own failure path) rather than silently
    /// falling through to "cancelled" -- `/trust permissions` is an
    /// explicit operator action, so the operator's belief ("I just
    /// confirmed trusting this file") diverging from reality ("nothing was
    /// recorded") must never be camouflaged.
    #[tokio::test]
    async fn confirm_facade_failure_keeps_the_card_open_with_the_error_shown() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let path = std::path::PathBuf::from("/tmp/permissions.json");
        state.offer_trust_preview(TrustPreviewCard {
            path: path.clone(),
            contents: "{}".to_string(),
            status: conway::TrustStatus::New,
            error: None,
        });
        let host = FakeHost::new(root); // trust_result: None -> fake io::Error

        apply_trust_decision(TrustDecision::Confirm, &mut state, &host).await;

        match &state.mode {
            Mode::TrustPreview(card) => {
                assert!(
                    card.error
                        .as_deref()
                        .is_some_and(|e| e.contains("could not trust")),
                    "expected an in-card error, got {:?}",
                    card.error
                );
            }
            other => panic!("a failed confirm must keep the card open, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn agents_toggles_the_tree_view_with_no_facade_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);
        assert!(!state.agent_view_open);

        let effect = execute(SlashCommand::Agents, &mut state, &host).await;

        assert!(state.agent_view_open, "/agents must open the tree view");
        assert!(matches!(effect, Effect::None));
        assert!(
            host.calls().is_empty(),
            "no facade call at all -- a pure state flip, mirroring /settings"
        );
    }

    /// The discriminating observable for `/ask`: `execute` sets
    /// `ask_in_flight` and hands the caller `Effect::RunModalAsk` to spawn
    /// -- it never spawns anything itself (see that effect's own doc).
    #[tokio::test]
    async fn ask_sets_ask_in_flight_and_returns_run_modal_ask() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);
        assert!(!state.ask_in_flight);

        let effect = execute(
            SlashCommand::Ask {
                question: "is this safe?".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(state.ask_in_flight);
        match effect {
            Effect::RunModalAsk { question } => assert_eq!(question, "is this safe?"),
            _ => panic!("expected Effect::RunModalAsk, got a different effect"),
        }
        assert!(
            host.calls().is_empty(),
            "no facade call -- forking the child is the CALLER's job (see Effect::RunModalAsk)"
        );
    }

    /// B5: one ask at a time -- a second `/ask` while one is already in
    /// flight is a notice, never a second `Effect::RunModalAsk`.
    #[tokio::test]
    async fn ask_while_already_in_flight_is_a_notice_with_no_effect() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.ask_in_flight = true;
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Ask {
                question: "another one?".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None));
        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("already running")
            )),
            "{:?}",
            state.transcript
        );
    }

    #[tokio::test]
    async fn context_maps_to_exactly_one_context_report_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Context {
                agent: Some(root.to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["context_report"]);
    }

    /// Acceptance 1 (`01M0RWKJD04JBR5NCVKBQXYHV4`): a bare `/context`
    /// resolves against `AppState::focused_agent`, not `root` and not a
    /// usage error -- `focused_agent` is deliberately switched away from
    /// `root` first so a bug that silently defaulted to root/the session
    /// origin instead of the FOCUSED agent would fail this.
    #[tokio::test]
    async fn context_with_no_agent_uses_the_focused_agent() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);
        assert_ne!(
            state.focused_agent, root,
            "precondition: focus moved off root"
        );
        let host = FakeHost::new(root);

        let effect = execute(SlashCommand::Context { agent: None }, &mut state, &host).await;

        assert!(matches!(effect, Effect::None));
        assert_eq!(host.calls(), vec!["context_report"]);
        assert_eq!(
            host.last_context_agent(),
            Some(child),
            "a bare /context must resolve to the FOCUSED agent, not root"
        );
        assert!(
            !state.transcript.iter().any(
                |e| matches!(e, Entry::Notice { text } if text.to_lowercase().contains("usage"))
            ),
            "a bare /context must not surface a usage error: {:?}",
            state.transcript
        );
    }

    #[tokio::test]
    async fn fork_at_agent_maps_to_exactly_one_fork_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Fork {
                agent: Some(root.to_string()),
                directive: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["fork"]);
        // The explicit-target `/fork @<agent> <directive>` arm is the
        // pre-existing AUTONOMOUS (non-keep-alive) fork -- unlike the bare
        // fork/spawn arms, it must keep the default toolset (`report`
        // included), exactly like a `conway_fork`/`conway_spawn`-started
        // child does.
        let spec = host
            .last_fork_spec
            .lock()
            .unwrap()
            .clone()
            .expect("fork should have been called");
        assert_eq!(
            spec.tools, None,
            "an explicit-target autonomous fork must keep the default toolset"
        );
    }

    #[tokio::test]
    async fn spawn_maps_to_exactly_one_spawn_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Spawn {
                agent_def: Some("reviewer".to_string()),
                prompt: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["spawn"]);
    }

    #[tokio::test]
    async fn spawn_without_agent_def_still_maps_to_exactly_one_spawn_call() {
        // C2: free-text `/spawn` (no `@agent_def`) now classifies first;
        // the default FakeHost's classify fails (`IntentClassification`),
        // so the manual fallback runs and `spawn` is still called exactly
        // once -- the pre-C2 assertion holds with one added classify call.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("review the diff".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(
            host.calls(),
            vec!["classify_agent_intent", "spawn"],
            "free-text /spawn classifies first, then falls back to spawn on error"
        );
    }

    #[tokio::test]
    async fn bare_spawn_builds_a_keep_alive_empty_prompt_spec_and_returns_focus_new_session() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.spawn_child = Some(child);

        let effect = execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: None,
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["spawn"]);
        match effect {
            Effect::FocusNewSession {
                child: focused,
                parent,
                first_message,
            } => {
                assert_eq!(focused, child);
                assert_eq!(parent, root, "a bare spawn attaches the child under root");
                assert_eq!(first_message, None);
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
        let spec = host
            .last_spawn_spec
            .lock()
            .unwrap()
            .clone()
            .expect("spawn should have been called");
        assert!(spec.keep_alive, "a bare spawn must be keep_alive");
        assert_eq!(spec.prompt, "", "the SpawnSpec's own prompt must be empty");
        assert_eq!(
            spec.tools,
            Some(ToolSelector::Except(vec!["report".into()])),
            "a bare, interactive keep-alive spawn must exclude `report`"
        );
    }

    #[tokio::test]
    async fn spawn_with_text_carries_the_text_as_the_effects_first_message_not_the_spec_prompt() {
        // C2: classify fails (default FakeHost) -> manual fallback -> the
        // raw text ("hello there") becomes the first message, exactly as
        // before C2. The first message is still NOT baked into the
        // SpawnSpec (the child idles until the app loop delivers it).
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.spawn_child = Some(child);

        let effect = execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: Some("hello there".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        match effect {
            Effect::FocusNewSession {
                child: focused,
                parent,
                first_message,
            } => {
                assert_eq!(focused, child);
                assert_eq!(parent, root, "a bare spawn attaches the child under root");
                assert_eq!(first_message, Some("hello there".to_string()));
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
        let spec = host
            .last_spawn_spec
            .lock()
            .unwrap()
            .clone()
            .expect("spawn should have been called");
        assert_eq!(
            spec.prompt, "",
            "the first message must not be baked into the SpawnSpec"
        );
    }

    #[tokio::test]
    async fn bare_fork_builds_a_keep_alive_empty_directive_spec_targeting_the_focused_agent() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child_focus = AgentId::new();
        state.focus_agent(child_focus);
        let mut host = FakeHost::new(root);
        let grandchild = AgentId::new();
        host.fork_child = Some(grandchild);

        let effect = execute(
            SlashCommand::Fork {
                agent: None,
                directive: None,
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["fork"]);
        match effect {
            Effect::FocusNewSession {
                child,
                parent,
                first_message,
            } => {
                assert_eq!(child, grandchild);
                assert_eq!(
                    parent, child_focus,
                    "a bare fork attaches the child under the focused agent"
                );
                assert_eq!(first_message, None);
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
        let spec = host
            .last_fork_spec
            .lock()
            .unwrap()
            .clone()
            .expect("fork should have been called");
        assert!(spec.keep_alive, "a bare fork must be keep_alive");
        assert_eq!(
            spec.directive, "",
            "the ForkSpec's own directive must be empty"
        );
        assert_eq!(
            spec.tools,
            Some(ToolSelector::Except(vec!["report".into()])),
            "a bare, interactive keep-alive fork must exclude `report`"
        );
    }

    #[tokio::test]
    async fn fork_with_text_carries_the_text_as_the_effects_first_message() {
        // C2: classify fails (default FakeHost) -> manual fallback -> the
        // raw text ("please review") becomes the first message, exactly as
        // before C2.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.fork_child = Some(child);

        let effect = execute(
            SlashCommand::Fork {
                agent: None,
                directive: Some("please review".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        match effect {
            Effect::FocusNewSession {
                child: focused,
                parent,
                first_message,
            } => {
                assert_eq!(focused, child);
                assert_eq!(
                    parent, root,
                    "the focused agent (root here) is the fork parent"
                );
                assert_eq!(first_message, Some("please review".to_string()));
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
    }

    #[tokio::test]
    async fn resume_maps_to_exactly_one_resume_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Resume {
                sid: SessionId::new().to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None), "the fake resume errors");
        assert_eq!(host.calls(), vec!["resume"]);
    }

    #[tokio::test]
    async fn model_forks_the_focused_agent_with_a_pinned_model_and_focuses_the_child() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child_focus = AgentId::new();
        state.focus_agent(child_focus);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.fork_child = Some(child);

        let effect = execute(
            SlashCommand::Model {
                model: Some("anthropic/claude-haiku".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["fork"]);
        match effect {
            Effect::FocusNewSession {
                child: focused,
                parent,
                first_message,
            } => {
                assert_eq!(focused, child);
                assert_eq!(
                    parent, child_focus,
                    "/model forks the FOCUSED agent, not the root"
                );
                assert_eq!(first_message, None, "/model never carries a first message");
            }
            _ => panic!("expected Effect::FocusNewSession, got a different effect"),
        }
        let spec = host
            .last_fork_spec
            .lock()
            .unwrap()
            .clone()
            .expect("fork should have been called");
        assert!(spec.keep_alive, "/model's fork must be keep_alive");
        assert_eq!(spec.directive, "", "/model carries no directive of its own");
        assert_eq!(
            spec.model,
            Some(
                "anthropic/claude-haiku"
                    .parse::<ModelRef>()
                    .expect("valid model ref")
            )
        );
        assert_eq!(
            spec.tools,
            Some(ToolSelector::Except(vec!["report".into()])),
            "/model's fork, like a bare /fork, must exclude `report`"
        );
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text })
                if text.contains("anthropic/claude-haiku") && text.contains(&child_focus.to_string())
        ));
    }

    #[tokio::test]
    async fn model_with_a_malformed_ref_is_a_notice_and_never_calls_fork() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(
            SlashCommand::Model {
                model: Some("not-a-valid-ref".to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(effect, Effect::None));
        assert!(
            host.calls().is_empty(),
            "a malformed --model must never reach fork"
        );
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text.contains("not-a-valid-ref")
        ));
    }

    fn ui_installed_browser() -> Vec<PluginBrowserEntry> {
        vec![PluginBrowserEntry {
            id: "conway.ui".to_string(),
            version: "0.0.0".to_string(),
            installed: true,
            description: conway::plugin::PluginDescription::default(),
        }]
    }

    /// **VERIFICATION ANCHOR, board item `01M1A35S609TZ613GAECPEHX8D`
    /// acceptance 4.** Bare `/model` with `conway.ui` ABSENT (the default --
    /// `state.plugin_browser` empty) lists the configured pairs as plain
    /// transcript text, marking the focused agent's own model `(active)` --
    /// never a facade call, since nothing is being switched yet.
    #[tokio::test]
    async fn model_bare_lists_configured_pairs_and_marks_the_active_one() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.configured_models = vec![
            "anthropic/claude-haiku".to_string(),
            "anthropic/claude-sonnet-4-6".to_string(),
        ];
        state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());
        let host = FakeHost::new(root);

        let effect = execute(SlashCommand::Model { model: None }, &mut state, &host).await;

        assert!(matches!(effect, Effect::None));
        assert!(
            host.calls().is_empty(),
            "listing what's configured must never touch the facade"
        );
        assert!(
            !matches!(state.mode, Mode::UiForm(_)),
            "with conway.ui absent this must be plain text, never a menu"
        );
        match state.transcript.last() {
            Some(Entry::Notice { text }) => {
                assert!(text.contains("anthropic/claude-haiku"), "{text}");
                let active_lines: Vec<&str> =
                    text.lines().filter(|l| l.contains("(active)")).collect();
                assert_eq!(
                    active_lines.len(),
                    1,
                    "exactly the focused agent's own model must be marked: {text}"
                );
                assert!(
                    active_lines[0].contains("anthropic/claude-sonnet-4-6"),
                    "{text}"
                );
            }
            other => panic!("expected a Notice listing, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_bare_with_nothing_configured_is_a_named_notice() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let effect = execute(SlashCommand::Model { model: None }, &mut state, &host).await;

        assert!(matches!(effect, Effect::None));
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text.contains("no models are configured")
        ));
    }

    /// **VERIFICATION ANCHOR, board item `01M1A35S609TZ613GAECPEHX8D`
    /// acceptance 5 (menu half).** With `conway.ui` installed, bare
    /// `/model` opens `Mode::UiForm` -- the SAME surface a model-called
    /// `ask_question` opens -- rather than printing text, and marks
    /// `AppState::model_picker_active` so `run.rs`'s dispatch arm knows to
    /// run the switch once answered.
    #[tokio::test]
    async fn model_bare_opens_a_menu_when_conway_ui_is_installed() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.configured_models = vec![
            "anthropic/claude-haiku".to_string(),
            "openai/gpt-5".to_string(),
        ];
        state.plugin_browser = ui_installed_browser();
        let host = FakeHost::new(root);

        let effect = execute(SlashCommand::Model { model: None }, &mut state, &host).await;

        assert!(matches!(effect, Effect::None));
        assert!(
            host.calls().is_empty(),
            "opening the menu makes no facade call yet"
        );
        assert!(
            state.model_picker_active,
            "the picker flag must be set so answering it runs the switch"
        );
        match &state.mode {
            Mode::UiForm(form) => {
                assert_eq!(form.ask.request.prompt, "select a model");
                assert_eq!(
                    form.ask.request.options,
                    vec![
                        "anthropic/claude-haiku".to_string(),
                        "openai/gpt-5".to_string()
                    ]
                );
            }
            other => panic!("expected Mode::UiForm, got {other:?}"),
        }
    }

    /// **VERIFICATION ANCHOR, board item `01M1A35S609TZ613GAECPEHX8D`
    /// acceptance 4.** "A pair shown is accepted verbatim by `/model
    /// <pair>`" -- takes a string straight out of `AppState::
    /// configured_models` (exactly what the bare listing shows) and feeds
    /// it back through `parse` + `execute` as a normal `/model <pair>`
    /// invocation.
    #[tokio::test]
    async fn a_configured_pair_is_accepted_verbatim_by_model_with_an_argument() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.configured_models = vec!["anthropic/claude-haiku".to_string()];
        let mut host = FakeHost::new(root);
        host.fork_child = Some(AgentId::new());

        let pair = state.configured_models[0].clone();
        let cmd = parse(&format!("/model {pair}")).expect("a listed pair must parse");

        let effect = execute(cmd, &mut state, &host).await;

        assert!(
            matches!(effect, Effect::FocusNewSession { .. }),
            "a pair straight from the listing ({pair}) must be accepted verbatim \
             by `/model <arg>` -- the round trip that makes the bare listing \
             useful. `Effect` deliberately carries no `Debug` (it holds a live \
             `SessionHandle`), so this reports the input, not the effect."
        );
    }

    #[tokio::test]
    async fn role_forks_the_focused_agent_with_the_named_role_and_focuses_the_child() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.fork_child = Some(child);

        let effect = execute(
            SlashCommand::Role {
                role: "planner".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert_eq!(host.calls(), vec!["fork"]);
        assert!(matches!(effect, Effect::FocusNewSession { child: c, .. } if c == child));
        let spec = host
            .last_fork_spec
            .lock()
            .unwrap()
            .clone()
            .expect("fork should have been called");
        assert!(spec.keep_alive, "/role's fork must be keep_alive");
        assert_eq!(spec.role, Some(RoleAlias::new("planner")));
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text.contains("planner")
        ));
    }

    #[tokio::test]
    async fn why_makes_no_facade_call() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(SlashCommand::Why, &mut state, &host).await;

        assert!(host.calls().is_empty(), "expected no facade call for /why");
    }

    #[tokio::test]
    async fn why_before_any_model_decision_renders_no_routing_decision_yet() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        execute(SlashCommand::Why, &mut state, &host).await;

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text == "no routing decision yet"
        ));
    }

    #[tokio::test]
    async fn why_after_a_model_decision_renders_its_fields() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let chosen = conway::ModelRef {
            backend: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
        };
        state.last_model_decision = Some(conway::Envelope {
            seq: 1,
            ts: chrono::Utc::now(),
            session: SessionId::new(),
            agent: root,
            event: Event::ModelDecision {
                role: "planner".into(),
                chosen: chosen.clone(),
                reason: RoutingReason::PinnedByApi,
                attempt: 1,
            },
        });

        execute(SlashCommand::Why, &mut state, &host).await;

        let texts: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Notice { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("planner")));
        assert!(texts.iter().any(|t| t.contains(&chosen.to_string())));
        assert!(texts.iter().any(|t| t.contains("pinned by API")));
        assert!(texts.iter().any(|t| t.contains('1')));
    }

    /// A `/model`/`/role` switch (or an ordinary router fallback) shows up
    /// in `/why` as an `X -> Y` diff on whichever field changed -- see
    /// `render_why`'s own doc for why this needs `previous_model_decision`
    /// at all (a session's SECOND `ModelDecision`, not its first).
    #[tokio::test]
    async fn why_after_a_model_switch_shows_what_changed() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let host = FakeHost::new(root);

        let old_model = conway::ModelRef {
            backend: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
        };
        let new_model = conway::ModelRef {
            backend: "anthropic".into(),
            model: "claude-haiku".into(),
        };
        state.previous_model_decision = Some(conway::Envelope {
            seq: 1,
            ts: chrono::Utc::now(),
            session: SessionId::new(),
            agent: root,
            event: Event::ModelDecision {
                role: "planner".into(),
                chosen: old_model,
                reason: RoutingReason::PinnedByApi,
                attempt: 1,
            },
        });
        state.last_model_decision = Some(conway::Envelope {
            seq: 2,
            ts: chrono::Utc::now(),
            session: SessionId::new(),
            agent: root,
            event: Event::ModelDecision {
                role: "planner".into(),
                chosen: new_model.clone(),
                reason: RoutingReason::PinnedByApi,
                attempt: 1,
            },
        });

        execute(SlashCommand::Why, &mut state, &host).await;

        let texts: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Notice { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("claude-sonnet-4-6") && t.contains("claude-haiku")),
            "expected an `old -> new` model diff, got: {texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|t| t.starts_with("role:") && t.contains("->")),
            "role did not change and must render bare"
        );
    }

    #[tokio::test]
    async fn unknown_slash_command_never_reaches_the_model() {
        // The app-level guarantee this criterion is about lives in
        // `app.rs::submit` (parse fails before `execute` is ever called) --
        // this test locks down the piece owned here: `parse` rejects an
        // unknown command rather than silently accepting it as a `SlashCommand`
        // some `execute` arm would forward as a prompt.
        assert!(parse("/nope").is_err());
    }

    #[tokio::test]
    async fn context_renders_one_line_per_segment() {
        let root = AgentId::new();
        let seg0 = SegmentId::new();
        let seg1 = SegmentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.context = Some(ContextReport {
            agent_id: root,
            turn: 1,
            tokenizer: "heuristic-chars4".to_string(),
            segments: vec![
                ContextReportEntry {
                    segment: seg0,
                    provenance: Provenance::UserPrompt,
                    tokens_est: 12,
                    estimated: true,
                },
                ContextReportEntry {
                    segment: seg1,
                    provenance: Provenance::AgentDef {
                        name: "reviewer".to_string(),
                    },
                    tokens_est: 40,
                    estimated: true,
                },
            ],
            total_tokens_est: 52,
            dropped: Vec::new(),
            curator_failed: None,
            instruction_fragments: Vec::new(),
        });

        execute(
            SlashCommand::Context {
                agent: Some(root.to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        let lines: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Notice { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 2, "expected one line per segment");
        // Each line must carry the segment id, a provenance label, and the
        // token estimate -- not just be present.
        assert!(
            lines[0].contains(&seg0.to_string())
                && lines[0].contains("user prompt")
                && lines[0].contains("12tok"),
            "line 0 missing id/provenance/tokens: {:?}",
            lines[0]
        );
        assert!(
            lines[1].contains(&seg1.to_string())
                && lines[1].contains("agent def `reviewer`")
                && lines[1].contains("40tok"),
            "line 1 missing id/provenance/tokens: {:?}",
            lines[1]
        );
    }

    /// Board item `01M0K5MD59YZRSHE31JKZKFRMY`: `/context` renders a
    /// "preamble" section from `report.instruction_fragments`, ahead of
    /// the ordinary per-segment lines -- carrying the (plugin_id, name)
    /// source attribution the per-segment listing alone cannot express.
    #[tokio::test]
    async fn context_renders_a_preamble_section_with_plugin_source() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.context = Some(ContextReport {
            agent_id: root,
            turn: 1,
            tokenizer: "heuristic-chars4".to_string(),
            segments: Vec::new(),
            total_tokens_est: 0,
            dropped: Vec::new(),
            curator_failed: None,
            instruction_fragments: vec![InstructionFragmentEntry {
                plugin_id: "conway.trim".to_string(),
                name: "when-to-compose".to_string(),
                tokens_est: 7,
                unreachable_tool_ids: Vec::new(),
            }],
        });

        execute(
            SlashCommand::Context {
                agent: Some(root.to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        let lines: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Notice { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            lines[0].contains("preamble") && lines[0].contains("1 plugin-declared fragment"),
            "expected a preamble header line, got: {:?}",
            lines
        );
        assert!(
            lines[1].contains("conway.trim.when-to-compose")
                && lines[1].contains("7tok")
                && lines[1].contains("conway.trim"),
            "expected the fragment's name, size, and plugin source, got: {:?}",
            lines[1]
        );
    }

    /// The reachability failure "renders inline" (decision
    /// `01M0K5K8DCRVR523P54DZF4BY3`): an unreachable fragment's preamble
    /// line names the missing tool id rather than silently vanishing.
    #[tokio::test]
    async fn context_renders_an_unreachable_fragment_inline() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.context = Some(ContextReport {
            agent_id: root,
            turn: 1,
            tokenizer: "heuristic-chars4".to_string(),
            segments: Vec::new(),
            total_tokens_est: 0,
            dropped: Vec::new(),
            curator_failed: None,
            instruction_fragments: vec![InstructionFragmentEntry {
                plugin_id: "conway.trim".to_string(),
                name: "when-to-compose".to_string(),
                tokens_est: 7,
                unreachable_tool_ids: vec![ToolName::new("compose_path")],
            }],
        });

        execute(
            SlashCommand::Context {
                agent: Some(root.to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        let lines: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Notice { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            lines[1].contains("compose_path") && lines[1].contains("not installed"),
            "expected the missing tool named inline, got: {:?}",
            lines[1]
        );
    }

    #[tokio::test]
    async fn context_with_zero_segments_renders_an_explicit_empty_line() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        host.context = Some(ContextReport {
            agent_id: root,
            turn: 1,
            tokenizer: "heuristic-chars4".to_string(),
            segments: Vec::new(),
            total_tokens_est: 0,
            dropped: Vec::new(),
            curator_failed: None,
            instruction_fragments: Vec::new(),
        });

        execute(
            SlashCommand::Context {
                agent: Some(root.to_string()),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text == "empty context"
        ));
    }

    #[tokio::test]
    async fn ambiguous_agent_prefix_is_reported_and_does_not_call_the_facade() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let session = SessionId::new();

        // `b` is `a`'s own string with only its last character swapped for a
        // different valid Crockford char -- guarantees a 25-char shared
        // prefix between two distinct, valid agent ids deterministically
        // (no dependence on `AgentId::new()`'s timing-derived value).
        let a = AgentId::new();
        let a_str = a.to_string();
        let last = a_str.chars().next_back().expect("ULID string is non-empty");
        let alt = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
            .chars()
            .find(|&c| c != last)
            .expect("Crockford alphabet has more than one symbol");
        let mut b_str = a_str.clone();
        b_str.pop();
        b_str.push(alt);
        let b: AgentId = b_str
            .parse()
            .expect("swapping the last char keeps it a valid ULID");
        let shared_prefix = &a_str[..a_str.len() - 1];

        // Populate `state.tree` the same way `state.rs`'s own tests do
        // (`AppState::apply`) -- this module has no access to
        // `AgentTreeView`'s private `insert` and is out of scope to widen
        // `state.rs` for one.
        for child in [a, b] {
            state.apply(&conway::Envelope {
                seq: 1,
                ts: chrono::Utc::now(),
                session,
                agent: child,
                event: Event::AgentSpawned {
                    kind: SubagentMode::Spawn,
                    parent: Some(root),
                    agent_def: None,
                    inherited_upto: None,
                    ephemeral: false,
                },
            });
        }
        let host = FakeHost::new(root);

        execute(
            SlashCommand::Steer {
                target: shared_prefix.to_string(),
                text: "hi".to_string(),
            },
            &mut state,
            &host,
        )
        .await;

        assert!(
            host.calls().is_empty(),
            "ambiguous prefix must not reach the facade"
        );
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text.contains("ambiguous")
        ));
    }

    /// Board item `01M0RWKJD04JBR5NCVKBQXYHV4`, VERIFICATION ANCHOR
    /// (acceptance 3) + acceptance 4. The identifier fed to `resolve_agent`
    /// is DERIVED from the real render pass's own output (`view::agents`'s
    /// row, through the exact `view::draw` a running TUI calls), not
    /// reconstructed independently via `short_agent_id` -- a bug that
    /// rendered one id but accepted only a different one would fail this,
    /// which a test computing both sides from the same call cannot show.
    ///
    /// The fixture is deliberately NOT vacuous for acceptance 4: `root` and
    /// `child` share the exact SAME `agent_def` (`None`), so both rows
    /// render the literal label `"agent"` -- identical text, the case the
    /// item's own spec calls out as visually indistinguishable today. If
    /// the short id were missing, or identical for both rows, the
    /// `assert_ne!` below fails rather than passing by construction.
    #[test]
    fn agents_panel_short_id_is_what_resolve_agent_accepts_copied_off_the_row() {
        // Deliberately NOT `AgentId::new()` for both: two ids generated
        // back-to-back in the same test very often land in the same
        // 1024ms ULID timestamp bucket and so SHARE their first 8
        // characters (see `short_id_prefix_collision_...` below and
        // `short_agent_id`'s own doc) -- this test hit exactly that
        // collision, non-deterministically, when it first used
        // `AgentId::new()` for both. Fixed, non-colliding literal ULIDs
        // make this test's own pass/fail about the loop this item exists
        // to close, not about ULID clock timing.
        let root: AgentId = "01HF7YAT000000000000000001"
            .parse()
            .expect("valid ULID string");
        let mut state = AppState::new(root);
        let child: AgentId = "01J000000000000000000000A2"
            .parse()
            .expect("valid ULID string");
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: None, // same as root's -- both rows read "agent"
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.focus_agent(child);
        state.agent_view_open = true;

        let rows = crate::tui::test_support::render(&state, RENDER_WIDTH, 24);
        let header_idx = rows
            .iter()
            .position(|r| r.contains("agents ("))
            .expect("the /agents panel header must render");
        // A bordered `List`'s content starts the row right after its own
        // top-border/title row; `root` was inserted first (`AppState::new`)
        // and `visible_agent_nodes` does not reorder, so it is the first
        // content row, `child` the second.
        let root_row = &rows[header_idx + 1];
        let child_row = &rows[header_idx + 2];

        assert!(
            root_row.contains("agent") && child_row.contains("agent"),
            "precondition: both rows render the same label: {root_row:?} / {child_row:?}"
        );

        // The row shape (`view/agents.rs::draw`) is `<indent><marker>
        // <short_id> <label>...`, drawn onto the `List` widget's own left
        // border ('\u{2502}') with no space of its own -- at zero indent
        // (root) the marker sits flush against the border, fusing into one
        // `split_whitespace` token; at any deeper indent (child) the
        // indent's spaces separate them again. Stripping the border
        // character (and the leading indent, itself just whitespace)
        // BEFORE tokenizing makes every row line up the same way,
        // regardless of depth -- exactly what a person selecting the
        // visible short id off the screen does without even noticing the
        // border is there.
        fn short_id_token(row: &str) -> &str {
            row.trim_start_matches(|c: char| c == '\u{2502}' || c.is_whitespace())
                .split_whitespace()
                .nth(1)
                .expect("row must carry a short id token after the marker")
        }
        let root_token = short_id_token(root_row);
        let child_token = short_id_token(child_row);

        assert_ne!(
            root_token, child_token,
            "two same-def agents must be distinguishable once the short id is shown: \
             {root_row:?} / {child_row:?}"
        );

        let resolved_root = resolve_agent(&state, root_token)
            .unwrap_or_else(|e| panic!("root's own rendered short id must resolve: {e}"));
        let resolved_child = resolve_agent(&state, child_token)
            .unwrap_or_else(|e| panic!("child's own rendered short id must resolve: {e}"));

        assert_eq!(
            resolved_root, root,
            "copying root's row must resolve to root"
        );
        assert_eq!(
            resolved_child, child,
            "copying child's row must resolve to child"
        );
    }

    /// "Determine before building" question 2: an 8-char short id is NOT
    /// guaranteed unique -- verified, not assumed. A ULID's first 8
    /// Crockford-base32 characters (5 bits each) encode only the top 40 of
    /// its 48-bit millisecond timestamp, so two agents created with the
    /// same timestamp (a common case: e.g. a parent and a child it spawns
    /// in the same tick) can share one no matter how their remaining 18
    /// characters -- pure randomness -- differ. `a`/`b` below share their
    /// timestamp (`01HF7YAT`, an arbitrary real ULID timestamp field) and
    /// differ everywhere else, reproducing exactly that case -- not the
    /// pre-existing `ambiguous_agent_prefix...` test's "swap only the last
    /// character" construction, which would also collide on a MUCH longer
    /// shared prefix and so would not isolate the 8-char case specifically.
    ///
    /// The fix is not a new resolver rule: `resolve_agent`'s existing
    /// prefix-ambiguity handling already covers this correctly -- an error
    /// naming every candidate, never a silent wrong pick. This test is the
    /// verification that claim actually holds for an 8-char token
    /// specifically, not just for the longer prefixes the existing test
    /// exercises.
    #[test]
    fn short_id_prefix_collision_is_reported_as_ambiguous_not_resolved_to_the_wrong_agent() {
        let a: AgentId = "01HF7YAT0123456789ABCDEFGH"
            .parse()
            .expect("valid ULID string");
        let b: AgentId = "01HF7YATZZZZZZZZZZZZZZZZZZ"
            .parse()
            .expect("valid ULID string");
        let shared_short_id = &a.to_string()[..8];
        assert_eq!(
            shared_short_id,
            &b.to_string()[..8],
            "precondition: a and b share their 8-char short id"
        );
        assert_ne!(a, b, "precondition: a and b are distinct full ids");

        let root = AgentId::new();
        let mut state = AppState::new(root);
        for id in [a, b] {
            state.tree.nodes.push(TreeNode {
                agent_id: id,
                parent: Some(root),
                agent_def: None,
                status: NodeStatus::Running,
                kind: None,
                inherited_upto: None,
                ephemeral: false,
            });
        }

        let err = resolve_agent(&state, shared_short_id)
            .expect_err("a colliding short id must not silently resolve to either agent");
        assert!(
            err.contains("ambiguous") && err.contains(&a.to_string()) && err.contains(&b.to_string()),
            "the ambiguity error must name both real candidates, not just the shared prefix: {err:?}"
        );
    }

    // ---------------------------------------------------------------
    // Render/state: after the `Effect::FocusNewSession` an app loop would
    // handle by focusing `child` (`app.rs::try_focus_agent`, thin over
    // `AppState::focus_agent` -- reused, not duplicated, here since this
    // module has no live facade to drive the REAL `agent_events` resubscribe
    // that `try_focus_agent` also performs), the focused agent really is the
    // new child, through the REAL render pass (`crate::tui::test_support`).
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn after_a_bare_spawns_focus_new_session_effect_the_focused_agent_is_the_new_child() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.spawn_child = Some(child);

        let effect = execute(
            SlashCommand::Spawn {
                agent_def: None,
                prompt: None,
            },
            &mut state,
            &host,
        )
        .await;
        let Effect::FocusNewSession {
            child: focused_child,
            parent,
            ..
        } = effect
        else {
            panic!("expected Effect::FocusNewSession");
        };
        assert_eq!(focused_child, child);
        assert_eq!(parent, root, "a bare spawn's parent is root");

        // Mirrors `App::run`'s own handling of this effect: seed the tree
        // node, THEN focus + resubscribe. The seed is the fix under test --
        // without it the freshly spawned child is absent from the `/agents`
        // panel, since its own `AgentSpawned` reaches neither the child
        // stream's replay (own records only) nor its live half (subscribed
        // after the spawn already fired).
        assert_ne!(
            state.focused_agent, child,
            "must not already be focused on the not-yet-focused child"
        );
        assert!(
            !state.tree.nodes.iter().any(|n| n.agent_id == child),
            "precondition: the child is not in the tree until seeded"
        );
        state.ensure_agent_tracked(child, parent);
        assert!(
            state
                .tree
                .nodes
                .iter()
                .any(|n| n.agent_id == child && n.parent == Some(root)),
            "the /agents tree must list the newly spawned child under root: {:?}",
            state.tree.nodes
        );
        state.focus_agent(child);

        assert_eq!(state.focused_agent, child);
        // Through the REAL render pass, not a hand-rolled assertion on
        // `AppState` alone: the status line names the newly focused child
        // (mirrors `view::status`'s own `status_line_names_the_focused_
        // agent_once_switched_off_root` test). The status line's `lineage`
        // field (this item relocated V5's breadcrumb here from T6's sticky
        // header) names the agent by its SHORT id, not the full ULID --
        // matching `view/agents.rs::short_agent_id`'s truncation.
        let rendered = crate::tui::test_support::render(&state, RENDER_WIDTH, 24);
        assert!(
            rendered
                .iter()
                .any(|row| row.contains(&crate::tui::view::agents::short_agent_id(child))),
            "the rendered status line must name the newly focused child: {rendered:?}"
        );
    }

    #[tokio::test]
    async fn after_a_bare_forks_focus_new_session_effect_the_focused_agent_is_the_new_child() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let mut host = FakeHost::new(root);
        let child = AgentId::new();
        host.fork_child = Some(child);

        let effect = execute(
            SlashCommand::Fork {
                agent: None,
                directive: Some("go".to_string()),
            },
            &mut state,
            &host,
        )
        .await;
        let Effect::FocusNewSession {
            child: focused_child,
            parent,
            first_message,
        } = effect
        else {
            panic!("expected Effect::FocusNewSession");
        };
        assert_eq!(focused_child, child);
        assert_eq!(
            parent, root,
            "a bare fork's parent is the focused agent (root here)"
        );
        assert_eq!(first_message, Some("go".to_string()));

        // Same regression as the spawn case: seed the tree node (the fix)
        // before focusing, and confirm the child now appears in the panel.
        state.ensure_agent_tracked(child, parent);
        assert!(
            state.tree.nodes.iter().any(|n| n.agent_id == child),
            "the /agents tree must list the newly forked child: {:?}",
            state.tree.nodes
        );
        state.focus_agent(child);
        assert_eq!(state.focused_agent, child);
        let rendered = crate::tui::test_support::render(&state, RENDER_WIDTH, 24);
        assert!(
            rendered
                .iter()
                .any(|row| row.contains(&crate::tui::view::agents::short_agent_id(child))),
            "the rendered status line must name the newly focused child: {rendered:?}"
        );
    }

    /// Board item `01M0TV5BSE98S16SFYECG9G9WP`, **acceptance 1 -- the whole
    /// loop, closed**: an operator names an agent they can see, through the
    /// PLUGIN'S OWN `/conway.names.rename` command, and then steers it by
    /// that name through `resolve_agent`. Neither half proves this alone
    /// (this project has shipped a test of one half before), so the test
    /// drives both across the seam that joins them -- one `AgentNames`
    /// `Arc` held by the plugin and by `AppState`, exactly as `main.rs`
    /// threads it.
    ///
    /// The fixture is deliberately not vacuous: `root` and `child` share
    /// the same `agent_def` (`None`), so the panel labels both rows
    /// `"agent"` -- the case a name exists to disambiguate -- and the name
    /// is resolved against a tree containing BOTH, so a resolver that
    /// returned any-old-agent would fail the `assert_eq!`.
    #[tokio::test]
    async fn a_name_set_through_the_plugin_command_resolves_back_to_the_same_agent() {
        let root: AgentId = "01HF7YAT000000000000000001"
            .parse()
            .expect("valid ULID string");
        let child: AgentId = "01J000000000000000000000A2"
            .parse()
            .expect("valid ULID string");

        // The ONE store, held by the plugin and by the app state -- a
        // compiled interface, not two readers of a file (decision
        // `01M0TV3ZZBDKSSV7MD0FW3FSY7`). `InMemoryAgentNames`, not
        // `FsAgentNames`: this test is about the loop, and it must never
        // write into the operator's own `~/.conway/`. Durability across a
        // restart is proven separately, over a tempdir, in
        // `conway-plugin-names`'s own `tests/names_end_to_end.rs`.
        let store: std::sync::Arc<dyn conway_plugin_names::AgentNames> =
            std::sync::Arc::new(conway_plugin_names::InMemoryAgentNames::new());
        let plugin: std::sync::Arc<dyn conway::plugin::Plugin> =
            std::sync::Arc::new(conway_plugin_names::NamesPlugin::new(store.clone()));
        // Reached the way an operator reaches it: the typed line is parsed,
        // and the resulting full name is looked up in the SAME registry
        // `App` builds from the installed plugin set. A test that called
        // `Plugin::commands()[0]` directly would not show that the string
        // an operator types arrives anywhere.
        let registry = CommandRegistry::build(&[plugin]).expect("the names plugin registers");

        let mut state = AppState::new(root);
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: None, // same as root's -- both rows read "agent"
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.agent_names = Some(store);
        state.focus_agent(child);

        // Before the rename, `scout` is nobody -- so the assertion below
        // cannot pass on a resolver that ignores the token entirely.
        assert!(
            resolve_agent(&state, "scout").is_err(),
            "precondition: nothing answers to `scout` yet"
        );

        // The operator types it.
        let typed = parse("/conway.names.rename scout").expect("the line must parse");
        let SlashCommand::Plugin { full_name, args } = typed else {
            panic!("a dotted command word must parse as a plugin command, got {typed:?}");
        };
        assert_eq!(full_name, "conway.names.rename");
        let rename = registry
            .resolve(&full_name)
            .unwrap_or_else(|| panic!("/{full_name} must be registered"));
        let outcome = rename
            .invoke(conway::plugin::CommandCtx {
                focused_agent: state.focused_agent,
                root_agent: root,
                session_id: conway::SessionId::new(),
                args,
            })
            .await;
        assert!(
            matches!(outcome, conway::plugin::CommandOutcome::Output(_)),
            "the rename must succeed: {outcome:?}"
        );

        // ... and steers it by that name. `/steer <agent> <text>` and every
        // other agent-targeted command reach exactly this call.
        let resolved = resolve_agent(&state, "scout")
            .unwrap_or_else(|e| panic!("the name just set must resolve: {e}"));
        assert_eq!(
            resolved, child,
            "`scout` must resolve to the agent that was named, not to some other row"
        );

        // The name is also what the operator sees on the row they pointed
        // at -- the panel and the resolver agreeing is the affordance.
        state.agent_view_open = true;
        let rows = crate::tui::test_support::render(&state, RENDER_WIDTH, 24);
        assert!(
            rows.iter().any(|row| row.contains("scout")),
            "the /agents panel must show the name that was just set: {rows:?}"
        );

        // And removing it puts the operator back exactly where they were.
        let unname = registry
            .resolve("conway.names.unname")
            .expect("/conway.names.unname must be registered");
        unname
            .invoke(conway::plugin::CommandCtx {
                focused_agent: state.focused_agent,
                root_agent: root,
                session_id: conway::SessionId::new(),
                args: String::new(),
            })
            .await;
        assert!(
            resolve_agent(&state, "scout").is_err(),
            "a removed name must stop resolving"
        );
    }

    /// A name is matched only against agents in THIS session's tree. The
    /// store is flat and global across projects
    /// (`conway_plugin_names`'s own module doc), so a name belonging to an
    /// agent nobody here can see must fall through to the SAME "no agent
    /// matches" message an unknown prefix already produces -- never resolve
    /// to something off screen.
    #[test]
    fn a_name_for_an_agent_outside_this_tree_does_not_resolve() {
        let root = AgentId::new();
        let stranger = AgentId::new();
        let store: std::sync::Arc<dyn conway_plugin_names::AgentNames> =
            std::sync::Arc::new(conway_plugin_names::InMemoryAgentNames::new());
        store.set(&stranger, "scout").expect("set");
        let mut state = AppState::new(root);
        state.agent_names = Some(store);
        let err = resolve_agent(&state, "scout").expect_err("a stranger's name must not resolve");
        assert!(
            err.contains("no agent matches"),
            "it must fail the way an unknown token already fails: {err:?}"
        );
    }

    /// Two live agents given the same name is an AMBIGUITY reported with
    /// every candidate named -- the same shape `resolve_agent` already uses
    /// for a colliding id prefix, deliberately not a second failure mode
    /// (determination question 3: duplicates are allowed at write time,
    /// because only the resolver knows which agents are on screen).
    #[test]
    fn two_agents_sharing_a_name_are_reported_as_ambiguous_with_both_candidates() {
        let root: AgentId = "01HF7YAT000000000000000001".parse().expect("ULID");
        let other: AgentId = "01J000000000000000000000A2".parse().expect("ULID");
        let store: std::sync::Arc<dyn conway_plugin_names::AgentNames> =
            std::sync::Arc::new(conway_plugin_names::InMemoryAgentNames::new());
        store.set(&root, "scout").expect("set");
        store.set(&other, "scout").expect("set");
        let mut state = AppState::new(root);
        state.tree.nodes.push(TreeNode {
            agent_id: other,
            parent: Some(root),
            agent_def: None,
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.agent_names = Some(store);
        let err = resolve_agent(&state, "scout").expect_err("a duplicate name must not pick one");
        assert!(
            err.contains("ambiguous")
                && err.contains(&root.to_string())
                && err.contains(&other.to_string()),
            "both candidates must be named, as the prefix path already does: {err:?}"
        );
    }

    /// An exact name beats an id PREFIX that would also have matched --
    /// `resolve_agent`'s decided ordering (exact id, exact name, then
    /// prefix). Without it, naming an agent `01J` while another agent's id
    /// starts with `01J` would make the deliberate name unreachable behind
    /// an accident.
    #[test]
    fn an_exact_name_wins_over_an_id_prefix_that_would_also_have_matched() {
        let named: AgentId = "01HF7YAT000000000000000001".parse().expect("ULID");
        let prefixed: AgentId = "01J000000000000000000000A2".parse().expect("ULID");
        let store: std::sync::Arc<dyn conway_plugin_names::AgentNames> =
            std::sync::Arc::new(conway_plugin_names::InMemoryAgentNames::new());
        store.set(&named, "01J0").expect("set");
        let mut state = AppState::new(named);
        state.tree.nodes.push(TreeNode {
            agent_id: prefixed,
            parent: Some(named),
            agent_def: None,
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.agent_names = Some(store);
        assert_eq!(
            resolve_agent(&state, "01J0").expect("the name must resolve"),
            named,
            "an exact name must not be shadowed by another agent's id prefix"
        );
        // The prefix path still works for a token no name claims.
        assert_eq!(
            resolve_agent(&state, "01J00").expect("the prefix must still resolve"),
            prefixed
        );
    }

    /// With `conway.names` uninstalled -- `AppState::agent_names` `None`,
    /// which is what every `AppState::new` produces -- `resolve_agent` is
    /// exactly the two-pass function it was before this item. Stated as its
    /// own test because "uninstalled changes nothing" is half of this
    /// item's deliverable, and the pre-existing resolver tests above (which
    /// this file did not edit) are the other half.
    #[test]
    fn with_no_names_store_installed_resolve_agent_is_unchanged() {
        let root: AgentId = "01HF7YAT000000000000000001".parse().expect("ULID");
        let state = AppState::new(root);
        assert!(
            state.agent_names.is_none(),
            "precondition: a plain AppState has no names store"
        );
        assert_eq!(
            resolve_agent(&state, &root.to_string()).expect("full id"),
            root
        );
        assert_eq!(resolve_agent(&state, "01HF7YAT0").expect("prefix"), root);
        assert!(resolve_agent(&state, "scout").is_err());
    }
}
