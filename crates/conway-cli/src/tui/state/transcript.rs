//! The transcript's own entry model ([`Entry`], [`ToolStatus`]) and the
//! [`AppState`] methods that build/mutate it from applied events: assistant
//! and reasoning-trace deltas, tool-call lifecycle (proposed -> running ->
//! finished, plus streamed progress notes), the T4 "show reasoning" /
//! "show timestamps" toggles, and the T5 tool-preview expand/collapse +
//! line-count cap. Turn-end summary stamping lives in
//! [`super::turn_summary`]; transcript-pane scrolling lives in
//! [`super::scroll`] -- both act on the same [`AppState::transcript`] this
//! module owns the entries of, but are their own seams.

use super::*;

/// One line of the transcript pane.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
    /// Assistant reply text. T4 adds three provenance fields:
    /// - `model` is the serving model's display name (e.g.
    ///   `anthropic/claude-sonnet-4-6`), stamped from
    ///   [`AppState::focused_model`] at the time the entry is created by
    ///   `TextDelta` -> `AppState::append_assistant_text`. `None` for
    ///   replayed entries (`record_to_event` maps a stored `Assistant` record
    ///   to a bare `TextDelta` carrying no model -- see that function's own
    ///   doc); the renderer then omits the `[modelname]> ` marker so a
    ///   replayed bubble renders as it originally streamed.
    /// - `summary` is the turn-end summary line (`1m 6s · 1.4k tok (88%
    ///   cached)`), stamped onto the last assistant/reasoning entry by
    ///   `TurnFinished` -> `AppState::stamp_turn_summary`. `None` until
    ///   the turn ends (and stays `None` if no assistant/reasoning block
    ///   exists to attach to).
    /// - `ts` is the per-entry timestamp, stamped from the envelope's `ts`
    ///   at apply time. The `/settings` menu's "show timestamps" toggle
    ///   (V4; formerly the standalone `/timestamps` command) prepends
    ///   `HH:MM ` to the entry's first rendered line.
    Assistant {
        text: String,
        model: Option<String>,
        summary: Option<String>,
        ts: Option<DateTime<Utc>>,
    },
    /// T4: reasoning-trace text, fed by `Event::ThinkingDelta` (previously
    /// dropped by `apply`'s wildcard arm -- only `activity` was flipped to
    /// `Thinking`). Mirrors [`Entry::Assistant`]: `ThinkingDelta` ->
    /// `AppState::append_reasoning_text` creates-or-appends, stamping the
    /// current serving model + envelope timestamp onto a freshly-created
    /// entry. Rendered dim+italic with a `thinking` prefix, EXPANDED by
    /// default (the `show_reasoning` flag -- toggled from the `/settings`
    /// menu, V4; formerly the standalone `/thinking` command -- defaults
    /// `true`, so reasoning is visible until the user hides it;
    /// when hidden, `build_lines` skips `Entry::Reasoning` entirely). The
    /// `summary` field is shared with `Entry::Assistant`: a turn-end
    /// summary attaches to whichever of the two was the LAST block under
    /// the turn.
    Reasoning {
        text: String,
        model: Option<String>,
        summary: Option<String>,
        ts: Option<DateTime<Utc>>,
    },
    Tool {
        call_id: String,
        name: String,
        status: ToolStatus,
        preview: String,
        /// T4: the tool call's arguments, stored from
        /// `Event::ToolCallProposed { args, .. }` (previously discarded --
        /// only `name` was stored). Serialized to a compact JSON string at
        /// apply time. Rendered as a one-line truncated `args: …` preview
        /// while collapsed and pretty-printed (multi-line) while expanded.
        /// Reuses the `expanded` flag + Ctrl-E toggle below -- args and
        /// output expand/collapse together (the single flag governs both).
        args: String,
        /// T4: accumulated `Event::ToolProgress { call_id, note }` notes
        /// (previously dropped by `apply`'s wildcard arm), appended to the
        /// matching in-flight tool entry by `call_id`. Joined with `\n` and
        /// rendered as dim `-> {note}` lines between the args line and the
        /// output block.
        progress: String,
        /// T5: whether this tool entry's preview is shown in full (`true`)
        /// or collapsed to the `tool_preview_lines` cap + a dim affordance
        /// (`false`, the default). Flipped on EVERY `Entry::Tool` at once by
        /// [`AppState::toggle_all_tool_entries_expanded`] (the `Ctrl-E`
        /// keybinding). The flag is kept on the entry itself -- not derived
        /// from a single global toggle -- so a future per-entry selective
        /// expand (T4's tool-args reuse, or a transcript-cursor selection)
        /// can flip individual entries without touching the rest. The render
        /// branch in `view/transcript.rs::tool_lines` reads this plus the
        /// stored `preview` (which is NEVER truncated -- the cap is
        /// render-time only) and emits either the first N lines + a `… (+M
        /// lines, Ctrl-E to expand)` affordance or the full content. T4
        /// reuses the same `expanded` flag + render branch for tool-args
        /// previews: a one-line-truncated args preview is the same shape
        /// (collapsed: cap lines + affordance; expanded: full), just with a
        /// different cap and content.
        expanded: bool,
        /// T4: per-entry timestamp, stamped from the envelope's `ts` at
        /// apply time. The `/settings` menu's "show timestamps" toggle (V4;
        /// formerly the standalone `/timestamps` command) prepends
        /// `HH:MM ` to the entry's first rendered line.
        ts: Option<DateTime<Utc>>,
    },
    /// A subagent's lifecycle, rendered inline in the conversation stream
    /// (criterion: "inline subagent activity in the stream,
    /// Claude-Code-style") instead of only being reflected in the
    /// below-chat `/agents` panel. Pushed once at spawn time
    /// (`apply_agent_spawned`) and updated in place at finish time
    /// (`apply_agent_finished`) -- never a second entry for the same agent.
    Agent {
        agent_id: AgentId,
        label: String,
        status: NodeStatus,
    },
    Notice {
        text: String,
    },
    /// A runtime error surfaced via `Event::Error`. Kept as its OWN variant rather than a
    /// field bolted onto [`Entry::Notice`]: a field would still let severity
    /// leak into an existing cyan-styled call site by accident, and (more
    /// concretely) a recon on this item found a field/constructor approach
    /// touches every one of `Entry::Notice`'s ~50 construction sites while a
    /// separate variant touches exactly three (this apply arm,
    /// `view/transcript.rs::entry_lines`, and the variant-enumerating
    /// clean-copy test). `fatal: true` renders in `theme.fatal_error`
    /// (Red+Bold) -- conway's loudest possible message, previously
    /// indistinguishable from a routine cyan notice save for the word
    /// "fatal" inside the string. `fatal: false` is a real, non-recoverable-
    /// looking-but-actually-recoverable error too, so it does not fall back
    /// to `theme.notice` either: it renders in `theme.error` (plain Red, the
    /// same slot the `/ask` modal's failed-fate line already uses), one step
    /// down from `fatal_error`'s bold. Red still means failure at both
    /// severities; only the loudest one gets the bold escalation. See
    /// `entry_lines`'s `Entry::Error` arm for the one place this severity
    /// decision is made.
    Error {
        text: String,
        fatal: bool,
    },
}

