//! `conway.skills`: progressive skill disclosure -- a first-party plugin
//! that narrows full-body `Provenance::Skill` context segments down to a
//! one-line `name: description (call read_skill(name="...") for the full
//! document)` index entry, plus a `read_skill` tool that returns the full
//! body on demand.
//!
//! This is the packaged form of `docs/plugins/cookbook.md` example 4
//! (board item `01M03GMNB3P048G72M158XPDG2`), which proved both halves
//! against a scratch crate outside this workspace: a `ContextHook`
//! narrowing a `Skill` segment exactly as example 1 narrows a
//! `ToolResult` one, and an ordinary companion `Tool` answering "fetch
//! the full document on invoke" -- neither needing anything the plugin
//! surface did not already ship. This crate ports that proven composition
//! into an installable, off-by-default first-party plugin, written
//! entirely against `conway::plugin` (the identical public surface a
//! third-party plugin author gets), mirroring
//! `crates/conway-plugin-stepguard`/`crates/conway-plugin-history`'s
//! shape.
//!
//! # Why this is a plugin
//!
//! `ContextBuilder` injects one `Provenance::Skill` segment per
//! configured skill, **full body, always** -- there is no
//! name/description-only mode built into assembly itself. Putting that
//! narrowing policy in core would make the harness author into the
//! model's context on its own initiative, the same §6 reason
//! `conway.stepguard`'s loop detection moved out. The seam
//! (`ContextHook::before_request` can edit any segment) ships in core;
//! the POLICY (which segments to narrow, to what one-line form, under what
//! threshold) attaches as this plugin. Nothing here is a recommended
//! default -- see `docs/plugins/cookbook.md` example 4's own "plausible
//! efficiency win, not a measured one" disclosure, restated rather than
//! re-derived: this crate demonstrates the architecture does not stand in
//! the way of progressive disclosure, it does not recommend it.
//!
//! # How the hook and tool reach skill bodies -- no privileged channel
//!
//! Both halves share one `Arc<HashMap<String, SkillDef>>` constructed at
//! plugin build time ([`SkillsPlugin::new`]/[`SkillsPlugin::from_dir`]).
//! The map is loaded by the SAME public `conway::skills::load_skill_defs`
//! the facade itself uses -- a third-party plugin could call it too. The
//! hook narrows a `Provenance::Skill { name }` segment using the name (read
//! from the segment's own provenance) plus the description (looked up in
//! the map); the tool returns the body (looked up in the same map). Neither
//! reaches into `Runtime.skills` or any runtime-internal state -- the map
//! is this plugin's own copy, the cookbook example 4's static `SKILLS`
//! table with a runtime-populated stand-in. See the board item's own
//! note: a first-party plugin needing a hook a third party cannot reach
//! would be a design failure; [`Plugin::context_hooks`] (the trait method
//! that lets this plugin's hook install through the SAME `with_plugin`/
//! `install_selected` surface its tool does) is what closes that gap
//! cleanly, and this crate is its first consumer.
//!
//! # Installing it
//!
//! ```json
//! { "plugins": { "install": ["conway.skills"] } }
//! ```
//!
//! With it uninstalled, `ContextBuilder`'s full-body `Skill` segments
//! reach the model unchanged -- the runtime's context hook stays unset.
//!
//! # What it does NOT do
//!
//! It never refuses a `read_skill` call for an unknown name with a hard
//! `Err`/crash -- it returns `is_error: true` with a model-visible "no
//! such skill" message (cookbook example 4's own failure path). A skill
//! segment whose name is not in this plugin's map is left COMPLETELY
//! UNCHANGED rather than narrowed or dropped (fail SAFE = "leave the model
//! with what it already had", the opposite direction from example 1's
//! spill hook, the same principle stated in the cookbook).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use conway::plugin::{
    async_trait, ContentBlock, ContextHook, ContextHookCtx, ContextPayload, PathArgs,
    PermissionClass, Plugin, PluginDescription, PluginManifest, Provenance, RenderKind, Tool,
    ToolCall, ToolCategory, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};
use conway::skills::load_skill_defs;
use conway::SkillDef;

/// The install id an operator names in `plugins.install` -- the same
/// `[plugins].install` surface `conway.stepguard`/`conway.history` use.
pub const PLUGIN_ID: &str = "conway.skills";

/// The bare name `ReadSkillTool` registers under -- reachable by the model
/// as `read_skill` once this plugin is installed.
pub const TOOL_NAME: &str = "read_skill";

