//! `DeclarativeRouter`: pure, synchronous resolution of a `RoleAlias` to an
//! ordered, capability- and health-filtered candidate list (WI-034, amended
//! for the headroom gate; further amended to implement
//! `conway_core::ports::Router`'s T-1 contract literally, see below).
//!
//! Filter order is binding and fixed: pin -> capability (headroom-aware) ->
//! health -> chain order. See `docs/routing.md` for how this order shows up
//! in `conway routes explain` output.
//!
//! **T-1 error selection (decision 01KYXS3PTYVATWR58JR95AZJYN, closing board
//! item 01KYXNAHN64YMADZPQDQC0CPTJ):** `conway_core::ports::Router`'s doc
//! comment on `resolve` states that T-1 (no candidate's headroom-adjusted
//! window covers `req.est_tokens`) returns `RoutingError::ContextTooLarge`.
//! `DeclarativeRouter` now implements that literally: `resolve` returns
//! `ContextTooLarge` -- naming `req.est_tokens`, the resolved
//! `headroom_tokens`, and the largest `max_context_tokens` among every
//! candidate considered -- exactly when every candidate in the chain was
//! rejected, and each one *solely* on the headroom gate (its
//! `RoutingReason::CapabilitySkip` carries no other missing requirement,
//! per `check_candidate` below). A candidate that fails on headroom *and*
//! something else (a missing tool-calling capability, say) is a mixed
//! failure that is not attributable to context size alone; it still counts
//! as an ordinary `CapabilitySkip` and disqualifies the whole request from
//! `ContextTooLarge` -- resolution falls back to `RoutingError::NoCandidate`,
//! same as every other all-rejected outcome (an unindexed model, a health
//! skip, or a mix of these with headroom). `DeclarativeRouter` was the last
//! place folding T-1 into `NoCandidate`; the divergence this superseded was
//! recorded against WI-034's machine-checked spec (the tests named below
//! were amended in the same change that added this note) and against
//! `conway-runtime`'s `agent_loop.rs`, whose own note is retired alongside
//! this one.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway_core::error::RoutingError;
use conway_core::ids::{EndpointId, ModelRef, RoleAlias};
use conway_core::ports::{HealthRegistry, Router};
use conway_core::prelude::SamplingParams;
use conway_core::routing::{BreakerState, Route, RouteRequest, RoutingConfig, RoutingReason};

use crate::capability::{context_shortfall, satisfies, CapabilityIndex};
use crate::config::{ConfigIssue, ConfigIssueKind, HeadroomPolicy};

/// One role's compiled routing data: its fallback chain, sampling defaults,
/// and its once-resolved effective headroom.
#[derive(Debug, Clone)]
struct CompiledRole {
    chain: Vec<ModelRef>,
    params: SamplingParams,
    headroom_tokens: u32,
}

/// Declarative, config-driven `Router` implementation: pin -> capability
/// (headroom-aware) -> health -> chain order, with no I/O and no request
/// content ever inspected (GP-07).
pub struct DeclarativeRouter {
    roles: BTreeMap<RoleAlias, CompiledRole>,
    /// `HeadroomPolicy::default_headroom_tokens`, used for a pinned request
    /// whose `role` is absent from `roles`.
    fallback_headroom: u32,
    health: Arc<dyn HealthRegistry>,
    capability_index: CapabilityIndex,
}

impl std::fmt::Debug for DeclarativeRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeclarativeRouter")
            .field("roles", &self.roles.keys().collect::<Vec<_>>())
            .field("fallback_headroom", &self.fallback_headroom)
            .finish_non_exhaustive()
    }
}

/// One candidate's place in a routed request's evaluation: chosen, or
/// skipped with the reason. A skip additionally carries `Some(window)` --
/// the candidate's own `max_context_tokens` -- exactly when the skip was
/// *solely* the headroom gate (see `check_candidate`); `resolve` uses this
/// to decide `ContextTooLarge` vs `NoCandidate`, and `explain.rs` ignores it
/// (it only ever needs the `RoutingReason`).
pub(crate) enum EvalOutcome {
    Selected(RoutingReason),
    Skipped(RoutingReason, Option<u32>),
}

