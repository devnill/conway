//! `--output-format jsonl`: one JSON line per `Envelope`, uniformly and
//! unconditionally -- no event-kind filtering, unlike `text`/`json`. This is
//! the machine-consumable mirror of the whole stream: everything `text`
//! suppresses or redirects to stderr (thinking deltas, tool-call activity,
//! permission resolutions, routing decisions) is a line here.
//!
//! See `docs/scripting.md`'s `jsonl` section for the `seq`/multi-agent
//! contract a consumer of this stream can actually rely on (per-session
//! monotonic, not global; a subagent appears only as a sparse lifecycle
//! slice; the stream ends at the ROOT agent's `agent_finished`, not the
//! first one).

use std::io::{self, Write};

use conway::{AgentResult, Envelope};

use super::Renderer;

pub struct JsonlRenderer {
    out: Box<dyn Write + Send>,
}

impl JsonlRenderer {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self { out }
    }
}

impl Renderer for JsonlRenderer {
    fn on_event(&mut self, env: &Envelope) -> io::Result<()> {
        let json = serde_json::to_string(env)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.out.write_all(json.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.out.flush()
    }

    fn finish(&mut self, _result: Option<&AgentResult>) -> io::Result<()> {
        // Every envelope up to and including the terminal `AgentFinished`
        // was already emitted by `on_event` as the render loop streamed it
        // -- `finish` here has nothing left to write (unlike `json`, which
        // has been withholding output specifically until now).
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use conway::{AgentId, Event, SessionId};

    use super::*;
    use crate::render::test_support::RecordingWriter;

    #[test]
    fn one_parseable_line_per_envelope_with_required_keys_and_no_esc_byte() {
        let writer = RecordingWriter::default();
        let mut renderer = JsonlRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let agent = AgentId::new();

        let events = vec![
            Event::TextDelta { text: "hi".into() },
            Event::ThinkingDelta { text: "hmm".into() },
            Event::AgentProgress {
                note: "working".into(),
            },
        ];
        for (i, event) in events.into_iter().enumerate() {
            renderer
                .on_event(&Envelope {
                    seq: i as u64,
                    ts: Utc::now(),
                    session,
                    agent,
                    event,
                })
                .unwrap();
        }

        let contents = writer.contents();
        assert!(
            !contents.contains(&0x1bu8),
            "no ESC byte anywhere in jsonl output"
        );
        let text = String::from_utf8(contents).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["seq"], i);
            assert!(value.get("ts").is_some());
            assert!(value.get("session").is_some());
            assert!(value.get("agent").is_some());
            assert!(value.get("event").is_some());
        }
    }

    #[test]
    fn finish_writes_no_additional_line() {
        let writer = RecordingWriter::default();
        let mut renderer = JsonlRenderer::new(Box::new(writer.clone()));
        let session = SessionId::new();
        let agent = AgentId::new();
        renderer
            .on_event(&Envelope {
                seq: 0,
                ts: Utc::now(),
                session,
                agent,
                event: Event::TextDelta { text: "hi".into() },
            })
            .unwrap();
        let before = writer.contents();

        renderer.finish(None).unwrap();

        assert_eq!(writer.contents(), before);
    }
}
