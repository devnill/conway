//! Serde-only agent/skill definitions and the top-level `ConwayConfig`.
//!
//! Types only: no file discovery, no path resolution, no environment reads
//! anywhere in this crate. Loading (TOML parsing, `AgentDef`/`SkillDef`
//! discovery on disk, environment variable resolution) lives in the `conway`
//! facade.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::{Budget, ToolSelector};
use crate::capabilities::CacheTtl;
use crate::ids::{ModelRef, RoleAlias};
use crate::routing::{BackendConfig, RoutingConfig};

/// The default for [`ConwayConfig::max_parallel_tools`] when a facade
/// constructs a config with no explicit override.
pub const DEFAULT_MAX_PARALLEL_TOOLS: usize = 4;

/// A named agent definition: system prompt, model/role preference, tool
/// selection, and included skills.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentDef {
    pub name: String,
    pub description: Option<String>,
    /// The markdown body used as the agent's system prompt.
    pub system_prompt: String,
    pub role: Option<RoleAlias>,
    pub model: Option<ModelRef>,
    pub tools: ToolSelector,
    pub skills: Vec<String>,
    pub max_steps: Option<u32>,
    /// Applied to a child that spawns/forks FROM this def -- i.e. a call
    /// site (a `conway_fork`/`conway_spawn` argument, or an embedder's
    /// `ForkSpec`/`SpawnSpec` builder field) that NAMES this def, and left
    /// its own `result_contract` unset (a call-site contract always wins;
    /// see `docs/agents.md`'s result-contract table).
    ///
    /// **Never applied when this def arrived by inheritance rather than by
    /// being named** (decision 01KZHEWXDZWPWMEAQ01XY2RDCB): a forked child
    /// whose `agent_def` is filled in from its parent's own
    /// (`conway_runtime`'s `SubagentHost::start`, Fork-only fallback, gated
    /// on the call site leaving `agent_def` unset) still gets this def's
    /// system prompt, tools selector, and model pin, but NOT this field --
    /// a result contract is always declared AT a call site, never merely
    /// carried along because the def that happens to define it was. This is
    /// otherwise knowable only from that method's own implementation, which
    /// is why it is spelled out here too.
    pub result_contract: Option<schemars::schema::RootSchema>,
}

/// A named, reusable prompt fragment injected into context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillDef {
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    pub always_include: bool,
}

/// The complete, assembled configuration for one `conway` instance.
///
/// This is a *types-only* struct: nothing in this crate discovers,
/// resolves, or reads this value from disk or the environment. `RoutingConfig`
/// and `BackendConfig` (defined in `routing.rs`, WI-004) do not derive
/// `PartialEq`, so this struct does not either; tests compare instances via
/// `serde_json::Value` equality instead.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConwayConfig {
    pub backends: Vec<BackendConfig>,
    pub routing: RoutingConfig,
    pub default_role: RoleAlias,
    /// See [`DEFAULT_MAX_PARALLEL_TOOLS`] for the documented default (`4`);
    /// resolving that default onto a concrete value is the facade's job,
    /// since this crate does no config loading.
    pub max_parallel_tools: usize,
    pub fsync: FsyncPolicy,
    pub session_root: PathBuf,
    pub default_budget: Budget,
    pub cache_ttl: CacheTtl,
}

/// How aggressively the session store flushes to disk.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FsyncPolicy {
    Always,
    Interval { millis: u64 },
    Never,
}

impl Default for FsyncPolicy {
    fn default() -> Self {
        FsyncPolicy::Interval { millis: 200 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolSelector;
    use crate::ids::{BackendId, ModelId};
    use crate::routing::HealthConfig;
    use std::collections::BTreeMap;

    #[test]
    fn fsync_policy_default_is_interval_200ms() {
        assert_eq!(
            FsyncPolicy::default(),
            FsyncPolicy::Interval { millis: 200 }
        );
    }

    #[test]
    fn default_max_parallel_tools_constant_is_four() {
        assert_eq!(DEFAULT_MAX_PARALLEL_TOOLS, 4);
    }

    #[test]
    fn agent_def_round_trips() {
        let def = AgentDef {
            name: "reviewer".into(),
            description: Some("Reviews diffs".into()),
            system_prompt: "You are a careful reviewer.".into(),
            role: Some(RoleAlias::new("coder")),
            model: Some(ModelRef {
                backend: BackendId::new("anthropic"),
                model: ModelId::new("claude-sonnet-4-6"),
            }),
            tools: ToolSelector::Only(vec!["read".into(), "search".into()]),
            skills: vec!["review".into()],
            max_steps: Some(20),
            result_contract: None,
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: AgentDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn skill_def_round_trips() {
        let skill = SkillDef {
            name: "review".into(),
            description: None,
            body: "# Review checklist".into(),
            always_include: true,
        };
        let json = serde_json::to_string(&skill).unwrap();
        let back: SkillDef = serde_json::from_str(&json).unwrap();
        assert_eq!(skill, back);
    }

    #[test]
    fn conway_config_round_trips_via_json_value() {
        let cfg = ConwayConfig {
            backends: vec![],
            routing: RoutingConfig {
                roles: BTreeMap::new(),
                health: HealthConfig::default(),
                default_headroom_tokens: 8_192,
            },
            default_role: RoleAlias::new("planner"),
            max_parallel_tools: DEFAULT_MAX_PARALLEL_TOOLS,
            fsync: FsyncPolicy::default(),
            session_root: PathBuf::from("/tmp/sessions"),
            default_budget: Budget::default(),
            cache_ttl: CacheTtl::FiveMinutes,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ConwayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&cfg).unwrap()
        );
        assert_eq!(back.max_parallel_tools, 4);
    }
}
