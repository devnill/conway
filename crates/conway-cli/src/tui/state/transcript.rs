//! The transcript's own entry model ([`Entry`], [`ToolStatus`]) and the
//! [`AppState`] methods that build/mutate it from applied events: assistant
//! and reasoning-trace deltas, tool-call lifecycle (proposed -> running ->
//! finished, plus streamed progress notes), the T4 "show reasoning" /
//! "show timestamps" toggles, and the T5 tool-preview expand/collapse +
//! line-count cap. Turn-end summary stamping lives in
//! [`super::turn_summary`]; transcript-pane scrolling lives in
//! [`super::scroll`] -- both act on the same [`AppState::transcript`] this
//! module owns the entries of, but are their own seams.
//!
//! [`backfill_entries`] (board item `01M1YS4FMJH004D1Y619MTBY7A`) is the
//! ONE other way an `Entry` gets built, alongside `apply`'s live event
//! dispatch: a pure `&[LogRecord] -> Vec<Entry>` mapping for
//! `/resume`/`--resume`/`--fork-from`/`--continue` to draw a resumed
//! session's history from, with no live `Event` stream involved. See its
//! own doc for why this is a NEW mapping rather than a reuse of `conway::
//! SessionHandle`'s `record_to_event` or `conway sessions show`'s renderer.

use super::*;
use conway::plugin::ContentBlock;

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
        /// expand (tool-args reuse, or a transcript-cursor selection)
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
    /// concretely) a recon found a field/constructor approach
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
    /// A PROMPTED `Event::PermissionDecision`, rendered dim, one line,
    /// directly beneath the [`Entry::Tool`] it belongs to (`call_id`
    /// matches -- `AppState::apply_permission_decision` pushes this
    /// immediately when the event arrives, which is always chronologically
    /// between that tool's own `ToolCallProposed` and `ToolCallStarted`/
    /// `ToolCallFinished`, so no separate lookup/re-sort is needed to place
    /// it correctly). A SYSTEM record -- like `conway::LogRecord::
    /// PermissionDecisionRecord`'s own doc says of its durable twin, this
    /// is neither model output nor operator-typed prose,
    /// so it deliberately does NOT reuse [`Entry::Notice`]'s cyan styling
    /// (which every other informational line in this enum, and the
    /// operator's own typed messages elsewhere, share) or [`Entry::Error`]'s
    /// red -- `view/transcript.rs::entry_lines` renders it in `theme.dim`
    /// instead, the same slot a tool's own accumulated `progress` notes and
    /// truncated `args:` preview already use, so an audit fact about HOW a
    /// call was authorized reads as exactly that, never as something the
    /// model said or the operator typed. `text` is pre-formatted at apply
    /// time (mirroring [`Entry::Notice`]/[`Entry::Error`]'s own convention
    /// -- a decision arrives complete, never streamed, so there is nothing
    /// render-time formatting would buy here) by `state::transcript::
    /// format_permission_decision_note`. Only ever pushed for a decision
    /// that actually reached the operator's own gate (`waited_ms.is_some()`
    /// on the source event) -- see `AppState::apply`'s own
    /// `Event::PermissionDecision` arm doc for why every other source is
    /// silent here.
    PermissionDecision {
        call_id: String,
        text: String,
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

    /// Board item `01M1FSJ4E2S5M9KBSBJAAPJQ48`: `Event::StreamRestarted`
    /// discarded a mid-stream failure's partial deltas -- already appended
    /// to the transcript by `append_assistant_text`/`append_reasoning_text`
    /// as they streamed in (see those methods' own doc) -- and this truncates the
    /// in-progress entries back to their pre-delta content before appending
    /// a visible discard notice, so the retry's own deltas resume on a
    /// clean bubble rather than silently splicing onto the discarded
    /// partial text.
    ///
    /// Bounded below by `turn_transcript_start` (mirrors
    /// `stamp_turn_summary`'s own bound, same reasoning): only the CURRENT
    /// turn's own last Assistant/Reasoning entry is ever a same-candidate
    /// retry's target, never a previous turn's already-settled bubble.
    /// Either scan is a no-op if its own discarded count is `0` (the common
    /// case for a text-only or thinking-only partial reply) or if no
    /// matching entry exists in that window.
    pub(super) fn apply_stream_restarted(
        &mut self,
        attempt: u32,
        discarded_text_chars: usize,
        discarded_thinking_chars: usize,
    ) {
        let start = self.turn_transcript_start.min(self.transcript.len());
        if discarded_text_chars > 0 {
            if let Some(Entry::Assistant { text, .. }) = self.transcript[start..]
                .iter_mut()
                .rev()
                .find(|e| matches!(e, Entry::Assistant { .. }))
            {
                truncate_tail_chars(text, discarded_text_chars);
            }
        }
        if discarded_thinking_chars > 0 {
            if let Some(Entry::Reasoning { text, .. }) = self.transcript[start..]
                .iter_mut()
                .rev()
                .find(|e| matches!(e, Entry::Reasoning { .. }))
            {
                truncate_tail_chars(text, discarded_thinking_chars);
            }
        }
        self.transcript.push(Entry::Notice {
            text: format!("stream restarted (attempt {attempt}); partial output above discarded"),
        });
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
        let mut diff_seed: Option<(String, String)> = None; // (name, args) of the matched entry
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool {
                call_id: id,
                status,
                preview: p,
                name,
                args,
                ..
            } = entry
            {
                if id == call_id {
                    *status = ToolStatus::Finished { is_error };
                    *p = preview;
                    if !is_error {
                        diff_seed = Some((name.clone(), args.clone()));
                    }
                    break;
                }
            }
        }
        // Board item 01M1YVEJB6GAPST5YZET4KZZE2: compute this call's own
        // diff EXACTLY ONCE, right here, and store it in `self.tool_diffs`
        // -- never recomputed later (see that field's own doc for why).
        // Done AFTER the `transcript` borrow above is released.
        if let Some((name, args_json)) = diff_seed {
            self.settle_tool_diff(call_id, &name, &args_json);
        }
    }

    /// The other half of [`Self::finish_tool`]'s diff computation, split out
    /// so the borrow of `self.transcript` above is fully released before
    /// this touches `self.diff_track`/`self.tool_diffs`. Folds this ONE
    /// call's own change onto the running `self.diff_track[path]` (via
    /// `crate::diff::apply_touch` -- the SAME fold rule `crate::diff::
    /// cumulative_diffs` uses, so a call's own settled-entry diff and its
    /// contribution to `/diff`'s cumulative view never disagree), advances
    /// the tracker, and stores the resulting unified diff in
    /// `self.tool_diffs` keyed by `call_id`. A no-op (no entry stored) for
    /// any tool other than `edit`/`write`, malformed/missing `args` JSON,
    /// or a fold that produced no line-level change.
    fn settle_tool_diff(&mut self, call_id: &str, name: &str, args_json: &str) {
        let Ok(args) = serde_json::from_str::<serde_json::Value>(args_json) else {
            return;
        };
        let Some(touch) = crate::diff::file_touch_from_args(name, &args) else {
            return;
        };
        let before = self
            .diff_track
            .get(&touch.path)
            .cloned()
            .unwrap_or_default();
        let after = crate::diff::apply_touch(&before, &touch.kind);
        self.diff_track.insert(touch.path.clone(), after.clone());
        let diff_text = crate::diff::unified_diff(&touch.path, &touch.path, &before, &after);
        if !diff_text.is_empty() {
            self.tool_diffs.insert(call_id.to_string(), diff_text);
        }
    }

    /// Builds and pushes the dim [`Entry::PermissionDecision`] line for one
    /// `Event::PermissionDecision`, called from `AppState::apply`'s own arm.
    /// ALWAYS removes this call's stashed `PermissionResolved` kind from
    /// [`AppState::permission_decision_pending`] first, regardless of the
    /// early return below -- every call gets exactly one `PermissionResolved`
    /// AND one `PermissionDecision`, so a version of this that only removed
    /// on the render path would leak one map entry per silently-resolved
    /// (pattern/rule/hook/mode) call for the rest of the session.
    ///
    /// Renders nothing (no push) when `waited_ms` is `None` -- this
    /// decision never reached the operator's own gate; see `AppState::
    /// apply`'s own `Event::PermissionDecision` arm doc for why that is the
    /// deliberate "prompted decisions only" gate, not a bug.
    pub(super) fn apply_permission_decision(
        &mut self,
        call_id: &str,
        waited_ms: Option<u64>,
        feedback: Option<String>,
    ) {
        let kind = self.permission_decision_pending.remove(call_id);
        let Some(waited_ms) = waited_ms else {
            return;
        };
        self.transcript.push(Entry::PermissionDecision {
            call_id: call_id.to_string(),
            text: format_permission_decision_note(kind, waited_ms, feedback.as_deref()),
        });
    }
}

