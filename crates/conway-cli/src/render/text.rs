//! `--output-format text` (the default): stdout carries only the
//! assistant's raw text, verbatim, flushed after every delta, so
//! `conway -p "…" > out.txt` yields clean content. Everything else --
//! tool-call activity, permission denials, backend health, routing detail,
//! progress notes (`AgentProgress`, e.g. the max_tokens silent-turn notice,
//! board item `01M23JRDAM480SRXFGM46GBVA6`), and runway/budget-crossing
//! notices (`BudgetWarning`, board item A5.6) -- is a one-line stderr note
//! (or, for `ModelDecision`, suppressed unless `--verbose`), never mixed
//! into stdout. Board item `01M2MGPF52NHFYN1AKBPR9FDK6`: `AgentProgress`/
//! `BudgetWarning` used to fall into the wildcard arm below and be dropped
//! entirely -- see that arm's own comment.
//!
//! Permission denials are classified by matching the facade-re-exported
//! `PermissionDecisionKind` directly (`Denied`/`DeniedWithFeedback` -- the
//! only decisions `AllowListGate` produces for a rejection).

use std::io::{self, Write};

use conway::{AgentId, AgentResult, Envelope, Event, PermissionDecisionKind};

use super::Renderer;
use crate::diag;

pub struct TextRenderer {
    out: Box<dyn Write + Send>,
    /// Whether the last byte this renderer actually wrote to `out` was a
    /// newline. `None` until the first write, so "nothing written yet"
    /// and "last write ended with a newline" aren't conflated (finishing an
    /// empty run must not emit a bare `\n`).
    ends_with_newline: Option<bool>,
    /// The run's root agent, once [`Renderer::set_root`] supplies it.
    /// `AgentFinished`'s trailing-newline flush fires only for this agent:
    /// a subagent's `AgentFinished` now reaches this session-scoped stream
    /// too (it bypasses the stream filter), and flushing on it would inject
    /// a spurious `\n` into the root's still-streaming stdout. `None` (never
    /// set, e.g. direct unit tests) preserves the pre-subagent behavior of
    /// treating any `AgentFinished` as terminal.
    root: Option<AgentId>,
}

impl TextRenderer {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self {
            out,
            ends_with_newline: None,
            root: None,
        }
    }

    fn write_delta(&mut self, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.out.write_all(text.as_bytes())?;
        self.out.flush()?;
        self.ends_with_newline = Some(text.ends_with('\n'));
        Ok(())
    }

    /// `AgentFinished` -> "trailing `\n` if the last write wasn't one"
    /// (binding table). Idempotent: called from both `on_event` (the
    /// terminal envelope, as the render loop streams it) and `finish` (the
    /// same occurrence, handed back as an `AgentResult` once the loop
    /// exits) -- the second call sees `ends_with_newline == Some(true)`
    /// already and is a no-op, so the newline is never written twice.
    fn ensure_trailing_newline(&mut self) -> io::Result<()> {
        if self.ends_with_newline == Some(false) {
            self.out.write_all(b"\n")?;
            self.ends_with_newline = Some(true);
        }
        Ok(())
    }

    /// `true` for either denial variant of `PermissionDecisionKind`.
    fn decision_is_denied(decision: &PermissionDecisionKind) -> bool {
        matches!(
            decision,
            PermissionDecisionKind::Denied | PermissionDecisionKind::DeniedWithFeedback
        )
    }
}

