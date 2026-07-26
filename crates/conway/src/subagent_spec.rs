//! `ForkSpec`/`SpawnSpec`: the library-consumer-facing request shapes for
//! [`crate::SessionHandle::fork`]/[`crate::SessionHandle::spawn`] (WI-102).
//!
//! GP-02 (the project's centerpiece): **fork** inherits the forker's entire
//! context plus an additional directive; **spawn** is clean-slate. The two
//! specs below are kept as distinct types -- not one type with a mode flag
//! -- so that distinction is visible at the call site, even though
//! `agent_def` is now optional on both (see [`SpawnSpec`]'s doc for the
//! no-`agent_def`-means-inherit-the-parent's-role/model semantics this
//! relaxes from the 0.1.0 "agent_def mandatory for spawn" rule, WI-099).
//!
//! Both convert into [`conway_core::agent::SubagentSpec`] via `From`, the
//! type `conway_core::ports::SubagentHost::start` (`impl` on
//! `conway_runtime::Runtime`, WI-084) actually consumes. This module
//! contains no fork/spawn *logic* -- only the request shape and that
//! conversion; see [`crate::SessionHandle::fork`]/`::spawn` for the
//! delegation itself.

use conway_core::agent::{AgentDefRef, Budget, SubagentMode, SubagentSpec, ToolSelector};
use conway_core::ids::RoleAlias;

/// A request to fork a live agent: the child inherits the forker's entire
/// context (by reference, as of the fork point) plus `directive`.
///
/// **Reconciliation (disclosed):** the WI-102 binding notes' illustrative
/// struct types `result_contract` as `Option<serde_json::Value>`. The
/// committed `conway_core::agent::SubagentSpec::result_contract` (this
/// item's own conversion target) is `Option<schemars::schema::RootSchema>`,
/// not `serde_json::Value` -- confirmed by reading `conway-core/src/agent.rs`
/// directly, and already the type `conway_core::config::AgentDef::
/// result_contract` uses too (so a consumer of this crate's public API
/// already depends on `schemars` for that field on `AgentDef`, re-exported
/// from this crate's root). This type follows the committed shape instead:
/// `result_contract: Option<schemars::schema::RootSchema>`. This keeps
/// `From<ForkSpec> for SubagentSpec` a total, infallible field-for-field
/// move (the binding criterion asks for a plain `From`, not a `TryFrom`);
/// deserializing an inline JSON Schema value into a `RootSchema` -- and
/// validating it compiles, as `crate::agents::compile_result_contract`
/// already does for `AgentDef` -- is the caller's concern before
/// constructing a `ForkSpec`, the same division of labor `AgentDef` already
/// establishes for the analogous field.
#[derive(Clone, Debug, PartialEq)]
pub struct ForkSpec {
    /// Becomes the `LogRecord::ForkDirective` text -- the forker's own
    /// additional instruction to the child, layered on top of everything
    /// the child inherits.
    pub directive: String,
    /// Overrides the forker's own system prompt. `None` means the child
    /// keeps whatever the forker was running under.
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    /// Intersected with the forker's own tool set by the runtime -- this
    /// spec cannot *grant* a tool the forker itself lacks.
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    /// Never correctness-bearing (GP-06) -- a caching hint only. Defaults to
    /// `true` via [`ForkSpec::new`], matching
    /// `conway_core::agent::SubagentSpec::fork`'s own default.
    pub cache_hint: bool,
    pub result_contract: Option<schemars::schema::RootSchema>,
    /// Opt-in interactive keep-alive (WI "bare /spawn & /fork open an
    /// interactive session"): the child idles for the caller's next
    /// [`crate::SessionHandle::prompt_agent`] after each turn instead of
    /// finishing on natural completion. Defaults `false` via
    /// [`ForkSpec::new`], matching `conway_core::agent::SubagentSpec::fork`'s
    /// own default and preserving the existing autonomous, one-shot fork
    /// behavior unchanged. Set via [`ForkSpec::keep_alive`].
    pub keep_alive: bool,
}

impl ForkSpec {
    /// **Disclosed deviation:** the WI-102 binding notes describe `budget`
    /// as defaulting to "the session's configured budget." That is
    /// infeasible as written -- `ForkSpec` (and `SpawnSpec`) are plain,
    /// freestanding request structs with no session reference at
    /// construction time, so `new` has nothing to read a session's
    /// configured limits from. `budget` instead defaults to
    /// `Budget::default()` (`conway-core`'s own baseline: 40 steps, no
    /// deadline/token/tool-call ceiling). Override via [`ForkSpec::budget`]
    /// when a caller wants the enclosing session's actual configured
    /// budget.
    pub fn new(directive: impl Into<String>) -> Self {
        Self {
            directive: directive.into(),
            agent_def: None,
            role: None,
            tools: None,
            budget: Budget::default(),
            cache_hint: true,
            result_contract: None,
            keep_alive: false,
        }
    }

