//! Routing-crate-owned configuration: semantic validation of
//! `conway_core::routing::RoutingConfig`, plus the declarative headroom
//! reservation policy (`HeadroomPolicy`).
//!
//! Config *types* (`RoutingConfig`, `RoleConfig`, `HealthConfig`,
//! `ModelRef`) and their `serde` shapes are owned by `conway-core`; loading
//! (file discovery, env resolution, merging) is owned by the `conway`
//! facade. This module owns only the semantic checks a loaded
//! `RoutingConfig` must pass, and a sidecar policy for the reserved
//! output/reasoning token budget ("headroom").
//!
//! Divergence note (flagged, not worked around): the WI-031 amendment
//! anticipated `conway-core`'s `RoleConfig` lacking a `headroom_tokens`
//! field, with this module owning a document-parsing sidecar as the interim
//! path. The `conway-core` this crate builds against already carries
//! `RoleConfig::headroom_tokens` and `RoutingConfig::headroom_for` /
//! `RoutingConfig::default_headroom_tokens` -- the flagged interface gap is
//! already closed. `HeadroomPolicy` below is still implemented per the
//! amendment's criteria (its own default, its own `resolve`, its own
//! deserialization of the `[routing]` table shape) and gains a
//! `from_routing_config` constructor that delegates to the now-existing
//! core field, which is the path the amendment's notes anticipated ("write
//! both, the latter delegating when the field exists"). `validate` uses
//! `RoutingConfig::headroom_for` directly for the `HeadroomExceedsBudget`
//! check, since that is now the authoritative resolution path for a loaded
//! config.

use std::collections::BTreeMap;

use conway_core::ids::RoleAlias;
use conway_core::routing::RoutingConfig;
use serde::{Deserialize, Serialize};

/// Tokens reserved for model output/reasoning when a role has neither a
/// per-role override nor a configured `[routing] default_headroom_tokens`.
///
/// Aliased to `conway_core::capabilities::DEFAULT_HEADROOM_TOKENS` so the
/// config-time fallback and `RequiredCaps`'s per-request fallback can never
/// diverge: whichever construction path produces a headroom value
/// (`HeadroomPolicy::default`, bare deserialization, `from_routing_config`,
/// or core's `RoutingConfig` serde default), the omitted-key case resolves
/// to the same number. (The WI-031 amendment's literal `4_096` predates
/// core's own headroom amendment landing at `8_192`; a single value
/// supersedes both — incremental review S2, cycle 1.)
pub const DEFAULT_HEADROOM_TOKENS: u32 = conway_core::capabilities::DEFAULT_HEADROOM_TOKENS;

/// A resolved headroom at or above this value is rejected by [`validate`]
/// as an implausible configuration (a stray digit, a unit mix-up, etc.).
const MAX_PLAUSIBLE_HEADROOM_TOKENS: u32 = 1_000_000;

/// A declarative, config-time-resolved reservation of output/reasoning
/// tokens: a global default with per-role overrides.
///
/// Resolution ([`HeadroomPolicy::resolve`]) happens once, is total, and
/// never depends on anything but the policy and a role name -- no request
/// data, no runtime measurement. `per_role` is not populated by `serde`
/// (`#[serde(skip)]`): it is built explicitly by
/// [`HeadroomPolicy::from_routing_config`] from `conway-core`'s
/// `RoleConfig::headroom_tokens`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HeadroomPolicy {
    /// Mirrors the `[routing] default_headroom_tokens` config key.
    pub default_headroom_tokens: u32,
    #[serde(skip)]
    pub per_role: BTreeMap<RoleAlias, u32>,
}

impl Default for HeadroomPolicy {
    fn default() -> Self {
        Self {
            default_headroom_tokens: DEFAULT_HEADROOM_TOKENS,
            per_role: BTreeMap::new(),
        }
    }
}

impl HeadroomPolicy {
    /// The per-role override when present, else `default_headroom_tokens`.
    /// Total: never errors, never panics, including for a `role` absent
    /// from `per_role` (the global default is returned).
    pub fn resolve(&self, role: &RoleAlias) -> u32 {
        self.per_role
            .get(role)
            .copied()
            .unwrap_or(self.default_headroom_tokens)
    }

