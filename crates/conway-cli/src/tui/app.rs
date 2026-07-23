//! The interactive app loop (WI-114): three tasks joined by channels (module
//! notes' architecture) -- the session's own `EventStream`, the gate's
//! `PendingPrompt` channel, and crossterm's key/resize stream -- driving one
//! [`AppState`] and redrawing at a capped rate.
//!
//! `run` is not unit-tested directly (it owns the real terminal and a live
//! `SessionHandle`); the pieces it composes (`state::apply`, `input::handle_key`,
//! `view::draw`, `gate::TuiGate`) are each unit-tested on their own, per the
//! module notes' guidance to avoid a real-PTY test.

use std::time::{Duration, Instant};

use conway::{Conway, RoleAlias, SessionHandle, SessionSpec};
use futures::StreamExt;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{Event as CEvent, EventStream as CrosstermEventStream};
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::cli::Cli;
use crate::exit::ExitCode;

use super::commands::{self, Effect};
use super::gate::GateReceiver;
use super::input::{self, Action};
use super::state::AppState;
use super::view;

/// The result of one spawned `/ask` task (see [`App::submit`]'s `/ask`
/// branch and [`run_ask`]), matched back to its [`super::state::Entry::EphemeralAsk`]
/// by `id`.
struct AskResult {
    id: u64,
    reply: conway::Result<String>,
}

/// How long a lone `Ctrl-C` remains "armed" -- a second `Ctrl-C` within this
/// window exits 130; after it, a `Ctrl-C` is treated as a fresh first press
/// (module notes: "second within 2 s exits with 130").
const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_secs(2);
/// The app loop's redraw cap (module notes: "60 fps cap / redraw-on-change").
const REDRAW_TICK: Duration = Duration::from_millis(16);

pub struct App {
    handle: conway::SessionHandle,
    state: AppState,
    // `/resume` (WI-115) needs `Conway::resume`, not just the current
    // `SessionHandle` -- cheap to hold (every field is `Arc`-backed, per
    // `Conway`'s own doc: "Cheap to `Clone`").
    conway: Conway,
    /// `/ask` (WI-127 criterion 5) spawns a `tokio::spawn`ed task per
    /// question (fork-ask, then drain the child's turn to completion via
    /// `TurnHandle::text` -- see [`run_ask`]) rather than folding it into
    /// `self.handle.events()`: the forked child is a DIFFERENT session, so
    /// its envelopes never arrive on that stream. `ask_tx` is cloned into
    /// each spawned task; `ask_rx` is taken out of `self` once, in `run`,
    /// and polled there as an extra `tokio::select!` arm.
    ask_tx: mpsc::UnboundedSender<AskResult>,
    ask_rx: Option<mpsc::UnboundedReceiver<AskResult>>,
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
}

impl App {
    /// Creates the interactive session. `--role-override` is the only
    /// `SessionSpec` field this item's flags reach -- `--model`/`--session`/
    /// `--resume`/`--fork-from` are `SessionSpec`-adjacent but belong to
    /// WI-117 (session continuity, out of this item's scope) and, for
    /// `--model`, to a `SessionSpec` field that does not exist yet (the
    /// facade has no model-pin field on this type today -- see
    /// `crates/conway/src/session_handle.rs`'s `SessionSpec`).
    pub async fn new(cli: &Cli, conway: &Conway) -> conway::Result<Self> {
        let spec = SessionSpec {
            role: cli.role_override.clone().map(RoleAlias::new),
            // The TUI drives one `SessionHandle::prompt` call per chat
            // message on the same handle/session for the app's whole
            // lifetime (`App::submit`, below) -- without this, the root
            // agent's task terminates after the FIRST message's turn and
            // every later message silently runs no turn (the confirmed
            // keep-alive bug; see `SessionSpec::keep_alive`'s own doc).
            keep_alive: true,
            ..SessionSpec::default()
        };
        let handle = conway.new_session(spec).await?;
        let state = AppState::new(handle.root());
        let (ask_tx, ask_rx) = mpsc::unbounded_channel();
        Ok(Self {
            handle,
            state,
            conway: conway.clone(),
            ask_tx,
            ask_rx: Some(ask_rx),
        })
    }

