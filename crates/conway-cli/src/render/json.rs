//! `--output-format json`: stdout carries nothing at all until the run
//! finishes, then exactly one JSON object -- the terminal `AgentResult` --
//! and nothing else. This mode trades incremental output for "one document
//! in, one document out" scriptability.
//!
//! Two envelopes are the exception, and they do NOT touch stdout: progress
//! notes (`AgentProgress`) and runway/budget-crossing notices
//! (`BudgetWarning`) go to stderr as prose, exactly as `TextRenderer`
//! renders them (board item `01M2MGPF52NHFYN1AKBPR9FDK6`). A caller
//! redirecting stdout to a file still gets one parseable document; a human
//! watching a pipeline still learns that the run is running out of context
//! or that a turn produced nothing. Silence is the one outcome
//! indistinguishable from a hang, and a `json`-mode caller has no
//! transcript to read it from instead. Every other streaming envelope is
//! still dropped.

use std::io::{self, Write};

use conway::{AgentResult, Envelope, Event};

use super::Renderer;
use crate::diag;

pub struct JsonRenderer {
    out: Box<dyn Write + Send>,
}

impl JsonRenderer {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self { out }
    }
}

impl Renderer for JsonRenderer {
    fn on_event(&mut self, env: &Envelope) -> io::Result<()> {
        match &env.event {
            // Board item `01M2MGPF52NHFYN1AKBPR9FDK6`. `diag::warn` writes
            // to the real process stderr, never to `self.out`, so the
            // one-document-on-stdout contract this whole renderer exists
            // for is structurally untouched -- there is no path from here
            // to `self.out` at all.
            Event::AgentProgress { note } => diag::warn(note),
            Event::BudgetWarning { text, .. } => diag::warn(text),
            // Everything else really is dropped: this mode's whole promise
            // is that stdout stays empty until `finish`. The wildcard is
            // unavoidable (`conway_core::Event` is `#[non_exhaustive]` and
            // this is a different crate), so a future variant lands here
            // silently. `conway-core`'s own variant-count test is the
            // trip-wire that catches the addition; this comment is where
            // whoever trips it should look.
            _ => {}
        }
        Ok(())
    }

    fn finish(&mut self, result: Option<&AgentResult>) -> io::Result<()> {
        let Some(result) = result else {
            return Ok(());
        };
        let json = serde_json::to_string(result)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.out.write_all(json.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use conway::{AgentId, Event, ResultStatus, SessionId};

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
    fn writes_nothing_on_non_terminal_events() {
        let writer = RecordingWriter::default();
        let mut renderer = JsonRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let agent = AgentId::new();

        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::TextDelta { text: "hi".into() },
            ))
            .unwrap();
        renderer
            .on_event(&envelope(
                session,
                agent,
                Event::AgentProgress {
                    note: "working".into(),
                },
            ))
            .unwrap();

        assert_eq!(writer.contents(), b"");
    }

    #[test]
    fn finish_writes_exactly_one_json_object() {
        let writer = RecordingWriter::default();
        let mut renderer = JsonRenderer::new(Box::new(writer.clone()));
        let agent = AgentId::new();
        let session = SessionId::new();
        let result = AgentResult::new(agent, session, ResultStatus::Completed, "done");

        renderer.finish(Some(&result)).unwrap();

        let contents = writer.contents();
        let text = String::from_utf8(contents).unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert!(value.is_object());
        assert_eq!(value["status"]["status"], "completed");
    }

    #[test]
    fn finish_with_no_result_writes_nothing() {
        let writer = RecordingWriter::default();
        let mut renderer = JsonRenderer::new(Box::new(writer.clone()));
        renderer.finish(None).unwrap();
        assert_eq!(writer.contents(), b"");
    }
}
