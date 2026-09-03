//! Proactive, model-facing runway notices (the fix for a real incident: a
//! long-running `keep_alive` session silently hit its step budget mid-real-
//! work, with zero warning, and the operator lost the ability to continue a
//! real conversation with no idea why).
//!
//! Before this module, `AgentLoop::check_budget` computed every turn how
//! close each budget dimension was to tripping, and `ContextReport` computed
//! how full the context window was -- but neither number was ever told to
//! the MODEL. The human could see it (`/context`, the TUI status line); the
//! model found out only when it was cut off. This module is the other half:
//! [`notes_for_turn`] is called once per turn from
//! `AgentLoop::run_inner` (after that turn's tool
//! results are appended, using the SAME `conway_core::provenance::
//! ContextReport` that turn's request was actually built from -- see that
//! call site's own comment for why re-estimating a second time is neither
//! necessary nor honest here) and returns zero or more note texts, each
//! persisted as a `LogRecord::SystemNote { reason: "runway", .. }` -- the
//! exact mechanism `conway.stepguard`'s `repeated_step` note and the
//! result-contract-violation note already use, so this ships through both
//! renderers (TUI, jsonl) for free: neither treats `SystemNote` specially by
//! `reason`.
//!
//! **No change to what trips a budget.** `AgentLoop::check_budget` is
//! untouched; this module only ever informs, never enforces. **No note when
//! the window is unknown** ([`TurnInputs::max_context_tokens`] is `None`)
//! -- say nothing rather than guess.
//!
//! ## Headroom arithmetic: one implementation, reused
//!
//! The window-fill percentage is `RequiredCaps::total_required(est_tokens)`
//! (`conway_core::capabilities`, the SAME helper `AgentLoop::route_and_attempt`
//! already builds a `RequiredCaps` around every turn) divided by the
//! resolved model's `max_context_tokens` -- never a second, independently
//! written `est + headroom` sum.
//!
//! ## The "is the window even known" gap, disclosed
//!
//! `conway-runtime` depends on `conway-core` only (see this crate's
//! `Cargo.toml`) -- it cannot see `conway-plugin-backends::capabilities::
//! ContextTokensSource`, the real signal for "this ceiling is a sourced
//! fact vs. an internal admission-safety clamp nobody actually declared"
//! (that crate's own `Backend::capabilities` doc). `conway_core::
//! capabilities::Capabilities::max_context_tokens` is a plain, mandatory
//! `u32` with no room for "unknown" today, and widening it would ripple
//! through the ~40 call sites that construct a `Capabilities` by field
//! literal across the workspace -- out of this item's scope. Until that
//! signal is threaded all the way down through `conway_core::ports::Backend`
//! (a real, larger follow-up, not done here), [`crate::attempt::
//! AttemptEngine`] reports [`TurnInputs::max_context_tokens`] as `None`
//! only for the one sentinel a real dialect never legitimately returns --
//! `u32::MAX` -- see [`crate::attempt::AttemptOutcome::max_context_tokens`]'s
//! own doc for the exact conversion. Every real backend today (Anthropic,
//! every `openai-compat` dialect, verified or not) resolves to a real,
//! finite number, so in production this module's window note fires
//! whenever a route is chosen at all; the `u32::MAX` seam exists so a test
//! double -- and a future, properly-wired `Unverified` signal -- can turn it
//! off honestly rather than the module having to guess.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use conway_core::agent::Budget;
use conway_core::capabilities::RequiredCaps;
use conway_core::ids::ModelRef;

/// Percent-of-window checkpoints the model is told about, once each, per
/// session -- never re-announced once crossed (a context window only grows
/// within a run, so there is no "re-crossing" a lower one to warn about
/// again).
pub const WINDOW_THRESHOLDS: [u32; 3] = [50, 75, 90];

/// Fraction of a budget dimension's ceiling that trips ONE runway note for
/// that dimension -- distinct from, and always earlier than,
/// `AgentLoop::check_budget`'s hard trip at 100%.
pub const BUDGET_WARN_FRACTION: f64 = 0.8;

/// Every budget dimension name this module ever reports, in the fixed order
/// `budget_notes` checks them.
const MAX_STEPS: &str = "max_steps";
const MAX_TOOL_CALLS: &str = "max_tool_calls";
const MAX_TOKENS: &str = "max_tokens";
const DEADLINE: &str = "deadline";

