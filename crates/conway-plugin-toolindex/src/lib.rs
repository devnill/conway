//! `conway.toolindex`: deferred tool schemas -- a first-party plugin
//! (board item `01M1YS138H8T0HNV5YMZ6KD767` part 1) that narrows every
//! *deferrable* tool's announced JSON schema down to a one-line
//! `name: description (call describe_tool(name="...") for the full
//! schema)` index entry, plus a `describe_tool` tool that returns the full
//! description + schema on demand -- and, once called for a given tool,
//! keeps THAT tool fully announced for the rest of the session.
//!
//! This is the tool-shaped analog of `conway-plugin-skills`'s progressive
//! *skill* disclosure (that crate's own module doc is worth reading first
//! -- this crate mirrors its "narrow in `ContextHook::before_request`,
//! serve the full text through a companion `Tool`" composition almost
//! exactly). The reason a SEPARATE plugin exists rather than widening
//! `conway.skills` is what it narrows: `conway.skills` edits a
//! `Provenance::Skill` prompt SEGMENT's `content`; this plugin edits
//! [`conway::plugin::ContextPayload::tools`] itself -- the native `tools`
//! array a backend actually receives, entirely independent of any
//! `Role::System` segment (see "Why the tool-registry segment carries no
//! text" below).
//!
//! # The problem, measured (re-verify -- specs here go stale fast)
//!
//! A real session with one external MCP tool server installed showed the
//! `tool_registry` segment at ~10,087 estimated tokens, versus ~4.4k with
//! only built-ins -- on a 32.7k window that alone fires the runway notice
//! on turn one. `crates/conway-runtime/src/context/builder.rs`'s own
//! `[2] ToolSchemas` comment independently measured a representative
//! 14-tool built-in set at ~3.4k tokens; an installed MCP/subprocess-
//! plugin server on top of that is what pushes a small window over the
//! edge.
//!
//! # Why the tool-registry segment carries no text
//!
//! `ContextBuilder::build`'s `[2] ToolSchemas` segment (`Provenance::
//! ToolRegistry`) is deliberately assembled with EMPTY `content` -- the
//! native `tools` array (`ContextPayload::tools`, later
//! `ContextInput::tools`) is the ONE copy of every tool's schema that is
//! actually sent; a `Role::System` text segment restating the same JSON
//! would be a full second copy, paid on every turn (see that module's own
//! comment for the measurement). Its `tokens_est` is instead computed
//! directly from `ContextPayload::tools` by
//! `estimate_tool_schemas_tokens`/`retotal`, AFTER any `ContextHook` has
//! run -- so a hook that narrows `payload.tools` (this crate's whole job)
//! is the ONLY way to shrink that segment's reported cost, and doing so
//! needs no change to `conway-runtime` at all: the hook seam and the
//! re-totalling already exist for exactly this purpose (`ContextPayload`'s
//! own doc, `crate::context::builder::retotal`'s own doc, "a hook may
//! narrow/replace the announced tool set").
//!
//! # Which tools are deferrable, and why this is mechanical, not a guess
//!
//! Built-in `conway.fs`/`conway.shell`/`conway.subagent`/`conway.report`
//! tools -- [`conway::presets::builtin_plugins`]'s own four candidates,
//! read here at construction time via that SAME public function
//! `conway`'s own `ConwayBuilder::with_builtin_plugins` uses, no privileged
//! registry access -- stay FULLY announced always: they are used on
//! essentially every turn (read/write/edit/bash/fork/spawn/report), so
//! deferring them would trade a real per-turn extra round trip for no
//! measured win. [`TOOL_NAME`] itself (`describe_tool`) is ALSO always
//! fully announced, structurally: if it were deferred, no tool could ever
//! be un-deferred (a model cannot call `describe_tool` to learn about
//! `describe_tool`). Every other name this plugin ever sees in
//! `ContextPayload::tools` is deferred -- MCP tools, subprocess-plugin
//! tools, and (a known, disclosed simplification) any other installed
//! plugin's own tool (`read_skill`, `remember`, `ask_question`, ...): a
//! bare `ToolSpec` carries no `plugin_id`/source field
//! (`conway_core::content::ToolSpec`'s own four fields --
//! `name`/`description`/`schema`/`category`/`permission`, nothing else),
//! so "built-in vs. everything else" is the finest mechanical distinction
//! this seam can actually draw without a core change. This costs a small,
//! already-cheap tool (e.g. `read_skill`'s one-string schema) at most one
//! extra `describe_tool` round trip the first time it is called; it never
//! produces incorrect behavior (see "How the wire stays coherent" below).
//! **Nothing here is a model-driven guess about which tools "matter"** --
//! membership in the always-announced set is a fixed, computed-once name
//! list, never inferred from a tool's schema size, description text, or
//! any runtime signal.
//!
//! # How the wire stays coherent -- no un-announced call, ever
//!
//! **Every deferrable tool stays IN `ContextPayload::tools`, under its own
//! real name, on every single turn.** Narrowing never removes an entry --
//! it only replaces that entry's `description` with the one-line index
//! text and its `schema` with `deferred_schema`, a trivial, always-valid
//! placeholder (`{}`, an empty-properties object). This is the design
//! choice `docs/plugins/toolindex.md`/the board item's own brief asks for
//! explicitly ("the index entry has to be a real announced tool the model
//! can reach"): because the tool is always present in the announced set,
//! under the SAME name, a call to it is -- by construction -- always a
//! call to an announced tool. There is no new "was this announced this
//! turn" check to add anywhere, and
//! `conway_runtime::context::hook_guard::ensure_hook_payload_coherent`
//! needs no change and sees no new failure mode: that guard checks
//! `ToolUse`/`ToolResult` PAIRING within `payload.segments`
//! (`check_tool_call_coherence`), and this hook never touches `segments`
//! at all -- only `payload.tools`.
//!
//! Two more facts make this genuinely safe, not merely unobjected-to:
//!
//! 1. **Real argument validation never reads the narrowed schema.**
//!    `conway_runtime::tools::registry::PluginRegistry::from_plugins`
//!    compiles each tool's `jsonschema::Validator` ONCE, at registry
//!    construction, from that tool's own real `Tool::spec()` -- entirely
//!    before, and independently of, whatever a `ContextHook` later shows
//!    the model for a given turn (`PluginRegistry::specs`'s own doc:
//!    "Announcement... may further narrow the tools this method returns
//!    before it reaches the router/backend... Execution... decides
//!    whether a call... is allowed to run, regardless of what was
//!    announced"). A model that calls a still-narrowed tool with a guessed
//!    argument set is validated against the REAL schema and refused with
//!    the EXISTING, already-typed `ToolError` on mismatch -- no new
//!    refusal path, no silent fetch, nothing invented for this plugin.
//! 2. **`describe_tool` answers from data this plugin itself observed
//!    being announced, never a privileged registry read.** Every
//!    `before_request` call caches each currently-announced tool's REAL,
//!    pre-narrowing `ToolSpec` into a shared map (`full_specs`) BEFORE
//!    narrowing its own local copy -- `agent_loop.rs`'s `tool_specs` local
//!    is freshly recomputed from the real registry every turn
//!    (`self.deps.registry.specs(...)`), so this cache is always accurate,
//!    never stale, and never requires this crate to depend on
//!    `conway-runtime` or hold any registry handle at all. `describe_tool`
//!    just looks a name up in that cache -- the same "no privileged
//!    channel" shape `conway-plugin-skills`'s own module doc establishes
//!    for `read_skill`, adapted from a static, upfront-loaded map to a
//!    dynamic, turn-populated one (skills load once from disk; the tool
//!    universe genuinely is not known until the first request is
//!    assembled, since an MCP server's own tool list is only available
//!    after that server's handshake).
//!
//! # Revealing a tool
//!
//! `DescribeToolTool::invoke` records the requested name into a shared
//! `revealed` map keyed by [`conway::AgentId`] (mirroring
//! `conway-plugin-stepguard`'s own per-agent state keying, "sibling agents
//! in a fan-out never pool" their state) -- the SAME shared state
//! `ToolIndexHook::before_request` consults before narrowing anything on
//! every SUBSEQUENT turn for that agent. Once revealed, a tool stays fully
//! announced (real description, real schema) for the rest of that agent's
//! run; a sibling or later agent that never called `describe_tool` for the
//! same name still sees it narrowed. An unknown name is a model-visible
//! `is_error: true` result naming the failure ("no such tool: ..."),
//! never a hard `Err`/crash -- the identical fail-safe shape
//! `conway-plugin-skills`'s own `read_skill` uses for an unknown skill
//! name.
//!
//! # Default set or opt-in -- decided here, opt-in
//!
//! **This plugin is opt-in, not part of `first_party_plugins::
//! DEFAULT_OPINION_SET`.** Unlike `conway.skills` (whose narrowing is
//! author-controlled and uniformly safe -- an operator's own skill files),
//! this plugin changes MODEL-FACING tool-calling behavior for every
//! non-built-in tool: the model's first attempt at any deferred tool may
//! need an extra `describe_tool` round trip before it has the real
//! argument schema, a real (if usually small) reliability/latency
//! tradeoff `first_party_plugins::DEFAULT_OPINION_SET`'s own operator
//! ruling (`01M1FQFP5D0R3M9GC8R8Z24F5N`, six then-named ids) never
//! evaluated. Turning it on unprompted, for every fresh install, on a
//! decision that ruling never considered would be this crate overriding
//! that ruling rather than extending it -- exactly what `conway.web`'s own
//! `lib.rs` doc argues for staying opt-in despite being first-party and
//! bundled (`crates/conway-cli/src/first_party_plugins.rs`'s own bundle
//! doc). An operator who wants the token savings for a large MCP install
//! opts in explicitly: `"plugins": { "install": ["conway.toolindex"] }`.
//!
//! # Installing it
//!
//! ```json
//! { "plugins": { "install": ["conway.toolindex"] } }
//! ```
//!
//! With it uninstalled, `ContextPayload::tools` reaches the backend
//! unedited -- the runtime's context hook stays unset, exactly as before
//! this plugin existed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use conway::plugin::{
    async_trait, ContentBlock, ContextHook, ContextHookCtx, ContextPayload, PathArgs,
    PermissionClass, Plugin, PluginDescription, PluginManifest, Tool, ToolCall, ToolCategory,
    ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};
