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
//! # Reach: root agents only (disclosed, not fixed here)
//!
//! `resolve_instructions` (`crates/conway-runtime/src/runtime/root.rs`) and
//! `SubagentHost::start` (`crates/conway-runtime/src/subagent.rs`) both give
//! every forked or spawned child `instructions: Vec::new()` unconditionally
//! -- `docs/plugins/hooks.md`'s point 17 states this as a first-class
//! caveat: "If you author a fragment, assume a subagent will not see it."
//! [`FRAGMENT_TEXT`] is written KNOWING the audience is the root only: its
//! "ending a turn"/"permissions"/"steering" bullets describe how a *child*
//! agent should behave, which a child reading this text would need most --
//! and will never receive. This item does not fix that gap; it ships the
//! content anyway, with the limitation stated here, in
//! [`IdiomPlugin::description`]'s `you_lose`, and in `docs/plugins/
//! idiom.md`'s own row, rather than leaving it to be discovered.
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

use std::sync::Arc;

use conway::plugin::{InstructionFragment, Plugin, PluginDescription, PluginManifest, Tool};

/// This plugin's published manifest id -- a config author (or a first-party
/// bundle's own linking module) resolves `[plugins].install` entries
/// against this constant.
pub const PLUGIN_ID: &str = "conway.idiom";

/// The bare name of this plugin's one [`InstructionFragment`].
pub const INSTRUCTION_NAME: &str = "conway.idiom.base";

/// The fragment's text, sourced from a markdown file in this crate's own
/// `fragments/` directory (`crates/conway-plugin-path`/
/// `crates/conway-plugin-discover`'s own `include_str!` convention --
/// `Plugin::instructions`'s own doc, "Convention, not enforcement"). 27
/// lines, 250 words -- against a 40-line/400-word budget measured from
/// Pi's own `system-prompt.ts` core template (`docs/vision/INTENT.md`'s
/// citation of Pi as conway's extension-surface reference). See this
/// module's own doc, "Reach: root agents only", for who actually reads
/// this text.
pub const FRAGMENT_TEXT: &str = include_str!("../fragments/idiom.md");

/// The `conway.idiom` plugin: contributes no tool, one instruction
/// fragment -- conway's own short idioms primer, prepended near the front
/// of a root session's assembled context (see this crate's own module doc
/// for the full "prepend"/`tool_ids` argument).
pub struct IdiomPlugin;

impl Plugin for IdiomPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![],
            required_host_caps: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "prepends a short conway-idioms primer to a session's system-prompt \
                       segment"
                .to_string(),
            you_get: "one instruction fragment (fork vs. spawn, how an agent ends, \
                      configuration-dependent tools, context scarcity, permissions, budgets, \
                      steering) injected near the front of the assembled context, ahead of the \
                      tool schemas and the conversation -- and ahead of the whole context when \
                      no agent def supplies its own system prompt, which is the ordinary \
                      interactive-TUI case this plugin exists for"
                .to_string(),
            you_lose: "nothing else -- but the fragment reaches ROOT agents only. A forked or \
                       spawned child gets no instruction fragments at all \
                       (resolve_instructions/SubagentHost::start both pass instructions: \
                       Vec::new()), so a subagent never sees this text, even though part of it \
                       describes how a child should behave"
                .to_string(),
            costs: "one system-prompt segment's worth of tokens per turn (roughly 250 words) \
                    -- /context's preamble section names conway.idiom.base and its exact token \
                    cost"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn instructions(&self) -> Vec<InstructionFragment> {
        vec![InstructionFragment {
            name: INSTRUCTION_NAME.to_string(),
            text: FRAGMENT_TEXT.to_string(),
            // Empty, deliberately -- see this module's own doc, "Determine
            // first #2: the `tool_ids` trap".
            tool_ids: vec![],
        }]
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
        let description = IdiomPlugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
        assert!(!description.costs.is_empty());
    }

    /// The subagent-reach limitation must be disclosed in `you_lose`
    /// verbatim, not merely in this crate's own doc comment -- GP-14: a
    /// declaration site is one artifact, and an operator deciding whether
    /// to install this plugin reads the description, not the source.
    #[test]
    fn you_lose_names_the_subagent_limitation() {
        let description = IdiomPlugin.description();
        assert!(
            description.you_lose.to_lowercase().contains("subagent")
                || description.you_lose.to_lowercase().contains("child"),
            "you_lose must state that the fragment does not reach a forked/spawned child: {:?}",
            description.you_lose
        );
    }

    /// Exactly one fragment, contributing no tool -- `manifest().tools` is
    /// empty, so the reachability check's "same plugin also provides the
    /// tool" shortcut never applies here; every id in `tool_ids` (there are
    /// none) would have to be reachable through a DIFFERENT installed
    /// plugin.
    #[test]
    fn contributes_exactly_one_fragment_and_no_tool() {
        let plugin = IdiomPlugin;
        assert!(plugin.tools().is_empty());
        assert!(plugin.manifest().tools.is_empty());
        let instructions = plugin.instructions();
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].name, INSTRUCTION_NAME);
        assert!(instructions[0].tool_ids.is_empty());
    }

    /// The `tool_ids` choice deliberately makes this fragment reachable
    /// even when NO tool at all is announced this turn -- the empty-list
    /// case `InstructionFragment::tool_ids`'s own doc calls "trivially
    /// always reachable". Checked directly against `ContextBuilder`'s own
    /// reachability rule (a tool_id is unreachable iff it is not among the
    /// turn's announced set) rather than merely asserted: an empty set has
    /// no such id, so the fragment can never be withheld by this check.
    #[test]
    fn tool_ids_are_trivially_always_reachable() {
        let instructions = IdiomPlugin.instructions();
        let known_tool_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let unreachable: Vec<_> = instructions[0]
            .tool_ids
            .iter()
            .filter(|id| !known_tool_ids.contains(id.as_str()))
            .collect();
        assert!(
            unreachable.is_empty(),
            "an empty tool_ids list must never be withheld, even against an empty announced \
             tool set"
        );
    }

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