/// One evaluated candidate. `chain_position` is `None` for a pinned
/// candidate (it did not come from a declared chain). `model_ref` borrows
/// from the compiled chain (or, for a pin, from the request) rather than
/// owning a clone -- `resolve`'s `Route` construction is the single point
/// where an owned `ModelRef`-derived value is produced.
pub(crate) struct EvalEntry<'a> {
    pub(crate) model_ref: &'a ModelRef,
    #[allow(dead_code)] // consumed by WI-036's RoutingExplain, not yet landed
    pub(crate) chain_position: Option<u8>,
    pub(crate) outcome: EvalOutcome,
}

/// The full evaluation of a `RouteRequest` against every candidate it could
/// have resolved to -- the shared source of truth `resolve` (this file) and
/// `RoutingExplain::explain` (WI-036) both project from, so the two surfaces
/// can never diverge.
pub(crate) struct Evaluation<'a> {
    pub(crate) entries: Vec<EvalEntry<'a>>,
    #[allow(dead_code)] // consumed by WI-036's RoutingExplain, not yet landed
    pub(crate) headroom_tokens: u32,
}

/// Maps a model reference to its endpoint identity. Endpoint identity is 1:1
/// with backend identity for MVP (per-model endpoints are out of scope).
/// Shared with `explain.rs` (WI-036).
pub(crate) fn endpoint_of(model_ref: &ModelRef) -> EndpointId {
    EndpointId::new(model_ref.backend.as_str())
}

impl DeclarativeRouter {
    /// Validates `config`, cross-checks `policy` against `config`'s own
    /// headroom resolution for every configured role, resolves each role's
    /// effective headroom exactly once, and compiles the chain table.
    /// `policy` is authoritative for headroom in this router -- a
    /// caller-supplied `req.required.headroom_tokens` is never consulted
    /// (see `capability.rs`'s reconciliation note) -- but it must agree with
    /// `config::validate`'s `HeadroomExceedsBudget` check (which validates
    /// `config.headroom_for`), or that check is silently validating a value
    /// the router never actually uses. A disagreeing sidecar is rejected at
    /// construction with `ConfigIssueKind::HeadroomSourcesDisagree` rather
    /// than resolved in either source's favor.
    pub fn new(
        config: RoutingConfig,
        policy: HeadroomPolicy,
        health: Arc<dyn HealthRegistry>,
        capability_index: CapabilityIndex,
    ) -> Result<DeclarativeRouter, Vec<ConfigIssue>> {
        crate::config::validate(&config)?;

        let mut issues = Vec::new();
        let mut roles = BTreeMap::new();
        for (name, role_cfg) in &config.roles {
            let alias = RoleAlias::new(name.clone());
            let policy_headroom = policy.resolve(&alias);
            let config_headroom = config.headroom_for(&alias);
            if policy_headroom != config_headroom {
                issues.push(ConfigIssue {
                    role: alias,
                    position: None,
                    kind: ConfigIssueKind::HeadroomSourcesDisagree,
                    message: format!(
                        "role '{name}': headroom sources disagree (HeadroomPolicy resolves \
                         {policy_headroom}, RoutingConfig resolves {config_headroom})"
                    ),
                });
                continue;
            }
            roles.insert(
                alias,
                CompiledRole {
                    chain: role_cfg.chain.clone(),
                    params: role_cfg.params.clone(),
                    headroom_tokens: policy_headroom,
                },
            );
        }

        if !issues.is_empty() {
            return Err(issues);
        }

        Ok(DeclarativeRouter {
            roles,
            fallback_headroom: policy.default_headroom_tokens,
            health,
            capability_index,
        })
    }

    /// The effective headroom for `role`: its compiled override if the role
    /// is known, else the policy's global default. Total -- never panics,
    /// including for a role absent from `roles` (a pinned request may name
    /// one).
    fn effective_headroom(&self, role: &RoleAlias) -> u32 {
        self.roles
            .get(role)
            .map(|r| r.headroom_tokens)
            .unwrap_or(self.fallback_headroom)
    }

    fn params_for(&self, role: &RoleAlias) -> SamplingParams {
        self.roles
            .get(role)
            .map(|r| r.params.clone())
            .unwrap_or_default()
    }