impl Renderer for TextRenderer {
    fn on_event(&mut self, env: &Envelope) -> io::Result<()> {
        match &env.event {
            Event::TextDelta { text } => self.write_delta(text)?,
            Event::ThinkingDelta { .. } => {}
            // A mid-stream failure discarded whatever partial text this
            // attempt had already streamed to stdout -- board item
            // `01M1FSJ4E2S5M9KBSBJAAPJQ48`. Unlike every other diagnostic in
            // this file, this one MUST touch stdout too: partial text
            // already written there cannot be unprinted, so the newline
            // marks the discard boundary before the retry's own text
            // starts, and `docs/scripting.md`'s "Streaming" section tells a
            // script that needs clean stdout to use `--output-format json`
            // instead (non-streaming, so this never fires there).
            Event::StreamRestarted { attempt, .. } => {
                self.out.write_all(b"\n")?;
                self.out.flush()?;
                self.ends_with_newline = Some(true);
                diag::warn(format!(
                    "stream restarted (attempt {attempt}); partial output above discarded"
                ));
            }
            // A tool call going normally is not a warning, and the level
            // here is load-bearing rather than cosmetic. Emitting the whole
            // lifecycle at warning level did two things, and the second is
            // the one that matters: a healthy run looked alarming, and a
            // GENUINE failure became unfindable among dozens of identically
            // prefixed lines that were all fine (board item
            // `01M0PSJZ18R02JJ5NHH3G6ZV9S`, found by using conway rather
            // than by reading this file -- nothing here is wrong on paper).
            //
            // So: the routine lifecycle is `info`, which `diag` already
            // gates behind `--verbose`, and only the two things an operator
            // would actually act on -- a call that FAILED, and a call that
            // was DENIED -- stay unconditional. The opaque call ids ride
            // along at `info` for correlating a `--verbose` trace; nobody
            // matches 22-character ids by eye at the default verbosity.
            Event::ToolCallProposed { call_id, tool, .. } => {
                diag::info(format!("tool call proposed: {tool} ({call_id})"));
            }
            Event::ToolCallStarted { call_id } => {
                diag::info(format!("tool call started ({call_id})"));
            }
            Event::ToolCallFinished {
                call_id, is_error, ..
            } => {
                if *is_error {
                    diag::warn(format!("tool call failed ({call_id})"));
                } else {
                    diag::info(format!("tool call finished ({call_id}): ok"));
                }
            }
            Event::PermissionResolved { call_id, decision } => {
                if Self::decision_is_denied(decision) {
                    diag::warn(format!("permission denied for call {call_id}"));
                }
            }
            Event::ModelDecision { role, chosen, .. } => {
                diag::info(format!("routed role '{role}' to {chosen}"));
            }
            Event::BackendDegraded { endpoint, .. } => {
                diag::warn(format!("backend degraded: {endpoint}"));
            }
            Event::Error { error, fatal: true } => {
                diag::error(error.to_string());
            }
            // Board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`: a keep_alive root's
            // turn-scoped budget trip no longer ends the session, it aborts
            // just the turn -- see `Event::TurnAborted`'s own doc. One
            // stderr line is enough for this renderer; a one-shot `conway
            // -p` run is never `keep_alive` in practice (`SessionSpec::
            // keep_alive` is an opt-in only the interactive/library facade
            // sets), so this arm exists for exhaustiveness/forward
            // compatibility more than for anything a real one-shot run
            // triggers today.
            Event::TurnAborted { limit, .. } => {
                diag::warn(format!("turn ended: {limit} reached"));
            }
            // Only the ROOT's finish is this run's terminal occasion. A
            // subagent's `AgentFinished` reaches this stream too (lifecycle
            // events bypass the session/agent filter), and flushing on it
            // would split the root's still-streaming stdout with a stray
            // `\n`. Until `set_root` is called (`None`), any finish is
            // treated as terminal, preserving the single-agent behavior.
            Event::AgentFinished { .. } => {
                if self.root.is_none_or(|root| env.agent == root) {
                    self.ensure_trailing_newline()?;
                }
            }
            // Board item `01M2MGPF52NHFYN1AKBPR9FDK6`: previously fell into
            // the wildcard arm below and was silently dropped -- the ONLY
            // renderer this happened to. The TUI's own `tui/state.rs`
            // already pushes every `AgentProgress` note as an unconditional
            // `Entry::Notice` (never gated behind focus or `--verbose`); a
            // plain `conway -p` run deserves the same visibility, not less.
            // This carries a wide range of free-text notes -- steering,
            // replay-synthesized `SystemNote`s, and, since board item
            // `01M23JRDAM480SRXFGM46GBVA6`, the live twin of the
            // `max_tokens_silent`/`max_tokens_truncated` notice -- and every
            // one of them is exactly the kind of thing SILENCE would make
            // indistinguishable from a hang, a crash, or a bug in conway
            // itself (that board item's own motivating incident). `diag::
            // warn`, unconditional, never `diag::info` (which `--verbose`
            // would hide by default): stderr only, never stdout, so the
            // one-shot stdout contract (`docs/scripting.md`) is untouched.
            Event::AgentProgress { note } => diag::warn(note),
            // Board item `01M2MGPF52NHFYN1AKBPR9FDK6`: same gap, for the
            // runway/budget-crossing notice (board item A5.6,
            // `conway_runtime::runway`). `text` is already the exact
            // model-facing sentence that event's own doc says it carries
            // (e.g. `runway: 4 of 5 max_steps used this session
            // (max_steps=5). Wrap up or report now.`) -- no second
            // formatting pass needed, mirroring `Event::AgentProgress`
            // immediately above.
            Event::BudgetWarning { text, .. } => diag::warn(text),
            // `Event` is `#[non_exhaustive]` (`conway-core`'s `event.rs`)
            // and this match lives in a different crate, so rustc requires
            // a wildcard arm here NO MATTER how many variants above are
            // named explicitly -- enumerating the remaining ones (
            // `AgentSpawned`, `UserTurn`, `TurnStarted`, `ToolArgumentCoerced`,
            // `PermissionRequested`, `PermissionDecision`, `ToolProgress`,
            // `ContextSegmentAdded`, `MessageSent`, `SteerQueued`,
            // `SteerDropped`, `AgentPromoted`, `Lagged`, `Error { fatal:
            // false, .. }`) would not remove this arm or make a FUTURE
            // (28th) variant fail to compile here -- it would just add
            // churn without closing the actual gap `AgentProgress`/
            // `BudgetWarning` just closed. So this arm is deliberately left
            // catching the rest, silently, exactly as it always has.
            // What WOULD catch a future silent drop: `conway-core::event`'s
            // own `every_variant_constructs_and_round_trips_with_exact_tag`
            // test already pins the variant count (27) and fails the moment
            // a 28th is added -- that is a real trip-wire an author adding
            // one will hit, but it does not by itself point back at this
            // file. A cheap next step, out of this item's own file fence
            // (`crates/conway-core/src/event.rs`): extend that test's own
            // doc comment (or `Event`'s own module doc) to say explicitly
            // "a new variant needs an explicit look at every `Renderer`
            // (`conway-cli`'s `render/{text,json,jsonl}.rs`) and the TUI's
            // `tui/state.rs::apply`, not just this file" -- turning a count
            // mismatch into a checklist instead of leaving it a silent
            // no-op by default.
            _ => {}
        }
        Ok(())
    }

