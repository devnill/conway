//! `agents/*.md` -- board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K` reverses
//! `crate`'s own earlier "skills and agents are out of scope" ruling (see
//! [`crate`]'s own top doc, "Question 3", amended in place).
//!
//! **The operator's own words, quoted rather than paraphrased**: "Agents
//! should also be included as plugins; these are essentially prompts that
//! would be inserted into a session that would operate in a similar manner
//! to how a system prompt would... This is the most complex of these
//! changes to map so we should consider carefully."
//!
//! **Where the actual translation lives: NOT this module.** Unlike
//! [`crate::skills`] (which builds a real, invokable `Command` itself,
//! because a slash command has no OTHER seam into a running conway),
//! `crates/conway/src/agents.rs::load_agent_defs_from_roots` is ALREADY the
//! seam this needed: `ConwayBuilder::with_extra_agent_dir` already exists,
//! already documented as "the seam a Claude Code compat layer... calls to
//! hand a plugin's own `agents/` directory to a real build," and simply
//! never had a caller. `crate_cli::claude_compat_plugins::install` (this
//! item's own sibling change there) now calls it with each entry's own
//! `agents/` directory; `conway::agents.rs`'s own (now Claude-tolerant)
//! LENIENT loader is what actually turns a Claude Code agent file into a
//! real [`conway_core::config::AgentDef`] -- see that function's own doc
//! for the parsing rules (comma-separated `tools:` string, bare `model:`
//! alias, directory-name-is-identity) and the safety-critical tool-name
//! filter.
//!
//! **What THIS module does: audit only.** It never constructs an
//! `AgentDef` and never depends on `conway` (the facade) at all --
//! unchanged from this crate's own "no `conway` in production code"
//! posture ([`crate`]'s own top doc). It mirrors `conway::agents.rs`'s OWN
//! translation rules closely enough to predict, per file, whether that
//! REAL loader will translate it or skip it, and to predict which declared
//! `tools:` names that loader will drop -- so [`crate::ClaudeCompatReport::
//! unsupported`] (already surfaced by `/plugin`'s own listing, acceptance
//! 5's mechanism) NAMES both, rather than an operator only finding out via
//! a `tracing::warn!` line that may never reach their terminal. This is
//! The closed set of tool names both this module and `conway::agents.rs`
//! match against is `conway_core::agent::KNOWN_BUILTIN_TOOL_NAMES` -- ONE
//! definition, imported by both.
//!
//! It briefly existed as two hand-synced copies, each documenting that the
//! other must be updated in step "because no shared dependency exists to
//! enforce this at compile time". Both crates depend on `conway-core`, so
//! one did. That set decides which tools an imported agent may call, and
//! drift between two copies would not fail loudly -- it would silently
//! change an agent's permissions, which is exactly what the safety ruling
//! below forbids. A rule that must not drift does not get two homes.
//!
//! **The safety ruling, escalated and decided, quoted in full because it
//! is the one invariant this module (and `conway::agents.rs`'s lenient
//! loader) must never violate:** "keep the names that resolve to real
//! conway tools, DROP the rest, and warn naming exactly what was dropped.
//! NEVER widen." A Claude Code tool name (`Read`, `Edit`, `Write`, `Bash`,
//! `Grep`, `Glob`, ...) is never conway's own tool name (`read`, `edit`,
//! `write`, `bash`, `grep`, `glob`, lower-case, conway's own
//! `conway_core::ids::ToolName` convention) -- a byte-for-byte string
//! match would resolve NOTHING, which is safe (an agent that grants
//! nothing extra) but useless and looks like a bug, exactly the failure
//! mode the ruling names. Case-INSENSITIVE matching against a closed,
//! first-party set is the minimum translation that makes this feature
//! non-trivially useful without ever inventing a mapping that could
//! resolve to something the operator did not intend.
//!
//! **`agents/*.md` bodies use the LITERAL `${CLAUDE_PLUGIN_ROOT}` token**
//! (unlike `skills/*/SKILL.md`'s own prose convention -- see
//! `crate::skills`'s own doc for that contrast), e.g. `ideate`'s own
//! `code-reviewer.md`: `` ${CLAUDE_PLUGIN_ROOT}/bin/ideate-work ``.
//! `conway::agents.rs`'s own lenient loader substitutes it directly with
//! this plugin's own absolute root before the body becomes
//! `AgentDef.system_prompt`, reusing the identical token
//! `crate::hooks::PLUGIN_ROOT_TOKEN` already names for `hooks.json`/
//! `.mcp.json` commands.