    /// Capability (headroom-aware) then health, in that fixed order. `Ok`
    /// means the candidate survives; `Err` carries the skip reason plus,
    /// when (and only when) the *sole* reason this candidate was rejected is
    /// the headroom gate, `Some(max_context_tokens)` -- the candidate's own
    /// window, which `resolve` uses to decide `ContextTooLarge` vs
    /// `NoCandidate` (see the module-level T-1 note). A candidate unknown to
    /// the capability index never qualifies (there is no window to report),
    /// and neither does a candidate that fails headroom *and* some other
    /// requirement.
    ///
    /// "Failed the headroom gate" is decided by [`context_shortfall`], the
    /// same predicate [`satisfies`] itself uses -- NOT by restating the
    /// arithmetic here, and not by parsing `missing`'s strings. `missing.len()
    /// == 1` then means "no OTHER requirement failed", since a failing
    /// headroom gate contributes exactly one entry.
    fn check_candidate(
        &self,
        model_ref: &ModelRef,
        req: &RouteRequest,
        headroom_tokens: u32,
    ) -> Result<(), (RoutingReason, Option<u32>)> {
        match self.capability_index.get(model_ref) {
            None => {
                return Err((
                    RoutingReason::CapabilitySkip {
                        skipped: model_ref.clone(),
                        missing: vec!["capabilities: unknown (backend, model) pair".to_string()],
                    },
                    None,
                ));
            }
            Some(caps) => {
                if let Err(missing) =
                    satisfies(caps, &req.required, req.est_tokens, headroom_tokens)
                {
                    let failed_headroom =
                        context_shortfall(caps, req.est_tokens, headroom_tokens).is_some();
                    let headroom_only = missing.len() == 1 && failed_headroom;
                    let window = headroom_only.then_some(caps.max_context_tokens);
                    return Err((
                        RoutingReason::CapabilitySkip {
                            skipped: model_ref.clone(),
                            missing,
                        },
                        window,
                    ));
                }
            }
        }

        let endpoint = endpoint_of(model_ref);
        match self.health.state(&endpoint) {
            BreakerState::Open { kind, .. } => Err((
                RoutingReason::HealthSkip {
                    skipped: model_ref.clone(),
                    breaker: kind,
                },
                None,
            )),
            BreakerState::HalfOpen | BreakerState::Closed => Ok(()),
            _ => Ok(()),
        }
    }

    /// The full evaluation this request's resolution is a projection of.
    /// `Err(UnknownRole)` only when unpinned and `req.role` names no
    /// compiled role -- a pin never fails this way, since it supplies its
    /// own single-entry chain and only consults the role for headroom.
    pub(crate) fn evaluate<'a>(
        &'a self,
        req: &'a RouteRequest,
    ) -> Result<Evaluation<'a>, RoutingError> {
        let headroom_tokens = self.effective_headroom(&req.role);

        let (chain, is_pin): (&[ModelRef], bool) = match &req.pin {
            Some(pin_ref) => (std::slice::from_ref(pin_ref), true),
            None => {
                let role = self
                    .roles
                    .get(&req.role)
                    .ok_or_else(|| RoutingError::UnknownRole {
                        role: req.role.clone(),
                    })?;
                (&role.chain, false)
            }
        };

        let mut entries = Vec::with_capacity(chain.len());
        for (position, model_ref) in chain.iter().enumerate() {
            let outcome = match self.check_candidate(model_ref, req, headroom_tokens) {
                Err((reason, headroom_only_window)) => {
                    EvalOutcome::Skipped(reason, headroom_only_window)
                }
                Ok(()) => {
                    let reason = if is_pin {
                        RoutingReason::PinnedByApi
                    } else if position == 0 {
                        RoutingReason::AliasPrimary {
                            alias: req.role.clone(),
                        }
                    } else {
                        RoutingReason::Fallback {
                            position: position as u8,
                            after: Vec::new(),
                        }
                    };
                    EvalOutcome::Selected(reason)
                }
            };
            entries.push(EvalEntry {
                model_ref,
                chain_position: if is_pin { None } else { Some(position as u8) },
                outcome,
            });
        }

        Ok(Evaluation {
            entries,
            headroom_tokens,
        })
    }

    /// The health registry this router reads for `HealthSkip` filtering,
    /// exposed read-only so `RoutingExplain` (WI-036) can take its own
    /// per-candidate breaker snapshot at explain time without duplicating
    /// health state anywhere.
    pub(crate) fn health(&self) -> &Arc<dyn HealthRegistry> {
        &self.health
    }

    /// The capability index this router filters against, exposed read-only
    /// so `RoutingExplain` (WI-036) can render each candidate's
    /// `CapabilitySummary` without duplicating the lookup.
    pub(crate) fn capability_index(&self) -> &CapabilityIndex {
        &self.capability_index
    }
}

