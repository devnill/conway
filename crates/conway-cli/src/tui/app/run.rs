//! The interactive app loop itself: the `tokio::select!` that joins the
//! session's own `EventStream`, the gate's `PendingPrompt` channel,
//! crossterm's key/resize stream, and the `/ask`/plugin-command reply
//! channels, driving one `AppState` and redrawing at a capped rate.
//! Extracted out of `app.rs` verbatim (this item, board) -- `run` is not
//! unit-tested directly (it owns the real terminal and a live
//! `SessionHandle`); the pieces it composes (`state::apply`,
//! `input::handle_key`, `view::draw`, `gate::TuiGate`, and every `App`
//! method it calls) are each unit-tested on their own, per the module
//! notes' guidance to avoid a real-PTY test. The four pre-parser slash-
//! command interceptions `run` dispatches into via `self.submit(text)` stay
//! in `app.rs` itself, not here -- see that file's own module doc.

use std::time::{Duration, Instant};

use futures::StreamExt;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{Event as CEvent, EventStream as CrosstermEventStream};
use ratatui::Terminal;

use super::ask::AskUpdate;
use super::App;
use super::SubmitOutcome;
use crate::exit::ExitCode;
use crate::tui::commands::{self, Effect, Host};
use crate::tui::gate::GateReceiver;
use crate::tui::input::{self, Action};
use crate::tui::state::{should_animate, AskModal, Entry};
use crate::tui::view;

/// The app loop's redraw cap (module notes: "60 fps cap / redraw-on-change").
const REDRAW_TICK: Duration = Duration::from_millis(16);
/// T2 animation tick (8 TPS): advances the braille spinner frame and the
/// pulse-color index, and marks the frame dirty, ONLY while the focused
/// agent's `activity` is not `Idle`. An idle terminal is never redrawn by
/// this tick (the 16ms redraw tick still runs but is itself dirty-gated), so
/// idle cost stays flat. Additive to `REDRAW_TICK`, which is kept for
/// input/event responsiveness.
const ANIMATION_TICK: Duration = Duration::from_millis(125);

