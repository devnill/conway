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

use conway::{Conway, RoleAlias, SessionHandle, SessionSpec, ToolSelector};
use futures::StreamExt;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{Event as CEvent, EventStream as CrosstermEventStream};
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::cli::Cli;
use crate::exit::ExitCode;

use super::commands::{self, Effect, Host};
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
    /// A bare `/spawn`/`/fork` succeeded (`commands::Effect::
    /// FocusNewSession`, WI "bare /spawn & /fork open an interactive
    /// session"): the run loop must focus `child` (the same live-facade
    /// resubscribe `Action::FocusAgent` already performs -- `submit` has no
    /// `events` local to do it itself) and, if `first_message` is `Some`,
    /// deliver it to `child` as that session's first prompt.
    FocusNewSession {
        child: conway::AgentId,
        first_message: Option<String>,
    },
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
            // The interactive root has no parent to `report` an
            // `AgentResult` to (decision 01KYB0BWY27DWB69NCNK85D56J: the
            // "pure and light" tool profile for interactive chat sessions)
            // -- excluding `report` makes the model answer plain chat
            // questions in text instead of hitting the permission gate for a
            // tool call nothing downstream ever unblocks. `conway_subagent`
            // and every other builtin tool stay available.
            tools: Some(ToolSelector::Except(vec!["report".into()])),
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
                            // Board item 01KYAGP11FF9YC3G60TWHHKKST: whether
                            // this envelope marks the end of a turn/agent
                            // for the FOCUSED agent specifically -- checked
                            // BEFORE `apply` consumes `env` below (`apply`
                            // takes `&env`, so `env` itself is still usable
                            // after, but the check reads more clearly ahead
                            // of the mutation it gates).
                            let refresh_focused_usage = match &env.event {
                                conway::Event::TurnFinished { .. } => {
                                    env.agent == self.state.focused_agent
                                }
                                conway::Event::AgentFinished { result } => {
                                    result.agent_id == self.state.focused_agent
                                }
                                _ => false,
                            };
                            self.state.apply(&env);
                            if refresh_focused_usage {
                                // `AppState::apply`'s own `TurnFinished` arm
                                // already live-incremented
                                // `focused_agent_usage` for immediate
                                // feedback -- this authoritative refetch
                                // overwrites it with the true total (see
                                // that field's own doc for why: a live
                                // increment can never reconcile a
                                // mid-turn focus switch, and replay carries
                                // no `Usage` at all). Best-effort: a failed
                                // fetch just leaves whatever figure was
                                // already showing.
                                let host = commands::LiveHost {
                                    handle: &self.handle,
                                    conway: &self.conway,
                                };
                                if let Ok(usage) =
                                    host.session_usage(self.state.focused_agent).await
                                {
                                    self.state.focused_agent_usage = usage;
                                }
                            }
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
                                    SubmitOutcome::FocusNewSession {
                                        child,
                                        first_message,
                                    } => {
                                        // Fix 3 (minor, review): if the
                                        // resubscribe itself fails, a
                                        // pending `first_message` would
                                        // otherwise be silently dropped --
                                        // the user only sees "could not
                                        // focus agent" with no indication
                                        // their typed text never reached
                                        // `child`. Folded into the SAME
                                        // notice (via `try_focus_agent`'s
                                        // `on_fail_extra`) rather than
                                        // attempting `deliver_first_message`
                                        // anyway: `agent_events` failing
                                        // here means this session's own
                                        // stream is not even resubscribed,
                                        // so sending into it would land the
                                        // message somewhere the user has no
                                        // live view of yet.
                                        let on_fail_extra =
                                            first_message.is_some().then_some(
                                                "; your message was not sent",
                                            );
                                        if let Some(stream) =
                                            self.try_focus_agent(child, on_fail_extra).await
                                        {
                                            events = stream;
                                            if let Some(text) = first_message {
                                                self.deliver_first_message(child, text).await;
                                            }
                                        }
                                    }
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
                                Action::ScrollUp => self.page_scroll(terminal, true)?,
                                Action::ScrollDown => self.page_scroll(terminal, false)?,
                                Action::ScrollLineUp => self.line_scroll(terminal, true)?,
                                Action::ScrollLineDown => self.line_scroll(terminal, false)?,
                                Action::FocusAgent(agent) => {
                                    // See `Self::try_focus_agent`'s own doc
                                    // for why this is fallible-but-matched
                                    // rather than `?`-propagated, and for
                                    // the ordering guarantee (resubscribe
                                    // before mutating focus) a failure
                                    // relies on.
                                    if let Some(stream) = self.try_focus_agent(agent, None).await {
                                        events = stream;
                                    }
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
                        Effect::FocusNewSession {
                            child,
                            first_message,
                        } => {
                            return Ok(SubmitOutcome::FocusNewSession {
                                child,
                                first_message,
                            });
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
        self.state
            .transcript
            .push(super::state::Entry::User(text.clone()));
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
                // Bug 2 fix (01KYAN9EQ5BRZQ0V3DCW590YCZ): mark the
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
                self.state.activity = super::state::Activity::Thinking;
            }
            Err(e) => {
                self.state.transcript.push(super::state::Entry::Notice {
                    text: format!("could not send message: {e}"),
                });
            }
        }
        Ok(SubmitOutcome::Continue)
    }

    /// Shared by `Action::FocusAgent` and `SubmitOutcome::FocusNewSession`
    /// (WI "bare /spawn & /fork open an interactive session"): resubscribes
    /// `agent`'s own event stream and switches `state`'s focus to it,
    /// returning the new stream for the run loop's `events` local to adopt.
    ///
    /// **Fallible-but-matched, not `?`-propagated (carried from the WI-140
    /// review fix this factors out of `Action::FocusAgent`'s old inline
    /// body):** `agent_events` can fail (unknown/foreign agent, store I/O,
    /// ancestry depth) -- surfaced as a `Notice`, returning `None`, rather
    /// than killing the whole interactive session over one bad focus
    /// switch.
    ///
    /// **Ordering matters:** `agent_events` is called (and must succeed)
    /// BEFORE `state.focus_agent` runs, so a failure leaves both the
    /// transcript and the caller's live subscription exactly as they were.
    ///
    /// Board item 01KYAGP11FF9YC3G60TWHHKKST: `focus_agent` already reset
    /// `focused_agent_usage` to zero -- the `session_usage` call below is
    /// the authoritative fetch that fills in the newly focused agent's REAL
    /// cumulative total (replay carries no `Usage`, so the zero reset would
    /// otherwise stick). Best-effort: a failed fetch just leaves it at zero
    /// rather than failing the whole focus switch.
    ///
    /// `on_fail_extra` (Fix 3, minor review finding): appended verbatim to
    /// the failure `Notice` when `agent_events` errors -- lets
    /// `SubmitOutcome::FocusNewSession`'s call site disclose that a pending
    /// first message was ALSO dropped, rather than silently losing it with
    /// no trace in the transcript. `None` for the plain `Action::FocusAgent`
    /// call site, which has no message riding along.
    async fn try_focus_agent(
        &mut self,
        agent: conway::AgentId,
        on_fail_extra: Option<&str>,
    ) -> Option<conway::EventStream> {
        match self.handle.agent_events(agent).await {
            Ok(stream) => {
                self.state.focus_agent(agent);
                let host = commands::LiveHost {
                    handle: &self.handle,
                    conway: &self.conway,
                };
                if let Ok(usage) = host.session_usage(agent).await {
                    self.state.focused_agent_usage = usage;
                }
                Some(stream)
            }
            Err(e) => {
                let mut text = format!("could not focus agent: {e}");
                if let Some(extra) = on_fail_extra {
                    text.push_str(extra);
                }
                self.state
                    .transcript
                    .push(super::state::Entry::Notice { text });
                None
            }
        }
    }

    /// `SubmitOutcome::FocusNewSession`'s own first-message delivery (WI
    /// "bare /spawn & /fork open an interactive session"): records `text`
    /// as a `User` transcript entry (mirroring `Self::submit`'s own plain-
    /// prompt tail) and sends it as `child`'s first turn via `prompt_agent`
    /// -- best-effort, a failure becomes a `Notice` rather than propagating
    /// (the new session was already successfully created and focused by
    /// this point; losing the whole focus switch over a failed first
    /// message would be worse than just reporting it).
    async fn deliver_first_message(&mut self, child: conway::AgentId, text: String) {
        self.state
            .transcript
            .push(super::state::Entry::User(text.clone()));
        match self.handle.prompt_agent(child, text).await {
            Ok(_) => self.state.activity = super::state::Activity::Thinking,
            Err(e) => self.state.transcript.push(super::state::Entry::Notice {
                text: format!("could not deliver the first message: {e}"),
            }),
        }
    }

    /// `PageUp`/`PageDown`: steps the transcript by ~one viewport page
    /// (`view::transcript_area`'s height, minus one line so the last row of
    /// the previous page stays in view for context -- floored at 1 so even
    /// a tiny terminal still moves). Delegates the actual scroll math to
    /// `AppState::scroll_page_up`/`scroll_page_down` (auto-follow
    /// disengage/re-engage, clamping) -- this method's only job is
    /// supplying the terminal-size-derived `max_scroll`/page inputs those
    /// pure methods need but don't have access to themselves.
    fn page_scroll<B: Backend>(
        &mut self,
        terminal: &Terminal<B>,
        page_up: bool,
    ) -> conway::Result<()> {
        let size = terminal.size().map_err(conway::ConwayError::Io)?;
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let transcript_area = view::transcript_area(&self.state, area);
        let max = view::max_scroll(&self.state, area);
        let page = transcript_area.height.saturating_sub(1).max(1);
        if page_up {
            self.state.scroll_page_up(page, max);
        } else {
            self.state.scroll_page_down(page, max);
        }
        Ok(())
    }

    /// Bare arrow `Up`/`Down` (01KYASZPVVRCHGTEAN9XS5C6EC): steps the
    /// transcript by exactly one line, unlike [`Self::page_scroll`]'s
    /// viewport-height jump -- in alt-screen the terminal reports
    /// touchpad/wheel scroll as arrow keys, so a light nudge must not jump a
    /// whole page. Delegates the clamp/follow-tail math to
    /// `AppState::scroll_line_up`/`scroll_line_down`, mirroring how
    /// `page_scroll` delegates to the page-sized pair -- this method's only
    /// job is the terminal-size-derived `max_scroll` those pure methods need
    /// but don't have access to themselves.
    fn line_scroll<B: Backend>(
        &mut self,
        terminal: &Terminal<B>,
        line_up: bool,
    ) -> conway::Result<()> {
        let size = terminal.size().map_err(conway::ConwayError::Io)?;
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let max = view::max_scroll(&self.state, area);
        if line_up {
            self.state.scroll_line_up(max);
        } else {
            self.state.scroll_line_down(max);
        }
        Ok(())
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