    pub fn agent_def(mut self, agent_def: impl Into<String>) -> Self {
        self.agent_def = Some(agent_def.into());
        self
    }

    pub fn role(mut self, role: RoleAlias) -> Self {
        self.role = Some(role);
        self
    }

    pub fn tools(mut self, tools: ToolSelector) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    pub fn cache_hint(mut self, cache_hint: bool) -> Self {
        self.cache_hint = cache_hint;
        self
    }

    pub fn result_contract(mut self, result_contract: schemars::schema::RootSchema) -> Self {
        self.result_contract = Some(result_contract);
        self
    }

    /// See [`ForkSpec::keep_alive`]'s own field doc.
    pub fn keep_alive(mut self, keep_alive: bool) -> Self {
        self.keep_alive = keep_alive;
        self
    }
}

impl From<ForkSpec> for SubagentSpec {
    fn from(spec: ForkSpec) -> Self {
        SubagentSpec {
            mode: SubagentMode::Fork,
            prompt: spec.directive,
            agent_def: spec.agent_def.map(AgentDefRef),
            role: spec.role,
            tools: spec.tools,
            budget: spec.budget,
            cache_hint: spec.cache_hint,
            result_contract: spec.result_contract,
            // `SubagentSpec::await_result` postdates this item's own binding
            // notes (not named in either spec struct) -- it is what lets an
            // agent-initiated fork/spawn fan out without blocking
            // (`conway-tools`' `conway_subagent` tool). A library consumer
            // going through `SessionHandle::fork`/`::spawn` always gets a
            // handle back and decides for itself whether to call
            // `await_agent`, so `true` here matches
            // `SubagentSpec::fork`/`::spawn`'s own constructor default and
            // is not a place this facade needs to expose a toggle.
            await_result: true,
            keep_alive: spec.keep_alive,
            ephemeral: false,
        }
    }
}

/// A request to spawn a fresh agent: no inherited context, a clean-slate
/// system prompt from `agent_def` when one is given.
///
/// **Relaxed (WI-099 superseded):** the 0.1.0 design deliberately made
/// `agent_def` a required `String` here, enforcing "every spawn names a
/// model" at the type level. That rule is relaxed by a recorded design
/// decision: `agent_def` is now `Option<String>`, matching the internal
/// `conway_core::agent::SubagentSpec::agent_def` it converts into. Spawning
/// with `agent_def: None` does NOT invent a placeholder def -- it means the
/// child gets no agent-def system prompt and no model pin, and
/// `conway_runtime`'s `SubagentHost::start` resolves its role/model the same
/// way a roleless fork does: inherit the PARENT session's role (and, via
/// that role's own routing, effectively the parent's model), falling back to
/// the configured default role only if the parent itself has none. This
/// makes `SpawnSpec::new(prompt)` (no `agent_def` argument) a legitimate,
/// clean-slate-context-but-inherited-routing spawn -- distinct from `fork`,
/// which additionally inherits the forker's entire transcript.
///
/// Use [`SpawnSpec::new`] plus [`SpawnSpec::agent_def`] to name a def
/// explicitly, or leave it unset to inherit.
///
/// `SpawnSpec` still has no `cache_hint` field (`cache_hint` is meaningless
/// for spawn; `From<SpawnSpec> for SubagentSpec` forces it `false`, matching
/// `SubagentSpec::spawn`'s own constructor).
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnSpec {
    pub prompt: String,
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    pub result_contract: Option<schemars::schema::RootSchema>,
    /// See [`ForkSpec::keep_alive`]'s own field doc -- identical semantics,
    /// mapped through `From<SpawnSpec> for SubagentSpec` the same way.
    /// Defaults `false` via [`SpawnSpec::new`].
    pub keep_alive: bool,
}

impl SpawnSpec {
    /// **Disclosed deviation:** same as [`ForkSpec::new`] -- `budget`
    /// defaults to `Budget::default()` (`conway-core`'s baseline), not "the
    /// session's configured budget" the binding notes describe, since this
    /// type has no session reference at construction time either. See
    /// [`ForkSpec::new`]'s doc for the full reasoning.
    ///
    /// No `agent_def` argument: leaves it unset (`None`), meaning the
    /// spawned child inherits the parent session's role/model -- see this
    /// struct's own doc. Call [`SpawnSpec::agent_def`] to name one instead.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            agent_def: None,
            role: None,
            tools: None,
            budget: Budget::default(),
            result_contract: None,
            keep_alive: false,
        }
    }

    pub fn agent_def(mut self, agent_def: impl Into<String>) -> Self {
        self.agent_def = Some(agent_def.into());
        self
    }

    pub fn role(mut self, role: RoleAlias) -> Self {
        self.role = Some(role);
        self
    }

    pub fn tools(mut self, tools: ToolSelector) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    pub fn result_contract(mut self, result_contract: schemars::schema::RootSchema) -> Self {
        self.result_contract = Some(result_contract);
        self
    }

    /// See [`ForkSpec::keep_alive`]'s own field doc.
    pub fn keep_alive(mut self, keep_alive: bool) -> Self {
        self.keep_alive = keep_alive;
        self
    }
}