use conway::AgentId;

/// The install id an operator names in `plugins.install`.
pub const PLUGIN_ID: &str = "conway.toolindex";

/// The bare name `DescribeToolTool` registers under -- reachable by the
/// model as `describe_tool` once this plugin is installed.
pub const TOOL_NAME: &str = "describe_tool";

/// The one-line index entry a deferrable tool's `description` is narrowed
/// to: `name: description (call describe_tool(name="...") for the full
/// schema)`, or `name (call describe_tool(name="...") for the full
/// schema)` when the tool carries no (or only whitespace) description.
/// Kept as a plain function so the hook and any test share one source of
/// truth for the exact index form -- mirrors `conway_plugin_skills`'s own
/// `index_entry`.
fn index_entry(spec: &ToolSpec) -> String {
    let description = spec.description.trim();
    if description.is_empty() {
        format!(
            "{} (call describe_tool(name=\"{}\") for the full schema)",
            spec.name, spec.name
        )
    } else {
        format!(
            "{}: {description} (call describe_tool(name=\"{}\") for the full schema)",
            spec.name, spec.name
        )
    }
}

/// A trivial, always-valid `{}` schema (an object with no declared
/// properties, no `additionalProperties` restriction) -- what a
/// deferrable tool's real schema is replaced with in the announced set.
/// Never read for real argument validation (see this crate's own module
/// doc, "Real argument validation never reads the narrowed schema") --
/// its only job is costing far fewer tokens than the schema it stands in
/// for.
fn deferred_schema() -> schemars::schema::RootSchema {
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct DeferredToolArgs {}
    schemars::schema_for!(DeferredToolArgs)
}