/// Looks up a skill by name in `skills`. The plain function the cookbook
/// example 4's own second acceptance verdict drives directly -- factored
/// out of `Tool::invoke` so the lookup logic is testable without
/// constructing a `ToolCtx` (which only a live `ConwayBuilder` session can
/// produce, per `docs/plugins/cookbook.md`'s documented facade limit).
fn find_skill<'a>(skills: &'a HashMap<String, SkillDef>, name: &str) -> Option<&'a SkillDef> {
    skills.get(name)
}

/// The one-line index entry a full-body `Provenance::Skill { name }`
/// segment is narrowed to: `name: description (call read_skill(name="...")
/// for the full document)`, or `name (call read_skill(name="...") for the
/// full document)` when the skill carries no description. Kept as a
/// plain function so the hook and any test share one source of truth for
/// the exact index form.
fn index_entry(skill: &SkillDef) -> String {
    match &skill.description {
        Some(description) if !description.trim().is_empty() => format!(
            "{}: {} (call read_skill(name=\"{}\") for the full document)",
            skill.name, description, skill.name
        ),
        _ => format!(
            "{} (call read_skill(name=\"{}\") for the full document)",
            skill.name, skill.name
        ),
    }
}

/// Context-assembly half: narrows a full-body `Provenance::Skill` segment
/// down to the one-line [`index_entry`], pointing at `read_skill` for the
/// rest. A skill name the plugin's own map does not know about is left
/// COMPLETELY UNCHANGED (cookbook example 4's `an_unindexed_skill_segment_
/// is_left_alone` failure path -- fail SAFE = "leave the model with what
/// it already had").
struct SkillIndexHook {
    skills: Arc<HashMap<String, SkillDef>>,
}

#[async_trait]
impl ContextHook for SkillIndexHook {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        mut payload: ContextPayload,
    ) -> ContextPayload {
        for segment in &mut payload.segments {
            let Provenance::Skill { name } = &segment.provenance else {
                continue;
            };
            if let Some(skill) = find_skill(&self.skills, name) {
                segment.content = vec![ContentBlock::Text {
                    text: index_entry(skill),
                }];
            }
            // else: unindexed skill -- leave the segment untouched, never
            // drop it. See the module doc's "What it does NOT do" section.
        }
        payload
    }
}

/// Tool-execution half: an ordinary `Tool` (`docs/plugins/hooks.md` point
/// 2, Implemented) -- "fetch the full document on invoke" needed nothing
/// new at all. Looks the body up by name in the same shared skills map the
/// hook reads.
struct ReadSkillTool {
    skills: Arc<HashMap<String, SkillDef>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ReadSkillArgs {
    /// The skill name to fetch the full document for -- the same `name` a
    /// narrowed `Provenance::Skill` index entry advertises.
    name: String,
}

#[async_trait]
impl Tool for ReadSkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(TOOL_NAME),
            description: "Fetch a skill's full document by name, from the one-line index the \
                          conway.skills plugin narrows skill segments down to."
                .to_string(),
            schema: schemars::schema_for!(ReadSkillArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(
        &self,
        call: ToolCall,
        _ctx: conway::plugin::ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadSkillArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        Ok(match find_skill(&self.skills, &args.name) {
            Some(skill) => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: skill.body.clone(),
                }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
            None => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: format!("no such skill: {}", args.name),
                }],
                // Model-visible feedback, never a hard Err/crash -- the
                // cookbook example 4's own failure path: a model that
                // hallucinated a name learns it was wrong and can try
                // again, rather than the turn dying.
                is_error: true,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

/// The plugin. Holds the shared skills map both halves read; `Default`-free
/// because an empty map (no skills) would narrow nothing and serve only
/// "no such skill" replies, which is a uselessly-installed plugin rather
/// than a sensible default -- construct it explicitly via [`Self::new`] or
/// [`Self::from_dir`].
pub struct SkillsPlugin {
    skills: Arc<HashMap<String, SkillDef>>,
}

impl std::fmt::Debug for SkillsPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillsPlugin")
            .field("skill_count", &self.skills.len())
            .finish_non_exhaustive()
    }
}

impl SkillsPlugin {
    /// Builds the plugin from an already-loaded skills map -- the form a
    /// caller that already ran `conway::skills::load_skill_defs` (e.g. an
    /// embedder with its own skills directory, or a test fixture) uses.
    pub fn new(skills: Arc<HashMap<String, SkillDef>>) -> Self {
        Self { skills }
    }

