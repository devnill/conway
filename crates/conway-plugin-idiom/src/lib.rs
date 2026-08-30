//! `conway.idiom`: a plugin that prepends a short, conway-specific
//! instruction fragment to a session (board item
//! `01M0VR3BKW5N3V3WS28H7FV8ZK`). The operator's own framing: *"this is a
//! plugin which prepends a custom system prompt. Currently we send minimal
//! data, and the purpose of this is to add a little extra if desired."*
//! **A little extra** -- see [`FRAGMENT_TEXT`]'s own line/word budget
//! before adding to it.
//!
//! # The absence this closes, re-verified (not assumed)
//!
//! `App::session_spec` (`crates/conway-cli/src/tui/app/startup.rs`) sets
//! `role`/`keep_alive`/`tools`/`model` on its `SessionSpec` and never sets
//! `agent_def` or `system_prompt_override`. `SessionSpec::
//! system_prompt_override`'s own doc (`crates/conway/src/session_handle.rs`)
//! states the consequence in as many words: `None` "preserves the
//! pre-existing behavior: the resolved `agent_def`'s own `system_prompt`,
//! or **no system-prompt segment at all** when `agent_def` is also `None`."
//! A bare interactive TUI session sets neither, so `ContextBuilder::build`'s
//! `[0] SystemPrompt` step (`crates/conway-runtime/src/context/builder.rs`)
//! pushes nothing: `input.system_prompt` is `None`, the `if let Some(...)`
//! guard never fires, and the assembled context carries tool schemas and
//! the conversation, with no segment telling the model what harness it is
//! in. Confirmed by reading both sites directly rather than trusted from
//! the board item's own citation; the premise holds.
//!
//! # Determine first #1: what "prepend" means in the assembly order
//!
//! `ContextBuilder::build` assembles `[0] SystemPrompt` (an agent def's own
//! prompt, or a session's `system_prompt_override`), then `[1]
//! PluginInstructions*` (every installed plugin's own
//! `Plugin::instructions()` fragments, in `with_plugin`/`install_selected`
//! order), then `[1b] SkillFragments*`, then tool schemas and the
//! conversation. This plugin's one fragment lands in `[1]`.
//!
//! **That is where "prepend" lands, and it is the right place.** Against
//! the conversation -- the actual "instructions vs. transcript" boundary an
//! operator means by "prepend" -- `[1]` is unambiguously at the front: it
//! precedes every tool schema and every logged turn. It lands AFTER `[0]`
//! only when an `agent_def` supplies its own system prompt, which is
//! exactly the ordering an operator installing this plugin alongside a
//! curated agent def would want: the agent def's own, deliberately-authored
//! prompt (the specific job this agent was built for) stays the FIRST thing
//! the model reads, and conway's own harness orientation follows immediately
//! after, before anything else. Reversing that -- putting a generic harness
//! paragraph ahead of an agent def's own carefully-ordered prompt -- would
//! be surprising for exactly the operator who bothered to author one. For
//! the plugin's own primary case (the bare interactive TUI session this
//! item's premise section re-verifies has NO `[0]` segment at all), `[1]`
//! is not merely "after `[0]`" in the abstract -- it IS the front of the
//! whole assembled context, because there is nothing at `[0]` to be after.
//! Multiple plugins' fragments are ordered by `with_plugin`/
//! `install_selected` install order, so "first among plugin fragments" is
//! not a property this plugin can guarantee for itself -- see
//! `first_party_plugins::bundle`'s own doc for where this entry sits in
//! that list.
//!
//! **This item does not change `ContextBuilder::build`'s assembly order.**
//! If an operator's expectation of "prepend" turns out to mean "ahead of an
//! agent def's own prompt too", that is a runtime change affecting every
//! consumer of the context builder, out of this item's scope -- filed as a
//! follow-up in this crate's own completion report, not built here.
//!
//! # Determine first #2: the `tool_ids` trap
//!
//! [`IdiomPlugin::instructions`] declares `tool_ids: vec![]` -- empty,
//! deliberately. `ContextBuilder::build`'s reachability check withholds a
//! fragment's text ENTIRELY (not just the offending line) when any id in
//! `tool_ids` is not among the turn's announced tools; a session that
//! excludes even one named tool would silently lose the whole paragraph.
//! [`FRAGMENT_TEXT`] names `conway_fork`/`conway_spawn`/`report` in prose
//! (explaining what they are, not claiming this session has them), but
//! nothing in it requires the model to be ABLE to call any specific one for
//! the rest of the text to still be true and useful -- "the tool set is
//! configuration-dependent" and "context is scarce" hold regardless of
//! whether fork/spawn/report happen to be announced this turn, and an
//! interactive root SPECIFICALLY never has `report` (`startup.rs`'s own
//! `ToolSelector::Except(vec!["report".into()])`) -- naming `report` in
//! `tool_ids` would make the fragment vanish from the one session type this
//! item's premise section is about. This is orientation text about the
//! harness's idioms, not a usage note for one tool; withholding it entirely
//! because a session lacks any single tool it happens to mention would be
//! the wrong failure mode for that kind of content.
//!
//! # Reach: every agent, root or child (board item `01M0VSKA76NSEHDSH25XJGJ2J5`)
//!
//! At the time this plugin first shipped, `SubagentHost::start`
//! (`crates/conway-runtime/src/subagent.rs`) gave every forked or spawned
//! child `instructions: Vec::new()` unconditionally, and its sibling
//! `resolve_instructions` (`crates/conway-runtime/src/runtime/root.rs`) --
//! the function that forwards every installed plugin's fragments UNCHANGED
//! -- was root-only. That was disclosed as a caveat (`docs/plugins/
//! hooks.md` point 17: "If you author a fragment, assume a subagent will
//! not see it") but never *decided*: nobody had argued whether a child
//! SHOULD see it.
//!
//! Board item `01M0VSKA76NSEHDSH25XJGJ2J5` argued it and ruled a plugin
//! instruction fragment is harness configuration keyed to tool
//! reachability (the existing `tool_ids` gate), not transcript context --
//! so fork/spawn's "whole transcript vs. empty transcript" split does not
//! govern it, the same way it already does not govern `plugin_config`
//! (narrowed-and-inherited from the parent for spawn exactly as for fork,
//! predating this item). `SubagentHost::start` now calls the SAME
//! `resolve_instructions`/`resolve_skills` a root agent does, for both
//! fork and spawn, with no per-mode branch -- see that function's own doc
//! (`runtime/root.rs`) for the full argument. [`FRAGMENT_TEXT`] was written
//! knowing its "ending a turn"/"permissions"/"steering" bullets describe
//! how a *child* agent should behave; those agents now receive it, filtered
//! per turn by [`IdiomPlugin::instructions`]'s empty `tool_ids` (always
//! reachable) exactly as for a root.
//!
//! # Naming
//!
//! `conway.idiom` -- the exact id `conway_core::ports::plugin::Plugin::
//! instructions`'s own doc and `ContextBuilder::build`'s own "Precedence"
//! comment already use as their illustrative example of a base,
//! plugin-sourced fragment (`conway.idiom` "base" -> `conway.trim`/
//! `conway.memory` plugin-sourced -> `house-style` "(yours)"), so this
//! plugin fills a name the codebase's own comments already anticipated
//! rather than inventing a new one.
//!
//! # Operator instructions (board item `01M0VR4GMGSZ2682T908JCGVFG`)
//!
//! An operator has no other lever to add standing instructions to every
//! session -- `--system-prompt`/`--append-system-prompt`
//! (`crates/conway-cli/src/cli.rs`) reach `SessionSpec::
//! system_prompt_override` only on the one-shot path, and that field
//! REPLACES the whole `[0] SystemPrompt` segment rather than adding to it
//! (`crates/conway/src/session_handle.rs`'s own doc). This module gives an
//! interactive operator a file instead, following Pi's `AGENTS.md`/
//! `SYSTEM.md` precedent while staying additive: it contributes MORE
//! `InstructionFragment`s alongside [`FRAGMENT_TEXT`], never replacing
//! `system_prompt_override`, which stays the flag's job (report it here so
//! the next reader does not invent a second "replace" answer).
//!
//! **1. One file, not a search path.** `.conway/instructions.md`, matching
//! `.conway/agents/`/`.conway/skills/` -- both already resolved directly
//! against `cwd`, never walked up an ancestor chain
//! (`crates/conway-cli/src/first_party_plugins.rs`'s own `bundle`, `cwd.
//! join(".conway").join("skills")`). Pi merges several directories because
//! it walks from a deeply nested cwd up through a monorepo; conway's own
//! project-file convention never does that for `.conway/*`, so a search
//! path would be new shape for this plugin alone, not a precedent it is
//! following. No concrete case named here needs more than one project file
//! -- an operator who wants to say two different things says them in one
//! file.
//!
//! **2. Project AND global, both additive.** conway's config discovery
//! already resolves a project layer (`conway::config::discovery::discover`,
//! upward-walking for `settings.json` specifically) and a user layer
//! (`conway::config::discovery::user_config_path`). Reuse is cheap for the
//! GLOBAL half: [`global_instructions_path`] below is one call to
//! `user_config_path` plus a filename swap, no new dependency, no schema
//! change. **It honours `CONWAY_CONFIG_DIR` exactly the way
//! `settings.json` itself does** (board item
//! `01M0W5Q569F0T97HSEP6F0MPCR`, closing the same isolation gap board item
//! `01M0VV6CVSZM4XH8J4G6EBV5E3` closed for `settings.json` -- an operator
//! or embedder relocating conway's user-config layer relocates THIS file
//! with it, not only `settings.json`) -- so `global_instructions_path`
//! takes the same explicit `env: &HashMap<String, String>` every other
//! `CONWAY_CONFIG_DIR`-aware resolver in this codebase takes, threaded
//! from `first_party_plugins::install`/`all_bundle_plugins`/
//! `installed_plugins` (`crates/conway-cli/src/first_party_plugins.rs`)
//! down through `resolve_idiom_plugin`/`resolve_operator_paths` to here --
//! never read from `std::env` directly at any point in that chain (see
//! `config_isolation_guard.rs`'s own doc for why an ambient read anywhere
//! along it would be the identical defect wearing a new file's name). The
//! PROJECT half does not reuse `discover` at all -- that function's
//! candidate is hardcoded to `settings.json` (`crates/conway/src/config/
//! discovery.rs`), not a generic file-discovery primitive, so "reusing"
//! it here would mean requiring a `settings.json` to already exist beside
//! `instructions.md` for the latter to be found, a strictly worse answer
//! than the direct `cwd`-join every other `.conway/*` convention already
//! uses. Both files are read when present; neither one's presence
//! disables the other, matching the additive shape `system_prompt_override`
//! deliberately does NOT have.
//!
//! **3. Two operator fragments, not merged into the base.** The shipped
//! [`FRAGMENT_TEXT`] and an operator's own text have different authors and
//! different lifetimes (one ships with this crate, one is the operator's
//! own file, editable with no rebuild) -- `/context` already renders
//! per-fragment token costs, so keeping them apart is what lets an
//! operator see what THEIR OWN text costs, separately from this plugin's.
//! The same argument extends one level further, from "shipped vs.
//! operator" to "project vs. global": both are operator-authored, but a
//! project file changes at a different rate than a global one (per-repo
//! conventions vs. house-wide preference an operator carries everywhere),
//! and an operator who has authored both wants to see both costs, not one
//! opaque combined number. [`OPERATOR_PROJECT_INSTRUCTION_NAME`]/
//! [`OPERATOR_GLOBAL_INSTRUCTION_NAME`] are therefore two more names
//! alongside [`INSTRUCTION_NAME`], each optional, each independently
//! absent when its file is absent. When `cwd` genuinely IS the operator's
//! home directory, the two paths name the same underlying file --
//! [`resolve_operator_paths`] collapses that case to the project fragment
//! alone rather than injecting the same text twice under two names.
//!
//! **4. Missing is silent; unreadable or malformed is not.** A
//! `NotFound` read error yields no fragment and no `Result::Err` -- an
//! operator who never wrote either file sees conway behave exactly as
//! before this item. An empty (or whitespace-only) file is treated the
//! same way: it communicates nothing, same as absent. Any OTHER read
//! failure -- a permissions error, the path naming a directory, invalid
//! UTF-8 -- is `Err(FacadeError::Config { .. })`, surfaced at
//! `ConwayBuilder::build` (the same failure tier a malformed `.conway/
//! skills/*/SKILL.md` already fails at, `crate::skills::load_skill_defs`),
//! never silently dropped: a file the operator wrote and conway silently
//! ignored is exactly the failure mode this project cares most about.
//!
//! # The provenance limitation, restated for the operator-file case
//!
//! Every fragment this plugin contributes -- the shipped base AND now an
//! operator's own project/global text -- is stamped `Provenance::Skill {
//! name }` once assembled (`crates/conway-runtime/src/context/builder.rs`),
//! the SAME stamp an operator-authored `.conway/skills` body gets. Plugin
//! attribution lives only in the parallel `ContextReport::
//! instruction_fragments` list, a side-channel, not durable provenance. So
//! text an operator wrote themselves, in a file this plugin merely reads,
//! is attributed in the durable log to "a skill" and in `/context`'s
//! report to `conway.idiom` -- wrong in both directions, and now true for
//! operator-authored content specifically, not only for this plugin's own
//! shipped paragraph. Not fixed here: a `Provenance::Operator` variant is
//! a persisted wire-format change (precedent: `Provenance::CommandPrompt`,
//! added the same day for a different feature) and is its own decision,
//! filed rather than built as part of this item.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway::plugin::{InstructionFragment, Plugin, PluginDescription, PluginManifest, Tool};

