//! The content-free routing request/response contract -- routing is
//! content-blind and predictable -- plus the routing
//! reason vocabulary, health/breaker state, the declarative routing config
//! types, the "why did this model run" explain-report shape, and a minimal
//! config-only `Router`/`RoutingExplainer` fallback (`MinimalRouter`) usable
//! without depending on `conway-routing` at all.
//!
//! `Router::resolve` (defined as a port trait in an earlier item) never consults
//! request *content* — [`RouteRequest`] is constructed so that no field can
//! carry prompt text. This is a compile-time guarantee, not a convention: a
//! unit test below asserts the field set is exactly the five documented
//! fields.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::capabilities::{
    Capabilities, ReliabilityTier, RequiredCaps, StructuredOutput, ToolCallSupport,
    DEFAULT_HEADROOM_TOKENS,
};
use crate::content::SamplingParams;
use crate::error::RoutingError;
use crate::ids::{AgentId, BackendId, EndpointId, ModelId, ModelRef, RoleAlias};
use crate::ports::{HealthRegistry, Router, RoutingExplainer};

fn default_headroom_tokens() -> u32 {
    DEFAULT_HEADROOM_TOKENS
}

/// A request to resolve a routing role to an ordered candidate list.
///
/// Deliberately has no field of type `String`, `Vec<ContentBlock>`,
/// `PromptSegment`, or `Message` that could carry prompt text, so routing
/// cannot become content-aware by accident. Reasoning-headroom rides on
/// `required.headroom_tokens`, not as a separate top-level field, so this
/// five-field guarantee stays mechanically checkable.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteRequest {
    pub role: RoleAlias,
    pub pin: Option<ModelRef>,
    pub required: RequiredCaps,
    pub est_tokens: u32,
    pub agent_id: AgentId,
}

impl RouteRequest {
    /// The total token window this request will occupy, so callers never
    /// recompute the est_tokens + headroom_tokens sum by hand.
    pub fn total_required(&self) -> u32 {
        self.required.total_required(self.est_tokens)
    }
}

/// A single resolved routing candidate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    pub backend: BackendId,
    pub model: ModelId,
    pub params: SamplingParams,
    pub reason: RoutingReason,
}

/// Why a route was chosen, or why a candidate was skipped.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutingReason {
    PinnedByApi,
    PinnedByAgentDef,
    AliasPrimary {
        alias: RoleAlias,
    },
    Fallback {
        position: u8,
        after: Vec<AttemptFailure>,
    },
    CapabilitySkip {
        skipped: ModelRef,
        missing: Vec<String>,
    },
    HealthSkip {
        skipped: ModelRef,
        breaker: BreakerKind,
    },
}

/// A prior failed attempt, recorded as part of a `Fallback` reason.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttemptFailure {
    pub model: ModelRef,
    pub error: String,
    pub at: DateTime<Utc>,
}

/// The circuit breaker kind tracked per endpoint (Olla pattern).
///
/// A second, independent `Probe` variant — fed by a periodic health prober
/// decoupled from request traffic — used to exist here. It was retired, not
/// wired: the prober that would
/// have fed it had no production call site anywhere in this tree, and the
/// Transport breaker alone already handles recovery (a clock read takes it
/// half-open; the next real request retries), so wiring it would only have
/// shaved latency off the first request after an outage — an optimization
/// this project gates on a measured baseline that neither existed nor was
/// scheduled. `#[non_exhaustive]` is kept so a future breaker kind can be
/// added without a semver break.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerKind {
    Transport,
}

/// A circuit breaker's current state.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open {
        until: DateTime<Utc>,
        kind: BreakerKind,
    },
    HalfOpen,
}

/// A health observation fed to `HealthRegistry::record`.
///
/// `BadRequest`, `Auth`, and `ContextOverflow` deliberately have no
/// `Observation` representation (§8) — they are request problems, not
/// endpoint-health signals. Headroom exists specifically to convert most
/// would-be `ContextOverflow` failures into pre-flight `CapabilitySkip` /
/// `ContextTooLarge` decisions.
///
/// A `ProbeFail` variant, fed exclusively by the now-retired periodic health
/// prober, used to exist here. It
/// was removed along with its only producer rather than left unconstructible
/// beside a live variant.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observation {
    Ok { latency_ms: u32 },
    TransportError,
    ServerError,
    RateLimited { retry_after_secs: Option<u64> },
}

