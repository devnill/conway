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
use conway_core::log::AskOrigin;

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
    /// A `SessionMeta`-listing-visibility bit (P-2 provenance is unaffected
    /// -- the child stays attached to the live `AgentTreeSnapshot`
    /// regardless), NOT a third subagent mode (P-1: `ask` is fork+await-text,
    /// built on top of fork, never a new primitive). Defaults `false` via
    /// [`ForkSpec::new`], preserving the pre-existing non-ephemeral fork
    /// behavior unchanged. Set via [`ForkSpec::ephemeral`]. Mirrors
    /// [`conway_core::agent::SubagentSpec::ephemeral`]'s own doc.
    pub ephemeral: bool,
    /// Which `/ask`-style path is creating this child, stamped verbatim
    /// into the child's durable `SessionMeta::ask_origin`. `None` (the
    /// [`ForkSpec::new`] default) is the ordinary, non-ask fork path.
    /// **Ask is fork-only (P-1)**: `SpawnSpec` deliberately has no matching
    /// field at all, so a caller cannot express "spawn with an ask origin"
    /// -- an incoherent combination the type system rules out rather than
    /// rejecting at runtime. Set via [`ForkSpec::ask_origin`]. Mirrors
    /// [`conway_core::agent::SubagentSpec::ask_origin`]'s own doc.
    pub ask_origin: Option<AskOrigin>,
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
            ephemeral: false,
            ask_origin: None,
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

    /// See [`ForkSpec::ephemeral`]'s own field doc.
    pub fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    /// See [`ForkSpec::ask_origin`]'s own field doc.
    pub fn ask_origin(mut self, ask_origin: AskOrigin) -> Self {
        self.ask_origin = Some(ask_origin);
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
            ephemeral: spec.ephemeral,
            ask_origin: spec.ask_origin,
            // Deliberately NOT exposed on `ForkSpec` (C1): a fork inherits
            // the forker's ENTIRE context (GP-02), so a `ForkSpec` field
            // saying "but scope tools to this other directory" would be
            // incoherent with the context the child actually sees -- the
            // child's own transcript would keep describing the forker's
            // directory while its tools silently resolved somewhere else.
            // `cwd` is a `SpawnSpec`-only concept; see that struct's `cwd`
            // field and `From<SpawnSpec>` below.
            cwd: None,
            // (S3) Deliberately NOT exposed on `ForkSpec` either, for the
            // exact same reason as `cwd` immediately above: a fork inherits
            // the forker's entire context, so a confinement override here
            // would be incoherent with what the child's own inherited
            // transcript already describes. `root` is a `SpawnSpec`-only
            // concept; see that struct's `root` field and
            // `From<SpawnSpec>` below.
            root: None,
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
    /// (C1) Scopes the spawned child to its own working directory instead
    /// of unconditionally inheriting this session's -- an embedder (Kepler)
    /// scoping a drill-down explorer child to one region of a codebase is
    /// the motivating case. `None` (the [`SpawnSpec::new`] default)
    /// preserves the pre-existing "inherit the parent's cwd" behavior
    /// unchanged. Set via [`SpawnSpec::cwd`]; see
    /// [`conway_core::agent::SubagentSpec::cwd`]'s own doc for the exact
    /// absolute/relative/nonexistent-path semantics
    /// `conway_runtime::SubagentHost::start` resolves this against.
    ///
    /// Deliberately NOT a field on [`ForkSpec`] -- see that struct's own
    /// `From` impl for why a fork's inherited-context semantics make a cwd
    /// override incoherent there.
    pub cwd: Option<std::path::PathBuf>,
    /// (S3) Scopes the spawned child's confinement root, independent of (but
    /// validated against) `cwd` above -- the embedder-only surface for the
    /// "root plumbing" slice: `conway_subagent`/`conway_spawn` (the
    /// model-invoked tools) gain no equivalent argument (GP-04), exactly as
    /// they have none for `cwd` today. `None` (the [`SpawnSpec::new`]
    /// default) preserves the pre-existing "inherit the parent's root"
    /// behavior unchanged -- including staying unconfined when the parent
    /// itself is.
    ///
    /// `Some(requested)` is validated by `conway_runtime::SubagentHost::
    /// start` against the parent's own root per the inheritance algebra: a
    /// `requested` that narrows (or the parent has no root to narrow
    /// against) is accepted; a `requested` that is wider than, or disjoint
    /// from, the parent's root FAILS THE SPAWN with a typed error --
    /// silent clamping to the parent's root is deliberately never done (a
    /// silent narrowing would turn an operator's mistake into a working-
    /// but-not-what-was-asked-for configuration). See
    /// [`conway_core::agent::SubagentSpec::root`]'s own doc for the full
    /// semantics, including the explicit "not itself enforcement yet"
    /// caveat -- nothing checks a tool call's arguments against this root
    /// until a later slice wires the actual confinement check.
    ///
    /// Deliberately NOT a field on [`ForkSpec`] -- see that struct's own
    /// `From` impl for why a fork's inherited-context semantics make a root
    /// override incoherent there.
    pub root: Option<std::path::PathBuf>,
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
            cwd: None,
            root: None,
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

    /// See [`SpawnSpec::cwd`]'s own field doc.
    pub fn cwd(mut self, cwd: impl Into<std::path::PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// See [`SpawnSpec::root`]'s own field doc.
    pub fn root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.root = Some(root.into());
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
            ask_origin: None,
            cwd: spec.cwd,
            root: spec.root,
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
        assert_eq!(
            converted.cwd, None,
            "ForkSpec has no cwd field at all -- a fork always inherits the forker's cwd"
        );
        assert_eq!(
            converted.root, None,
            "ForkSpec has no root field at all -- a fork always inherits the forker's root"
        );
        assert!(
            !converted.ephemeral,
            "ephemeral defaults false when not set via the builder"
        );
        assert_eq!(
            converted.ask_origin, None,
            "ask_origin defaults None when not set via the builder"
        );
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

    /// The ephemeral-ask shape (P-1: `ask` is fork+await-text, not a third
    /// primitive) round-trips through `From<ForkSpec> for SubagentSpec`:
    /// `ephemeral: true` plus a concrete `ask_origin` both survive the
    /// conversion unchanged, and each defaults to today's non-ask behavior
    /// (`false`/`None`) when left unset.
    #[test]
    fn fork_spec_ephemeral_ask_shape_round_trips_through_the_conversion() {
        let default_spec = ForkSpec::new("x");
        assert!(!default_spec.ephemeral);
        assert_eq!(default_spec.ask_origin, None);
        let default_converted: SubagentSpec = default_spec.into();
        assert!(!default_converted.ephemeral);
        assert_eq!(default_converted.ask_origin, None);

        let ask_spec = ForkSpec::new("summarize this")
            .ephemeral(true)
            .ask_origin(AskOrigin::ToolAsk);
        assert!(ask_spec.ephemeral);
        assert_eq!(ask_spec.ask_origin, Some(AskOrigin::ToolAsk));

        let converted: SubagentSpec = ask_spec.into();
        assert_eq!(converted.mode, SubagentMode::Fork);
        assert!(
            converted.ephemeral,
            "ephemeral must survive the ForkSpec -> SubagentSpec conversion"
        );
        assert_eq!(
            converted.ask_origin,
            Some(AskOrigin::ToolAsk),
            "ask_origin must survive the ForkSpec -> SubagentSpec conversion"
        );
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
        assert_eq!(converted.cwd, None, "cwd defaults to None (inherit)");
        assert_eq!(converted.root, None, "root defaults to None (inherit)");
    }

    /// (a) C1's own acceptance test: `SpawnSpec::cwd` maps through
    /// `From<SpawnSpec> for SubagentSpec` unchanged, both when set and when
    /// left at its default `None`.
    #[test]
    fn spawn_spec_cwd_maps_through_from_spawn_spec() {
        let default_spec = SpawnSpec::new("x");
        assert_eq!(default_spec.cwd, None);
        let default_converted: SubagentSpec = default_spec.into();
        assert_eq!(default_converted.cwd, None);

        let scoped = SpawnSpec::new("x").cwd("/some/scoped/dir");
        assert_eq!(
            scoped.cwd,
            Some(std::path::PathBuf::from("/some/scoped/dir"))
        );
        let converted: SubagentSpec = scoped.into();
        assert_eq!(
            converted.cwd,
            Some(std::path::PathBuf::from("/some/scoped/dir"))
        );
    }

    /// (S3) `SpawnSpec::root` maps through `From<SpawnSpec> for
    /// SubagentSpec` unchanged, both when set and when left at its default
    /// `None` -- mirrors `spawn_spec_cwd_maps_through_from_spawn_spec`.
    #[test]
    fn spawn_spec_root_maps_through_from_spawn_spec() {
        let default_spec = SpawnSpec::new("x");
        assert_eq!(default_spec.root, None);
        let default_converted: SubagentSpec = default_spec.into();
        assert_eq!(default_converted.root, None);

        let scoped = SpawnSpec::new("x").root("/some/scoped/root");
        assert_eq!(
            scoped.root,
            Some(std::path::PathBuf::from("/some/scoped/root"))
        );
        let converted: SubagentSpec = scoped.into();
        assert_eq!(
            converted.root,
            Some(std::path::PathBuf::from("/some/scoped/root"))
        );
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
