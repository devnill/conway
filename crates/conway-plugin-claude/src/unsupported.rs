//! Everything an operator's Claude Code plugin directory can contain that
//! this crate does not use -- named, one item per finding, never folded
//! into a single "N things skipped" count. Acceptance 5's whole point: an
//! operator must be able to tell "this plugin works" from "this plugin
//! half works", and a count cannot name anything.

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
    /// A `skills/<name>/SKILL.md` directory that did NOT become a real,
    /// invokable `conway_core::ports::Command` -- board item
    /// `01M1DG5TTF6NHW2RXJRZ8ZPE7K` reverses the earlier "skills are out of
    /// scope" ruling (see `crate::skills`'s own module doc); this kind now
    /// names ONLY the ones that failed to translate, the same "named only
    /// on failure" narrowing board item `01M0X1G29EZSFEWB1YAG40SE69`
    /// already made for [`Self::Command`].
    Skill,
    /// A frontmatter key on a `skills/<name>/SKILL.md` this crate does not
    /// honor -- named EVEN ON a skill that otherwise translates
    /// successfully, mirroring [`Self::CommandFrontmatterKey`]'s own
    /// "worked, but this one key did nothing" distinction from
    /// [`Self::Skill`].
    SkillFrontmatterKey,
    /// An `agents/*.md` file that did NOT translate into an [`conway_core::
    /// config::AgentDef`] usable by `conway::agents::load_agent_defs_from_
    /// roots` -- board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K` reverses the
    /// earlier "agents are out of scope" ruling (see `crate::agents`'s own
    /// module doc).
    Agent,
    /// A DECLARED tool restriction on an `agents/*.md` file's own `tools:`
    /// frontmatter that named something conway has no counterpart for --
    /// named, PERMISSION-shaped (never silently widened; never silently
    /// dropped without saying so) -- see `crate::agents`'s own module doc,
    /// "the safety ruling".
    AgentToolRestriction,
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

    /// `reason` is supplied by the caller (`crate::skills::read_skills`) --
    /// mirrors [`Self::command`]'s own evolution (board item
    /// `01M0X1G29EZSFEWB1YAG40SE69`): a `skills/<name>` finding's fate now
    /// varies per directory, not a single hard-coded "out of scope" reason.
    pub(crate) fn skill(relative_path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: UnsupportedKind::Skill,
            name: relative_path.into(),
            reason: reason.into(),
        }
    }

    /// Names one frontmatter key, on one `skills/<name>/SKILL.md`, that
    /// `crate::skills` does not honor -- `name` is
    /// `"<relative_path>#<key>"`, the identical shape
    /// [`Self::command_frontmatter_key`] already uses for the sibling
    /// `commands/*.md` case.
    pub(crate) fn skill_frontmatter_key(
        relative_path: &str,
        key: &str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind: UnsupportedKind::SkillFrontmatterKey,
            name: format!("{relative_path}#{key}"),
            reason: reason.into(),
        }
    }

    /// `reason` is supplied by the caller (`crate::agents::audit_agents`) --
    /// the same "no longer one hard-coded reason" evolution [`Self::
    /// skill`]'s own doc gives.
    pub(crate) fn agent(relative_path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            kind: UnsupportedKind::Agent,
            name: relative_path.into(),
            reason: reason.into(),
        }
    }

    /// Names one DECLARED tool restriction, on one `agents/*.md` file, that
    /// conway dropped rather than granted -- `name` is
    /// `"<relative_path>#<claude_tool_name>"`, the same `#`-joined shape
    /// [`Self::command_frontmatter_key`]/[`Self::skill_frontmatter_key`]
    /// already use for a "one finding within one file" pairing.
    pub(crate) fn agent_tool_restriction(
        relative_path: &str,
        claude_tool_name: &str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind: UnsupportedKind::AgentToolRestriction,
            name: format!("{relative_path}#{claude_tool_name}"),
            reason: reason.into(),
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

// `commands/*.md`, `skills/<name>/SKILL.md`, and `agents/*.md` no longer
// scan through a generic name-only helper -- board item
// `01M0X1G29EZSFEWB1YAG40SE69` moved the first to `crate::commands::
// read_commands`; board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K` moved the other
// two to `crate::skills::read_skills`/`crate::agents::audit_agents`. Each
// reads its own file's content (frontmatter, body) rather than merely
// naming it, and needs a per-file REASON the old single-arg
// `build: impl Fn(String) -> UnsupportedItem` shape this module used to
// hand `scan_flat_markdown` could not carry. This module now only holds
// [`UnsupportedItem`]'s own constructors -- every SCAN lives with its own
// translation.

#[cfg(test)]
mod tests {
    use super::*;

    // Every scan itself is covered by its own crate module's test suite now
    // (`crate::commands`, `crate::skills`, `crate::agents`). What is local
    // and checkable HERE is only `UnsupportedItem`'s own constructors'
    // shape -- both live in this file.

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
    fn skill_names_the_relative_path_with_the_supplied_reason() {
        let item = UnsupportedItem::skill("skills/broken", "empty body");
        assert_eq!(item.kind, UnsupportedKind::Skill);
        assert_eq!(item.name, "skills/broken");
        assert_eq!(item.reason, "empty body");
    }

    #[test]
    fn skill_frontmatter_key_names_the_directory_and_key_together() {
        let item =
            UnsupportedItem::skill_frontmatter_key("skills/refine", "argument-hint", "ignored");
        assert_eq!(item.kind, UnsupportedKind::SkillFrontmatterKey);
        assert_eq!(item.name, "skills/refine#argument-hint");
        assert_eq!(item.reason, "ignored");
    }

    #[test]
    fn agent_names_the_relative_path_with_the_supplied_reason() {
        let item = UnsupportedItem::agent("agents/broken.md", "empty prompt");
        assert_eq!(item.kind, UnsupportedKind::Agent);
        assert_eq!(item.name, "agents/broken.md");
        assert_eq!(item.reason, "empty prompt");
    }

    #[test]
    fn agent_tool_restriction_names_the_file_and_dropped_tool_together() {
        let item = UnsupportedItem::agent_tool_restriction(
            "agents/worker.md",
            "WebSearch",
            "no conway counterpart",
        );
        assert_eq!(item.kind, UnsupportedKind::AgentToolRestriction);
        assert_eq!(item.name, "agents/worker.md#WebSearch");
        assert_eq!(item.reason, "no conway counterpart");
    }
}