/// A single breaker read at explain time. Carries the `HealthRegistry`
/// port's merged view (`state`), not an independent `{transport, probe}`
/// pair -- `conway-routing`'s `RoutingExplain` (this type's other producer)
/// documents why that split is unreachable through the port; `MinimalRouter`
/// below never has independent breaker state at all, so every entry it
/// produces carries `Closed`.
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
/// **Moved here from `conway-routing`,
/// replacing a dead, unreached second `ExplainReport` shape this module used
/// to declare on its own.** The type used to live only in `conway-routing`,
/// reachable exclusively through `RoutingExplain`'s projection of a concrete
/// `DeclarativeRouter` -- so a `Router` supplied from outside that crate
/// (`ConwayBuilder::with_router`) had no way to produce one, and
/// `Conway::explain_routing` fell back to a fabricated-empty report that
/// `conway routes explain` then misread as "unknown role" (a silent
/// inversion, not an honest degradation -- the bug this move exists to
/// close). It now lives here, where both `conway-routing::RoutingExplain`
/// (the rich, capability- and health-filtered answer) and `MinimalRouter`
/// (below; the honest degenerate answer core itself can produce with no
/// filtering at all) build the same shape. `conway-routing` re-exports these
/// five names for source compatibility.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplainReport {
    pub role: RoleAlias,
    pub pin: Option<ModelRef>,
    pub est_tokens: u32,
    /// The EFFECTIVE requirement every `entries` outcome was actually
    /// checked against, when the producer performs a real check
    /// (`conway-routing::RoutingExplain`: the role's configured floor
    /// merged with `req.required`). `MinimalRouter::explain` performs no
    /// check at all, so its value here is only the role's configured floor
    /// plus resolved headroom (`RoutingConfig::required_caps_for`) --
    /// informational, not a claim that any entry was actually verified
    /// against it, so nothing claims a reach it does not have.
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
            // `BreakerKind` is `#[non_exhaustive]` for OTHER crates; within
            // its own defining crate (this one, now that this type moved
            // here from conway-routing) every variant is already covered, so
            // a trailing wildcard would be unreachable dead code rather than
            // genuine forward-compatibility.
            let kind_name = match kind {
                BreakerKind::Transport => "transport",
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

// ---------------------------------------------------------------------
// Config types. Types only: loading (TOML parsing, env resolution, path
// discovery) lives in the `conway` facade, not here.
// ---------------------------------------------------------------------

/// The declarative routing policy: per-role fallback chains plus health
/// tuning. `BTreeMap`, not `HashMap`, so serialized config is deterministically
/// ordered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoutingConfig {
    pub roles: BTreeMap<String, RoleConfig>,
    pub health: HealthConfig,
    /// Global default reserved output/reasoning tokens, applied to any role
    /// without an override.
    #[serde(default = "default_headroom_tokens")]
    pub default_headroom_tokens: u32,
}

impl RoutingConfig {
    /// Resolves the effective headroom for a role. Precedence, fixed and
    /// total: per-role `RoleConfig::headroom_tokens` overrides
    /// `default_headroom_tokens`, which overrides
    /// [`DEFAULT_HEADROOM_TOKENS`] (that constant is only reachable if
    /// `default_headroom_tokens` itself is absent from the config, which the
    /// serde default on this field prevents in practice). Unknown roles get
    /// the global default.
    ///
    /// A caller-supplied `RouteRequest.required.headroom_tokens` set by the
    /// runtime (e.g. from an agent def) sits above all of this — this method
    /// is only consulted when *constructing* a `RouteRequest`, never when
    /// interpreting one that already carries a value.
    pub fn headroom_for(&self, role: &RoleAlias) -> u32 {
        self.roles
            .get(role.as_str())
            .and_then(|r| r.headroom_tokens)
            .unwrap_or(self.default_headroom_tokens)
    }

    /// Builds the filter input for a request: the role's `required` caps
    /// with `headroom_tokens` resolved from the override/default chain. An
    /// unknown role gets `RequiredCaps::default()` with headroom resolved
    /// the same way.
    pub fn required_caps_for(&self, role: &RoleAlias) -> RequiredCaps {
        let mut required = self
            .roles
            .get(role.as_str())
            .map(|r| r.required.clone())
            .unwrap_or_default();
        required.headroom_tokens = self.headroom_for(role);
        required
    }
}

