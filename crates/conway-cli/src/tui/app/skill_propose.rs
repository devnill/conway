//! `/conway.skills.propose`'s own async completion, and the automatic
//! trigger's identical shape for a non-`keep_alive` run (slice 2, board
//! item `01M3DTT078W25MD2S4527R0WAV`). Forks an EPHEMERAL child bound by
//! `conway_plugin_skills::PROPOSAL_MAX_STEPS`/`PROPOSAL_DEADLINE_SECS`,
//! awaits its single reply, purges it unconditionally (the child is a
//! "distillate only" -- see this module's own doc below), and parses the
//! reply into a proposal to show for approval. [`App::spawn_skill_propose`]
//! is the actual `tokio::spawn` call site, mirroring `await_cmd::App::
//! spawn_await`'s shape closely: `commands::execute`'s `SlashCommand::
//! SkillsPropose` arm cannot spawn this itself (it has no live
//! `SessionHandle`/`Conway` to clone and no `skill_propose_tx` -- see
//! `commands::Effect::RunSkillPropose`'s own doc), so it validates and hands
//! back that effect; `App::submit`'s shared `Effect` match calls this
//! method.
//!
//! # Why the ephemeral child is ALWAYS purged, with no fate to choose
//!
//! Unlike `/ask`'s three forced fates (`[f]` keep, `[p]` pull in, `[esc]`
//! discard), this feature's ephemeral child is a pure DISTILLATE -- its
//! entire job is to look back over the task and produce a candidate
//! `SKILL.md` (or say none is warranted), and once that text is captured
//! there is nothing about the child's own transcript worth keeping,
//! merging, or ever showing separately from the resulting proposal. So this
//! module purges it unconditionally, the instant its result lands (inside
//! the SAME spawned task, before `SkillProposeDone` is even sent) -- never
//! offering a modal fate over the CHILD the way `/ask` does. The MODAL this
//! produces is over the PROPOSAL text, a fork/pull-in/discard question was
//! never asked of the operator here at all.
//!
//! # Why a dedicated channel, not `modal_ask_tx`/`await_tx`
//!
//! Mirrors `await_cmd.rs`'s own "which channel" design note verbatim: a
//! new message type could be folded into an existing channel, but doing so
//! would couple an unrelated feature's delivery to this one's consumer
//! correctly disambiguating a widened enum. A dedicated `skill_propose_tx`/
//! `skill_propose_rx` pair, drained unconditionally by `App::run` exactly
//! like `await_rx`, keeps the two independent.
//!
//! # The fork's own bound -- why `SessionHandle::fork`, not `::ask`
//!
//! `SessionHandle::ask` (the `/ask` modal's own primitive) always uses
//! `Budget::default()` (40 steps, no deadline) -- its own doc discloses this
//! as a "disclosed simplification," not a knob a caller can override. This
//! feature needs a TIGHTER, STATED bound (`docs/plugins/skills.md`'s own
//! "Budget" section, `conway_plugin_skills::PROPOSAL_MAX_STEPS`/
//! `PROPOSAL_DEADLINE_SECS`) -- a reflection-and-write task should never
//! spend anywhere near as much as an ordinary turn, and the operator did
//! not ask for these tokens at all (the spec's own "tokens the operator did
//! not ask for" framing). `SessionHandle::fork` plus a hand-built
//! `ForkSpec::budget` is the lower-level primitive that lets this module set
//! that bound explicitly, at the cost of building the ephemeral-child
//! wiring (`ephemeral(true)`) `::ask` would otherwise have supplied for
//! free.

use conway::{AgentId, AgentResult, Budget, Conway, ForkSpec, SessionHandle};
use conway_plugin_skills::{
    parse_proposal_reply, skill_md_path, ProposalOutcome, PROPOSAL_DEADLINE_SECS,
    PROPOSAL_MAX_STEPS, PROPOSE_DIRECTIVE,
};

use super::App;
use crate::tui::state::{Entry, Mode, SkillProposalModal};

/// One spawned skill-proposal task's eventual reply. `result.agent_id`
/// (when `Ok`) is the now-already-purged ephemeral child's id, kept ONLY
/// for [`SkillProposalModal::child`]'s own display/debugging purpose -- see
/// this module's own doc for why there is no separate fate over it.
pub(super) struct SkillProposeDone {
    pub(super) result: conway::Result<AgentResult>,
}

/// The bounded [`ForkSpec`] this feature forks with -- see this module's own
/// doc, "The fork's own bound".
fn propose_fork_spec() -> ForkSpec {
    ForkSpec::new(PROPOSE_DIRECTIVE)
        .ephemeral(true)
        .budget(Budget {
            max_steps: PROPOSAL_MAX_STEPS,
            deadline: Some(
                chrono::Utc::now()
                    + chrono::Duration::seconds(
                        i64::try_from(PROPOSAL_DEADLINE_SECS).unwrap_or(i64::MAX),
                    ),
            ),
            max_tokens: None,
            max_tool_calls: None,
        })
}

