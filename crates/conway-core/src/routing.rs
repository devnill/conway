//! The content-free routing request/response contract (GP-07), the routing
//! reason vocabulary, health/breaker state, and the declarative routing
//! config types.
//!
//! `Router::resolve` (defined as a port trait in WI-007) never consults
//! request *content* — [`RouteRequest`] is constructed so that no field can
//! carry prompt text. This is a compile-time guarantee, not a convention: a
//! unit test below asserts the field set is exactly the five documented
//! fields.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::capabilities::{ReliabilityTier, RequiredCaps, DEFAULT_HEADROOM_TOKENS};
use crate::content::SamplingParams;
use crate::ids::{AgentId, BackendId, EndpointId, ModelId, ModelRef, RoleAlias};

fn default_headroom_tokens() -> u32 {
    DEFAULT_HEADROOM_TOKENS
}

/// A request to resolve a routing role to an ordered candidate list.
///
/// Deliberately has no field of type `String`, `Vec<ContentBlock>`,
/// `PromptSegment`, or `Message` that could carry prompt text (GP-07).
/// Reasoning-headroom rides on `required.headroom_tokens`, not as a separate
/// top-level field, so this five-field guarantee stays mechanically
/// checkable.
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

/// The two independent circuit breakers tracked per endpoint (Olla pattern).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerKind {
    Transport,
    Probe,
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
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observation {
    Ok { latency_ms: u32 },
    TransportError,
    ServerError,
    ProbeFail,
    RateLimited { retry_after_secs: Option<u64> },
}

/// The "why did this model run" answer for `conway routes explain <role>`:
/// the chosen route, every skipped candidate and why, breaker states, and
/// the effective headroom (a headroom-caused exclusion is otherwise
/// invisible and looks like an arbitrary skip).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplainReport {
    pub role: RoleAlias,
    pub chain: Vec<ModelRef>,
    pub chosen: Option<Route>,
    pub considered: Vec<(ModelRef, RoutingReason)>,
    pub breaker_states: Vec<(EndpointId, BreakerState)>,
    pub headroom_tokens: u32,
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

/// Circuit-breaker tuning, shared by every endpoint's `Transport` and
/// `Probe` breakers.
///
/// Every field has a serde default, so a config document omitting `[health]`
/// keys (or the whole table) deserializes to [`HealthConfig::default`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthConfig {
    pub transport_failures_to_open: u32,
    pub open_duration_secs: u64,
    pub probe_interval_secs: u64,
    pub probe_timeout_secs: u64,
    pub probe_failures_to_open: u32,
    /// Consecutive successful observations required to close a half-open
    /// breaker.
    pub half_open_successes_to_close: u32,
    /// Whether the periodic health prober runs at all.
    pub probe_enabled: bool,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            transport_failures_to_open: 3,
            open_duration_secs: 30,
            probe_interval_secs: 15,
            probe_timeout_secs: 2,
            probe_failures_to_open: 3,
            half_open_successes_to_close: 1,
            probe_enabled: true,
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
    /// (overrides > metadata > dialect defaults, per conway-backends'
    /// capability precedence).
    pub parallel_tool_calls: Option<bool>,
    /// A floor, not an override: `conway-routing` applies
    /// `effective = max(request.headroom_tokens, min_headroom_tokens.unwrap_or(0))`.
    /// A model that reasons heavily can insist on more reserved space than a
    /// role requests, but cannot reduce it.
    pub min_headroom_tokens: Option<u32>,
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
        assert_eq!(h.probe_interval_secs, 15);
        assert_eq!(h.probe_timeout_secs, 2);
        assert_eq!(h.probe_failures_to_open, 3);
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
            "probe_interval_secs": 15,
            "probe_timeout_secs": 2,
            "probe_failures_to_open": 3
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

    #[test]
    fn explain_report_carries_effective_headroom() {
        let report = ExplainReport {
            role: RoleAlias::new("planner"),
            chain: vec![],
            chosen: None,
            considered: vec![],
            breaker_states: vec![],
            headroom_tokens: 32_768,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["headroom_tokens"], 32_768);
    }
}