/// One role's fallback chain, capability floor, and sampling defaults.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RoleConfig {
    pub chain: Vec<ModelRef>,
    pub required: RequiredCaps,
    pub params: SamplingParams,
    /// Per-role override of [`RoutingConfig::default_headroom_tokens`].
    #[serde(default)]
    pub headroom_tokens: Option<u32>,
}

/// Circuit-breaker tuning for the endpoint's `Transport` breaker.
///
/// Every field has a serde default, so a config document omitting `[health]`
/// keys (or the whole table) deserializes to [`HealthConfig::default`].
///
/// **The `probe_*` fields (`probe_interval_secs`, `probe_timeout_secs`,
/// `probe_failures_to_open`, `probe_enabled`) that used to configure a
/// second, independent `Probe` breaker were removed , not merely left unwired.** The periodic
/// health prober that would have fed that breaker had no production call
/// site anywhere in this tree — the Transport breaker alone handles recovery
/// (it goes half-open on a clock read, and the next real request retries),
/// so the prober fixed no correctness gap; it would only have shaved latency
/// off the first request after an outage, which made wiring it an
/// optimization requiring a measured baseline that neither existed nor
/// was scheduled. **Breaking:** a `settings.json`/`RoutingConfig` document
/// naming any of the four removed keys under `[health]` now fails to
/// deserialize (`#[serde(deny_unknown_fields)]` on the facade's
/// `HealthSection` mirror) rather than silently accepting and ignoring them.
/// Do not confuse any of this with `[models].probe_on_startup`
/// (`conway::config::schema::ModelsConfig`), a different, already-wired
/// startup CAPABILITY probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthConfig {
    pub transport_failures_to_open: u32,
    pub open_duration_secs: u64,
    /// Consecutive successful observations required to close a half-open
    /// breaker.
    pub half_open_successes_to_close: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            transport_failures_to_open: 3,
            open_duration_secs: 30,
            half_open_successes_to_close: 1,
        }
    }
}

/// One configured backend instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackendConfig {
    pub id: BackendId,
    pub kind: BackendKind,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub dialect: Option<String>,
    pub models: BTreeMap<String, ModelOverrides>,
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The dialect family a backend adapter speaks.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Anthropic,
    OpenAiCompat,
}

/// Per-model overrides layered onto a backend's declared capabilities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelOverrides {
    pub stream_tools: Option<bool>,
    pub max_context_tokens: Option<u32>,
    pub reliability_tier: Option<ReliabilityTier>,
    /// Per-model override for the parallel-tool-calls capability
    /// (overrides > metadata > dialect defaults, per conway-plugin-backends'
    /// capability precedence).
    pub parallel_tool_calls: Option<bool>,
    /// A floor, not an override: `conway-routing` applies
    /// `effective = max(request.headroom_tokens, min_headroom_tokens.unwrap_or(0))`.
    /// A model that reasons heavily can insist on more reserved space than a
    /// role requests, but cannot reduce it.
    pub min_headroom_tokens: Option<u32>,
}

// ---------------------------------------------------------------------
// Minimal fallback implementations.
//
// `crate::ports`'s own module doc reserves `conway-core` for feature-gated
// test fakes plus "every other implementation lives in a dedicated crate".
// These two types are the narrow, deliberate exception: `MinimalRouter` and
// `AlwaysClosedHealthRegistry` are production code, not test doubles -- they
// back `Conway::explain_routing`'s honest degenerate answer when the caller
// supplied its own `Router` (`ConwayBuilder::with_router`) and there is no
// concrete `conway_plugin_routing::DeclarativeRouter` left to project an
// `ExplainReport` through. Neither performs I/O, matching every other port
// implementation's constraint.
// ---------------------------------------------------------------------

/// A `HealthRegistry` that always reports `Closed` and records nothing.
/// Not a test double (`crate::fakes::FakeHealth`, gated behind `feature =
/// "fakes"`, is that): this is the honest production answer for a caller
/// that has no real breaker state to consult at all, per the module note
/// above.
#[derive(Clone, Copy, Debug, Default)]
pub struct AlwaysClosedHealthRegistry;

impl HealthRegistry for AlwaysClosedHealthRegistry {
    fn state(&self, _ep: &EndpointId) -> BreakerState {
        BreakerState::Closed
    }