/// This plugin's published manifest id -- a config author (or a first-party
/// bundle's own linking module) resolves `[plugins].install` entries
/// against this constant.
pub const PLUGIN_ID: &str = "conway.idiom";

/// The bare name of this plugin's one shipped [`InstructionFragment`].
pub const INSTRUCTION_NAME: &str = "conway.idiom.base";

/// The name of the operator's project-scope instruction fragment, sourced
/// from `<cwd>/.conway/instructions.md` when that file exists, is
/// readable, and is non-empty. See this module's own doc, "Operator
/// instructions", point 3.
pub const OPERATOR_PROJECT_INSTRUCTION_NAME: &str = "conway.idiom.operator.project";

/// The name of the operator's global-scope instruction fragment, sourced
/// from `<home>/.conway/instructions.md`. See this module's own doc,
/// "Operator instructions", points 2 and 3.
pub const OPERATOR_GLOBAL_INSTRUCTION_NAME: &str = "conway.idiom.operator.global";

/// The bare filename an operator writes their own instructions into, at
/// either scope -- `instructions.md` alongside `.conway/agents/`/
/// `.conway/skills/`'s own convention of a `.conway/`-relative name.
pub const OPERATOR_INSTRUCTIONS_FILENAME: &str = "instructions.md";

/// The fragment's text, sourced from a markdown file in this crate's own
/// `fragments/` directory (`crates/conway-plugin-path`/
/// `crates/conway-plugin-discover`'s own `include_str!` convention --
/// `Plugin::instructions`'s own doc, "Convention, not enforcement"). 28
/// lines, well under a 40-line/400-word budget measured from Pi's own
/// `system-prompt.ts` core template (`docs/vision/INTENT.md`'s citation of
/// Pi as conway's extension-surface reference). See this module's own
/// doc, "Reach: every agent, root or child", for who actually reads this
/// text.
pub const FRAGMENT_TEXT: &str = include_str!("../fragments/idiom.md");

