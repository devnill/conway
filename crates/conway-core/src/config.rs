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
    /// A ceiling on this def's own agent's step count -- consumed ONLY by a
    /// `conway_fork`/`conway_spawn` CHILD started under this def (board item
    /// `01M32EC0F9S5HTZDR3DADFV9DK`): `conway_runtime::subagent::
    /// SubagentHost::start` applies it as the tier between `PluginConfig`'s
    /// `subagent.max_steps` and the hardwired `DEFAULT_MAX_STEPS` (40)
    /// fallback, and ONLY when neither the call's own `budget.max_steps`
    /// argument nor that config key already set one -- see
    /// `conway_core::agent::SubagentSpec::max_steps_unset`'s own doc for the
    /// full four-tier precedence this implements.
    ///
    /// **Deliberately NEVER consulted for a ROOT (or resumed) session,
    /// even one started under this exact def** -- a decision recorded here,
    /// not an oversight. `Conway::default_budget` (the facade) builds a
    /// root's budget solely from `ConwayConfig`'s own `[limits]` section;
    /// `conway_runtime::runtime::root::start_root`/`resume_root` thread that
    /// budget straight through (`spec.knobs.budget.clone()`) with no
    /// `agent_def.max_steps` read anywhere on that path, exactly as before
    /// this item. The reasoning is the same one `Budget`'s own doc already
    /// gives for why an operator's `[limits].max_steps` of `0` (unlimited)
    /// is the sane root default while a subagent's hardwired `40` is not: an
    /// interactive root is watched by the human running it, who can read the
    /// transcript and interrupt, so a step ceiling picked by whichever def
    /// the root happens to be running under can only be wrong for that
    /// human -- exactly the class of "an arbitrary fixed ceiling can only be
    /// wrong" reasoning that already keeps a root's OWN config-derived
    /// ceiling opt-in. Widening this field to also govern a root would be a
    /// silent behavior change for anyone who has already written an
    /// `agent_def` with a `max_steps` on the assumption (true up to this
    /// item, and still true after it) that it can only ever affect a
    /// delegated child, never the session they are watching themselves.
    pub max_steps: Option<u32>,
    /// A wall-clock deadline, in seconds from a `conway_fork`/`conway_spawn`
    /// child's start, consumed the SAME way as [`Self::max_steps`] just
    /// above -- the tier between `PluginConfig`'s `subagent.deadline_secs`
    /// and the hardwired `DEFAULT_DEADLINE_SECS` (600s) fallback, applied
    /// ONLY when neither the call's own `budget.deadline_secs` argument nor
    /// that config key already set one. See [`Self::max_steps`]'s own doc
    /// for the full precedence and the "never widens a root" decision, both
    /// identical here.
    ///
    /// **Added on direct evidence, not speculatively**: board item
    /// `01M32EC0F9S5HTZDR3DADFV9DK`'s own investigation of the 2026-09-20
    /// proxy run (conway session `01M3042Y...`, three `ideate` reviewer
    /// subagents spawned ~2026-09-21T05:42 UTC) found every one of them
    /// ended `budget_exceeded`/`cancelled` with a `limit`/`reason` string
    /// reading `deadline=<timestamp>` -- NEVER `max_steps=...` -- and
    /// `steps_taken` of `0` and `5`, far short of the 40-step default. The
    /// DEADLINE tripped, not the step count; this field exists because that
    /// evidence was conclusive, not because `max_steps` needed a sibling on
    /// principle.
    pub deadline_secs: Option<u64>,
    /// Applied to a child that spawns/forks FROM this def -- i.e. a call
    /// site (a `conway_fork`/`conway_spawn` argument, or an embedder's
    /// `ForkSpec`/`SpawnSpec` builder field) that NAMES this def, and left
    /// its own `result_contract` unset (a call-site contract always wins;
    /// see `docs/agents.md`'s result-contract table).
    ///
    /// **Never applied when this def arrived by inheritance rather than by
    /// being named**: a forked child
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
/// and `BackendConfig` (defined in `routing.rs`) do not derive
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
            deadline_secs: Some(900),
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
                default_headroom_tokens: 8_192,
                ..Default::default()
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