    fn record(&self, _ep: &EndpointId, _obs: Observation) {}
}

/// A minimal, config-only `Router` + `RoutingExplainer`: no capability
/// filtering, no health filtering, no invented values -- nothing claims a
/// capability it does not have. `resolve` returns a role's configured chain in
/// order (or a pin's single-element chain); `explain` answers with one
/// degenerate `ExplainEntry` per chain entry -- the first `Selected`, the rest
/// `Skipped`, `capabilities: None` (this type indexes no capabilities) and
/// `breaker: BreakerSnapshot { state: Closed }` (paired with
/// [`AlwaysClosedHealthRegistry`] -- this type tracks no real breaker state
/// either). See the module-note above this section for why these two live in
/// `conway-core` at all.
#[derive(Clone, Debug)]
pub struct MinimalRouter {
    config: RoutingConfig,
}

impl MinimalRouter {
    pub fn new(config: RoutingConfig) -> MinimalRouter {
        MinimalRouter { config }
    }

    /// The chain this request resolves against, and whether it came from a
    /// pin -- `None` only when `req` is unpinned and names a role absent
    /// from `self.config.roles`.
    fn chain_for(&self, req: &RouteRequest) -> Option<(Vec<ModelRef>, bool)> {
        match &req.pin {
            Some(pin) => Some((vec![pin.clone()], true)),
            None => self
                .config
                .roles
                .get(req.role.as_str())
                .map(|role| (role.chain.clone(), false)),
        }
    }

    fn reason_for(is_pin: bool, position: usize, role: &RoleAlias) -> RoutingReason {
        if is_pin {
            RoutingReason::PinnedByApi
        } else if position == 0 {
            RoutingReason::AliasPrimary {
                alias: role.clone(),
            }
        } else {
            RoutingReason::Fallback {
                position: position as u8,
                after: Vec::new(),
            }
        }
    }
}

impl Router for MinimalRouter {
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        let Some((chain, is_pin)) = self.chain_for(req) else {
            return Err(RoutingError::UnknownRole {
                role: req.role.clone(),
            });
        };
        if chain.is_empty() {
            return Err(RoutingError::NoCandidate {
                role: req.role.clone(),
                considered: Vec::new(),
            });
        }

        let params = self
            .config
            .roles
            .get(req.role.as_str())
            .map(|role| role.params.clone())
            .unwrap_or_default();

        Ok(chain
            .iter()
            .enumerate()
            .map(|(position, model_ref)| Route {
                backend: model_ref.backend.clone(),
                model: model_ref.model.clone(),
                params: params.clone(),
                reason: Self::reason_for(is_pin, position, &req.role),
            })
            .collect())
    }
}