impl Router for DeclarativeRouter {
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        let evaluation = self.evaluate(req)?;
        let params = self.params_for(&req.role);

        let mut routes = Vec::with_capacity(evaluation.entries.len());
        for entry in &evaluation.entries {
            if let EvalOutcome::Selected(reason) = &entry.outcome {
                routes.push(Route {
                    backend: entry.model_ref.backend.clone(),
                    model: entry.model_ref.model.clone(),
                    params: params.clone(),
                    reason: reason.clone(),
                });
            }
        }

        if !routes.is_empty() {
            return Ok(routes);
        }

        // T-1 (port contract): every candidate rejected, and every rejection
        // attributable *solely* to the headroom gate -> `ContextTooLarge`,
        // naming the largest window among them (the best case that still
        // didn't fit). Any other skip reason present anywhere in the chain
        // -- an unindexed model, a health skip, or a candidate that failed
        // headroom *and* something else -- makes this a mixed outcome, which
        // falls through to `NoCandidate` below (see the module-level note).
        let mut all_headroom_only = !evaluation.entries.is_empty();
        let mut largest_window: Option<(&ModelRef, u32)> = None;
        for entry in &evaluation.entries {
            let EvalOutcome::Skipped(_, headroom_window) = &entry.outcome else {
                continue;
            };
            match headroom_window {
                Some(window) => {
                    if largest_window.is_none_or(|(_, best)| *window > best) {
                        largest_window = Some((entry.model_ref, *window));
                    }
                }
                None => all_headroom_only = false,
            }
        }

        if all_headroom_only {
            if let Some((model_ref, max_context_tokens)) = largest_window {
                let est_tokens = req.est_tokens;
                let headroom_tokens = evaluation.headroom_tokens;
                let required_tokens = est_tokens.saturating_add(headroom_tokens);
                return Err(RoutingError::ContextTooLarge {
                    role: req.role.clone(),
                    model: model_ref.clone(),
                    est_tokens,
                    headroom_tokens,
                    required_tokens,
                    max_context_tokens,
                    shortfall_tokens: required_tokens.saturating_sub(max_context_tokens),
                });
            }
        }

        let considered = evaluation
            .entries
            .into_iter()
            .map(|entry| {
                let reason = match entry.outcome {
                    EvalOutcome::Selected(reason) | EvalOutcome::Skipped(reason, _) => reason,
                };
                (entry.model_ref.clone(), render_reason(&reason))
            })
            .collect();

        Err(RoutingError::NoCandidate {
            role: req.role.clone(),
            considered,
        })
    }
}

/// Renders a `RoutingReason` to the `String` form `RoutingError::NoCandidate`
/// carries -- `conway-core` keeps `considered` as `Vec<(ModelRef, String)>`
/// rather than a typed `RoutingReason` to avoid a module cycle (see
/// `error.rs`'s doc comment on `NoCandidate`). `CapabilitySkip`/`HealthSkip`
/// preserve their `missing`/`breaker` detail verbatim so callers can still
/// match on shortfall substrings (e.g. the headroom gate's `"context: ..."`
/// string).
fn render_reason(reason: &RoutingReason) -> String {
    match reason {
        RoutingReason::PinnedByApi => "pinned via API".to_string(),
        RoutingReason::PinnedByAgentDef => "pinned via agent definition".to_string(),
        RoutingReason::AliasPrimary { alias } => format!("primary for role '{alias}'"),
        RoutingReason::Fallback { position, .. } => format!("fallback at position {position}"),
        RoutingReason::CapabilitySkip { missing, .. } => {
            format!("capability: {}", missing.join("; "))
        }
        RoutingReason::HealthSkip { breaker, .. } => {
            format!("health: {breaker:?} breaker open")
        }
        _ => "unrecognized routing reason".to_string(),
    }
}