/// Per-agent-run bookkeeping: which window-fill thresholds and which budget
/// dimensions have already produced a runway note, so [`notes_for_turn`]
/// emits each crossing at most once. Lives on `AgentLoop`'s per-run
/// `LoopState`, exactly like `turn_steps`/`tool_calls` -- `Default` starts
/// both sets empty.
#[derive(Clone, Debug, Default)]
pub struct RunwayTracker {
    window_crossed: BTreeSet<u32>,
    budget_crossed: BTreeSet<&'static str>,
}

impl RunwayTracker {
    /// Clears bookkeeping for the two TURN-SCOPED budget dimensions
    /// (`max_steps`, `max_tool_calls` -- the same two `LoopState::turn_steps`/
    /// `tool_calls` themselves reset for a `keep_alive` agent, see those
    /// fields' own docs) at a keep-alive user-turn boundary. Window-fill and
    /// the session-lifetime dimensions (`max_tokens`, `deadline`) are left
    /// untouched: a context window does not shrink, and a session-lifetime
    /// ceiling is not reset just because one user turn ended.
    pub fn reset_turn_scoped(&mut self) {
        self.budget_crossed.remove(MAX_STEPS);
        self.budget_crossed.remove(MAX_TOOL_CALLS);
    }
}

/// Everything [`notes_for_turn`] needs to decide what (if anything) to tell
/// the model about this turn's own standing -- every number the caller
/// already resolved for its own purposes this turn, never re-derived here.
pub struct TurnInputs<'a> {
    /// The just-assembled turn's own `ContextReport::total_tokens_est` --
    /// the request actually sent, not a fresh re-estimate (see this
    /// module's own doc).
    pub total_tokens_est: u32,
    /// This turn's resolved headroom (`AgentLoop::resolve_headroom`'s
    /// result), the same value `RouteRequest`/`AttemptRequest` both used.
    pub headroom: u32,
    /// The routed model's context window, when known -- see this module's
    /// own doc on why `None` is the honest answer more often than a fully
    /// wired implementation would prefer.
    pub max_context_tokens: Option<u32>,
    pub model: &'a ModelRef,
    pub budget: &'a Budget,
    /// Gates whether `max_steps`/`max_tool_calls` read as "this turn" (a
    /// `keep_alive` agent, turn-scoped) or "this session" in the rendered
    /// text -- mirrors `AgentLoop::check_budget`'s own `keep_alive` branch.
    pub keep_alive: bool,
    /// `LoopState::turn_steps` for a `keep_alive` agent, `LoopState::turn`
    /// otherwise -- exactly the value `check_budget` itself gates
    /// `max_steps` on.
    pub steps_this_turn: u32,
    pub tool_calls: u32,
    /// `usage.input_tokens + usage.output_tokens`, accrued so far.
    pub tokens_spent: u64,
    /// When this agent's run began -- the reference point `budget.deadline`
    /// (an absolute timestamp) is measured against, since `Budget` itself
    /// carries no start time or duration.
    pub started_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
}

/// Computes, and records in `tracker`, every runway note this turn's
/// numbers newly justify. Called once per turn; returns an empty `Vec` on
/// the overwhelmingly common turn where nothing has newly crossed a
/// threshold.
pub fn notes_for_turn(tracker: &mut RunwayTracker, inputs: &TurnInputs<'_>) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(note) = window_note(tracker, inputs) {
        notes.push(note);
    }
    notes.extend(budget_notes(tracker, inputs));
    notes
}

fn window_note(tracker: &mut RunwayTracker, inputs: &TurnInputs<'_>) -> Option<String> {
    let max = inputs.max_context_tokens?;
    if max == 0 {
        return None;
    }
    let required = RequiredCaps {
        headroom_tokens: inputs.headroom,
        ..RequiredCaps::default()
    }
    .total_required(inputs.total_tokens_est);
    let pct = ((u64::from(required) * 100) / u64::from(max)).min(u64::from(u32::MAX)) as u32;

    // The highest threshold this turn's `pct` clears that has not already
    // been announced -- so a turn that jumps straight from 20% to 95% (one
    // huge tool result) still gets exactly one note, not three.
    let threshold = WINDOW_THRESHOLDS
        .iter()
        .rev()
        .find(|&&t| pct >= t && !tracker.window_crossed.contains(&t))
        .copied()?;
    for &t in WINDOW_THRESHOLDS.iter().filter(|&&t| t <= threshold) {
        tracker.window_crossed.insert(t);
    }

    Some(format!(
        "runway: context window {pct}% full ({used}k of {max_k}k tokens est., {model}). Fork \
         the remaining exploration to a child and keep only its distillate; do not accumulate \
         large tool results inline.",
        used = compact_k(inputs.total_tokens_est),
        max_k = compact_k(max),
        model = inputs.model,
    ))
}

