//! T4's turn-end summary line (`1m 6s * 1.4k tok (88% cached)`): the
//! [`AppState::stamp_turn_summary`] method that attaches it to the last
//! Assistant/Reasoning block a turn produced, and the pure formatting
//! helpers ([`compact_tokens`], [`format_turn_summary`]) it builds the text
//! from.

use super::*;

impl AppState {
    /// T4: stamp the turn-end summary (`1m 6s · 1.4k tok (88% cached)`)
    /// onto the last `Entry::Assistant` or `Entry::Reasoning` block in the
    /// transcript. Called from the `TurnFinished` arm BEFORE
    /// [`clear_turn_state`] zeroes `turn_started_at` (the elapsed figure
    /// reads `turn_started_at.elapsed()`). A no-op if THIS TURN produced no
    /// Assistant/Reasoning block to attach to (e.g. a turn that produced
    /// only tool calls)
    /// -- the summary is genuinely about a model-emitted block, so attaching
    /// it to a bare tool entry would be misleading; the status line's own
    /// token figures still convey the spend. Stamps onto Reasoning if it is
    /// the last block (the trace is what the user sees last in that case),
    /// else onto the last Assistant block.
    ///
    /// The scan is bounded below by [`Self::turn_transcript_start`], the
    /// transcript length at `TurnStarted`. An UNBOUNDED scan would walk a
    /// tool-only turn's own entries and then keep going into the PREVIOUS
    /// turn, overwriting an already-settled bubble's summary with this
    /// turn's elapsed/token figures -- misattributing spend to an unrelated
    /// reply.
    pub(super) fn stamp_turn_summary(&mut self, usage: &Usage) {
        let elapsed_secs = self
            .turn_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let summary = format_turn_summary(elapsed_secs, usage, self.focused_model.as_deref());
        // Clamp defensively: a `TurnFinished` with no preceding `TurnStarted`
        // (or a transcript cleared mid-turn) must never index out of range.
        let start = self.turn_transcript_start.min(self.transcript.len());
        for entry in self.transcript[start..].iter_mut().rev() {
            match entry {
                Entry::Reasoning { summary: s, .. } | Entry::Assistant { summary: s, .. } => {
                    *s = Some(summary);
                    return;
                }
                _ => {}
            }
        }
    }
}

/// T4: compact token-count formatting for the turn-end summary. `< 1000`
/// renders as-is; `>= 1000` renders as `{k}.{tenths}k` (e.g. `12345` ->
/// `12.3k`). Mirrors [`crate::tui::view::status::compact_tokens`] (which is
/// private to the status module); duplicated here rather than made `pub` to
/// keep the status module's helpers private to the status line's own
/// rendering surface, matching the existing module boundaries.
fn compact_tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = n / 1000;
    let tenths = (n % 1000) / 100;
    format!("{k}.{tenths}k")
}

