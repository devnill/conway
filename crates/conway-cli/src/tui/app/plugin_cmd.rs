//! A plugin command's own async lifecycle -- from the moment `App::submit`
//! (still in `app.rs`, via its `Effect::RunPluginCommand` arm) hands off an
//! invocation, through the spawned task that isolates a hang/panic from the
//! run loop, to applying the reply against `AppState` (including
//! `CommandOutcome::ForkSession`, `/rewind`'s own capability). Extracted out
//! of `app.rs` verbatim (this item, board); [`super::run`]'s own
//! `plugin_cmd_rx.recv()` arm is the production caller of
//! [`App::apply_plugin_command_done`].

use conway::{AgentId, ForkSpec, SessionId};

use super::App;
use crate::tui::commands;
use crate::tui::state::{AppState, Entry};

/// The result of one spawned plugin-command task -- see [`App`]'s own `plugin_cmd_tx`/
/// `plugin_cmd_rx` doc and [`commands::Effect::RunPluginCommand`]'s.
/// `outcome` is already a [`conway::plugin::CommandOutcome::Error`] when the
/// spawned task panicked (`App::run`'s own `Effect::RunPluginCommand` arm
/// converts a `JoinError` into this variant before sending -- the receiving
/// `select!` arm never needs to know the difference between "the plugin
/// returned an error" and "the plugin's task panicked").
pub(super) struct PluginCommandDone {
    pub(super) full_name: String,
    /// The session THIS invocation's
    /// `CommandCtx::session_id` was stamped with -- captured by
    /// [`App::spawn_plugin_command`] before the outer task is spawned, so it
    /// reflects whatever session was live at INVOCATION time, never
    /// whatever `self.handle` has since become (e.g. an operator's
    /// `/resume` racing a slow plugin command). [`App::
    /// apply_plugin_command_done`]'s `ForkSession` arm resolves against
    /// THIS field, not `self.handle.id()` -- the structural binding that
    /// makes "a command cannot act on a session it was not invoked from"
    /// hold even under that race; see `tests::a_fork_session_outcome_is_
    /// resolved_against_the_invoking_session_even_if_the_host_has_since_
    /// resumed_elsewhere` for the adversarial proof.
    pub(super) session_id: SessionId,
    /// The agent THIS invocation's `CommandCtx::focused_agent` was stamped
    /// with -- captured alongside `session_id`, for the identical reason
    /// (this struct's own doc): `CommandOutcome::SubmitPrompt`'s own arm in
    /// [`App::apply_plugin_command_done`] resolves against THIS field, not
    /// `self.state.focused_agent` at apply time, so "a command submits to
    /// the agent it was invoked from, never one it names" holds under the
    /// same race `session_id`'s own doc already covers.
    pub(super) agent: AgentId,
    pub(super) outcome: conway::plugin::CommandOutcome,
}

impl App {
    /// Runs a resolved plugin command
    /// off this loop's own `select!`, never on it -- the mechanism behind
    /// this item's "a hanging or panicking plugin command does not freeze
    /// the TUI" acceptance criterion.
    ///
    /// **Two nested `tokio::spawn`s, not one.** The OUTER task is what
    /// neither `Self::submit` nor `Self::run`'s `select!` ever awaits, so
    /// nothing on this call stack can block on `command.invoke` -- a hang
    /// inside it just leaves the outer task parked forever, with zero effect
    /// on rendering or key handling (`Self::run`'s `plugin_cmd_rx.recv()` arm
    /// simply never fires for THIS invocation; every other arm keeps
    /// running). The INNER `tokio::spawn` + its own `JoinHandle` is what lets
    /// the outer task tell a genuine panic apart from an ordinary return:
    /// `tokio::spawn` isolates a panicking task (unwinding it does NOT abort
    /// the process, unlike an unwind on the calling thread would), so
    /// `inner.await`'s `Err(JoinError)` arm converts that into an ordinary
    /// `CommandOutcome::Error` -- the receiving `plugin_cmd_rx.recv()` arm
    /// handles it exactly like any other command failure, never a crash.
    pub(super) fn spawn_plugin_command(&self, invocation: commands::PluginCommandInvocation) {
        let commands::PluginCommandInvocation {
            full_name,
            command,
            ctx,
        } = invocation;
        // captured HERE, before `ctx`
        // is moved into the spawned task -- see `PluginCommandDone::
        // session_id`'s own doc for why this specific capture point (not a
        // later read of `self.handle.id()`) is what makes the binding
        // structural rather than merely usually-correct.
        let session_id = ctx.session_id;
        let agent = ctx.focused_agent;
        let tx = self.plugin_cmd_tx.clone();
        let panic_name = full_name.clone();
        tokio::spawn(async move {
            let inner = tokio::spawn(async move { command.invoke(ctx).await });
            let outcome = match inner.await {
                Ok(outcome) => outcome,
                Err(join_err) => conway::plugin::CommandOutcome::Error(format!(
                    "plugin command `/{panic_name}` panicked: {join_err}"
                )),
            };
            // The receiver only goes away when `App::run`'s loop has already
            // exited -- nothing left to notify, so a send failure here is
            // silently dropped, mirroring `/ask`'s own `run_modal_ask` send
            // site exactly.
            let _ = tx.send(PluginCommandDone {
                full_name,
                session_id,
                agent,
                outcome,
            });
        });
    }