use std::path::Path;

use crate::frontmatter::{normalize_body, split_frontmatter};
use crate::fsutil::read_bounded;
use crate::unsupported::UnsupportedItem;

use conway_core::agent::KNOWN_BUILTIN_TOOL_NAMES;

/// Reads `<dir>/agents/*.md` (flat, non-recursive; a non-`.md` entry is
/// ignored entirely, mirroring `conway::agents::load_agent_defs_lenient`'s
/// own scan) and appends a named [`UnsupportedItem`] for every file that
/// this crate predicts `conway::agents.rs`'s own lenient loader will skip,
/// and for every declared `tools:` name that loader will drop. A file this
/// module predicts WILL translate, with no unresolved tool name,
/// contributes nothing here -- exactly like a `Ready` `commands/*.md`/
/// `skills/<name>` translation contributes nothing to `unsupported`.
///
/// Files are processed in SORTED name order -- the same determinism
/// `crate::commands::read_commands`/`crate::skills::read_skills` already
/// establish.
pub(crate) fn audit_agents(dir: &Path, unsupported: &mut Vec<UnsupportedItem>) {
    let agents_dir = dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return;
    };
    let mut file_names = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy().into_owned();
        if name.ends_with(".md") && entry.path().is_file() {
            file_names.push(name);
        }
    }
    file_names.sort();

    for file_name in file_names {
        audit_one(&agents_dir, &file_name, unsupported);
    }
}

fn audit_one(agents_dir: &Path, file_name: &str, unsupported: &mut Vec<UnsupportedItem>) {
    let relative_path = format!("agents/{file_name}");

    macro_rules! refuse {
        ($reason:expr) => {{
            unsupported.push(UnsupportedItem::agent(relative_path.clone(), $reason));
            return;
        }};
    }

    let content = match read_bounded(&agents_dir.join(file_name)) {
        Ok(content) => content,
        Err(err) => refuse!(format!("could not read this agent file: {err}")),
    };

    let (frontmatter_src, body) = match split_frontmatter(&content) {
        Ok((Some(src), body)) => (src, body),
        Ok((None, _)) => refuse!(
            "missing YAML frontmatter: conway's own agent-def loader requires a file to begin \
             with a `---` delimiter line, so this file translates into nothing"
                .to_string()
        ),
        Err(reason) => refuse!(reason.to_string()),
    };

    let normalized = normalize_body(body);
    if normalized.is_empty() {
        refuse!(
            "this agent file's body (its system prompt) is empty -- nothing to submit".to_string()
        );
    }

    let raw: RawFrontmatter = match serde_yaml::from_str(frontmatter_src) {
        Ok(raw) => raw,
        Err(err) => refuse!(format!("invalid YAML frontmatter: {err}")),
    };

    let Some(tools) = raw.tools else {
        return;
    };
    let declared = tools.into_names();
    for name in declared {
        let normalized_name = name.trim().to_lowercase();
        if !KNOWN_BUILTIN_TOOL_NAMES.contains(&normalized_name.as_str()) {
            unsupported.push(UnsupportedItem::agent_tool_restriction(
                &relative_path,
                name.trim(),
                "this Claude Code tool name has no known conway counterpart -- DROPPED from \
                 this agent's own tool restriction, never granted, so this agent's declared \
                 permissions can only ever be narrower than what it asked for, never wider"
                    .to_string(),
            ));
        }
    }
}

/// The frontmatter's wire shape, read for exactly the ONE field this
/// module needs an opinion on (`tools:`) -- every other key is simply
/// never looked at (this crate's own top doc, question 2: permissive, not
/// `deny_unknown_fields`). `#[serde(flatten)]` is deliberately NOT used
/// here (unlike `crate::commands`/`crate::skills`' own `RawFrontmatter`):
/// this module does not report unrecognized agent frontmatter keys as
/// findings of their own (unlike a `commands/*.md`/`SKILL.md` frontmatter
/// key) -- `conway::agents.rs`'s own lenient loader already ignores
/// anything it does not model, and duplicating a full unknown-key report
/// here would name every one of `model`/`role`/`max_steps`/`skills` (all
/// legitimately used by SOME agent files) as spuriously "unsupported" the
/// moment this module's own field list drifts even slightly behind that
/// loader's -- `tools:` is the one field with a PERMISSION consequence,
/// which is the one this module exists to police.
#[derive(Debug, Default, serde::Deserialize)]
struct RawFrontmatter {
    #[serde(default)]
    tools: Option<ToolsField>,
}