/// A tool call's lifecycle, as reflected in one [`Entry::Tool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Proposed,
    AwaitingPermission,
    Running,
    Finished { is_error: bool },
}

impl AppState {
    /// T5: flips `expanded` on EVERY `Entry::Tool` in the transcript at once
    /// (the `Ctrl-E` keybinding). MVP is all-at-once -- there is no
    /// transcript-cursor/selection state, so "expand/collapse all" is the
    /// only meaningful toggle. Pure state mutation: does NOT touch
    /// `scroll`/`follow_tail`/`max_scroll` -- the next render's existing
    /// clamp in `view/transcript.rs::draw` (`state.scroll.min(max)`)
    /// re-clamps to the nearest valid position without snapping the
    /// viewport (a toggle that shrinks the content height clamps an
    /// overscrolled `scroll` down to the new `max`; a toggle that grows it
    /// back restores the original `scroll` since it was never overwritten).
    /// Factored as a method (not inlined in `input.rs`) so the all-at-once
    /// behavior + the no-snap contract are directly unit-testable with no
    /// terminal/key event at all.
    pub fn toggle_all_tool_entries_expanded(&mut self) {
        for entry in self.transcript.iter_mut() {
            if let Entry::Tool { expanded, .. } = entry {
                *expanded = !*expanded;
            }
        }
    }