impl App {
    /// Drives the app loop until the user quits, cancels twice, or a fatal
    /// error occurs. `terminal` is already in raw/alternate-screen mode
    /// (`tui::run` owns that lifecycle); this only ever draws to it.
    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut Terminal<B>,
        mut gate_rx: GateReceiver,
    ) -> conway::Result<ExitCode>
    where
        // ratatui 0.30 widened `Backend::Error` from the fixed `io::Error` it
        // was in 0.29 to `B::Error: core::error::Error` (any backend, e.g.
        // `TestBackend`'s `Infallible`), so `conway::FacadeError::Io`
        // (`std::io::Error`-typed, not this crate's to widen) needs an
        // explicit `.into()` at each call site below instead of the old
        // direct `map_err(FacadeError::Io)`. `CrosstermBackend::Error` is
        // still `io::Error`, so this bound is satisfied trivially for the
        // only `B` this crate ever instantiates `run`/the scroll helpers
        // with.
        B::Error: Into<std::io::Error>,
    {
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
        // mirrors `modal_ask_rx`
        // exactly, same reasoning (this `select!`'s own `plugin_cmd_rx.recv()`
        // arm below and the other arms' `&mut self.state` borrows don't
        // conflict once this is a local rather than a field borrowed in
        // place).
        let mut plugin_cmd_rx = self
            .plugin_cmd_rx
            .take()
            .expect("plugin_cmd_rx is set in App::new and taken exactly once, here");

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if dirty {
                        terminal.draw(|f| view::draw(&self.state, f, &self.theme))
                            .map_err(|e| conway::FacadeError::Io(e.into()))?;
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
                // Board item `01M0RWFH6V709B7WTAFRZGFKG3`: the `||
                // self.state.ask_in_flight` half is what makes an in-flight
                // `/ask` visible at all -- `activity` is scoped to the
                // FOCUSED agent's own stream (this arm's own doc, `state/
                // status.rs`), and the ask's ephemeral child is never
                // focused, so before this the spinner simply never ticked
                // for it (the animation tick still ran every 125ms, but
                // `should_animate` gated the whole arm and nothing marked
                // the frame dirty). `view/status.rs::activity_ladder`
                // reads `ask_in_flight`/`ask_started_at` directly to render
                // the `⠋ asking… Ns` phrase this makes worth animating.
                _ = anim_ticker.tick() => {
                    if should_animate(&self.state.activity) || self.state.ask_in_flight {
                        self.state.tick_animation();
                        dirty = true;
                    }
                }
                maybe_ask = modal_ask_rx.recv() => {
                    if let Some(update) = maybe_ask {
                        match update {
                            // Board item `01M0RWFH6V709B7WTAFRZGFKG3`: the
                            // fork succeeded -- record the child so a
                            // keyboard abandon (`Action::CtrlC` ->
                            // `Self::handle_ctrl_c` -> `Self::abandon_ask`)
                            // has a target. If the operator already
                            // abandoned before this arrived (`ask_abandoned`
                            // was set with no child known yet -- `Self::
                            // abandon_ask`'s own doc), finish that job now:
                            // discard any pending prompt and cancel, the
                            // exact sequence `abandon_ask` itself runs when
                            // the child was already known.
                            AskUpdate::Started { child } => {
                                self.state.ask_child = Some(child);
                                if self.state.ask_abandoned {
                                    self.cancel_ask_child(child).await;
                                }
                                dirty = true;
                            }
                            AskUpdate::Done(outcome) => {
                                self.state.ask_in_flight = false;
                                self.state.ask_child = None;
                                self.state.ask_started_at = None;
                                let abandoned = std::mem::take(&mut self.state.ask_abandoned);
                                match (abandoned, outcome.child) {
                                    // Abandoned, and a child existed:
                                    // `Self::abandon_ask`/`Self::
                                    // cancel_ask_child` already cancelled it
                                    // and discarded any pending prompt --
                                    // but `AskUpdate::Done` arriving does
                                    // NOT by itself prove the agent tree
                                    // considers `child` terminal yet
                                    // (`TurnHandle::text`'s own drain-to-
                                    // event heuristic can resolve before the
                                    // tree's status flips -- measured
                                    // directly by this item's own tests, the
                                    // reason `await_agent` -- not a bare
                                    // `purge` attempt -- is what actually
                                    // confirms it below). A bare `purge`
                                    // here would risk reproducing the exact
                                    // `RuntimeError::Store(StoreError::
                                    // NotRemovable)` error this item was
                                    // filed over.
                                    (true, Some(child)) => {
                                        match tokio::time::timeout(
                                            Duration::from_secs(5),
                                            self.handle.await_agent(child),
                                        )
                                        .await
                                        {
                                            Ok(Ok(_)) => {
                                                if let Err(e) = self.conway.purge(child).await {
                                                    self.state.transcript.push(Entry::Notice {
                                                        text: format!(
                                                            "could not discard the abandoned \
                                                             /ask child: {e}"
                                                        ),
                                                    });
                                                } else {
                                                    self.state.transcript.push(Entry::Notice {
                                                        text: "ask abandoned".to_string(),
                                                    });
                                                }
                                            }
                                            // A genuinely bounded wait, not
                                            // an indefinite one -- the same
                                            // "never block the exit" spirit
                                            // `shutdown.rs::purge_open_ask_
                                            // modal`'s own doc states for a
                                            // purge failure: leftover
                                            // residue is reaped by the next
                                            // startup's own crash sweep
                                            // (`Conway::
                                            // sweep_stale_modal_asks`)
                                            // either way, so this never
                                            // blocks the app loop on
                                            // something that failed to wind
                                            // down promptly.
                                            Ok(Err(e)) => {
                                                self.state.transcript.push(Entry::Notice {
                                                    text: format!(
                                                        "ask abandoned, but its child's outcome \
                                                         could not be confirmed ({e}) -- it \
                                                         will be cleaned up on the next startup"
                                                    ),
                                                });
                                            }
                                            Err(_) => {
                                                self.state.transcript.push(Entry::Notice {
                                                    text: "ask abandoned, but its child is \
                                                           taking a while to stop -- it will be \
                                                           cleaned up on the next startup"
                                                        .to_string(),
                                                });
                                            }
                                        }
                                    }
                                    // Abandoned before the fork even
                                    // reported success (or it never did) --
                                    // nothing to purge.
                                    (true, None) => {}
                                    // The child's single turn is done -- open
                                    // the modal over its answer and force
                                    // the fate choice. A turn-level error
                                    // still opens the modal (with the error
                                    // text as the answer): the child exists
                                    // and the user must still choose its
                                    // fate (esc purges it, as ever).
                                    (false, Some(child)) => {
                                        let answer = outcome
                                            .reply
                                            .unwrap_or_else(|e| format!("error: {e}"));
                                        self.state.offer_ask_modal(AskModal {
                                            question: outcome.question,
                                            child,
                                            answer,
                                            error: None,
                                        });
                                    }
                                    // `SessionHandle::ask` itself failed: no
                                    // child was ever attached, so there is
                                    // nothing to fate -- a plain notice, no
                                    // modal.
                                    (false, None) => {
                                        let err = outcome
                                            .reply
                                            .err()
                                            .map(|e| e.to_string())
                                            .unwrap_or_else(|| "unknown error".to_string());
                                        self.state.transcript.push(Entry::Notice {
                                            text: format!("ask failed: {err}"),
                                        });
                                    }
                                }
                                dirty = true;
                            }
                        }
                    }
                }
                // the reply side of
                // `Effect::RunPluginCommand`'s spawned task (this loop's own
                // arm below) -- mirrors `modal_ask_rx.recv()` immediately
                // above in every structural respect (a spawned task's
                // eventual reply, never awaited directly here), which is the
                // load-bearing property behind "a hanging/panicking plugin
                // command cannot freeze the TUI": this `select!` iteration
                // never blocks on the plugin's own code, only on whichever
                // arm is ready first, exactly like every other arm here.
                maybe_plugin_cmd = plugin_cmd_rx.recv() => {
                    if let Some(done) = maybe_plugin_cmd {
                        // a
                        // `ForkSession` outcome swaps `self.handle` --
                        // resubscribe `events` exactly like `SubmitOutcome::
                        // Resubscribe`'s own call site does.
                        if self.apply_plugin_command_done(done).await {
                            events = self.handle.events();
                        }
                        dirty = true;
                    }
                }
                maybe_env = events.next() => {
                    match maybe_env {
                        Some(env) => {
                            // the `/why` reads this back; `AppState::apply`
                            // (state.rs, out of this item's file scope) does
                            // not populate it -- see the field's own doc.
                            // The OLD value shifts into `previous_model_
                            // decision` first (its own doc) so `/why` can
                            // report what changed after a `/model`/`/role`
                            // switch, not just the latest decision alone.
                            if matches!(env.event, conway::Event::ModelDecision { .. }) {
                                self.state.previous_model_decision =
                                    self.state.last_model_decision.take();
                                self.state.last_model_decision = Some(env.clone());
                            }
                            // whether
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
                            // the SAME
                            // check, scoped to `self.handle`'s own ROOT agent
                            // rather than whichever agent is focused -- see
                            // `AppState::session_head_seq`'s own doc for why
                            // those two can legitimately differ (`SessionStore`
                            // keys one session per agent, so only the root
                            // agent's own turns land in THIS session's log).
                            let refresh_session_head = match &env.event {
                                conway::Event::TurnFinished { .. } => {
                                    env.agent == self.handle.root()
                                }
                                conway::Event::AgentFinished { result, .. } => {
                                    result.agent_id == self.handle.root()
                                }
                                _ => false,
                            };
                            self.state.apply(&env);
                            if refresh_session_head {
                                self.refresh_session_head().await;
                            }
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
                                    commands: &self.command_registry,
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
                                    // The cycle is the three closed core
                                    // modes plus whatever installed plugins
                                    // declared, so this is no longer a
                                    // three-way switch. `Conway::
                                    // cycle_permission_mode` writes the
                                    // enforced mode and the display identity
                                    // together and hands back the entry it
                                    // moved to -- mirrored rather than
                                    // recomputed here, because computing the
                                    // answer a second time is how the status
                                    // line and the broker drift apart.
                                    let entry = self.conway.cycle_permission_mode();
                                    self.state.permission_mode = entry.base();
                                    self.state.active_declared_mode =
                                        entry.declared_ref().map(|r| (r.plugin_id, r.name));
                                }
                                Action::RevokePermissionGrants => {
                                    self.conway.revoke_permission_grants();
                                    self.state.permission_grants.clear();
                                    // The broker's revoke-all drops EVERY
                                    // allow grant, structured rules
                                    // included -- both mirrors must clear
                                    // or the menu would render stale rows
                                    // for rules that no longer authorize.
                                    self.state.structured_allow_rules.clear();
                                }
                                // // revoke exactly the one grant the operator
                                // selected. The broker's in-memory grant is
                                // dropped unconditionally before any file
                                // I/O is attempted (`Conway::
                                // revoke_permission_pattern`'s own doc) --
                                // revocation never fails open, so the
                                // mirror is refreshed from the broker
                                // (the authority) regardless of how
                                // persistence went, and the operator is
                                // told the whole truth via a transcript
                                // notice rather than a silent no-op.
                                Action::RevokePermissionPattern(rule, origin) => {
                                    let env_vars: std::collections::HashMap<String, String> =
                                        std::env::vars().collect();
                                    let outcome = self.conway.revoke_permission_pattern(
                                        &env_vars, &rule, &origin,
                                    );
                                    self.state.permission_grants =
                                        self.conway.active_permission_patterns();
                                    let text = match outcome {
                                        conway::RevokeOutcome::NotFound => {
                                            "that grant was already gone".to_string()
                                        }
                                        conway::RevokeOutcome::RevokedNoFile => {
                                            format!("revoked: {}", rule.describe())
                                        }
                                        conway::RevokeOutcome::RevokedAndPersisted {
                                            retrust_warning: None,
                                        } => format!(
                                            "revoked and removed from {}: {}",
                                            origin.describe(),
                                            rule.describe()
                                        ),
                                        conway::RevokeOutcome::RevokedAndPersisted {
                                            retrust_warning: Some(warning),
                                        } => format!(
                                            "revoked and removed from {}: {} -- {warning}",
                                            origin.describe(),
                                            rule.describe()
                                        ),
                                        conway::RevokeOutcome::RevokedButPersistFailed {
                                            error,
                                        } => format!(
                                            "revoked for this session, but could not update \
                                             {} ({error}) -- it may come back at the next \
                                             restart",
                                            origin.describe()
                                        ),
                                    };
                                    self.state
                                        .transcript
                                        .push(Entry::Notice { text });
                                }
                                // A2: the structured-allow
                                // counterpart of the arm above -- revoke
                                // exactly the one structured rule the
                                // operator selected, through the
                                // Rule-identity facade method (the flat
                                // revoke cannot name a structured rule).
                                // Same guarantees: the broker's in-memory
                                // grant is dropped before any file I/O
                                // (revocation never fails open), the mirror
                                // is refreshed from the broker regardless,
                                // and the outcome is reported whole.
                                Action::RevokeStructuredAllowRule(rule, origin, scope) => {
                                    let env_vars: std::collections::HashMap<String, String> =
                                        std::env::vars().collect();
                                    let outcome = self.conway.revoke_structured_allow_rule(
                                        &env_vars, &rule, &origin, &scope,
                                    );
                                    self.state.structured_allow_rules =
                                        self.conway.active_structured_allow_rules();
                                    let text = match outcome {
                                        conway::RevokeOutcome::NotFound => {
                                            "that grant was already gone".to_string()
                                        }
                                        conway::RevokeOutcome::RevokedNoFile => {
                                            format!("revoked: {}", rule.describe())
                                        }
                                        conway::RevokeOutcome::RevokedAndPersisted {
                                            retrust_warning: None,
                                        } => format!(
                                            "revoked and removed from {}: {}",
                                            origin.describe(),
                                            rule.describe()
                                        ),
                                        conway::RevokeOutcome::RevokedAndPersisted {
                                            retrust_warning: Some(warning),
                                        } => format!(
                                            "revoked and removed from {}: {} -- {warning}",
                                            origin.describe(),
                                            rule.describe()
                                        ),
                                        conway::RevokeOutcome::RevokedButPersistFailed {
                                            error,
                                        } => format!(
                                            "revoked for this session, but could not update \
                                             {} ({error}) -- it may come back at the next \
                                             restart",
                                            origin.describe()
                                        ),
                                    };
                                    self.state
                                        .transcript
                                        .push(Entry::Notice { text });
                                }
                                // // revoke exactly the one hook-backed rule
                                // the operator selected. `Conway::
                                // revoke_hook_rule` mutates the broker/
                                // dispatcher directly and NEVER attempts
                                // file I/O -- a hook rule has no
                                // `PatternOrigin` to persist a removal
                                // into (that method's own doc) -- so, unlike
                                // the two arms above, there is only one
                                // outcome to report, not an enum of them.
                                Action::RevokeHookRule(event, id) => {
                                    let revoked = self.conway.revoke_hook_rule(&event, &id);
                                    self.state.hook_rules =
                                        self.conway.active_deny_capable_hook_rules();
                                    let text = if revoked {
                                        format!("revoked hook rule `{id}` on `{event}`")
                                    } else {
                                        "that hook rule was already gone".to_string()
                                    };
                                    self.state
                                        .transcript
                                        .push(Entry::Notice { text });
                                }
                                // Board item `01M0KARX71A64NTSYTDBVANVPF`:
                                // the write itself lives in
                                // `App::apply_plugin_toggle`
                                // (`app/plugin_toggle.rs`), factored out so
                                // it is directly testable with no real
                                // terminal/`select!` loop -- mirrors
                                // `Self::apply_plugin_command_done`'s own
                                // shape one screen over. `env_vars` is
                                // collected HERE (never inside the method
                                // itself, which takes it as a plain
                                // parameter) for the SAME hermetic-testing
                                // reason `Action::RevokePermissionPattern`'s
                                // own arm above already collects its own
                                // copy.
                                Action::TogglePlugin(plugin_id, installed) => {
                                    let env_vars: std::collections::HashMap<String, String> =
                                        std::env::vars().collect();
                                    self.apply_plugin_toggle(
                                        plugin_id,
                                        installed,
                                        &env_vars,
                                        &std::env::current_dir()
                                            .unwrap_or_else(|_| std::path::PathBuf::from(".")),
                                    );
                                }
                                Action::GrantPermissionPattern(rule, scope) => {
                                    // The granting agent is the one whose
                                    // call is being decided -- NOT
                                    // `focused_agent`. For a Session grant
                                    // the identity is ignored, but an
                                    // Agent/AgentSubtree grant narrows to
                                    // this id, and the prompt's requester
                                    // need not be the focused agent.
                                    let agent = self
                                        .state
                                        .pending_permission_agent()
                                        .unwrap_or(self.state.focused_agent);
                                    self.conway.grant_permission_pattern(rule.clone(), scope, agent);
                                    // Persistence is best-effort by design:
                                    // a write failure loses the rule's
                                    // durability, never the operator's
                                    // decision. Session scope only: an
                                    // Agent/AgentSubtree grant names LIVE
                                    // agent ids, meaningless to a file read
                                    // at the next launch -- persisting it
                                    // would silently WIDEN it to the load
                                    // scope on restart, the opposite of the
                                    // narrowing the operator asked for.
                                    if scope == conway::PermissionScope::Session {
                                        persist_permission_rule(
                                            self.state.permission_paths.first(),
                                            &rule,
                                        );
                                    }
                                    self.state.resolve_current_prompt(
                                        conway::PermissionDecision::AllowOnce,
                                    );
                                }
                                Action::GrantPermissionRule(rule, scope) => {
                                    // The structured-argument counterpart to
                                    // [`Action::GrantPermissionPattern`]:
                                    // installs a `When::ArgsMatch` allow
                                    // rule (built by the `[p]` field editor
                                    // from the pinned fields) covering FUTURE
                                    // calls, then resolves THIS call as
                                    // `AllowOnce`. The granting agent is the
                                    // prompt's requester (not
                                    // `focused_agent`), same reasoning as
                                    // the flat-pattern arm above.
                                    let agent = self
                                        .state
                                        .pending_permission_agent()
                                        .unwrap_or(self.state.focused_agent);
                                    self.conway
                                        .grant_permission_rule(rule.clone(), scope, agent);
                                    // Persistence, mirroring the flat-pattern
                                    // arm above exactly: best-effort, silent
                                    // either way, session scope only (an
                                    // Agent/AgentSubtree grant names LIVE
                                    // agent ids, meaningless to a file read at
                                    // the next launch -- persisting it would
                                    // silently WIDEN it to the load scope on
                                    // restart). Unlike the flat form, this
                                    // writes into `permissions.json`'s
                                    // structured `rules` array (F12) rather
                                    // than the flat `allow` list -- the SAME
                                    // array a file-loaded `paths_under` rule
                                    // already round-trips through end to end
                                    // (`conway/tests/structured_rule_seam.rs`),
                                    // so an `ArgsMatch` rule needs no new wire
                                    // format, only this write path.
                                    if scope == conway::PermissionScope::Session {
                                        persist_permission_structured_rule(
                                            self.state.permission_paths.first(),
                                            &rule,
                                        );
                                    }
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
                                        commands: &self.command_registry,
                                    };
                                    commands::apply_ask_fate(fate, &mut self.state, &host).await;
                                }
                                Action::TrustDecision(decision) => {
                                    // Board item (split from
                                    // `01KZHVFCN6ZEAXV7K5JHRQN1YB`): the
                                    // trust-preview card's confirm/cancel,
                                    // via the SAME `Host` seam every other
                                    // facade call uses -- a failed confirm
                                    // keeps the card open with the error
                                    // shown (see
                                    // `commands::apply_trust_decision`'s own
                                    // doc), mirroring `Action::AskFate` just
                                    // above exactly.
                                    let host = commands::LiveHost {
                                        handle: &self.handle,
                                        conway: &self.conway,
                                        commands: &self.command_registry,
                                    };
                                    commands::apply_trust_decision(decision, &mut self.state, &host)
                                        .await;
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
                                        commands: &self.command_registry,
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
                                        // Structurally unreachable from THIS
                                        // call site today -- `execute_intent_
                                        // confirm` only ever returns
                                        // `Effect::None`/`FocusNewSession`
                                        // (it dispatches via `bare_fork`/
                                        // `bare_spawn` directly, never through
                                        // `execute`'s own `SlashCommand::
                                        // Plugin` arm, the only place this
                                        // variant is constructed). Handled
                                        // anyway, correctly, rather than with
                                        // a wildcard drop: `Effect` is one
                                        // enum shared by every dispatch site,
                                        // and a silently dropped plugin
                                        // command would be the exact "silent
                                        // loss" this item's own acceptance
                                        // criterion forbids, should a future
                                        // change ever route one here.
                                        Effect::RunPluginCommand(invocation) => {
                                            self.spawn_plugin_command(invocation);
                                        }
                                        // Structurally unreachable from THIS
                                        // call site for the same reason
                                        // `RunPluginCommand` just above is:
                                        // `execute_intent_confirm` only ever
                                        // returns `None`/`FocusNewSession`.
                                        // Handled correctly anyway, mirroring
                                        // `RunPluginCommand`'s own comment.
                                        Effect::RunModalAsk { question } => {
                                            self.spawn_modal_ask(question);
                                        }
                                        // Structurally unreachable from THIS
                                        // call site for the same reason
                                        // `RunPluginCommand`/`RunModalAsk`
                                        // just above are: `execute_intent_
                                        // confirm` only ever returns `None`/
                                        // `FocusNewSession`, never a
                                        // `SlashCommand::Plugins` action.
                                        // Handled correctly anyway, mirroring
                                        // both comments just above.
                                        Effect::RunMarketplaceInstall {
                                            marketplace_url,
                                            plugin_id,
                                        } => {
                                            let env = self.env.clone();
                                            let cwd = self.cwd.clone();
                                            self.apply_marketplace_install(
                                                marketplace_url,
                                                plugin_id,
                                                &env,
                                                &cwd,
                                            )
                                            .await;
                                        }
                                        Effect::RunMarketplaceUninstall { plugin_id } => {
                                            let env = self.env.clone();
                                            let cwd = self.cwd.clone();
                                            self.apply_marketplace_uninstall(plugin_id, &env, &cwd);
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
}

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

/// The structured counterpart to [`persist_permission_rule`]: appends
/// `rule` (a [`conway::Rule`] built by the `[p]` field editor -- an
/// `ArgsMatch` allow rule today, but this takes any `Rule`) to the
/// permission file at `path`'s structured `rules` array, best-effort,
/// tmp-then-rename.
///
/// This needs no new wire format: `permissions.json`'s `rules` array (F12)
/// already carries an arbitrary [`conway::Rule`] via `serde`'s ordinary
/// derive, and `Conway::load_permission_files` already installs whatever
/// `When` variant it finds there through the SAME generic path a
/// `paths_under` file rule uses -- proven end to end in
/// `conway/tests/structured_rule_seam.rs`. So an `ArgsMatch` rule granted
/// here round-trips through exactly that path at the next launch; this
/// function only had to exist, not invent anything new to write.
///
/// Same failure posture as [`persist_permission_rule`] throughout: every
/// failure path is a silent no-op (a rule that cannot be written still
/// applies to the running session); a corrupt or unreadable existing file
/// is treated as empty; matches by `Rule` equality (not a wire string, which
/// a structured rule has none of) so granting the identical rule twice
/// never duplicates the file entry.
fn persist_permission_structured_rule(path: Option<&std::path::PathBuf>, rule: &conway::Rule) {
    let Some(path) = path else {
        return;
    };
    let mut file = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<conway::PermissionFile>(&c).ok())
        .unwrap_or_default();

    if file.rules.contains(rule) {
        return;
    }
    file.rules.push(rule.clone());

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

#[cfg(test)]
mod persist_tests {
    //! Unit coverage for [`persist_permission_structured_rule`], the write
    //! half of the `[p]` field editor's durability round-trip (board item
    //! `01M0EMDVBJVT510GBJHPWBZ3G6`). `run` itself owns a real terminal and
    //! is not unit-tested (see this module's own doc); this free function
    //! needs none of that, so it gets tested directly here, same as its
    //! sibling `persist_permission_rule` should be (a pre-existing gap this
    //! item did not expand to cover).
    //!
    //! The headline test re-parses the written file through
    //! [`conway::permission_pattern::parse_rules`] -- the SAME parser
    //! `Conway::load_permission_files` calls at the next launch -- rather
    //! than hand-inspecting the JSON, so this proves the real round trip,
    //! not just that some bytes landed on disk.
    use super::persist_permission_structured_rule;
    use std::collections::BTreeMap;

    fn args_match_allow_rule() -> conway::Rule {
        let mut pinned = BTreeMap::new();
        pinned.insert(
            "path".to_string(),
            serde_json::Value::String("/etc/hosts".to_string()),
        );
        conway::Rule::args_match_allow_rule("read", pinned)
    }

    #[test]
    fn a_granted_structured_rule_round_trips_through_the_real_file_parser() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("permissions.json");
        let rule = args_match_allow_rule();

        persist_permission_structured_rule(Some(&path), &rule);

        let contents = std::fs::read_to_string(&path).expect("file must be written");
        let parsed = conway::permission_pattern::parse_rules(&contents);
        assert_eq!(
            parsed,
            vec![rule],
            "the written file must re-parse, through the real loader-facing parser, to \
             exactly the rule that was granted: {contents}"
        );
    }

    #[test]
    fn granting_the_identical_rule_twice_does_not_duplicate_the_file_entry() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("permissions.json");
        let rule = args_match_allow_rule();

        persist_permission_structured_rule(Some(&path), &rule);
        persist_permission_structured_rule(Some(&path), &rule);

        let contents = std::fs::read_to_string(&path).expect("file must be written");
        let parsed = conway::permission_pattern::parse_rules(&contents);
        assert_eq!(
            parsed.len(),
            1,
            "granting the same rule twice must not duplicate it"
        );
    }

    #[test]
    fn a_second_distinct_rule_is_appended_alongside_the_first() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("permissions.json");
        let first = args_match_allow_rule();
        let mut pinned = BTreeMap::new();
        pinned.insert(
            "path".to_string(),
            serde_json::Value::String("/etc/passwd".to_string()),
        );
        let second = conway::Rule::args_match_allow_rule("read", pinned);

        persist_permission_structured_rule(Some(&path), &first);
        persist_permission_structured_rule(Some(&path), &second);

        let contents = std::fs::read_to_string(&path).expect("file must be written");
        let parsed = conway::permission_pattern::parse_rules(&contents);
        assert_eq!(
            parsed.len(),
            2,
            "a distinct second rule must be appended, not replace the first"
        );
    }

    #[test]
    fn no_path_is_a_silent_no_op() {
        // Never panics, never creates anything -- the caller passes
        // `permission_paths.first()`, which is legitimately `None` when no
        // permissions file was discovered.
        persist_permission_structured_rule(None, &args_match_allow_rule());
    }
}