    /// Applies one [`PluginCommandDone`] reply to `self.state` -- factored
    /// out of `Self::run`'s own `plugin_cmd_rx.recv()` arm so it is directly
    /// callable from a test with no real terminal/`select!` loop needed
    /// (mirrors this file's own `drain_and_apply` test helper's reasoning).
    ///
    /// Returns `true` when `self.handle` was swapped (only the
    /// `ForkSession` arm on success does this) -- the caller (`Self::run`'s
    /// `plugin_cmd_rx.recv()` arm) must then resubscribe its `events` local,
    /// exactly like `SubmitOutcome::Resubscribe`'s own call site.
    ///
    /// **Now `async`** (this item, board): the
    /// `ForkSession` arm awaits `Conway::fork_from`, the SAME facade call
    /// `/rewind` needs and the reason this method could not stay
    /// synchronous. This does NOT reopen the hang-safety property point 15
    /// establishes -- `fork_from` runs on THIS loop's own async task, same
    /// as every other facade call `Self::run`'s `select!` already awaits
    /// directly (`host.fork`, `host.resume`, ...); the property that must
    /// never block is `Command::invoke` itself, which is already complete
    /// by the time a `PluginCommandDone` exists at all (see `Self::
    /// spawn_plugin_command`'s own doc).
    pub(super) async fn apply_plugin_command_done(&mut self, done: PluginCommandDone) -> bool {
        match done.outcome {
            conway::plugin::CommandOutcome::Output(lines) => {
                for line in lines {
                    self.state.transcript.push(Entry::Notice { text: line });
                }
                false
            }
            conway::plugin::CommandOutcome::Error(message) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("/{}: {message}", done.full_name),
                });
                false
            }
            // `/rewind`'s own
            // capability. `done.session_id` -- NOT `self.handle.id()` -- is
            // what this resolves against: see `PluginCommandDone::
            // session_id`'s own doc for why that specific field is what
            // keeps this bound to the session the command was actually
            // invoked from, even if `self.handle` has since changed.
            conway::plugin::CommandOutcome::ForkSession { at_seq, directive } => {
                match self
                    .conway
                    .fork_from(done.session_id, at_seq, ForkSpec::new(directive))
                    .await
                {
                    Ok(handle) => {
                        // Mirrors `SlashCommand::Resume`'s own reset
                        // exactly (`commands::execute`'s `Resume` arm): a
                        // full, fresh `AppState` scoped to the child's own
                        // root, with the process-lifetime plugin command
                        // list carried across by hand (the one field that
                        // is NOT session-scoped).
                        let plugin_commands = self.state.plugin_commands.clone();
                        let child_root = handle.root();
                        self.handle = handle;
                        self.state = AppState::new(child_root);
                        self.state.plugin_commands = plugin_commands;
                        // no facade
                        // round trip needed here, unlike `Self::
                        // refresh_session_head`'s other call sites -- a
                        // freshly forked child's own head IS `at_seq`,
                        // exactly the point it was forked at
                        // (`Conway::fork_from`'s own contract).
                        self.state.session_head_seq = Some(at_seq);
                        self.state.transcript.push(Entry::Notice {
                            text: format!(
                                "/{}: forked session at seq {} -- now driving {child_root}",
                                done.full_name, at_seq.0
                            ),
                        });
                        true
                    }
                    Err(e) => {
                        self.state.transcript.push(Entry::Notice {
                            text: format!("/{}: fork failed: {e}", done.full_name),
                        });
                        false
                    }
                }
            }
            // `/conway.history.mask`'s own capability (board item
            // 01KZY8QRAVVVKCRBZ6HAEGW3GG). `done.session_id` -- the SAME
            // field the `ForkSession` arm above resolves against, for the
            // identical "acts on its own session, never one it names"
            // reason (`CommandOutcome::MaskRecord`'s own doc). Never swaps
            // `self.handle` -- masking never changes which session is
            // driven.
            conway::plugin::CommandOutcome::MaskRecord {
                target_seq,
                excluded,
            } => {
                match self
                    .conway
                    .mask_record(done.session_id, target_seq, excluded)
                    .await
                {
                    Ok(_seq) => {
                        let verb = if excluded { "masked" } else { "un-masked" };
                        self.state.transcript.push(Entry::Notice {
                            text: format!(
                                "/{}: {verb} seq {} -- affects future forks of this session \
                                 only",
                                done.full_name, target_seq.0
                            ),
                        });
                        false
                    }
                    Err(e) => {
                        self.state.transcript.push(Entry::Notice {
                            text: format!("/{}: mask failed: {e}", done.full_name),
                        });
                        false
                    }
                }
            }
            // `/conway.history.checkout`'s own capability (board item
            // 01KZY8QRAVVVKCRBZ6HAEGW3GG): forks `target` at ITS OWN head
            // (never `done.session_id` -- `Checkout` deliberately names a
            // DIFFERENT session, see that variant's own doc) and drives the
            // child, exactly like a successful `ForkSession` above.
            conway::plugin::CommandOutcome::Checkout { target } => {
                let head = match self.conway.session_head(target).await {
                    Ok(head) => head,
                    Err(e) => {
                        self.state.transcript.push(Entry::Notice {
                            text: format!("/{}: checkout failed: {e}", done.full_name),
                        });
                        return false;
                    }
                };
                match self
                    .conway
                    .fork_from(target, head, ForkSpec::new(String::new()))
                    .await
                {
                    Ok(handle) => {
                        // Mirrors the `ForkSession` arm's own reset exactly.
                        let plugin_commands = self.state.plugin_commands.clone();
                        let child_root = handle.root();
                        self.handle = handle;
                        self.state = AppState::new(child_root);
                        self.state.plugin_commands = plugin_commands;
                        self.state.session_head_seq = Some(head);
                        self.state.transcript.push(Entry::Notice {
                            text: format!(
                                "/{}: checked out session {target} at seq {} -- now driving \
                                 {child_root} ({target} is untouched)",
                                done.full_name, head.0
                            ),
                        });
                        true
                    }
                    Err(e) => {
                        self.state.transcript.push(Entry::Notice {
                            text: format!("/{}: checkout failed: {e}", done.full_name),
                        });
                        false
                    }
                }
            }
            // `CommandOutcome::SubmitPrompt`'s own capability (board item
            // `01M0VSMF71S6VXX81YRAAF5S8Q`, "No command can submit a
            // prompt"). `done.agent` -- NOT `self.state.focused_agent` --
            // is what this resolves against, for the identical "acts on
            // the agent it was invoked from, never one it names" reason
            // `PluginCommandDone::agent`'s own doc gives (mirroring
            // `ForkSession`'s `done.session_id` binding exactly).
            conway::plugin::CommandOutcome::SubmitPrompt { text } => {
                // Determine-first question 4's guard (this item's own
                // spec): refuse rather than silently racing a second turn
                // onto the SAME agent the TUI is currently watching
                // mid-turn. Composes with, rather than fights, `state.rs`'s
                // own `turn_started_at.is_some()` guard (the fix for the
                // adjacent wedged-status-bar defect, board
                // `01M0VQ650R31MGTXD8E225RRFH`): `turn_started_at` is
                // `Some` ONLY between a real `Event::TurnStarted` and
                // `Event::TurnFinished` for `self.state.focused_agent` --
                // exactly the "a turn is in flight" predicate that fix
                // already established (`state.rs`'s own `TextDelta` arm
                // doc). Scoped to the FOCUSED agent because that is the
                // only agent `AppState` tracks turn-in-flight state for
                // itself. **Updated by board `01M0VWMMEG4CER8Y8VH77KZ0CV`:**
                // `Runtime` now DOES keep such a registry
                // (`AgentTree::turn_in_flight`, reachable here via
                // `SessionHandle::turn_in_progress`) -- built for
                // `App::try_focus_agent`'s own refocus-mid-turn seed, not
                // yet consulted by this guard. Widening THIS check to query
                // it for a non-focused `done.agent` is a genuine option a
                // future item can pick up (it would let this guard refuse a
                // target the operator has merely navigated away from, not
                // only the one currently on screen) but is deliberately not
                // done here -- this item's own ownership was `state.rs`/
                // `focus.rs`/`session_handle.rs`, and widening a REFUSAL
                // surface is a behavior change this guard's own author,
                // not this item, should weigh. A target agent the operator
                // has since navigated away from therefore still has no
                // tracked state HERE to consult, so the submission
                // proceeds in that case -- `Runtime::prompt`'s own
                // concurrent-call contract (durable append either way,
                // never lost, never corrupted -- `SessionHandle::prompt`'s
                // own "concurrent-call footgun" doc) makes that the safe
                // direction to fail open in, rather than refusing on a
                // signal this layer cannot actually observe.
                if done.agent == self.state.focused_agent && self.state.turn_started_at.is_some() {
                    self.state.transcript.push(Entry::Notice {
                        text: format!(
                            "/{}: a turn is already running for the focused agent -- prompt \
                             not submitted",
                            done.full_name
                        ),
                    });
                    return false;
                }
                // No `Entry::Notice`/`Entry::User` pushed here on success,
                // deliberately: `state.rs`'s own `Event::UserTurn` arm is
                // the SINGLE path that renders a prompt bubble (its own
                // doc: "pushing locally would double it"). `prompt_command`
                // emits that event live on this agent's stream, which this
                // loop is already subscribed to whenever `done.agent`
                // still matches what is focused -- the ordinary case --
                // so the submitted text appears exactly as if the operator
                // had typed it, with no separate confirmation notice
                // competing with it.
                match self
                    .handle
                    .prompt_command(done.agent, text, done.full_name.clone())
                    .await
                {
                    Ok(_turn) => false,
                    Err(e) => {
                        self.state.transcript.push(Entry::Notice {
                            text: format!("/{}: could not submit prompt: {e}", done.full_name),
                        });
                        false
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::fixtures::{
        drain_and_apply, echo_conway, echo_conway_and_store, minimal_cli,
    };
    use super::{App, PluginCommandDone};
    use crate::tui::state::Entry;

    // -----------------------------------------------------------------
    // Plugin-declared TUI commands
    // -- the VERIFICATION ANCHOR: a fixture plugin declaring `/greet`,
    // driven through the real `App::submit` -> spawned task -> channel ->
    // `apply_plugin_command_done` pipeline (`Self::run`'s own path, minus
    // the real terminal), asserted against the observable transcript.
    // -----------------------------------------------------------------

    /// Mirrors `conway_plugin_skeleton`'s own shipped example in shape (a
    /// fixed reply plus the caller's argument echoed back) -- this crate's
    /// own equivalent of that crate's `SkeletonPingTool`, for a command
    /// rather than a tool.
    struct GreetCommandFixture;

    #[async_trait::async_trait]
    impl conway::plugin::Command for GreetCommandFixture {
        fn spec(&self) -> conway::plugin::CommandSpec {
            conway::plugin::CommandSpec {
                name: "greet".to_string(),
                summary: "greets the operator".to_string(),
            }
        }

        async fn invoke(&self, ctx: conway::plugin::CommandCtx) -> conway::plugin::CommandOutcome {
            conway::plugin::CommandOutcome::Output(vec![format!("hello, {}!", ctx.args)])
        }
    }

    /// Never resolves -- see `commands::tests::HangingCommand`'s own doc for
    /// why this exists and how it is safely used (never `.await`ed to
    /// completion by any test).
    struct HangCommandFixture;

    #[async_trait::async_trait]
    impl conway::plugin::Command for HangCommandFixture {
        fn spec(&self) -> conway::plugin::CommandSpec {
            conway::plugin::CommandSpec {
                name: "hang".to_string(),
                summary: "never returns".to_string(),
            }
        }

        async fn invoke(&self, _ctx: conway::plugin::CommandCtx) -> conway::plugin::CommandOutcome {
            std::future::pending::<()>().await;
            unreachable!("pending() never resolves")
        }
    }

    struct GreetPluginFixture;

    impl conway::plugin::Plugin for GreetPluginFixture {
        fn manifest(&self) -> conway::plugin::PluginManifest {
            conway::plugin::PluginManifest {
                id: "acme".to_string(),
                version: "0.1.0".to_string(),
                tools: vec![],
                required_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
            vec![]
        }

        fn commands(&self) -> Vec<Arc<dyn conway::plugin::Command>> {
            vec![Arc::new(GreetCommandFixture), Arc::new(HangCommandFixture)]
        }
    }

    /// The verification anchor's positive half: with the fixture plugin
    /// installed, `/acme.greet <args>` reaches `GreetCommandFixture::invoke`
    /// and its output lands in the transcript, driven through the SAME
    /// `submit` -> spawn -> channel path `App::run` uses (only the terminal
    /// itself is stubbed out -- `apply_plugin_command_done` is `Self::run`'s
    /// own method, called here exactly as its `select!` arm calls it).
    #[tokio::test]
    async fn plugin_command_end_to_end_reaches_the_transcript() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(GreetPluginFixture);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");

        // Also proves discovery: the fixture's command shows up in
        // `/help`'s own pointer surface, the `/` palette's backing data.
        assert!(
            app.state
                .plugin_commands
                .iter()
                .any(|c| c.name == "/acme.greet"),
            "the installed plugin's command must appear in the palette-backing \
             list: {:?}",
            app.state.plugin_commands
        );

        let outcome = app
            .submit("/acme.greet world".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));

        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        assert_eq!(done.full_name, "acme.greet");
        app.apply_plugin_command_done(done).await;

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text == "hello, world!"
            )),
            "the plugin's own output must reach the transcript: {:?}",
            app.state.transcript
        );
    }

    /// **Hang-safety, at the `App`/`submit` layer.** A command whose
    /// `invoke` never completes must not block `submit` itself, and the app
    /// must stay fully usable afterward -- an ordinary prompt submitted
    /// right after still works. Wrapped in a generous timeout so a
    /// regression that made `submit` actually await the hang fails this
    /// test rather than hanging the whole suite.
    #[tokio::test]
    async fn a_hanging_plugin_command_does_not_block_submit_or_leave_the_app_unusable() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(GreetPluginFixture);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");
        let mut events = app.handle.events();

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.submit("/acme.hang".to_string()),
        )
        .await
        .expect(
            "submit must return promptly even though the plugin command's \
             invoke() never completes",
        )
        .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));

        // The app is still fully responsive: an ordinary prompt submitted
        // right after the hang still works, proving the hang did not wedge
        // `App` (its `handle`, its facade, or `submit` itself) in any way.
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.submit("hello after the hang".to_string()),
        )
        .await
        .expect("submit must still work after a plugin command hangs in the background")
        .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));
        drain_and_apply(&mut events, &mut app.state);
        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::User(text) if text == "hello after the hang")),
            "an ordinary prompt must still reach the transcript after a \
             plugin command hangs: {:?}",
            app.state.transcript
        );
        // The hung task's own reply, if it ever arrived, would be a
        // `CommandOutcome` this test never needs -- it is left running in
        // the background exactly as a real hang would be; the process exits
        // when the test binary does, same as any other orphaned test-scoped
        // `tokio::spawn`.
    }

    // -----------------------------------------------------------------
    // CommandOutcome::ForkSession
    // -- the VERIFICATION ANCHOR: a fixture plugin command that forks its
    // own calling session and returns; the TUI ends up driving the child,
    // and the parent's log is byte-identical to before. Paired with a
    // negative test proving the fork is resolved against the session the
    // command was actually invoked from, never whatever session the host
    // happens to be driving by the time the reply is applied.
    // -----------------------------------------------------------------

    /// This crate's own `/rewind`-shaped fixture: forks the calling session
    /// at whatever sequence number the operator typed. Mirrors
    /// `GreetCommandFixture`'s own shape (a fixed reply plus the operator's
    /// argument) one level up -- here the "reply" is a request, not output.
    struct RewindCommandFixture;

    #[async_trait::async_trait]
    impl conway::plugin::Command for RewindCommandFixture {
        fn spec(&self) -> conway::plugin::CommandSpec {
            conway::plugin::CommandSpec {
                name: "rewind".to_string(),
                summary: "forks the calling session at a sequence".to_string(),
            }
        }

        async fn invoke(&self, ctx: conway::plugin::CommandCtx) -> conway::plugin::CommandOutcome {
            let at_seq = ctx.args.trim().parse::<u64>().unwrap_or(0);
            conway::plugin::CommandOutcome::ForkSession {
                at_seq: conway::LogSeq(at_seq),
                directive: String::new(),
            }
        }
    }

    struct RewindPluginFixture;

    impl conway::plugin::Plugin for RewindPluginFixture {
        fn manifest(&self) -> conway::plugin::PluginManifest {
            conway::plugin::PluginManifest {
                id: "acme".to_string(),
                version: "0.1.0".to_string(),
                tools: vec![],
                required_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
            vec![]
        }

        fn commands(&self) -> Vec<Arc<dyn conway::plugin::Command>> {
            vec![Arc::new(RewindCommandFixture)]
        }
    }

    /// The verification anchor's positive half: `/acme.rewind <seq>` forks
    /// the REAL calling session (real history, real store) and the app ends
    /// up driving a genuinely different, genuinely drivable child -- while
    /// the parent's own log is untouched, proven by its head staying
    /// identical before and after (append-only, zero-copy fork: `Conway::
    /// fork_from`'s own contract).
    #[tokio::test]
    async fn fork_session_outcome_forks_the_calling_session_and_drives_the_child() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(RewindPluginFixture);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");

        let parent_sid = app.handle.id();
        // `app.handle.prompt(..).await?.text().await?` -- NOT `app.submit`
        // -- drives a turn to full completion (waits for the assistant's
        // reply, not merely the `UserTurn`) so `session_head` below reads a
        // deterministic, fully-persisted count rather than racing the
        // agent loop's own async write.
        app.handle
            .prompt("first")
            .await
            .expect("prompt 1 should not error")
            .text()
            .await
            .expect("turn 1 should complete");
        app.handle
            .prompt("second")
            .await
            .expect("prompt 2 should not error")
            .text()
            .await
            .expect("turn 2 should complete");

        let head_before = app
            .conway
            .session_head(parent_sid)
            .await
            .expect("session_head should succeed");
        assert!(
            head_before.0 > 0,
            "the parent session must have real history to fork from, got head {head_before:?}"
        );

        let outcome = app
            .submit(format!("/acme.rewind {}", head_before.0))
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));

        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        assert_eq!(done.full_name, "acme.rewind");
        assert_eq!(
            done.session_id, parent_sid,
            "CommandCtx::session_id (and therefore PluginCommandDone::session_id) must be \
             the CALLING session's own id"
        );

        let resubscribe = app.apply_plugin_command_done(done).await;
        assert!(
            resubscribe,
            "a successful ForkSession must ask the caller to resubscribe its event stream"
        );

        // The TUI is now driving a genuinely different session...
        let child_sid = app.handle.id();
        assert_ne!(child_sid, parent_sid);

        // ...and that session is genuinely drivable, not merely a fresh,
        // inert handle -- the "TUI drives the result" property.
        let child_prompt_err = app
            .handle
            .prompt("hello from the child")
            .await
            .err()
            .map(|e| e.to_string());
        assert!(
            child_prompt_err.is_none(),
            "the forked child must be drivable: {child_prompt_err:?}"
        );

        // The parent's own log is append-only and was never mutated by the
        // fork.
        let head_after = app
            .conway
            .session_head(parent_sid)
            .await
            .expect("session_head should succeed");
        assert_eq!(
            head_before, head_after,
            "forking must never mutate the parent session's own log"
        );

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("acme.rewind") && text.contains("forked")
            )),
            "a successful fork must be surfaced as a transcript notice: {:?}",
            app.state.transcript
        );
    }

    /// The verification anchor's negative half, and the discriminating
    /// observable this whole item exists to prove: **a command cannot act
    /// on a session it was not invoked from.**
    ///
    /// Adversarial timing: the plugin command is invoked while the host is
    /// driving `invoking_sid` (which stamps `CommandCtx::session_id` /
    /// `PluginCommandDone::session_id` to it) -- but by the time its reply
    /// is APPLIED, the host has since started driving a totally different,
    /// already-existing session (`other_sid`, simulating an operator's
    /// `/resume` racing a slow plugin command). The fork must still land on
    /// `invoking_sid`, never `other_sid` -- proven not just by "no crash"
    /// but by `other_sid`'s own log staying byte-for-byte untouched, and,
    /// for contrast, by showing forking `other_sid` DIRECTLY at the same
    /// `at_seq` genuinely fails (its log is empty, so that seq is out of
    /// range for it) -- the "fails for the right reason, not incidentally"
    /// half of the acceptance criterion.
    #[tokio::test]
    async fn a_fork_session_outcome_is_resolved_against_the_invoking_session_even_if_the_host_has_since_moved_on(
    ) {
        let conway = echo_conway();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(RewindPluginFixture);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");

        let invoking_sid = app.handle.id();
        // Full-completion drive (see the previous test's own comment for
        // why `app.handle.prompt(..).text()`, not `app.submit`).
        app.handle
            .prompt("first")
            .await
            .expect("prompt should not error")
            .text()
            .await
            .expect("turn should complete");
        let invoking_head = app
            .conway
            .session_head(invoking_sid)
            .await
            .expect("session_head should succeed");
        assert!(invoking_head.0 > 0);

        // Invoked while `self.handle` is still `invoking_sid` -- this is
        // what stamps `CommandCtx::session_id` to it.
        let outcome = app
            .submit(format!("/acme.rewind {}", invoking_head.0))
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));
        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        assert_eq!(done.session_id, invoking_sid);

        // SIMULATE THE RACE: a completely separate, freshly created session
        // (empty log) becomes the one the host is driving, as if `/resume`
        // had landed while the plugin command was still in flight.
        let other = conway
            .new_session(App::session_spec(&cli).expect("session_spec"))
            .await
            .expect("new_session should succeed");
        let other_sid = other.id();
        assert_ne!(other_sid, invoking_sid);
        let other_head_before = conway
            .session_head(other_sid)
            .await
            .expect("session_head should succeed");
        app.handle = other;

        // Applying the ALREADY-CAPTURED reply must still resolve against
        // `invoking_sid` -- never `other_sid`.
        let resubscribed = app.apply_plugin_command_done(done).await;
        assert!(resubscribed);
        let driven_sid = app.handle.id();
        assert_ne!(
            driven_sid, other_sid,
            "must not have forked the session the host happened to be driving at apply time"
        );

        // `other_sid`'s own log is completely untouched -- not merely "no
        // crash", but genuinely never reached.
        let other_head_after = conway
            .session_head(other_sid)
            .await
            .expect("session_head should succeed");
        assert_eq!(other_head_before, other_head_after);

        // For contrast: forking `other_sid` DIRECTLY at the SAME `at_seq`
        // that just succeeded against `invoking_sid` fails outright --
        // `other_sid`'s log is empty, so that seq is out of range for it.
        // This is the concrete, distinguishable failure a naive
        // `self.handle.id()`-at-apply-time implementation would have
        // produced by accident; the correct, bound implementation never
        // gets near it, because it never asks.
        let would_have_failed = conway
            .fork_from(
                other_sid,
                conway::LogSeq(invoking_head.0),
                conway::ForkSpec::new(""),
            )
            .await;
        assert!(
            would_have_failed.is_err(),
            "sanity: at_seq must genuinely be out of range for `other_sid`, or this test \
             proves nothing"
        );
    }

    // -----------------------------------------------------------------
    // `conway-plugin-history`'s
    // `/conway.history.rewind`, driven through the REAL shipped plugin
    // crate (not `RewindPluginFixture` above) -- the discriminating
    // acceptance criterion this item names: absent the plugin, the command
    // is simply unknown, with no stub or special case anywhere in core;
    // installed, it forks the real calling session end to end and the
    // parent's own log is provably untouched.
    // -----------------------------------------------------------------

    /// The positive half, driven through the SAME real crate: installed,
    /// `/conway.history.rewind <seq>` forks the real calling session and the
    /// TUI ends up driving a genuinely different, genuinely drivable child
    /// -- while the parent's own log is provably untouched, checked two
    /// ways: `Conway::session_head` (the same proof
    /// `fork_session_outcome_forks_the_calling_session_and_drives_the_child`
    /// above already gives for the local fixture) AND, stronger, every
    /// persisted `LogRecord` read back byte-for-byte equal
    /// (`LogRecord: PartialEq`) -- the literal "the parent's bytes are
    /// unchanged" this item's acceptance criterion names.
    #[tokio::test]
    async fn conway_history_rewind_forks_the_real_plugin_and_leaves_the_parent_log_byte_for_byte_unchanged(
    ) {
        let (conway, store) = echo_conway_and_store();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> =
            Arc::new(conway_plugin_history::HistoryPlugin);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");

        let parent_sid = app.handle.id();
        app.handle
            .prompt("first")
            .await
            .expect("prompt 1 should not error")
            .text()
            .await
            .expect("turn 1 should complete");
        app.handle
            .prompt("second")
            .await
            .expect("prompt 2 should not error")
            .text()
            .await
            .expect("turn 2 should complete");

        let head_before = app
            .conway
            .session_head(parent_sid)
            .await
            .expect("session_head should succeed");
        assert!(head_before.0 > 0);
        let records_before: Vec<conway_core::log::LogRecord> = conway::SessionStore::read(
            store.as_ref(),
            &parent_sid,
            conway_core::ids::SeqRange::full(),
        )
        .await
        .expect("read should succeed");
        assert!(!records_before.is_empty());

        let outcome = app
            .submit(format!("/conway.history.rewind {}", head_before.0))
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));

        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        assert_eq!(done.full_name, "conway.history.rewind");
        assert_eq!(done.session_id, parent_sid);

        let resubscribe = app.apply_plugin_command_done(done).await;
        assert!(
            resubscribe,
            "a successful ForkSession must ask the caller to resubscribe its event stream"
        );

        let child_sid = app.handle.id();
        assert_ne!(child_sid, parent_sid);
        assert_eq!(
            app.state.session_head_seq,
            Some(head_before),
            "the child's fresh head is exactly the seq it was forked at -- no round trip needed \
             (apply_plugin_command_done's own ForkSession arm sets this directly)"
        );
        let child_prompt_err = app
            .handle
            .prompt("hello from the child")
            .await
            .err()
            .map(|e| e.to_string());
        assert!(
            child_prompt_err.is_none(),
            "the forked child must be drivable: {child_prompt_err:?}"
        );

        let head_after = app
            .conway
            .session_head(parent_sid)
            .await
            .expect("session_head should succeed");
        assert_eq!(
            head_before, head_after,
            "forking through the real plugin must never mutate the parent session's own head"
        );
        let records_after: Vec<conway_core::log::LogRecord> = conway::SessionStore::read(
            store.as_ref(),
            &parent_sid,
            conway_core::ids::SeqRange::full(),
        )
        .await
        .expect("read should succeed");
        assert_eq!(
            records_before, records_after,
            "the parent session's own persisted records must be byte-for-byte unchanged after a \
             fork through the real conway-plugin-history crate -- append-only, never mutated"
        );

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("conway.history.rewind") && text.contains("forked")
            )),
            "a successful fork must be surfaced as a transcript notice: {:?}",
            app.state.transcript
        );
    }

    // -----------------------------------------------------------------
    // `conway-plugin-history`'s `/conway.history.mask` --
    // `CommandOutcome::MaskRecord`, driven through the REAL shipped
    // plugin crate (board item 01KZY8QRAVVVKCRBZ6HAEGW3GG, "`/checkout`
    // and a reachable `ContextMask`") -- the plugin's SECOND command.
    // -----------------------------------------------------------------

    /// Masking never swaps `self.handle` (unlike `ForkSession`/`Checkout`)
    /// and appends a real, readable-back `LogRecord::ContextMask` against
    /// the CALLING session -- the record round-trips, is reversible, and
    /// is surfaced as a transcript notice.
    #[tokio::test]
    async fn conway_history_mask_appends_a_real_record_through_the_real_plugin() {
        let (conway, store) = echo_conway_and_store();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> =
            Arc::new(conway_plugin_history::HistoryPlugin);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");

        let sid = app.handle.id();
        app.handle
            .prompt("only turn")
            .await
            .expect("prompt should not error")
            .text()
            .await
            .expect("turn should complete");

        let records: Vec<conway_core::log::LogRecord> =
            conway::SessionStore::read(store.as_ref(), &sid, conway_core::ids::SeqRange::full())
                .await
                .expect("read should succeed");
        let target_seq = records
            .iter()
            .find_map(|r| match r {
                conway_core::log::LogRecord::UserTurn { seq, text, .. } if text == "only turn" => {
                    Some(*seq)
                }
                _ => None,
            })
            .expect("the UserTurn record for the prompt just sent must exist");

        let outcome = app
            .submit(format!("/conway.history.mask {}", target_seq.0))
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));

        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        assert_eq!(done.full_name, "conway.history.mask");
        assert_eq!(done.session_id, sid);

        let resubscribe = app.apply_plugin_command_done(done).await;
        assert!(
            !resubscribe,
            "masking must never swap the driven session -- there is no new session to drive"
        );
        assert_eq!(
            app.handle.id(),
            sid,
            "masking must never change which session is driven"
        );

        let records_after: Vec<conway_core::log::LogRecord> =
            conway::SessionStore::read(store.as_ref(), &sid, conway_core::ids::SeqRange::full())
                .await
                .expect("read should succeed");
        let mask = records_after
            .iter()
            .find(|r| {
                matches!(
                    r,
                    conway_core::log::LogRecord::ContextMask { target_seq: ts, .. }
                        if *ts == target_seq
                )
            })
            .expect("a real ContextMask record must be appended and readable back");
        match mask {
            conway_core::log::LogRecord::ContextMask { excluded, .. } => assert!(*excluded),
            _ => unreachable!(),
        }
        // The original record is untouched -- an overlay, not a mutation.
        assert!(records_after
            .iter()
            .any(|r| matches!(r, conway_core::log::LogRecord::UserTurn { text, .. } if text == "only turn")));

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("conway.history.mask") && text.contains("masked")
            )),
            "a successful mask must be surfaced as a transcript notice: {:?}",
            app.state.transcript
        );
    }

    // -----------------------------------------------------------------
    // `conway-plugin-history`'s `/conway.history.checkout` --
    // `CommandOutcome::Checkout`, driven through the REAL shipped
    // plugin crate -- the plugin's THIRD command. Mirrors the `/rewind`
    // real-plugin test above, except the forked session is NOT the one
    // the command was invoked from.
    // -----------------------------------------------------------------

    /// Checking out a DIFFERENT, already-existing session forks it (never
    /// attaching live) and swaps the TUI onto the child -- while the
    /// checked-out-FROM session's own log stays byte-for-byte unchanged
    /// and remains listed.
    #[tokio::test]
    async fn conway_history_checkout_forks_the_target_and_leaves_it_untouched_through_the_real_plugin(
    ) {
        let (conway, store) = echo_conway_and_store();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> =
            Arc::new(conway_plugin_history::HistoryPlugin);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");
        let invoking_sid = app.handle.id();

        // A SEPARATE, already-existing session -- what the operator is
        // about to check out INTO, while sitting in `invoking_sid`.
        let target = conway
            .new_session(App::session_spec(&cli).expect("session_spec"))
            .await
            .expect("new_session should succeed");
        let target_sid = target.id();
        assert_ne!(target_sid, invoking_sid);
        target
            .prompt("target session content")
            .await
            .expect("prompt should not error")
            .text()
            .await
            .expect("turn should complete");
        let target_head_before = conway
            .session_head(target_sid)
            .await
            .expect("session_head should succeed");
        let target_records_before: Vec<conway_core::log::LogRecord> = conway::SessionStore::read(
            store.as_ref(),
            &target_sid,
            conway_core::ids::SeqRange::full(),
        )
        .await
        .expect("read should succeed");

        let outcome = app
            .submit(format!("/conway.history.checkout {target_sid}"))
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));

        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        assert_eq!(done.full_name, "conway.history.checkout");
        assert_eq!(
            done.session_id, invoking_sid,
            "CommandCtx::session_id must be the CALLING session, even though `Checkout` acts \
             on a DIFFERENT, named session"
        );

        let resubscribe = app.apply_plugin_command_done(done).await;
        assert!(
            resubscribe,
            "a successful checkout must ask the caller to resubscribe its event stream"
        );

        let driven_sid = app.handle.id();
        assert_ne!(driven_sid, invoking_sid, "checkout must move the head");
        assert_ne!(
            driven_sid, target_sid,
            "checkout forks the target rather than attaching to it live -- the driven session \
             must be a NEW child, not `target_sid` itself"
        );
        // The new child is genuinely drivable.
        let child_prompt_err = app
            .handle
            .prompt("hello from the checked-out child")
            .await
            .err()
            .map(|e| e.to_string());
        assert!(
            child_prompt_err.is_none(),
            "the checked-out child must be drivable: {child_prompt_err:?}"
        );

        // The checked-out-FROM session is untouched...
        let target_head_after = conway
            .session_head(target_sid)
            .await
            .expect("session_head should succeed");
        assert_eq!(
            target_head_before, target_head_after,
            "checkout must never mutate the checked-out-FROM session's own head"
        );
        let target_records_after: Vec<conway_core::log::LogRecord> = conway::SessionStore::read(
            store.as_ref(),
            &target_sid,
            conway_core::ids::SeqRange::full(),
        )
        .await
        .expect("read should succeed");
        assert_eq!(
            target_records_before, target_records_after,
            "the checked-out-FROM session's own persisted records must be byte-for-byte \
             unchanged"
        );
        // ...and still listed.
        let listed = conway
            .sessions(conway_core::log::SessionFilter::default())
            .await
            .expect("sessions should succeed");
        assert!(
            listed.iter().any(|m| m.id == target_sid),
            "the checked-out-FROM session must still be listed: {listed:?}"
        );

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("conway.history.checkout") && text.contains("checked out")
            )),
            "a successful checkout must be surfaced as a transcript notice: {:?}",
            app.state.transcript
        );
    }

    // -----------------------------------------------------------------
    // `CommandOutcome::SubmitPrompt` (board item `01M0VSMF71S6VXX81YRAAF5S8Q`)
    // -- the VERIFICATION ANCHOR: a fixture plugin command that submits a
    // literal prompt; the resulting turn is a real `LogRecord::UserTurn`,
    // readable back, stamped `Provenance::CommandPrompt` (determine-first
    // question 1) rather than `Provenance::UserPrompt`, driven through the
    // SAME `App::submit` -> spawn -> channel -> `apply_plugin_command_done`
    // pipeline every other `CommandOutcome` variant is proven through
    // above. Paired with determine-first question 4's in-flight guard test
    // and its falsification (P-15).
    // -----------------------------------------------------------------

    /// Ignores `ctx.args` entirely -- this item's own determine-first
    /// question 3 answer (v1 performs no interpolation of any kind): the
    /// submitted text is always this literal string.
    struct SubmitPromptCommandFixture;

    #[async_trait::async_trait]
    impl conway::plugin::Command for SubmitPromptCommandFixture {
        fn spec(&self) -> conway::plugin::CommandSpec {
            conway::plugin::CommandSpec {
                name: "submit".to_string(),
                summary: "submits a fixed prompt as a new turn".to_string(),
            }
        }

        async fn invoke(&self, _ctx: conway::plugin::CommandCtx) -> conway::plugin::CommandOutcome {
            conway::plugin::CommandOutcome::SubmitPrompt {
                text: "hello from a command".to_string(),
            }
        }
    }

    struct SubmitPromptPluginFixture;

    impl conway::plugin::Plugin for SubmitPromptPluginFixture {
        fn manifest(&self) -> conway::plugin::PluginManifest {
            conway::plugin::PluginManifest {
                id: "acme".to_string(),
                version: "0.1.0".to_string(),
                tools: vec![],
                required_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
            vec![]
        }

        fn commands(&self) -> Vec<Arc<dyn conway::plugin::Command>> {
            vec![Arc::new(SubmitPromptCommandFixture)]
        }
    }

    /// The verification anchor's positive half: `/acme.submit` submits a
    /// REAL turn on the calling agent -- a genuine `LogRecord::UserTurn`,
    /// readable back from the store, stamped `Provenance::CommandPrompt {
    /// command: "acme.submit" }` rather than `Provenance::UserPrompt`
    /// (checked directly against the persisted record, not merely asserted
    /// in a doc comment) -- and the turn actually runs to completion: the
    /// echo backend's own reply lands in the transcript, proving this
    /// reaches a real agent turn, not merely an appended record nobody
    /// reads. Never swaps the driven session (mirrors `MaskRecord`).
    #[tokio::test]
    async fn submit_prompt_outcome_submits_a_real_turn_stamped_command_prompt_provenance() {
        use futures::StreamExt;

        let (conway, store) = echo_conway_and_store();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(SubmitPromptPluginFixture);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");
        let sid = app.handle.id();
        let root = app.state.focused_agent;
        let mut events = app.handle.events();

        let outcome = app
            .submit("/acme.submit".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));

        let done = app
            .plugin_cmd_rx
            .as_mut()
            .expect("plugin_cmd_rx is set by App::new")
            .recv()
            .await
            .expect("the spawned command task must reply");
        assert_eq!(done.full_name, "acme.submit");
        assert_eq!(done.session_id, sid);
        assert_eq!(
            done.agent, root,
            "CommandCtx::focused_agent (and therefore PluginCommandDone::agent) must be the \
             agent the command was actually invoked against"
        );

        let resubscribe = app.apply_plugin_command_done(done).await;
        assert!(
            !resubscribe,
            "submitting a prompt must never swap the driven session -- there is no new session \
             to drive"
        );
        assert_eq!(
            app.handle.id(),
            sid,
            "submitting a prompt must never change which session is driven"
        );

        // The submitted text landed as a genuine, readable-back record --
        // and its provenance is `CommandPrompt`, never `UserPrompt`
        // (determine-first question 1).
        let records: Vec<conway_core::log::LogRecord> =
            conway::SessionStore::read(store.as_ref(), &sid, conway_core::ids::SeqRange::full())
                .await
                .expect("read should succeed");
        let submitted = records
            .iter()
            .find(
                |r| matches!(r, conway_core::log::LogRecord::UserTurn { text, .. } if text == "hello from a command"),
            )
            .expect("the submitted prompt must be appended as a real UserTurn record");
        match submitted {
            conway_core::log::LogRecord::UserTurn { prov, .. } => match prov {
                conway_core::provenance::Provenance::CommandPrompt { command } => {
                    assert_eq!(command, "acme.submit")
                }
                other => panic!(
                    "expected Provenance::CommandPrompt, got {other:?} -- a command-submitted \
                     turn must never be stamped as if the operator typed it"
                ),
            },
            _ => unreachable!(),
        }

        // The turn actually runs: drain events until the root's own
        // `TurnFinished`, proving this is a genuine agent turn (the echo
        // backend's own reply), not merely an appended record nobody ever
        // reads.
        let mut saw_finished = false;
        for _ in 0..200 {
            let Ok(Some(env)) =
                tokio::time::timeout(std::time::Duration::from_millis(50), events.next()).await
            else {
                break;
            };
            app.state.apply(&env);
            if matches!(&env.event, conway::Event::TurnFinished { .. }) && env.agent == root {
                saw_finished = true;
                break;
            }
        }
        assert!(
            saw_finished,
            "the submitted prompt must run a real agent turn to completion"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::User(text) if text == "hello from a command"
            )),
            "the submitted prompt must render exactly like an operator-typed one, through the \
             SAME Event::UserTurn -> Entry::User path: {:?}",
            app.state.transcript
        );
    }

    /// Determine-first question 4's guard, TESTED (P-15): a `SubmitPrompt`
    /// targeting the SAME agent the TUI is currently watching mid-turn is
    /// refused -- no second `UserTurn` record appended -- rather than
    /// raced in silently. Composes with `state.rs`'s own `turn_started_at`
    /// field (the fix for the adjacent wedged-status-bar defect, board
    /// `01M0VQ650R31MGTXD8E225RRFH`): set directly here to simulate "a real
    /// turn is in flight" without needing to race a live one.
    ///
    /// **Falsified** (removing the guard's `if` block from `Self::
    /// apply_plugin_command_done` makes this test fail -- verified by hand
    /// during this item's own development, see the completion report): a
    /// fixture that leaves `turn_started_at` at its default `None` would
    /// prove nothing about the guard (P-15's own "a check is not
    /// established until it has been shown to fail" -- this is why
    /// `turn_started_at` is set explicitly, not left at whatever `AppState::
    /// new` defaults to).
    #[tokio::test]
    async fn submit_prompt_outcome_is_refused_while_the_focused_agent_has_a_turn_in_flight() {
        let (conway, store) = echo_conway_and_store();
        let cli = minimal_cli();
        let plugin: Arc<dyn conway::plugin::Plugin> = Arc::new(SubmitPromptPluginFixture);
        let mut app = App::new(&cli, &conway, &[plugin])
            .await
            .expect("App::new should succeed");
        let sid = app.handle.id();

        // Simulate "a real turn is in flight for the focused agent" the
        // SAME way `state.rs`'s own guard reads it -- `turn_started_at` is
        // `Some` only between a real `TurnStarted` and `TurnFinished`.
        app.state.turn_started_at = Some(std::time::Instant::now());

        let done = PluginCommandDone {
            full_name: "acme.submit".to_string(),
            session_id: sid,
            agent: app.state.focused_agent,
            outcome: conway::plugin::CommandOutcome::SubmitPrompt {
                text: "should never land".to_string(),
            },
        };

        let resubscribe = app.apply_plugin_command_done(done).await;
        assert!(!resubscribe);

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text }
                    if text.contains("already running") && text.contains("not submitted")
            )),
            "a refused submission must be surfaced as a transcript notice: {:?}",
            app.state.transcript
        );

        let records: Vec<conway_core::log::LogRecord> =
            conway::SessionStore::read(store.as_ref(), &sid, conway_core::ids::SeqRange::full())
                .await
                .expect("read should succeed");
        assert!(
            !records.iter().any(
                |r| matches!(r, conway_core::log::LogRecord::UserTurn { text, .. } if text == "should never land")
            ),
            "a refused submission must never durably append a record: {records:?}"
        );
    }
}