impl RoutingExplainer for MinimalRouter {
    fn explain(&self, req: &RouteRequest) -> ExplainReport {
        let generated_at = Utc::now();
        let (chain, is_pin) = self.chain_for(req).unwrap_or_default();

        let entries = chain
            .iter()
            .enumerate()
            .map(|(position, model_ref)| {
                let reason = Self::reason_for(is_pin, position, &req.role);
                let outcome = if position == 0 {
                    EntryOutcome::Selected { reason }
                } else {
                    EntryOutcome::Skipped { reason }
                };
                ExplainEntry {
                    model_ref: model_ref.clone(),
                    chain_position: if is_pin { None } else { Some(position as u8) },
                    outcome,
                    capabilities: None,
                    breaker: BreakerSnapshot {
                        state: BreakerState::Closed,
                    },
                }
            })
            .collect();

        ExplainReport {
            role: req.role.clone(),
            pin: req.pin.clone(),
            est_tokens: req.est_tokens,
            required: self.config.required_caps_for(&req.role),
            headroom_tokens: self.config.headroom_for(&req.role),
            entries,
            generated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_request_field_set_is_exactly_five_and_content_free() {
        let req = RouteRequest {
            role: RoleAlias::new("planner"),
            pin: None,
            required: RequiredCaps::default(),
            est_tokens: 100,
            agent_id: AgentId::new(),
        };
        let value = serde_json::to_value(&req).unwrap();
        let obj = value
            .as_object()
            .expect("RouteRequest must serialize to an object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["agent_id", "est_tokens", "pin", "required", "role"]
        );

        // No field could ever carry raw prompt text: nothing named `text`,
        // `content`, `prompt`, `message`, or `blocks`.
        for forbidden in ["text", "content", "prompt", "message", "blocks"] {
            assert!(
                !obj.contains_key(forbidden),
                "RouteRequest must not carry a {forbidden:?} field"
            );
        }
    }

    #[test]
    fn route_request_total_required_matches_required_caps() {
        let req = RouteRequest {
            role: RoleAlias::new("planner"),
            pin: None,
            required: RequiredCaps {
                headroom_tokens: 1_000,
                ..RequiredCaps::default()
            },
            est_tokens: 500,
            agent_id: AgentId::new(),
        };
        assert_eq!(req.total_required(), 1_500);
    }

    #[test]
    fn health_config_default_matches_documented_toml() {
        let h = HealthConfig::default();
        assert_eq!(h.transport_failures_to_open, 3);
        assert_eq!(h.open_duration_secs, 30);
        assert_eq!(h.half_open_successes_to_close, 1);
    }

    #[test]
    fn headroom_for_precedence_role_override_then_global_default() {
        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            RoleConfig {
                chain: vec![],
                required: RequiredCaps::default(),
                params: SamplingParams::default(),
                headroom_tokens: Some(32_768),
            },
        );
        roles.insert(
            "fast".to_string(),
            RoleConfig {
                chain: vec![],
                required: RequiredCaps::default(),
                params: SamplingParams::default(),
                headroom_tokens: None,
            },
        );
        let config = RoutingConfig {
            roles,
            health: HealthConfig::default(),
            default_headroom_tokens: 8_192,
        };

        let planner: RoleAlias = "planner".parse().unwrap();
        let fast: RoleAlias = "fast".parse().unwrap();
        let unknown: RoleAlias = "unknown".parse().unwrap();

        // Per-role override wins.
        assert_eq!(config.headroom_for(&planner), 32_768);
        // No override -> global default.
        assert_eq!(config.headroom_for(&fast), 8_192);
        // Unknown role -> global default.
        assert_eq!(config.headroom_for(&unknown), 8_192);

        let required = config.required_caps_for(&planner);
        assert_eq!(required.headroom_tokens, 32_768);
    }

    /// Deserializes a JSON document shaped like the architecture
    /// §"conway-routing / Internal Design Notes" TOML snippet (roles.planner
    /// chain, roles.fast chain, health block), extended per the amendment
    /// with `default_headroom_tokens` and a per-role `headroom_tokens`
    /// override on `planner`. Field values use this crate's actual wire
    /// shapes (`ModelRef` is a `{backend, model}` object, not a
    /// `"backend/model"` string; `HealthConfig` uses plain integer-second
    /// fields, not humantime strings — both documented deviations from the
    /// doc's illustrative TOML). Round-trips through `serde_json`.
    #[test]
    fn routing_config_deserializes_reference_shape_and_round_trips() {
        let json = r#"
        {
          "roles": {
            "planner": {
              "chain": [
                {"backend": "anthropic", "model": "claude-sonnet-4-6"},
                {"backend": "ollama-cloud", "model": "glm-5.2"},
                {"backend": "local", "model": "qwen3-coder-80b"}
              ],
              "required": {},
              "params": {"stop": [], "extra": {}},
              "headroom_tokens": 32768
            },
            "fast": {
              "chain": [
                {"backend": "local", "model": "qwen3-coder-80b"},
                {"backend": "anthropic", "model": "claude-haiku-4-5"}
              ],
              "required": {},
              "params": {"stop": [], "extra": {}}
            }
          },
          "health": {
            "transport_failures_to_open": 3,
            "open_duration_secs": 30,
            "half_open_successes_to_close": 1
          },
          "default_headroom_tokens": 8192
        }
        "#;

        let config: RoutingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.roles.len(), 2);
        assert_eq!(config.roles["planner"].chain.len(), 3);
        assert_eq!(config.roles["fast"].chain.len(), 2);
        assert_eq!(config.default_headroom_tokens, 8_192);
        assert_eq!(config.health, HealthConfig::default());

        let planner: RoleAlias = "planner".parse().unwrap();
        let fast: RoleAlias = "fast".parse().unwrap();
        assert_eq!(config.headroom_for(&planner), 32_768);
        assert_eq!(config.headroom_for(&fast), 8_192);

        // Round-trip.
        let reserialized = serde_json::to_string(&config).unwrap();
        let back: RoutingConfig = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&config).unwrap()
        );
    }

    #[test]
    fn model_overrides_round_trip() {
        let mo = ModelOverrides {
            stream_tools: Some(true),
            max_context_tokens: Some(131_072),
            reliability_tier: Some(ReliabilityTier::Verified),
            parallel_tool_calls: None,
            min_headroom_tokens: Some(16_384),
        };
        let json = serde_json::to_string(&mo).unwrap();
        let back: ModelOverrides = serde_json::from_str(&json).unwrap();
        assert_eq!(mo, back);
    }

    #[test]
    fn backend_config_round_trips_with_btreemap_models() {
        let mut models = BTreeMap::new();
        models.insert(
            "qwen3-coder-80b".to_string(),
            ModelOverrides {
                stream_tools: None,
                max_context_tokens: None,
                reliability_tier: None,
                parallel_tool_calls: None,
                min_headroom_tokens: None,
            },
        );
        let cfg = BackendConfig {
            id: BackendId::new("local"),
            kind: BackendKind::OpenAiCompat,
            base_url: Some("http://localhost:8080".into()),
            api_key_env: None,
            dialect: None,
            models,
            extra: serde_json::Map::new(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: BackendConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.models.len(), 1);
        assert_eq!(back.id, cfg.id);
    }

    // -------------------------------------------------------------------
    // `MinimalRouter`/`ExplainReport`
    // -------------------------------------------------------------------

    fn model_ref(backend: &str, model: &str) -> ModelRef {
        ModelRef {
            backend: BackendId::new(backend),
            model: ModelId::new(model),
        }
    }

    fn two_entry_chain_config() -> RoutingConfig {
        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            RoleConfig {
                chain: vec![
                    model_ref("anthropic", "claude-sonnet-4-6"),
                    model_ref("local", "qwen3-coder-80b"),
                ],
                required: RequiredCaps::default(),
                params: SamplingParams::default(),
                headroom_tokens: None,
            },
        );
        RoutingConfig {
            roles,
            health: HealthConfig::default(),
            default_headroom_tokens: 4_096,
        }
    }

    fn request(role: &str) -> RouteRequest {
        RouteRequest {
            role: RoleAlias::new(role),
            pin: None,
            required: RequiredCaps::default(),
            est_tokens: 0,
            agent_id: AgentId::new(),
        }
    }

    #[test]
    fn minimal_router_explain_over_two_entry_chain_is_honestly_degenerate() {
        let router = MinimalRouter::new(two_entry_chain_config());
        let report = router.explain(&request("planner"));

        assert_eq!(report.entries.len(), 2);
        assert!(matches!(
            report.entries[0].outcome,
            EntryOutcome::Selected { .. }
        ));
        assert!(matches!(
            report.entries[1].outcome,
            EntryOutcome::Skipped { .. }
        ));
        for entry in &report.entries {
            assert_eq!(entry.capabilities, None);
            assert_eq!(
                entry.breaker,
                BreakerSnapshot {
                    state: BreakerState::Closed
                }
            );
        }
    }

    #[test]
    fn minimal_router_resolve_returns_configured_chain_in_order() {
        let router = MinimalRouter::new(two_entry_chain_config());
        let routes = router
            .resolve(&request("planner"))
            .expect("configured role resolves");
        assert_eq!(routes.len(), 2);
        assert!(matches!(
            routes[0].reason,
            RoutingReason::AliasPrimary { .. }
        ));
        assert!(matches!(
            routes[1].reason,
            RoutingReason::Fallback { position: 1, .. }
        ));
    }

    #[test]
    fn minimal_router_resolve_unconfigured_role_is_unknown_role() {
        let router = MinimalRouter::new(two_entry_chain_config());
        let err = router
            .resolve(&request("no-such-role"))
            .expect_err("unconfigured role must not resolve");
        assert!(matches!(err, RoutingError::UnknownRole { .. }));
    }

    #[test]
    fn minimal_router_explain_unconfigured_role_is_empty_not_invented() {
        let router = MinimalRouter::new(two_entry_chain_config());
        let report = router.explain(&request("no-such-role"));
        assert!(report.entries.is_empty());
    }
}