/// Removes up to `chars_to_remove` characters from the END of `text`,
/// char-boundary safe (`String::truncate` panics on a non-char-boundary
/// byte index, which a naive `text.len() - chars_to_remove` would risk the
/// moment any multi-byte character is involved). `chars_to_remove` larger
/// than `text`'s own char count truncates to empty rather than panicking or
/// going negative -- `Event::StreamRestarted`'s discarded-char counts are
/// this attempt's own running total and are always `<=` what was actually
/// appended to the matching entry, but this stays defensive rather than
/// trusting that invariant across the bus.
fn truncate_tail_chars(text: &mut String, chars_to_remove: usize) {
    if chars_to_remove == 0 {
        return;
    }
    let keep = text.chars().count().saturating_sub(chars_to_remove);
    let new_len = text
        .char_indices()
        .nth(keep)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(0);
    text.truncate(new_len);
}

/// The dim one-line text [`AppState::apply_permission_decision`] pushes as
/// an [`Entry::PermissionDecision`] -- e.g. `"allowed once · waited 4m
/// 12s"`, `"denied with feedback: too risky · waited 12s"`. `kind` is the
/// correlated `Event::PermissionResolved`'s own `conway::
/// PermissionDecisionKind` for the same call (`None` only when this
/// session's stream never observed that sibling event for this call -- a
/// subscription that started mid-call; falls back to a wording derived
/// from `feedback.is_some()` alone in that case, matching the same-shaped
/// fallback the `#[non_exhaustive]` catch-all below uses for a future
/// variant this crate does not yet know about -- never a panic or an empty
/// string either way).
///
/// `kind` decides the VERB (`allowed once`/`allowed always`/`allowed
/// (cached)`/`denied`/`denied with feedback`); `feedback`, when `Some`, is
/// appended after a colon regardless of which verb was chosen -- a bare
/// `Denied` (the operator's own `n`, or a structural refusal that still
/// reached the gate) carries a reason too (`conway::log::
/// PermissionDecisionRecordKind`'s own doc, in `conway-core`: "feedback ...
/// for any DENY-shaped decision"), so `"denied: <reason>"` and `"denied
/// with feedback: <reason>"` stay visually distinct -- the former is the
/// call's own rendered refusal text, the latter is what the OPERATOR
/// typed after pressing `Esc`.
fn format_permission_decision_note(
    kind: Option<conway::PermissionDecisionKind>,
    waited_ms: u64,
    feedback: Option<&str>,
) -> String {
    use conway::PermissionDecisionKind as Kind;
    let mut verb = match kind {
        Some(Kind::AllowOnce) => "allowed once".to_string(),
        Some(Kind::AllowAlways) => "allowed always".to_string(),
        Some(Kind::Cached) => "allowed (cached)".to_string(),
        Some(Kind::Denied) => "denied".to_string(),
        Some(Kind::DeniedWithFeedback) => "denied with feedback".to_string(),
        Some(_) | None => {
            if feedback.is_some() {
                "denied".to_string()
            } else {
                "allowed".to_string()
            }
        }
    };
    if let Some(reason) = feedback {
        verb.push_str(": ");
        verb.push_str(reason);
    }
    format!("{verb} · waited {}", humanize_wait_ms(waited_ms))
}