/// One budget dimension's current standing, resolved by the caller
/// (`budget_notes`, below) exactly as `AgentLoop::check_budget` resolves
/// the same numbers -- `limit: None` is this fn's own "0/None = skip"
/// normalization, already applied by the time a `BudgetDim` exists.
struct BudgetDim {
    name: &'static str,
    used: u64,
    limit: Option<u64>,
    limit_key: String,
    /// Whether this dimension is turn-scoped for a `keep_alive` agent
    /// (`max_steps`/`max_tool_calls`, reset at the keep-alive boundary) or
    /// always session-lifetime (`max_tokens`).
    turn_scoped: bool,
}

fn budget_notes(tracker: &mut RunwayTracker, inputs: &TurnInputs<'_>) -> Vec<String> {
    let mut notes = Vec::new();

    let dims = [
        BudgetDim {
            name: MAX_STEPS,
            used: u64::from(inputs.steps_this_turn),
            limit: (inputs.budget.max_steps > 0).then_some(u64::from(inputs.budget.max_steps)),
            limit_key: format!("max_steps={}", inputs.budget.max_steps),
            turn_scoped: true,
        },
        BudgetDim {
            name: MAX_TOOL_CALLS,
            used: u64::from(inputs.tool_calls),
            limit: inputs
                .budget
                .max_tool_calls
                .filter(|&v| v > 0)
                .map(u64::from),
            limit_key: format!(
                "max_tool_calls={}",
                inputs.budget.max_tool_calls.unwrap_or(0)
            ),
            turn_scoped: true,
        },
        BudgetDim {
            name: MAX_TOKENS,
            used: inputs.tokens_spent,
            limit: inputs.budget.max_tokens.filter(|&v| v > 0).map(u64::from),
            limit_key: format!("max_tokens={}", inputs.budget.max_tokens.unwrap_or(0)),
            turn_scoped: false,
        },
    ];

    for dim in dims {
        let Some(limit) = dim.limit else { continue };
        if tracker.budget_crossed.contains(dim.name) {
            continue;
        }
        let warn_at = (limit as f64 * BUDGET_WARN_FRACTION) as u64;
        if dim.used < warn_at {
            continue;
        }
        tracker.budget_crossed.insert(dim.name);
        let scope = if dim.turn_scoped && inputs.keep_alive {
            "this turn"
        } else {
            "this session"
        };
        let used = dim.used;
        let name = dim.name;
        let limit_key = dim.limit_key;
        notes.push(format!(
            "runway: {used} of {limit} {name} used {scope} ({limit_key}). Wrap up or report now."
        ));
    }

    if let Some(deadline) = inputs.budget.deadline {
        if !tracker.budget_crossed.contains(DEADLINE) {
            let total_secs = (deadline - inputs.started_at).num_seconds().max(0) as u64;
            let elapsed_secs = (inputs.now - inputs.started_at).num_seconds().max(0) as u64;
            if total_secs > 0 {
                let warn_at = (total_secs as f64 * BUDGET_WARN_FRACTION) as u64;
                if elapsed_secs >= warn_at {
                    tracker.budget_crossed.insert(DEADLINE);
                    notes.push(format!(
                        "runway: {elapsed_secs} of {total_secs} seconds used this session \
                         (deadline={deadline}). Wrap up or report now."
                    ));
                }
            }
        }
    }

    notes
}

