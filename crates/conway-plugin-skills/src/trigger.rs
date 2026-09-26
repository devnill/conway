//! The mechanical "does this task look like real work" trigger (board item
//! `01M3DTRY56PQ5BX8HTVE7V96K0`): evaluated once per ROOT agent's own
//! `Event::AgentFinished`, from evidence accumulated purely by watching the
//! host's live event stream through [`conway::plugin::Plugin::observe_sink`]
//! -- `conway.skills` is the first IN-PROCESS `Plugin` to override that
//! seam (every existing implementor, `conway_plugin_subprocess::
//! SubprocessPlugin`, only forwards it over a wire; `docs/plugins/
//! hooks.md` point 11's own "What REMAINS design-only" line names this gap
//! before this module closes it).
//!
//! **Scope, restated from the board item.** This module decides "a
//! proposal is warranted" and records why. It does not fork anything, show
//! anything, or write anything -- that is a separate, not-yet-built slice.
//! `SkillsPlugin::last_trigger_evidence` (`../lib.rs`) is this module's
//! entire hand-off surface to it.
//!
//! # Why `AgentFinished`, not `TurnFinished`, is "the operator's turn ended"
//!
//! Two candidates carry a turn-boundary meaning in `conway_core::event::
//! Event`: `TurnFinished { usage, stop }` and `AgentFinished { result,
//! ephemeral }`. Two facts about the ACTUAL delivery path settle which one
//! this module can use at all:
//!
//! 1. **`TurnFinished` brackets one MODEL round trip, not one operator
//!    turn.** `conway_runtime::tree::AgentTree`'s own doc calls this
//!    bracket "the window... for one model round-trip" -- a single operator
//!    prompt that triggers several tool-call rounds produces several
//!    `TurnFinished` events before the reply the operator actually sees.
//!    `stop: StopReason::EndTurn` (vs `ToolUse`) is what marks the LAST one
//!    of a batch as the operator-visible turn's true end.
//! 2. **The bare `Event` `Plugin::observe_sink` receives carries NO agent or
//!    session identity for most variants.** `conway::builder`'s own
//!    forwarding task calls `sink.emit(envelope.event)` -- the `Envelope`
//!    wrapper (`seq`/`session`/`agent`) is stripped before an in-process
//!    sink ever sees it; the identical stripping happens on the wire (
//!    `conway_plugin_subprocess::wire::build_observe_notification`
//!    serializes the bare `Event` only). `TurnFinished` carries no
//!    `agent_id` field of its own, so a GLOBAL observer -- one `EventSink`
//!    watching every agent in the tree, which is what "the first in-process
//!    observer" has to be -- cannot tell whose `TurnFinished` it just
//!    received, root's or a delegated child's. There is structurally no way
//!    to build the root/child discriminator this item's own acceptance
//!    criteria require on top of `TurnFinished` alone.
//!
//! `AgentFinished` does not have this problem: `result: AgentResult` carries
//! `result.agent_id` in-band, in the event's own payload, independent of the
//! envelope. That is the one piece of identity a global observer actually
//! gets for free, and it is why `AgentFinished::ephemeral` existing at all
//! -- filtering out a `/ask`-style aside's own finish -- only makes sense as
//! a real per-call nuance if this variant is meant to be read by something
//! that does not already know, from context, which agent is asking.
//!
//! **The trade-off, disclosed rather than hidden:** for a `keep_alive: true`
//! root (the TUI's own interactive session), `AgentFinished` fires only when
//! the WHOLE session ends (the operator quits), not after each prompt --
//! `conway_core::agent::AgentKnobs::keep_alive`'s own contract. This trigger
//! therefore evaluates once per COMPLETED ROOT AGENT (the common `keep_alive:
//! false` case -- `conway-cli`'s own one-shot root, and every `SessionSpec::
//! default()` fixture in this crate's own end-to-end suite), not once per
//! conversational exchange within a long-lived interactive session. Getting
//! per-turn firing inside a keep-alive root would need `Envelope`-level
//! identity threaded through `observe_sink`, which is a change to
//! `conway_core::ports::plugin`/`conway_core::ports::events` this item's own
//! fence forbids ("Do NOT edit... conway-core/src/ports/plugin.rs... If you
//! conclude the trait must change, STOP and report") -- named here as the
//! concrete follow-up rather than worked around inside this crate.
//!
//! # Recovering root-vs-child identity anyway: the `ContextHook` side channel
//!
//! Since `Event::AgentFinished` gives us `result.agent_id` but nothing that
//! says "and this agent IS the root of its own tree", this module reuses a
//! seam `conway.skills` already has for an unrelated reason:
//! `Plugin::context_hooks`. [`RootTrackerHook::before_request`] fires once
//! per model round trip for EVERY agent, root and child alike, and its own
//! `ContextHookCtx::agent_path` is documented as existing precisely "to tell
//! a depth-four agent apart from a depth-one one" -- `vec![agent_id]`, a
//! single element, for a root, longer for any descendant. Recording
//! `agent_path.len() == 1` against `agent_id` the first time each agent's
//! hook fires -- which, by architecture §8's "`AgentSpawned` precedes every
//! other event for that id" guarantee (and the fact that ANY agent must
//! call the model before it can finish), always happens before that same
//! agent's own `AgentFinished` -- gives [`TriggerState::record_event`] a
//! table it can consult by
//! `result.agent_id` when a finish arrives. An agent this plugin never saw a
//! `before_request` for (e.g. cancelled before its first model call)
//! defaults to "not root" -- conservative: the trigger can under-fire on a
//! degenerate case, never over-fire on an agent it never actually observed
//! taking root-shaped action.
//!
//! # The lossy-delivery decision
//!
//! `Plugin::observe_sink`'s own doc: a slow subscriber falls behind the
//! runtime's broadcast bus and sees `Event::Lagged { skipped }` rather than
//! stalling any producer, and a plugin's own bounded forwarding queue drops
//! with a warn on overflow rather than ever blocking the host turn. Either
//! failure mode can cost this trigger the one `AgentFinished` (or an
//! evidence-building event feeding it) that would have fired a proposal for
//! a given task. **The decision: do nothing about it.** `Event::Lagged`
//! is matched and deliberately ignored by [`TriggerState::record_event`]
//! (not "handled" -- there is nothing to recover: the skipped events are
//! gone, and re-requesting them would mean this observer stalling the host
//! turn to ask, which is exactly the failure mode `observe_sink` exists to
//! rule out). A missed evaluation means one fewer skill-authoring proposal
//! offered, never a wrong one and never a hung turn -- the honest read of
//! "best-effort" this feature can make given the delivery guarantee it is
//! built on, restated at the definition site rather than left implicit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use conway::plugin::{
    async_trait, ContextHook, ContextHookCtx, ContextPayload, Event, EventSink,
    PluginConfigureError,
};
use conway::AgentId;

