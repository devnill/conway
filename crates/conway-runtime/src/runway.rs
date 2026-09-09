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
//! ## The "is the window even known" gap, closed
//!
//! `conway-runtime` depends on `conway-core` only (see this crate's
//! `Cargo.toml`); it used to be unable to see `conway-plugin-backends::
//! capabilities::ContextTokensSource`, the real signal for "this ceiling is
//! a sourced fact vs. an internal admission-safety clamp nobody actually
//! declared" (that crate's own `Backend::capabilities` doc). The hosted
//! OpenAI-compatible models item closed this by moving that enum down into
//! `conway-core::capabilities` (this crate's one dependency) and adding
//! `conway_core::ports::Backend::context_window_source` -- a provided
//! trait method beside `Backend::capabilities` itself, so
//! [`crate::attempt::AttemptEngine`] (which already holds the `Arc<dyn
//! Backend>` `capabilities()` came from) reads it directly, no new
//! dependency and no second resolution. [`TurnInputs::
//! max_context_tokens_source`] carries that value into `window_note`,
//! which now names the provenance in its own text whenever a floor
//! governs -- see that function's doc for exactly which two
//! `ContextTokensSource` variants trigger the "assumed" wording.
//!
//! `TurnInputs::max_context_tokens` itself stays `Option<u32>`, still keyed
//! off the `u32::MAX` sentinel a real dialect never legitimately returns
//! (`conway_runtime::attempt::window_of`'s own doc) -- that answers a
//! DIFFERENT question ("is there a number to report at all", still `None`
//! only for a test double declaring it has nothing) from `
//! max_context_tokens_source` ("how much can the number be trusted",
//! always present, `Unverified` being the honest floor answer). A real
//! backend's `Unverified`-sourced floor still carries a real, usable
//! `max_context_tokens` -- the two fields are orthogonal, not a fallback
//! chain of each other.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use conway_core::agent::Budget;
use conway_core::capabilities::{ContextTokensSource, RequiredCaps};
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
    /// `max_context_tokens`'s provenance (hosted OpenAI-compatible models
    /// item) -- `AttemptOutcome::max_context_tokens_source`, read straight
    /// from `Backend::context_window_source`. Independent of whether
    /// `max_context_tokens` itself is `Some`/`None`: a test double can set
    /// this to anything regardless of its own `max_context_tokens` choice,
    /// though every production caller's two fields describe the same
    /// resolved `Capabilities` value and therefore agree in practice.
    pub max_context_tokens_source: ContextTokensSource,
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

/// One note [`notes_for_turn`] produced this turn, tagged with what kind of
/// crossing it reports. Introduced by board item A5.6 so a caller can tell
/// a context-window-fill note (self-facing only -- there is no "parent" for
/// a context window to notify) apart from a BUDGET-dimension crossing
/// (`max_steps`/`max_tool_calls`/`max_tokens`/`deadline`), which A5.6 also
/// forwards to the agent's parent (`AgentMessage::BudgetNotice`) and to the
/// live event stream (`Event::BudgetWarning`) -- WITHOUT re-deriving which
/// notes are which from the rendered `text` (string-matching a human
/// sentence to recover a fact this module already knows at the point it
/// formats it would be exactly the kind of second, drifting implementation
/// this module's own doc forbids).
#[derive(Clone, Debug, PartialEq)]
pub struct RunwayNote {
    /// The exact model-facing sentence, unchanged from every caller that
    /// existed before this struct did -- `notes_for_turn`'s callers that
    /// only want the text (persisting it as the model-facing `SystemNote`)
    /// read this field and nothing else.
    pub text: String,
    pub kind: RunwayNoteKind,
}

/// Which threshold `RunwayNote` reports crossing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunwayNoteKind {
    /// A context-window-fill threshold (`WINDOW_THRESHOLDS`) -- always
    /// self-facing; `RunwayNoteKind::limit_key` is always `None` for this
    /// variant, since window-fill has no `Budget` dimension name to carry.
    Window,
    /// A `Budget` dimension crossed [`BUDGET_WARN_FRACTION`]. `limit_key`
    /// names the exact dimension (e.g. `"max_steps=5"`, the same string
    /// `AgentResult::BudgetExceeded::limit` uses for the SAME dimension's
    /// hard trip) -- what `Event::BudgetWarning::limit` and
    /// `AgentMessage::BudgetNotice` both need and would otherwise have to
    /// re-parse out of `text`.
    Budget { limit_key: String },
}