/// `waited_ms` as `"{m}m {s}s"` for >= 60_000ms, else `"{s}s"` -- mirrors
/// `state::turn_summary::format_turn_summary`'s own `elapsed` shape exactly
/// (duplicated here, not shared, matching this crate's own established
/// practice of a handful of near-identical small per-module time-formatting
/// helpers rather than one shared util with drifting callers -- see
/// `view/transcript.rs::truncate_chars_with_ellipsis`'s own doc for the
/// identical precedent on a truncation helper). Rounds DOWN to the nearest
/// whole second -- a wait is never shown to the operator at millisecond
/// precision, matching the turn-summary's own elapsed figure.
fn humanize_wait_ms(waited_ms: u64) -> String {
    let secs = waited_ms / 1000;
    if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m {s}s")
    } else {
        format!("{secs}s")
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

/// Board item `01M1YS4FMJH004D1Y619MTBY7A`: turns a session's own
/// persisted [`conway::LogRecord`]s into the transcript [`Entry`]s
/// `/resume`, `--resume`/`--fork-from`/`--continue`
/// (`tui::app::startup::App::resolve_handle`) all draw on open -- the
/// mapping this crate long did without (`tui::commands`'s old
/// `/resume` arm named the gap outright: "no LogRecord -> Entry mapping
/// exists anywhere in this crate today").
///
/// **Why this is a NEW mapping, not a reuse of `conway::SessionHandle`'s
/// own `record_to_event` (`LogRecord -> Event`):** every caller of THIS
/// function reads its input the same way `conway sessions show` reads
/// its own (`SessionHandle::transcript`, the ancestry-resolved effective
/// transcript) -- that data-source IS reused. But `record_to_event` itself
/// (read in full before writing this) discloses its own narrowing in its
/// own doc: it maps a whole `LogRecord::Assistant` to ONE bare `TextDelta`
/// of the record's TEXT content alone, dropping any `ToolUse` content
/// block outright -- correct for ITS purpose (focus-switch replay within
/// an already-live process) but wrong for this one, since it would
/// silently drop every backfilled tool call the resume path's acceptance
/// criteria require showing. Piping `record_to_event`'s output through
/// `AppState::apply` was tried against that doc first and rejected for
/// exactly this reason, not attempted-and-abandoned blindly.
///
/// **Why this is not `conway sessions show`'s renderer either:** that
/// command's entire non-JSON rendering is `println!("{record:#?}")` -- a
/// bare `Debug` dump, no per-kind text formatting, tool-call/result
/// folding, or preview truncation of any kind to reuse. There is
/// genuinely no existing formatter to share; this is the first one, in
/// the one crate (`conway-cli`) that owns `Entry`'s shape at all.
///
/// **Never rewrites or reorders the log**: pure `&[LogRecord] ->
/// Vec<Entry>`, read-only over an already-fetched, already-ordered slice
/// -- every production caller passes `SessionHandle::transcript`'s own
/// return value straight through, in the seq order the store already
/// resolved it in.
///
/// **Declaration-honesty**: every record kind without a dedicated arm in
/// `push_record` below still produces exactly one `Entry` -- a dim
/// placeholder naming the kind (`LogRecord::kind_str`) -- never silently
/// dropped. See that function's own trailing wildcard arm and
/// `unknown_record_kind_becomes_a_named_placeholder_not_a_silent_drop`,
/// below, for the test that catches an implementation which drops it
/// instead.
pub fn backfill_entries(records: &[conway::LogRecord]) -> Vec<Entry> {
    let mut entries = Vec::new();
    for record in records {
        push_record(&mut entries, record);
    }
    entries
}

/// One [`backfill_entries`] record. `#[non_exhaustive]` on
/// `conway::LogRecord` (`conway-core`'s own attribute) means this match
/// needs a wildcard arm regardless of how many kinds get a dedicated one
/// below -- exactly the arm the module doc's "declaration-honesty"
/// paragraph describes.
fn push_record(entries: &mut Vec<Entry>, record: &conway::LogRecord) {
    use conway::LogRecord;
    match record {
        // The session's own header: metadata, not a transcript line --
        // `record_to_event`'s own first arm treats it identically.
        LogRecord::Header(_) => {}
        LogRecord::UserTurn { text, .. } => entries.push(Entry::User(text.clone())),
        LogRecord::Assistant { ts, content, .. } => push_assistant_content(entries, content, *ts),
        LogRecord::ToolResultRecord { result, .. } => {
            fold_tool_result(
                entries,
                &result.call_id,
                &result.blocks,
                result.is_error,
                &result.tool.to_string(),
            );
        }
        LogRecord::ForkDirective { text, by, .. } => entries.push(Entry::Notice {
            text: format!("fork directive from {by}: {text}"),
        }),
        LogRecord::ParentSteer { text, from, .. } => entries.push(Entry::Notice {
            text: format!("parent steer from {from}: {text}"),
        }),
        LogRecord::SystemNote { text, reason, .. } => entries.push(Entry::Notice {
            text: format!("{reason}: {text}"),
        }),
        LogRecord::AgentResultRecord { result, .. } => entries.push(Entry::Notice {
            text: if result.summary.is_empty() {
                "agent finished".to_string()
            } else {
                format!("agent finished: {}", result.summary)
            },
        }),
        LogRecord::ChildResultRecord { result, .. } => entries.push(Entry::Notice {
            text: format!("child {} finished: {}", result.agent_id, result.summary),
        }),
        LogRecord::ContextReportRecord { report, .. } => entries.push(Entry::Notice {
            text: format!(
                "context report: {} segments, {} tokens",
                report.segments.len(),
                report.total_tokens_est
            ),
        }),
        // Declaration-honesty: every OTHER record kind (today:
        // `ContextMask`/`ContextPathSet`/`ContextPathNamed`/
        // `PermissionDecisionRecord` -- internal bookkeeping this
        // transcript pane has no dedicated line for yet, matching
        // `record_to_event`'s own identical `_ => None` for these same
        // four) still produces exactly one entry, naming the kind, rather
        // than vanishing.
        //
        // Reuses `Entry::PermissionDecision`'s existing render slot
        // (the ONE `theme.dim` line `view/transcript.rs::entry_lines`
        // already has -- this module owns transcript STATE, not the view,
        // and `view/transcript.rs` was fenced off when this landed;
        // adding a ninth `Entry` variant would need a matching
        // arm there). `call_id` is left empty: nothing reads it outside
        // `AppState::apply_permission_decision`'s own construction path,
        // which this is not. Disclosed, deliberate reuse of an
        // existing-but-differently-named variant for its STYLE, not its
        // semantics -- the text itself always says plainly what it is
        // ("record not shown"), so a reader who greps for a real
        // permission audit line and finds one of these is not misled once
        // they read it.
        other => entries.push(Entry::PermissionDecision {
            call_id: String::new(),
            text: format!("[{} record not shown in the transcript]", other.kind_str()),
        }),
    }
}

/// Folds one `LogRecord::Assistant`'s content blocks into `entries`,
/// mirroring the exact live shape [`AppState::apply`] already builds
/// (`Event::TextDelta`/`Event::ThinkingDelta`/`Event::ToolCallProposed`,
/// one call per block, in content order) -- so a backfilled `Assistant`
/// record renders identically to however that same reply looked when it
/// originally streamed in live: consecutive `Text` blocks fold into ONE
/// `Entry::Assistant` (mirrors [`AppState::append_assistant_text`]'s own
/// create-or-append onto `entries.last_mut()`), consecutive `Thinking`
/// blocks fold into ONE `Entry::Reasoning` the same way, and each
/// `ToolUse` block becomes its own `Entry::Tool` in `Proposed` state
/// (mirrors `Event::ToolCallProposed`'s own apply arm) -- later folded to
/// `Finished` by this same record's or a later record's
/// `ToolResultRecord`/`ToolResultBlock` (`fold_tool_result`).
///
/// `model` is always `None` on a replayed entry, matching [`Entry::
/// Assistant`]'s own doc ("`None` for replayed entries ... the renderer
/// then omits the `[modelname]> ` marker").
fn push_assistant_content(entries: &mut Vec<Entry>, content: &[ContentBlock], ts: DateTime<Utc>) {
    for block in content {
        match block {
            ContentBlock::Text { text } => {
                if let Some(Entry::Assistant { text: existing, .. }) = entries.last_mut() {
                    existing.push_str(text);
                } else {
                    entries.push(Entry::Assistant {
                        text: text.clone(),
                        model: None,
                        summary: None,
                        ts: Some(ts),
                    });
                }
            }
            ContentBlock::Thinking { text, .. } => {
                if let Some(Entry::Reasoning { text: existing, .. }) = entries.last_mut() {
                    existing.push_str(text);
                } else {
                    entries.push(Entry::Reasoning {
                        text: text.clone(),
                        model: None,
                        summary: None,
                        ts: Some(ts),
                    });
                }
            }
            ContentBlock::ToolUse {
                call_id,
                name,
                arguments,
            } => {
                entries.push(Entry::Tool {
                    call_id: call_id.clone(),
                    name: name.to_string(),
                    status: ToolStatus::Proposed,
                    preview: String::new(),
                    args: arguments.to_string(),
                    progress: String::new(),
                    expanded: false,
                    ts: Some(ts),
                });
            }
            // Not produced by any live path today (a `tool_result` block
            // embedded directly in an ASSISTANT message's own content,
            // rather than as its own top-level `ToolResultRecord`) -- but
            // `ContentBlock` is a real enum variant, so it gets a real
            // fold rather than a silent skip, exactly like the top-level
            // `ToolResultRecord` case just above.
            ContentBlock::ToolResultBlock {
                call_id,
                blocks,
                is_error,
            } => {
                fold_tool_result(entries, call_id, blocks, *is_error, "");
            }
            ContentBlock::Image { .. } => entries.push(Entry::Notice {
                text: "[image content omitted]".to_string(),
            }),
            // `ContentBlock` is `#[non_exhaustive]`, so this arm is REQUIRED
            // to compile and will silently start catching real content the
            // day a variant is added. It pushes a visible placeholder rather
            // than dropping, for the same reason the unknown-record-kind arm
            // does: a backfilled transcript that quietly omits content would
            // misrepresent what the session actually contains.
            _ => entries.push(Entry::Notice {
                text: "[unrecognized content block omitted]".to_string(),
            }),
        }
    }
}

/// Mirrors `conway::session_handle`'s own (private, unexported)
/// `tool_result_preview`: the first `ContentBlock::Text` block's text,
/// truncated to 200 chars. Deliberately duplicated, not called --
/// `conway-cli`'s production code cannot depend on `conway-core` directly
/// (`no_forbidden_deps`, `crates/conway-cli/tests/cli_surface.rs`), and
/// that helper is private to `conway`'s own crate besides -- but the
/// algorithm is exactly three lines and needs to match the live shape
/// exactly, so a backfilled tool result's preview reads identically to
/// however that same result looked when it originally finished live.
fn tool_result_preview(blocks: &[ContentBlock]) -> String {
    const PREVIEW_LIMIT: usize = 200;
    let text = blocks
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    text.chars().take(PREVIEW_LIMIT).collect()
}

/// Folds a tool result into the matching in-flight [`Entry::Tool`] pushed
/// earlier in THIS SAME backfill pass (by `call_id`, searching backward --
/// mirrors [`AppState::finish_tool`]'s identical live-path search, just
/// over the `Vec` under construction instead of `self.transcript`).
///
/// No match found (a truncated record window, or the `ContentBlock::
/// ToolResultBlock` shape above with no preceding `ToolUse` earlier in
/// this same slice) still produces an entry -- `Finished` immediately,
/// `tool_name` best-effort (possibly empty, for the embedded-block case,
/// which carries no tool name of its own) -- rather than silently
/// discarding a real tool result this session's log genuinely recorded.
fn fold_tool_result(
    entries: &mut Vec<Entry>,
    call_id: &str,
    blocks: &[ContentBlock],
    is_error: bool,
    tool_name: &str,
) {
    let preview = tool_result_preview(blocks);
    for entry in entries.iter_mut().rev() {
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
    entries.push(Entry::Tool {
        call_id: call_id.to_string(),
        name: tool_name.to_string(),
        status: ToolStatus::Finished { is_error },
        preview,
        args: String::new(),
        progress: String::new(),
        expanded: false,
        ts: None,
    });
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

    /// A full turn's exact event sequence: one
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

    /// Board item `01M1FSJ4E2S5M9KBSBJAAPJQ48`, acceptance criterion 4:
    /// `Event::StreamRestarted` truncates the in-progress `Entry::Assistant`
    /// back to its pre-delta content ("hello world" minus the discarded
    /// "world") and appends a visible discard notice naming the retry
    /// attempt.
    #[test]
    fn stream_restarted_truncates_the_in_progress_assistant_entry_and_appends_a_notice() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::TextDelta {
                text: "hello ".to_string(),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::TextDelta {
                text: "world".to_string(),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::StreamRestarted {
                agent_id: root,
                attempt: 2,
                discarded_text_chars: "world".chars().count(),
                discarded_thinking_chars: 0,
            },
        ));

        assert_eq!(
            state.transcript.len(),
            2,
            "the assistant entry plus one notice"
        );
        match &state.transcript[0] {
            Entry::Assistant { text, .. } => assert_eq!(
                text, "hello ",
                "truncated back to the content before the discarded delta"
            ),
            other => panic!("expected Entry::Assistant, got {other:?}"),
        }
        match &state.transcript[1] {
            Entry::Notice { text } => {
                assert!(
                    text.contains("attempt 2"),
                    "notice names the attempt: {text:?}"
                );
                assert!(
                    text.contains("discarded"),
                    "notice says discarded: {text:?}"
                );
            }
            other => panic!("expected Entry::Notice, got {other:?}"),
        }
    }

    /// The reasoning-side mirror of the test above: a `ThinkingDelta`
    /// in-progress `Entry::Reasoning` is truncated by
    /// `discarded_thinking_chars`, independent of (and without touching) any
    /// `Entry::Assistant`.
    #[test]
    fn stream_restarted_truncates_the_in_progress_reasoning_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ThinkingDelta {
                text: "pondering ".to_string(),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ThinkingDelta {
                text: "deeply".to_string(),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::StreamRestarted {
                agent_id: root,
                attempt: 2,
                discarded_text_chars: 0,
                discarded_thinking_chars: "deeply".chars().count(),
            },
        ));

        match &state.transcript[0] {
            Entry::Reasoning { text, .. } => assert_eq!(text, "pondering "),
            other => panic!("expected Entry::Reasoning, got {other:?}"),
        }
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

    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: a full `edit` call's own
    /// event sequence -- `ToolCallProposed` (which reads the target file's
    /// CURRENT bytes into `diff_track`, before the call runs) then
    /// `ToolCallFinished { is_error: false }` -- stores a unified diff in
    /// `state.tool_diffs`, keyed by `call_id`, showing exactly the
    /// substitution the call made. `old_string` spans TWO lines here (the
    /// item's own required check), so this also pins that both removed
    /// lines and both added lines appear.
    #[test]
    fn a_settled_edit_call_stores_a_two_line_diff_in_tool_diffs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").expect("seed file");
        let path_str = path.to_string_lossy().to_string();

        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("edit"),
                args: serde_json::json!({
                    "path": path_str,
                    "old_string": "beta\ngamma",
                    "new_string": "BETA\nGAMMA",
                }),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallFinished {
                call_id: "tc_1".to_string(),
                is_error: false,
                preview: format!("edited {path_str}: 1 replacement(s)"),
            },
        ));

        let diff = state
            .tool_diffs
            .get("tc_1")
            .unwrap_or_else(|| panic!("expected a stored diff, got {:?}", state.tool_diffs));
        assert!(diff.contains("-beta"), "{diff}");
        assert!(diff.contains("-gamma"), "{diff}");
        assert!(diff.contains("+BETA"), "{diff}");
        assert!(diff.contains("+GAMMA"), "{diff}");
    }

    /// The same sequence for `write`: the whole new `content` replaces
    /// whatever `diff_track` captured at propose time, and the resulting
    /// diff shows the old lines removed, the new lines added.
    #[test]
    fn a_settled_write_call_stores_a_diff_against_its_pre_write_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "old content\n").expect("seed file");
        let path_str = path.to_string_lossy().to_string();

        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("write"),
                args: serde_json::json!({"path": path_str, "content": "new content\n"}),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallFinished {
                call_id: "tc_1".to_string(),
                is_error: false,
                preview: format!("wrote 12 bytes to {path_str}"),
            },
        ));

        let diff = state
            .tool_diffs
            .get("tc_1")
            .expect("expected a stored diff");
        assert!(diff.contains("-old content"), "{diff}");
        assert!(diff.contains("+new content"), "{diff}");
    }

    /// A FAILED call (`is_error: true`, e.g. `old_string not found`) stores
    /// no diff -- nothing actually changed, so there is nothing to show.
    #[test]
    fn a_failed_edit_call_stores_no_diff() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "alpha\n").expect("seed file");
        let path_str = path.to_string_lossy().to_string();

        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("edit"),
                args: serde_json::json!({
                    "path": path_str,
                    "old_string": "does not exist",
                    "new_string": "x",
                }),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallFinished {
                call_id: "tc_1".to_string(),
                is_error: true,
                preview: "old_string not found".to_string(),
            },
        ));

        assert!(
            !state.tool_diffs.contains_key("tc_1"),
            "a failed call must not store a diff: {:?}",
            state.tool_diffs
        );
    }

    /// A non-`edit`/`write` tool (e.g. `bash`) stores no diff, even on
    /// success -- there is no file-content change to show.
    #[test]
    fn a_settled_non_edit_tool_stores_no_diff() {
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
            Event::ToolCallFinished {
                call_id: "tc_1".to_string(),
                is_error: false,
                preview: "".to_string(),
            },
        ));

        assert!(state.tool_diffs.is_empty(), "{:?}", state.tool_diffs);
    }

    /// A multi-byte character sitting mid-line in the edited content must
    /// never panic while being folded into a diff -- the exact bug class
    /// `view/transcript.rs`'s own `truncate_chars_with_ellipsis` was fixed
    /// for (a raw byte-index slice landing inside a multi-byte char).
    /// Nothing in this diff path slices by byte offset, but this pins that
    /// invariant against a regression the same way the item's own required
    /// check asks for.
    #[test]
    fn a_settled_edit_with_a_multibyte_character_mid_line_never_panics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "price: 10\u{2014}20 dollars\n").expect("seed file"); // em dash
        let path_str = path.to_string_lossy().to_string();

        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("edit"),
                args: serde_json::json!({
                    "path": path_str,
                    "old_string": "10\u{2014}20",
                    "new_string": "15\u{2014}25",
                }),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallFinished {
                call_id: "tc_1".to_string(),
                is_error: false,
                preview: format!("edited {path_str}: 1 replacement(s)"),
            },
        ));

        let diff = state
            .tool_diffs
            .get("tc_1")
            .expect("expected a stored diff");
        assert!(diff.contains('\u{2014}'), "{diff}");
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

    /// Board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`, acceptance criterion 4:
    /// `Event::TurnAborted` -- the live signal a `keep_alive` agent's
    /// turn-scoped budget trip ended just the current turn, not the whole
    /// session -- renders as a transcript `Notice` naming the limit that
    /// tripped. Unlike `Event::AgentFinished`, this must NOT touch the tree
    /// node status: the agent is still alive, just idling for the next
    /// prompt.
    #[test]
    fn turn_aborted_event_pushes_a_notice_naming_the_limit() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::TurnAborted {
                agent_id: root,
                limit: "max_steps=40".to_string(),
                steps_this_turn: 40,
            },
        ));

        match state.transcript.last() {
            Some(Entry::Notice { text }) => {
                assert!(
                    text.contains("max_steps=40"),
                    "expected the tripped limit in the notice, got: {text}"
                );
                assert!(
                    text.contains("turn ended"),
                    "expected the notice to say the TURN (not the session) ended, got: {text}"
                );
            }
            other => panic!("expected a Notice entry, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // `Event::PermissionDecision` -- the TUI's own dim transcript note for
    // a PROMPTED permission decision. `/context`'s own count over the
    // durable record (a separate half of the same board work; it reads the
    // session's own record history, never `ContextReport::segments` --
    // `ContextBuilder` deliberately excludes this record kind from context
    // assembly) is tested in `commands.rs`, not here.
    // -----------------------------------------------------------------

    /// The full real sequence for one prompted, ALLOWED-once call:
    /// `ToolCallProposed` -> `PermissionRequested` -> `PermissionResolved`
    /// (stashes the kind) -> `PermissionDecision` (the new event, carrying
    /// `waited_ms`/`feedback`) -> `ToolCallFinished`. Asserts the pushed
    /// `Entry::PermissionDecision`'s `call_id` matches the tool call it
    /// belongs to, its exact wording, and -- the "beneath the tool call"
    /// placement criterion -- that it sits strictly AFTER the matching
    /// `Entry::Tool` in transcript order.
    #[test]
    fn prompted_allow_decision_renders_a_dim_note_under_its_tool_call() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        let events = vec![
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
            Event::PermissionDecision {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                decision: conway_core::log::PermissionDecisionRecordKind::Allow,
                source: conway_core::log::PermissionDecisionSource::Operator,
                waited_ms: Some(252_000), // 4m 12s
                feedback: None,
            },
            Event::ToolCallFinished {
                call_id: "tc_1".to_string(),
                is_error: false,
                preview: "ok".to_string(),
            },
        ];
        for event in events {
            state.apply(&envelope(session, root, event));
        }

        let tool_idx = state
            .transcript
            .iter()
            .position(|e| matches!(e, Entry::Tool { .. }))
            .expect("a Tool entry");
        let decision_idx = state
            .transcript
            .iter()
            .position(|e| matches!(e, Entry::PermissionDecision { .. }))
            .expect("a PermissionDecision entry");
        assert!(
            decision_idx > tool_idx,
            "the decision note must render BENEATH (after) the tool call it belongs to"
        );

        match &state.transcript[decision_idx] {
            Entry::PermissionDecision { call_id, text } => {
                assert_eq!(call_id, "tc_1");
                assert_eq!(text, "allowed once · waited 4m 12s");
            }
            other => panic!("expected Entry::PermissionDecision, got {other:?}"),
        }
    }

    /// A DENIED, prompted call with the operator's own typed feedback.
    #[test]
    fn prompted_deny_with_feedback_decision_renders_the_reason() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        let events = vec![
            Event::ToolCallProposed {
                call_id: "tc_2".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "rm -rf /"}),
            },
            Event::PermissionRequested {
                call_id: "tc_2".to_string(),
                rendered: "bash: rm -rf /".to_string(),
            },
            Event::PermissionResolved {
                call_id: "tc_2".to_string(),
                decision: PermissionDecisionKind::DeniedWithFeedback,
            },
            Event::PermissionDecision {
                call_id: "tc_2".to_string(),
                tool: ToolName::new("bash"),
                decision: conway_core::log::PermissionDecisionRecordKind::DenyWithFeedback,
                source: conway_core::log::PermissionDecisionSource::Operator,
                waited_ms: Some(12_000),
                feedback: Some("too risky".to_string()),
            },
        ];
        for event in events {
            state.apply(&envelope(session, root, event));
        }

        let decision = state
            .transcript
            .iter()
            .find_map(|e| match e {
                Entry::PermissionDecision { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .expect("a PermissionDecision entry");
        assert_eq!(decision, "denied with feedback: too risky · waited 12s");
    }

    /// PAIRING for the render test above: the SAME call, but the decision
    /// never reached the operator (`waited_ms: None` -- a pattern/rule/
    /// hook/mode resolution, matching every source but `Operator`). No
    /// `Entry::PermissionDecision` is pushed at all. A wrong implementation
    /// that renders for every `Event::PermissionDecision` regardless of
    /// `waited_ms` would pass the ALLOW test above but fail this one.
    #[test]
    fn unprompted_decision_renders_no_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        let events = vec![
            Event::ToolCallProposed {
                call_id: "tc_3".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls"}),
            },
            Event::PermissionResolved {
                call_id: "tc_3".to_string(),
                decision: PermissionDecisionKind::AllowAlways,
            },
            Event::PermissionDecision {
                call_id: "tc_3".to_string(),
                tool: ToolName::new("bash"),
                decision: conway_core::log::PermissionDecisionRecordKind::Pattern,
                source: conway_core::log::PermissionDecisionSource::Rule,
                waited_ms: None,
                feedback: None,
            },
        ];
        for event in events {
            state.apply(&envelope(session, root, event));
        }

        assert!(
            !state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::PermissionDecision { .. })),
            "a decision that never reached the operator must render no transcript line"
        );
        // The stash is still cleared -- not leaked -- even on the
        // no-render path.
        assert!(!state.permission_decision_pending.contains_key("tc_3"));
    }

    /// PAIRING for the two tests above: a tool call with NO
    /// `Event::PermissionDecision` at all (most calls -- no permission
    /// gate involved) renders no `Entry::PermissionDecision` either. A
    /// wrong implementation that pushed a decision note unconditionally
    /// from `Event::ToolCallProposed` (rather than from the real event)
    /// would pass the two tests above but fail this one.
    #[test]
    fn a_tool_call_with_no_permission_decision_event_renders_no_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_4".to_string(),
                tool: ToolName::new("read"),
                args: serde_json::json!({"path": "/tmp/x"}),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallFinished {
                call_id: "tc_4".to_string(),
                is_error: false,
                preview: "ok".to_string(),
            },
        ));

        assert!(!state
            .transcript
            .iter()
            .any(|e| matches!(e, Entry::PermissionDecision { .. })));
    }

    /// `waited_ms` renders as a HUMAN duration (`"4m 12s"`), never the raw
    /// millisecond count -- a wrong implementation that `format!`ed
    /// `waited_ms` directly would still pass the wording tests above IF
    /// they only checked for a substring match on the minutes/seconds, but
    /// fails this one by asserting the raw number is absent.
    #[test]
    fn wait_duration_renders_as_a_human_duration_not_raw_milliseconds() {
        assert_eq!(
            format_permission_decision_note(Some(PermissionDecisionKind::AllowOnce), 252_000, None),
            "allowed once · waited 4m 12s"
        );
        let short =
            format_permission_decision_note(Some(PermissionDecisionKind::AllowOnce), 3_000, None);
        assert_eq!(short, "allowed once · waited 3s");
        assert!(
            !short.contains("3000"),
            "must never leak the raw millisecond count: {short}"
        );
    }

    // ---- board item 01M1YS4FMJH004D1Y619MTBY7A: `backfill_entries` ----

    fn ts() -> DateTime<Utc> {
        "2026-07-20T00:00:00Z".parse().expect("valid timestamp")
    }

    /// Required test (the backfill's own acceptance criteria): a log with a
    /// user turn, an assistant turn WITH a tool call, that tool's result,
    /// and a child result -- asserting the resulting entries' TEXTS AND
    /// ORDER.
    ///
    /// Catches two distinct wrong implementations at once:
    /// - **Reusing `record_to_event`'s `Assistant` mapping verbatim**
    ///   (mapping the whole record to one bare `TextDelta` of its text
    ///   content, per that function's own disclosed narrowing) would drop
    ///   the `ToolUse` content block entirely -- `entries.len()` would be 3,
    ///   not 4, and the middle entry would carry no proposed tool call at
    ///   all. The `entries.len() == 4` assertion plus the `Entry::Tool`
    ///   match on `entries[2]` both fail against that implementation.
    /// - **Failing to fold the `ToolResultRecord` into the SAME `Entry::
    ///   Tool` pushed by the preceding `ToolUse` block** (e.g. always
    ///   pushing a fresh entry instead of searching backward by `call_id`)
    ///   would yield 5 entries, not 4, and `entries[2]`'s `status` would
    ///   still read `Proposed` rather than `Finished` -- both assertions
    ///   below fail against that implementation too.
    #[test]
    fn backfill_entries_orders_a_user_turn_a_tool_call_its_result_and_a_child_result() {
        let child = AgentId::new();
        let child_session = SessionId::new();
        let model: conway::ModelRef = "test/echo".parse().expect("valid model ref");

        let records = vec![
            conway::LogRecord::UserTurn {
                seq: LogSeq(1),
                ts: ts(),
                text: "list the files".to_string(),
                prov: conway::Provenance::UserPrompt,
            },
            conway::LogRecord::Assistant {
                seq: LogSeq(2),
                ts: ts(),
                content: vec![
                    ContentBlock::Text {
                        text: "sure, checking now".to_string(),
                    },
                    ContentBlock::ToolUse {
                        call_id: "tc_1".to_string(),
                        name: ToolName::new("bash"),
                        arguments: serde_json::json!({"command": "ls"}),
                    },
                ],
                model,
                route_reason: serde_json::json!({}),
                usage: conway::Usage::default(),
                stop: conway::backend::StopReason::EndTurn,
            },
            conway::LogRecord::ToolResultRecord {
                seq: LogSeq(3),
                ts: ts(),
                result: conway_core::content::ToolResult {
                    call_id: "tc_1".to_string(),
                    tool: ToolName::new("bash"),
                    blocks: vec![ContentBlock::Text {
                        text: "one.txt\ntwo.txt".to_string(),
                    }],
                    is_error: false,
                    truncated: None,
                },
            },
            conway::LogRecord::ChildResultRecord {
                seq: LogSeq(4),
                ts: ts(),
                result: conway::AgentResult::new(
                    child,
                    child_session,
                    ResultStatus::Completed,
                    "wrote the report",
                ),
                prov: conway::Provenance::ChildResult { from: child },
            },
        ];

        let entries = backfill_entries(&records);

        assert_eq!(
            entries.len(),
            4,
            "expected exactly one entry per record except the folded tool result: {entries:#?}"
        );
        assert_eq!(entries[0], Entry::User("list the files".to_string()));
        match &entries[1] {
            Entry::Assistant { text, model, .. } => {
                assert_eq!(text.as_str(), "sure, checking now");
                assert_eq!(
                    *model, None,
                    "replayed entries carry no model -- matches Entry::Assistant's own \
                     doc ('None for replayed entries')"
                );
            }
            other => panic!("expected Entry::Assistant at index 1, got {other:?}"),
        }
        match &entries[2] {
            Entry::Tool {
                call_id,
                name,
                status,
                preview,
                ..
            } => {
                assert_eq!(call_id.as_str(), "tc_1");
                assert_eq!(name.as_str(), "bash");
                assert_eq!(
                    *status,
                    ToolStatus::Finished { is_error: false },
                    "the later ToolResultRecord must fold INTO this same entry, not push \
                     a second one"
                );
                assert_eq!(preview.as_str(), "one.txt\ntwo.txt");
            }
            other => panic!("expected Entry::Tool at index 2, got {other:?}"),
        }
        match &entries[3] {
            Entry::Notice { text } => {
                assert_eq!(text, &format!("child {child} finished: wrote the report"));
            }
            other => panic!("expected Entry::Notice at index 3, got {other:?}"),
        }
    }

    /// Required test: a record kind [`push_record`] has no dedicated arm
    /// for (`ContextMask`, chosen as a representative -- `record_to_event`
    /// itself falls back to its own `_ => None` for the identical four
    /// kinds) still produces exactly one entry, naming the kind -- never
    /// silently dropped.
    ///
    /// Catches a wrong implementation whose wildcard arm is `other => {}`
    /// (or omits the arm's push): `entries.len()` would be `0`, not `1`.
    /// Also catches one whose placeholder text does not name which kind was
    /// unrecognized (a bare `"[unrecognized record]"`, say) -- silent about
    /// WHAT was dropped is still a form of the same honesty failure this
    /// item's own acceptance criteria call out.
    #[test]
    fn unknown_record_kind_becomes_a_named_placeholder_not_a_silent_drop() {
        let records = vec![conway::LogRecord::ContextMask {
            seq: LogSeq(1),
            ts: ts(),
            target_seq: LogSeq::ZERO,
            excluded: true,
        }];

        let entries = backfill_entries(&records);

        assert_eq!(
            entries.len(),
            1,
            "an unrecognized record kind must still produce exactly one entry, never zero: \
             {entries:#?}"
        );
        match &entries[0] {
            Entry::PermissionDecision { text, .. } => {
                assert!(
                    text.contains("context_mask"),
                    "the placeholder must name the record's own kind: {text:?}"
                );
            }
            other => panic!("expected a placeholder entry, got {other:?}"),
        }
    }
}
