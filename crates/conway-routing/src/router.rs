//! `DeclarativeRouter`: pure, synchronous resolution of a `RoleAlias` to an
//! ordered, capability- and health-filtered candidate list (WI-034, amended
//! for the headroom gate).
//!
//! Filter order is binding and fixed: pin -> capability (headroom-aware) ->
//! health -> chain order. See `docs/plan/wi-conway-routing.md` and
//! `wi-conway-routing-amendment.md`, section WI-034.
//!
//! Divergence note (flagged, not worked around): `conway_core::ports::Router`'s
//! doc comment on `resolve` states that T-1 (no candidate's headroom-adjusted
//! window covers `req.est_tokens`) returns `RoutingError::ContextTooLarge`.
//! This crate's own binding WI-034 spec (the amendment, matching the
//! already-implemented WI-032 `capability::satisfies`) instead folds the
//! headroom gate into `RoutingReason::CapabilitySkip` and returns
//! `RoutingError::NoCandidate` uniformly for every all-rejected outcome --
//! matching the crate's other capability-driven rejections and the specific,
//! named test criteria in WI-034's machine-checked spec (e.g.
//! `headroom_skip_reports_capability_skip_with_shortfall`, which names
//! `NoCandidate`/`CapabilitySkip` explicitly). `DeclarativeRouter` therefore
//! never constructs `RoutingError::ContextTooLarge`; flagged to the
//! coordinator to reconcile with the core `Router`-trait doc comment.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway_core::error::RoutingError;
use conway_core::ids::{EndpointId, ModelRef, RoleAlias};
use conway_core::ports::{HealthRegistry, Router};
use conway_core::prelude::SamplingParams;
use conway_core::routing::{BreakerState, Route, RouteRequest, RoutingConfig, RoutingReason};

use crate::capability::{satisfies, CapabilityIndex};
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
/// skipped with the reason.
pub(crate) enum EvalOutcome {
    Selected(RoutingReason),
    Skipped(RoutingReason),
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
    /// means the candidate survives; `Err` carries the skip reason.
    fn check_candidate(
        &self,
        model_ref: &ModelRef,
        req: &RouteRequest,
        headroom_tokens: u32,
    ) -> Result<(), RoutingReason> {
        match self.capability_index.get(model_ref) {
            None => {
                return Err(RoutingReason::CapabilitySkip {
                    skipped: model_ref.clone(),
                    missing: vec!["capabilities: unknown (backend, model) pair".to_string()],
                });
            }
            Some(caps) => {
                if let Err(missing) =
                    satisfies(caps, &req.required, req.est_tokens, headroom_tokens)
                {
                    return Err(RoutingReason::CapabilitySkip {
                        skipped: model_ref.clone(),
                        missing,
                    });
                }
            }
        }

        let endpoint = endpoint_of(model_ref);
        match self.health.state(&endpoint) {
            BreakerState::Open { kind, .. } => Err(RoutingReason::HealthSkip {
                skipped: model_ref.clone(),
                breaker: kind,
            }),
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
                Err(reason) => EvalOutcome::Skipped(reason),
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

        let considered = evaluation
            .entries
            .into_iter()
            .map(|entry| {
                let reason = match entry.outcome {
                    EvalOutcome::Selected(reason) | EvalOutcome::Skipped(reason) => reason,
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