    /// Loads `.conway/skills/<name>/SKILL.md` from `dir` via the SAME public
    /// `conway::skills::load_skill_defs` the facade's own builder uses, and
    /// builds the plugin from the result. The map this plugin holds is its
    /// OWN copy -- it does not reach into `Runtime.skills` (see the module
    /// doc's "no privileged channel" section). A caller that wants the
    /// plugin's view of skills to stay in lockstep with the runtime's own
    /// load points both at the same directory.
    pub fn from_dir(dir: &Path) -> conway::Result<Self> {
        let skills = load_skill_defs(dir)?;
        Ok(Self::new(Arc::new(skills)))
    }

    /// The hook on its own, for an embedder that wants to register it via
    /// `ConwayBuilder::with_context_hook` directly (e.g. a library user who
    /// is not installing this crate as a `Plugin` but still wants the
    /// narrowing). Installing via `with_plugin` instead routes through
    /// [`Plugin::context_hooks`], the SAME surface -- this method exists
    /// for the lower-level standalone case, mirroring the way
    /// `ConwayBuilder::with_context_hook` itself remains alongside the
    /// plugin-trait path.
    pub fn hook(&self) -> Arc<dyn ContextHook> {
        Arc::new(SkillIndexHook {
            skills: self.skills.clone(),
        })
    }
}