    /// Builds a `HeadroomPolicy` from a `conway-core` `RoutingConfig`: the
    /// global default from `RoutingConfig::default_headroom_tokens`, and
    /// the per-role table from every `RoleConfig::headroom_tokens` that is
    /// `Some(_)`. This is the delegating path the WI-031 amendment
    /// anticipated once `RoleConfig` carried the field -- it already does
    /// in the `conway-core` this crate builds against.
    pub fn from_routing_config(config: &RoutingConfig) -> Self {
        let per_role = config
            .roles
            .iter()
            .filter_map(|(name, role)| {
                role.headroom_tokens
                    .map(|tokens| (RoleAlias::new(name.clone()), tokens))
            })
            .collect();
        Self {
            default_headroom_tokens: config.default_headroom_tokens,
            per_role,
        }
    }
}

/// One semantic problem found by [`validate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigIssue {
    pub role: RoleAlias,
    pub position: Option<usize>,
    pub kind: ConfigIssueKind,
    pub message: String,
}

/// The category of semantic problem a [`ConfigIssue`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigIssueKind {
    /// A role's `chain` has no entries.
    EmptyChain,
    /// A chain entry has an empty backend or model segment.
    MalformedModelRef,
    /// The same `(backend, model)` pair appears more than once in a chain.
    DuplicateEntry,
    /// A role's resolved headroom is implausibly large.
    HeadroomExceedsBudget,
}