/// The `conway.idiom` plugin: contributes no tool, one shipped instruction
/// fragment -- conway's own short idioms primer, prepended near the front
/// of a root session's assembled context (see this crate's own module doc
/// for the full "prepend"/`tool_ids` argument) -- plus up to two more,
/// optional fragments sourced from an operator's own `instructions.md`
/// (see this module's own doc, "Operator instructions").
///
/// `Default`/[`IdiomPlugin::new`] carry no operator fragments at all --
/// the shape every existing caller of bare `IdiomPlugin` already gets.
/// [`IdiomPlugin::from_operator_files`] is the constructor that actually
/// reads an operator's files; `first_party_plugins::bundle` is this
/// binary's one production call site.
#[derive(Default)]
pub struct IdiomPlugin {
    operator_project: Option<InstructionFragment>,
    operator_global: Option<InstructionFragment>,
}

impl IdiomPlugin {
    /// No operator fragments -- equivalent to `Default::default()`, spelled
    /// out for a caller (or test) that wants a plain constructor rather
    /// than a trait method.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads `project_path`/`global_path` (each `None` when that scope has
    /// no resolvable location at all -- see [`resolve_operator_paths`]) into
    /// this plugin's two optional operator fragments. Point 4 of this
    /// module's own "Operator instructions" doc for the missing/empty/
    /// unreadable/malformed policy this implements.
    pub fn from_operator_files(
        project_path: Option<&Path>,
        global_path: Option<&Path>,
    ) -> conway::Result<Self> {
        let operator_project =
            read_operator_fragment(project_path, OPERATOR_PROJECT_INSTRUCTION_NAME)?;
        let operator_global =
            read_operator_fragment(global_path, OPERATOR_GLOBAL_INSTRUCTION_NAME)?;
        Ok(Self {
            operator_project,
            operator_global,
        })
    }
}

