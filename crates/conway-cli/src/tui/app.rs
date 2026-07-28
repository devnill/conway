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
use super::view::{self, Theme};

/// The result of one spawned `/ask` task (B5 -- see [`App::submit`]'s
/// `/ask` branch and [`run_modal_ask`]). `child` is the ephemeral fork
/// child's `AgentId` (from `TurnHandle::agent`), the value the modal's
/// three fates need; it is `None` only when `SessionHandle::ask` itself
/// failed (no child was ever attached -- nothing to open a modal over, so
/// the failure becomes a plain transcript `Notice` instead).
struct ModalAskOutcome {
    question: String,
    child: Option<conway::AgentId>,
    reply: conway::Result<String>,
}

/// How long a lone `Ctrl-C` remains "armed" -- a second `Ctrl-C` within this
/// window exits 130; after it, a `Ctrl-C` is treated as a fresh first press
/// (module notes: "second within 2 s exits with 130").
const DOUBLE_CTRL_C_WINDOW: Duration = Duration::from_secs(2);
/// The app loop's redraw cap (module notes: "60 fps cap / redraw-on-change").
const REDRAW_TICK: Duration = Duration::from_millis(16);
/// T2 animation tick (8 TPS): advances the braille spinner frame and the
/// pulse-color index, and marks the frame dirty, ONLY while the focused
/// agent's `activity` is not `Idle`. An idle terminal is never redrawn by
/// this tick (the 16ms redraw tick still runs but is itself dirty-gated), so
/// idle cost stays flat. Additive to `REDRAW_TICK`, which is kept for
/// input/event responsiveness.
const ANIMATION_TICK: Duration = Duration::from_millis(125);

pub struct App {
    handle: conway::SessionHandle,
    state: AppState,
    // `/resume` (WI-115) needs `Conway::resume`, not just the current
    // `SessionHandle` -- cheap to hold (every field is `Arc`-backed, per
    // `Conway`'s own doc: "Cheap to `Clone`").
    conway: Conway,
    /// The TUI's resolved color/style table (T1): built once at startup from
    /// `[tui.theme]` config (defaults when the key is absent or a value is
    /// malformed -- P-10) and passed by reference into `view::draw` every
    /// frame. Decision D-T1: threaded as `&Theme`, not re-fetched via a
    /// call-site accessor or a global `Lazy`.
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
    /// T8: where [`Self::submit`] persists `state.history` to after every
    /// push -- `~/.conway/history` (or `$XDG_CONFIG_HOME/conway/history`
    /// when set), resolved once at `App::new` via
    /// `conway::config::discovery::history_file_path`. `None` only when
    /// that resolution itself fails (no resolvable home directory --
    /// `directories::BaseDirs::new()` returned `None`), in which case
    /// history still works for the running session (in-memory, via
    /// `AppState::history`), it just never round-trips to disk. P-10: this
    /// is a degrade, never a startup failure.
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
        let mut state = AppState::new(handle.root());
        // T1: build the theme once from the loaded `[tui.theme]` config
        // (defaults when the section is absent; malformed values fall back
        // to per-slot defaults -- P-10, never a panic). `Theme::from_config`
        // is infallible by construction.
        let theme = Theme::from_config(&conway.config().tui.theme);
        // T3: status-line field order/visibility from `[tui.status_line]`
        // (defaults to the Lean line when absent; unknown field names are
        // dropped at render time -- P-10, never a panic).
        state.status_line_config = conway.config().tui.status_line.clone();
        // T5: collapsed tool-preview line cap from
        // `[tui.tool_preview_lines]` (default 3). P-10: the config is
        // untrusted -- `clamp_tool_preview_lines` clamps to `1..=200` and
        // falls back to the default of 3 on a missing/out-of-range value.
        // Never a panic, no `unwrap`/`expect`/indexing on the config value.
        state.tool_preview_lines = super::state::clamp_tool_preview_lines(
            conway.config().tui.tool_preview_lines,
        );
        // T8: input-history cap from `[tui.history_size]` (default 500,
        // clamped the same P-10 way as `tool_preview_lines` just above),
        // then load whatever history already exists on disk -- best-effort
        // (`history::load` degrades to an empty history on a missing,
        // unreadable, or corrupt file, never a panic/startup failure -- see
        // that function's own doc). `history_file_path` itself can return
        // `None` (no resolvable home directory); the session still runs
        // with in-memory-only history in that case.
        state.history_cap =
            super::state::clamp_history_size(conway.config().tui.history_size);
        let history_path = conway::config::discovery::history_file_path(
            &std::env::vars().collect::<std::collections::HashMap<_, _>>(),
        );
        if let Some(path) = &history_path {
            state.history = super::history::load(path);
        }