/// T4: format the turn-end summary line (`1m 6s · 1.4k tok (88% cached)`,
/// or `1m 6s · 1.4k tok (cache: not reported by ollama)` when the backend's
/// wire format carries no cache field at all) from the elapsed seconds
/// (read from `turn_started_at` before `clear_turn_state` zeroes it), the
/// turn's `Usage`, and the focused agent's `"backend/model"` string (for
/// naming the backend in the not-reported form; `None` renders the generic
/// `cache: not reported`). Elapsed is `1m 6s` for >= 60s, else `{secs}s`.
/// Tokens is the sum of every `Usage` field (matching
/// [`crate::tui::view::status::spent_tokens`]); the cache suffix itself is
/// [`crate::tui::usage_format::cache_suffix`] -- see its own doc for the
/// `Reported`/`NotReported` rule this shares with the status line's
/// `tokens` field.
fn format_turn_summary(elapsed_secs: u64, usage: &Usage, focused_model: Option<&str>) -> String {
    let elapsed = if elapsed_secs >= 60 {
        let m = elapsed_secs / 60;
        let s = elapsed_secs % 60;
        format!("{m}m {s}s")
    } else {
        format!("{elapsed_secs}s")
    };
    let total = u64::from(usage.input_tokens)
        + u64::from(usage.output_tokens)
        + u64::from(usage.cache_read_tokens)
        + u64::from(usage.cache_write_tokens)
        + u64::from(usage.reasoning_tokens);
    let suffix = crate::tui::usage_format::cache_suffix(usage, focused_model);
    format!("{elapsed} · {} tok{suffix}", compact_tokens(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::fixtures::envelope;
    use conway::{CacheAccounting, SessionId, ToolName};

    /// `TurnFinished` stamps a turn-end summary onto the last
    /// `Entry::Assistant` (or `Entry::Reasoning` if that was the last
    /// block). The summary reads `turn_started_at` BEFORE
    /// `clear_turn_state` zeroes it.
    #[test]
    fn turn_finished_stamps_summary_on_last_assistant() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.apply(&envelope(
            session,
            root,
            Event::TextDelta {
                text: "hello".to_string(),
            },
        ));
        state.turn_started_at = Some(Instant::now() - std::time::Duration::from_secs(66));

        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 300,
                    cache_read_tokens: 800,
                    cache_write_tokens: 100,
                    reasoning_tokens: 0,
                    ..Usage::default()
                },
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        match state.transcript.last() {
            Some(Entry::Assistant { summary, .. }) => {
                let s = summary.as_ref().expect("summary stamped");
                assert!(s.contains("1m 6s"), "elapsed in m/s form: {s}");
                assert!(s.contains("tok"), "token count present: {s}");
                assert!(s.contains("% cached"), "cache pct present: {s}");
            }
            other => panic!("expected an Assistant entry, got {other:?}"),
        }
    }

    /// When the last block is `Entry::Reasoning`, the summary attaches to
    /// IT instead (the trace is what the user sees last in that case).
    #[test]
    fn turn_finished_stamps_summary_on_last_reasoning_when_last() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.apply(&envelope(
            session,
            root,
            Event::ThinkingDelta {
                text: "pondering".to_string(),
            },
        ));
        state.turn_started_at = Some(Instant::now() - std::time::Duration::from_secs(5));

        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    ..Usage::default()
                },
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        match state.transcript.last() {
            Some(Entry::Reasoning { summary, .. }) => {
                let s = summary.as_ref().expect("summary stamped");
                assert!(s.contains("5s"), "elapsed in seconds form: {s}");
                // Board item: a provider that REPORTS a genuine zero cache
                // hit rate must still show `0% cached` -- omitting it here
                // (the pre-existing behavior this assertion inverts) made
                // "zero percent cached" indistinguishable from "this
                // backend doesn't report caching at all". `Usage::default`
                // (via `..Usage::default()` above) is `CacheAccounting`'s
                // own default, `Reported`.
                assert!(
                    s.contains("0% cached"),
                    "a Reported zero cache-read rate must still render 0%: {s}"
                );
            }
            other => panic!("expected a Reasoning entry, got {other:?}"),
        }
    }

    /// A turn with no assistant/reasoning block (only tool calls) gets no
    /// summary -- the summary is genuinely about a model-emitted block.
    #[test]
    fn turn_finished_with_no_assistant_or_reasoning_attaches_no_summary() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({}),
            },
        ));
        state.turn_started_at = Some(Instant::now());

        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage::default(),
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        match state.transcript.last() {
            Some(Entry::Tool { .. }) => {}
            other => panic!("expected a Tool entry unchanged, got {other:?}"),
        }
    }

    /// T4 review regression (significant): a tool-only turn must NOT reach
    /// back past its own entries and re-stamp the PREVIOUS turn's settled
    /// assistant bubble with this turn's elapsed/token figures.
    ///
    /// The companion test above only covers a transcript with no prior
    /// Assistant entry at all, so it cannot catch the walk-into-the-previous
    /// -turn path. Here turn 1 produces a real reply that gets a correct
    /// summary; turn 2 is a tool-only agentic round. Turn 2's `TurnFinished`
    /// must be a no-op, leaving turn 1's summary exactly as it was.
    #[test]
    fn tool_only_turn_does_not_restamp_the_previous_turns_summary() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        // --- Turn 1: a real model reply, correctly summarized. ---
        state.apply(&envelope(session, root, Event::TurnStarted { turn: 1 }));
        state.apply(&envelope(
            session,
            root,
            Event::TextDelta {
                text: "hi".to_string(),
            },
        ));
        state.turn_started_at = Some(Instant::now());
        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    ..Usage::default()
                },
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        let turn_one_summary = state
            .transcript
            .iter()
            .find_map(|e| match e {
                Entry::Assistant { summary, .. } => summary.clone(),
                _ => None,
            })
            .expect("turn 1 must stamp a summary on its assistant block");

        // --- Turn 2: a tool-only round (no model text of its own). ---
        state.apply(&envelope(session, root, Event::TurnStarted { turn: 2 }));
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({}),
            },
        ));
        state.turn_started_at = Some(Instant::now());
        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage {
                    input_tokens: 9_000,
                    output_tokens: 900,
                    ..Usage::default()
                },
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        let after = state
            .transcript
            .iter()
            .find_map(|e| match e {
                Entry::Assistant { summary, .. } => summary.clone(),
                _ => None,
            })
            .expect("turn 1's assistant block must still exist");

        assert_eq!(
            after, turn_one_summary,
            "a tool-only turn must not overwrite the previous turn's summary \
             (turn 2's 9.9k-token figures leaked onto turn 1's 120-token reply)"
        );
    }

    /// `format_turn_summary` formats elapsed >= 60s as `1m 6s` and < 60s
    /// as `{n}s`; `Reported` always renders a cache pct (including `0%`),
    /// `NotReported` renders `cache: not reported[ by <backend>]` instead.
    #[test]
    fn format_turn_summary_shapes() {
        let with_cache = Usage {
            input_tokens: 100,
            output_tokens: 400,
            cache_read_tokens: 800,
            cache_write_tokens: 100,
            reasoning_tokens: 0,
            ..Usage::default()
        };
        // 800 / (100+800+100) = 80%.
        assert_eq!(
            format_turn_summary(66, &with_cache, None),
            "1m 6s · 1.4k tok (80% cached)"
        );
        assert_eq!(
            format_turn_summary(5, &with_cache, None),
            "5s · 1.4k tok (80% cached)"
        );

        let zero_cache_reported = Usage {
            input_tokens: 100,
            output_tokens: 400,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            ..Usage::default()
        };
        assert_eq!(
            format_turn_summary(5, &zero_cache_reported, None),
            "5s · 500 tok (0% cached)"
        );

        let not_reported = Usage {
            input_tokens: 100,
            output_tokens: 400,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cache_accounting: CacheAccounting::NotReported,
        };
        assert_eq!(
            format_turn_summary(5, &not_reported, None),
            "5s · 500 tok (cache: not reported)"
        );
        assert_eq!(
            format_turn_summary(5, &not_reported, Some("ollama/gemma4:e4b")),
            "5s · 500 tok (cache: not reported by ollama)"
        );
    }

    /// `compact_tokens` mirrors the status line's helper: `<1000` as-is,
    /// `>=1000` as `{k}.{tenths}k`.
    #[test]
    fn compact_tokens_formats() {
        assert_eq!(compact_tokens(0), "0");
        assert_eq!(compact_tokens(999), "999");
        assert_eq!(compact_tokens(1000), "1.0k");
        assert_eq!(compact_tokens(12345), "12.3k");
        assert_eq!(compact_tokens(1400), "1.4k");
    }
}
