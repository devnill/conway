//! Routing-crate-owned configuration: semantic validation of
//! `conway_core::routing::RoutingConfig`.
//!
//! Config *types* (`RoutingConfig`, `RoleConfig`, `HealthConfig`,
//! `ModelRef`) and their `serde` shapes are owned by `conway-core`; loading
//! (file discovery, env resolution, merging) is owned by the `conway`
//! facade. This module owns only the semantic checks a loaded
//! `RoutingConfig` must pass.
//!
//! **`HeadroomPolicy` moved to `conway_core::capabilities` **, beside `DEFAULT_HEADROOM_TOKENS`: checking
//! every read of `HeadroomPolicy::resolve` and every construction site found
//! `DeclarativeRouter::new` (`router.rs`) still takes it as a caller-supplied
//! sidecar and cross-checks its resolution against
//! `RoutingConfig::headroom_for` per role, so `RoutingConfig::headroom_for`
//! is not a total replacement -- the type stayed, it just no longer lives in
//! this crate. `crate::config::validate` below is unaffected: its
//! `HeadroomExceedsBudget` check already used `RoutingConfig::headroom_for`
//! directly, never `HeadroomPolicy`.
//!
//! Divergence note (flagged, not worked around): the amendment
//! anticipated `conway-core`'s `RoleConfig` lacking a `headroom_tokens`
//! field, with this module owning a document-parsing sidecar as the interim
//! path. The `conway-core` this crate builds against already carries
//! `RoleConfig::headroom_tokens` and `RoutingConfig::headroom_for` /
//! `RoutingConfig::default_headroom_tokens` -- the flagged interface gap is
//! already closed.

use std::collections::BTreeMap;

use conway_core::ids::RoleAlias;
use conway_core::routing::RoutingConfig;
use serde::{Deserialize, Serialize};

/// A resolved headroom at or above this value is rejected by [`validate`]
/// as an implausible configuration (a stray digit, a unit mix-up, etc.).
const MAX_PLAUSIBLE_HEADROOM_TOKENS: u32 = 1_000_000;

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
    /// A role's headroom resolves to different values from the
    /// separately-passed `HeadroomPolicy` sidecar and from
    /// `RoutingConfig::headroom_for` -- the two sources of truth disagree, so
    /// `validate`'s `HeadroomExceedsBudget` check (which only sees the
    /// config-derived value) cannot be trusted to cover what the router
    /// actually resolves at request time.
    HeadroomSourcesDisagree,
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
    // conway-core's actual type).
    //
    // `probe_interval_secs`/`probe_timeout_secs`/`probe_failures_to_open`/
    // `probe_enabled` (which used to configure a periodic health prober and
    // the independent `Probe` breaker it fed) were removed from
    // `conway_core::routing::HealthConfig` (//, "retire the health prober") -- the
    // prober had no production call site anywhere in this tree, and the
    // Transport breaker alone already handles recovery. The two fixtures
    // below were updated in step; the earlier divergence note this comment
    // block used to carry (an earlier item's illustrative `HealthConfig` shape versus
    // conway-core's actual one) is moot now that both the doc and the type
    // agree on three fields.
    // -----------------------------------------------------------------

    #[test]
    fn health_config_full_fragment_deserializes_and_matches_core_default() {
        let json = r#"{
            "transport_failures_to_open": 3,
            "open_duration_secs": 30,
            "half_open_successes_to_close": 1
        }"#;
        let parsed: HealthConfig = serde_json::from_str(json).expect("valid HealthConfig json");
        assert_eq!(parsed, HealthConfig::default());
    }

    #[test]
    fn health_config_default_matches_documented_core_values() {
        let h = HealthConfig::default();
        assert_eq!(h.transport_failures_to_open, 3);
        assert_eq!(h.open_duration_secs, 30);
        assert_eq!(h.half_open_successes_to_close, 1);
    }
}
