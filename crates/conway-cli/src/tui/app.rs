//! The interactive app loop: three tasks joined by channels (module
//! notes' architecture) -- the session's own `EventStream`, the gate's
//! `PendingPrompt` channel, and crossterm's key/resize stream -- driving one
//! [`AppState`] and redrawing at a capped rate.
//!
//! `run` is not unit-tested directly (it owns the real terminal and a live
//! `SessionHandle`); the pieces it composes (`state::apply`, `input::handle_key`,
//! `view::draw`, `gate::TuiGate`) are each unit-tested on their own, per the
//! module notes' guidance to avoid a real-PTY test.
//!
//! **This item (board): split out of a single 3,492-line file.** `App`
//! itself -- its fields, `App::submit` (the pre-`commands::parse`
//! dispatch entry point) -- is the one thing every seam below shares, so it
//! stays here, alongside `SubmitOutcome`. `App::session_spec`/`App::new`
//! (construction) live in `startup`; the event loop itself, `App::run`,
//! lives in `run`; plugin-command execution in `plugin_cmd`;
//! focus-switching in `focus`; terminal-size-derived scrolling in
//! `viewport`; `Ctrl-C`/quit handling in `shutdown`; the `/ask` modal's
//! own async completion in `ask`. Each submodule's own methods are
//! additional `impl App` blocks -- ordinary Rust, not a language feature --
//! so this split is purely organizational: `App` is exactly the same type,
//! with exactly the same fields and methods, as before it moved.
//!
//! **`submit` stays in THIS file, not a submodule.** It is `App`'s single
//! dispatch entry point, alongside the fields every seam shares, and it is
//! genuinely small now (board item `01KZVZ5XV162XCQR96AQKCCCF7`): `/settings`,
//! `/trust`, `/agents` and `/ask` used to be intercepted here by direct
//! string comparison, each running BEFORE `commands::parse` ever saw the
//! input -- a known structural defect T9
//! (`crates/conway/tests/architecture_invariants.rs`'s
//! `t9_tui_has_no_parser_bypasses`) used to pin at
//! exactly those four by grepping this file's own source text. All four are
//! now ordinary `commands::SlashCommand` variants, reached through the SAME
//! single `commands::parse` + `commands::execute` call every other command
//! already used -- see `submit`'s own doc for the one remaining piece of
//! housekeeping (`/settings`' state refresh) that still lives here rather
//! than in `commands::execute`, and why. T9 now asserts there are ZERO such
//! interceptions left, and fails the moment one reappears.

use tokio::sync::mpsc;

use crate::tui::commands::{self, Effect};
use crate::tui::state::{AppState, Entry};
use crate::tui::view::Theme;

mod ask;
mod focus;
mod marketplace;
mod plugin_cmd;
mod plugin_status;
mod plugin_toggle;
mod provider_manage;
mod provider_status;
mod run;
mod shutdown;
mod startup;
mod viewport;

#[cfg(test)]
pub(super) mod fixtures;

use ask::AskUpdate;
use plugin_cmd::PluginCommandDone;

pub struct App {
    handle: conway::SessionHandle,
    state: AppState,
    // `/resume` needs `Conway::resume`, not just the current
    // `SessionHandle` -- cheap to hold (every field is `Arc`-backed, per
    // `Conway`'s own doc: "Cheap to `Clone`").
    conway: conway::Conway,
    /// The TUI's resolved color/style table (T1): built once at startup from
    /// `[tui.theme]` config (defaults when the key is absent or a value is
    /// malformed -- config is untrusted input) and passed by reference into
    /// `view::draw` every frame. Decision D-T1: threaded as `&Theme`, not
    /// re-fetched via a call-site accessor or a global `Lazy`.
    theme: Theme,
    /// `/ask` (B5) spawns a `tokio::spawn`ed task per question (fork-ask,
    /// then drain the child's single turn to completion via
    /// `TurnHandle::text` -- see [`run_modal_ask`]) rather than folding it
    /// into `self.handle.events()`: the forked child is a DIFFERENT
    /// session, so its envelopes never arrive on that stream. When the
    /// task resolves, the loop opens the single-turn modal
    /// (`state.offer_ask_modal`) showing the child's answer and forcing
    /// exactly one fate (`f`/`p`/`Esc`). `modal_ask_tx` is cloned into
    /// each spawned task; `modal_ask_rx` is taken out of `self` once, in
    /// `run`, and polled there as an extra `tokio::select!` arm.
    modal_ask_tx: mpsc::UnboundedSender<AskUpdate>,
    modal_ask_rx: Option<mpsc::UnboundedReceiver<AskUpdate>>,
    /// The installed plugin commands,
    /// built once at [`Self::new`] from the plugin list the caller (`tui::run`,
    /// ultimately `main.rs`) was handed -- the SAME list installed into the
    /// `Conway` this `App` already holds, so a plugin command reaches only a
    /// plugin actually installed this run (see `commands::CommandRegistry::
    /// build`'s own doc). `Arc` so [`commands::LiveHost`] can borrow it
    /// per-call the same way it borrows `handle`/`conway`.
    command_registry: std::sync::Arc<commands::CommandRegistry>,
    /// Mirrors `modal_ask_tx`/`modal_ask_rx` exactly, for the SAME reason
    /// (module notes on those two fields): a plugin command's `invoke` is
    /// spawned off this loop (`Self::run`'s own `Effect::RunPluginCommand`
    /// arm), never awaited directly on it -- see that arm's own doc, and
    /// `commands::Effect::RunPluginCommand`'s, for why this is the
    /// structural guarantee behind this item's hang/panic-safety acceptance
    /// criterion.
    plugin_cmd_tx: mpsc::UnboundedSender<PluginCommandDone>,
    plugin_cmd_rx: Option<mpsc::UnboundedReceiver<PluginCommandDone>>,
    /// Board item `01M11XWB4T8ZADNDB4M8R482MA`: mirrors `plugin_cmd_tx`/
    /// `plugin_cmd_rx` exactly, same reasoning, for the providers section's
    /// own background classification -- see `provider_status.rs`'s own
    /// module doc for why `classify_fleet` is spawned off this loop rather
    /// than awaited inline.
    provider_status_tx: mpsc::UnboundedSender<provider_status::ProviderStatusDone>,
    provider_status_rx: Option<mpsc::UnboundedReceiver<provider_status::ProviderStatusDone>>,
    /// T8: where [`Self::submit`] persists `state.history` to after every push
    /// -- `~/.conway/history` (or `$CONWAY_CONFIG_DIR/history` when set),
    /// resolved once at `App::new` via
    /// `conway::config::discovery::history_file_path`. `None` only when that
    /// resolution itself fails (no resolvable home directory --
    /// `directories::BaseDirs::new()` returned `None`), in which case history
    /// still works for the running session (in-memory, via
    /// `AppState::history`), it just never round-trips to disk. The history
    /// file is untrusted input: this is a degrade, never a startup failure.
    history_path: Option<std::path::PathBuf>,
    /// Board item `01M0WB5W5DX844HSJQG3JP23X0`'s determine-first question 1
    /// answer: `App::apply_marketplace_install`/`apply_marketplace_
    /// uninstall` need `env` (to resolve `settings.json`'s path via
    /// `CONWAY_CONFIG_DIR`) and `cwd` (the project config layer, for their
    /// own honesty check) -- neither of which `commands::Host` carries
    /// (that trait is deliberately "a thin abstraction over exactly
    /// `SessionHandle`/`Conway`'s own methods", its own doc, and this needs
    /// neither). Both are resolved ONCE here, at construction (`App::new`,
    /// `startup.rs`), from the SAME ambient sources that method already
    /// reads for `history_path` (`std::env::vars()`) and `state.cwd_display`
    /// (`cli.cwd` falling back to `conway.config().cwd`) -- reused, not a
    /// second independent read. Two alternatives considered and rejected:
    /// a NEW `App::new` parameter (this file's own `startup.rs` doc on
    /// `plugin_browser`/`agent_names`: ~40 existing call sites, nearly all
    /// of this crate's own tests, would each have to name a value they
    /// never touch), and an ambient `std::env::vars()` read done fresh
    /// inside the command-dispatch path itself, which would defeat
    /// `CONWAY_CONFIG_DIR`-based test isolation for this AND every future
    /// command needing it -- exactly the hazard this item's own spec named.
    /// Threaded to the two methods by cloning at the one call site that
    /// needs them (`Effect::RunMarketplaceInstall`/`RunMarketplaceUninstall`'s
    /// arms in `Self::submit`, both awaited/called directly -- neither is
    /// spawned off this loop, see `app/marketplace.rs`'s own module doc for
    /// why). Tests override both directly after construction for isolation
    /// -- private-field access already used throughout this crate's own
    /// test modules (e.g. `app.state`).
    env: std::collections::HashMap<String, String>,
    cwd: std::path::PathBuf,
}