/// Context-assembly half: narrows every deferrable tool's announced
/// `description`/`schema` down to [`index_entry`]/[`deferred_schema`],
/// leaving [`Self::always_announced`] members and any tool already
/// [`Self::revealed`] for this call's agent completely unchanged. See this
/// crate's own module doc, "How the wire stays coherent", for why this
/// never produces an unreachable tool.
struct ToolIndexHook {
    always_announced: Arc<HashSet<ToolName>>,
    full_specs: Arc<Mutex<HashMap<ToolName, ToolSpec>>>,
    revealed: Arc<Mutex<HashMap<AgentId, HashSet<ToolName>>>>,
}

#[async_trait]
impl ContextHook for ToolIndexHook {
    async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        mut payload: ContextPayload,
    ) -> ContextPayload {
        // Cache every currently-announced tool's REAL spec BEFORE
        // narrowing anything -- the only source `describe_tool` can answer
        // from (module doc, "no privileged registry read"). `tool_specs`
        // is freshly re-derived from the real registry every turn
        // (`agent_loop.rs`), so this is never stale.
        {
            let mut cache = self.full_specs.lock().expect("full_specs mutex poisoned");
            for spec in &payload.tools {
                cache.insert(spec.name.clone(), spec.clone());
            }
        }

        let revealed_for_agent = {
            let revealed = self.revealed.lock().expect("revealed mutex poisoned");
            revealed.get(&ctx.agent_id).cloned().unwrap_or_default()
        };

        for spec in &mut payload.tools {
            if self.always_announced.contains(&spec.name) || revealed_for_agent.contains(&spec.name)
            {
                continue;
            }
            let index = index_entry(spec);
            spec.description = index;
            spec.schema = deferred_schema();
        }
        payload
    }
}