    pub(super) fn append_assistant_text(&mut self, delta: &str, ts: DateTime<Utc>) {
        if let Some(Entry::Assistant { text, .. }) = self.transcript.last_mut() {
            text.push_str(delta);
        } else {
            self.transcript.push(Entry::Assistant {
                text: delta.to_string(),
                // T4: stamp the serving model from the live focus. Replay
                // (`record_to_event` maps a stored `Assistant` record to a
                // bare `TextDelta` carrying no model) leaves `focused_model`
                // as whatever the live focus happens to be -- but a replay
                // envelope is only ever applied on the focused agent's
                // stream, and the renderer omits the marker when `None`,
                // which is the backward-compatible shape for a replayed
                // bubble that has no model provenance.
                model: self.focused_model.clone(),
                summary: None,
                ts: Some(ts),
            });
        }
    }

    /// T4: append a reasoning-trace delta (from `Event::ThinkingDelta`),
    /// mirroring [`append_assistant_text`]. Creates a new
    /// [`Entry::Reasoning`] on the first delta of a run (stamping the
    /// serving model + envelope timestamp), or appends to the last
    /// `Reasoning` entry if one is already in progress. Reasoning is
    /// EXPANDED by default (the `show_reasoning` flag defaults `true`);
    /// `build_lines` skips `Entry::Reasoning` entirely when the flag is
    /// `false`, but the entries are still STORED, so toggling back on
    /// restores them without replay.
    pub(super) fn append_reasoning_text(&mut self, delta: &str, ts: DateTime<Utc>) {
        if let Some(Entry::Reasoning { text, .. }) = self.transcript.last_mut() {
            text.push_str(delta);
        } else {
            self.transcript.push(Entry::Reasoning {
                text: delta.to_string(),
                model: self.focused_model.clone(),
                summary: None,
                ts: Some(ts),
            });
        }
    }

    /// T4: append a `ToolProgress { call_id, note }` note to the matching
    /// in-flight [`Entry::Tool`] by `call_id` (previously dropped by the
    /// wildcard arm). Joined with `\n` -- the renderer emits each as a dim
    /// `-> {note}` line. A no-op if no tool entry with that `call_id` exists
    /// (never panics on untrusted input).
    pub(super) fn append_tool_progress(&mut self, call_id: &str, note: &str) {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool {
                call_id: id,
                progress,
                ..
            } = entry
            {
                if id == call_id {
                    if !progress.is_empty() {
                        progress.push('\n');
                    }
                    progress.push_str(note);
                    return;
                }
            }
        }
    }

    /// T4: toggle the `show_reasoning` flag. V4: the one caller of this is
    /// now the `/settings` menu's `Enter` key on the "show reasoning traces"
    /// leaf (`input::handle_settings_key`) -- the standalone `/thinking`
    /// slash command this originally backed is REMOVED, not aliased (see
    /// `commands.rs`'s module doc), but the toggle itself is unchanged: same
    /// field, same flip, same return value.
    pub fn toggle_thinking(&mut self) -> bool {
        self.show_reasoning = !self.show_reasoning;
        self.show_reasoning
    }

    /// T4: toggle the `show_timestamps` flag. V4: now called from the
    /// `/settings` menu's `Enter` key on the "show timestamps" leaf, exactly
    /// as [`Self::toggle_thinking`]'s doc describes for its own removed
    /// `/thinking` command -- the standalone `/timestamps` command is
    /// REMOVED, the toggle is not.
    pub fn toggle_timestamps(&mut self) -> bool {
        self.show_timestamps = !self.show_timestamps;
        self.show_timestamps
    }

    /// V4: adjusts `tool_preview_lines` by `delta` -- the `/settings` menu's
    /// Left(`-1`)/Right(`+1`) numeric stepper for the one non-boolean
    /// setting. Floors/caps at `TOOL_PREVIEW_LINES_RANGE`'s own bounds
    /// rather than routing the stepped value through
    /// [`clamp_tool_preview_lines`] directly: that function's job is
    /// validating an untrusted CONFIG value, where out-of-range means
    /// "malformed, fall back to the built-in default (3)" -- applying that
    /// same fallback to an interactive stepper would make pressing Left at
    /// the floor (1) bounce UP to 3 instead of simply stopping, which reads
    /// as broken, not as a safety net. Both functions still share the ONE
    /// range constant (no independently-typed-in second bounds check
    /// that could silently drift from it) -- only the OUT-OF-RANGE behavior
    /// differs, matched to what each caller actually needs. Never panics on
    /// any `delta` (`saturating_add` on a widened `i64` before the final
    /// clamp). Returns the new value.
    pub fn adjust_tool_preview_lines(&mut self, delta: i32) -> u32 {
        let stepped = i64::from(self.tool_preview_lines).saturating_add(i64::from(delta));
        let floor = i64::from(*TOOL_PREVIEW_LINES_RANGE.start());
        let ceil = i64::from(*TOOL_PREVIEW_LINES_RANGE.end());
        self.tool_preview_lines = stepped.clamp(floor, ceil) as u32;
        self.tool_preview_lines
    }

    pub(super) fn set_tool_status(&mut self, call_id: &str, status: ToolStatus) {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool {
                call_id: id,
                status: s,
                ..
            } = entry
            {
                if id == call_id {
                    *s = status;
                    return;
                }
            }
        }
    }

    pub(super) fn finish_tool(&mut self, call_id: &str, is_error: bool, preview: String) {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool {
                call_id: id,
                status,
                preview: p,
                ..
            } = entry
            {
                if id == call_id {
                    *status = ToolStatus::Finished { is_error };
                    *p = preview;
                    return;
                }
            }
        }
    }
}

