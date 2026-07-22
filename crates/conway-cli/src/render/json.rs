//! `--output-format json`: stdout carries nothing at all until the run
//! finishes, then exactly one JSON object -- the terminal `AgentResult` --
//! and nothing else. Every streaming envelope is silently dropped; this
//! mode trades incremental output for "one document in, one document out"
//! scriptability.

use std::io::{self, Write};

use conway::{AgentResult, Envelope};

use super::Renderer;

pub struct JsonRenderer {
    out: Box<dyn Write + Send>,
}

impl JsonRenderer {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self { out }
    }
}

impl Renderer for JsonRenderer {
    fn on_event(&mut self, _env: &Envelope) -> io::Result<()> {
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