/// Tool-execution half: an ordinary `Tool` (`docs/plugins/hooks.md` point
/// 2, Implemented) -- looks the full spec up by name in the shared cache
/// [`ToolIndexHook::before_request`] populates, then marks it revealed for
/// this call's agent so every subsequent turn announces it in full.
struct DescribeToolTool {
    full_specs: Arc<Mutex<HashMap<ToolName, ToolSpec>>>,
    revealed: Arc<Mutex<HashMap<AgentId, HashSet<ToolName>>>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct DescribeToolArgs {
    /// The tool name to fetch the full description and JSON schema for --
    /// the same `name` a narrowed announcement's index entry advertises.
    name: String,
}

#[async_trait]
impl Tool for DescribeToolTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(TOOL_NAME),
            description: "Fetch a deferred tool's full description and JSON schema by name, \
                          from the one-line index the conway.toolindex plugin narrows its \
                          announcement down to. Calling this makes that tool fully announced \
                          (real schema) for the rest of this agent's turns."
                .to_string(),
            schema: schemars::schema_for!(DescribeToolArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: DescribeToolArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let name = ToolName::new(args.name.clone());
        let found = {
            let cache = self.full_specs.lock().expect("full_specs mutex poisoned");
            cache.get(&name).cloned()
        };
        Ok(match found {
            Some(spec) => {
                {
                    let mut revealed = self.revealed.lock().expect("revealed mutex poisoned");
                    revealed.entry(ctx.agent_id).or_default().insert(name);
                }
                let schema_text = serde_json::to_string_pretty(&spec.schema)
                    .unwrap_or_else(|_| "<schema failed to serialize>".to_string());
                let text = format!(
                    "{}: {}\n\nJSON Schema:\n{schema_text}",
                    spec.name, spec.description
                );
                ToolOutput {
                    blocks: vec![ContentBlock::Text { text }],
                    is_error: false,
                    truncation: TruncationPolicy::None,
                    artifacts: Vec::new(),
                }
            }
            None => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: format!("no such tool: {}", args.name),
                }],
                // Model-visible feedback, never a hard Err/crash -- the
                // same fail-safe path `conway_plugin_skills::ReadSkillTool`
                // takes for an unknown skill name.
                is_error: true,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> conway::plugin::RenderKind {
        conway::plugin::RenderKind::Structured
    }
}

/// Every tool name from [`conway::presets::builtin_plugins`] -- the SAME
/// public candidate list `conway`'s own `ConwayBuilder::
/// with_builtin_plugins` uses -- plus [`TOOL_NAME`] itself. See this
/// crate's own module doc, "Which tools are deferrable, and why this is
/// mechanical, not a guess".
fn always_announced_names() -> HashSet<ToolName> {
    let mut names: HashSet<ToolName> = conway::presets::builtin_plugins()
        .iter()
        .flat_map(|plugin| plugin.tools())
        .map(|tool| tool.spec().name)
        .collect();
    names.insert(ToolName::new(TOOL_NAME));
    names
}

/// The plugin. Holds the shared `full_specs`/`revealed` state both halves
/// read and write, plus the fixed `always_announced` name set computed
/// once at construction.
pub struct ToolIndexPlugin {
    always_announced: Arc<HashSet<ToolName>>,
    full_specs: Arc<Mutex<HashMap<ToolName, ToolSpec>>>,
    revealed: Arc<Mutex<HashMap<AgentId, HashSet<ToolName>>>>,
}

impl std::fmt::Debug for ToolIndexPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolIndexPlugin")
            .field("always_announced_count", &self.always_announced.len())
            .finish_non_exhaustive()
    }
}