/// Claude Code's own `tools:` frontmatter convention is a comma-separated
/// STRING (`tools: Read, Edit, Write, Bash, Grep, Glob`, YAML block-scalar
/// syntax, verified against `ideate` 3.2.2's own real agent files) --
/// conway's OWN `.conway/agents` convention is a YAML LIST
/// (`tools: [read, grep]`, `conway::agents::RawFrontmatter`). Both are
/// accepted here (and by `conway::agents.rs`'s own lenient loader, which
/// carries the identical `#[serde(untagged)]` shape) so a third-party root
/// shipping either convention translates.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ToolsField {
    List(Vec<String>),
    Csv(String),
}

impl ToolsField {
    fn into_names(self) -> Vec<String> {
        match self {
            ToolsField::List(names) => names,
            ToolsField::Csv(csv) => csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_agent(dir: &Path, file_name: &str, contents: &str) {
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(dir.join("agents").join(file_name), contents).unwrap();
    }

    #[test]
    fn a_well_formed_agent_with_only_known_tools_contributes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_agent(
            dir.path(),
            "worker.md",
            "---\nname: worker\ntools: Read, Edit, Write, Bash, Grep, Glob\nmodel: sonnet\n---\n\nYou are the worker.\n",
        );
        let mut unsupported = Vec::new();
        audit_agents(dir.path(), &mut unsupported);
        assert!(unsupported.is_empty(), "{unsupported:?}");
    }

    /// The load-bearing safety case: a real Claude Code `tools:` name that
    /// has no conway counterpart is NAMED, with a permission-shaped
    /// reason -- proving this module can actually see the drop a naive
    /// silent translation would hide.
    #[test]
    fn an_unresolvable_tool_name_is_named_with_a_permission_shaped_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_agent(
            dir.path(),
            "worker.md",
            "---\nname: worker\ntools: Read, WebSearch, Task\n---\n\nBody.\n",
        );
        let mut unsupported = Vec::new();
        audit_agents(dir.path(), &mut unsupported);
        let names: Vec<_> = unsupported.iter().map(|u| u.name.as_str()).collect();
        assert!(names.contains(&"agents/worker.md#WebSearch"), "{names:?}");
        assert!(names.contains(&"agents/worker.md#Task"), "{names:?}");
        // `Read` DID resolve -- must not be named.
        assert!(!names.contains(&"agents/worker.md#Read"), "{names:?}");
        let web_search = unsupported
            .iter()
            .find(|u| u.name == "agents/worker.md#WebSearch")
            .unwrap();
        assert_eq!(
            web_search.kind,
            crate::unsupported::UnsupportedKind::AgentToolRestriction
        );
        assert!(web_search.reason.contains("DROPPED"), "{web_search:?}");
    }

    #[test]
    fn a_yaml_list_form_of_tools_is_also_understood() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_agent(
            dir.path(),
            "worker.md",
            "---\nname: worker\ntools: [Read, Bogus]\n---\n\nBody.\n",
        );
        let mut unsupported = Vec::new();
        audit_agents(dir.path(), &mut unsupported);
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].name, "agents/worker.md#Bogus");
    }

    #[test]
    fn no_tools_key_at_all_contributes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_agent(dir.path(), "worker.md", "---\nname: worker\n---\n\nBody.\n");
        let mut unsupported = Vec::new();
        audit_agents(dir.path(), &mut unsupported);
        assert!(unsupported.is_empty());
    }

    #[test]
    fn a_file_with_no_frontmatter_is_named_as_an_unusable_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_agent(dir.path(), "broken.md", "just a body, no frontmatter\n");
        let mut unsupported = Vec::new();
        audit_agents(dir.path(), &mut unsupported);
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].name, "agents/broken.md");
        assert_eq!(
            unsupported[0].kind,
            crate::unsupported::UnsupportedKind::Agent
        );
    }

    #[test]
    fn an_empty_body_is_named_as_an_unusable_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_agent(dir.path(), "blank.md", "---\nname: blank\n---\n");
        let mut unsupported = Vec::new();
        audit_agents(dir.path(), &mut unsupported);
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].name, "agents/blank.md");
    }

    #[test]
    fn an_absent_agents_directory_reports_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut unsupported = Vec::new();
        audit_agents(dir.path(), &mut unsupported);
        assert!(unsupported.is_empty());
    }

    #[test]
    fn a_non_markdown_file_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_agent(dir.path(), "README.txt", "not an agent");
        let mut unsupported = Vec::new();
        audit_agents(dir.path(), &mut unsupported);
        assert!(unsupported.is_empty());
    }
}