/// The `[plugins.config."conway.skills"]` key `TriggerState::configure`
/// recognizes: the minimum number of `Event::ToolCallProposed` occurrences
/// observed during one root task for the tool-call-count rule to fire on
/// its own. The other two mechanical rules (an error-then-success sequence,
/// an operator correction) have no numeric knob -- they are structural,
/// not thresholds, so there is nothing for an operator to tune about them.
pub const TOOL_CALL_THRESHOLD_KEY: &str = "tool_call_threshold";

/// **Default: 8.** Not a measured calibration -- the same "plausible, not
/// proven" disclosure this crate's own module doc already makes about
/// progressive disclosure applies here verbatim. Chosen as a round number
/// comfortably above a trivial one-or-two-tool-call exchange (a single file
/// read, a quick grep) and comfortably below what a long, genuinely
/// exploratory session accumulates, so the count rule alone does not fire
/// on every turn that merely touches a tool. An operator who finds this
/// wrong for their own workload overrides it via
/// `[plugins.config."conway.skills"] tool_call_threshold = <n>`.
pub const DEFAULT_TOOL_CALL_THRESHOLD: u32 = 8;

/// Evidence assembled at one qualifying root turn's end, plus the verdict
/// computed from it -- the entire hand-off contract to the NEXT slice (a
/// fork, a modal, a written skill file), none of which is built here. See
/// [`SkillsPlugin::last_trigger_evidence`](crate::SkillsPlugin::last_trigger_evidence).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillProposalEvidence {
    /// The root agent this evidence was recorded for -- `result.agent_id`
    /// off the `Event::AgentFinished` that closed the window this evidence
    /// covers.
    pub root_agent: AgentId,
    /// How many `Event::ToolCallProposed` occurrences were observed during
    /// this task (any agent in the tree, not root alone -- a delegated
    /// child's own tool calls are legitimately part of how much work the
    /// task took).
    pub tool_call_count: u32,
    /// The threshold `tool_call_count` was compared against (the
    /// configured value, or [`DEFAULT_TOOL_CALL_THRESHOLD`]) -- carried
    /// alongside the count so a consumer never has to ask separately what
    /// this decision was measured against.
    pub tool_call_threshold: u32,
    /// `tool_call_count >= tool_call_threshold`.
    pub tool_call_threshold_met: bool,
    /// A `Event::ToolCallFinished { is_error: true, .. }` was followed,
    /// later in the same task, by one with `is_error: false` -- "hit a
    /// wall, then got past it", the second mechanical rule.
    pub error_then_success: bool,
    /// This task's own opening `Event::UserTurn` (observed while no
    /// subagent's window was open -- see `TriggerState::record_event`'s
    /// own doc for the depth-gating this relies on and its known limit)
    /// began with "no" or "instead" -- the third mechanical rule.
    pub operator_correction: bool,
}