/// T5's valid range for `tool_preview_lines` (`1..=200`), factored out as a
/// named constant (V4) so [`clamp_tool_preview_lines`] (config validation,
/// which falls back to the built-in default on ANY out-of-range value) and
/// [`AppState::adjust_tool_preview_lines`] (the `/settings` menu's
/// interactive stepper, which floors/caps at the boundary instead) share
/// ONE source of truth for the bound -- no second, independently
/// typed-in bounds check that could silently drift from this one. The
/// `1..=200` range itself keeps the cap meaningful (a cap of 0 would
/// collapse every preview to zero content lines + the affordance; a cap of
/// `u32::MAX` would effectively disable folding, defeating T5's purpose).
const TOOL_PREVIEW_LINES_RANGE: std::ops::RangeInclusive<u32> = 1..=200;

/// T5: clamps a loaded `[tui.tool_preview_lines]` config value into a safe
/// render-time cap. `None` (the serde default for the `Option<u32>` field)
/// -> the built-in default of 3. A value in `TOOL_PREVIEW_LINES_RANGE` is
/// kept as-is. Any other value (0, > 200, or a value that failed to parse
/// as `u32` and so arrived as `None`) falls back to the default of 3.
/// Config is untrusted input -- this function never panics, and there is no
/// `unwrap`/`expect`/indexing on the config value (the `?`-shaped
/// `and_then` + `unwrap_or` chain is the entire bound on `n`).
pub fn clamp_tool_preview_lines(n: Option<u32>) -> u32 {
    n.and_then(|v| {
        if TOOL_PREVIEW_LINES_RANGE.contains(&v) {
            Some(v)
        } else {
            None
        }
    })
    .unwrap_or(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::fixtures::{envelope, spawned};
    use conway::{PermissionDecisionKind, SessionId, ToolName};

    /// `Event::Error { fatal: true }` pushes a dedicated `Entry::Error`
    /// (never `Entry::Notice`), carrying `fatal: true` and the `"fatal "`
    /// text prefix through to the entry -- the prefix is kept even though
    /// severity is now structural, because a clean-copied transcript carries
    /// no style at all, so the word is the only surviving trace.
    #[test]
    fn fatal_error_event_pushes_dedicated_error_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::Error {
                error: conway_core::error::ConwayError::Config {
                    detail: "boom".to_string(),
                },
                fatal: true,
            },
        ));

        assert_eq!(state.transcript.len(), 1);
        match &state.transcript[0] {
            Entry::Error { text, fatal } => {
                assert!(*fatal);
                assert!(
                    text.starts_with("fatal error:"),
                    "expected the 'fatal ' prefix to survive into the entry text: {text:?}"
                );
            }
            other => panic!("expected Entry::Error, got {other:?}"),
        }
    }

    /// `Event::Error { fatal: false }` is a real, recoverable error -- it
    /// also gets `Entry::Error`, not `Entry::Notice`, just with `fatal:
    /// false` and no `"fatal "` prefix in the text.
    #[test]
    fn non_fatal_error_event_pushes_dedicated_error_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::Error {
                error: conway_core::error::ConwayError::Config {
                    detail: "retrying".to_string(),
                },
                fatal: false,
            },
        ));

        assert_eq!(state.transcript.len(), 1);
        match &state.transcript[0] {
            Entry::Error { text, fatal } => {
                assert!(!*fatal);
                assert!(
                    text.starts_with("error:") && !text.starts_with("fatal error:"),
                    "non-fatal must not carry the 'fatal ' prefix: {text:?}"
                );
            }
            other => panic!("expected Entry::Error, got {other:?}"),
        }
    }

    /// The exact event sequence from this item's own criterion: one
    /// coalesced "ab" assistant message, one completed tool-call entry, and
    /// a tree with the one (root) node in `Finished` state.
    #[test]
    fn full_turn_sequence_yields_coalesced_text_completed_tool_and_finished_tree() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        let events = vec![
            spawned(None),
            Event::TextDelta {
                text: "a".to_string(),
            },
            Event::TextDelta {
                text: "b".to_string(),
            },
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls"}),
            },
            Event::PermissionRequested {
                call_id: "tc_1".to_string(),
                rendered: "bash: ls".to_string(),
            },
            Event::PermissionResolved {
                call_id: "tc_1".to_string(),
                decision: PermissionDecisionKind::AllowOnce,
            },
            Event::ToolCallFinished {
                call_id: "tc_1".to_string(),
                is_error: false,
                preview: "ok".to_string(),
            },
            Event::AgentFinished {
                result: AgentResult::new(root, session, ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ];
        for event in events {
            state.apply(&envelope(session, root, event));
        }

        let assistant_texts: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            assistant_texts,
            vec!["ab"],
            "TextDeltas must coalesce into one Assistant entry"
        );

        let completed_tools = state
            .transcript
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Entry::Tool {
                        status: ToolStatus::Finished { is_error: false },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            completed_tools, 1,
            "expected exactly one completed tool-call entry"
        );

        assert_eq!(state.tree.nodes.len(), 1, "expected exactly one tree node");
        assert_eq!(state.tree.nodes[0].agent_id, root);
        assert_eq!(state.tree.nodes[0].status, NodeStatus::Finished);
    }

    #[test]
    fn lagged_appends_a_visible_notice_without_panicking() {
        let mut state = AppState::new(AgentId::new());
        let before = state.transcript.len();

        state.apply(&envelope(
            SessionId::new(),
            AgentId::new(),
            Event::Lagged { skipped: 7 },
        ));

        assert_eq!(state.transcript.len(), before + 1);
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { .. })
        ));
    }

    #[test]
    fn agent_progress_pushes_a_visible_notice() {
        // A genuine free-text `AgentProgress` (e.g. a `SystemNote`/
        // `ContextReportRecord` replay, or a live runtime-authored note) --
        // NOT a user turn, which now has its own typed `Event::UserTurn`
        // variant and its own arm/tests below.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::AgentProgress {
                note: "repeated step detected".to_string(),
            },
        ));

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text == "repeated step detected"
        ));
    }

    #[test]
    fn user_turn_event_pushes_entry_user_not_a_notice() {
        // This item's acceptance test: a consumer (here, the TUI's own
        // `apply`) can identify a user turn from the typed `Event::UserTurn`
        // variant alone -- no `"user turn: "` string-matching -- and it
        // renders as a real `Entry::User`, not `Entry::Notice`.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::UserTurn {
                text: "hi".to_string(),
                prov: conway::Provenance::UserPrompt,
            },
        ));

        assert!(
            matches!(state.transcript.last(), Some(Entry::User(text)) if text == "hi"),
            "expected exactly one Entry::User(\"hi\"), got {:?}",
            state.transcript
        );
    }

    #[test]
    fn a_single_user_turn_event_appears_in_the_transcript_exactly_once() {
        // The regression the local-push removal (`app.rs`'s `submit`/
        // `deliver_first_message`) risks: not zero, not twice.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::UserTurn {
                text: "only once".to_string(),
                prov: conway::Provenance::UserPrompt,
            },
        ));

        let user_entries: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::User(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_entries,
            vec!["only once"],
            "the prompt must appear exactly once, got {:?}",
            state.transcript
        );
    }

    #[test]
    fn replayed_user_turn_and_assistant_reply_both_render_in_the_transcript() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        // Exactly the envelope sequence `record_to_event` now synthesizes
        // for one `UserTurn` record followed by one `Assistant` record on
        // replay (`SessionHandle::agent_events`/`events_from`'s replay
        // batch): `Event::UserTurn{text, prov}`, then `TextDelta{text}`
        // carrying the assistant's full reply.
        state.apply(&envelope(
            session,
            agent,
            Event::UserTurn {
                text: "hi".to_string(),
                prov: conway::Provenance::UserPrompt,
            },
        ));
        state.apply(&envelope(
            session,
            agent,
            Event::TextDelta {
                text: "hello there".to_string(),
            },
        ));

        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::User(text) if text == "hi"
            )),
            "the replayed user prompt must render as a real Entry::User, not be dropped or \
             turned into a Notice: {:?}",
            state.transcript
        );
        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::Assistant { text, .. } if text == "hello there"
            )),
            "the replayed assistant reply must render as a real Entry::Assistant, not be \
             dropped: {:?}",
            state.transcript
        );
    }

    #[test]
    fn a_notice_between_two_replayed_assistant_turns_keeps_them_as_separate_entries() {
        // The consecutive-turns concern from the review: since each
        // replayed user turn now pushes a non-`Assistant` `Entry::User`
        // first, `append_assistant_text`'s existing "start fresh unless the
        // last entry is already an Assistant" check keeps two different
        // assistant replies from coalescing into one bubble.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        for (prompt, reply) in [("first", "reply one"), ("second", "reply two")] {
            state.apply(&envelope(
                session,
                agent,
                Event::UserTurn {
                    text: prompt.to_string(),
                    prov: conway::Provenance::UserPrompt,
                },
            ));
            state.apply(&envelope(
                session,
                agent,
                Event::TextDelta {
                    text: reply.to_string(),
                },
            ));
        }

        let assistant_texts: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            assistant_texts,
            vec!["reply one", "reply two"],
            "two separate replayed assistant turns must stay as two separate entries"
        );
    }

    fn tool_entry(call_id: &str, preview: &str, expanded: bool) -> Entry {
        Entry::Tool {
            call_id: call_id.to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview: preview.to_string(),
            args: String::new(),
            progress: String::new(),
            expanded,
            ts: None,
        }
    }

    #[test]
    fn toggle_flips_expanded_on_every_tool_entry() {
        let mut state = AppState::new(AgentId::new());
        // Three tool entries: two collapsed, one already expanded. Plus a
        // non-tool entry to confirm the toggle only touches `Entry::Tool`.
        state.transcript.push(Entry::Assistant {
            text: "hi".to_string(),
            model: None,
            summary: None,
            ts: None,
        });
        state.transcript.push(tool_entry("c1", "out1\nout2", false));
        state.transcript.push(tool_entry("c2", "x\ny\nz", false));
        state.transcript.push(tool_entry("c3", "p", true));

        state.toggle_all_tool_entries_expanded();

        let expanded_flags: Vec<bool> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(expanded_flags, vec![true, true, false]);
        // The assistant entry is untouched (still an Assistant, not a Tool).
        assert!(matches!(state.transcript[0], Entry::Assistant { .. }));
    }

    #[test]
    fn toggle_is_an_involution_round_trips_back_to_the_original_state() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(tool_entry("c1", "out1\nout2", false));
        state.transcript.push(tool_entry("c2", "x\ny\nz", true));

        state.toggle_all_tool_entries_expanded();
        let after_first: Vec<bool> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(after_first, vec![true, false]);

        state.toggle_all_tool_entries_expanded();
        let after_second: Vec<bool> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(after_second, vec![false, true]);
    }

    /// The no-snap contract: toggling `expanded` must NOT touch `scroll` or
    /// `follow_tail`. The next render's clamp (`state.scroll.min(max)`)
    /// re-clamps to the nearest valid position without jumping the viewport.
    #[test]
    fn toggle_does_not_touch_scroll_or_follow_tail() {
        let mut state = AppState::new(AgentId::new());
        state
            .transcript
            .push(tool_entry("c1", "a\nb\nc\nd\ne", false));
        state.scroll = 7;
        state.follow_tail = false;

        state.toggle_all_tool_entries_expanded();

        assert_eq!(
            state.scroll, 7,
            "toggle must not change `scroll` -- the render clamp re-clamps"
        );
        assert!(!state.follow_tail, "toggle must not change `follow_tail`");
    }

    /// T5 default: a freshly-constructed `Entry::Tool` (via `apply`'s
    /// `ToolCallProposed` arm) starts collapsed.
    #[test]
    fn new_tool_entry_from_apply_starts_collapsed() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls"}),
            },
        ));

        match state.transcript.last() {
            Some(Entry::Tool { expanded, .. }) => assert!(
                !*expanded,
                "a freshly-proposed tool entry must start collapsed"
            ),
            other => panic!("expected a Tool entry, got {other:?}"),
        }
    }

    /// T5 config default: `AppState::new` defaults `tool_preview_lines` to
    /// 3 (the documented default).
    #[test]
    fn new_state_defaults_tool_preview_lines_to_3() {
        let state = AppState::new(AgentId::new());
        assert_eq!(state.tool_preview_lines, 3);
    }

    #[test]
    fn clamp_none_falls_back_to_default_3() {
        assert_eq!(clamp_tool_preview_lines(None), 3);
    }

    #[test]
    fn clamp_in_range_value_is_kept() {
        assert_eq!(clamp_tool_preview_lines(Some(1)), 1);
        assert_eq!(clamp_tool_preview_lines(Some(3)), 3);
        assert_eq!(clamp_tool_preview_lines(Some(50)), 50);
        assert_eq!(clamp_tool_preview_lines(Some(200)), 200);
    }

    #[test]
    fn clamp_zero_falls_back_to_default() {
        assert_eq!(clamp_tool_preview_lines(Some(0)), 3);
    }

    #[test]
    fn clamp_above_max_falls_back_to_default() {
        assert_eq!(clamp_tool_preview_lines(Some(201)), 3);
        assert_eq!(clamp_tool_preview_lines(Some(u32::MAX)), 3);
    }

    /// `ThinkingDelta` creates an `Entry::Reasoning` on the first delta and
    /// appends to it on subsequent deltas (mirroring `TextDelta` ->
    /// `Entry::Assistant`). The entry is stored EXPANDED-by-default --
    /// `show_reasoning` defaults `true`; `build_lines` is the gate that
    /// hides it when the flag is off, not the apply path.
    #[test]
    fn thinking_delta_creates_and_appends_reasoning_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());

        state.apply(&envelope(
            session,
            root,
            Event::ThinkingDelta {
                text: "think".to_string(),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ThinkingDelta {
                text: "ing".to_string(),
            },
        ));

        match state.transcript.last() {
            Some(Entry::Reasoning { text, model, .. }) => {
                assert_eq!(text, "thinking", "deltas coalesce");
                assert_eq!(
                    model.as_deref(),
                    Some("anthropic/claude-sonnet-4-6"),
                    "model stamped from focused_model"
                );
            }
            other => panic!("expected a Reasoning entry, got {other:?}"),
        }
        assert!(
            state.show_reasoning,
            "show_reasoning defaults true (EXPANDED by default)"
        );
    }

    /// `ToolProgress` notes append to the matching in-flight `Entry::Tool`
    /// by `call_id` (previously dropped by the wildcard arm).
    #[test]
    fn tool_progress_appends_to_matching_tool_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls"}),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolProgress {
                call_id: "tc_1".to_string(),
                note: "step 1".to_string(),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolProgress {
                call_id: "tc_1".to_string(),
                note: "step 2".to_string(),
            },
        ));

        match state.transcript.last() {
            Some(Entry::Tool { progress, .. }) => {
                assert_eq!(progress, "step 1\nstep 2", "notes joined with newline");
            }
            other => panic!("expected a Tool entry, got {other:?}"),
        }
    }

    /// `ToolProgress` for an unknown `call_id` is a no-op (never panics on
    /// an id it has no record of).
    #[test]
    fn tool_progress_for_unknown_call_id_is_a_noop() {
        let mut state = AppState::new(AgentId::new());
        state.append_tool_progress("nope", "note");
        assert!(state.transcript.is_empty());
    }

    /// `ToolCallProposed` stores the `args` as a compact JSON string
    /// (previously discarded).
    #[test]
    fn tool_call_proposed_stores_args() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls", "path": "/tmp"}),
            },
        ));

        match state.transcript.last() {
            Some(Entry::Tool { args, .. }) => {
                assert!(
                    args.contains("\"command\":\"ls\""),
                    "args stored compact: {args}"
                );
                assert!(
                    args.contains("\"path\":\"/tmp\""),
                    "args stored compact: {args}"
                );
            }
            other => panic!("expected a Tool entry, got {other:?}"),
        }
    }

    /// `toggle_thinking` flips `show_reasoning` and returns the new value.
    #[test]
    fn toggle_thinking_flips_show_reasoning() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.show_reasoning, "defaults true");
        assert!(!state.toggle_thinking(), "toggles to false");
        assert!(state.toggle_thinking(), "toggles back to true");
    }

    /// `toggle_timestamps` flips `show_timestamps` (default false) and
    /// returns the new value.
    #[test]
    fn toggle_timestamps_flips_show_timestamps() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.show_timestamps, "defaults false");
        assert!(state.toggle_timestamps(), "toggles to true");
        assert!(!state.toggle_timestamps(), "toggles back to false");
    }

    #[test]
    fn adjust_tool_preview_lines_steps_by_delta() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.tool_preview_lines, 3, "the built-in default");
        assert_eq!(state.adjust_tool_preview_lines(1), 4);
        assert_eq!(state.adjust_tool_preview_lines(1), 5);
        assert_eq!(state.adjust_tool_preview_lines(-2), 3);
    }

    /// Stepping below the floor stops AT the floor -- it must not
    /// bounce up to `clamp_tool_preview_lines`'s config-validation fallback
    /// (3), which would read as broken for an interactive stepper.
    #[test]
    fn adjust_tool_preview_lines_floors_at_one_without_bouncing_to_the_default() {
        let mut state = AppState::new(AgentId::new());
        state.tool_preview_lines = 1;

        assert_eq!(
            state.adjust_tool_preview_lines(-1),
            1,
            "must stop at the floor"
        );
        assert_eq!(
            state.adjust_tool_preview_lines(-1000),
            1,
            "a huge negative step must still land on the floor, not panic or wrap"
        );
    }

    #[test]
    fn adjust_tool_preview_lines_caps_at_two_hundred() {
        let mut state = AppState::new(AgentId::new());
        state.tool_preview_lines = 200;

        assert_eq!(
            state.adjust_tool_preview_lines(1),
            200,
            "must stop at the cap"
        );
        assert_eq!(
            state.adjust_tool_preview_lines(1_000_000),
            200,
            "a huge positive step must still land on the cap, not panic or wrap"
        );
    }

    #[test]
    fn adjust_tool_preview_lines_never_panics_at_either_i32_extreme() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.adjust_tool_preview_lines(i32::MIN), 1);
        assert_eq!(state.adjust_tool_preview_lines(i32::MAX), 200);
    }
}