/// The default project-scope operator file: `<cwd>/.conway/instructions.md`
/// -- never walked up an ancestor chain, matching `.conway/agents/`/
/// `.conway/skills/`'s own direct-`cwd`-join convention (this module's own
/// doc, "Operator instructions", point 1).
pub fn project_instructions_path(cwd: &Path) -> PathBuf {
    cwd.join(".conway").join(OPERATOR_INSTRUCTIONS_FILENAME)
}

/// The default global-scope operator file: alongside conway's user-scoped
/// `settings.json` (`conway::config::discovery::user_config_path`'s own
/// directory) -- `None` under the exact condition that function returns
/// `None` (no home directory discoverable on this platform/environment,
/// and `CONWAY_CONFIG_DIR` unset or empty in `env`). **Honours
/// `CONWAY_CONFIG_DIR`** (board item `01M0W5Q569F0T97HSEP6F0MPCR`): unlike
/// the raw, override-independent `home_settings_path`, `user_config_path`
/// relocates to `$CONWAY_CONFIG_DIR/settings.json` whenever that variable
/// is set and non-empty in `env`, and this function follows it there --
/// see this module's own doc, "Operator instructions", point 2, for why
/// that parity with `settings.json` matters.
pub fn global_instructions_path(
    env: &std::collections::HashMap<String, String>,
) -> Option<PathBuf> {
    conway::config::discovery::user_config_path(env)
        .and_then(|settings| settings.parent().map(Path::to_path_buf))
        .map(|dir| dir.join(OPERATOR_INSTRUCTIONS_FILENAME))
}