/// What `App::submit` learned the app loop must additionally do, beyond the
/// `AppState` mutation `submit` already performed directly.
enum SubmitOutcome {
    Continue,
    /// `/resume` replaced `self.handle` -- the loop's own `events` stream
    /// (a local in `run`, not reachable from `submit`) must be
    /// re-subscribed from the new handle.
    Resubscribe,
    /// `/quit` -- exit the app loop.
    Quit,
    /// A bare `/spawn`/`/fork` succeeded (`commands::Effect::
    /// FocusNewSession`, WI "bare /spawn & /fork open an interactive
    /// session"): the run loop must focus `child` (the same live-facade
    /// resubscribe `Action::FocusAgent` already performs -- `submit` has no
    /// `events` local to do it itself) and, if `first_message` is `Some`,
    /// deliver it to `child` as that session's first prompt.
    FocusNewSession {
        child: conway::AgentId,
        /// The agent `child` was created under -- seeds its `/agents` tree
        /// node immediately (`AppState::ensure_agent_tracked`), since the
        /// child's own `AgentSpawned` never reaches the stream the loop is
        /// about to switch to (see that method's doc and `Effect::
        /// FocusNewSession::parent`).
        parent: conway::AgentId,
        first_message: Option<String>,
    },
}

impl App {
    /// Routes `text` to `commands::parse` + `commands::execute` when it
    /// starts with `/` (module notes: "the dispatch hook is defined here,
    /// handlers land elsewhere"); otherwise sends it as a prompt. A
    /// malformed or unknown slash command becomes a `Notice` -- it is never
    /// sent to the model (module notes' binding requirement, carried from
    /// the stub).
    ///
    /// **One dispatch path (board item `01KZVZ5XV162XCQR96AQKCCCF7`):**
    /// `commands::parse` runs exactly once per `/`-prefixed `text`, and
    /// every command -- `/settings`, `/trust`, `/agents`, `/ask` included --
    /// is reached from ITS result, never from a raw string comparison ahead
    /// of it. `/settings`, `/trust` and `/agents` used to be intercepted
    /// HERE by direct string comparison, before `commands::parse` ever saw
    /// them; `/ask` too. T9 (`crates/conway/tests/architecture_invariants.
    /// rs`'s `t9_tui_has_no_parser_bypasses`) used to
    /// pin that count at exactly four and now asserts it is zero.
    ///
    /// Two of the four still need a little more than a bare `commands::
    /// execute` call can give them on its own, both handled in the ONE
    /// match below, never by a pre-parse special case:
    /// - **`/settings`** needs its eight `Conway`-backed mirrors
    ///   (`permission_grants`, `structured_allow_rules`, `permission_mode`,
    ///   `permission_denies`, `permission_prompts`, `structured_deny_rules`,
    ///   `structured_prompt_rules`, `hook_rules`) refreshed BEFORE the menu
    ///   renders, so the menu builder stays a pure function of `AppState`
    ///   (V2b). That refresh runs here, keyed off the PARSED
    ///   `commands::SlashCommand::Settings` variant (never off `text`
    ///   itself), immediately before the shared `commands::execute` call
    ///   that actually opens the menu -- kept OUT of `commands::execute`
    ///   itself because `commands.rs::tests::settings_opens_the_menu`
    ///   already pins `/settings` as "a pure `AppState` flip, no facade call
    ///   at all", and this item's own acceptance forbids editing that test.
    /// - **`/ask`** needs a `tokio::spawn`ed task (forking the ephemeral
    ///   child and draining its turn) that only `App` can start -- it owns
    ///   the live `SessionHandle` to clone and `modal_ask_tx` to reply on,
    ///   neither of which `commands::execute`'s `Host` seam exposes.
    ///   `commands::execute`'s `SlashCommand::Ask` arm does everything it
    ///   CAN from there (validates, sets `state.ask_in_flight`) and hands
    ///   back `Effect::RunModalAsk`, handled in the SAME `Effect` match this
    ///   method already has for `Effect::RunPluginCommand` -- see `Self::
    ///   spawn_modal_ask`.
    ///
    /// `/trust` and `/agents` need neither: both are now ordinary
    /// `commands::execute` arms (`/trust` through the new `Host::
    /// trust_permission_file`, `/agents` a pure `AppState` flip), reached
    /// through the identical `commands::parse` -> `commands::execute` call
    /// this method already makes for `/steer`, `/fork`, `/spawn`, and every
    /// other command. `/ask`'s three FATES (fork the child into a real
    /// session, pull the answer into the transcript, discard it) are
    /// untouched by this item -- they run through `commands::
    /// apply_ask_fate`, driven by `Action::AskFate` in `run.rs`, a
    /// completely separate call site from `submit` that this refactor never
    /// touches.
    async fn submit(&mut self, text: String) -> conway::Result<SubmitOutcome> {
        // T8: every submitted line (prompt or slash command) is recorded into
        // the history FIFO before dispatch, so a slash command that changes
        // `self.handle`/exits the loop still recorded exactly what the user
        // typed. `AppState::push_history` is pure/in-memory and bounds the
        // deque to `state.history_cap` itself; persisting the updated deque to
        // disk is a SEPARATE, best-effort step (the history file is untrusted:
        // a failed WRITE must never fail the submit it was recording, so the
        // `io::Result` is swallowed here, not `?`-propagated). Run on the
        // blocking pool -- same reasoning `App::new`'s `read_git_branch` uses
        // -- so a slow/contended filesystem never stalls the app loop.
        self.state.push_history(text.clone());
        if let Some(path) = self.history_path.clone() {
            let history = self.state.history.clone();
            let _ = tokio::task::spawn_blocking(move || crate::tui::history::save(&path, &history))
                .await;
        }
        // V4: `/thinking` and `/timestamps` -- the state-only toggles that
        // used to be intercepted HERE (mirroring `/agents`'s old pattern) --
        // are REMOVED, not aliased. Both are now a single `/settings` menu
        // (`view/settings.rs`), reached through the ordinary
        // `commands::parse`/`execute` path below like any other command.
        if text.starts_with('/') {
            match commands::parse(&text) {
                Ok(cmd) => {
                    // V2b: refresh the eight `Conway`-backed settings
                    // mirrors BEFORE the shared dispatch below runs
                    // `commands::execute` (which is what actually opens the
                    // menu) -- see `submit`'s own doc for why this lives
                    // here, keyed off the PARSED variant, rather than
                    // inside `commands::execute` itself. The broker is the
                    // authority; these copies exist so the menu builder
                    // stays a pure function of `AppState`, and would be
                    // stale (or empty) if refreshed anywhere else. Kept as
                    // `(rule, origin)` pairs rather than pre-formatted
                    // strings -- `view/settings.rs::build_tree` both labels
                    // each row (`[interactive]`/the originating file's
                    // path, via `origin.describe()`) AND addresses it for
                    // per-rule revocation, which a bare formatted string
                    // could never do.
                    if matches!(cmd, commands::SlashCommand::Settings) {
                        self.state.permission_grants = self.conway.active_permission_patterns();
                        self.state.structured_allow_rules =
                            self.conway.active_structured_allow_rules();
                        self.state.permission_mode = self.conway.permission_mode();
                        // The read-only deny/prompt review lists refresh on
                        // the same seam. Unlike grants, these never change
                        // in-session (deny and prompt rules install only at
                        // file load, from any file, trusted or not -- there
                        // is no interactive path that adds one), so this
                        // refresh exists to keep the menu builder a pure
                        // function of `AppState`, not to track churn.
                        self.state.permission_denies =
                            self.conway.active_deny_permission_patterns();
                        self.state.permission_prompts =
                            self.conway.active_prompt_permission_patterns();
                        self.state.structured_deny_rules =
                            self.conway.active_structured_deny_rules();
                        self.state.structured_prompt_rules =
                            self.conway.active_structured_prompt_rules();
                        // the fourth mirror, refreshed on the SAME seam as
                        // the four above -- see this block's own doc for
                        // why the refresh lives here rather than anywhere
                        // else.
                        self.state.hook_rules = self.conway.active_deny_capable_hook_rules();
                        // Board item `01M11XWB4T8ZADNDB4M8R482MA`: the
                        // providers section's own listing, refreshed on the
                        // SAME seam as the five mirrors above, for the same
                        // reason -- `view/settings.rs::build_tree` stays a
                        // pure function of `AppState`. `env` is collected
                        // HERE (never inside the method itself) for the
                        // hermetic-testing idiom this file's own doc already
                        // establishes for every other `env`-needing call
                        // site.
                        let env_vars: std::collections::HashMap<String, String> =
                            std::env::vars().collect();
                        self.refresh_provider_entries_and_kick_off_status(
                            &env_vars,
                            &std::env::current_dir()
                                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
                        );
                    }
                    let host = commands::LiveHost {
                        handle: &self.handle,
                        conway: &self.conway,
                        commands: &self.command_registry,
                    };
                    match commands::execute(cmd, &mut self.state, &host).await {
                        Effect::None => {}
                        Effect::Quit => return Ok(SubmitOutcome::Quit),
                        Effect::Resumed(handle) => {
                            self.handle = handle;
                            // the
                            // resumed session's own head, same reasoning as
                            // `Self::new`'s initial fetch -- `execute`'s
                            // `Resume` arm already reset `self.state` to a
                            // fresh `AppState` (this call site's own
                            // sibling in `apply_plugin_command_done` does
                            // the identical reset for `ForkSession`), so
                            // `session_head_seq` starts `None` here.
                            self.refresh_session_head().await;
                            return Ok(SubmitOutcome::Resubscribe);
                        }
                        Effect::FocusNewSession {
                            child,
                            parent,
                            first_message,
                        } => {
                            return Ok(SubmitOutcome::FocusNewSession {
                                child,
                                parent,
                                first_message,
                            });
                        }
                        // `execute`
                        // itself never ran a byte of the plugin's own code
                        // (see `Effect::RunPluginCommand`'s own doc) -- THIS
                        // is where it actually runs. See
                        // `Self::spawn_plugin_command`'s own doc for the
                        // hang/panic-isolation mechanism.
                        Effect::RunPluginCommand(invocation) => {
                            self.spawn_plugin_command(invocation);
                        }
                        // B5: `execute`'s `SlashCommand::Ask` arm has
                        // already validated and set `state.ask_in_flight`
                        // -- THIS is where the actual `tokio::spawn` runs,
                        // mirroring `RunPluginCommand`'s own arm exactly
                        // (and for the identical reason: only `App` owns
                        // the live `SessionHandle`/`modal_ask_tx` this
                        // needs). See `Effect::RunModalAsk`'s own doc and
                        // `Self::spawn_modal_ask`'s.
                        Effect::RunModalAsk { question } => {
                            self.spawn_modal_ask(question);
                        }
                        // Board item `01M0WB5W5DX844HSJQG3JP23X0`: `execute`
                        // cannot reach `App::apply_marketplace_install`
                        // itself (see `Effect::RunMarketplaceInstall`'s own
                        // doc for why -- it needs `env`/`cwd`, which live on
                        // `App`, and a network dependency `execute`'s own
                        // crate graph keeps out of reach), so THIS is where
                        // it actually runs -- awaited directly, like every
                        // other `Host`-routed facade call `execute` already
                        // made above (see `app/marketplace.rs`'s own module
                        // doc for why this is not spawned off the loop the
                        // way `RunPluginCommand`/`RunModalAsk` are).
                        Effect::RunMarketplaceInstall {
                            marketplace_url,
                            plugin_id,
                        } => {
                            let env = self.env.clone();
                            let cwd = self.cwd.clone();
                            self.apply_marketplace_install(marketplace_url, plugin_id, &env, &cwd)
                                .await;
                        }
                        // Mirrors `RunMarketplaceInstall` immediately above;
                        // `env`/`cwd` are cloned before the call for the
                        // same reason `spawn_modal_ask`/`spawn_plugin_command`
                        // clone `self.handle`/`self.modal_ask_tx` first: the
                        // method needs `&mut self` too.
                        Effect::RunMarketplaceUninstall { plugin_id } => {
                            let env = self.env.clone();
                            let cwd = self.cwd.clone();
                            self.apply_marketplace_uninstall(plugin_id, &env, &cwd);
                        }
                    }
                }
                Err(e) => {
                    self.state.transcript.push(Entry::Notice {
                        text: e.to_string(),
                    });
                }
            }
            return Ok(SubmitOutcome::Continue);
        }
        // Fix 2 (SIGNIFICANT, review): `Runtime::prompt` never removes a
        // finished agent from its live registry (it only checks the id is
        // KNOWN, not that its task is still running), so prompting one
        // still returns `Ok` -- Fix 1's error handling just below never
        // sees this case. Left unguarded, it appends a `UserTurn` to a
        // session no task will ever read again (message silently lost) and
        // unconditionally setting `activity = Thinking` would wedge the
        // status line on "thinking" forever: a finished agent emits no
        // further `TurnFinished`/`AgentFinished` to ever reset it (`state.
        // rs`'s own `Event::AgentFinished`/`TurnStarted` arms are the only
        // things that clear `Thinking`). Same wedge shape, a different door,
        // as an `/ask` pull-in used to wedge `Responding` instead --
        // `state.rs`'s `Event::TextDelta` arm now guards its own write the
        // same way this comment describes, on `turn_started_at.is_some()`
        // rather than a live-registry check, since that arm has no
        // registry to consult. Checked BEFORE the message is even
        // echoed into the transcript, so a message that was never actually
        // sent never appears to have been. `AppState::
        // block_message_if_focused_agent_finished` (which itself defers to
        // `is_focused_agent_live`) owns the actual guard + `Notice` text --
        // see its own doc for why a keep-alive root/idle keep-alive child
        // (never terminal) is never blocked here, and why an as-yet-
        // untracked agent (e.g. this exact turn's own freshly spawned
        // child) fails open rather than blocking.
        if self.state.block_message_if_focused_agent_finished() {
            return Ok(SubmitOutcome::Continue);
        }
        // This item (typed `Event::UserTurn`, closing the gap where one mode
        // had a capability the facade did not): no local `Entry::User` push
        // here anymore. `Runtime::prompt` (reached via `prompt_agent` below)
        // now emits `Event::UserTurn` live on the SAME event stream this app's
        // run loop already polls, and `state. rs`'s `apply` builds the
        // `Entry::User` bubble from that envelope
        // -- the one path both this TUI and a library embedder watching the
        // bare `EventStream` now share. Pushing it here too would double it
        // (the exact regression this item's own tests guard against); NOT
        // pushing it at all on a failed `prompt_agent` (the `Err` arm below)
        // is also correct, not a gap -- a message that was never actually
        // sent must never appear to have been (echoing it locally used to
        // do exactly that on a failure).
        //
        // WI "bare /spawn & /fork open an interactive session": a plain
        // (non-slash-command) message now prompts the FOCUSED agent, not
        // unconditionally the root -- `handle.prompt_agent` (generalizing
        // `handle.prompt`, which only ever targeted `self.handle.root()`)
        // is what makes typing into an interactive keep-alive child (after
        // a bare `/spawn`/`/fork` auto-focused it) actually reach THAT
        // session instead of silently talking to the root underneath it.
        //
        // Fix 1 (SIGNIFICANT, review): `prompt_agent` is fallible
        // (transient store I/O, etc.) -- `?`-propagating it here used to
        // unwind straight out of `submit` -> `App::run` -> `tui::mod.rs`'s
        // `run`, killing the WHOLE interactive process over one failed
        // prompt. Matched instead, mirroring every other facade call this
        // module already treats this way (`Self::try_focus_agent`, `Self::
        // deliver_first_message`): a failure becomes a `Notice`, `activity`
        // is left alone (nothing was actually started, so it must not read
        // `Thinking`), and the app loop keeps running.
        match self
            .handle
            .prompt_agent(self.state.focused_agent, text)
            .await
        {
            Ok(_) => {
                // Bug 2 fix: mark the
                // indicator working the instant Enter is pressed, rather
                // than waiting for the first event to arrive on the stream
                // (`state.rs`'s `TurnStarted` arm covers the same window
                // from the event side; this covers the sliver of time
                // before that envelope has even round-tripped back).
                // Unconditional (the old `is_root_focused` guard is
                // obsolete): `prompt_agent` above targeted `state.
                // focused_agent` directly, so the focused agent IS the
                // agent whose turn was just started -- unlike the old
                // hardcoded-root `handle.prompt`, there is no longer a
                // "prompted a different agent than the one in view" case
                // this needed to guard against.
                self.state.activity = crate::tui::state::Activity::Thinking;
            }
            Err(e) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("could not send message: {e}"),
                });
            }
        }
        Ok(SubmitOutcome::Continue)
    }
}