impl SkillProposalEvidence {
    /// The mechanical OR across all three rules -- INTENT.md §5a's "no LLM
    /// judging complexity" boundary: every input here is a plain count or
    /// boolean, and this is the only place they are combined
    /// (`TriggerState::record_event` calls nothing else that recomputes
    /// this -- one implementation, so the three rules cannot drift apart;
    /// see `TriggerState::evaluate`'s doc).
    pub fn should_propose(&self) -> bool {
        self.tool_call_threshold_met || self.error_then_success || self.operator_correction
    }
}

/// Per-task evidence accumulated purely from the bare `Event` stream,
/// between one root's `AgentFinished` and the next. See the module doc's
/// "depth" discussion for why `depth` gates only the correction rule and
/// nothing else.
#[derive(Default)]
struct Accumulator {
    /// Best-effort open-agent counter: `+1` per `Event::AgentSpawned`
    /// occurrence, `-1` per `Event::AgentFinished` occurrence, counted
    /// WITHOUT knowing which agent (the bare `Event` never says). Correct
    /// for `SubagentMode::Spawn` (a synchronous delegation: the spawning
    /// agent's own execution is paused until the child returns, so the
    /// child's whole lifecycle nests cleanly between two of the caller's own
    /// events); a concurrently-running `SubagentMode::Fork` can make this
    /// counter briefly wrong (a fork's own events can interleave with its
    /// parent's). Used ONLY to gate the operator-correction rule below --
    /// never the tool-call count, which is deliberately task-wide regardless
    /// of nesting.
    depth: i64,
    tool_call_count: u32,
    /// Set on `Event::ToolCallFinished { is_error: true, .. }`; cleared the
    /// moment a subsequent success is observed (at which point
    /// `error_then_success` is what stays set for the rest of the task).
    pending_tool_error: bool,
    error_then_success: bool,
    operator_correction: bool,
}