impl RunwayNote {
    /// `true` for [`RunwayNoteKind::Budget`] -- the ONLY kind A5.6 forwards
    /// to a parent / the live event stream. `false` for `Window`: a context
    /// window has no owner outside this agent to notify, and forwarding a
    /// window-fill note to a parent would misrepresent it as the CHILD's
    /// own resource pressure, which it partly is not (the parent has its
    /// own, entirely separate window).
    pub fn is_budget(&self) -> bool {
        matches!(self.kind, RunwayNoteKind::Budget { .. })
    }

    /// The bare `"<key>=<n>"` this note reports, when it has one
    /// (`RunwayNoteKind::Budget` only) -- `Event::BudgetWarning::limit`'s
    /// exact source.
    pub fn limit_key(&self) -> Option<&str> {
        match &self.kind {
            RunwayNoteKind::Budget { limit_key } => Some(limit_key.as_str()),
            RunwayNoteKind::Window => None,
        }
    }
}

/// Computes, and records in `tracker`, every runway note this turn's
/// numbers newly justify. Called once per turn; returns an empty `Vec` on
/// the overwhelmingly common turn where nothing has newly crossed a
/// threshold.
pub fn notes_for_turn(tracker: &mut RunwayTracker, inputs: &TurnInputs<'_>) -> Vec<RunwayNote> {
    let mut notes = Vec::new();
    if let Some(text) = window_note(tracker, inputs) {
        notes.push(RunwayNote {
            text,
            kind: RunwayNoteKind::Window,
        });
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

    let max_k = compact_k(max);
    let provenance = provenance_clause(inputs.max_context_tokens_source, &max_k);

    Some(format!(
        "runway: context window {pct}% full ({used} of {max_k} tokens est.{provenance}, \
         {model}). Fork the remaining exploration to a child and keep only its distillate; do \
         not accumulate large tool results inline.",
        used = compact_k(inputs.total_tokens_est),
        model = inputs.model,
    ))
}

/// The provenance clause `window_note` appends to its own message when
/// `source` is a FLOOR, not a real declared fact -- GP-14 (declaration
/// honesty): a floor must say so everywhere it is shown, and the runway
/// notice is model-facing prose an operator reads over the model's
/// shoulder, not exempt from that rule just because its audience is mixed.
/// Empty string for every other source (`Override`/`Metadata`/`Probed`): a
/// real declared or discovered window needs no caveat.
///
/// **`DialectDefaultFloor` also gets the clause,** not only `Unverified` --
/// unlike `ContextTokensSource`'s own doc (which frames `DialectDefaultFloor`
/// as "not invented, a real per-provider figure"), THIS specific number is
/// still not a fact about the routed MODEL, only about the provider in
/// general (`openai`'s `128_000`, Anthropic's `200_000`) -- an operator
/// reading a per-turn window-fill note about one specific model deserves
/// the same "this may not be this model's real ceiling" caveat either way.
fn provenance_clause(source: ContextTokensSource, max_k: &str) -> String {
    match source {
        ContextTokensSource::DialectDefaultFloor | ContextTokensSource::Unverified => {
            // `max_k` ALREADY carries its own `k` (`compact_k(32_768)` is
            // "32.7k"). Appending another produced "32.7kk" in real
            // sessions long after the identical bug was fixed in
            // `window_note`'s own template above -- the first fix corrected
            // the one call site it was reported against and left this one,
            // which is why the paired test below now covers BOTH.
            format!(", {max_k} assumed -- set the real window in .conway/models.json")
        }
        _ => String::new(),
    }
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

fn budget_notes(tracker: &mut RunwayTracker, inputs: &TurnInputs<'_>) -> Vec<RunwayNote> {
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
        notes.push(RunwayNote {
            text: format!(
                "runway: {used} of {limit} {name} used {scope} ({limit_key}). Wrap up or \
                 report now."
            ),
            kind: RunwayNoteKind::Budget { limit_key },
        });
    }

    if let Some(deadline) = inputs.budget.deadline {
        if !tracker.budget_crossed.contains(DEADLINE) {
            let total_secs = (deadline - inputs.started_at).num_seconds().max(0) as u64;
            let elapsed_secs = (inputs.now - inputs.started_at).num_seconds().max(0) as u64;
            if total_secs > 0 {
                let warn_at = (total_secs as f64 * BUDGET_WARN_FRACTION) as u64;
                if elapsed_secs >= warn_at {
                    tracker.budget_crossed.insert(DEADLINE);
                    notes.push(RunwayNote {
                        text: format!(
                            "runway: {elapsed_secs} of {total_secs} seconds used this session \
                             (deadline={deadline}). Wrap up or report now."
                        ),
                        kind: RunwayNoteKind::Budget {
                            limit_key: format!("deadline={deadline}"),
                        },
                    });
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
            // A real, declared window by default -- tests that specifically
            // want the "assumed floor" wording set this explicitly (see
            // `window_note_names_the_floor_as_assumed_and_never_does_for_a_declared_window`).
            max_context_tokens_source: ContextTokensSource::Override,
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
        assert_eq!(
            notes.len(),
            1,
            "one note even though two thresholds cleared at once"
        );
        assert!(notes[0].text.contains("80%"), "{}", notes[0].text);
        // A5.6: a window-fill note is never a `Budget` crossing -- there is
        // no parent-facing counterpart for context-window pressure.
        assert!(!notes[0].is_budget());
        assert_eq!(notes[0].limit_key(), None);

        // Same size again: no NEW threshold crossed (80% < 90%, and 50/75
        // are already recorded) -- no note.
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert!(
            notes.is_empty(),
            "must not repeat an already-announced threshold: {notes:?}"
        );

        // A later turn crossing 90% DOES get its own note -- a higher
        // threshold is a genuinely new crossing, not a repeat.
        inputs.total_tokens_est = 3_700; // 92.5%
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].text.contains("92%"), "{}", notes[0].text);
    }

    /// Board item `01M1YS0B0NYTMWM1M5C7250FFT`: `window_note`'s old template appended a literal `k`
    /// after both `{used}` and `{max_k}`, which `compact_k` had *already*
    /// suffixed with its own `k` for any value >= 1000 -- `10.6kk of
    /// 32.7kk tokens` instead of `10.6k of 32.7k tokens`. Both `used` and
    /// `max` are chosen >= 1000 here specifically so a regression that
    /// reintroduces the doubled suffix is caught on both terms at once, not
    /// only the one this test happens to check first.
    #[test]
    fn window_note_units_never_double_the_k_suffix() {
        let model = model();
        let budget = budget(0);
        let mut tracker = RunwayTracker::default();
        let mut inputs = base_inputs(&model, &budget);
        // 21_000 / 32_768 = 64% -- both used and max render with a `k`
        // suffix (`compact_k(21_000) == "21.0k"`, `compact_k(32_768) ==
        // "32.7k"`), the exact shape that exposed the doubled-`k` defect.
        inputs.total_tokens_est = 21_000;
        inputs.max_context_tokens = Some(32_768);
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].text.contains("21.0k of 32.7k tokens"),
            "{}",
            notes[0].text
        );
        assert!(
            !notes[0].text.contains("kk"),
            "units must never double the `k` suffix: {}",
            notes[0].text
        );

        // THE OTHER SITE THAT BUILDS THIS TEXT. `provenance_clause` appends
        // the "assumed" caveat, and it interpolated `max_k` -- already
        // `k`-suffixed -- followed by another literal `k`. The assertion
        // above never reached it, because the default source is not a
        // floor and the clause is empty for every other source. So the
        // doubled suffix survived in real sessions ("32.7kk assumed") long
        // after the template above was fixed. Both floor variants, since
        // both produce the clause.
        for floor_source in [
            ContextTokensSource::DialectDefaultFloor,
            ContextTokensSource::Unverified,
        ] {
            let mut tracker = RunwayTracker::default();
            let mut inputs = base_inputs(&model, &budget);
            inputs.total_tokens_est = 21_000;
            inputs.max_context_tokens = Some(32_768);
            inputs.max_context_tokens_source = floor_source;
            let notes = notes_for_turn(&mut tracker, &inputs);
            assert_eq!(notes.len(), 1);
            assert!(
                notes[0].text.contains("32.7k assumed"),
                "{floor_source:?} must name the window once, with one `k`: {}",
                notes[0].text
            );
            assert!(
                !notes[0].text.contains("kk"),
                "{floor_source:?} must never double the `k` suffix: {}",
                notes[0].text
            );
        }
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

    /// **Hosted OpenAI-compatible models item, acceptance criterion 4:**
    /// the runway notice names the window's provenance when it is a floor
    /// (`DialectDefaultFloor`/`Unverified`), and says nothing extra when a
    /// real source declared it (`Override`/`Metadata`/`Probed`) -- GP-14,
    /// shown at exactly the surface the reported incident (a floor-governed
    /// window that looked exactly like a real one) crossed.
    #[test]
    fn window_note_names_the_floor_as_assumed_and_never_does_for_a_declared_window() {
        let model = model();
        let budget = budget(0);

        for floor_source in [
            ContextTokensSource::DialectDefaultFloor,
            ContextTokensSource::Unverified,
        ] {
            let mut tracker = RunwayTracker::default();
            let mut inputs = base_inputs(&model, &budget);
            inputs.total_tokens_est = 3_200;
            inputs.max_context_tokens = Some(4_000);
            inputs.max_context_tokens_source = floor_source;
            let notes = notes_for_turn(&mut tracker, &inputs);
            assert_eq!(notes.len(), 1);
            assert!(
                notes[0].text.contains("assumed"),
                "{floor_source:?} must be labelled assumed: {}",
                notes[0].text
            );
            assert!(
                notes[0].text.contains(".conway/models.json"),
                "must point the operator at the fix: {}",
                notes[0].text
            );
        }

        for declared_source in [
            ContextTokensSource::Override,
            ContextTokensSource::Metadata,
            ContextTokensSource::Probed,
        ] {
            let mut tracker = RunwayTracker::default();
            let mut inputs = base_inputs(&model, &budget);
            inputs.total_tokens_est = 3_200;
            inputs.max_context_tokens = Some(4_000);
            inputs.max_context_tokens_source = declared_source;
            let notes = notes_for_turn(&mut tracker, &inputs);
            assert_eq!(notes.len(), 1);
            assert!(
                !notes[0].text.contains("assumed"),
                "{declared_source:?} must not be labelled assumed: {}",
                notes[0].text
            );
        }
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
        assert!(notes[0].text.contains("4 of 5"), "{}", notes[0].text);
        assert!(notes[0].text.contains(MAX_STEPS), "{}", notes[0].text);
        // A5.6: a budget-dimension crossing, unlike a window-fill note,
        // carries the exact `<key>=<n>` a parent notice / `Event::
        // BudgetWarning` needs.
        assert!(notes[0].is_budget());
        assert_eq!(notes[0].limit_key(), Some("max_steps=5"));

        // Still within the same turn/user-turn: repeating (or exceeding)
        // the same step count must not re-fire.
        inputs.steps_this_turn = 5;
        assert!(notes_for_turn(&mut tracker, &inputs).is_empty());

        // Keep-alive user-turn boundary: `AgentLoop::run_inner` calls this
        // exactly where it resets `turn_steps`/`tool_calls`.
        tracker.reset_turn_scoped();
        inputs.steps_this_turn = 4;
        let notes = notes_for_turn(&mut tracker, &inputs);
        assert_eq!(
            notes.len(),
            1,
            "a fresh user turn can trip the same dimension again"
        );
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
        assert!(
            notes[0].text.contains("80 of 100 seconds"),
            "{}",
            notes[0].text
        );
        assert!(notes[0].is_budget());
        assert_eq!(
            notes[0].limit_key(),
            Some(format!("deadline={deadline}")).as_deref()
        );

        // No repeat.
        assert!(notes_for_turn(&mut tracker, &inputs).is_empty());
    }
}