    fn finish(&mut self, _result: Option<&AgentResult>) -> io::Result<()> {
        self.ensure_trailing_newline()?;
        self.out.flush()
    }

    fn set_root(&mut self, root: AgentId) {
        self.root = Some(root);
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use conway::{AgentId, ResultStatus, SessionId};
    use conway_core::content::{StopReason, Usage};

    use super::*;
    use crate::render::test_support::RecordingWriter;

    fn envelope(session: SessionId, agent: AgentId, event: Event) -> Envelope {
        Envelope {
            seq: 0,
            ts: Utc::now(),
            session,
            agent,
            event,
        }
    }

    #[test]
    fn hello_world_exactly_and_flush_count_matches_delta_count() {
        let writer = RecordingWriter::default();
        let mut renderer = TextRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let agent = AgentId::new();

        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::TextDelta { text: "he".into() },
            ))
            .unwrap();
        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::TextDelta { text: "llo".into() },
            ))
            .unwrap();
        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::TurnFinished {
                    usage: Usage::default(),
                    stop: StopReason::EndTurn,
                },
            ))
            .unwrap();
        let result = AgentResult::new(agent, session, ResultStatus::Completed, "");
        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::AgentFinished {
                    result,
                    ephemeral: false,
                },
            ))
            .unwrap();

        assert_eq!(writer.contents(), b"hello\n");
        assert_eq!(
            writer.flush_count(),
            2,
            "flush must be called exactly once per TextDelta, no more"
        );
    }

    #[test]
    fn thinking_delta_is_suppressed() {
        let writer = RecordingWriter::default();
        let mut renderer = TextRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let agent = AgentId::new();

        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::ThinkingDelta {
                    text: "pondering".into(),
                },
            ))
            .unwrap();

        assert_eq!(writer.contents(), b"");
        assert_eq!(writer.flush_count(), 0);
    }

    /// Board item `01M1FSJ4E2S5M9KBSBJAAPJQ48`: partial stdout text from a
    /// discarded stream attempt cannot be unprinted, so `StreamRestarted`
    /// marks the discard boundary with a newline (distinct from
    /// `ThinkingDelta`'s pure suppression above) rather than dropping it
    /// silently. The stderr diagnostic itself is not captured here (`diag`
    /// writes to the real process stderr, not this renderer's `out`) --
    /// only the stdout-visible half of the contract is asserted.
    #[test]
    fn stream_restarted_writes_a_newline_to_stdout() {
        let writer = RecordingWriter::default();
        let mut renderer = TextRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let agent = AgentId::new();

        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::TextDelta {
                    text: "partial".into(),
                },
            ))
            .unwrap();
        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::StreamRestarted {
                    agent_id: agent,
                    attempt: 2,
                    discarded_text_chars: 7,
                    discarded_thinking_chars: 0,
                },
            ))
            .unwrap();
        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::TextDelta {
                    text: "retried".into(),
                },
            ))
            .unwrap();

        assert_eq!(writer.contents(), b"partial\nretried");
    }

    #[test]
    fn finish_after_on_event_agent_finished_does_not_double_newline() {
        let writer = RecordingWriter::default();
        let mut renderer = TextRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let agent = AgentId::new();

        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::TextDelta { text: "hi".into() },
            ))
            .unwrap();
        let result = AgentResult::new(agent, session, ResultStatus::Completed, "");
        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::AgentFinished {
                    result: result.clone(),
                    ephemeral: false,
                },
            ))
            .unwrap();
        renderer.finish(Some(&result)).unwrap();

        assert_eq!(writer.contents(), b"hi\n");
    }

    #[test]
    fn subagent_finish_does_not_flush_newline_into_root_stream() {
        // Once `set_root` names the root, a *subagent's* AgentFinished
        // (arriving mid-root-stream, now that lifecycle events bypass the
        // stream filter) must NOT inject a trailing `\n` -- only the root's
        // own finish does. Regression guard for the -p clean-output contract.
        let writer = RecordingWriter::default();
        let mut renderer = TextRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let root = AgentId::new();
        let child = AgentId::new();
        renderer.set_root(root);

        renderer
            .on_event(&envelope(
                session,
                root,
                Event::TextDelta {
                    text: "partial".into(),
                },
            ))
            .unwrap();
        // Subagent finishes while the root is still mid-stream.
        let child_result = AgentResult::new(child, session, ResultStatus::Completed, "");
        renderer
            .on_event(&envelope(
                session,
                child,
                Event::AgentFinished {
                    result: child_result,
                    ephemeral: false,
                },
            ))
            .unwrap();
        assert_eq!(
            writer.contents(),
            b"partial",
            "a subagent's finish must not append a newline to the root's stream"
        );

        // The root's own finish still flushes the trailing newline.
        let root_result = AgentResult::new(root, session, ResultStatus::Completed, "");
        renderer
            .on_event(&envelope(
                session,
                root,
                Event::AgentFinished {
                    result: root_result,
                    ephemeral: false,
                },
            ))
            .unwrap();
        assert_eq!(writer.contents(), b"partial\n");
    }

    #[test]
    fn empty_run_finish_writes_nothing() {
        let writer = RecordingWriter::default();
        let mut renderer = TextRenderer::new(Box::new(writer.clone()));
        renderer.finish(None).unwrap();
        assert_eq!(writer.contents(), b"");
    }

    /// Board item `01M2MGPF52NHFYN1AKBPR9FDK6`: `AgentProgress`/
    /// `BudgetWarning` now render to stderr via `diag::warn` (see
    /// `on_event`'s own arms), which this in-process unit test cannot
    /// observe -- `diag` always writes to the real process stderr, never
    /// through this renderer's own `out` handle (`stream_restarted_writes_a_
    /// newline_to_stdout`'s own comment states the same limitation for
    /// `StreamRestarted`'s diagnostic half). What IS observable here, and
    /// is the other half of this item's own acceptance criterion, is that
    /// neither event writes a single byte to STDOUT -- the one-shot
    /// stdout contract (`docs/scripting.md`) stays untouched. The
    /// stderr-visible half is asserted by the compiled-binary integration
    /// test `crates/conway-cli/tests/oneshot_notice_stderr.rs`.
    #[test]
    fn agent_progress_and_budget_warning_never_touch_stdout() {
        let writer = RecordingWriter::default();
        let mut renderer = TextRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let agent = AgentId::new();

        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::AgentProgress {
                    note: "the model exhausted its output token budget (stop: max_tokens) \
                           while still reasoning, and produced no visible answer -- no text, \
                           no tool call."
                        .into(),
                },
            ))
            .unwrap();
        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::BudgetWarning {
                    agent_id: agent,
                    limit: "max_steps=5".into(),
                    text: "runway: 4 of 5 max_steps used this session (max_steps=5). Wrap up \
                           or report now."
                        .into(),
                },
            ))
            .unwrap();

        assert_eq!(
            writer.contents(),
            b"",
            "neither notice may write anything to this renderer's stdout handle"
        );
        assert_eq!(writer.flush_count(), 0);
    }
}