impl From<SpawnSpec> for SubagentSpec {
    fn from(spec: SpawnSpec) -> Self {
        SubagentSpec {
            mode: SubagentMode::Spawn,
            prompt: spec.prompt,
            agent_def: spec.agent_def.map(AgentDefRef),
            role: spec.role,
            tools: spec.tools,
            budget: spec.budget,
            cache_hint: false,
            result_contract: spec.result_contract,
            // See the matching note on `From<ForkSpec>` above.
            await_result: true,
            keep_alive: spec.keep_alive,
            ephemeral: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_spec_converts_with_mode_fork_and_every_field_mapped() {
        let budget = Budget {
            max_steps: 7,
            deadline: None,
            max_tokens: Some(100),
            max_tool_calls: Some(3),
        };
        let spec = ForkSpec::new("do the thing")
            .agent_def("reviewer")
            .role(RoleAlias::new("planner"))
            .tools(ToolSelector::Only(vec!["read".into()]))
            .budget(budget.clone())
            .cache_hint(false);

        let converted: SubagentSpec = spec.into();
        assert_eq!(converted.mode, SubagentMode::Fork);
        assert_eq!(converted.prompt, "do the thing");
        assert_eq!(converted.agent_def, Some(AgentDefRef("reviewer".into())));
        assert_eq!(converted.role, Some(RoleAlias::new("planner")));
        assert_eq!(
            converted.tools,
            Some(ToolSelector::Only(vec!["read".into()]))
        );
        assert_eq!(converted.budget, budget);
        assert!(!converted.cache_hint);
        assert!(converted.result_contract.is_none());
        assert!(converted.await_result);
    }

    #[test]
    fn fork_spec_default_cache_hint_is_true() {
        let spec = ForkSpec::new("x");
        assert!(spec.cache_hint);
        let converted: SubagentSpec = spec.into();
        assert!(converted.cache_hint);
    }

    #[test]
    fn fork_spec_default_keep_alive_is_false_and_the_builder_maps_through() {
        // Existing autonomous fork behavior must be unchanged by default.
        let default_spec = ForkSpec::new("x");
        assert!(!default_spec.keep_alive);
        let default_converted: SubagentSpec = default_spec.into();
        assert!(!default_converted.keep_alive);

        let opted_in = ForkSpec::new("x").keep_alive(true);
        assert!(opted_in.keep_alive);
        let converted: SubagentSpec = opted_in.into();
        assert!(converted.keep_alive);
    }

    #[test]
    fn spawn_spec_converts_with_mode_spawn_and_every_field_mapped() {
        let budget = Budget {
            max_steps: 12,
            deadline: None,
            max_tokens: None,
            max_tool_calls: None,
        };
        let spec = SpawnSpec::new("review this")
            .agent_def("reviewer")
            .role(RoleAlias::new("fast"))
            .tools(ToolSelector::All)
            .budget(budget.clone());

        let converted: SubagentSpec = spec.into();
        assert_eq!(converted.mode, SubagentMode::Spawn);
        assert_eq!(converted.prompt, "review this");
        assert_eq!(converted.agent_def, Some(AgentDefRef("reviewer".into())));
        assert_eq!(converted.role, Some(RoleAlias::new("fast")));
        assert_eq!(converted.tools, Some(ToolSelector::All));
        assert_eq!(converted.budget, budget);
        assert!(
            !converted.cache_hint,
            "spawn always forces cache_hint false"
        );
        assert!(converted.await_result);
    }

    #[test]
    fn spawn_spec_without_agent_def_converts_to_none_not_a_placeholder() {
        // `agent_def` is optional now -- omitting it must NOT invent a
        // placeholder `AgentDefRef`, it must convert to a plain `None` so
        // `SubagentHost::start` takes the "inherit the parent's role/model"
        // path.
        let spec = SpawnSpec::new("please review");
        assert_eq!(spec.agent_def, None);

        let converted: SubagentSpec = spec.into();
        assert_eq!(converted.mode, SubagentMode::Spawn);
        assert_eq!(converted.agent_def, None);
    }

    #[test]
    fn spawn_spec_default_keep_alive_is_false_and_the_builder_maps_through() {
        let default_spec = SpawnSpec::new("x");
        assert!(!default_spec.keep_alive);
        let default_converted: SubagentSpec = default_spec.into();
        assert!(!default_converted.keep_alive);

        let opted_in = SpawnSpec::new("x").keep_alive(true);
        assert!(opted_in.keep_alive);
        let converted: SubagentSpec = opted_in.into();
        assert!(converted.keep_alive);
    }
}
