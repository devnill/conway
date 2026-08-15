//! The focused agent's live activity signal: [`Activity`] itself, the T2
//! braille spinner ([`SPINNER_FRAMES`], [`should_animate`],
//! [`AppState::tick_animation`]) and per-turn elapsed/token-estimate
//! tracking ([`AppState::clear_turn_state`]). `apply`'s `ModelDecision`/
//! `ContextSegmentAdded` arms (T3's serving-model/context-window tracking)
//! stay inline in [`AppState::apply`] -- they are plain field mutations
//! with no standalone method of their own -- but this module's own tests
//! cover that behavior alongside T2's, since both are the status line's
//! "what is the focused agent doing right now" surface.

use super::*;

/// The focused agent's live activity, rendered as the status line's primary
/// "is it working?" signal.
/// Transitions live in [`AppState::apply`], driven by events on the
/// FOCUSED agent's own stream only (`ThinkingDelta`->`Thinking`,
/// `TextDelta`->`Responding`, `ToolCallProposed{tool}`->`RunningTool(name)`
/// -- the name is captured from `Proposed`, not `Started`, which carries
/// only a `call_id` -- `PermissionRequested`->`AwaitingPermission`,
/// `TurnFinished`/`AgentFinished`->`Idle`). Reset to `Idle` whenever the
/// focus itself changes ([`AppState::focus_agent`]) -- a freshly focused
/// agent shows no activity signal until its own next event arrives, rather
/// than carrying over whatever the PREVIOUS focus was doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Thinking,
    Responding,
    RunningTool(String),
    AwaitingPermission,
}

/// The braille spinner frame sequence (T2, 8 TPS animation tick). Advanced by
/// [`AppState::tick_animation`] only while [`AppState::activity`] is not
/// [`Activity::Idle`] (idle terminal stays flat-cost -- no animation tick
/// work, no redraw). The 10-glyph braille cycle is the same one `spinners`-
/// style CLI indicators use.
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Whether `activity` should drive the 125ms animation tick (T2): true for
/// every variant but [`Activity::Idle`]. The app loop's animation-tick arm
/// calls this to decide whether to advance the spinner/frame counters and
/// mark the frame dirty -- an idle terminal is never redrawn by the animation
/// tick, keeping idle cost flat (the 16ms redraw tick still runs but is itself
/// dirty-gated).
pub fn should_animate(activity: &Activity) -> bool {
    !matches!(activity, Activity::Idle)
}

impl AppState {
    /// T2 animation tick (125ms / 8 TPS): advances the braille spinner frame
    /// and the pulse-color index, both wrapping. The caller (the app loop's
    /// animation-tick arm) is responsible for only calling this while
    /// [`should_animate`] is true for [`Self::activity`], so an idle terminal
    /// never pays for animation. The frame index wraps modulo
    /// [`SPINNER_FRAMES`]' length.
    ///
    /// V6 removed the color-pulse half of this. T2 also advanced a palette
    /// index so the glyph and activity word cycled colors on every tick;
    /// that read as strobing rather than as liveness. The advancing frame
    /// already conveys "something is happening" -- adding color motion on
    /// top only competed with it.
    pub fn tick_animation(&mut self) {
        let frames = SPINNER_FRAMES.len();
        if frames != 0 {
            self.spinner_frame = (self.spinner_frame + 1) % frames;
        }
    }

    /// Clears the per-turn timing/token counters (T2). Called whenever
    /// `activity` transitions back to [`Activity::Idle`] -- the working
    /// indicator no longer shows elapsed/running tokens once the turn is
    /// done. The spinner counters themselves are zeroed by [`Self::focus_agent`]
    /// and otherwise left alone on idle (they simply stop advancing, which
    /// is fine -- the renderer only draws them while active).
    pub(super) fn clear_turn_state(&mut self) {
        self.turn_started_at = None;
        self.turn_running_tokens = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::fixtures::{envelope, spawned};
    use conway::{SessionId, ToolName};

    #[test]
    fn new_state_starts_idle_with_zero_usage() {
        let state = AppState::new(AgentId::new());
        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.focused_agent_usage, Usage::default());
    }