impl Plugin for SkillsPlugin {
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
            summary: "reusable instructions you write, loaded on demand".to_string(),
            you_get: format!(
                "1 tool ({TOOL_NAME}) and a narrowed skill index in context -- full skill \
                 bodies from .conway/skills load on demand instead of every turn"
            ),
            you_lose: "skills stay fully expanded in context on every turn instead (uses more \
                       of the context window as skills accumulate)"
                .to_string(),
            costs: format!("none beyond the {TOOL_NAME} calls the model makes"),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(ReadSkillTool {
            skills: self.skills.clone(),
        })]
    }

    fn context_hooks(&self) -> Vec<Arc<dyn ContextHook>> {
        vec![self.hook()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: Option<&str>, body: &str) -> SkillDef {
        SkillDef {
            name: name.to_string(),
            description: description.map(|s| s.to_string()),
            body: body.to_string(),
            always_include: false,
        }
    }

    fn map_of(skills: &[SkillDef]) -> Arc<HashMap<String, SkillDef>> {
        let mut m = HashMap::new();
        for s in skills {
            m.insert(s.name.clone(), s.clone());
        }
        Arc::new(m)
    }

    // -----------------------------------------------------------------
    // Cookbook example 4 verdict 2 (re-run as a real unit test on the
    // plain lookup function, exactly as the cookbook's own
    // `read_skill_looks_up_the_full_body_by_name` does): `find_skill`
    // resolves a known name and refuses an unknown one.
    // -----------------------------------------------------------------
    #[test]
    fn read_skill_looks_up_the_full_body_by_name() {
        let git_commit = skill(
            "git-commit",
            Some("How to write a good commit message."),
            "## git-commit\n\nBody text.",
        );
        let skills = map_of(std::slice::from_ref(&git_commit));
        let found = find_skill(&skills, "git-commit").expect("git-commit is in the table");
        assert_eq!(found.body, git_commit.body);
        assert!(find_skill(&skills, "does-not-exist").is_none());
    }

    // -----------------------------------------------------------------
    // Cookbook example 4 verdict 1 (re-run here on the hook in isolation,
    // MATCHING the cookbook's own `a_full_skill_body_is_narrowed_to_a_
    // one_line_index_entry`; the END-TO-END re-run through the real
    // Runtime lives in `tests/skills_e2e.rs`).
    // -----------------------------------------------------------------
    #[tokio::test]
    async fn a_full_skill_body_is_narrowed_to_a_one_line_index_entry() {
        let body = "## git-commit\n\nA long, multi-paragraph skill body that is \
                    substantially larger than the one-line index entry it is narrowed \
                    to, so the token-savings assertion is meaningful.";
        let git_commit = skill(
            "git-commit",
            Some("How to write a good commit message."),
            body,
        );
        let skills = map_of(&[git_commit]);
        let hook = SkillIndexHook { skills };

        // Build a payload with one full-body `Provenance::Skill` segment,
        // the shape `ContextBuilder` assembles for a configured skill.
        let segment = prompt_segment("git-commit", body);
        let payload = ContextPayload {
            segments: vec![segment],
            tools: vec![],
        };

        let ctx = hook_ctx();
        let out = hook.before_request(&ctx, payload).await;
        let ContentBlock::Text { text } = &out.segments[0].content[0] else {
            panic!("narrowed segment should be a single Text block");
        };
        assert!(
            text.len() < body.len(),
            "the index entry must be shorter than the full body"
        );
        assert!(
            text.contains("read_skill(name=\"git-commit\")"),
            "the index entry must point at read_skill for this name: {text}"
        );
        assert!(
            !text.contains("multi-paragraph"),
            "the full body must NOT survive into the index entry: {text}"
        );
    }

    /// A skill name the plugin's own table does not know about is left
    /// COMPLETELY UNCHANGED (cookbook example 4's
    /// `an_unindexed_skill_segment_is_left_alone`).
    #[tokio::test]
    async fn an_unindexed_skill_segment_is_left_alone() {
        let skills = map_of(&[skill("git-commit", Some("d"), "body")]);
        let hook = SkillIndexHook { skills };
        let original = "the full body of a skill this plugin does not index";
        let segment = prompt_segment("unknown-skill", original);
        let payload = ContextPayload {
            segments: vec![segment],
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(), payload).await;
        let ContentBlock::Text { text } = &out.segments[0].content[0] else {
            panic!()
        };
        assert_eq!(
            text, original,
            "an unindexed skill segment must be left byte-for-byte unchanged"
        );
    }

    /// A skill with no description narrows to the description-less index
    /// form (no empty `name: ` prefix), proving the `Option<String>`
    /// description path does not produce a malformed line.
    #[tokio::test]
    async fn a_skill_with_no_description_narrows_without_an_empty_prefix() {
        let skills = map_of(&[skill("noisy", None, "a body longer than the index line")]);
        let hook = SkillIndexHook { skills };
        let out = hook
            .before_request(
                &hook_ctx(),
                ContextPayload {
                    segments: vec![prompt_segment("noisy", "a body longer than the index line")],
                    tools: vec![],
                },
            )
            .await;
        let ContentBlock::Text { text } = &out.segments[0].content[0] else {
            panic!()
        };
        assert!(
            !text.starts_with("noisy: "),
            "no description must not produce an empty `name: ` prefix: {text}"
        );
        assert!(text.contains("read_skill(name=\"noisy\")"));
    }

    /// The plugin's manifest id matches the published constant a config
    /// author resolves `[plugins].install` against.
    #[test]
    fn manifest_id_matches_the_published_constant() {
        let plugin = SkillsPlugin::new(map_of(&[]));
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
    }

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default.
    #[test]
    fn description_is_non_empty() {
        let plugin = SkillsPlugin::new(map_of(&[]));
        let description = plugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
    }

    /// Installing via `with_plugin` announces `read_skill` AND contributes
    /// the hook through the SAME surface -- the packaging half of the
    /// acceptance, asserted at the trait level (the END-TO-END install +
    /// hook-fires proof lives in `tests/skills_e2e.rs`).
    #[test]
    fn plugin_registers_one_tool_and_one_context_hook() {
        let plugin = SkillsPlugin::new(map_of(&[skill("git-commit", Some("d"), "b")]));
        let tools = plugin.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].spec().name.as_str(), TOOL_NAME);
        let hooks = plugin.context_hooks();
        assert_eq!(
            hooks.len(),
            1,
            "the plugin contributes exactly one context hook"
        );
        assert_eq!(plugin.manifest().tools, vec![ToolName::new(TOOL_NAME)]);
    }

    // ---- helpers -------------------------------------------------------

    /// Builds a `Provenance::Skill { name }` `PromptSegment` carrying
    /// `body` as its content -- the shape `ContextBuilder` assembles for a
    /// configured skill, replicated here so the hook tests do not depend on
    /// the runtime crate.
    fn prompt_segment(name: &str, body: &str) -> conway::plugin::PromptSegment {
        conway::plugin::PromptSegment::new(
            conway::plugin::Role::System,
            vec![ContentBlock::Text {
                text: body.to_string(),
            }],
            Provenance::Skill {
                name: name.to_string(),
            },
        )
    }

    fn hook_ctx() -> ContextHookCtx {
        let agent_id = conway::AgentId::new();
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
}
