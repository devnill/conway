//! `--output-format text` (the default): stdout carries only the
//! assistant's raw text, verbatim, flushed after every delta, so
//! `conway -p "…" > out.txt` yields clean content. Everything else --
//! tool-call activity, permission denials, backend health, routing detail --
//! is a one-line stderr note (or, for `ModelDecision`, suppressed unless
//! `--verbose`), never mixed into stdout.
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
}