/// Learned root-vs-child table plus the accumulating evidence and the
/// per-agent published decisions this plugin's [`conway::plugin::Plugin::
/// observe_sink`]/[`conway::plugin::Plugin::context_hooks`] impls both
/// read and write. One instance per [`crate::SkillsPlugin`], held behind an
/// `Arc` so the `EventSink`/`ContextHook` halves (each wrapped and handed
/// to the host separately) share the identical state.
pub(crate) struct TriggerState {
    tool_call_threshold: Mutex<u32>,
    /// `agent_id -> is this agent the root of its own tree`, learned from
    /// [`RootTrackerHook::before_request`]'s own `ContextHookCtx::
    /// agent_path`. An agent id absent from this table (never observed via
    /// the context hook) is treated as NOT root -- see the module doc's
    /// "Recovering root-vs-child identity" section for why that default is
    /// the conservative one.
    roots: Mutex<HashMap<AgentId, bool>>,
    acc: Mutex<Accumulator>,
    /// The latest published verdict per root agent id -- overwritten only
    /// by a LATER qualifying finish for the SAME agent id (which cannot
    /// happen twice for one non-keep-alive root; kept as a map rather than
    /// a single slot so a plugin instance reused across more than one
    /// sequential session's root still answers each session's own agent id
    /// correctly).
    decisions: Mutex<HashMap<AgentId, SkillProposalEvidence>>,
}