#[cfg(test)]
mod tests {
    //! This item's own end-to-end acceptance test: "a prompt appears exactly
    //! once in the transcript -- not zero, not twice" (the regression the
    //! removal of `submit`'s local `Entry::User` push risks). Unlike the
    //! module doc's "`run` is not unit-tested directly" note -- which is
    //! about the real-terminal, real-crossterm-stream loop -- `submit`
    //! itself needs only a live `SessionHandle`, no PTY, so it is
    //! reasonable, narrow scope to drive it directly here with a fully
    //! in-memory `Conway` (the same fake port set `conway`'s own
    //! `tests/session_handle.rs` builds).

    use std::sync::Arc;

    use conway::config::schema::HooksConfig;
    use conway::{ConwayBuilder, PermissionGate};
    use conway_core::agent::PermissionDecision;
    use conway_core::ids::{BackendId, ModelId};
    use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

    use super::ask;
    use super::fixtures::{
        base_config, conway_with_contributing_plugin_and_store, echo_conway, echo_conway_and_store,
        echo_conway_over, minimal_cli,
    };
    use super::{App, SubmitOutcome};
    use crate::tui::commands;
    use crate::tui::state::{Activity, AskFate, AskModal, Entry, Mode};

    /// The `Effect::Resumed` call site's own `refresh_session_head` call
    /// (`Self::submit`, this item): resuming a DIFFERENT, non-empty
    /// session must read back ITS OWN head, not leave the field `None`
    /// (`execute`'s `Resume` arm resets `self.state` to a fresh
    /// `AppState`, which starts `None`) or carry over whatever the
    /// previous session's head happened to be.
    ///
    /// **Two `Conway`s sharing one `FakeStore`, not one** -- the SAME
    /// "simulated restart" shape `crates/conway/tests/resume.rs`'s own
    /// `resume_returns_handle_whose_id_and_root_match_the_session_header`
    /// establishes and explains: attaching an agent id already live in the
    /// SAME `Runtime`'s tree is a genuine, unrelated error
    /// (`conway_runtime::tree::already_attached`), so `other` is created
    /// and driven to completion under its OWN `Conway`/`Runtime` (dropped
    /// before `app.submit` runs), leaving only its PERSISTED record behind
    /// for `app`'s own `Conway::resume` to re-attach.
    #[tokio::test]
    async fn resuming_a_session_refreshes_its_own_head_seq() {
        let (conway, store) = echo_conway_and_store();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let other_sid = {
            let other_conway = echo_conway_over(store.clone());
            let other = other_conway
                .new_session(conway::SessionSpec::default())
                .await
                .expect("new_session should succeed");
            let sid = other.id();
            other
                .prompt("hello")
                .await
                .expect("prompt should not error")
                .text()
                .await
                .expect("turn should complete");
            sid
            // `other_conway`/`other` drop here -- their in-memory
            // `Runtime`/tree go with them, leaving only the persisted log
            // in the SHARED `store` behind for `app.conway.resume` to read.
        };
        let other_head = conway
            .session_head(other_sid)
            .await
            .expect("session_head should succeed");
        assert!(other_head.0 > 0);

        let outcome = app
            .submit(format!("/resume {other_sid}"))
            .await
            .expect("submit should not error");
        assert!(
            matches!(outcome, SubmitOutcome::Resubscribe),
            "transcript: {:?}",
            app.state.transcript
        );
        assert_eq!(
            app.state.session_head_seq,
            Some(other_head),
            "resuming must read back the RESUMED session's own head, not leave it None or \
             stale"
        );
    }

