//! Everything an operator's Claude Code plugin directory can contain that
//! this crate does not use -- named, one item per finding, never folded
//! into a single "N things skipped" count. Acceptance 5's whole point: an
//! operator must be able to tell "this plugin works" from "this plugin
//! half works", and a count cannot name anything.

use std::path::Path;

/// What kind of thing a given [`UnsupportedItem`] names -- carried
/// separately from `reason` so a caller (the `/plugin` listing, a report)
/// can group or count by kind without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedKind {
    /// A `commands/*.md` file -- out of scope for this item; see
    /// `UnsupportedItem::command`'s own doc for the corrected reason
    /// (spec update 2: `CommandOutcome::SubmitPrompt` now exists, but
    /// wiring Claude Code commands to it is a separate, deferred item).
    Command,
    /// A `skills/<name>/SKILL.md` directory -- out of scope (question 3:
    /// skills are single-rooted in conway today).
    Skill,
    /// An `agents/*.md` file -- out of scope (question 3: `AgentsConfig::
    /// dir` is a single `PathBuf`, not a list).
    Agent,
    /// A `hooks/hooks.json` rule whose Claude Code event has no conway
    /// counterpart.
    Hook,
    /// A `.mcp.json` server entry this crate could not translate (a
    /// malformed shape -- see `mcp::read_mcp_servers`).
    McpServer,
}

/// One named, reasoned finding: something this crate saw in the plugin
/// directory and did not use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedItem {
    pub kind: UnsupportedKind,
    /// The thing's own name -- a relative path (`commands/foo.md`,
    /// `skills/bar`, `agents/baz.md`), a hook event name, or an `.mcp.json`
    /// server key. Always specific enough that an operator who reads the
    /// plugin directory's own listing recognizes exactly which entry this
    /// is about.
    pub name: String,
    pub reason: String,
}

impl UnsupportedItem {
    pub(crate) fn command(relative_path: impl Into<String>) -> Self {
        Self {
            kind: UnsupportedKind::Command,
            name: relative_path.into(),
            reason: "commands/*.md submits a prompt, which conway can now DO \
                     (CommandOutcome::SubmitPrompt / SessionHandle::prompt_command exist) -- \
                     wiring a Claude Code command file to that capability is a separate, \
                     deferred follow-up, not something this item's absence of the capability \
                     ever blocked. Out of scope here regardless."
                .to_string(),
        }
    }

    pub(crate) fn skill(relative_path: impl Into<String>) -> Self {
        Self {
            kind: UnsupportedKind::Skill,
            name: relative_path.into(),
            reason: "skill import is out of scope for this item -- conway's own skill loader \
                     is single-rooted (crates/conway/src/skills.rs hardcodes .conway/skills) \
                     with no multi-root discovery to read a second directory from"
                .to_string(),
        }
    }

    pub(crate) fn agent(relative_path: impl Into<String>) -> Self {
        Self {
            kind: UnsupportedKind::Agent,
            name: relative_path.into(),
            reason: "agent import is out of scope for this item -- AgentsConfig::dir is a \
                     single PathBuf, not a list of roots to search"
                .to_string(),
        }
    }

    pub(crate) fn hook(event_name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: UnsupportedKind::Hook,
            name: event_name.into(),
            reason: reason.into(),
        }
    }

    pub(crate) fn mcp_server(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: UnsupportedKind::McpServer,
            name: name.into(),
            reason: reason.into(),
        }
    }
}

/// Appends one [`UnsupportedItem::command`] per `commands/*.md` file found
/// directly under `dir/commands/` (no recursion -- Claude Code's own
/// convention is a flat directory of command files).
pub(crate) fn scan_commands(dir: &Path, out: &mut Vec<UnsupportedItem>) {
    scan_flat_markdown(dir, "commands", out, UnsupportedItem::command);
}

/// Appends one [`UnsupportedItem::agent`] per `agents/*.md` file.
pub(crate) fn scan_agents(dir: &Path, out: &mut Vec<UnsupportedItem>) {
    scan_flat_markdown(dir, "agents", out, UnsupportedItem::agent);
}

fn scan_flat_markdown(
    dir: &Path,
    subdir: &str,
    out: &mut Vec<UnsupportedItem>,
    build: impl Fn(String) -> UnsupportedItem,
) {
    let path = dir.join(subdir);
    let Ok(entries) = std::fs::read_dir(&path) else {
        return;
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name.ends_with(".md") && entry.path().is_file() {
            names.push(format!("{subdir}/{name}"));
        }
    }
    names.sort();
    out.extend(names.into_iter().map(build));
}

/// Appends one [`UnsupportedItem::skill`] per subdirectory of `dir/skills/`
/// that itself contains a `SKILL.md` file -- a subdirectory without one is
/// not a skill by Claude Code's own convention, and is left unreported (it
/// is simply not a skill directory).
pub(crate) fn scan_skills(dir: &Path, out: &mut Vec<UnsupportedItem>) {
    let path = dir.join("skills");
    let Ok(entries) = std::fs::read_dir(&path) else {
        return;
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if entry.path().join("SKILL.md").is_file() {
            names.push(format!("skills/{}", entry.file_name().to_string_lossy()));
        }
    }
    names.sort();
    out.extend(names.into_iter().map(UnsupportedItem::skill));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_commands_names_every_command_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("commands")).unwrap();
        std::fs::write(dir.path().join("commands").join("review.md"), "").unwrap();
        std::fs::write(dir.path().join("commands").join("deploy.md"), "").unwrap();
        // Not a command file -- must not appear.
        std::fs::write(dir.path().join("commands").join("README.txt"), "").unwrap();

        let mut out = Vec::new();
        scan_commands(dir.path(), &mut out);
        let names: Vec<_> = out.iter().map(|i| i.name.clone()).collect();
        assert_eq!(names, vec!["commands/deploy.md", "commands/review.md"]);
        assert!(out.iter().all(|i| i.kind == UnsupportedKind::Command));
    }

    #[test]
    fn scan_skills_only_counts_directories_that_actually_contain_skill_md() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("skills").join("real-skill")).unwrap();
        std::fs::write(
            dir.path()
                .join("skills")
                .join("real-skill")
                .join("SKILL.md"),
            "",
        )
        .unwrap();
        // No SKILL.md -- not a skill, must not be reported.
        std::fs::create_dir_all(dir.path().join("skills").join("not-a-skill")).unwrap();

        let mut out = Vec::new();
        scan_skills(dir.path(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "skills/real-skill");
        assert_eq!(out[0].kind, UnsupportedKind::Skill);
    }

    #[test]
    fn scan_agents_names_every_agent_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        std::fs::write(dir.path().join("agents").join("reviewer.md"), "").unwrap();

        let mut out = Vec::new();
        scan_agents(dir.path(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "agents/reviewer.md");
        assert_eq!(out[0].kind, UnsupportedKind::Agent);
    }

    #[test]
    fn an_absent_subdirectory_reports_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut out = Vec::new();
        scan_commands(dir.path(), &mut out);
        scan_skills(dir.path(), &mut out);
        scan_agents(dir.path(), &mut out);
        assert!(out.is_empty());
    }
}