impl TriggerState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            tool_call_threshold: Mutex::new(DEFAULT_TOOL_CALL_THRESHOLD),
            roots: Mutex::new(HashMap::new()),
            acc: Mutex::new(Accumulator::default()),
            decisions: Mutex::new(HashMap::new()),
        })
    }

    /// Applies this plugin's own `[plugins.config."conway.skills"]` slice --
    /// mirrors `conway_plugin_trim::TrimPlugin::configure`'s exact shape
    /// (the first real `Plugin::configure` implementor): an unrecognized key
    /// is refused BY NAME ([`PluginConfigureError::UnknownKey`]), a
    /// wrong-shaped or out-of-range value is refused naming which key and
    /// why ([`PluginConfigureError::InvalidValue`]), and a non-object whole
    /// value is refused naming the JSON type it actually got
    /// ([`PluginConfigureError::NotAnObject`]) -- never a silent no-op, the
    /// same "typo surfaces as a config error, not nothing" rule
    /// `Plugin::configure`'s own doc states.
    pub(crate) fn configure(&self, value: &serde_json::Value) -> Result<(), PluginConfigureError> {
        let object = value
            .as_object()
            .ok_or_else(|| PluginConfigureError::NotAnObject {
                actual: json_value_kind(value).to_string(),
            })?;
        let mut threshold = *self.tool_call_threshold.lock().expect("threshold poisoned");
        for (key, raw) in object {
            match key.as_str() {
                TOOL_CALL_THRESHOLD_KEY => {
                    let n = raw
                        .as_u64()
                        .ok_or_else(|| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: "must be a JSON integer".to_string(),
                        })?;
                    if n == 0 {
                        return Err(PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: "must be >= 1".to_string(),
                        });
                    }
                    threshold =
                        u32::try_from(n).map_err(|_| PluginConfigureError::InvalidValue {
                            key: key.clone(),
                            message: format!("must fit in a u32, got {n}"),
                        })?;
                }
                other => {
                    return Err(PluginConfigureError::UnknownKey {
                        key: other.to_string(),
                    });
                }
            }
        }
        *self.tool_call_threshold.lock().expect("threshold poisoned") = threshold;
        Ok(())
    }

    /// Records, from a live [`ContextHookCtx`], whether `agent_id` is the
    /// root of its own tree (`agent_path.len() == 1`). Overwritten on every
    /// call rather than inserted-once: an agent's own `agent_path` never
    /// changes across its lifetime (mirroring `ContextHookCtx::agent_path`'s
    /// own doc), so a repeat call for the same agent is always idempotent,
    /// and there is no ordering hazard in letting a later call win.
    fn record_agent_path(&self, agent_id: AgentId, agent_path_len: usize) {
        self.roots
            .lock()
            .expect("roots poisoned")
            .insert(agent_id, agent_path_len <= 1);
    }

    /// The single evaluation site (P-14): every mechanical rule this trigger
    /// implements is read here, once, from `acc`, and nowhere else computes
    /// "does this evidence warrant a proposal" independently -- see
    /// [`SkillProposalEvidence::should_propose`], which is a plain `||` over
    /// this function's own output fields rather than a second re-derivation.
    fn evaluate(
        acc: &Accumulator,
        root_agent: AgentId,
        tool_call_threshold: u32,
    ) -> SkillProposalEvidence {
        SkillProposalEvidence {
            root_agent,
            tool_call_count: acc.tool_call_count,
            tool_call_threshold,
            tool_call_threshold_met: acc.tool_call_count >= tool_call_threshold,
            error_then_success: acc.error_then_success,
            operator_correction: acc.operator_correction,
        }
    }

    /// The `EventSink::emit` body -- see the module doc for the full
    /// argument behind every branch below (why `AgentFinished` is the
    /// anchor, why `depth` gates only the correction rule, and the
    /// lossy-delivery decision `Event::Lagged`'s own arm restates in code).
    fn record_event(&self, event: Event) {
        match event {
            Event::AgentSpawned { .. } => {
                self.acc.lock().expect("acc poisoned").depth += 1;
            }
            Event::UserTurn { text, .. } => {
                let mut acc = self.acc.lock().expect("acc poisoned");
                if acc.depth <= 0 && is_operator_correction(&text) {
                    acc.operator_correction = true;
                }
            }
            Event::ToolCallProposed { .. } => {
                self.acc.lock().expect("acc poisoned").tool_call_count += 1;
            }
            Event::ToolCallFinished { is_error, .. } => {
                let mut acc = self.acc.lock().expect("acc poisoned");
                if is_error {
                    acc.pending_tool_error = true;
                } else if acc.pending_tool_error {
                    acc.error_then_success = true;
                    acc.pending_tool_error = false;
                }
            }
            Event::AgentFinished { result, ephemeral } => {
                let mut acc = self.acc.lock().expect("acc poisoned");
                acc.depth -= 1;
                let is_root = self
                    .roots
                    .lock()
                    .expect("roots poisoned")
                    .get(&result.agent_id)
                    .copied()
                    .unwrap_or(false);
                if !ephemeral && is_root {
                    let threshold = *self.tool_call_threshold.lock().expect("threshold poisoned");
                    let evidence = Self::evaluate(&acc, result.agent_id, threshold);
                    self.decisions
                        .lock()
                        .expect("decisions poisoned")
                        .insert(result.agent_id, evidence);
                    *acc = Accumulator::default();
                }
            }
            // `Event::Lagged { skipped }`: the module doc's lossy-delivery
            // decision -- deliberately ignored, not "handled". Every other
            // variant carries nothing this trigger's three mechanical rules
            // read.
            _ => {}
        }
    }

    /// The published verdict for `root_agent`'s most recent qualifying
    /// finish, if any -- the read half of this module's hand-off to the
    /// next slice. `None` before that agent's root turn has ended, for an
    /// agent id this plugin never learned was a root, or for an agent this
    /// plugin never observed at all.
    pub(crate) fn last_evidence(&self, root_agent: &AgentId) -> Option<SkillProposalEvidence> {
        self.decisions
            .lock()
            .expect("decisions poisoned")
            .get(root_agent)
            .cloned()
    }
}