    #[test]
    fn thinking_delta_sets_activity_thinking() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::ThinkingDelta {
                text: "hmm".to_string(),
            },
        ));

        assert_eq!(state.activity, Activity::Thinking);
    }

    #[test]
    fn text_delta_sets_activity_responding() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::TextDelta {
                text: "hi".to_string(),
            },
        ));

        assert_eq!(state.activity, Activity::Responding);
    }

    #[test]
    fn tool_call_proposed_sets_activity_running_tool_captured_from_proposed() {
        // The tool name must come from `ToolCallProposed`, not
        // `ToolCallStarted` -- the latter carries only a `call_id`, per this
        // item's own binding note.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({}),
            },
        ));

        assert_eq!(state.activity, Activity::RunningTool("bash".to_string()));
    }

    #[test]
    fn permission_requested_sets_activity_awaiting_permission() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::PermissionRequested {
                call_id: "tc_1".to_string(),
                rendered: "bash: ls".to_string(),
            },
        ));

        assert_eq!(state.activity, Activity::AwaitingPermission);
    }

    #[test]
    fn turn_finished_resets_activity_to_idle_and_live_increments_usage() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.activity = Activity::Responding;

        let usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        };
        state.apply(&envelope(
            session,
            agent,
            Event::TurnFinished {
                usage,
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.focused_agent_usage, usage);

        // A second turn accumulates on top of the first (live-increment,
        // not overwrite).
        state.apply(&envelope(
            session,
            agent,
            Event::TurnFinished {
                usage,
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));
        assert_eq!(state.focused_agent_usage, usage + usage);
    }

    #[test]
    fn agent_finished_resets_activity_to_idle_only_for_the_focused_agent() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let sibling = AgentId::new();
        state.apply(&envelope(session, sibling, spawned(Some(root))));
        state.activity = Activity::Responding;

        // The SIBLING finishing (not the focused root) must not touch
        // `activity`.
        state.apply(&envelope(
            session,
            sibling,
            Event::AgentFinished {
                result: AgentResult::new(sibling, session, ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ));
        assert_eq!(
            state.activity,
            Activity::Responding,
            "an unrelated agent's finish must not reset the focused agent's activity"
        );

        // The focused (root) agent finishing DOES reset it.
        state.apply(&envelope(
            session,
            root,
            Event::AgentFinished {
                result: AgentResult::new(root, session, ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ));
        assert_eq!(state.activity, Activity::Idle);
    }

    /// Bug 2 fix: `TurnStarted` for the FOCUSED
    /// agent, BEFORE any delta arrives, must already mark the activity
    /// indicator as working -- this is the whole point of the fix (the
    /// submit->model-latency window used to show `Idle` with no `TurnStarted`
    /// arm at all).
    #[test]
    fn turn_started_sets_activity_thinking_for_the_focused_agent() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        assert_eq!(state.activity, Activity::Idle);

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));

        assert_eq!(state.activity, Activity::Thinking);
    }

    /// Companion case: a `TurnStarted` for a NON-focused agent (sibling/other
    /// subtree) must not mislabel the focused agent as working.
    #[test]
    fn turn_started_for_a_non_focused_agent_leaves_activity_unchanged() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let other = AgentId::new();
        assert_eq!(state.activity, Activity::Idle);

        state.apply(&envelope(session, other, Event::TurnStarted { turn: 1 }));

        assert_eq!(
            state.activity,
            Activity::Idle,
            "an unrelated agent's TurnStarted must not touch the focused agent's activity"
        );
    }

    /// The render-level companion ('s
    /// harness): `TurnStarted` fed BEFORE any delta must already show a
    /// working phrase on screen, not "idle" -- this is the actual bug report
    /// ("I only saw ready and awaiting permission"), reproduced through the
    /// real `view::draw` render pass rather than only asserting on the
    /// `Activity` enum directly.
    #[test]
    fn turn_started_renders_a_working_status_before_any_delta_arrives() {
        use crate::tui::test_support::render_text;

        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));

        let screen = render_text(&state, 80, 24).to_lowercase();
        assert!(
            screen.contains("thinking") || screen.contains("working"),
            "expected a working/thinking status phrase right after TurnStarted, got:\n{screen}"
        );
        assert!(
            !screen.contains("idle"),
            "must not still show idle right after TurnStarted, got:\n{screen}"
        );
    }

    #[test]
    fn focus_agent_resets_activity_and_usage() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.activity = Activity::Thinking;
        state.focused_agent_usage = Usage {
            input_tokens: 42,
            ..Usage::default()
        };

        state.focus_agent(AgentId::new());

        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.focused_agent_usage, Usage::default());
    }

    #[test]
    fn spinner_frame_cycles_the_braille_sequence_and_wraps() {
        // Advance one full cycle plus one: the frame index must wrap back to
        // 1 (frame 0 is the starting position, so a full `len` ticks lands
        // back on 0, and one more lands on 1).
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.spinner_frame, 0);
        let n = SPINNER_FRAMES.len();
        for _ in 0..n {
            state.tick_animation();
        }
        assert_eq!(state.spinner_frame, 0, "frame must wrap modulo {}", n);
        state.tick_animation();
        assert_eq!(state.spinner_frame, 1);
        // The glyph lookup itself never panics on any in-range frame.
        let glyph = SPINNER_FRAMES[state.spinner_frame % SPINNER_FRAMES.len()];
        assert!(SPINNER_FRAMES.contains(&glyph));
    }

    #[test]
    fn should_animate_is_false_for_idle_true_otherwise() {
        assert!(!should_animate(&Activity::Idle));
        assert!(should_animate(&Activity::Thinking));
        assert!(should_animate(&Activity::Responding));
        assert!(should_animate(&Activity::RunningTool("bash".to_string())));
        assert!(should_animate(&Activity::AwaitingPermission));
    }

    #[test]
    fn turn_started_records_the_start_instant_and_resets_running_tokens() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.turn_running_tokens = 999;

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));

        assert!(
            state.turn_started_at.is_some(),
            "TurnStarted for the focused agent must stamp the start instant"
        );
        assert_eq!(
            state.turn_running_tokens, 0,
            "a new turn must reset the new-segment-token count"
        );
    }

    #[test]
    fn turn_started_for_a_non_focused_agent_does_not_stamp_or_reset() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let other = AgentId::new();

        state.apply(&envelope(session, other, Event::TurnStarted { turn: 1 }));

        assert!(
            state.turn_started_at.is_none(),
            "a non-focused agent's TurnStarted must not stamp the focused agent's clock"
        );
    }

    #[test]
    fn context_segment_added_accumulates_running_tokens_for_the_focused_agent() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        // A turn must be in flight for the accumulator to engage.
        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        assert_eq!(state.turn_running_tokens, 0);

        state.apply(&envelope(
            session,
            agent,
            Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 120,
            },
        ));
        state.apply(&envelope(
            session,
            agent,
            Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 200,
            },
        ));

        assert_eq!(state.turn_running_tokens, 320);
    }

    #[test]
    fn context_segment_added_outside_a_turn_does_not_accumulate() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        // No TurnStarted yet -- `turn_started_at` is None.
        state.apply(&envelope(
            session,
            agent,
            Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 120,
            },
        ));
        assert_eq!(state.turn_running_tokens, 0);
    }

    #[test]
    fn turn_finished_clears_the_turn_state() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        state.apply(&envelope(
            session,
            agent,
            Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 50,
            },
        ));
        assert!(state.turn_started_at.is_some());
        assert_eq!(state.turn_running_tokens, 50);

        state.apply(&envelope(
            session,
            agent,
            Event::TurnFinished {
                usage: Usage::default(),
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        assert!(state.turn_started_at.is_none());
        assert_eq!(state.turn_running_tokens, 0);
        assert_eq!(state.activity, Activity::Idle);
    }

    #[test]
    fn focus_agent_resets_the_spinner_and_turn_state() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.spinner_frame = 5;
        state.turn_started_at = Some(Instant::now());
        state.turn_running_tokens = 42;
        state.activity = Activity::Responding;

        state.focus_agent(child);

        assert_eq!(state.spinner_frame, 0);
        assert!(state.turn_started_at.is_none());
        assert_eq!(state.turn_running_tokens, 0);
        assert_eq!(state.activity, Activity::Idle);
    }

    fn model_decision_env(agent: AgentId, chosen: &str) -> Envelope {
        Envelope {
            seq: 0,
            ts: chrono::Utc::now(),
            session: SessionId::new(),
            agent,
            event: Event::ModelDecision {
                role: conway::RoleAlias::new("coder"),
                chosen: chosen.parse().expect("valid ModelRef"),
                reason: conway::RoutingReason::PinnedByApi,
                attempt: 0,
            },
        }
    }

    fn context_segment_env(agent: AgentId, tokens_est: u32) -> Envelope {
        Envelope {
            seq: 0,
            ts: chrono::Utc::now(),
            session: SessionId::new(),
            agent,
            event: Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est,
            },
        }
    }

    #[test]
    fn model_decision_sets_focused_model_and_max_context() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state
            .model_max_context
            .insert("anthropic/claude-sonnet-4-6".to_string(), 200_000);

        state.apply(&model_decision_env(root, "anthropic/claude-sonnet-4-6"));

        assert_eq!(
            state.focused_model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(state.focused_model_max_context, Some(200_000));
    }

    #[test]
    fn model_decision_with_unknown_model_leaves_max_context_none() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        // Metadata has a different model; the chosen one is unknown.
        state
            .model_max_context
            .insert("anthropic/claude-haiku-4-5".to_string(), 32_768);

        state.apply(&model_decision_env(root, "anthropic/claude-sonnet-4-6"));

        assert_eq!(
            state.focused_model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert!(
            state.focused_model_max_context.is_none(),
            "unknown model -> no max context (renderer falls back to raw tokens)"
        );
    }

    #[test]
    fn model_decision_for_non_focused_agent_does_not_touch_focused_model() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.focus_agent(child);
        state
            .model_max_context
            .insert("anthropic/claude-sonnet-4-6".to_string(), 200_000);

        // A ModelDecision on the root (not focused) must not overwrite the
        // focused child's model fields.
        state.apply(&model_decision_env(root, "anthropic/claude-sonnet-4-6"));

        assert!(
            state.focused_model.is_none(),
            "non-focused ModelDecision must not set focused_model"
        );
    }

    #[test]
    fn context_segment_added_accumulates_cumulative_ctx_tokens_across_turns() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        // Turn 1.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::TurnStarted { turn: 1 },
        ));
        state.apply(&context_segment_env(root, 1_000));
        state.apply(&context_segment_env(root, 500));
        // Turn 1 ends.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::TurnFinished {
                usage: Usage::default(),
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));
        assert_eq!(state.turn_running_tokens, 0, "per-turn counter resets");
        assert_eq!(
            state.focused_ctx_tokens, 1_500,
            "cumulative counter persists across turns"
        );

        // Turn 2 -- only genuinely new segments fire.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::TurnStarted { turn: 2 },
        ));
        state.apply(&context_segment_env(root, 200));
        assert_eq!(state.turn_running_tokens, 200, "per-turn counter restarts");
        assert_eq!(
            state.focused_ctx_tokens, 1_700,
            "cumulative counter keeps growing across turns"
        );
    }

    #[test]
    fn context_segment_added_dedups_cumulative_ctx_tokens_by_segment_id() {
        // T3 code-review fix 1: a non-keep-alive focused agent's second
        // run re-emits `ContextSegmentAdded` for EVERY existing context
        // segment (its fresh `AgentLoop`'s local `seen_segments` is
        // empty). The renderer must dedup by `SegmentId` so the
        // cumulative `focused_ctx_tokens` counts each segment ONCE per
        // focused session, not once per run.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let segment = conway_core::ids::SegmentId::new();

        // First emission of `segment` -- counted.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::ContextSegmentAdded {
                segment,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 1_000,
            },
        ));
        assert_eq!(state.focused_ctx_tokens, 1_000);

        // Re-emit the SAME segment id (simulating the second run of a
        // non-keep-alive agent re-emitting its existing context). The
        // cumulative figure must NOT double-count it.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::ContextSegmentAdded {
                segment,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 1_000,
            },
        ));
        assert_eq!(
            state.focused_ctx_tokens, 1_000,
            "re-emitted segment id must not double-count into focused_ctx_tokens"
        );

        // A DISTINCT segment id is genuinely new -- counted.
        let other = conway_core::ids::SegmentId::new();
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::ContextSegmentAdded {
                segment: other,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 250,
            },
        ));
        assert_eq!(
            state.focused_ctx_tokens, 1_250,
            "a distinct segment id is counted alongside the deduped one"
        );
    }

    #[test]
    fn focus_agent_resets_focused_seen_segments() {
        // T3 code-review fix 1: `focused_seen_segments` is per focused
        // session -- a freshly focused agent starts with an empty
        // seen-set, so a segment id seen under the PREVIOUS focus is
        // correctly counted again under the new focus (it is a different
        // session's dedup window).
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        let segment = conway_core::ids::SegmentId::new();
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::ContextSegmentAdded {
                segment,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 800,
            },
        ));
        assert_eq!(state.focused_ctx_tokens, 800);
        assert!(
            state.focused_seen_segments.contains(&segment),
            "segment id recorded for the root focus"
        );

        state.focus_agent(child);
        assert!(
            state.focused_seen_segments.is_empty(),
            "focus switch clears the seen-set"
        );
        assert_eq!(state.focused_ctx_tokens, 0);

        // The same segment id, re-emitted under the new focus, counts
        // again -- it is new to THIS focused session's dedup window.
        state.apply(&envelope(
            SessionId::new(),
            child,
            Event::ContextSegmentAdded {
                segment,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 800,
            },
        ));
        assert_eq!(
            state.focused_ctx_tokens, 800,
            "segment id counts again under the new focused session"
        );
    }

    #[test]
    fn focus_agent_resets_focused_model_and_ctx_tokens() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state
            .model_max_context
            .insert("anthropic/claude-sonnet-4-6".to_string(), 200_000);
        state.apply(&model_decision_env(root, "anthropic/claude-sonnet-4-6"));
        state.focused_ctx_tokens = 5_000;

        state.focus_agent(child);

        assert!(
            state.focused_model.is_none(),
            "focus switch resets focused_model"
        );
        assert!(
            state.focused_model_max_context.is_none(),
            "focus switch resets focused_model_max_context"
        );
        assert_eq!(
            state.focused_ctx_tokens, 0,
            "focus switch resets focused_ctx_tokens"
        );
    }
}