/// Forks `root`, awaits the child's single reply, and purges the child
/// UNCONDITIONALLY (this module's own doc) before sending the result back
/// on `tx`. A free function (not an `App` method), mirroring `await_cmd::
/// run_await`'s own shape: it runs inside a `tokio::spawn`ed task that
/// outlives any single `submit` call, so it cannot borrow `self`.
pub(super) async fn run_skill_propose(
    handle: SessionHandle,
    conway: Conway,
    root: AgentId,
    tx: tokio::sync::mpsc::UnboundedSender<SkillProposeDone>,
) {
    let result = match handle.fork(root, propose_fork_spec()).await {
        Ok(child) => {
            let awaited = handle.await_agent(child).await;
            // Distillate only -- purge regardless of outcome, best-effort:
            // a purge failure here only leaves residue for the next
            // startup's own crash sweep, mirroring every other best-effort
            // ephemeral-child cleanup in this crate.
            let _ = conway.purge(child).await;
            awaited
        }
        Err(e) => Err(e),
    };
    // The receiver only goes away when `App::run`'s loop has already
    // exited -- nothing left to notify, so a send failure here is silently
    // dropped, mirroring `run_await`/`run_modal_ask`'s own send sites.
    let _ = tx.send(SkillProposeDone { result });
}

impl App {
    /// Spawns the skill-proposal task off this loop's own `select!`, never
    /// on it -- see this module's own doc and `commands::Effect::
    /// RunSkillPropose`'s for why this specific method (not `commands::
    /// execute` itself) is what does the actual `tokio::spawn`.
    /// `commands::execute`'s `SlashCommand::SkillsPropose` arm has already
    /// checked `write_approval`/set `state.skill_propose_in_flight` before
    /// returning the effect that reaches this call.
    pub(super) fn spawn_skill_propose(&self, root: AgentId) {
        let handle = self.handle.clone();
        let conway = self.conway.clone();
        let tx = self.skill_propose_tx.clone();
        tokio::spawn(async move {
            run_skill_propose(handle, conway, root, tx).await;
        });
    }

    /// The directory `conway_plugin_skills::skill_md_path` joins a name
    /// onto -- the SAME fixed `.conway/skills` operator root
    /// `ConwayBuilder::build`/`first_party_plugins::bundle`'s own
    /// `SkillsPlugin::from_dir` call both read, resolved against THIS
    /// process's own `ConwayConfig::cwd` (already absolute -- `conway`'s
    /// own config loader normalizes it).
    fn skills_root(&self) -> std::path::PathBuf {
        self.conway.config().cwd.join(".conway").join("skills")
    }