/// `text.trim()`'s first run of alphabetic characters, lower-cased, compared
/// against "no"/"instead" -- deliberately word-bounded (`"nonsense..."`
/// must not match "no", `"no,"`/`"No."` must) rather than a bare
/// `starts_with` on the raw string.
fn is_operator_correction(text: &str) -> bool {
    let first_word = text
        .trim()
        .split(|c: char| !c.is_alphabetic())
        .find(|w| !w.is_empty());
    matches!(
        first_word.map(str::to_ascii_lowercase).as_deref(),
        Some("no") | Some("instead")
    )
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// The `Plugin::observe_sink` half: a thin [`EventSink`] forwarding every
/// received `Event` into [`TriggerState::record_event`]. `emit` never
/// blocks and never panics on a well-formed `Event` -- every lock it takes
/// is uncontended in the overwhelmingly common single-writer-task case
/// (`ConwayBuilder::build`'s own forwarding task is the sink's only caller
/// in production; [`RootTrackerHook`] below takes only the SEPARATE `roots`
/// lock, never `acc`, so the two seams cannot deadlock each other).
pub(crate) struct TriggerEventSink {
    pub(crate) state: Arc<TriggerState>,
}

impl EventSink for TriggerEventSink {
    fn emit(&self, event: Event) {
        self.state.record_event(event);
    }
}

/// The `Plugin::context_hooks` half: records root-vs-child identity (see
/// the module doc) and otherwise leaves the assembled request completely
/// untouched -- unlike [`crate::SkillIndexHook`], this hook exists purely
/// for its side effect on [`TriggerState`], never to narrow or edit
/// anything the model sees.
pub(crate) struct RootTrackerHook {
    pub(crate) state: Arc<TriggerState>,
}

#[async_trait]
impl ContextHook for RootTrackerHook {
    async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        self.state
            .record_agent_path(ctx.agent_id, ctx.agent_path.len());
        payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::plugin::ResultStatus;
    use conway::AgentResult;

    fn finished(agent: AgentId, ephemeral: bool) -> Event {
        Event::AgentFinished {
            result: AgentResult::new(
                agent,
                conway::SessionId::new(),
                ResultStatus::Completed,
                "done",
            ),
            ephemeral,
        }
    }

    fn tool_call(state: &TriggerState) {
        state.record_event(Event::ToolCallProposed {
            call_id: "c".to_string(),
            tool: conway::plugin::ToolName::new("t"),
            args: serde_json::json!({}),
        });
    }

    fn tool_finish(state: &TriggerState, is_error: bool) {
        state.record_event(Event::ToolCallFinished {
            call_id: "c".to_string(),
            is_error,
            preview: "p".to_string(),
        });
    }

    fn root_agent(state: &TriggerState) -> AgentId {
        let agent = AgentId::new();
        state.record_agent_path(agent, 1);
        agent
    }

    fn child_agent(state: &TriggerState, depth: usize) -> AgentId {
        let agent = AgentId::new();
        state.record_agent_path(agent, depth);
        agent
    }

    // -----------------------------------------------------------------
    // Threshold 1: tool-call count, independently tested with a value on
    // each side of the boundary.
    // -----------------------------------------------------------------
    #[test]
    fn tool_call_count_below_threshold_does_not_propose() {
        let state = TriggerState::new();
        state
            .configure(&serde_json::json!({ "tool_call_threshold": 3 }))
            .unwrap();
        let root = root_agent(&state);
        tool_call(&state);
        tool_call(&state);
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).expect("root turn recorded");
        assert_eq!(evidence.tool_call_count, 2);
        assert!(!evidence.tool_call_threshold_met);
        assert!(!evidence.error_then_success);
        assert!(!evidence.operator_correction);
        assert!(!evidence.should_propose(), "{evidence:?}");
    }

    #[test]
    fn tool_call_count_at_or_above_threshold_proposes() {
        let state = TriggerState::new();
        state
            .configure(&serde_json::json!({ "tool_call_threshold": 3 }))
            .unwrap();
        let root = root_agent(&state);
        tool_call(&state);
        tool_call(&state);
        tool_call(&state);
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).expect("root turn recorded");
        assert_eq!(evidence.tool_call_count, 3);
        assert!(evidence.tool_call_threshold_met);
        assert!(evidence.should_propose(), "{evidence:?}");
    }

    #[test]
    fn default_threshold_is_used_when_never_configured() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        for _ in 0..(DEFAULT_TOOL_CALL_THRESHOLD - 1) {
            tool_call(&state);
        }
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).unwrap();
        assert_eq!(evidence.tool_call_threshold, DEFAULT_TOOL_CALL_THRESHOLD);
        assert!(!evidence.tool_call_threshold_met);

        let state2 = TriggerState::new();
        let root2 = root_agent(&state2);
        for _ in 0..DEFAULT_TOOL_CALL_THRESHOLD {
            tool_call(&state2);
        }
        state2.record_event(finished(root2, false));
        assert!(
            state2
                .last_evidence(&root2)
                .unwrap()
                .tool_call_threshold_met
        );
    }

    // -----------------------------------------------------------------
    // Threshold 2: an error-then-success sequence, independent of the
    // tool-call count (kept well below the default threshold here).
    // -----------------------------------------------------------------
    #[test]
    fn a_tool_error_followed_by_a_success_proposes() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        tool_call(&state);
        tool_finish(&state, true);
        tool_call(&state);
        tool_finish(&state, false);
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).unwrap();
        assert!(!evidence.tool_call_threshold_met, "well under the default");
        assert!(evidence.error_then_success);
        assert!(evidence.should_propose(), "{evidence:?}");
    }

    #[test]
    fn a_success_never_preceded_by_an_error_does_not_set_the_flag() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        tool_call(&state);
        tool_finish(&state, false);
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).unwrap();
        assert!(!evidence.error_then_success);
        assert!(!evidence.should_propose());
    }

    #[test]
    fn an_error_with_no_subsequent_success_does_not_set_the_flag() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        tool_call(&state);
        tool_finish(&state, true);
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).unwrap();
        assert!(!evidence.error_then_success);
        assert!(!evidence.should_propose());
    }

    // -----------------------------------------------------------------
    // Threshold 3: an operator correction, independent of the other two.
    // -----------------------------------------------------------------
    #[test]
    fn a_user_turn_starting_with_no_proposes() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        state.record_event(Event::UserTurn {
            text: "no, do it the other way".to_string(),
            prov: conway::plugin::Provenance::UserPrompt,
        });
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).unwrap();
        assert!(!evidence.tool_call_threshold_met);
        assert!(!evidence.error_then_success);
        assert!(evidence.operator_correction);
        assert!(evidence.should_propose(), "{evidence:?}");
    }

    #[test]
    fn a_user_turn_starting_with_instead_proposes() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        state.record_event(Event::UserTurn {
            text: "Instead, use the other file".to_string(),
            prov: conway::plugin::Provenance::UserPrompt,
        });
        state.record_event(finished(root, false));
        assert!(state.last_evidence(&root).unwrap().operator_correction);
    }

    #[test]
    fn an_ordinary_user_turn_is_not_a_correction() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        state.record_event(Event::UserTurn {
            text: "please add a test for this".to_string(),
            prov: conway::plugin::Provenance::UserPrompt,
        });
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).unwrap();
        assert!(!evidence.operator_correction);
        assert!(!evidence.should_propose());
    }

    #[test]
    fn a_word_merely_starting_with_no_is_not_a_correction() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        state.record_event(Event::UserTurn {
            text: "nonsense, try again".to_string(),
            prov: conway::plugin::Provenance::UserPrompt,
        });
        state.record_event(finished(root, false));
        assert!(!state.last_evidence(&root).unwrap().operator_correction);
    }

    #[test]
    fn a_correction_phrased_user_turn_inside_a_spawned_childs_window_is_not_counted() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        // A child's own AgentSpawned opens the window; a `UserTurn` seen
        // while it is open is presumed to be the CHILD's own (model-authored)
        // task prompt, never the human operator correcting the root -- see
        // the module doc's depth-gating note.
        state.record_event(Event::AgentSpawned {
            kind: conway::SubagentMode::Spawn,
            parent: Some(root),
            agent_def: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.record_event(Event::UserTurn {
            text: "no, actually look at the other module".to_string(),
            prov: conway::plugin::Provenance::UserPrompt,
        });
        state.record_event(finished(AgentId::new(), false)); // the child's own finish
        state.record_event(finished(root, false));
        let evidence = state.last_evidence(&root).unwrap();
        assert!(
            !evidence.operator_correction,
            "a correction-shaped prompt inside a spawned child's window must not count: \
             {evidence:?}"
        );
    }

    // -----------------------------------------------------------------
    // Root vs. child attribution (pure state-machine level; the real,
    // through-a-live-Conway proof lives in `tests/skills_e2e.rs`).
    // -----------------------------------------------------------------
    #[test]
    fn a_delegated_childs_finish_does_not_record_evidence() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        let child = child_agent(&state, 2);
        tool_call(&state);
        state.record_event(finished(child, false));
        assert!(
            state.last_evidence(&child).is_none(),
            "a non-root agent's finish must never publish evidence"
        );
        // The root has not finished yet -- nothing published for it either.
        assert!(state.last_evidence(&root).is_none());
    }

    #[test]
    fn an_ephemeral_asks_finish_does_not_record_evidence_even_if_root_shaped() {
        let state = TriggerState::new();
        let root = root_agent(&state);
        tool_call(&state);
        state.record_event(finished(root, true));
        assert!(
            state.last_evidence(&root).is_none(),
            "an ephemeral (/ask-style) finish must never publish evidence, even for an agent \
             this plugin learned was root-shaped"
        );
    }

    #[test]
    fn an_unknown_agents_finish_defaults_to_not_root() {
        let state = TriggerState::new();
        let unknown = AgentId::new(); // never seen via record_agent_path
        state.record_event(finished(unknown, false));
        assert!(state.last_evidence(&unknown).is_none());
    }

    // -----------------------------------------------------------------
    // `Plugin::configure` shape, mirroring `conway_plugin_trim::TrimPlugin`.
    // -----------------------------------------------------------------
    #[test]
    fn configure_refuses_an_unknown_key_by_name() {
        let state = TriggerState::new();
        let err = state
            .configure(&serde_json::json!({ "tool_call_thresholdd": 3 }))
            .unwrap_err();
        assert!(
            matches!(err, PluginConfigureError::UnknownKey { key } if key == "tool_call_thresholdd")
        );
    }

    #[test]
    fn configure_refuses_a_non_integer_value() {
        let state = TriggerState::new();
        let err = state
            .configure(&serde_json::json!({ "tool_call_threshold": "eight" }))
            .unwrap_err();
        assert!(
            matches!(err, PluginConfigureError::InvalidValue { key, .. } if key == "tool_call_threshold")
        );
    }

    #[test]
    fn configure_refuses_zero() {
        let state = TriggerState::new();
        let err = state
            .configure(&serde_json::json!({ "tool_call_threshold": 0 }))
            .unwrap_err();
        assert!(
            matches!(err, PluginConfigureError::InvalidValue { key, .. } if key == "tool_call_threshold")
        );
    }

    #[test]
    fn configure_refuses_a_non_object_value() {
        let state = TriggerState::new();
        let err = state.configure(&serde_json::json!(3)).unwrap_err();
        assert!(matches!(err, PluginConfigureError::NotAnObject { actual } if actual == "number"));
    }

    #[test]
    fn configure_accepts_a_valid_threshold_and_it_takes_effect() {
        let state = TriggerState::new();
        state
            .configure(&serde_json::json!({ "tool_call_threshold": 2 }))
            .unwrap();
        let root = root_agent(&state);
        tool_call(&state);
        tool_call(&state);
        state.record_event(finished(root, false));
        assert!(state.last_evidence(&root).unwrap().tool_call_threshold_met);
    }
}