/// Resolves both operator-file locations for `cwd`, collapsing the global
/// path to `None` when it names the SAME underlying file as the project
/// path (an operator whose project genuinely lives at `$HOME`) -- see this
/// module's own doc, "Operator instructions", point 3, for why that case
/// must not inject the same text twice under two fragment names.
///
/// `env` is the same explicit map every `CONWAY_CONFIG_DIR`-aware resolver
/// in this codebase takes -- forwarded to [`global_instructions_path`],
/// never read from `std::env` here or anywhere downstream of this call.
pub fn resolve_operator_paths(
    cwd: &Path,
    env: &std::collections::HashMap<String, String>,
) -> (PathBuf, Option<PathBuf>) {
    let project = project_instructions_path(cwd);
    let global = collapse_global_onto_project(project.clone(), global_instructions_path(env));
    (project, global)
}

/// The collapse rule itself, pulled out as a pure function so it is
/// testable with two arbitrary paths rather than requiring a real,
/// process-global home directory to coincide with a test's own `cwd`
/// (which would not be test-isolated). `None` in, `None` out; `Some`
/// naming the same underlying file as `project` collapses to `None`;
/// anything else passes through unchanged.
fn collapse_global_onto_project(project: PathBuf, global: Option<PathBuf>) -> Option<PathBuf> {
    match global {
        Some(candidate) if same_file_lenient(&candidate, &project) => None,
        other => other,
    }
}

/// Whether `a` and `b` name the same underlying file -- canonicalizes both
/// sides when possible, falling back to a lexical comparison when either
/// side does not exist (the ordinary case for a file an operator has not
/// written yet), mirroring `conway::config::discovery`'s own
/// `same_settings_file` comparison one level up rather than re-exporting
/// it (that helper is private to its own module).
fn same_file_lenient(a: &Path, b: &Path) -> bool {
    fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| normalize(p));
    resolve(a) == resolve(b)
}

/// Reads one operator instruction file at `path` into a named
/// [`InstructionFragment`], implementing this module's own "Operator
/// instructions" point 4:
///
/// - `path` is `None` (no resolvable location for this scope): `Ok(None)`,
///   silently -- there is nothing to read.
/// - The file does not exist (`io::ErrorKind::NotFound`): `Ok(None)`,
///   silently (P-13: absence is the normal, unremarkable case).
/// - The file exists and is whitespace-only: `Ok(None)` -- an empty file
///   communicates nothing, the same as an absent one.
/// - The file exists, has content, and reads cleanly: `Ok(Some(fragment))`.
/// - Any OTHER read failure (permission denied, the path names a
///   directory, the bytes are not valid UTF-8): `Err(FacadeError::Config)`,
///   naming `path` and the underlying error -- surfaced at
///   `ConwayBuilder::build`, never dropped. A file the operator wrote and
///   conway silently ignored is the failure mode P-13 exists to prevent.
fn read_operator_fragment(
    path: Option<&Path>,
    name: &str,
) -> conway::Result<Option<InstructionFragment>> {
    let Some(path) = path else {
        return Ok(None);
    };
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if text.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(InstructionFragment {
                    name: name.to_string(),
                    text,
                    // Empty, deliberately, exactly like the shipped
                    // fragment's own `tool_ids` -- an operator's own prose
                    // is not tied to any specific tool being reachable
                    // this turn.
                    tool_ids: vec![],
                }))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(conway::FacadeError::Config {
            path: Some(path.to_path_buf()),
            message: format!(
                "could not read operator instructions file {}: {err}",
                path.display()
            ),
        }),
    }
}