    /// Applies one [`SkillProposeDone`] reply: clears `state.
    /// skill_propose_in_flight` and either posts a plain notice (no
    /// proposal was warranted, the reply could not be parsed, or the fork
    /// itself failed) or opens the skill-proposal modal over a validated
    /// proposal -- reading whatever `.conway/skills/<name>/SKILL.md`
    /// already holds (if anything) so the modal can show a diff for an
    /// UPDATE rather than silently replacing it (acceptance: "An update to
    /// an existing skill shows a diff rather than silently replacing it").
    /// Called from `App::run`'s own `skill_propose_rx.recv()` arm,
    /// unconditionally -- mirrors `App::apply_await_done`'s own doc for why
    /// this is never gated on `state.mode`.
    pub(super) fn apply_skill_propose_done(&mut self, done: SkillProposeDone) {
        self.state.skill_propose_in_flight = false;
        let result = match done.result {
            Ok(result) => result,
            Err(e) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("conway.skills propose failed: {e}"),
                });
                return;
            }
        };
        match parse_proposal_reply(&result.summary) {
            ProposalOutcome::NoneWarranted => {
                self.state.transcript.push(Entry::Notice {
                    text: "conway.skills: no skill proposal was warranted for this task"
                        .to_string(),
                });
            }
            ProposalOutcome::Malformed { reason } => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("conway.skills: the proposal could not be used ({reason})"),
                });
            }
            ProposalOutcome::Proposal {
                name,
                description,
                content,
            } => {
                let path = skill_md_path(&self.skills_root(), &name);
                let existing = std::fs::read_to_string(&path).ok();
                let mut modal = SkillProposalModal {
                    child: result.agent_id,
                    name,
                    description,
                    content,
                    existing,
                    diff: None,
                    error: None,
                };
                modal.recompute_diff();
                self.state.offer_skill_proposal(modal);
            }
        }
    }

    /// `Enter` on the skill-proposal modal: writes `modal.content` verbatim
    /// to `.conway/skills/<name>/SKILL.md` (creating the directory if
    /// needed), closes the modal, and records the write as a transcript
    /// notice -- the operator-visible "every write recorded" acceptance
    /// criterion; see this crate's own `docs/plugins/skills.md` for the
    /// disclosed reason this lands as a transcript `Notice` rather than a
    /// durable `LogRecord::SystemNote` (no facade primitive lets an
    /// external caller append one to a live session without starting a new
    /// turn, and `conway-core` is out of this slice's fence). A failed
    /// write keeps the modal OPEN with the error shown
    /// (`AppState::fail_skill_proposal`) -- mirrors `commands::
    /// apply_ask_fate`'s own "a failed fate never silently vanishes" rule.
    /// A no-op when no skill-proposal modal is open.
    pub(super) fn write_skill_proposal(&mut self) {
        let (name, content) = match &self.state.mode {
            Mode::SkillProposal(modal) => (modal.name.clone(), modal.content.clone()),
            _ => return,
        };
        let path = skill_md_path(&self.skills_root(), &name);
        let outcome: std::io::Result<()> = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &content)
        })();
        match outcome {
            Ok(()) => {
                self.state.close_skill_proposal();
                self.state.transcript.push(Entry::Notice {
                    text: format!("conway.skills: wrote {}", path.display()),
                });
            }
            Err(e) => {
                self.state
                    .fail_skill_proposal(format!("could not write {}: {e}", path.display()));
            }
        }
    }

    /// `e` on the skill-proposal modal: suspends the terminal and opens
    /// `modal.content` in `$VISUAL`/`$EDITOR`/`vi`, exactly like `Ctrl-G`'s
    /// `Action::OpenExternalEditor` arm does for the main input line
    /// (`app/editor.rs`) -- the ONE piece of this modal's own key handling
    /// that needs a live terminal, which is why it is a separate `App`
    /// method (called from `run.rs`'s own `select!` loop, which holds the
    /// terminal) rather than living in `input::handle_key` (`input.rs`'s
    /// own "stays pure with respect to I/O" doc). Factored out of that
    /// `run.rs` match arm (review round 1, board item
    /// `01M3DTT078W25MD2S4527R0WAV`) so it is directly callable from a test
    /// with a `Terminal<TestBackend>` and a fake `$EDITOR` script, mirroring
    /// `write_skill_proposal`'s own "testable without driving `App::run`
    /// itself" shape. A no-op when no skill-proposal modal is open.
    ///
    /// **`editor_command` is a caller-resolved parameter, not read from
    /// `std::env` internally** -- the SAME rule `app/editor.rs`'s own
    /// module doc states for `edit_prompt_externally` itself, applied here
    /// one layer up: `run.rs`'s own call site resolves `editor::
    /// resolve_editor_command()` and passes it in (mirroring `Action::
    /// OpenExternalEditor`'s own arm exactly), so this method's own tests
    /// can exercise a real spawned child process against a known script
    /// path without ever touching `std::env::set_var` (this crate's own
    /// documented hazard: process-global mutation racing `cargo test`'s
    /// parallel-by-default threads).
    pub(super) fn apply_skill_proposal_edit_action<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
        editor_command: &str,
    ) {
        let Mode::SkillProposal(modal) = &self.state.mode else {
            return;
        };
        let current = modal.content.clone();
        let outcome = super::editor::edit_prompt_externally(terminal, &current, editor_command);
        match outcome {
            super::editor::EditorOutcome::Replace(text) => {
                self.state.apply_skill_proposal_edit(text);
            }
            super::editor::EditorOutcome::Unchanged => {}
            super::editor::EditorOutcome::Failed { notice } => {
                self.state.transcript.push(Entry::Notice { text: notice });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Slice 2's own primary test: `/conway.skills.propose` drives a REAL
    //! `Conway` (a `ScriptedBackend` returning a fixed SKILL.md, per this
    //! item's own acceptance criterion), and the write to disk happens
    //! ONLY after `Enter` -- never on `Esc`, never merely because the
    //! proposal appeared (P-15: asserted on the file's existence and
    //! content, not on an intermediate signal like `state.mode`).

    use std::sync::Arc;
    use std::time::Duration;

    use conway::test_support::base_config_at;
    use conway::{Conway, SessionFilter};
    use conway_core::ids::BackendId;
    use conway_testkit::{text_response, ScriptedBackend, ScriptedTurn};

    use super::super::fixtures::minimal_cli;
    use super::App;
    use crate::tui::input::{self, Action};
    use crate::tui::state::{Mode, SkillProposalFate};
    use crate::tui::test_support::key;

    /// See `ask.rs`'s own `HANG_TIMEOUT` doc for why every bound in this
    /// module is a hang detector, not a promptness assertion.
    const HANG_TIMEOUT: Duration = Duration::from_secs(60);

    const FIXED_SKILL_MD: &str = "---\nname: propose-test-skill\ndescription: a test fixture \
                                   skill.\n---\n\n## propose-test-skill\n\nDo the thing this way.\n";

    /// A real, fully in-memory `Conway` whose backend scripts the ephemeral
    /// child's ONE turn as a fixed SKILL.md reply -- the root itself is
    /// never prompted, so nothing else consumes the script. `cwd` points at
    /// a fresh tempdir so `App::skills_root`'s `.conway/skills` write
    /// target is isolated per test.
    fn conway_with_fixed_proposal(cwd: &std::path::Path) -> Conway {
        let backend = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(FIXED_SKILL_MD))])
                .with_id(BackendId::new("fake")),
        );
        conway::test_support::test_builder(base_config_at(cwd))
            .with_backend(backend)
            .build()
            .expect("build should succeed with every port injected")
    }

    /// Drains `app.skill_propose_rx` for the ONE `SkillProposeDone` reply
    /// `/conway.skills.propose` produces.
    async fn recv_done(app: &mut App) -> super::SkillProposeDone {
        tokio::time::timeout(
            HANG_TIMEOUT,
            app.skill_propose_rx
                .as_mut()
                .expect("skill_propose_rx is set by App::new")
                .recv(),
        )
        .await
        .expect("the spawned propose task must reply promptly")
        .expect("skill_propose_tx's sender half is alive for the duration")
    }

    /// **The primary test.** `Enter` writes the file; `Esc` (a separate run,
    /// below) does not; the proposal APPEARING (before `Enter`) does not
    /// either.
    #[tokio::test]
    async fn enter_writes_the_proposed_skill_only_after_enter_is_pressed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conway = conway_with_fixed_proposal(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let path = dir
            .path()
            .join(".conway")
            .join("skills")
            .join("propose-test-skill")
            .join("SKILL.md");

        app.submit("/conway.skills.propose".to_string())
            .await
            .expect("submit should not error");
        assert!(app.state.skill_propose_in_flight);

        let done = recv_done(&mut app).await;
        app.apply_skill_propose_done(done);
        assert!(!app.state.skill_propose_in_flight);

        // The proposal has ARRIVED (the modal is open) -- still not written.
        assert!(
            matches!(app.state.mode, Mode::SkillProposal(_)),
            "expected the skill-proposal modal to be open, got {:?}",
            app.state.mode
        );
        assert!(
            !path.exists(),
            "the file must not exist merely because the proposal appeared: {}",
            path.display()
        );
        match &app.state.mode {
            Mode::SkillProposal(modal) => {
                assert_eq!(modal.name, "propose-test-skill");
                assert_eq!(modal.description.as_deref(), Some("a test fixture skill."));
                assert!(modal.content.contains("Do the thing this way."));
                assert!(modal.existing.is_none(), "this is a brand-new skill");
                assert!(
                    modal.diff.is_none(),
                    "a brand-new skill has nothing to diff"
                );
            }
            other => panic!("expected Mode::SkillProposal, got {other:?}"),
        }

        // `Enter` -- via the REAL key-handling path, not a hand-built
        // Action -- then the REAL production write method `run.rs`'s own
        // `Action::SkillProposalFate` arm calls.
        let action = input::handle_key(
            &mut app.state,
            key(ratatui::crossterm::event::KeyCode::Enter),
        );
        assert_eq!(action, Action::SkillProposalFate(SkillProposalFate::Write));
        app.write_skill_proposal();

        assert!(
            path.exists(),
            "the file must exist after Enter: {}",
            path.display()
        );
        let written = std::fs::read_to_string(&path).expect("the written file must be readable");
        assert_eq!(written, FIXED_SKILL_MD);
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "the modal must close after a successful write, got {:?}",
            app.state.mode
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                crate::tui::state::Entry::Notice { text }
                    if text.contains("wrote") && text.contains("propose-test-skill")
            )),
            "the write must be recorded as a notice: {:?}",
            app.state.transcript
        );
    }

    /// `Esc` discards -- the file is never written, even though the SAME
    /// fixture proposal reached the modal.
    #[tokio::test]
    async fn esc_discards_and_never_writes_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conway = conway_with_fixed_proposal(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let path = dir
            .path()
            .join(".conway")
            .join("skills")
            .join("propose-test-skill")
            .join("SKILL.md");

        app.submit("/conway.skills.propose".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;
        app.apply_skill_propose_done(done);
        assert!(matches!(app.state.mode, Mode::SkillProposal(_)));

        let action =
            input::handle_key(&mut app.state, key(ratatui::crossterm::event::KeyCode::Esc));
        assert_eq!(
            action,
            Action::SkillProposalFate(SkillProposalFate::Discard)
        );
        // Mirrors `run.rs`'s own `SkillProposalFate::Discard` arm exactly.
        app.state.close_skill_proposal();

        assert!(
            !path.exists(),
            "Esc must never write the file: {}",
            path.display()
        );
        assert!(matches!(app.state.mode, Mode::Normal));
    }

    /// `write_approval = never` suppresses EVERYTHING: no fork (asserted
    /// structurally, via the session tree's ephemeral-inclusive listing --
    /// P-15: not merely "no file appeared"), no modal, and `state.
    /// skill_propose_in_flight` never flips.
    #[tokio::test]
    async fn write_approval_never_suppresses_the_fork_entirely() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The SAME fixture backend as the positive test -- if a fork
        // somehow DID happen, it would succeed and produce a proposal,
        // which is exactly what this test must prove did NOT occur (P-15:
        // a fixture with a trigger that WOULD have fired, not one that
        // trivially can't).
        let mut config = base_config_at(dir.path());
        config.plugins.config.insert(
            conway_plugin_skills::PLUGIN_ID.to_string(),
            serde_json::json!({ "write_approval": "never" }),
        );
        let backend = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(FIXED_SKILL_MD))])
                .with_id(BackendId::new("fake")),
        );
        let conway = conway::test_support::test_builder(config)
            .with_backend(backend)
            .build()
            .expect("build should succeed");
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let before = app
            .conway
            .sessions(SessionFilter {
                include_ephemeral: true,
                ..Default::default()
            })
            .await
            .expect("sessions() should succeed");

        app.submit("/conway.skills.propose".to_string())
            .await
            .expect("submit should not error");

        assert!(
            !app.state.skill_propose_in_flight,
            "write_approval=never must never even start a proposal"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                crate::tui::state::Entry::Notice { text } if text.contains("never")
            )),
            "the refusal must be recorded as a notice: {:?}",
            app.state.transcript
        );

        // No task was ever spawned -- a bounded wait on the reply channel
        // must time out, never resolve.
        let recv = tokio::time::timeout(
            Duration::from_millis(200),
            app.skill_propose_rx
                .as_mut()
                .expect("skill_propose_rx is set by App::new")
                .recv(),
        )
        .await;
        assert!(
            recv.is_err(),
            "LOAD-BEARING: no SkillProposeDone must ever arrive when write_approval=never -- a \
             reply here would mean the fork happened anyway"
        );

        // LOAD-BEARING (P-15): the fork genuinely never happened -- the
        // session tree's own ephemeral-inclusive listing is unchanged, not
        // merely "no file appeared on disk."
        let after = app
            .conway
            .sessions(SessionFilter {
                include_ephemeral: true,
                ..Default::default()
            })
            .await
            .expect("sessions() should succeed");
        assert_eq!(
            before.len(),
            after.len(),
            "no ephemeral child session must have been created: before {before:?}, after \
             {after:?}"
        );

        let path = dir
            .path()
            .join(".conway")
            .join("skills")
            .join("propose-test-skill")
            .join("SKILL.md");
        assert!(!path.exists());
    }

    /// An update to an EXISTING skill of the same name shows a diff rather
    /// than silently replacing it (acceptance criterion, stated verbatim).
    #[tokio::test]
    async fn an_update_to_an_existing_skill_shows_a_diff() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir
            .path()
            .join(".conway")
            .join("skills")
            .join("propose-test-skill");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        let existing = "---\nname: propose-test-skill\ndescription: the OLD description.\n---\n\n\
                         ## propose-test-skill\n\nThe OLD body.\n";
        std::fs::write(skill_dir.join("SKILL.md"), existing).expect("write existing skill");

        let conway = conway_with_fixed_proposal(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/conway.skills.propose".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;
        app.apply_skill_propose_done(done);

        match &app.state.mode {
            Mode::SkillProposal(modal) => {
                assert_eq!(modal.existing.as_deref(), Some(existing));
                let diff = modal
                    .diff
                    .as_deref()
                    .expect("an update over an existing skill must produce a diff");
                assert!(diff.contains("-The OLD body."), "{diff}");
                assert!(diff.contains("+Do the thing this way."), "{diff}");
            }
            other => panic!("expected Mode::SkillProposal, got {other:?}"),
        }

        // Enter still writes -- replacing the old content, verbatim.
        app.write_skill_proposal();
        let written = std::fs::read_to_string(skill_dir.join("SKILL.md"))
            .expect("the written file must be readable");
        assert_eq!(written, FIXED_SKILL_MD);
    }

    /// **Acceptance criterion, stated verbatim: "A written skill appears in
    /// the next session's skill index."** Review round 1 (board item
    /// `01M3DTT078W25MD2S4527R0WAV`): the ONLY prior coverage was
    /// `conway_plugin_skills::propose`'s own pure-parser unit tests --
    /// nothing drove the write through the real `/conway.skills.propose`
    /// Enter path and then reloaded it through a genuinely FRESH `Conway`
    /// the way the next session actually would.
    ///
    /// **Not vacuous (P-15):** an agent def naming the skill is written up
    /// front, and the BEFORE half proves the skill is genuinely ABSENT --
    /// starting a session under that def fails outright (`conway_runtime::
    /// runtime::root::resolve_skills`'s own hard "agent def names unknown
    /// skill" error, since resolution happens at session-start time, not
    /// `ConwayBuilder::build` time) -- rather than merely "this test never
    /// tried." The AFTER half builds a SECOND, wholly independent `Conway`
    /// (a fresh `ConwayBuilder`, a fresh `ScriptedBackend`, `conway.skills`
    /// installed) over the SAME `.conway` directory and drives a REAL turn:
    /// the assembled request the backend actually receives is the
    /// observable, not `SkillDef`/`from_dir` parsing in isolation -- and,
    /// with the plugin installed, it is the NARROWED one-line index entry
    /// (`read_skill(name="...")`), the literal "skill index" the acceptance
    /// criterion names.
    #[tokio::test]
    async fn a_written_skill_appears_in_the_next_sessions_skill_index_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");

        // An agent def naming the skill the propose flow will write, below
        // -- written up front: `ConwayBuilder::build` discovers
        // `.conway/agents` independent of whether `.conway/skills` has
        // caught up yet (resolution is deferred to session-start).
        let agents_dir = dir.path().join(".conway").join("agents");
        std::fs::create_dir_all(&agents_dir).expect("create agents dir");
        std::fs::write(
            agents_dir.join("skilled.md"),
            "---\nname: skilled\nskills: [propose-test-skill]\n---\n\nYou use a skill.\n",
        )
        .expect("write agent def");

        // BEFORE: the skill has never been written. A fresh `Conway` builds
        // fine (an agent def naming an unresolved skill is not a build-time
        // error), but STARTING a session under that def fails outright,
        // naming the missing skill -- the non-vacuous proof of absence.
        {
            let backend = Arc::new(
                ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
                    .with_id(BackendId::new("fake")),
            );
            let conway_before = conway::test_support::test_builder(base_config_at(dir.path()))
                .with_backend(backend)
                .build()
                .expect(
                    "build must succeed even though the agent def names a not-yet-written \
                     skill -- resolution happens at session-start, not build time",
                );
            let err = match conway_before
                .new_session(conway::SessionSpec {
                    agent_def: Some("skilled".to_string()),
                    ..Default::default()
                })
                .await
            {
                Ok(_) => panic!(
                    "starting a session under an agent def naming a not-yet-written skill must \
                     fail"
                ),
                Err(e) => e,
            };
            assert!(
                err.to_string().contains("propose-test-skill"),
                "the refusal must name the missing skill: {err}"
            );
        }

        // Propose and write the skill through the REAL `/conway.skills.
        // propose` Enter path -- not a hand-written fixture file.
        let conway = conway_with_fixed_proposal(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        app.submit("/conway.skills.propose".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;
        app.apply_skill_propose_done(done);
        app.write_skill_proposal();
        let skill_path = dir
            .path()
            .join(".conway")
            .join("skills")
            .join("propose-test-skill")
            .join("SKILL.md");
        assert!(
            skill_path.exists(),
            "the propose flow must have written the file before the AFTER half runs"
        );

        // AFTER: a WHOLLY FRESH `Conway` -- new `ConwayBuilder`, new
        // `ScriptedBackend`, `conway.skills` installed -- simulating the
        // next session. It now finds the skill, a session under the SAME
        // agent def starts successfully, and a real turn's assembled
        // request carries the narrowed index entry.
        let backend_after = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
                .with_id(BackendId::new("fake")),
        );
        let skills_plugin = conway_plugin_skills::SkillsPlugin::from_dir(
            &dir.path().join(".conway").join("skills"),
        )
        .expect("SkillsPlugin::from_dir must load the freshly-written skill");
        let conway_after = conway::test_support::test_builder(base_config_at(dir.path()))
            .with_backend(backend_after.clone())
            .with_plugin(std::sync::Arc::new(skills_plugin))
            .build()
            .expect("build should succeed now that the skill exists on disk");
        let handle = conway_after
            .new_session(conway::SessionSpec {
                agent_def: Some("skilled".to_string()),
                ..Default::default()
            })
            .await
            .expect("starting a session under the agent def must now succeed");
        let turn = handle.prompt("go").await.expect("prompt");
        let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
            .await
            .expect("result() must not hang")
            .expect("result() should succeed");

        let requests = backend_after.calls();
        assert!(
            !requests.is_empty(),
            "the backend must have received a request"
        );
        let skill_text = requests[0]
            .segments
            .iter()
            .find_map(|s| {
                if matches!(
                    &s.provenance,
                    conway_core::provenance::Provenance::Skill { name } if name == "propose-test-skill"
                ) {
                    s.content.iter().find_map(|b| match b {
                        conway_core::content::ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                panic!(
                    "the assembled request must contain a Provenance::Skill {{ name: \
                     \"propose-test-skill\" }} segment; provenances seen: {:?}",
                    requests[0]
                        .segments
                        .iter()
                        .map(|s| s.provenance.clone())
                        .collect::<Vec<_>>()
                )
            });
        assert!(
            skill_text.contains("read_skill(name=\"propose-test-skill\")"),
            "the next session's skill index must be the narrowed one-line entry: {skill_text}"
        );
        assert!(
            !skill_text.contains("Do the thing this way."),
            "the full body must not survive into the narrowed index: {skill_text}"
        );
    }

    // -----------------------------------------------------------------
    // `e` -- the external-editor path (review round 1, board item
    // `01M3DTT078W25MD2S4527R0WAV`). Drives the REAL `App::
    // apply_skill_proposal_edit_action` (the method `run.rs`'s own
    // `Action::SkillProposalEdit` arm calls) against a real spawned child
    // process -- not merely `editor::edit_prompt_externally` in isolation
    // (already covered by `app/editor.rs`'s own tests).
    // -----------------------------------------------------------------

    /// A test-only "editor": a tiny shell script this test writes and marks
    /// executable, so `apply_skill_proposal_edit_action` spawns a REAL child
    /// process -- mirrors `app/editor.rs`'s own `write_test_script` exactly
    /// (a second, independent copy: that helper is private to its own
    /// module's test tree, and six lines is cheaper than widening its
    /// visibility for one more caller in a different module).
    fn write_test_script(name: &str, body: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "conway-skill-propose-edit-test-{}-{}-{name}.sh",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, body).expect("write must succeed against a writable temp path");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .expect("metadata must succeed")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod must succeed");
        }
        path
    }

    /// Retries on the ETXTBSY race `app/editor.rs`'s own module doc
    /// describes (a fresh script written and exec'd moments later, unsafe
    /// under `cargo test`'s parallelism) -- same bounded shape as that
    /// module's own retry wrapper, a second, independent copy for the
    /// identical "private to its own test tree" reason `write_test_script`
    /// above already gives.
    fn apply_skill_proposal_edit_action_retrying<B: ratatui::backend::Backend>(
        app: &mut App,
        terminal: &mut ratatui::Terminal<B>,
        editor_command: &str,
    ) {
        const ATTEMPTS: u32 = 10;
        for attempt in 0..ATTEMPTS {
            app.apply_skill_proposal_edit_action(terminal, editor_command);
            let busy = matches!(
                &app.state.mode,
                Mode::SkillProposal(modal)
                    if modal.error.as_deref().is_some_and(|e| e.contains("Text file busy"))
            );
            if !busy {
                return;
            }
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(Duration::from_millis(20 * u64::from(attempt + 1)));
            }
        }
        panic!(
            "apply_skill_proposal_edit_action lost the ETXTBSY race {ATTEMPTS} times in a row \
             for {editor_command:?}; that is no longer a race, investigate rather than raising \
             the bound"
        );
    }

    fn test_terminal() -> ratatui::Terminal<ratatui::backend::TestBackend> {
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 5))
            .expect("TestBackend construction cannot fail")
    }

    /// **`e` opens the editor, and its edit is applied through the REAL
    /// `run.rs` call path.** A real child process (a shell script) appends
    /// text to the temp file `apply_skill_proposal_edit_action` writes the
    /// modal's current content into; the modal's `content` afterward is the
    /// edited text, verbatim, and the write target (`name`) is untouched.
    #[tokio::test]
    #[cfg(unix)]
    async fn e_opens_the_editor_and_the_edit_is_applied_through_the_real_run_rs_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conway = conway_with_fixed_proposal(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        // Open the modal directly (no need to drive the whole propose fork
        // for this test -- the fork/parse path is already covered above).
        app.state
            .offer_skill_proposal(crate::tui::state::SkillProposalModal {
                child: conway::AgentId::new(),
                name: "propose-test-skill".to_string(),
                description: None,
                content: "---\nname: propose-test-skill\n---\n\nOriginal body.\n".to_string(),
                existing: None,
                diff: None,
                error: None,
            });

        let script = write_test_script(
            "appends",
            "#!/bin/sh\necho 'Appended by editor.' >> \"$1\"\nexit 0\n",
        );
        let mut terminal = test_terminal();

        apply_skill_proposal_edit_action_retrying(
            &mut app,
            &mut terminal,
            script.to_str().expect("utf8 path"),
        );
        let _ = std::fs::remove_file(&script);

        match &app.state.mode {
            Mode::SkillProposal(modal) => {
                assert!(
                    modal.content.contains("Appended by editor."),
                    "the editor's own edit must round-trip into the modal's content: {}",
                    modal.content
                );
                assert_eq!(
                    modal.name, "propose-test-skill",
                    "the write target must be untouched by an external edit"
                );
            }
            other => panic!("expected Mode::SkillProposal to still be open, got {other:?}"),
        }

        // And writing now lands at the ORIGINAL path, with the EDITED
        // content -- the full round trip this modal exists for.
        app.write_skill_proposal();
        let path = dir
            .path()
            .join(".conway")
            .join("skills")
            .join("propose-test-skill")
            .join("SKILL.md");
        let written = std::fs::read_to_string(&path).expect("the written file must be readable");
        assert!(written.contains("Appended by editor."), "{written}");
    }

    /// A non-zero editor exit leaves the modal's content UNCHANGED (a
    /// `Failed` outcome, never `Replace`) -- mirrors `app/editor.rs`'s own
    /// `editor_exiting_non_zero_leaves_input_unchanged` for this modal.
    #[tokio::test]
    #[cfg(unix)]
    async fn e_with_a_failing_editor_leaves_the_modal_content_unchanged_and_notes_the_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let conway = conway_with_fixed_proposal(dir.path());
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let original_content = "---\nname: propose-test-skill\n---\n\nOriginal body.\n".to_string();
        app.state
            .offer_skill_proposal(crate::tui::state::SkillProposalModal {
                child: conway::AgentId::new(),
                name: "propose-test-skill".to_string(),
                description: None,
                content: original_content.clone(),
                existing: None,
                diff: None,
                error: None,
            });

        let script = write_test_script(
            "fails",
            "#!/bin/sh\necho 'should never be applied' >> \"$1\"\nexit 1\n",
        );
        let mut terminal = test_terminal();

        apply_skill_proposal_edit_action_retrying(
            &mut app,
            &mut terminal,
            script.to_str().expect("utf8 path"),
        );
        let _ = std::fs::remove_file(&script);

        match &app.state.mode {
            Mode::SkillProposal(modal) => {
                assert_eq!(
                    modal.content, original_content,
                    "a non-zero editor exit must not change the modal's content"
                );
            }
            other => panic!("expected Mode::SkillProposal to still be open, got {other:?}"),
        }
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                crate::tui::state::Entry::Notice { text } if text.contains("exited with")
            )),
            "the failure must be recorded as a notice: {:?}",
            app.state.transcript
        );
    }

    // -----------------------------------------------------------------
    // The fork's own budget, enforced on the REAL propose path (review
    // round 1 MINOR item, board item `01M3DTT078W25MD2S4527R0WAV`).
    // -----------------------------------------------------------------

    /// A trivial always-succeeds tool the scripted child calls repeatedly,
    /// past `PROPOSAL_MAX_STEPS` -- mirrors `ask.rs`'s own `MarkerTool`
    /// fixture exactly (a second, independent copy: that one is private to
    /// its own test module).
    struct BudgetMarkerTool;

    #[async_trait::async_trait]
    impl conway::plugin::Tool for BudgetMarkerTool {
        fn spec(&self) -> conway::plugin::ToolSpec {
            conway::plugin::ToolSpec {
                name: conway::plugin::ToolName::new("budget_marker"),
                description: "test-only marker tool".into(),
                schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
                category: conway::plugin::ToolCategory::Read,
                permission: conway::plugin::PermissionClass::Safe,
            }
        }

        async fn invoke(
            &self,
            _call: conway::plugin::ToolCall,
            _ctx: conway::plugin::ToolCtx,
        ) -> Result<conway::plugin::ToolOutput, conway::plugin::ToolError> {
            Ok(conway::plugin::ToolOutput {
                blocks: vec![conway::plugin::ContentBlock::Text {
                    text: "marked".into(),
                }],
                is_error: false,
                truncation: conway::plugin::TruncationPolicy::None,
                artifacts: vec![],
            })
        }

        fn path_args(&self) -> conway::plugin::PathArgs {
            conway::plugin::PathArgs::None
        }
        fn render_kind(&self) -> conway::plugin::RenderKind {
            conway::plugin::RenderKind::Structured
        }
    }

    struct BudgetMarkerPlugin;

    impl conway::plugin::Plugin for BudgetMarkerPlugin {
        fn manifest(&self) -> conway::plugin::PluginManifest {
            conway::plugin::PluginManifest {
                id: "test.budget_marker".to_string(),
                version: "0.0.0".to_string(),
                tools: vec![conway::plugin::ToolName::new("budget_marker")],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
            vec![Arc::new(BudgetMarkerTool)]
        }
    }

    /// **The fork's own bound (15 steps) is actually enforced, on the real
    /// propose path.** A `ScriptedBackend` that keeps calling
    /// `budget_marker` far past `PROPOSAL_MAX_STEPS` (20 scripted rounds,
    /// never a final plain-text reply) cannot run forever: the child's own
    /// turn is cut off by `RuntimeError`/`ResultStatus::BudgetExceeded {
    /// limit: "max_steps=15" }`, and `maybe_propose_skill`'s (mirrored here
    /// by `apply_skill_propose_done`) own parse of the resulting empty/
    /// unusable summary lands on `ProposalOutcome::Malformed` -- a clean
    /// "no proposal," never a hang and never a write. This test's own
    /// `recv_done` call is itself bounded by `HANG_TIMEOUT`, so a
    /// regression that let the child run unbounded would fail this test by
    /// timing out, not merely by asserting the wrong status.
    #[tokio::test]
    async fn the_forks_own_budget_is_enforced_and_produces_a_clean_non_proposal() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A tool-call response, every round, never a final plain-text reply
        // -- the child's own turn never naturally ends on its own.
        let rounds: Vec<ScriptedTurn> = (0..20)
            .map(|i| {
                ScriptedTurn::Respond(conway_core::ports::GenerateResponse {
                    content: vec![],
                    tool_calls: vec![conway_core::content::ToolCall {
                        call_id: format!("call_{i}"),
                        name: conway::plugin::ToolName::new("budget_marker"),
                        arguments: serde_json::json!({}),
                    }],
                    stop: conway_core::content::StopReason::ToolUse,
                    usage: conway_core::content::Usage::default(),
                })
            })
            .collect();
        let backend = Arc::new(ScriptedBackend::new(rounds).with_id(BackendId::new("fake")));
        let conway = conway::test_support::test_builder(base_config_at(dir.path()))
            .with_backend(backend)
            .with_plugin(Arc::new(BudgetMarkerPlugin))
            .build()
            .expect("build should succeed with the marker plugin installed");
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("/conway.skills.propose".to_string())
            .await
            .expect("submit should not error");
        let done = recv_done(&mut app).await;

        let result = done
            .result
            .as_ref()
            .expect("the child must reach a terminal AgentResult, not a facade error");
        assert!(
            matches!(
                &result.status,
                conway::ResultStatus::BudgetExceeded { limit } if limit.contains("max_steps=15")
            ),
            "the child must be cut off at the documented step bound, got: {:?}",
            result.status
        );

        app.apply_skill_propose_done(done);
        assert!(
            matches!(app.state.mode, Mode::Normal),
            "a budget-exceeded reflection must never open the proposal modal, got {:?}",
            app.state.mode
        );
        let path = dir
            .path()
            .join(".conway")
            .join("skills")
            .join("propose-test-skill")
            .join("SKILL.md");
        assert!(
            !path.exists(),
            "a cut-off reflection must never write a file"
        );
    }
}
