//! `RoutingExplain`: the "why did this model run, and why not the others"
//! answer (WI-036, amended for headroom). Implemented **solely** as a
//! projection of `DeclarativeRouter::evaluate` (`router.rs`, WI-034) plus a
//! per-candidate health/capability snapshot -- it must never re-implement
//! filtering, which is the specific bug this structure prevents.
//!
//! Divergence note (flagged, not worked around): the binding plan
//! specifies each
//! entry's `breaker` field as `BreakerSnapshot { transport: BreakerState,
//! probe: BreakerState }` -- an independent read of each of the two breaker
//! kinds. `conway_core::ports::HealthRegistry` (the only handle this crate's
//! `DeclarativeRouter` holds on health state) exposes just one method,
//! `state(&EndpointId) -> BreakerState`, which returns the *merged* view
//! (`BreakerRegistry::merged_state`); the transport/probe split is an
//! inherent method on `BreakerRegistry` itself (`kind_state`), not part of
//! the port trait, so it is unreachable through `Arc<dyn HealthRegistry>`.
//! `BreakerSnapshot` below therefore carries a single merged `state` field
//! instead of the specified `{transport, probe}` pair. Coordinator-approved
//! (option (a) of the WI-036 scoping blocker): a `HealthRegistry::kind_state`
//! trait addition is queued for the refinement phase; this note is the
//! breadcrumb back to the original spec shape.
//!
//! Second divergence note: the plan's implementation notes say
//! `generated_at` "is injected via the router's `Clock`", matching an
//! earlier draft. `DeclarativeRouter` (as landed at WI-034, commit 0a38c42)
//! carries no `Clock` field -- only `BreakerRegistry` does, and it is not
//! reachable from here (see above). `generated_at` is instead read directly
//! from `chrono::Utc::now()` at explain time. This does not affect
//! determinism of the golden-file criterion: `render_text` never emits
//! `generated_at`.
//!
//! Third note: `explain` is infallible (`-> ExplainReport`, not `Result`),
//! but `DeclarativeRouter::evaluate` can fail with
//! `RoutingError::UnknownRole` for a non-pinned request naming an
//! unconfigured role. In that case `explain` returns a report with
//! `entries: vec![]` and `headroom_tokens: 0` rather than propagating the
//! error -- the router's per-role headroom resolution
//! (`DeclarativeRouter::effective_headroom`) is private and not part of the
//! two coordinator-approved accessors (`health`, `capability_index`), so no
//! better value is reachable here without widening that approval. This is a
//! deliberate, documented limitation, not a silent swallow: `resolve` still
//! surfaces `UnknownRole` as an error to callers that need it.

use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use conway_core::capabilities::{
    Capabilities, ReliabilityTier, RequiredCaps, StructuredOutput, ToolCallSupport,
};
use conway_core::ids::{ModelRef, RoleAlias};
use conway_core::routing::{BreakerKind, BreakerState, RouteRequest, RoutingReason};

use crate::router::{endpoint_of, DeclarativeRouter, EvalOutcome};

/// A single breaker read at explain time. See the module-level divergence
/// note: this carries the `HealthRegistry`-port-visible merged state, not
/// the plan's originally specified `{transport, probe}` split.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BreakerSnapshot {
    pub state: BreakerState,
}

/// A read-only projection of a `(backend, model)` pair's `Capabilities`, for
/// rendering in an `ExplainEntry`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySummary {
    pub tool_calling: ToolCallSupport,
    pub max_context_tokens: u32,
    pub structured_output: StructuredOutput,
    pub parallel_tool_calls: bool,
    pub reasoning: bool,
    pub reliability_tier: ReliabilityTier,
}

impl From<&Capabilities> for CapabilitySummary {
    fn from(caps: &Capabilities) -> CapabilitySummary {
        CapabilitySummary {
            tool_calling: caps.tool_calling,
            max_context_tokens: caps.max_context_tokens,
            structured_output: caps.structured_output,
            parallel_tool_calls: caps.parallel_tool_calls,
            reasoning: caps.reasoning,
            reliability_tier: caps.reliability_tier,
        }
    }
}

/// Whether a candidate was chosen, or skipped -- carrying the router's exact
/// `RoutingReason` either way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EntryOutcome {
    Selected { reason: RoutingReason },
    Skipped { reason: RoutingReason },
}

/// One evaluated candidate: its place in the chain (or `None` for a pin),
/// whether it was selected or skipped and why, its capability summary (when
/// indexed), and its breaker snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplainEntry {
    pub model_ref: ModelRef,
    pub chain_position: Option<u8>,
    pub outcome: EntryOutcome,
    pub capabilities: Option<CapabilitySummary>,
    pub breaker: BreakerSnapshot,
}

/// The full "why did this model run, and why not the others" answer for one
/// `RouteRequest`, including the effective headroom reservation used for
/// the admission check (see the amendment).
///
/// Invariant consumers may rely on: `entries.is_empty()` is possible only
/// for an unrecognized role, since `config::validate` rejects empty chains
/// — a recognized role always yields one entry per chain candidate (or
/// exactly one for a pin), whatever their outcomes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplainReport {
    pub role: RoleAlias,
    pub pin: Option<ModelRef>,
    pub est_tokens: u32,
    pub required: RequiredCaps,
    pub headroom_tokens: u32,
    pub entries: Vec<ExplainEntry>,
    pub generated_at: DateTime<Utc>,
}