    /// Drives the app loop until the user quits, cancels twice, or a fatal
    /// error occurs. `terminal` is already in raw/alternate-screen mode
    /// (`tui::run` owns that lifecycle); this only ever draws to it.
    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut Terminal<B>,
        mut gate_rx: GateReceiver,
    ) -> conway::Result<ExitCode> {
        let mut events = self.handle.events();
        let mut keys = CrosstermEventStream::new();
        let mut ticker = tokio::time::interval(REDRAW_TICK);
        let mut dirty = true;
        let mut last_ctrl_c: Option<Instant> = None;
        // Taken out of `self` once here (rather than borrowed from it inside
        // the loop below) so this `select!`'s `ask_rx.recv()` arm and the
        // other arms' `&mut self.state` borrows don't conflict -- the same
        // reason `events`/`keys`/`ticker` are already locals, not fields
        // borrowed in place.
        let mut ask_rx = self
            .ask_rx
            .take()
            .expect("ask_rx is set in App::new and taken exactly once, here");

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if dirty {
                        terminal.draw(|f| view::draw(&self.state, f))
                            .map_err(conway::ConwayError::Io)?;
                        dirty = false;
                    }
                }
                maybe_ask = ask_rx.recv() => {
                    if let Some(AskResult { id, reply }) = maybe_ask {
                        let text = reply.unwrap_or_else(|e| format!("error: {e}"));
                        self.state.resolve_ephemeral_ask(id, text);
                        dirty = true;
                    }
                }
                maybe_env = events.next() => {
                    match maybe_env {
                        Some(env) => {
                            // WI-115's `/why` reads this back; `AppState::apply`
                            // (state.rs, out of this item's file scope) does
                            // not populate it -- see the field's own doc.
                            if matches!(env.event, conway::Event::ModelDecision { .. }) {
                                self.state.last_model_decision = Some(env.clone());
                            }
                            self.state.apply(&env);
                            dirty = true;
                        }
                        None => return Ok(ExitCode::Completed),
                    }
                }
                maybe_prompt = gate_rx.recv() => {
                    if let Some(prompt) = maybe_prompt {
                        self.state.offer_prompt(prompt);
                        dirty = true;
                    }
                }
                maybe_key = keys.next() => {
                    let ev = match maybe_key {
                        Some(Ok(ev)) => ev,
                        // Stream ended (detached tty): nothing more will
                        // ever arrive on this arm, so keep looping would
                        // busy-spin it forever. Shut down cleanly instead.
                        None => return Ok(ExitCode::Completed),
                        // A read error is not expected to recur productively
                        // either; treat it the same as a clean shutdown
                        // rather than spinning on it.
                        Some(Err(_)) => return Ok(ExitCode::Completed),
                    };
                    match ev {
                        CEvent::Key(key) => {
                            dirty = true;
                            match input::handle_key(&mut self.state, key) {
                                Action::None => {}
                                Action::Submit(text) => match self.submit(text).await? {
                                    SubmitOutcome::Continue => {}
                                    SubmitOutcome::Resubscribe => events = self.handle.events(),
                                    SubmitOutcome::Quit => return Ok(ExitCode::Completed),
                                },
                                Action::PermissionDecision(decision) => {
                                    self.state.resolve_current_prompt(decision);
                                }
                                Action::CtrlC => {
                                    if let Some(code) = self.handle_ctrl_c(&mut last_ctrl_c).await? {
                                        return Ok(code);
                                    }
                                }
                                Action::Quit => return Ok(ExitCode::Completed),
                                Action::ScrollUp => {
                                    self.state.scroll = self.state.scroll.saturating_sub(1);
                                }
                                Action::ScrollDown => {
                                    self.state.scroll = self.state.scroll.saturating_add(1);
                                }
                            }
                        }
                        CEvent::Resize(_, _) => dirty = true,
                        _ => {}
                    }
                }
            }
        }
    }

    /// Routes `text` to `commands::parse` + `commands::execute` when it
    /// starts with `/` (module notes: "the dispatch hook is defined here,
    /// handlers land in WI-115"); otherwise sends it as a prompt. A
    /// malformed or unknown slash command becomes a `Notice` -- it is never
    /// sent to the model (module notes' binding requirement, carried from
    /// WI-114's stub).
    ///
    /// `/ask` and `/agents` (WI-127 criteria 4 & 5) are intercepted HERE,
    /// before `commands::parse` ever sees them: `commands.rs` is out of
    /// this item's file scope, so its `SlashCommand`/`parse`/`execute` are
    /// left untouched, and both new commands are handled entirely within
    /// this in-scope method instead. Same invariant as every other slash
    /// command: neither ever reaches `commands::parse` as an "unknown
    /// command" error, and neither is ever sent to the model as a prompt.
    async fn submit(&mut self, text: String) -> conway::Result<SubmitOutcome> {
        if text.trim() == "/agents" || text.starts_with("/agents ") {
            if text.trim() == "/agents" {
                self.state.toggle_agent_view();
            } else {
                self.state.transcript.push(super::state::Entry::Notice {
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
                self.state.transcript.push(super::state::Entry::Notice {
                    text: "usage: /ask <text>".to_string(),
                });
            } else {
                let id = self.state.push_ephemeral_ask(question.clone());
                let handle = self.handle.clone();
                let tx = self.ask_tx.clone();
                tokio::spawn(async move {
                    let reply = run_ask(handle, question).await;
                    // The receiver only goes away when `App::run`'s loop
                    // has already exited -- nothing left to notify, so a
                    // send failure here is silently dropped rather than
                    // treated as an error.
                    let _ = tx.send(AskResult { id, reply });
                });
            }
            return Ok(SubmitOutcome::Continue);
        }
        if text.starts_with('/') {
            match commands::parse(&text) {
                Ok(cmd) => {
                    let host = commands::LiveHost {
                        handle: &self.handle,
                        conway: &self.conway,
                    };
                    match commands::execute(cmd, &mut self.state, &host).await {
                        Effect::None => {}
                        Effect::Quit => return Ok(SubmitOutcome::Quit),
                        Effect::Resumed(handle) => {
                            self.handle = handle;
                            return Ok(SubmitOutcome::Resubscribe);
                        }
                    }
                }
                Err(e) => {
                    self.state.transcript.push(super::state::Entry::Notice {
                        text: e.to_string(),
                    });
                }
            }
            return Ok(SubmitOutcome::Continue);
        }
        self.state
            .transcript
            .push(super::state::Entry::User(text.clone()));
        self.handle.prompt(text).await?;
        Ok(SubmitOutcome::Continue)
    }

    /// First `Ctrl-C`: cancel the running turn, arm the double-press
    /// window. Second `Ctrl-C` within [`DOUBLE_CTRL_C_WINDOW`]: exit 130.
    async fn handle_ctrl_c(
        &mut self,
        last_ctrl_c: &mut Option<Instant>,
    ) -> conway::Result<Option<ExitCode>> {
        let now = Instant::now();
        if let Some(prev) = *last_ctrl_c {
            if now.duration_since(prev) <= DOUBLE_CTRL_C_WINDOW {
                return Ok(Some(ExitCode::Interrupted));
            }
        }
        *last_ctrl_c = Some(now);
        // Best-effort: a cancel failure (e.g. nothing running) is not fatal
        // to the session -- surfaced as a notice, not a crash.
        if let Err(e) = self.handle.cancel(self.handle.root(), "user cancel").await {
            self.state.transcript.push(super::state::Entry::Notice {
                text: format!("cancel failed: {e}"),
            });
        }
        Ok(None)
    }
}

/// Drives one `/ask` (WI-127 criterion 5) to completion: `SessionHandle::ask`
/// forks an ephemeral child and returns a `TurnHandle` over it (exactly like
/// `SessionHandle::prompt`, but scoped to that throwaway child); `text()`
/// drains it to the finished reply. A free function (not an `App` method)
/// since it owns none of `App`'s state -- it runs inside a `tokio::spawn`ed
/// task that outlives any single `submit` call, so it cannot borrow `self`.
async fn run_ask(handle: SessionHandle, question: String) -> conway::Result<String> {
    let turn = handle.ask(question).await?;
    turn.text().await
}
