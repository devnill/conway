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
    /// A `commands/*.md` file that did NOT become a real, invokable
    /// `conway_core::ports::Command` -- board item `01M0X1G29EZSFEWB1YAG40SE69`
    /// wires most `commands/*.md` files up (see `crate::commands`'s own
    /// module doc); this kind now names ONLY the ones that failed to
    /// translate: an unreadable file, unterminated/malformed frontmatter,
    /// an empty body, a raw `$ARGUMENTS` placeholder this crate refuses to
    /// submit verbatim, or a file-stem-derived bare name that could never
    /// be typed. See `crate::commands::CommandMapOutcome::Refused`'s own
    /// doc for the full, closed list of reasons.
    Command,
    /// A frontmatter key on a `commands/*.md` file this crate does not
    /// honor -- named EVEN ON a file that otherwise translates
    /// successfully (unlike [`Self::Command`], this is not "this file
    /// failed", it is "this file worked, but this one declared key did
    /// nothing"). `description` is the only key `crate::commands` reads;
    /// every other key present is named this way, `allowed-tools` above
    /// all (a PERMISSION surprise, not a fidelity gap -- the operator
    /// ruling `crate::commands`'s own module doc quotes).
    CommandFrontmatterKey,
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
    /// `reason` is supplied by the caller now (board item
    /// `01M0X1G29EZSFEWB1YAG40SE69`): unlike the earlier "every
    /// `commands/*.md` file is out of scope, unconditionally" posture this
    /// constructor used to encode with a single hard-coded reason, a
    /// `commands/*.md` file's own fate now varies file by file -- see
    /// `crate::commands::CommandMapOutcome::Refused`'s own doc for the
    /// closed set of reasons a caller passes here.
    pub(crate) fn command(relative_path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: UnsupportedKind::Command,
            name: relative_path.into(),
            reason: reason.into(),
        }
    }

    /// Names one frontmatter key, on one `commands/*.md` file, that
    /// `crate::commands` does not honor -- `name` is
    /// `"<relative_path>#<key>"` so an operator reading the flat
    /// `unsupported` list can tell exactly which file's which key this is
    /// about, distinct from a whole-file [`Self::command`] finding.
    pub(crate) fn command_frontmatter_key(
        relative_path: &str,
        key: &str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind: UnsupportedKind::CommandFrontmatterKey,
            name: format!("{relative_path}#{key}"),
            reason: reason.into(),
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

/// `commands/*.md` no longer scans through this generic helper -- board
/// item `01M0X1G29EZSFEWB1YAG40SE69` moved it to `crate::commands::
/// read_commands`, which reads each file's own content (frontmatter, body)
/// rather than merely naming the file, and needs a per-file REASON this
/// helper's single-arg `build: impl Fn(String) -> UnsupportedItem` shape
/// cannot carry. `scan_flat_markdown` remains exactly as it was for
/// `agents/*.md`, which is still purely name-only (question 3: out of
/// scope, unconditionally, with one fixed reason).
///
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

    // `commands/*.md` scanning itself is covered by `crate::commands`'s own
    // test suite now (`read_commands` -- it reads each file's content, not
    // merely its name, so it no longer fits this module's generic
    // name-only `scan_flat_markdown` helper). `UnsupportedItem::command`/
    // `UnsupportedItem::command_frontmatter_key`'s own shape is still
    // exercised directly, here, since both constructors live in this file.

    #[test]
    fn command_names_the_relative_path_with_the_supplied_reason() {
        let item = UnsupportedItem::command("commands/broken.md", "empty body");
        assert_eq!(item.kind, UnsupportedKind::Command);
        assert_eq!(item.name, "commands/broken.md");
        assert_eq!(item.reason, "empty body");
    }

    #[test]
    fn command_frontmatter_key_names_the_file_and_key_together() {
        let item = UnsupportedItem::command_frontmatter_key(
            "commands/config.md",
            "allowed-tools",
            "ignored",
        );
        assert_eq!(item.kind, UnsupportedKind::CommandFrontmatterKey);
        assert_eq!(item.name, "commands/config.md#allowed-tools");
        assert_eq!(item.reason, "ignored");
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
        scan_skills(dir.path(), &mut out);
        scan_agents(dir.path(), &mut out);
        assert!(out.is_empty());
    }
}