impl ExplainReport {
    /// A stable, line-oriented rendering (see `docs/routing.md`'s "Asking
    /// why a route was chosen" section for the exact format). Two-space
    /// indent, `[<position>]` (or `[pin]`), the model ref right-padded to the
    /// longest ref in the report plus two spaces, `SELECTED`/`SKIPPED`
    /// padded to eight columns, then the reason. Timestamps are RFC 3339
    /// UTC. Trailing newline present. No ANSI codes -- rendering is the
    /// CLI's concern, not this crate's.
    pub fn render_text(&self) -> String {
        let mut out = format!(
            "role: {}  (est_tokens={}, headroom_tokens={})\n",
            self.role, self.est_tokens, self.headroom_tokens
        );

        let width = self
            .entries
            .iter()
            .map(|e| e.model_ref.to_string().len())
            .max()
            .unwrap_or(0)
            + 2;

        for entry in &self.entries {
            let marker = match entry.chain_position {
                Some(position) => format!("[{position}]"),
                None => "[pin]".to_string(),
            };
            let (word, reason) = match &entry.outcome {
                EntryOutcome::Selected { reason } => ("SELECTED", render_selected(reason)),
                EntryOutcome::Skipped { reason } => {
                    ("SKIPPED", render_skipped(reason, &entry.breaker))
                }
            };
            let model_ref = entry.model_ref.to_string();
            let _ = writeln!(
                out,
                "  {marker} {model_ref:<width$}{word:<8} {reason}",
                width = width,
            );
        }

        out
    }
}

/// Renders an `EntryOutcome::Selected` reason for `render_text`.
fn render_selected(reason: &RoutingReason) -> String {
    match reason {
        RoutingReason::PinnedByApi => "pinned(via=api)".to_string(),
        RoutingReason::PinnedByAgentDef => "pinned(via=agent_def)".to_string(),
        RoutingReason::AliasPrimary { alias } => format!("primary(role={alias})"),
        RoutingReason::Fallback { position, .. } => format!("fallback(position={position})"),
        _ => "selected".to_string(),
    }
}

/// Renders an `EntryOutcome::Skipped` reason for `render_text`. The health
/// case reads its `until` timestamp from `breaker` (the independent snapshot
/// taken at explain time), since `RoutingReason::HealthSkip` itself carries
/// only the breaker kind, not a timestamp.
fn render_skipped(reason: &RoutingReason, breaker: &BreakerSnapshot) -> String {
    match reason {
        RoutingReason::CapabilitySkip { missing, .. } => {
            format!("capability: {}", missing.join("; "))
        }
        RoutingReason::HealthSkip { breaker: kind, .. } => {
            let kind_name = match kind {
                BreakerKind::Transport => "transport",
                BreakerKind::Probe => "probe",
                _ => "unknown",
            };
            match &breaker.state {
                BreakerState::Open { until, .. } => format!(
                    "health: {kind_name} breaker open until {}",
                    until.to_rfc3339_opts(SecondsFormat::Secs, true)
                ),
                _ => format!("health: {kind_name} breaker open"),
            }
        }
        _ => "skipped".to_string(),
    }
}

/// Builds `ExplainReport`s as a pure projection of `DeclarativeRouter`'s
/// evaluation -- the "why did this model run" answer, sharing its filtering
/// logic with `resolve` by construction rather than duplicating it.
pub struct RoutingExplain<'a> {
    router: &'a DeclarativeRouter,
}

impl<'a> RoutingExplain<'a> {
    pub fn new(router: &'a DeclarativeRouter) -> RoutingExplain<'a> {
        RoutingExplain { router }
    }

    /// Synchronous, no I/O: reads `router.evaluate(req)` (no filtering logic
    /// of its own) plus one health-registry read and one capability-index
    /// lookup per candidate -- never calls `HealthRegistry::record`.
    pub fn explain(&self, req: &RouteRequest) -> ExplainReport {
        let generated_at = Utc::now();

        match self.router.evaluate(req) {
            Ok(evaluation) => {
                let entries = evaluation
                    .entries
                    .into_iter()
                    .map(|entry| {
                        let model_ref = entry.model_ref.clone();
                        let capabilities = self
                            .router
                            .capability_index()
                            .get(&model_ref)
                            .map(CapabilitySummary::from);
                        let breaker = BreakerSnapshot {
                            state: self.router.health().state(&endpoint_of(&model_ref)),
                        };
                        let outcome = match entry.outcome {
                            EvalOutcome::Selected(reason) => EntryOutcome::Selected { reason },
                            EvalOutcome::Skipped(reason) => EntryOutcome::Skipped { reason },
                        };
                        ExplainEntry {
                            model_ref,
                            chain_position: entry.chain_position,
                            outcome,
                            capabilities,
                            breaker,
                        }
                    })
                    .collect();

                ExplainReport {
                    role: req.role.clone(),
                    pin: req.pin.clone(),
                    est_tokens: req.est_tokens,
                    required: req.required.clone(),
                    headroom_tokens: evaluation.headroom_tokens,
                    entries,
                    generated_at,
                }
            }
            // See the module-level "Third note": UnknownRole is the one
            // evaluate() error, reachable only for a non-pinned request
            // naming an unconfigured role.
            Err(_unknown_role) => ExplainReport {
                role: req.role.clone(),
                pin: req.pin.clone(),
                est_tokens: req.est_tokens,
                required: req.required.clone(),
                headroom_tokens: 0,
                entries: Vec::new(),
                generated_at,
            },
        }
    }
}