/// Semantic validation of a loaded `RoutingConfig`. Returns every problem
/// found (not just the first), in role-then-position order.
///
/// Checks: a role whose `chain` is empty; a chain entry with an empty
/// backend or model segment; a duplicate `(backend, model)` pair within one
/// chain; a role whose resolved headroom (`RoutingConfig::headroom_for`) is
/// `>= 1_000_000`.
pub fn validate(config: &RoutingConfig) -> Result<(), Vec<ConfigIssue>> {
    let mut issues = Vec::new();

    for (name, role_cfg) in &config.roles {
        let role = RoleAlias::new(name.clone());

        if role_cfg.chain.is_empty() {
            issues.push(ConfigIssue {
                role: role.clone(),
                position: None,
                kind: ConfigIssueKind::EmptyChain,
                message: format!("role '{name}': chain is empty"),
            });
        }

        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for (position, entry) in role_cfg.chain.iter().enumerate() {
            if entry.backend.as_str().is_empty() || entry.model.as_str().is_empty() {
                issues.push(ConfigIssue {
                    role: role.clone(),
                    position: Some(position),
                    kind: ConfigIssueKind::MalformedModelRef,
                    message: format!(
                        "role '{name}' position {position}: '{entry}' is not a valid backend/model reference"
                    ),
                });
                continue;
            }

            let key = entry.to_string();
            if let Some(&first) = seen.get(&key) {
                issues.push(ConfigIssue {
                    role: role.clone(),
                    position: Some(position),
                    kind: ConfigIssueKind::DuplicateEntry,
                    message: format!(
                        "role '{name}' position {position}: duplicate entry '{entry}' (first at position {first})"
                    ),
                });
            } else {
                seen.insert(key, position);
            }
        }

        let headroom = config.headroom_for(&role);
        if headroom >= MAX_PLAUSIBLE_HEADROOM_TOKENS {
            issues.push(ConfigIssue {
                role: role.clone(),
                position: None,
                kind: ConfigIssueKind::HeadroomExceedsBudget,
                message: format!(
                    "role '{name}': headroom_tokens {headroom} is implausibly large (maximum 999999)"
                ),
            });
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::ids::{BackendId, ModelId, ModelRef};
    use conway_core::routing::{HealthConfig, RoleConfig};

    fn model_ref(backend: &str, model: &str) -> ModelRef {
        ModelRef {
            backend: BackendId::new(backend),
            model: ModelId::new(model),
        }
    }

    fn role(chain: Vec<ModelRef>, headroom_tokens: Option<u32>) -> RoleConfig {
        RoleConfig {
            chain,
            headroom_tokens,
            ..Default::default()
        }
    }

    fn config(roles: BTreeMap<String, RoleConfig>, default_headroom_tokens: u32) -> RoutingConfig {
        RoutingConfig {
            roles,
            health: HealthConfig::default(),
            default_headroom_tokens,
        }
    }

    // -----------------------------------------------------------------
    // HeadroomPolicy
    // -----------------------------------------------------------------

    #[test]
    fn headroom_policy_default_matches_core_constant_and_empty() {
        let policy = HeadroomPolicy::default();
        assert_eq!(policy.default_headroom_tokens, DEFAULT_HEADROOM_TOKENS);
        assert_eq!(
            DEFAULT_HEADROOM_TOKENS,
            conway_core::capabilities::DEFAULT_HEADROOM_TOKENS
        );
        assert!(policy.per_role.is_empty());
    }

    #[test]
    fn headroom_policy_deserializes_the_routing_table_shape() {
        // The `[routing]` table from the amendment's fragment:
        //   [routing]
        //   default_headroom_tokens = 4096
        let policy: HeadroomPolicy =
            toml::from_str("default_headroom_tokens = 4096").expect("valid HeadroomPolicy toml");
        assert_eq!(policy.default_headroom_tokens, 4_096);
        assert!(policy.per_role.is_empty());
    }

    #[test]
    fn headroom_policy_omitting_the_field_deserializes_to_default() {
        let policy: HeadroomPolicy = toml::from_str("").expect("empty document uses default");
        assert_eq!(policy, HeadroomPolicy::default());
    }

    #[test]
    fn headroom_resolve_per_role_hit() {
        let mut per_role = BTreeMap::new();
        per_role.insert(RoleAlias::new("planner"), 16_384);
        let policy = HeadroomPolicy {
            default_headroom_tokens: 4_096,
            per_role,
        };
        assert_eq!(policy.resolve(&RoleAlias::new("planner")), 16_384);
    }

    #[test]
    fn headroom_resolve_per_role_miss_falls_back_to_global_default() {
        let mut per_role = BTreeMap::new();
        per_role.insert(RoleAlias::new("planner"), 16_384);
        let policy = HeadroomPolicy {
            default_headroom_tokens: 4_096,
            per_role,
        };
        // "fast" has no override.
        assert_eq!(policy.resolve(&RoleAlias::new("fast")), 4_096); // explicit key below sets 4096
    }

    #[test]
    fn headroom_resolve_empty_policy_returns_default() {
        let policy = HeadroomPolicy::default();
        assert_eq!(
            policy.resolve(&RoleAlias::new("anything")),
            DEFAULT_HEADROOM_TOKENS
        );
        // Total: even a role absent from every table never panics/errors.
        assert_eq!(
            policy.resolve(&RoleAlias::new("unknown-role")),
            DEFAULT_HEADROOM_TOKENS
        );
    }

    #[test]
    fn headroom_policy_from_routing_config_reads_per_role_override_and_global_default() {
        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            role(
                vec![model_ref("anthropic", "claude-sonnet-4-6")],
                Some(16_384),
            ),
        );
        roles.insert(
            "fast".to_string(),
            role(vec![model_ref("local", "qwen3-coder-80b")], None),
        );
        let routing_config = config(roles, 4_096);

        let policy = HeadroomPolicy::from_routing_config(&routing_config);
        assert_eq!(policy.default_headroom_tokens, 4_096);
        assert_eq!(policy.resolve(&RoleAlias::new("planner")), 16_384);
        assert_eq!(policy.resolve(&RoleAlias::new("fast")), 4_096); // explicit key below sets 4096
    }

    // -----------------------------------------------------------------
    // config::validate
    // -----------------------------------------------------------------

    #[test]
    fn validate_passes_for_a_well_formed_config() {
        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            role(
                vec![
                    model_ref("anthropic", "claude-sonnet-4-6"),
                    model_ref("ollama-cloud", "glm-5.2"),
                ],
                None,
            ),
        );
        let routing_config = config(roles, 4_096);
        assert!(validate(&routing_config).is_ok());
    }

    #[test]
    fn validate_rejects_empty_chain() {
        let mut roles = BTreeMap::new();
        roles.insert("planner".to_string(), role(vec![], None));
        let routing_config = config(roles, 4_096);

        let issues = validate(&routing_config).unwrap_err();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ConfigIssueKind::EmptyChain);
        assert_eq!(issues[0].message, "role 'planner': chain is empty");
        assert_eq!(issues[0].position, None);
    }

    #[test]
    fn validate_rejects_malformed_model_ref() {
        let mut roles = BTreeMap::new();
        // Empty backend segment: unreachable via `ModelRef::from_str` (it
        // rejects an empty backend/model), but reachable via a
        // structurally-deserialized `{backend: "", model: "glm-5.2"}` table
        // entry, which is `conway-core`'s actual chain-entry wire shape.
        roles.insert(
            "planner".to_string(),
            role(
                vec![
                    model_ref("anthropic", "claude-sonnet-4-6"),
                    model_ref("", "glm-5.2"),
                ],
                None,
            ),
        );
        let routing_config = config(roles, 4_096);

        let issues = validate(&routing_config).unwrap_err();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ConfigIssueKind::MalformedModelRef);
        assert_eq!(issues[0].position, Some(1));
        assert_eq!(
            issues[0].message,
            "role 'planner' position 1: '/glm-5.2' is not a valid backend/model reference"
        );
    }

    #[test]
    fn validate_rejects_duplicate_entry() {
        let mut roles = BTreeMap::new();
        roles.insert(
            "fast".to_string(),
            role(
                vec![
                    model_ref("local", "qwen3-coder-80b"),
                    model_ref("anthropic", "claude-haiku-4-5"),
                    model_ref("local", "qwen3-coder-80b"),
                ],
                None,
            ),
        );
        let routing_config = config(roles, 4_096);

        let issues = validate(&routing_config).unwrap_err();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ConfigIssueKind::DuplicateEntry);
        assert_eq!(issues[0].position, Some(2));
        assert_eq!(
            issues[0].message,
            "role 'fast' position 2: duplicate entry 'local/qwen3-coder-80b' (first at position 0)"
        );
    }

    #[test]
    fn validate_rejects_headroom_exceeding_budget() {
        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            role(
                vec![model_ref("anthropic", "claude-sonnet-4-6")],
                Some(1_000_000),
            ),
        );
        let routing_config = config(roles, 4_096);

        let issues = validate(&routing_config).unwrap_err();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ConfigIssueKind::HeadroomExceedsBudget);
        assert_eq!(
            issues[0].message,
            "role 'planner': headroom_tokens 1000000 is implausibly large (maximum 999999)"
        );
    }

    #[test]
    fn validate_accepts_headroom_just_under_the_budget() {
        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            role(
                vec![model_ref("anthropic", "claude-sonnet-4-6")],
                Some(999_999),
            ),
        );
        let routing_config = config(roles, 4_096);
        assert!(validate(&routing_config).is_ok());
    }

    #[test]
    fn validate_collects_every_issue_not_just_the_first() {
        let mut roles = BTreeMap::new();
        roles.insert("empty".to_string(), role(vec![], None));
        roles.insert(
            "dupes".to_string(),
            role(
                vec![
                    model_ref("local", "qwen3-coder-80b"),
                    model_ref("local", "qwen3-coder-80b"),
                ],
                None,
            ),
        );
        let routing_config = config(roles, 4_096);

        let issues = validate(&routing_config).unwrap_err();
        assert_eq!(issues.len(), 2);
    }

    // -----------------------------------------------------------------
    // HealthConfig (owned by conway-core; this module only tests against
    // conway-core's actual type -- see the divergence note below).
    //
    // Divergence note (flagged, not worked around): the WI-031 doc's
    // illustrative `HealthConfig` has fields
    // `{transport_failures_to_open, probe_failures_to_open, open_duration,
    // half_open_successes_to_close, probe_interval, probe_timeout,
    // probe_enabled}` with humantime-string durations, `#[serde(default)]`
    // on the whole struct, and `probe_failures_to_open: 2` in its default.
    // The `conway-core` this crate builds against instead defines
    // `HealthConfig` with fields `{transport_failures_to_open,
    // open_duration_secs, probe_interval_secs, probe_timeout_secs,
    // probe_failures_to_open}` (plain integer seconds, no
    // `half_open_successes_to_close` field, no `probe_enabled` field, no
    // `#[serde(default)]`), and its documented default has
    // `probe_failures_to_open: 3`. Per the coordinator's instruction, this
    // crate tests conway-core's actual type/default rather than modifying
    // conway-core; the two divergences (missing fields, and 3 vs. 2 for
    // `probe_failures_to_open`) are flagged in the work-item completion
    // report.
    // -----------------------------------------------------------------

    #[test]
    fn health_config_full_fragment_deserializes_and_matches_core_default() {
        let json = r#"{
            "transport_failures_to_open": 3,
            "open_duration_secs": 30,
            "probe_interval_secs": 15,
            "probe_timeout_secs": 2,
            "probe_failures_to_open": 3
        }"#;
        let parsed: HealthConfig = serde_json::from_str(json).expect("valid HealthConfig json");
        assert_eq!(parsed, HealthConfig::default());
    }

    #[test]
    fn health_config_default_matches_documented_core_values() {
        let h = HealthConfig::default();
        assert_eq!(h.transport_failures_to_open, 3);
        assert_eq!(h.open_duration_secs, 30);
        assert_eq!(h.probe_interval_secs, 15);
        assert_eq!(h.probe_timeout_secs, 2);
        assert_eq!(h.probe_failures_to_open, 3);
    }
}