impl Plugin for IdiomPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "prepends a short conway-idioms primer to a session's system-prompt \
                       segment, plus an operator's own project/global instructions.md when \
                       either exists"
                .to_string(),
            you_get: "one shipped instruction fragment (fork vs. spawn, how an agent ends, \
                      configuration-dependent tools, context scarcity, permissions, budgets, \
                      steering) injected near the front of the assembled context, ahead of the \
                      tool schemas and the conversation -- and ahead of the whole context when \
                      no agent def supplies its own system prompt, which is the ordinary \
                      interactive-TUI case this plugin exists for. Reaches every forked or \
                      spawned child too, not the root alone (board item \
                      01M0VSKA76NSEHDSH25XJGJ2J5's ruling: an instruction fragment is harness \
                      configuration, not transcript context, so fork/spawn's inheritance split \
                      does not govern it) -- the ending/permissions/steering bullets it carries \
                      describe how a *child* agent should behave, and now reach exactly that \
                      agent. Also up to two more fragments, additive alongside the shipped one, \
                      read from an operator's own `.conway/instructions.md` (project scope) and \
                      `<home>/.conway/instructions.md` (global scope) when either file exists -- \
                      reaching a forked/spawned child exactly the same way, for the same reason"
                .to_string(),
            you_lose: "nothing else".to_string(),
            costs: "one system-prompt segment's worth of tokens per turn for the shipped \
                    fragment (roughly 275 words), plus whatever an operator's own \
                    instructions.md file(s) cost -- /context's preamble section names \
                    conway.idiom.base, conway.idiom.operator.project, and \
                    conway.idiom.operator.global separately, each with its own exact token cost"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn instructions(&self) -> Vec<InstructionFragment> {
        let mut fragments = vec![InstructionFragment {
            name: INSTRUCTION_NAME.to_string(),
            text: FRAGMENT_TEXT.to_string(),
            // Empty, deliberately -- see this module's own doc, "Determine
            // first #2: the `tool_ids` trap".
            tool_ids: vec![],
        }];
        fragments.extend(self.operator_project.clone());
        fragments.extend(self.operator_global.clone());
        fragments
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default.
    #[test]
    fn description_is_non_empty() {
        let description = IdiomPlugin::new().description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
        assert!(!description.costs.is_empty());
    }

    /// Board item `01M0VSKA76NSEHDSH25XJGJ2J5` ruled a plugin instruction
    /// fragment reaches a forked/spawned child too, not the root alone --
    /// `you_get` (not `you_lose`, now that this is a capability rather than
    /// a limitation) must say so verbatim, not merely in this crate's own
    /// doc comment -- GP-14: a declaration site is one artifact, and an
    /// operator deciding whether to install this plugin reads the
    /// description, not the source.
    #[test]
    fn you_get_names_the_subagent_reach() {
        let description = IdiomPlugin::new().description();
        assert!(
            description.you_get.to_lowercase().contains("subagent")
                || description.you_get.to_lowercase().contains("child"),
            "you_get must state that the fragment reaches a forked/spawned child: {:?}",
            description.you_get
        );
    }

    /// Exactly one fragment, contributing no tool -- `manifest().tools` is
    /// empty, so the reachability check's "same plugin also provides the
    /// tool" shortcut never applies here; every id in `tool_ids` (there are
    /// none) would have to be reachable through a DIFFERENT installed
    /// plugin.
    #[test]
    fn contributes_exactly_one_fragment_and_no_tool() {
        let plugin = IdiomPlugin::new();
        assert!(plugin.tools().is_empty());
        assert!(plugin.manifest().tools.is_empty());
        let instructions = plugin.instructions();
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].name, INSTRUCTION_NAME);
        assert!(instructions[0].tool_ids.is_empty());
    }

    // NOTE: there was a `tool_ids_are_trivially_always_reachable` test here.
    // It was removed rather than repaired. It built an empty `known_tool_ids`
    // set, filtered `instructions[0].tool_ids` against it, and asserted the
    // result was empty -- but `tool_ids` is empty by construction, so
    // filtering it can never yield anything regardless of the set's contents.
    // The assertion was true by construction and could not fail for any future
    // change to this plugin. `contributes_exactly_one_fragment_and_no_tool`
    // above already asserts `tool_ids.is_empty()` directly, and the REAL
    // reachability property -- that the fragment survives an actual
    // `ContextBuilder::build` pass -- is proven by
    // `fragment_reaches_a_bare_sessions_wire_request` in
    // `tests/idiom_end_to_end.rs`, which drives production code rather than
    // reimplementing the filter shape locally.

    /// Budget pin (acceptance 3): fails loudly if a future edit grows the
    /// fragment past the item's stated 40-line/400-word cap, rather than
    /// letting the cap drift unnoticed.
    #[test]
    fn fragment_stays_within_budget() {
        let line_count = FRAGMENT_TEXT.lines().count();
        let word_count = FRAGMENT_TEXT.split_whitespace().count();
        assert!(
            line_count <= 40,
            "fragment grew to {line_count} lines, past the 40-line budget"
        );
        assert!(
            word_count <= 400,
            "fragment grew to {word_count} words, past the 400-word budget"
        );
    }
}