    /// Board item `01M0XDEDBR5YDF71Q7ZRXYMT85`: the third link in the
    /// "populate `AppState::plugin_status_contributions`" chain, closed
    /// end to end. `commands::execute`'s `Resume` arm used to carry
    /// `plugin_commands`/`agent_names` across its `AppState::new` reset by
    /// hand but leave `plugin_status_contributions` out of that list --
    /// this is that field's OWN `resuming_a_session_refreshes_its_own_
    /// head_seq`, proving the fix the same "real `App::submit`, two
    /// `Conway`s sharing one `FakeStore`" way that test proves
    /// `session_head_seq`'s own survival.
    ///
    /// **Still a snapshot, not a live view** -- the assertion is that
    /// `/resume` does not DROP the value `App::new` already captured, not
    /// that a health change occurring on the resumed session is now
    /// visible (`app/startup.rs`'s `ContributingPlugin`/`AppState::
    /// plugin_status_contributions`'s own doc name that as a separate,
    /// larger, deliberately unbuilt piece). Render-asserts too, matching
    /// this crate's own binding convention: the value must still reach the
    /// literal rendered status line after `/resume`, not merely survive on
    /// the struct.
    #[tokio::test]
    async fn plugin_status_contribution_survives_resume() {
        let (conway, store) = conway_with_contributing_plugin_and_store();
        let mut cli = minimal_cli();
        // `plugins` is not in the Lean line's default fields -- an
        // operator has to opt in, matching `app/startup.rs`'s own
        // `app_new_populates_plugin_status_contributions_from_a_real_
        // plugin` fixture exactly.
        let tui_config_dir = tempfile::tempdir().expect("tempdir");
        let tui_config_path = tui_config_dir.path().join("settings.json");
        std::fs::write(
            &tui_config_path,
            serde_json::json!({"tui": {"status_line": {"fields": ["plugins"]}}}).to_string(),
        )
        .expect("write settings.json carrying [tui.status_line.fields]");
        cli.config = Some(tui_config_path);

        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let expected = vec![conway::plugin::PluginStatusContribution {
            key: "guard".to_string(),
            status: conway::ResultStatus::Completed,
            value: "qwen2.5-3b".to_string(),
        }];
        assert_eq!(
            app.state.plugin_status_contributions, expected,
            "precondition: App::new must populate the contribution before any /resume runs"
        );

        // Same "simulated restart" shape as `resuming_a_session_refreshes_
        // its_own_head_seq`: a second, independent `Conway`/`Runtime` over
        // the SAME store, dropped before `app.submit` runs, leaving only
        // its persisted record for `app.conway.resume` to re-attach.
        let other_sid = {
            let other_conway = echo_conway_over(store.clone());
            let other = other_conway
                .new_session(conway::SessionSpec::default())
                .await
                .expect("new_session should succeed");
            let sid = other.id();
            other
                .prompt("hello")
                .await
                .expect("prompt should not error")
                .text()
                .await
                .expect("turn should complete");
            sid
        };

        let outcome = app
            .submit(format!("/resume {other_sid}"))
            .await
            .expect("submit should not error");
        assert!(
            matches!(outcome, SubmitOutcome::Resubscribe),
            "transcript: {:?}",
            app.state.transcript
        );

        assert_eq!(
            app.state.plugin_status_contributions, expected,
            "/resume must not drop the plugin status-contribution snapshot, matching how \
             plugin_commands/agent_names already survive it"
        );

        // Buffer-asserting half (this crate's binding TUI test convention):
        // the contribution must still be READABLE on the real, rendered
        // status line after `/resume`, not merely present on the struct.
        let text = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            text.contains("guard: qwen2.5-3b"),
            "the plugin's status contribution must still reach the rendered status line after \
             /resume: {text}"
        );
    }

    /// This item's own end-to-end acceptance test: "a prompt appears exactly
    /// once in the transcript -- not zero, not twice" (the regression the
    /// removal of `submit`'s local `Entry::User` push risks).
    #[tokio::test]
    async fn submit_renders_the_prompt_exactly_once_not_zero_not_twice() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        // Subscribed BEFORE `submit`, exactly like `App::run`'s own `events`
        // local -- the live `Event::UserTurn` `Runtime::prompt` emits (this
        // item) must not be missed.
        let mut events = app.handle.events();

        let outcome = app
            .submit("hello from the test".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        // `submit` itself pushes nothing locally anymore (this item) --
        // the transcript is empty until the live envelope is drained below.
        super::fixtures::drain_and_apply(&mut events, &mut app.state);

        let user_entries: Vec<&str> = app
            .state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::User(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_entries,
            vec!["hello from the test"],
            "the prompt must appear exactly once in the transcript, got {:?}",
            app.state.transcript
        );

        // Buffer-asserting half (this crate's binding TUI test convention):
        // render the REAL `AppState` through the REAL `view::draw` and
        // confirm the prompt shows up exactly once on screen too, not
        // duplicated.
        let text = crate::tui::test_support::render_text(&app.state, 80, 24);
        assert_eq!(
            text.matches("hello from the test").count(),
            1,
            "the prompt must render exactly once on screen: {text}"
        );
    }

    /// The verification anchor's negative half, at the SAME `App`/`submit`
    /// layer: with the plugin NOT installed, the identical input is simply
    /// an unknown command -- shown to fail (a bare `Effect::None` +
    /// `Notice`, never a stub, never a special case) when the positive test
    /// in `plugin_cmd`'s own test module's fixture is absent, which is what
    /// proves that test is not vacuous.
    #[tokio::test]
    async fn plugin_command_is_unknown_when_the_plugin_is_not_installed() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]) // no plugins installed
            .await
            .expect("App::new should succeed");

        assert!(app.state.plugin_commands.is_empty());

        let outcome = app
            .submit("/acme.greet world".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("unknown command") && text.contains("acme.greet")
            )),
            "an unresolved plugin-shaped command must become an ordinary \
             unknown-command notice: {:?}",
            app.state.transcript
        );
    }

    /// **The discriminating observable this item exists to prove.** With
    /// `conway_plugin_history::HistoryPlugin` NOT installed (`App::new`'s
    /// own `plugins` slice is empty -- no fixture, no substitute), typing
    /// `/conway.history.rewind 1` produces the ordinary "unknown command"
    /// notice `commands::execute`'s `SlashCommand::Plugin` arm already
    /// produces for ANY unresolved plugin-shaped name -- never a
    /// core-level special case for this specific command, and never a
    /// `ForkSession` outcome (nothing was ever invoked to produce one).
    #[tokio::test]
    async fn conway_history_rewind_is_an_unknown_command_when_the_plugin_is_not_installed() {
        let conway = echo_conway();
        let cli = minimal_cli();
        // Deliberately `&[]`: no plugin installed at all, not even an
        // unrelated one -- the empty case this whole item's acceptance
        // criterion is about.
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let outcome = app
            .submit("/conway.history.rewind 1".to_string())
            .await
            .expect("submit should not error even for an unresolved plugin command");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        assert!(
            app.plugin_cmd_rx
                .as_mut()
                .expect("plugin_cmd_rx is set by App::new")
                .try_recv()
                .is_err(),
            "nothing was ever invoked -- no plugin command task exists to reply at all"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("unknown command") && text.contains("conway.history.rewind")
            )),
            "an uninstalled plugin's command must be the ORDINARY unknown-command notice, not a \
             stub or a special case: {:?}",
            app.state.transcript
        );
    }

    /// A3: a deny or prompt rule shipped by an UNTRUSTED project
    /// checkout applies immediately (that asymmetry is the sound part of
    /// the model -- `permission_trust_seam.rs` proves the application
    /// half) -- and the operator can SEE it in `/settings`, with its
    /// origin. This drives the real production seam end to end: a real
    /// `.conway/permissions.json` on a real filesystem, loaded by the real
    /// `App::new` loader (`Conway::load_permission_files`), `/settings`
    /// opened through the real `submit` path, and the assertion made on
    /// the OBSERVABLE rows -- the exact labels `view::draw` renders (via
    /// `view::settings::build_tree`) plus the rendered screen buffer --
    /// never on an internal call count. No trust decision and no user config
    /// isolation are needed precisely because deny/prompt install from any
    /// file, trusted or not -- the case this item exists for.
    #[tokio::test]
    async fn untrusted_file_deny_and_prompt_rules_are_visible_in_settings() {
        let project = tempfile::TempDir::new().expect("tempdir");
        let conway_dir = project.path().join(".conway");
        std::fs::create_dir_all(&conway_dir).expect("mkdir .conway");
        // Pin project discovery to the tempdir (an empty `settings.json` is
        // all `discover` checks for) so no ancestor `.conway/` can
        // redirect the permissions-file path.
        std::fs::write(conway_dir.join("settings.json"), "").expect("write settings.json");
        // One flat deny, one structured-only deny (multiple tools -- the
        // flat form cannot express it), one prompt that round-trips to the
        // flat form, one structured-only prompt: both halves of both
        // review lists, from the same untrusted file.
        std::fs::write(
            conway_dir.join("permissions.json"),
            r#"{
                "deny": ["bash:curl"],
                "rules": [
                    {"select": {"tools": ["bash", "read"]}, "when": "always", "then": "deny"},
                    {"select": {"tools": ["bash"]}, "when": {"command_prefix": "rm"}, "then": "prompt"},
                    {"select": {"tools": ["bash", "read"]}, "when": "always", "then": "prompt"}
                ]
            }"#,
        )
        .expect("write permissions.json");

        let conway = echo_conway();
        let mut cli = minimal_cli();
        cli.cwd = Some(project.path().to_path_buf());
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let outcome = app
            .submit("/settings".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));
        assert!(app.state.settings_open, "/settings must open the menu");

        let origin = conway_dir.join("permissions.json").display().to_string();
        let rows = crate::tui::view::settings::build_tree(&app.state).rows();
        let labels: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();
        let joined = labels.join("\n");

        // Every deny/prompt rule from the untrusted file -- flat and
        // structured -- is a visible row carrying the file's own path.
        for needle in [
            format!("[{origin}] `bash` commands starting with `curl`"),
            format!("[{origin}] [bash, read] (any call)"),
            format!("[{origin}] `bash` commands starting with `rm`"),
        ] {
            assert!(
                joined.contains(&needle),
                "missing review row: {needle}\n{joined}"
            );
        }
        // The two `[bash, read] (any call)` rules (one deny, one prompt)
        // both render -- the count, not just presence, since they share a
        // description.
        assert_eq!(
            joined.matches("[bash, read] (any call)").count(),
            2,
            "the structured deny AND the structured prompt must each appear: {joined}"
        );
        // And they are read-only rows, never selectable ones.
        let deny_prompt_rows: Vec<_> = rows.iter().filter(|r| r.label.contains(&origin)).collect();
        assert_eq!(deny_prompt_rows.len(), 4, "{joined}");
        for row in deny_prompt_rows {
            assert_eq!(
                row.kind,
                crate::tui::view::menu::MenuRowKind::Static,
                "a deny/prompt row from an untrusted file must be read-only: {row:?}"
            );
        }

        // Buffer-asserting half (this crate's binding TUI test convention):
        // the operator can actually READ the sections on screen.
        let text = crate::tui::test_support::render_text(&app.state, 200, 50);
        for needle in [
            "deny",
            "prompt",
            "commands starting with `curl`",
            "commands starting with `rm`",
            "permissions.json",
        ] {
            assert!(text.contains(needle), "missing on screen: {needle}\n{text}");
        }
    }

    /// The fourth review list --
    /// every DENY-CAPABLE hook rule (`pre_tool_use` AND `prompt_submitted`)
    /// refreshes into `state.hook_rules` on the same `/settings` seam as
    /// the other four mirrors, and renders with its id, event, matcher, and
    /// origin. An OBSERVATION-only rule (`post_tool_use`) in the SAME
    /// config must NOT appear -- pinning the scoping decision
    /// `Conway::active_deny_capable_hook_rules`'s own doc states.
    #[tokio::test]
    async fn hook_rules_are_visible_in_settings_scoped_to_deny_capable_events() {
        use conway::config::schema::HookEntry;

        let mut config = base_config();
        config.hooks = HooksConfig {
            rules: vec![
                HookEntry {
                    id: "deny-writes".to_string(),
                    event: "pre_tool_use".to_string(),
                    match_tool: Some("fs.write".to_string()),
                    command: vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "exit 1".to_string(),
                    ],
                    ..Default::default()
                },
                HookEntry {
                    id: "deny-prompts".to_string(),
                    event: "prompt_submitted".to_string(),
                    command: vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "exit 1".to_string(),
                    ],
                    ..Default::default()
                },
                HookEntry {
                    id: "log-every-call".to_string(),
                    event: "post_tool_use".to_string(),
                    command: vec!["/bin/sh".to_string(), "-c".to_string(), "true".to_string()],
                    ..Default::default()
                },
            ],
        };

        let backend: Arc<dyn conway::Backend> = Arc::new(FakeBackend::echo(BackendId::new("fake")));
        let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
        let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(conway::ModelRef {
            backend: BackendId::new("fake"),
            model: ModelId::new("echo-model"),
        }));
        let conway = ConwayBuilder::from_parts(config)
            .with_backend(backend)
            .with_session_store(Arc::new(FakeStore::new()))
            .with_permission_gate(gate)
            .with_router(router)
            .build()
            .expect("build should succeed with a hooks config and no runner injected");

        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let outcome = app
            .submit("/settings".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        assert_eq!(
            app.state.hook_rules.len(),
            2,
            "exactly the two deny-capable rules, not the observation-only \
             third: {:?}",
            app.state.hook_rules
        );
        let by_id = |id: &str| {
            app.state
                .hook_rules
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("missing hook rule {id}: {:?}", app.state.hook_rules))
        };
        let deny_writes = by_id("deny-writes");
        assert_eq!(deny_writes.event, "pre_tool_use");
        assert_eq!(deny_writes.match_tool.as_deref(), Some("fs.write"));
        let deny_prompts = by_id("deny-prompts");
        assert_eq!(deny_prompts.event, "prompt_submitted");
        assert_eq!(deny_prompts.match_tool, None);
        assert!(
            app.state
                .hook_rules
                .iter()
                .all(|r| r.id != "log-every-call"),
            "an observation-only rule must not appear: {:?}",
            app.state.hook_rules
        );

        // Renders on the real menu tree AND on screen.
        let rows = crate::tui::view::settings::build_tree(&app.state).rows();
        let joined = rows
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("deny-writes"), "{joined}");
        assert!(joined.contains("deny-prompts"), "{joined}");
        assert!(!joined.contains("log-every-call"), "{joined}");

        let text = crate::tui::test_support::render_text(&app.state, 200, 50);
        assert!(text.contains("hooks"), "{text}");
        assert!(text.contains("deny-writes"), "{text}");
    }

    // ---------------------------------------------------------------
    // Board item `01KZVZ5XV162XCQR96AQKCCCF7`: `/settings`, `/trust`,
    // `/agents`, `/ask` collapsed onto the SAME `commands::parse` ->
    // `commands::execute` dispatch every other command uses. Each command
    // below gets a MALFORMED-input test: the discriminating observable this
    // item exists to prove is that a malformed call for ANY of the four
    // never reaches its handler at all (no state mutation, no facade call)
    // -- which can only be true if the handler is gated by `commands::
    // parse`'s own `Result`, not by hand-rolled validation duplicated
    // ahead of it. "It works" (already covered by this file's own
    // `untrusted_file_deny_and_prompt_rules_are_visible_in_settings` and
    // `hook_rules_are_visible_in_settings_scoped_to_deny_capable_events`
    // for `/settings`, and `commands.rs`'s own `execute()`-level tests for
    // `/trust`/`/agents`/`/ask`) proves nothing on its own -- all four
    // worked before this item too, by bypassing the parser entirely.
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn settings_malformed_input_is_rejected_by_the_parser_before_any_refresh_or_open() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        assert!(!app.state.settings_open);

        let outcome = app
            .submit("/settings bogus".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        assert!(
            !app.state.settings_open,
            "a malformed /settings must never reach the handler that opens the menu"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text == "usage: /settings (no arguments)"
            )),
            "{:?}",
            app.state.transcript
        );
    }

    #[tokio::test]
    async fn agents_reaches_its_handler_through_the_parser_and_toggles_the_tree_view() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        assert!(!app.state.agent_view_open);

        let outcome = app
            .submit("/agents".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));
        assert!(
            app.state.agent_view_open,
            "/agents must toggle the tree view open"
        );
    }

    #[tokio::test]
    async fn agents_malformed_input_is_rejected_by_the_parser_and_never_toggles() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let outcome = app
            .submit("/agents foo".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        assert!(
            !app.state.agent_view_open,
            "a malformed /agents must never reach the handler that toggles the view"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text == "usage: /agents (no arguments)"
            )),
            "{:?}",
            app.state.transcript
        );
    }

    /// Drives `/trust permissions` through a REAL `App`/`Conway`/`LiveHost`
    /// (not `commands.rs`'s `FakeHost`), safely: `App::new`'s permission
    /// loader ALWAYS populates `state.permission_paths` with at least the
    /// project candidate -- present on disk or not (`Conway::
    /// load_permission_files`'s own doc: "every candidate path considered
    /// ... present or not"), so the old `permission_paths.is_empty()`
    /// no-op branch is realistically unreachable through `App::new`; that
    /// branch is instead covered, directly, at the `commands.rs::execute()`
    /// level in `trust_with_no_permission_paths_configured_is_a_notice_
    /// with_no_facade_call`. Pointed at a freshly created, empty tempdir
    /// with NO `.conway/permissions.json` on disk, so `Conway::
    /// preview_trust_target`'s OWN first step (`std::fs::read_to_string`)
    /// fails BEFORE ever reaching `TrustStore::trust` (the actual disk
    /// write, which only `Conway::trust_permission_file` performs, after a
    /// confirm this test never reaches) -- this test can never write to the
    /// real operator's own `~/.conway/trust.json`, unlike a genuinely
    /// successful trust would (see `LiveHost::trust_permission_file`'s own
    /// doc on why that path is deliberately left to `FakeHost` instead).
    /// The observable this test exists to prove is that the REAL, non-fake
    /// dispatch chain reaches `Conway::preview_trust_target` at all -- a
    /// genuine "file does not exist" `Entry::Error`, not a parser rejection
    /// and not a stub. Board item (split from
    /// `01KZHVFCN6ZEAXV7K5JHRQN1YB`): the failure text moved from "could
    /// not trust" to "could not read" because the read this test exercises
    /// now happens at the PREVIEW step, ahead of any trust decision -- see
    /// `commands::execute`'s `SlashCommand::Trust` arm.
    #[tokio::test]
    async fn trust_reaches_its_handler_through_the_parser_against_a_real_conway() {
        let project = tempfile::TempDir::new().expect("tempdir");
        let conway = echo_conway();
        let mut cli = minimal_cli();
        cli.cwd = Some(project.path().to_path_buf());
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        assert!(
            !app.state.permission_paths.is_empty(),
            "App::new always populates at least the project candidate, present or not"
        );

        let outcome = app
            .submit("/trust permissions".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, .. } if text.contains("could not read")
            )),
            "a real Conway must have reached the trust-preview handler and \
             reported the genuine 'file does not exist' failure, not a \
             stub: {:?}",
            app.state.transcript
        );
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a preview READ failure must never open the card"
        );
    }

    #[tokio::test]
    async fn trust_malformed_input_is_rejected_by_the_parser_before_any_facade_call() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let outcome = app
            .submit("/trust nope".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text == "usage: /trust permissions"
            )),
            "{:?}",
            app.state.transcript
        );
        assert!(
            !app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Notice { text } if text.contains("trusted"))),
            "a malformed /trust must never reach the handler that installs rules: {:?}",
            app.state.transcript
        );
    }

    #[tokio::test]
    async fn ask_malformed_input_is_rejected_by_the_parser_and_never_sets_ask_in_flight() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let outcome = app
            .submit("/ask".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));

        assert!(
            !app.state.ask_in_flight,
            "a malformed /ask (no question) must never reach the handler that spawns the task"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text == "usage: /ask <text>"
            )),
            "{:?}",
            app.state.transcript
        );
        assert!(
            app.modal_ask_rx
                .as_mut()
                .expect("modal_ask_rx is set by App::new")
                .try_recv()
                .is_err(),
            "nothing was ever spawned -- no task exists to reply at all"
        );
    }

    // ---------------------------------------------------------------
    // `/ask`'s three fates, driven end to end THROUGH the new dispatch:
    // `App::submit` -> `commands::parse` -> `commands::execute` ->
    // `Effect::RunModalAsk` -> `Self::spawn_modal_ask` -> the real spawned
    // task -> `modal_ask_rx` -> the modal opens -- exactly `App::run`'s own
    // path, minus the terminal. Each fate is its OWN test (module notes'
    // convention, and this item's own acceptance: "a test asserting /ask
    // 'works' does not distinguish them"), proving the dispatch refactor
    // changed none of the three: fork the child into a real, persistent
    // session; pull the answer into the parent and purge the child;
    // discard the child outright.
    // ---------------------------------------------------------------

    /// Drives `/ask <question>` through the REAL `submit` -> parser ->
    /// `commands::execute` -> `Effect::RunModalAsk` -> `Self::
    /// spawn_modal_ask` pipeline, waits for the spawned task's reply, and
    /// opens the modal exactly as `App::run`'s own `modal_ask_rx.recv()`
    /// arm does (`run.rs`) -- so each fate test below starts from the SAME
    /// state a real interactive `/ask` would. Returns the ephemeral
    /// child's `AgentId`.
    ///
    /// Board item `01M0RWFH6V709B7WTAFRZGFKG3` widened `modal_ask_rx`'s own
    /// message type from the bare final outcome to `ask::AskUpdate`
    /// (`Started` then `Done`) -- this helper drains the (now guaranteed)
    /// leading `Started` message first, exactly as `App::run`'s own arm
    /// does, before waiting on `Done`; nothing below this point changed.
    async fn drive_ask_to_modal(app: &mut App, question: &str) -> conway::AgentId {
        let outcome = app
            .submit(format!("/ask {question}"))
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, SubmitOutcome::Continue));
        assert!(
            app.state.ask_in_flight,
            "commands::execute's SlashCommand::Ask arm must set ask_in_flight \
             before Effect::RunModalAsk ever reaches Self::spawn_modal_ask"
        );

        let started = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.modal_ask_rx
                .as_mut()
                .expect("modal_ask_rx is set by App::new")
                .recv(),
        )
        .await
        .expect("the spawned /ask task must report AskUpdate::Started promptly")
        .expect("modal_ask_tx's sender half is alive for the duration of this call");
        let started_child = match started {
            ask::AskUpdate::Started { child } => child,
            ask::AskUpdate::Done(_) => {
                panic!("AskUpdate::Started must always precede AskUpdate::Done")
            }
        };
        app.state.ask_child = Some(started_child);

        let done = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.modal_ask_rx
                .as_mut()
                .expect("modal_ask_rx is set by App::new")
                .recv(),
        )
        .await
        .expect("the spawned /ask task must reply promptly")
        .expect("modal_ask_tx's sender half is alive for the duration of this call");
        let ask_outcome = match done {
            ask::AskUpdate::Done(outcome) => outcome,
            ask::AskUpdate::Started { .. } => {
                panic!("exactly one Started must precede exactly one Done")
            }
        };

        // Mirrors `App::run`'s own `modal_ask_rx.recv()` arm exactly.
        app.state.ask_in_flight = false;
        app.state.ask_child = None;
        app.state.ask_started_at = None;
        let child = ask_outcome
            .child
            .expect("SessionHandle::ask must succeed against the in-memory echo backend");
        assert_eq!(
            child, started_child,
            "the child id reported by Started must match the one Done reports"
        );
        app.state.offer_ask_modal(AskModal {
            question: ask_outcome.question,
            child,
            answer: ask_outcome.reply.unwrap_or_else(|e| format!("error: {e}")),
            error: None,
        });
        child
    }

    #[tokio::test]
    async fn ask_fate_fork_promotes_the_child_into_a_persistent_session() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let child = drive_ask_to_modal(&mut app, "should I merge this?").await;

        let host = commands::LiveHost {
            handle: &app.handle,
            conway: &app.conway,
            commands: &app.command_registry,
        };
        commands::apply_ask_fate(AskFate::Fork, &mut app.state, &host).await;

        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a successful Fork fate must close the modal, got: {:?}",
            app.state.mode
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("forked session") && text.contains("persistent")
            )),
            "{:?}",
            app.state.transcript
        );

        // B3: the child is genuinely no longer ephemeral -- it survives as
        // an ordinary, listed session, the whole point of this fate.
        let persistent = app
            .conway
            .sessions(conway::SessionFilter::default())
            .await
            .expect("sessions() should succeed");
        assert!(
            persistent.iter().any(|m| m.agent_id == child),
            "the promoted child must now be a listed, persistent session: {persistent:?}"
        );
    }

    #[tokio::test]
    async fn ask_fate_pull_in_merges_the_answer_and_purges_the_child() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let child = drive_ask_to_modal(&mut app, "what does this flag do?").await;

        let host = commands::LiveHost {
            handle: &app.handle,
            conway: &app.conway,
            commands: &app.command_registry,
        };
        commands::apply_ask_fate(AskFate::PullIn, &mut app.state, &host).await;

        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a successful PullIn fate must close the modal, got: {:?}",
            app.state.mode
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("pulled into the parent session")
            )),
            "{:?}",
            app.state.transcript
        );

        // B4: the child is purged (`SessionStore::remove`) as part of the
        // merge -- gone even from the ephemeral-inclusive listing.
        let all = app
            .conway
            .sessions(conway::SessionFilter {
                include_ephemeral: true,
                ..Default::default()
            })
            .await
            .expect("sessions() should succeed");
        assert!(
            all.iter().all(|m| m.agent_id != child),
            "the pulled-in child must be purged, not merely closed: {all:?}"
        );
    }

    /// This item's own load-bearing test (board: "pulling in an /ask answer
    /// wedges the status bar in a working state forever"). `pull_in` is a
    /// LOG operation, not an agent run -- it never emits `TurnStarted`, so
    /// nothing about it should ever leave `AppState::activity` reading as
    /// though a turn were in flight.
    ///
    /// Non-vacuous by construction, not merely by assertion shape: `App::new`
    /// leaves `activity` at its default `Idle`, and NOTHING in
    /// `drive_ask_to_modal` (the modal `/ask` flow runs on the CHILD's own
    /// forked session, never on `app.handle`'s stream) touches it either --
    /// so if this test's `drain_and_apply` below did not really exercise the
    /// merge's live `TextDelta` twin on the parent's OWN focused stream, the
    /// final assertion would trivially hold against broken code too. It does
    /// not: pre-fix, this test fails with `left: Responding, right: Idle`
    /// (captured verbatim in this item's own report) -- proof the fixture
    /// genuinely drives `activity` away from `Idle` before the fix makes it
    /// return.
    #[tokio::test]
    async fn ask_fate_pull_in_leaves_the_status_bar_idle_not_wedged_responding() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        assert_eq!(
            app.state.activity,
            Activity::Idle,
            "sanity: a fresh App starts idle"
        );

        let _child = drive_ask_to_modal(&mut app, "what time is it").await;
        assert_eq!(
            app.state.activity,
            Activity::Idle,
            "the modal /ask round trip runs on the child's OWN forked session, \
             never on app.handle's stream -- it must not have touched activity \
             either, or the assertion below would be vacuous"
        );

        // Subscribed BEFORE the fate call, mirroring `pull_in.rs`'s own
        // subscribe-first discipline (`crates/conway/tests/pull_in.rs:305`):
        // the merge's live twins are emitted synchronously during
        // `apply_ask_fate` itself, so a subscription taken afterward would
        // miss them on the broadcast bus.
        let mut events = app.handle.events();

        let host = commands::LiveHost {
            handle: &app.handle,
            conway: &app.conway,
            commands: &app.command_registry,
        };
        commands::apply_ask_fate(AskFate::PullIn, &mut app.state, &host).await;

        // The exact run-loop call: poll the parent's own stream to
        // `Poll::Pending` and apply every envelope through the REAL
        // `AppState::apply`.
        super::fixtures::drain_and_apply(&mut events, &mut app.state);

        // Enum assertion: this is `AppState::activity` returning to `Idle`
        // after the merge, not staying at its untouched default -- see the
        // sanity assertions above.
        assert_eq!(
            app.state.activity,
            Activity::Idle,
            "a pull-in merge starts no turn (no TurnStarted is ever emitted for \
             it), so it must never leave activity reading as though one were \
             still running"
        );

        // Render assertion (acceptance 3: the enum alone would not catch a
        // renderer reading a different field) -- the same
        // no-spinner-glyph shape `view/status.rs`'s own
        // `status_line_shows_no_elapsed_or_running_tokens_while_idle` uses.
        let text = crate::tui::test_support::render_text(&app.state, 80, 24);
        assert!(
            !crate::tui::state::SPINNER_FRAMES
                .iter()
                .any(|glyph| text.contains(glyph)),
            "the rendered status line must show no spinner glyph once the merge \
             has settled: {text}"
        );
    }

    #[tokio::test]
    async fn ask_fate_discard_purges_the_child_with_no_merge() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let child = drive_ask_to_modal(&mut app, "unused aside").await;

        let host = commands::LiveHost {
            handle: &app.handle,
            conway: &app.conway,
            commands: &app.command_registry,
        };
        commands::apply_ask_fate(AskFate::Discard, &mut app.state, &host).await;

        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a successful Discard fate must close the modal, got: {:?}",
            app.state.mode
        );
        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Notice { text } if text == "ask discarded")),
            "{:?}",
            app.state.transcript
        );

        let all = app
            .conway
            .sessions(conway::SessionFilter {
                include_ephemeral: true,
                ..Default::default()
            })
            .await
            .expect("sessions() should succeed");
        assert!(
            all.iter().all(|m| m.agent_id != child),
            "the discarded child must be purged: {all:?}"
        );
    }
}
