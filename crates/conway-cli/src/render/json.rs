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
    /// Board item `01M1YVRS0K284H9QB32ZZW6D5G`: set via [`Renderer::set_cost`]
    /// before [`Renderer::finish`]; `None` when no price was configured for
    /// the model that served this run.
    cost: Option<conway::Cost>,
}

impl JsonRenderer {
    pub fn new(out: Box<dyn Write + Send>) -> Self {
        Self { out, cost: None }
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
            // Board item `01M3XGPGT5W7GABVTC7F2NA0C9`: `TurnAbortedByUser`
            // (the operator-abort sibling of `TurnAborted`, which this
            // renderer already drops into the wildcard below) is deliberately
            // left there too, for the identical reason `TurnAborted` is --
            // this renderer's whole promise is silence on stdout until
            // `finish`, and a one-shot `conway -p` run is never `keep_alive`
            // in practice, so there is no real caller of `abort_turn` that
            // ever reaches this renderer.
            //
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
        let mut json = serde_json::to_string(result)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Board item `01M1YVRS0K284H9QB32ZZW6D5G`: `cost` is appended into the
        // `usage` object's own JSON text, by string splice, rather than via
        // a round trip through `serde_json::Value` -- this crate links no
        // `preserve_order` feature, so `Value`'s `Map` is key-sorted, and a
        // round trip would silently re-order every OTHER field of this
        // object alphabetically too (`docs/scripting.md`'s own worked
        // example documents a specific, non-alphabetical field order).
        // Splicing the typed `usage` serialization's own exact text (found
        // verbatim inside `json`, since both came from the identical
        // serializer a moment apart) changes nothing else in the document.
        // Neither `conway_core::agent::AgentResult` nor `Usage` itself gains
        // a `cost` field: neither type has any notion of price, and giving
        // `Usage` one would mean every OTHER reader of it (the TUI, the
        // session log codec) carries a field only this renderer ever
        // populates.
        if let Some(cost) = self.cost {
            let usage_json = serde_json::to_string(&result.usage)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if let Some(pos) = json.find(&usage_json) {
                let cost_json = serde_json::json!({
                    "amount": cost.amount,
                    "currency": conway::SUPPORTED_CURRENCY,
                    "approximate": cost.approximate,
                });
                let spliced = format!(
                    "{},\"cost\":{cost_json}}}",
                    &usage_json[..usage_json.len() - 1]
                );
                json.replace_range(pos..pos + usage_json.len(), &spliced);
            }
        }
        self.out.write_all(json.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.out.flush()
    }

    fn set_cost(&mut self, cost: Option<conway::Cost>) {
        self.cost = cost;
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

    // ---- `cost` in `usage` (board item `01M1YVRS0K284H9QB32ZZW6D5G`) ----

    #[test]
    fn finish_includes_cost_in_usage_when_set_cost_was_called() {
        let writer = RecordingWriter::default();
        let mut renderer = JsonRenderer::new(Box::new(writer.clone()));
        let agent = AgentId::new();
        let session = SessionId::new();
        let mut result = AgentResult::new(agent, session, ResultStatus::Completed, "done");
        result.usage = conway::Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Default::default()
        };

        renderer.set_cost(Some(conway::Cost {
            amount: 3.0,
            approximate: false,
        }));
        renderer.finish(Some(&result)).unwrap();

        let contents = writer.contents();
        let text = String::from_utf8(contents).unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["usage"]["cost"]["amount"], 3.0);
        assert_eq!(value["usage"]["cost"]["currency"], "USD");
        assert_eq!(value["usage"]["cost"]["approximate"], false);
        // Every pre-existing `usage` field must still be there, unchanged.
        assert_eq!(value["usage"]["input_tokens"], 1_000_000);
    }

    /// No `set_cost` call at all (the default one-shot path, or a run with
    /// no price configured) -- `usage` carries no `cost` field, never a
    /// placeholder (GP-14).
    #[test]
    fn finish_omits_cost_when_set_cost_was_never_called() {
        let writer = RecordingWriter::default();
        let mut renderer = JsonRenderer::new(Box::new(writer.clone()));
        let agent = AgentId::new();
        let session = SessionId::new();
        let result = AgentResult::new(agent, session, ResultStatus::Completed, "done");

        renderer.finish(Some(&result)).unwrap();

        let contents = writer.contents();
        let text = String::from_utf8(contents).unwrap();
        let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert!(value["usage"].get("cost").is_none());
    }
}