/// Unit coverage for the operator-file reading/resolution logic itself,
/// in isolation from a real `Conway` build -- `tests/idiom_end_to_end.rs`
/// is the end-to-end proof the resulting fragments reach a real wire
/// request (acceptance 1); this module is P-15's "shown to fail" proof
/// for the missing/empty/unreadable/malformed policy (acceptance 2).
#[cfg(test)]
mod operator_file_tests {
    use super::*;

    /// P-13: a file the operator never wrote must not be an error, and
    /// must not contribute a fragment either -- silent and normal.
    #[test]
    fn missing_file_is_silent_and_contributes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("does-not-exist.md");
        let plugin = IdiomPlugin::from_operator_files(Some(&path), None)
            .expect("a missing file must not be an error");
        assert_eq!(plugin.instructions().len(), 1, "base fragment only");
    }

    /// An empty (or whitespace-only) file communicates nothing, same as an
    /// absent one -- not an error, and no fragment.
    #[test]
    fn empty_file_is_silent_and_contributes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(&path, "   \n\n\t\n").expect("write");
        let plugin = IdiomPlugin::from_operator_files(Some(&path), None)
            .expect("a whitespace-only file must not be an error");
        assert_eq!(plugin.instructions().len(), 1, "base fragment only");
    }

    /// The positive path: a real, non-empty file becomes a real fragment,
    /// named and reachable, its exact text intact.
    #[test]
    fn present_file_becomes_a_named_reachable_fragment() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(&path, "Always run `cargo test` before reporting done.\n").expect("write");
        let plugin = IdiomPlugin::from_operator_files(Some(&path), None).expect("read ok");
        let instructions = plugin.instructions();
        assert_eq!(instructions.len(), 2);
        let operator = instructions
            .iter()
            .find(|f| f.name == OPERATOR_PROJECT_INSTRUCTION_NAME)
            .expect("operator project fragment present");
        assert!(operator.text.contains("Always run `cargo test`"));
        assert!(operator.tool_ids.is_empty());
    }

    /// Acceptance 2, the negative half, and P-15's "shown to fail" bar:
    /// a file that exists but cannot be read as UTF-8 text (a directory,
    /// standing in for "the read fails for a reason other than absence")
    /// must surface as an `Err`, never be silently dropped. Falsified by
    /// temporarily deleting the `Err(err) => Err(...)` arm's guard (i.e.
    /// treating every read failure as `Ok(None)`, the fail-open shape this
    /// test exists to reject) -- confirmed to fail before restoring it.
    #[test]
    fn unreadable_file_surfaces_as_an_error_not_a_silent_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A directory named `instructions.md`: `fs::read_to_string` fails
        // with `ErrorKind::IsADirectory` (or an OS-specific equivalent),
        // never `NotFound` -- so this exercises the exact "exists but
        // cannot be read" branch this test is named for, without relying
        // on a platform-specific permissions setup.
        let path = tmp.path().join("instructions.md");
        std::fs::create_dir(&path).expect("mkdir");
        let result = IdiomPlugin::from_operator_files(Some(&path), None);
        assert!(
            result.is_err(),
            "an unreadable operator file must surface as an error, not be silently dropped"
        );
    }

    /// The malformed case: bytes that are not valid UTF-8 must surface too,
    /// on the same footing as the directory case above -- `parse_skill_def`'s
    /// own precedent (`crate::skills::load_skill_defs` fails loudly on a
    /// malformed `SKILL.md`) is the shape this mirrors.
    #[test]
    fn malformed_utf8_file_surfaces_as_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0xff]).expect("write invalid utf-8");
        let result = IdiomPlugin::from_operator_files(Some(&path), None);
        assert!(
            result.is_err(),
            "invalid UTF-8 in an operator file must surface as an error"
        );
    }

    /// `None` for a scope (no resolvable location -- e.g. no home directory
    /// discoverable) is treated exactly like "file absent": silent, no
    /// fragment, no error.
    #[test]
    fn none_path_is_silent_and_contributes_nothing() {
        let plugin = IdiomPlugin::from_operator_files(None, None).expect("ok");
        assert_eq!(plugin.instructions().len(), 1);
    }

    /// Both scopes present at once: two additional, independently named
    /// fragments, neither one displacing the other.
    #[test]
    fn project_and_global_both_present_yield_two_independent_fragments() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("project-instructions.md");
        let global = tmp.path().join("global-instructions.md");
        std::fs::write(&project, "Project-specific convention.\n").expect("write");
        std::fs::write(&global, "House-wide preference.\n").expect("write");
        let plugin =
            IdiomPlugin::from_operator_files(Some(&project), Some(&global)).expect("read ok");
        let instructions = plugin.instructions();
        assert_eq!(instructions.len(), 3);
        assert!(instructions
            .iter()
            .any(|f| f.name == OPERATOR_PROJECT_INSTRUCTION_NAME
                && f.text.contains("Project-specific")));
        assert!(instructions
            .iter()
            .any(|f| f.name == OPERATOR_GLOBAL_INSTRUCTION_NAME && f.text.contains("House-wide")));
    }

    /// [`resolve_operator_paths`]: an ordinary project/global split names
    /// two distinct paths -- driven through an isolated `CONWAY_CONFIG_DIR`
    /// (never a real, ambient home directory: this crate's own tests must
    /// stay parallel-safe and must never touch the invoking user's real
    /// `$HOME`).
    #[test]
    fn resolve_operator_paths_names_two_distinct_paths_in_the_ordinary_case() {
        let project_tmp = tempfile::tempdir().expect("tempdir");
        let global_tmp = tempfile::tempdir().expect("tempdir");
        let mut env = std::collections::HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            global_tmp.path().display().to_string(),
        );
        let (project, global) = resolve_operator_paths(project_tmp.path(), &env);
        assert_eq!(
            project,
            project_tmp.path().join(".conway").join("instructions.md")
        );
        assert_eq!(global, Some(global_tmp.path().join("instructions.md")));
    }

    /// Board item `01M0W5Q569F0T97HSEP6F0MPCR`, at the unit level:
    /// [`global_instructions_path`] must track wherever `CONWAY_CONFIG_DIR`
    /// points -- two distinct values must resolve to two distinct paths,
    /// never a single ambient location neither one names.
    #[test]
    fn global_instructions_path_honours_conway_config_dir() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        let dir_b = tempfile::tempdir().expect("tempdir");
        let mut env_a = std::collections::HashMap::new();
        env_a.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir_a.path().display().to_string(),
        );
        let mut env_b = std::collections::HashMap::new();
        env_b.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir_b.path().display().to_string(),
        );
        assert_eq!(
            global_instructions_path(&env_a),
            Some(dir_a.path().join("instructions.md"))
        );
        assert_eq!(
            global_instructions_path(&env_b),
            Some(dir_b.path().join("instructions.md"))
        );
        assert_ne!(
            global_instructions_path(&env_a),
            global_instructions_path(&env_b)
        );
    }

    /// The collapse case (point 3 of this module's own doc): when the
    /// global path would name the SAME file as the project path (the
    /// operator's project genuinely lives at their resolved global
    /// location), it must collapse to `None` rather than double-inject the
    /// same text under two fragment names.
    #[test]
    fn collapse_global_onto_project_drops_a_coincident_global_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join(".conway").join("instructions.md");
        let same_path_different_spelling =
            tmp.path().join(".").join(".conway").join("instructions.md");
        assert_eq!(
            collapse_global_onto_project(project.clone(), Some(same_path_different_spelling)),
            None,
            "a global path naming the same file as project must collapse to None"
        );
    }

    /// The ordinary case: a genuinely different global path passes through
    /// untouched.
    #[test]
    fn collapse_global_onto_project_keeps_a_distinct_global_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join(".conway").join("instructions.md");
        let global = tmp.path().join("elsewhere").join("instructions.md");
        assert_eq!(
            collapse_global_onto_project(project, Some(global.clone())),
            Some(global)
        );
    }

    /// `None` in, `None` out -- no home directory resolvable is not an
    /// error and does not accidentally conjure a global path.
    #[test]
    fn collapse_global_onto_project_passes_through_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join(".conway").join("instructions.md");
        assert_eq!(collapse_global_onto_project(project, None), None);
    }
}