/// Compact token-count formatting matching `conway-cli`'s own
/// `view/status.rs::compact_tokens` in shape (independent copy: this crate
/// cannot depend on `conway-cli`, the dependency runs the other way) --
/// `< 1000` renders as-is, `>= 1000` renders as `{k}.{tenths}k`.
fn compact_k(n: u32) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = n / 1000;
    let tenths = (n % 1000) / 100;
    format!("{k}.{tenths}k")
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::ids::{BackendId, ModelId};

    fn model() -> ModelRef {
        ModelRef {
            backend: BackendId::new("b"),
            model: ModelId::new("m"),
        }
    }

    fn budget(max_steps: u32) -> Budget {
        Budget {
            max_steps,
            deadline: None,
            max_tokens: None,
            max_tool_calls: None,
        }
    }

    fn base_inputs<'a>(model: &'a ModelRef, budget: &'a Budget) -> TurnInputs<'a> {
        let now = Utc::now();
        TurnInputs {
            total_tokens_est: 0,
            headroom: 0,
            max_context_tokens: None,
            model,
            budget,
            keep_alive: true,
            steps_this_turn: 0,
            tool_calls: 0,
            tokens_spent: 0,
            started_at: now,
            now,
        }
    }

    #[test]
    fn window_note_fires_once_per_newly_crossed_threshold() {
        let model = model();
        let budget = budget(0);
        let mut tracker = RunwayTracker::default();

        // 3_200 / 4_000 = 80% -- crosses 50 and 75 in one call.
        let mut inputs = base_inputs(&model, &budget);
        inputs.total_tokens_est = 3_200;
        inputs.max_context_tokens = Some(4_000);
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert_eq!(notes.len(), 1, "one note even though two thresholds cleared at once");
        assert!(notes[0].contains("80%"), "{}", notes[0]);

        // Same size again: no NEW threshold crossed (80% < 90%, and 50/75
        // are already recorded) -- no note.
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert!(notes.is_empty(), "must not repeat an already-announced threshold: {notes:?}");

        // A later turn crossing 90% DOES get its own note -- a higher
        // threshold is a genuinely new crossing, not a repeat.
        inputs.total_tokens_est = 3_700; // 92.5%
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("92%"), "{}", notes[0]);
    }

    #[test]
    fn unknown_window_never_produces_a_note() {
        let model = model();
        let budget = budget(0);
        let mut tracker = RunwayTracker::default();
        let mut inputs = base_inputs(&model, &budget);
        inputs.total_tokens_est = 1_000_000;
        inputs.max_context_tokens = None;
        assert!(notes_for_turn(&mut tracker, &inputs).is_empty());
    }

    #[test]
    fn max_steps_zero_skips_the_dimension() {
        let model = model();
        let budget = budget(0);
        let mut tracker = RunwayTracker::default();
        let mut inputs = base_inputs(&model, &budget);
        inputs.steps_this_turn = 1_000;
        assert!(notes_for_turn(&mut tracker, &inputs).is_empty());
    }

    /// Acceptance criterion 4: turn-scoped bookkeeping resets at a
    /// keep-alive user-turn boundary (a second user turn can trigger the
    /// same 80%-of-`max_steps` note again), but nothing resets mid-turn.
    #[test]
    fn max_steps_note_resets_at_keep_alive_boundary_not_within_a_turn() {
        let model = model();
        let budget = budget(5);
        let mut tracker = RunwayTracker::default();
        let mut inputs = base_inputs(&model, &budget);

        inputs.steps_this_turn = 4; // 80% of 5
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("4 of 5"), "{}", notes[0]);
        assert!(notes[0].contains(MAX_STEPS), "{}", notes[0]);

        // Still within the same turn/user-turn: repeating (or exceeding)
        // the same step count must not re-fire.
        inputs.steps_this_turn = 5;
        assert!(notes_for_turn(&mut tracker, &inputs).is_empty());

        // Keep-alive user-turn boundary: `AgentLoop::run_inner` calls this
        // exactly where it resets `turn_steps`/`tool_calls`.
        tracker.reset_turn_scoped();
        inputs.steps_this_turn = 4;
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert_eq!(notes.len(), 1, "a fresh user turn can trip the same dimension again");
    }

    #[test]
    fn deadline_note_fires_at_80_percent_elapsed() {
        let model = model();
        let started = Utc::now() - chrono::Duration::seconds(80);
        let deadline = started + chrono::Duration::seconds(100);
        let budget = Budget {
            max_steps: 0,
            deadline: Some(deadline),
            max_tokens: None,
            max_tool_calls: None,
        };
        let mut tracker = RunwayTracker::default();
        let mut inputs = base_inputs(&model, &budget);
        inputs.started_at = started;
        inputs.now = started + chrono::Duration::seconds(80);

        let notes = notes_for_turn(&mut tracker, &inputs);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("80 of 100 seconds"), "{}", notes[0]);

        // No repeat.
        assert!(notes_for_turn(&mut tracker, &inputs).is_empty());
    }
}