impl ToolIndexPlugin {
    /// Builds the plugin, computing `Self::always_announced` from
    /// [`conway::presets::builtin_plugins`] once.
    pub fn new() -> Self {
        Self {
            always_announced: Arc::new(always_announced_names()),
            full_specs: Arc::new(Mutex::new(HashMap::new())),
            revealed: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Default for ToolIndexPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ToolIndexPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![ToolName::new(TOOL_NAME)],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "deferred tool schemas -- keeps a large or rarely-used tool's full JSON \
                      schema out of context until the model actually asks for it"
                .to_string(),
            you_get: format!(
                "1 tool ({TOOL_NAME}) and a narrowed one-line announcement for every non-built-in \
                 tool -- full schemas load on demand instead of on every turn, which is most of \
                 the win when an MCP server or subprocess plugin registers many tools"
            ),
            you_lose: "every non-built-in tool's first call may cost the model one extra \
                       describe_tool round trip before it has that tool's real argument schema"
                .to_string(),
            costs: format!("none beyond the {TOOL_NAME} calls the model makes"),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(DescribeToolTool {
            full_specs: self.full_specs.clone(),
            revealed: self.revealed.clone(),
        })]
    }

    fn context_hooks(&self) -> Vec<Arc<dyn ContextHook>> {
        vec![Arc::new(ToolIndexHook {
            always_announced: self.always_announced.clone(),
            full_specs: self.full_specs.clone(),
            revealed: self.revealed.clone(),
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, description: &str) -> ToolSpec {
        // Read by `schema_for!` below, never by Rust -- this struct
        // exists to GENERATE a schema, so its fields are live to the
        // derive macro and dead to the compiler.
        #[allow(dead_code)]
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        struct RichArgs {
            /// A representative field, so the "before" schema is
            /// meaningfully larger than the one-line index it narrows to.
            query: String,
            /// A second representative field.
            limit: Option<u32>,
        }
        ToolSpec {
            name: ToolName::new(name),
            description: description.to_string(),
            schema: schemars::schema_for!(RichArgs),
            category: ToolCategory::Execute,
            permission: PermissionClass::Dangerous,
        }
    }

    fn hook_ctx(agent_id: AgentId) -> ContextHookCtx {
        ContextHookCtx {
            agent_id,
            agent_path: vec![agent_id],
            session_id: conway::SessionId::new(),
            turn: 0,
            model: None,
            estimated_tokens: 100,
            artifacts: conway::plugin::ArtifactWriteHandle::noop(agent_id),
            tag: None,
        }
    }

    // -----------------------------------------------------------------
    // `index_entry` -- the exact one-line form, with and without a
    // description.
    // -----------------------------------------------------------------

    #[test]
    fn index_entry_includes_description_and_the_describe_tool_call() {
        let s = spec("mcp_search", "Search the configured index.");
        let text = index_entry(&s);
        assert!(text.starts_with("mcp_search: Search the configured index."));
        assert!(text.contains("describe_tool(name=\"mcp_search\")"));
    }

    #[test]
    fn index_entry_with_no_description_has_no_empty_prefix() {
        let s = spec("mcp_noisy", "");
        let text = index_entry(&s);
        assert!(
            !text.starts_with("mcp_noisy: "),
            "no description must not produce an empty `name: ` prefix: {text}"
        );
        assert!(text.contains("describe_tool(name=\"mcp_noisy\")"));
    }

    // -----------------------------------------------------------------
    // The hook: narrows a deferrable tool, leaves an always-announced one
    // (and a revealed one) completely unchanged.
    // -----------------------------------------------------------------

    fn hook_with(always_announced: &[&str]) -> ToolIndexHook {
        ToolIndexHook {
            always_announced: Arc::new(
                always_announced.iter().map(|n| ToolName::new(*n)).collect(),
            ),
            full_specs: Arc::new(Mutex::new(HashMap::new())),
            revealed: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[tokio::test]
    async fn a_deferrable_tool_is_narrowed_to_the_one_line_index() {
        let hook = hook_with(&["read"]);
        let agent_id = AgentId::new();
        let payload = ContextPayload {
            segments: vec![],
            tools: vec![spec("mcp_search", "Search the configured index.")],
        };
        let out = hook.before_request(&hook_ctx(agent_id), payload).await;
        let narrowed = &out.tools[0];
        assert!(narrowed
            .description
            .contains("describe_tool(name=\"mcp_search\")"));
        // The narrowed schema must be dramatically smaller than the real
        // one -- proves the SCHEMA, not only the description, shrank.
        let narrowed_len = serde_json::to_string(&narrowed.schema).unwrap().len();
        let real_len =
            serde_json::to_string(&spec("mcp_search", "Search the configured index.").schema)
                .unwrap()
                .len();
        assert!(
            narrowed_len < real_len,
            "narrowed schema ({narrowed_len} bytes) must be smaller than the real one \
             ({real_len} bytes)"
        );
    }

    #[tokio::test]
    async fn an_always_announced_tool_is_left_completely_unchanged() {
        let hook = hook_with(&["read"]);
        let agent_id = AgentId::new();
        let original = spec("read", "Read a file from disk.");
        let payload = ContextPayload {
            segments: vec![],
            tools: vec![original.clone()],
        };
        let out = hook.before_request(&hook_ctx(agent_id), payload).await;
        assert_eq!(out.tools[0].description, original.description);
        assert_eq!(
            serde_json::to_value(&out.tools[0].schema).unwrap(),
            serde_json::to_value(&original.schema).unwrap()
        );
    }

    #[tokio::test]
    async fn a_revealed_tool_stays_fully_announced_on_a_later_turn() {
        let hook = hook_with(&["read"]);
        let agent_id = AgentId::new();
        let original = spec("mcp_search", "Search the configured index.");

        // Turn 1: narrowed (not yet revealed).
        let out1 = hook
            .before_request(
                &hook_ctx(agent_id),
                ContextPayload {
                    segments: vec![],
                    tools: vec![original.clone()],
                },
            )
            .await;
        assert_ne!(out1.tools[0].description, original.description);

        // Simulate `describe_tool` having been called for this agent.
        hook.revealed
            .lock()
            .unwrap()
            .entry(agent_id)
            .or_default()
            .insert(ToolName::new("mcp_search"));

        // Turn 2: fully announced again, unchanged.
        let out2 = hook
            .before_request(
                &hook_ctx(agent_id),
                ContextPayload {
                    segments: vec![],
                    tools: vec![original.clone()],
                },
            )
            .await;
        assert_eq!(out2.tools[0].description, original.description);
        assert_eq!(
            serde_json::to_value(&out2.tools[0].schema).unwrap(),
            serde_json::to_value(&original.schema).unwrap()
        );
    }

    #[tokio::test]
    async fn a_revealed_tool_for_one_agent_stays_narrowed_for_a_sibling() {
        let hook = hook_with(&["read"]);
        let revealed_agent = AgentId::new();
        let sibling_agent = AgentId::new();
        let original = spec("mcp_search", "Search the configured index.");
        hook.revealed
            .lock()
            .unwrap()
            .entry(revealed_agent)
            .or_default()
            .insert(ToolName::new("mcp_search"));

        let out = hook
            .before_request(
                &hook_ctx(sibling_agent),
                ContextPayload {
                    segments: vec![],
                    tools: vec![original.clone()],
                },
            )
            .await;
        assert_ne!(
            out.tools[0].description, original.description,
            "a sibling agent that never called describe_tool must still see the narrowed form"
        );
    }

    #[tokio::test]
    async fn before_request_caches_every_announced_tools_real_spec() {
        let hook = hook_with(&["read"]);
        let agent_id = AgentId::new();
        let original = spec("mcp_search", "Search the configured index.");
        let _ = hook
            .before_request(
                &hook_ctx(agent_id),
                ContextPayload {
                    segments: vec![],
                    tools: vec![original.clone()],
                },
            )
            .await;
        let cached = hook
            .full_specs
            .lock()
            .unwrap()
            .get(&ToolName::new("mcp_search"))
            .cloned()
            .expect("the real spec must be cached before narrowing");
        assert_eq!(cached.description, original.description);
    }

    // -----------------------------------------------------------------
    // Packaging: manifest id, non-empty description, one tool + one hook.
    // -----------------------------------------------------------------

    #[test]
    fn manifest_id_matches_the_published_constant() {
        let plugin = ToolIndexPlugin::new();
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
        assert_eq!(plugin.manifest().tools, vec![ToolName::new(TOOL_NAME)]);
    }

    #[test]
    fn description_is_non_empty() {
        let plugin = ToolIndexPlugin::new();
        let description = plugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
    }

    #[test]
    fn plugin_registers_one_tool_and_one_context_hook() {
        let plugin = ToolIndexPlugin::new();
        let tools = plugin.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].spec().name.as_str(), TOOL_NAME);
        assert_eq!(plugin.context_hooks().len(), 1);
    }

    /// `describe_tool` itself must never be deferrable -- otherwise no
    /// tool could ever be un-deferred (module doc, "structurally").
    #[test]
    fn describe_tool_is_always_in_the_always_announced_set() {
        let plugin = ToolIndexPlugin::new();
        assert!(plugin.always_announced.contains(&ToolName::new(TOOL_NAME)));
    }

    /// The four built-in candidates' own tools are all in the
    /// always-announced set -- proves this is read from the real
    /// `conway::presets::builtin_plugins` list, not a hand-typed guess
    /// that could drift from it.
    #[test]
    fn every_builtin_plugins_tool_is_always_announced() {
        let plugin = ToolIndexPlugin::new();
        for builtin in conway::presets::builtin_plugins() {
            for tool in builtin.tools() {
                assert!(
                    plugin.always_announced.contains(&tool.spec().name),
                    "{} must be in the always-announced set",
                    tool.spec().name
                );
            }
        }
    }
}
