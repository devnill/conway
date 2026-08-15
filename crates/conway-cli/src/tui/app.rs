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
//! itself -- its fields, [`App::submit`] (the pre-`commands::parse`
//! dispatch entry point) -- is the one thing every seam below shares, so it
//! stays here, alongside [`SubmitOutcome`]. `App::session_spec`/`App::new`
//! (construction) live in [`startup`]; the event loop itself, [`App::run`],
//! lives in [`run`]; plugin-command execution in [`plugin_cmd`];
//! focus-switching in [`focus`]; terminal-size-derived scrolling in
//! [`viewport`]; `Ctrl-C`/quit handling in [`shutdown`]; the `/ask` modal's
//! own async completion in [`ask`]. Each submodule's own methods are
//! additional `impl App` blocks -- ordinary Rust, not a language feature --
//! so this split is purely organizational: `App` is exactly the same type,
//! with exactly the same fields and methods, as before it moved.
//!
//! **`submit` stays in THIS file, not a submodule, and that is deliberate.**
//! Four commands are intercepted by direct string comparison here, before
//! `commands::parse` ever runs -- see `submit`'s own doc. That is a known
//! structural defect (open item `01KZVZ5XV162XCQR96AQKCCCF7`, out of scope
//! for this behaviour-preserving split), and T9
//! (`crates/conway/tests/architecture_invariants.rs`'s
//! `t9_tui_has_exactly_the_four_known_parser_bypasses`) asserts exactly
//! those four by grepping THIS file's own source text. Moving `submit` to a
//! submodule would silently break that guard without changing anything it
//! is meant to catch, so the four interceptions -- and the whole of
//! `submit`, since splitting a single match arm out of its own function
//! would be its own kind of confusion -- stay here. The item that
//! eventually collapses them (Stage 5c) now has a smaller file to work in:
//! this one, ~450 lines instead of 3,492.

use tokio::sync::mpsc;

use crate::tui::commands::{self, Effect};
use crate::tui::state::{AppState, Entry};
use crate::tui::view::Theme;

mod ask;
mod focus;
mod plugin_cmd;
mod run;
mod shutdown;
mod startup;
mod viewport;

#[cfg(test)]
pub(super) mod fixtures;