        // V2b: load persisted permission rules from both scopes, project
        // first then global, and MERGE them.
        //
        // Merge rather than override: the two answer different questions.
        // A global rule is "I always allow this, everywhere" (`read:*`);
        // a project rule is "this checkout's build command is fine"
        // (`bash:cargo test`). Having the project file silently discard a
        // global grant would surprise an operator who set one deliberately,
        // and the union is still bounded by the metacharacter gate, which
        // applies to every rule regardless of where it came from.
        //
        // Every failure here is silent and narrowing: a missing file is
        // normal, and `parse_rules` already fails closed on a corrupt one
        // (returning no rules rather than erroring). Deliberately NOT
        // surfaced as a startup error — a broken rules file should cost
        // extra prompting, never a refusal to start.
        let permission_paths = conway::config::discovery::permission_file_paths(
            cli.cwd.as_deref().unwrap_or(&conway.config().cwd),
            &std::env::vars().collect::<std::collections::HashMap<_, _>>(),
        );
        let root_agent = state.root_agent();
        for path in &permission_paths {
            let Ok(contents) = std::fs::read_to_string(path) else {
                continue;
            };
            for rule in conway::permission_pattern::parse_rules(&contents) {
                conway.grant_permission_pattern(
                    rule,
                    conway::PermissionScope::Session,
                    root_agent,
                );
            }
        }
        state.permission_mode = conway.permission_mode();
        state.permission_paths = permission_paths;
        // T3: cwd display -- prefer the CLI `--cwd` override, fall back to
        // the config's `cwd`. Both are `PathBuf`; render the display string
        // via `display()` (lossy for non-UTF8).
        state.cwd_display = cli
            .cwd
            .as_ref()
            .or(Some(&conway.config().cwd))
            .map(|p| p.display().to_string());
        // T3: load the local model-metadata map (`[models.metadata_path]`)
        // once at startup so the status line's `ctx%` field can look up
        // the focused model's max context window by `"backend/model"`
        // string. Best-effort: a missing/unreadable file yields an empty
        // map, which makes the renderer fall back to raw tokens (no
        // percentage) -- never an error, never blocks startup.
        let metadata_path = conway
            .config()
            .cwd
            .join(&conway.config().models.metadata_path);
        if let Ok(metadata) = conway::config::model_metadata::load(&metadata_path) {
            state.model_max_context = metadata
                .models
                .iter()
                .map(|(k, v)| (k.clone(), v.max_context_tokens))
                .collect();
        }
        // T3: read the current git branch once at startup (best-effort,
        // no polling). On any failure (not a repo, git absent, non-UTF8
        // output) -> `None`, and the status line's `git` field is omitted.
        // Run on the blocking pool so it never stalls the async startup
        // path -- `git rev-parse` is fast, but the spawn isolates us from
        // a hung `git` or a slow filesystem.
        state.git_branch = read_git_branch().await;
        let (modal_ask_tx, modal_ask_rx) = mpsc::unbounded_channel();
        Ok(Self {
            handle,
            state,
            conway: conway.clone(),
            theme,
            modal_ask_tx,
            modal_ask_rx: Some(modal_ask_rx),
            history_path,
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
        let mut anim_ticker = tokio::time::interval(ANIMATION_TICK);
        let mut dirty = true;
        let mut last_ctrl_c: Option<Instant> = None;
        // Taken out of `self` once here (rather than borrowed from it inside
        // the loop below) so this `select!`'s `modal_ask_rx.recv()` arm and
        // the other arms' `&mut self.state` borrows don't conflict -- the
        // same reason `events`/`keys`/`ticker` are already locals, not
        // fields borrowed in place.
        let mut modal_ask_rx = self
            .modal_ask_rx
            .take()
            .expect("modal_ask_rx is set in App::new and taken exactly once, here");

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if dirty {
                        terminal.draw(|f| view::draw(&self.state, f, &self.theme))
                            .map_err(conway::ConwayError::Io)?;
                        dirty = false;
                    }
                }
                // T2: 125ms animation tick -- advances the spinner frame and
                // pulse-color index (wrapping) and marks the frame dirty,
                // ONLY while the focused agent's `activity` is not `Idle`.
                // An idle terminal never pays for animation: `should_animate`
                // gates the whole arm, so the counters don't advance and
                // `dirty` stays false (no redraw). The palette length comes
                // from the resolved `Theme` so a config-driven palette of a
                // different size still wraps correctly.
                _ = anim_ticker.tick() => {
                    if super::state::should_animate(&self.state.activity) {
                        self.state.tick_animation();
                        dirty = true;
                    }
                }
                maybe_ask = modal_ask_rx.recv() => {
                    if let Some(outcome) = maybe_ask {
                        self.state.ask_in_flight = false;
                        match outcome.child {
                            // The child's single turn is done -- open the
                            // modal over its answer and force the fate
                            // choice. A turn-level error still opens the
                            // modal (with the error text as the answer): the
                            // child exists and the user must still choose
                            // its fate (esc purges it, as ever).
                            Some(child) => {
                                let answer = outcome
                                    .reply
                                    .unwrap_or_else(|e| format!("error: {e}"));
                                self.state.offer_ask_modal(super::state::AskModal {
                                    question: outcome.question,
                                    child,
                                    answer,
                                    error: None,
                                });
                            }
                            // `SessionHandle::ask` itself failed: no child
                            // was ever attached, so there is nothing to
                            // fate -- a plain notice, no modal.
                            None => {
                                let err = outcome
                                    .reply
                                    .err()
                                    .map(|e| e.to_string())
                                    .unwrap_or_else(|| "unknown error".to_string());
                                self.state.transcript.push(super::state::Entry::Notice {
                                    text: format!("ask failed: {err}"),
                                });
                            }
                        }
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
                                conway::Event::AgentFinished { result, .. } => {
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
                                        parent,
                                        first_message,
                                    } => {
                                        // Seed the `/agents` node NOW, before
                                        // the subscription swap below drops the
                                        // parent stream (with the child's
                                        // buffered `AgentSpawned` still in it,
                                        // undrained). Without this the new
                                        // session is absent from the panel --
                                        // its spawn event reaches neither the
                                        // child's replay nor its live half.
                                        // See `AppState::ensure_agent_tracked`.
                                        self.state.ensure_agent_tracked(child, parent);
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
                                // V2b: install the grant, persist it
                                // best-effort, then resolve the pending
                                // prompt as an allow-once. The grant covers
                                // FUTURE matching calls; this one is allowed
                                // explicitly rather than relying on the
                                // pattern to re-authorize it, so the
                                // operator's decision takes effect even if
                                // installation somehow did not.
                                // V2b: the broker is the authority; the
                                // AppState copy is a display mirror. Both
                                // are written here, together, so the status
                                // line can never disagree with what
                                // actually gates calls.
                                Action::CyclePermissionMode => {
                                    let next = match self.conway.permission_mode() {
                                        conway::PermissionMode::Prompt => {
                                            conway::PermissionMode::Plan
                                        }
                                        conway::PermissionMode::Plan => {
                                            conway::PermissionMode::AutoAllow
                                        }
                                        _ => conway::PermissionMode::Prompt,
                                    };
                                    self.conway.set_permission_mode(next);
                                    self.state.permission_mode = next;
                                }
                                Action::RevokePermissionGrants => {
                                    self.conway.revoke_permission_grants();
                                    self.state.permission_grants.clear();
                                }
                                Action::GrantPermissionPattern(rule) => {
                                    let agent = self.state.focused_agent;
                                    self.conway.grant_permission_pattern(
                                        rule.clone(),
                                        conway::PermissionScope::Session,
                                        agent,
                                    );
                                    // Persistence is best-effort by design:
                                    // a write failure loses the rule's
                                    // durability, never the operator's
                                    // decision.
                                    persist_permission_rule(
                                        self.state.permission_paths.first(),
                                        &rule,
                                    );
                                    self.state.resolve_current_prompt(
                                        conway::PermissionDecision::AllowOnce,
                                    );
                                }
                                Action::AskFate(fate) => {
                                    // B5: exactly one facade op per fate,
                                    // via the same Host seam `commands::execute`
                                    // uses -- a failure keeps the modal open
                                    // with the error shown (see
                                    // `commands::apply_ask_fate`'s own doc).
                                    let host = commands::LiveHost {
                                        handle: &self.handle,
                                        conway: &self.conway,
                                    };
                                    commands::apply_ask_fate(fate, &mut self.state, &host).await;
                                }
                                Action::IntentConfirm(choice) => {
                                    // C2: the confirmation card's trust
                                    // gate. `execute_intent_confirm` runs the
                                    // classified/default recipe for
                                    // `Confirm`/`Manual` via `bare_fork`/
                                    // `bare_spawn` directly (returning
                                    // whatever `Effect` that produces --
                                    // typically `FocusNewSession`, wired
                                    // below exactly as a bare `/fork`/
                                    // `/spawn` would be) and is a no-op for
                                    // `Edit` (the key handler already
                                    // dropped the prompt into `state.input`
                                    // and closed the card).
                                    let host = commands::LiveHost {
                                        handle: &self.handle,
                                        conway: &self.conway,
                                    };
                                    match commands::execute_intent_confirm(
                                        choice,
                                        &mut self.state,
                                        &host,
                                    )
                                    .await
                                    {
                                        Effect::None => {}
                                        Effect::Quit => return Ok(ExitCode::Completed),
                                        Effect::Resumed(handle) => {
                                            self.handle = handle;
                                            events = self.handle.events();
                                        }
                                        Effect::FocusNewSession {
                                            child,
                                            parent,
                                            first_message,
                                        } => {
                                            // Same seed-then-focus-then-
                                            // deliver sequence as a bare
                                            // /fork//spawn -- see the
                                            // `Action::Submit` arm's own
                                            // `FocusNewSession` handling
                                            // for the rationale.
                                            self.state.ensure_agent_tracked(child, parent);
                                            let on_fail_extra =
                                                first_message.is_some().then_some(
                                                    "; your message was not sent",
                                                );
                                            if let Some(stream) = self
                                                .try_focus_agent(child, on_fail_extra)
                                                .await
                                            {
                                                events = stream;
                                                if let Some(text) = first_message {
                                                    self.deliver_first_message(child, text).await;
                                                }
                                            }
                                        }
                                    }
                                }
                                Action::CtrlC => {
                                    if let Some(code) = self.handle_ctrl_c(&mut last_ctrl_c).await? {
                                        return Ok(code);
                                    }
                                }
                                Action::Quit => {
                                    // B5: quitting with the /ask modal open
                                    // is the discard fate -- the child is
                                    // purged first, so there is no fourth,
                                    // fate-less way out of the modal.
                                    self.purge_open_ask_modal().await;
                                    return Ok(ExitCode::Completed);
                                }
                                Action::ScrollUp => self.page_scroll(terminal, true)?,
                                Action::ScrollDown => self.page_scroll(terminal, false)?,
                                // V3: bare Up/Down scroll one line. Shares
                                // `line_scroll` with the page variants so
                                // the clamp/follow-tail rules can never
                                // diverge between the two.
                                Action::ScrollLineUp => self.line_scroll(terminal, true)?,
                                Action::ScrollLineDown => self.line_scroll(terminal, false)?,
                                // T6: `End`/`Home` jump straight to the
                                // transcript's tail/top. `JumpToTail` needs
                                // no terminal-size-derived input at all
                                // (`AppState::jump_to_tail` just re-engages
                                // `follow_tail`); `JumpToTop` mirrors
                                // `page_scroll`/`line_scroll`'s own
                                // terminal-size lookup for `max_scroll`
                                // (kept for call-site symmetry -- see
                                // `AppState::jump_to_top`'s own doc on why
                                // the value itself goes unused).
                                Action::JumpToTail => self.state.jump_to_tail(),
                                Action::JumpToTop => self.jump_to_top(terminal)?,
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
                        // T8: bracketed paste (enabled in `tui/mod.rs`'s
                        // terminal setup) -- the whole pasted block arrives
                        // as one `Event::Paste(String)`, inserted as ONE
                        // edit at the cursor by `input::handle_paste`
                        // (rather than each character re-entering this
                        // `select!` loop as its own `CEvent::Key`, which is
                        // what happened before bracketed paste was enabled
                        // at all -- the terminal fell back to sending a
                        // paste as a flood of ordinary key events).
                        CEvent::Paste(text) => {
                            dirty = true;
                            input::handle_paste(&mut self.state, &text);
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
        // T8: every submitted line (prompt or slash command) is recorded
        // into the history FIFO before dispatch, so a slash command that
        // changes `self.handle`/exits the loop still recorded exactly what
        // the user typed. `AppState::push_history` is pure/in-memory and
        // bounds the deque to `state.history_cap` itself; persisting the
        // updated deque to disk is a SEPARATE, best-effort step (P-10: a
        // failed WRITE must never fail the submit it was recording, so the
        // `io::Result` is swallowed here, not `?`-propagated). Run on the
        // blocking pool -- same reasoning `App::new`'s `read_git_branch`
        // uses -- so a slow/contended filesystem never stalls the app loop.
        self.state.push_history(text.clone());
        if let Some(path) = self.history_path.clone() {
            let history = self.state.history.clone();
            let _ = tokio::task::spawn_blocking(move || super::history::save(&path, &history))
                .await;
        }
        // V2b: refresh the grant mirror before `/settings` renders its
        // review list. The broker is the authority; this copy exists so
        // the menu builder stays a pure function of `AppState`, and it
        // would be stale (or empty) if refreshed anywhere else.
        if text.trim() == "/settings" {
            self.state.permission_grants = self
                .conway
                .active_permission_patterns()
                .iter()
                .map(|rule| rule.describe())
                .collect();
            self.state.permission_mode = self.conway.permission_mode();
        }
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
            } else if self.state.ask_in_flight {
                // B5: the modal is a single-question surface -- one ask at
                // a time, never a pile-up competing for the one
                // `Mode::AskModal` slot.
                self.state.transcript.push(super::state::Entry::Notice {
                    text: "an /ask is already running -- wait for its answer".to_string(),
                });
            } else {
                self.state.ask_in_flight = true;
                let handle = self.handle.clone();
                let tx = self.modal_ask_tx.clone();
                tokio::spawn(async move {
                    let outcome = run_modal_ask(handle, question).await;
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
                            parent,
                            first_message,
                        } => {
                            return Ok(SubmitOutcome::FocusNewSession {
                                child,
                                parent,
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
    /// V3: a one-line transcript scroll, for bare `Up`/`Down` (which is
    /// what a terminal's alternate-scroll mode turns a wheel event into).
    /// Delegates to the same `scroll_page_up`/`scroll_page_down` state
    /// mutations with a page of 1, so the clamping and follow-tail
    /// re-engagement rules are literally the same code as the page-sized
    /// scroll -- one line is just a smaller page.
    fn line_scroll<B: Backend>(
        &mut self,
        terminal: &Terminal<B>,
        up: bool,
    ) -> conway::Result<()> {
        let size = terminal.size().map_err(conway::ConwayError::Io)?;
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let max = view::max_scroll(&self.state, area);
        if up {
            self.state.scroll_page_up(1, max);
        } else {
            self.state.scroll_page_down(1, max);
        }
        Ok(())
    }

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

    /// `Home` (T6): jumps the transcript straight to its own top. Delegates
    /// the actual mutation to `AppState::jump_to_top`, mirroring how
    /// `page_scroll` delegates to `scroll_page_up`/`scroll_page_down` --
    /// this method's only job is the terminal-size-derived `max_scroll`
    /// that pure method's signature takes (for call-site symmetry with the
    /// page-scroll pair; see `AppState::jump_to_top`'s own doc on why the
    /// value itself goes unused). `End`'s `Action::JumpToTail` needs no
    /// terminal size at all, so it calls `AppState::jump_to_tail` directly
    /// from the action-dispatch match instead of routing through a method
    /// here.
    fn jump_to_top<B: Backend>(&mut self, terminal: &Terminal<B>) -> conway::Result<()> {
        let size = terminal.size().map_err(conway::ConwayError::Io)?;
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let max = view::max_scroll(&self.state, area);
        self.state.jump_to_top(max);
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
                // B5: exiting with the /ask modal open purges its child
                // first, exactly like `Action::Quit` (see that arm).
                self.purge_open_ask_modal().await;
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

    /// B5's "no fourth way out": every quit path (`Action::Quit`, the
    /// double-`Ctrl-C` exit) funnels through here before leaving the app
    /// loop. If the `/ask` modal is open -- OR parked behind a permission
    /// prompt in `pending_ask_modal` (the two compete for the one modal
    /// slot, so at most one is present) -- its child is purged via
    /// `Conway::purge`. Quitting IS the discard fate (P-2/GP-10: purge
    /// only ever happens by an explicit user action, and quitting with the
    /// modal open is one). Best-effort: the process is exiting anyway, so
    /// a purge failure only leaves residue the NEXT startup's crash sweep
    /// (`Conway::sweep_stale_modal_asks`, wired in `tui::mod.rs`) reaps --
    /// it never blocks the exit.
    async fn purge_open_ask_modal(&mut self) {
        // The modal is either live (`Mode::AskModal`) or parked in
        // `pending_ask_modal` while a permission prompt is showing; take
        // the child from whichever holds it. Without the parked arm,
        // quitting while a prompt covered the modal would leave the child
        // for the next startup's sweep instead of discarding it now (M1).
        let live_child = if matches!(self.state.mode, super::state::Mode::AskModal(_)) {
            let modal = match std::mem::replace(&mut self.state.mode, super::state::Mode::Normal) {
                super::state::Mode::AskModal(m) => m,
                _ => unreachable!("guarded by the matches! check above"),
            };
            Some(modal.child)
        } else {
            None
        };
        let parked_child = self.state.take_pending_ask_modal().map(|m| m.child);
        for child in live_child.into_iter().chain(parked_child) {
            if let Err(e) = self.conway.purge(child).await {
                self.state.transcript.push(super::state::Entry::Notice {
                    text: format!("could not discard the /ask child on exit: {e}"),
                });
            }
        }
        // C2: drain a parked intent confirmation card on exit too. Unlike
        // the /ask modal there is no live child to purge (the card opens
        // BEFORE any agent is created -- quitting with the card open IS
        // the manual fallback), so this is just a drop-on-the-floor for
        // symmetry with `take_pending_ask_modal` above: it keeps the
        // parking slot empty rather than leaving a classified intent
        // dangling in `pending_intent_confirm` at process exit.
        let _ = self.state.take_pending_intent_confirm();
    }
}

/// Drives one `/ask` (B5) to completion: `SessionHandle::ask` forks an
/// ephemeral child (attaching it as a proper fork child of the asker --
/// post-B2, so its `AgentSpawned` reaches the `/agents` tree marked
/// `(ephemeral)`) and returns a `TurnHandle` over it (exactly like
/// `SessionHandle::prompt`, but scoped to that throwaway child); `text()`
/// drains it to the finished reply. The child's `AgentId`
/// (`TurnHandle::agent`) rides along in the outcome -- the modal's fates
/// all need it. A free function (not an `App` method) since it owns none
/// of `App`'s state -- it runs inside a `tokio::spawn`ed task that
/// outlives any single `submit` call, so it cannot borrow `self`.
async fn run_modal_ask(handle: SessionHandle, question: String) -> ModalAskOutcome {
    match handle.ask(question.clone()).await {
        Ok(turn) => {
            let child = turn.agent();
            let reply = turn.text().await;
            ModalAskOutcome {
                question,
                child: Some(child),
                reply,
            }
        }
        Err(e) => ModalAskOutcome {
            question,
            child: None,
            reply: Err(e),
        },
    }
}

/// T3: best-effort one-shot `git rev-parse --abbrev-ref HEAD` at startup,
/// returning the current branch name. `None` on any failure -- not a git
/// repo, `git` not on `PATH`, non-zero exit, non-UTF8 output, or a spawn
/// error. Never panics, never blocks startup on a hung `git`: the command
/// runs on the blocking pool and its output is bounded by `Command::output`
/// (which reads stdout into a buffer and waits for the child). C-04: no new
/// deps -- `std::process::Command` only.
/// V2b: appends `rule` to the permission file at `path`, best-effort.
///
/// Every failure path is a silent no-op. A rule that cannot be written
/// still applies to the running session — losing durability is a far
/// smaller harm than failing the operator's decision or, worse, tearing
/// down the session over a filesystem problem.
///
/// Read-modify-write rather than append: the file is JSON, so a bare
/// append would corrupt it. A corrupt or unreadable existing file is
/// treated as empty, which means a broken file gets replaced by a valid
/// one containing just this rule — the rules it could not parse were
/// already authorizing nothing (`parse_rules` fails closed), so nothing is
/// silently lost that was previously in force.
///
/// Writes via tmp-then-rename (`tui/history.rs`'s precedent) so a crash
/// mid-write cannot leave a half-written rules file.
fn persist_permission_rule(path: Option<&std::path::PathBuf>, rule: &conway::PatternRule) {
    let Some(path) = path else {
        return;
    };
    let mut file = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<conway::PermissionFile>(&c).ok())
        .unwrap_or_default();

    let wire = rule.to_wire();
    if file.allow.contains(&wire) {
        return;
    }
    file.allow.push(wire);

    let Ok(serialized) = serde_json::to_string_pretty(&file) else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, serialized).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, path);
}

async fn read_git_branch() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let branch = String::from_utf8(output.stdout).ok()?;
        let trimmed = branch.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
    .await
    .ok()
    .flatten()
}