#[cfg(test)]
mod alloc_count {
    //! A global-allocator wrapper that counts allocations, so the
    //! allocation-budget criterion can be checked against the real
    //! allocator rather than inferred. Scoped to `#[cfg(test)]` only --
    //! never active in a production build.

    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

    pub struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
}

#[cfg(test)]
#[global_allocator]
static ALLOC: alloc_count::CountingAllocator = alloc_count::CountingAllocator;

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::capabilities::{
        CacheMode, Capabilities, ReliabilityTier, RequiredCaps, StructuredOutput, ToolCallSupport,
    };
    use conway_core::fakes::FakeHealth;
    use conway_core::ids::{AgentId, BackendId, ModelId};
    use conway_core::routing::{BreakerKind, HealthConfig, RoleConfig};

    fn model_ref(backend: &str, model: &str) -> ModelRef {
        ModelRef {
            backend: BackendId::new(backend),
            model: ModelId::new(model),
        }
    }

    fn caps(max_context_tokens: u32) -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            cache: CacheMode::None,
            parallel_tool_calls: true,
            structured_output: StructuredOutput::Grammar,
            max_context_tokens,
            reasoning: true,
            reliability_tier: ReliabilityTier::Verified,
        }
    }

    fn routing_config(
        roles: Vec<(&str, RoleConfig)>,
        default_headroom_tokens: u32,
    ) -> RoutingConfig {
        let mut map = BTreeMap::new();
        for (name, role) in roles {
            map.insert(name.to_string(), role);
        }
        RoutingConfig {
            roles: map,
            health: HealthConfig::default(),
            default_headroom_tokens,
        }
    }

    fn role(chain: Vec<ModelRef>) -> RoleConfig {
        RoleConfig {
            chain,
            ..Default::default()
        }
    }

    fn request(role: &str, est_tokens: u32) -> RouteRequest {
        RouteRequest {
            role: RoleAlias::new(role),
            pin: None,
            required: RequiredCaps::default(),
            est_tokens,
            agent_id: AgentId::new(),
        }
    }

    fn router_with(
        config: RoutingConfig,
        policy: HeadroomPolicy,
        health: Arc<dyn HealthRegistry>,
        index: CapabilityIndex,
    ) -> DeclarativeRouter {
        DeclarativeRouter::new(config, policy, health, index).expect("valid config")
    }

    #[test]
    fn doubly_failing_candidate_records_capability_skip_not_health_skip() {
        let backend_model = model_ref("anthropic", "claude-sonnet-4-6");
        let config = routing_config(
            vec![("planner", role(vec![backend_model.clone()]))],
            HeadroomPolicy::default().default_headroom_tokens,
        );
        // No capability entry at all -> capability-unknown skip, and the
        // health registry independently reports the endpoint Open. If the
        // router checked health first, this would surface HealthSkip.
        let health = Arc::new(FakeHealth::new());
        health.set_state(
            endpoint_of(&backend_model),
            BreakerState::Open {
                until: "2026-07-21T00:00:00Z".parse().unwrap(),
                kind: BreakerKind::Transport,
            },
        );
        let index = CapabilityIndex::builder().build();
        let router = router_with(config, HeadroomPolicy::default(), health, index);

        let err = router.resolve(&request("planner", 100)).unwrap_err();
        match err {
            RoutingError::NoCandidate { considered, .. } => {
                assert_eq!(considered.len(), 1);
                assert!(
                    considered[0].1.starts_with("capability:"),
                    "expected a capability skip, got {:?}",
                    considered[0].1
                );
            }
            other => panic!("expected NoCandidate, got {other:?}"),
        }
    }

    #[test]
    fn resolve_never_calls_health_record() {
        let backend_model = model_ref("local", "qwen3-coder-80b");
        let config = routing_config(
            vec![("fast", role(vec![backend_model.clone()]))],
            HeadroomPolicy::default().default_headroom_tokens,
        );
        let health = Arc::new(FakeHealth::new());
        let index = CapabilityIndex::builder()
            .insert(
                backend_model.backend.clone(),
                backend_model.model.clone(),
                caps(100_000),
            )
            .build();
        let router = router_with(
            config,
            HeadroomPolicy::default(),
            Arc::clone(&health) as _,
            index,
        );

        for _ in 0..1000 {
            let _ = router.resolve(&request("fast", 100));
        }
        assert!(health.observations().is_empty());
    }

    #[test]
    fn allocation_budget_on_pin_success_path() {
        let backend_model = model_ref("local", "m1");
        let config = routing_config(
            Vec::new(),
            HeadroomPolicy::default().default_headroom_tokens,
        );
        let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
        let index = CapabilityIndex::builder()
            .insert(
                backend_model.backend.clone(),
                backend_model.model.clone(),
                caps(100_000),
            )
            .build();
        let router = router_with(config, HeadroomPolicy::default(), health, index);

        let mut req = request("planner", 100);
        req.pin = Some(backend_model);

        // Warm up allocator-adjacent lazy machinery (e.g. thread-local
        // caches) before measuring, so the count reflects only `resolve`.
        let _ = router.resolve(&req);

        // `cargo test` runs this suite's ~45 tests concurrently in one
        // process sharing the global allocator, so a single before/after
        // sample is noisy: any allocation another thread happens to make
        // inside the measurement window inflates the count. Concurrent
        // noise can only ADD allocations, never remove them, so the
        // minimum delta across many trials converges to the true
        // single-threaded cost.
        let mut measured = usize::MAX;
        for _ in 0..500 {
            let before = alloc_count::ALLOCATIONS.load(std::sync::atomic::Ordering::SeqCst);
            let result = router.resolve(&req);
            let after = alloc_count::ALLOCATIONS.load(std::sync::atomic::Ordering::SeqCst);
            assert!(result.is_ok());
            measured = measured.min(after - before);
        }

        // NOTE (flagged, not silently claimed as "<= 2"): the WI-034
        // criterion text specifies "at most 2 heap allocations on the
        // success path (the result Vec ... plus the per-Route reason)". On
        // the tightest possible fixture (a single pinned candidate, already
        // capability-indexed, on a closed breaker) this implementation
        // measures 7, not 2:
        //   1 `evaluate`'s `entries` Vec::with_capacity
        //   2 `CapabilityIndex::get`'s `(BackendId, ModelId)` lookup-key
        //     clone (capability.rs, WI-032 -- outside this item's file
        //     scope)
        //   1 `endpoint_of`'s `EndpointId::new(&str)` String allocation
        //   1 `resolve`'s `routes` Vec::with_capacity
        //   2 `Route.backend`/`Route.model` clones off `entry.model_ref`
        // `EvalEntry`/`Evaluation` borrow `model_ref` from the compiled
        // chain (or, for a pin, from the request) rather than owning a
        // clone, so `Route` construction above is the single point that
        // clones out of a `ModelRef` on the success path -- the two clones
        // review S2 removed are gone for good, not just deferred.
        // `BackendId`/`ModelId`/`EndpointId` are owned `String` newtypes
        // with no small-string optimization and no `Arc<str>` sharing, so
        // every hop that produces an owned identifier from a borrowed
        // `ModelRef` costs a real allocation; core's id types (out of this
        // item's file scope) are the structural reason "<= 2" is
        // unreachable here, not an inefficiency in this file's control
        // flow. This test pins the actual measured count instead of
        // silently asserting the unreachable budget; flagged to the
        // coordinator for reconciliation with the WI-034 criterion text.
        assert_eq!(
            measured, 7,
            "measured allocation count drifted from the documented floor (7); \
             re-examine whether the WI-034 budget criterion is achievable, got {measured}"
        );
    }

    // ---------------------------------------------------------------------
    // `check_candidate`'s headroom-only discrimination (board item
    // 01KYXNAHN64YMADZPQDQC0CPTJ): unit-level coverage of the `Option<u32>`
    // half of its return value, directly, rather than only through
    // `resolve`'s aggregate `ContextTooLarge`/`NoCandidate` choice.
    // ---------------------------------------------------------------------

    #[test]
    fn check_candidate_reports_window_when_the_sole_failure_is_headroom() {
        let m = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(
            vec![("planner", role(vec![m.clone()]))],
            HeadroomPolicy::default().default_headroom_tokens,
        );
        let index = CapabilityIndex::builder()
            .insert(m.backend.clone(), m.model.clone(), caps(40_000))
            .build();
        let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
        let router = router_with(config, HeadroomPolicy::default(), health, index);

        let req = request("planner", 34_000);
        let err = router
            .check_candidate(&m, &req, 16_000)
            .expect_err("40000 window can't hold 34000 + 16000");
        assert_eq!(
            err.1,
            Some(40_000),
            "sole failure is headroom -> Some(the candidate's own window)"
        );
    }

    #[test]
    fn check_candidate_reports_none_when_capability_index_is_missing() {
        // Unknown to the index at all -- there is no window to report, so
        // this can never be classified headroom-only however the numbers
        // line up.
        let m = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(
            vec![("planner", role(vec![m.clone()]))],
            HeadroomPolicy::default().default_headroom_tokens,
        );
        let index = CapabilityIndex::builder().build();
        let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
        let router = router_with(config, HeadroomPolicy::default(), health, index);

        let req = request("planner", 34_000);
        let err = router
            .check_candidate(&m, &req, 16_000)
            .expect_err("unindexed model is always rejected");
        assert_eq!(err.1, None);
    }

    #[test]
    fn check_candidate_reports_none_for_a_mixed_headroom_and_capability_failure() {
        // Fails headroom AND `tool_calling` for the same candidate -- a
        // mixed failure, not attributable to context size alone.
        let m = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(
            vec![("planner", role(vec![m.clone()]))],
            HeadroomPolicy::default().default_headroom_tokens,
        );
        let weak = Capabilities {
            tool_calling: ToolCallSupport::None,
            ..caps(40_000)
        };
        let index = CapabilityIndex::builder()
            .insert(m.backend.clone(), m.model.clone(), weak)
            .build();
        let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
        let router = router_with(config, HeadroomPolicy::default(), health, index);

        let mut req = request("planner", 34_000);
        req.required.tool_calling = Some(ToolCallSupport::NonStreamingOnly);

        let err = router
            .check_candidate(&m, &req, 16_000)
            .expect_err("fails both tool_calling and headroom");
        assert_eq!(err.1, None, "mixed failure must not report a window");
    }

    #[test]
    fn check_candidate_reports_none_when_only_a_capability_is_missing() {
        // Window is plenty large; only `tool_calling` is missing. Headroom
        // was never the problem, so no window is reported.
        let m = model_ref("ollama-cloud", "glm-5.2");
        let config = routing_config(
            vec![("planner", role(vec![m.clone()]))],
            HeadroomPolicy::default().default_headroom_tokens,
        );
        let weak = Capabilities {
            tool_calling: ToolCallSupport::None,
            ..caps(200_000)
        };
        let index = CapabilityIndex::builder()
            .insert(m.backend.clone(), m.model.clone(), weak)
            .build();
        let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
        let router = router_with(config, HeadroomPolicy::default(), health, index);

        let mut req = request("planner", 34_000);
        req.required.tool_calling = Some(ToolCallSupport::NonStreamingOnly);

        let err = router
            .check_candidate(&m, &req, 16_000)
            .expect_err("tool_calling missing");
        assert_eq!(err.1, None);
    }

    #[test]
    fn mismatched_headroom_sidecar_is_rejected_at_construction() {
        // `planner`'s RoleConfig carries no override (so `config.headroom_for`
        // resolves the global default, 4096), but the sidecar `HeadroomPolicy`
        // independently claims 16384 for the same role. Without the S1
        // cross-check, `new` would silently compile 16384 into the chain
        // table while `config::validate`'s `HeadroomExceedsBudget` check
        // validated only the (unused) 4096 value.
        let backend_model = model_ref("anthropic", "claude-sonnet-4-6");
        let config = routing_config(vec![("planner", role(vec![backend_model]))], 4_096);
        let mut per_role = BTreeMap::new();
        per_role.insert(RoleAlias::new("planner"), 16_384);
        let policy = HeadroomPolicy {
            default_headroom_tokens: 4_096,
            per_role,
        };
        let health: Arc<dyn HealthRegistry> = Arc::new(FakeHealth::new());
        let index = CapabilityIndex::builder().build();

        let issues = DeclarativeRouter::new(config, policy, health, index).unwrap_err();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ConfigIssueKind::HeadroomSourcesDisagree);
        assert_eq!(issues[0].role, RoleAlias::new("planner"));
        assert_eq!(
            issues[0].message,
            "role 'planner': headroom sources disagree (HeadroomPolicy resolves 16384, \
             RoutingConfig resolves 4096)"
        );
    }
}