use ask::ModalAskOutcome;
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
    modal_ask_tx: mpsc::UnboundedSender<ModalAskOutcome>,
    modal_ask_rx: Option<mpsc::UnboundedReceiver<ModalAskOutcome>>,
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
    /// T8: where [`Self::submit`] persists `state.history` to after every push
    /// -- `~/.conway/history` (or `$XDG_CONFIG_HOME/conway/history` when set),
    /// resolved once at `App::new` via
    /// `conway::config::discovery::history_file_path`. `None` only when that
    /// resolution itself fails (no resolvable home directory --
    /// `directories::BaseDirs::new()` returned `None`), in which case history
    /// still works for the running session (in-memory, via
    /// `AppState::history`), it just never round-trips to disk. The history
    /// file is untrusted input: this is a degrade, never a startup failure.
    history_path: Option<std::path::PathBuf>,
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
    /// **Four commands are intercepted HERE, before `commands::parse` ever
    /// sees them: `/settings`, `/trust`, `/agents`, `/ask`.** `commands.rs`
    /// is out of this item's file scope, so its `SlashCommand`/`parse`/
    /// `execute` are left untouched, and all four are handled entirely
    /// within this in-scope method instead. Same invariant as every other
    /// slash command: none of the four ever reaches `commands::parse` as an
    /// "unknown command" error, and none is ever sent to the model as a
    /// prompt. This is a known structural defect, not the intended shape --
    /// see item `01KZVZ5XV162XCQR96AQKCCCF7` (out of scope here, a
    /// behaviour change) and T9
    /// (`crates/conway/tests/architecture_invariants.rs`'s
    /// `t9_tui_has_exactly_the_four_known_parser_bypasses`), which pins the
    /// count at exactly four by grepping this file's own source text and
    /// fails on a fifth.
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
        // V2b: refresh the grant mirror before `/settings` renders its
        // review list. The broker is the authority; this copy exists so
        // the menu builder stays a pure function of `AppState`, and it
        // would be stale (or empty) if refreshed anywhere else.
        //
        // kept as `(rule, origin)`
        // pairs rather than pre-formatted strings -- `view/settings.rs::
        // build_tree` both labels each row (`[interactive]`/the
        // originating file's path, via `origin.describe()`) AND addresses
        // it for per-rule revocation,
        // which a bare formatted string could never do.
        if text.trim() == "/settings" {
            self.state.permission_grants = self.conway.active_permission_patterns();
            self.state.structured_allow_rules = self.conway.active_structured_allow_rules();
            self.state.permission_mode = self.conway.permission_mode();
            // The read-only deny/prompt review lists refresh on the same
            // seam. Unlike grants, these never change in-session (deny and
            // prompt rules install only at file load, from any file,
            // trusted or not -- there is no interactive path that adds
            // one), so this refresh exists to keep the menu builder a pure
            // function of `AppState`, not to track churn.
            self.state.permission_denies = self.conway.active_deny_permission_patterns();
            self.state.permission_prompts = self.conway.active_prompt_permission_patterns();
            self.state.structured_deny_rules = self.conway.active_structured_deny_rules();
            self.state.structured_prompt_rules = self.conway.active_structured_prompt_rules();
            // the fourth mirror,
            // refreshed on the SAME seam as the four above -- see this
            // block's own doc for why the refresh lives here rather than
            // anywhere else.
            self.state.hook_rules = self.conway.active_deny_capable_hook_rules();
        }
        // the ONLY path that writes
        // a trust record -- an explicit operator action, never automatic,
        // never a side effect of starting a session (D4 §5/§9). Trusts the
        // project-scoped candidate (`state.permission_paths`' first entry,
        // the same file `persist_permission_rule` already writes an
        // interactively-approved grant into) and installs its CURRENT
        // allow rules immediately, so the effect is visible in this
        // session rather than only on the next restart.
        if text.trim() == "/trust" || text.starts_with("/trust ") {
            let arg = text.strip_prefix("/trust").unwrap_or(&text).trim();
            if !arg.is_empty() && arg != "permissions" {
                self.state.transcript.push(Entry::Notice {
                    text: "usage: /trust permissions".to_string(),
                });
                return Ok(SubmitOutcome::Continue);
            }
            match self.state.permission_paths.first() {
                None => {
                    self.state.transcript.push(Entry::Notice {
                        text: "no project permissions file is configured to trust".to_string(),
                    });
                }
                Some(path) => {
                    let env_vars: std::collections::HashMap<String, String> =
                        std::env::vars().collect();
                    let root_agent = self.state.root_agent();
                    match self.conway.trust_permission_file(
                        &env_vars,
                        path,
                        conway::PermissionScope::Session,
                        root_agent,
                    ) {
                        Ok(report) => {
                            // B3: surface each registration error through the
                            // SAME `Entry::Error { fatal: false }` channel
                            // `load_permission_files`'s `registration_errors`
                            // uses, so it reaches the operator rather than
                            // being discarded -- a `paths_under` rule the
                            // broker dropped as uncanonicalizable must not be
                            // camouflaged as a routine notice.
                            for err in report.registration_errors {
                                self.state.transcript.push(Entry::Error {
                                    text: format!(
                                        "permission rule not installed: {} -- {}",
                                        err.rule.describe(),
                                        err.reason.describe()
                                    ),
                                    fatal: false,
                                });
                            }
                            // A4: surface each partial-inertness notice
                            // (today: a `command_prefix` rule selecting a
                            // mix of `Structured`- and `ShellCommand`-render
                            // tools) through the SAME `Entry::Notice`
                            // channel `load_permission_files`'s `notices`
                            // uses -- the rule installs (its `ShellCommand`
                            // members match), but the operator is warned the
                            // `Structured` members are inert.
                            for notice in report.notices {
                                self.state.transcript.push(Entry::Notice { text: notice });
                            }
                            self.state.transcript.push(Entry::Notice {
                                text: format!(
                                    "trusted {} -- {} allow rule(s) installed for this \
                                     session, and will load automatically until its \
                                     content next changes",
                                    path.display(),
                                    report.installed
                                ),
                            });
                        }
                        Err(e) => {
                            // this
                            // arm's most consequential case is
                            // `Conway::trust_permission_file` refusing a
                            // file that names an unrecognized top-level key
                            // -- the exact defect the startup loader's own
                            // `report.parse_errors` -> `Entry::Error`
                            // branch above exists to make loud. Reporting
                            // THAT specific case through `Entry::Notice`
                            // here (as this arm previously did, for every
                            // `Err`) would surface the identical failure at
                            // a WEAKER severity depending only on which of
                            // the two entry points hit it -- exactly the
                            // inconsistency this item's review found.
                            //
                            // Every other `Err` this arm can see (the file
                            // became unreadable between being listed and
                            // being trusted; `TrustStore::trust`'s write
                            // failed) is promoted the same way rather than
                            // split out: `/trust permissions` is an
                            // explicit operator action, so ANY failure to
                            // do what it says on the tin means the
                            // operator's belief ("I just trusted this
                            // file") and reality (nothing was recorded)
                            // have diverged -- the same camouflage risk
                            // `report.registration_errors` and
                            // `report.parse_errors` are surfaced through
                            // `Entry::Error` for just above, not a
                            // narrower one that would need a case split on
                            // `e`'s `ErrorKind` to keep the two failure
                            // classes apart here.
                            self.state.transcript.push(Entry::Error {
                                text: format!("could not trust {}: {e}", path.display()),
                                fatal: false,
                            });
                        }
                    }
                }
            }
            return Ok(SubmitOutcome::Continue);
        }
        if text.trim() == "/agents" || text.starts_with("/agents ") {
            if text.trim() == "/agents" {
                self.state.toggle_agent_view();
            } else {
                self.state.transcript.push(Entry::Notice {
                    text: "usage: /agents (no arguments)".to_string(),
                });
            }
            return Ok(SubmitOutcome::Continue);
        }
        if text.trim() == "/ask" || text.starts_with("/ask ") {
            let question = text
                .strip_prefix("/ask")
                .unwrap_or(&text)
                .trim()
                .to_string();
            if question.is_empty() {
                self.state.transcript.push(Entry::Notice {
                    text: "usage: /ask <text>".to_string(),
                });
            } else if self.state.ask_in_flight {
                // B5: the modal is a single-question surface -- one ask at
                // a time, never a pile-up competing for the one
                // `Mode::AskModal` slot.
                self.state.transcript.push(Entry::Notice {
                    text: "an /ask is already running -- wait for its answer".to_string(),
                });
            } else {
                self.state.ask_in_flight = true;
                let handle = self.handle.clone();
                let tx = self.modal_ask_tx.clone();
                tokio::spawn(async move {
                    let outcome = ask::run_modal_ask(handle, question).await;
                    // The receiver only goes away when `App::run`'s loop
                    // has already exited -- nothing left to notify, so a
                    // send failure here is silently dropped rather than
                    // treated as an error.
                    let _ = tx.send(outcome);
                });
            }
            return Ok(SubmitOutcome::Continue);
        }
        // V4: `/thinking` and `/timestamps` -- the state-only toggles that
        // used to be intercepted HERE (mirroring `/agents`'s pattern) --
        // are REMOVED, not aliased. Both are now a single `/settings` menu
        // (`view/settings.rs`), reached through the ordinary
        // `commands::parse`/`execute` path just below like any other
        // command with no live-facade special-casing need.
        if text.starts_with('/') {
            match commands::parse(&text) {
                Ok(cmd) => {
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
        // things that clear `Thinking`). Checked BEFORE the message is even
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
    use conway_core::fakes::{FakeBackend, FakeGate, FakeRouter, FakeStore};
    use conway_core::ids::{BackendId, ModelId};

    use super::fixtures::{
        base_config, build_conway_with_echo_backend, build_conway_with_echo_backend_and_store,
        build_conway_with_echo_backend_over, minimal_cli,
    };
    use super::{App, SubmitOutcome};
    use crate::tui::state::Entry;

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
        let (conway, store) = build_conway_with_echo_backend_and_store();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let other_sid = {
            let other_conway = build_conway_with_echo_backend_over(store.clone());
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

    /// This item's own end-to-end acceptance test: "a prompt appears exactly
    /// once in the transcript -- not zero, not twice" (the regression the
    /// removal of `submit`'s local `Entry::User` push risks).
    #[tokio::test]
    async fn submit_renders_the_prompt_exactly_once_not_zero_not_twice() {
        let conway = build_conway_with_echo_backend();
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
        let conway = build_conway_with_echo_backend();
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
        let conway = build_conway_with_echo_backend();
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
    /// never on an internal call count. No trust decision and no XDG
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

        let conway = build_conway_with_echo_backend();
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
}
