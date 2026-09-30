//! Slice 2 (board item `01M3DTT078W25MD2S4527R0WAV`): the shared, pure
//! logic behind turning slice 1's mechanical trigger (`trigger.rs`'s
//! [`crate::SkillProposalEvidence`]) into a written skill -- what the
//! ephemeral child is asked for, how its reply is parsed into a proposal
//! (or a refusal, or a malformed reply), and the write-approval config
//! knob that gates whether a proposal is ever offered at all.
//!
//! **This module owns no I/O and no live session.** `conway-cli`'s
//! `tui/app/skill_propose.rs` (`/conway.skills.propose`, the primary,
//! interactive path) and `conway-cli`'s `oneshot.rs` (the automatic
//! trigger, scoped to a non-`keep_alive` run -- see this crate's own
//! `trigger` module doc for exactly why a `keep_alive` TUI root cannot use
//! it) both call into this module's pure functions, so the two surfaces
//! can never parse a proposal, validate a name, or read the config knob
//! differently. Forking the ephemeral child, awaiting its reply, writing
//! the file, and showing anything to the operator are ALL `conway-cli`'s
//! job -- this module never touches `conway::SessionHandle`/`std::fs`.
//!
//! # The fork's own budget -- tokens the operator did not ask for
//!
//! A proposal is a byproduct of a task the operator actually wanted, not
//! the point of the session -- [`PROPOSAL_MAX_STEPS`] (15) and
//! [`PROPOSAL_DEADLINE_SECS`] (180) bound the ephemeral child so a
//! reflection that goes sideways (the model tool-calling instead of just
//! answering, or stalling) cannot spend an unbounded number of the
//! operator's own tokens or wall-clock seconds before the modal even
//! appears -- see `docs/plugins/skills.md`'s own "Budget" section for the
//! same two numbers stated to an operator. Both are round, plausible
//! numbers for "read back over what just happened and write a couple of
//! paragraphs," not measured calibrations -- the same "plausible, not
//! proven" disclosure `trigger.rs`'s own `DEFAULT_TOOL_CALL_THRESHOLD` doc
//! already makes, restated here for the same reason.

use std::path::{Path, PathBuf};

/// The directive handed to the ephemeral child `/conway.skills.propose`
/// (and the automatic trigger, for a one-shot run) forks -- asks for
/// EXACTLY one of two machine-parseable shapes (see
/// [`parse_proposal_reply`]): the literal [`NONE_SENTINEL`] alone, or a
/// complete `SKILL.md` document (the same frontmatter+body shape
/// `conway::skills::load_skill_defs` itself parses for an operator's own
/// `.conway/skills/<name>/SKILL.md`) with nothing else around it.
pub const PROPOSE_DIRECTIVE: &str = "\
Reflect on the procedure you just carried out in this conversation. If it represents a \
reusable skill worth capturing for future tasks, respond with ONLY a complete SKILL.md \
document, in exactly this shape:\n\
\n\
---\n\
name: a-short-kebab-case-identifier\n\
description: one line describing when to use this skill\n\
---\n\
\n\
The skill's body, in markdown, explaining the procedure so a future agent with no memory \
of this conversation could follow it.\n\
\n\
`name` must contain only lowercase letters, digits, and hyphens. Respond with nothing else \
-- no preamble, no code fence, no commentary before or after the document.\n\
\n\
If no reusable procedure was actually exercised, or the task was too specific to this one \
occasion to generalize into a skill, respond with exactly the single word NONE and nothing \
else.";

/// The literal reply [`parse_proposal_reply`] treats as "no skill is
/// warranted" -- matched case-insensitively, trimmed, against the WHOLE
/// reply (never a prefix match: a reply that happens to start with the
/// word "none" but continues is a proposal attempt, not a refusal, and
/// falls through to the frontmatter parse, where it is reported as
/// malformed rather than silently treated as a refusal).
pub const NONE_SENTINEL: &str = "NONE";

/// **180 seconds.** See this module's own doc, "The fork's own budget".
pub const PROPOSAL_DEADLINE_SECS: u64 = 180;

/// **15 steps.** See this module's own doc, "The fork's own budget".
pub const PROPOSAL_MAX_STEPS: u32 = 15;

/// `[plugins.config."conway.skills"].write_approval`'s key.
pub const WRITE_APPROVAL_KEY: &str = "write_approval";

/// `[plugins.config."conway.skills"].write_approval` -- **deliberately only
/// two values.** `Ask` (the default) shows the proposal and writes only on
/// `Enter`; `Never` suppresses the whole feature (no fork, no modal, no
/// tokens spent) -- see [`Self::parse`]'s own doc for why there is
/// deliberately no `always`: an unattended write is out of scope for this
/// slice on its own terms (the spec's own "Not to build" list), not merely
/// unimplemented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteApproval {
    #[default]
    Ask,
    Never,
}

impl WriteApproval {
    /// `"ask"`/`"never"`, case-sensitive (matching every other string-typed
    /// plugin config value in this workspace, e.g. `conway_plugin_trim`'s
    /// own `keep_turns` sibling keys never case-fold either) -- `None` for
    /// anything else, INCLUDING `"always"`: that string is refused BY NAME,
    /// not silently accepted as `Never`'s safest neighbor, so an operator
    /// who types it learns immediately that the mode does not exist rather
    /// than getting a mode they did not ask for.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ask" => Some(Self::Ask),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// Reads `[plugins.config."conway.skills"].write_approval` straight out of
/// the plugin's own already-loaded config JSON blob (`conway::ConwayConfig::
/// plugins.config.get("conway.skills")`) -- the same blob
/// `TriggerState::configure` (`crate::SkillsPlugin::configure`) validates loudly at plugin-install
/// time (an unrecognized value there is a build-time `PluginConfigureError`,
/// never silently accepted). This function is the LENIENT mirror of that
/// same parse, for a caller (`conway-cli`'s TUI/one-shot dispatch) that
/// only holds the merged `ConwayConfig`, not a live `SkillsPlugin` instance
/// -- by the time a caller here has a built `Conway` at all, `configure`
/// already ran and any invalid value would have failed the build, so
/// falling back to the default here on a missing/malformed value is safe,
/// never a second, looser validation pass a typo could sneak through.
pub fn write_approval_from_config(config: Option<&serde_json::Value>) -> WriteApproval {
    config
        .and_then(|value| value.as_object())
        .and_then(|object| object.get(WRITE_APPROVAL_KEY))
        .and_then(|value| value.as_str())
        .and_then(WriteApproval::parse)
        .unwrap_or_default()
}

/// What [`parse_proposal_reply`] made of the ephemeral child's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProposalOutcome {
    /// The child judged no skill was warranted ([`NONE_SENTINEL`]).
    NoneWarranted,
    /// The reply was neither [`NONE_SENTINEL`] nor a parseable `SKILL.md` --
    /// fails SAFE (never partially written, never guessed at): the caller
    /// shows `reason` and offers nothing to write.
    Malformed { reason: String },
    /// A parsed, validated proposal, ready to show for approval. `content`
    /// is the COMPLETE file text (frontmatter and body) to write verbatim
    /// to `.conway/skills/<name>/SKILL.md` -- see [`skill_md_path`].
    Proposal {
        name: String,
        description: Option<String>,
        content: String,
    },
}

/// Strips a single leading/trailing markdown code fence the model may have
/// wrapped the document in despite [`PROPOSE_DIRECTIVE`] asking it not to
/// -- lenient acceptance of a common instruction-following slip, not a
/// second grammar: only a plain opening ` ``` ` (optionally followed by a
/// language tag on the same line, e.g. ` ```markdown`) and a plain closing
/// ` ``` ` are recognized and removed; anything else is left untouched and
/// handled (or rejected) by the frontmatter parse that follows.
fn strip_code_fence(text: &str) -> String {
    let mut text = text;
    if let Some(rest) = text.strip_prefix("```") {
        text = match rest.find('\n') {
            Some(idx) => &rest[idx + 1..],
            None => rest,
        };
    }
    let text = text.trim_end();
    match text.strip_suffix("```") {
        Some(rest) => rest.trim_end().to_string(),
        None => text.to_string(),
    }
}

/// Whether `name` is safe to join onto a directory as
/// `<skills_root>/<name>/SKILL.md` -- lowercase ASCII letters, digits, and
/// hyphens only, non-empty, starting and ending with a letter or digit
/// (never a leading/trailing `-`, which would round-trip oddly as a
/// directory name even though it is technically legal on every common
/// filesystem). **This is a security boundary, not a style preference:**
/// `name` originates from model-authored text (the ephemeral child's own
/// reply), so without this check a hallucinated or adversarial frontmatter
/// value like `../../etc/cron.d/x` would reach [`skill_md_path`]'s `join`
/// verbatim and could write outside `.conway/skills` entirely. Rejecting
/// every character other than `[a-z0-9-]` makes a path-traversal segment
/// (`.`, `/`, `..`) structurally unrepresentable, rather than relying on a
/// caller to separately canonicalize or bounds-check the resulting path.
pub fn valid_skill_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 100 {
        return false;
    }
    let is_edge_ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let first_ok = name.chars().next().is_some_and(is_edge_ok);
    let last_ok = name.chars().next_back().is_some_and(is_edge_ok);
    first_ok
        && last_ok
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The path a validated `name` writes to under `skills_root` (conventionally
/// `<cwd>/.conway/skills`, the same fixed root `ConwayBuilder::build`/
/// `conway_plugin_skills::SkillsPlugin::from_dir` both read) -- the exact
/// `<name>/SKILL.md` layout `conway::skills::load_skill_defs` expects, so a
/// file this module's caller writes is discoverable by the very next
/// session with no translation step.
pub fn skill_md_path(skills_root: &Path, name: &str) -> PathBuf {
    skills_root.join(name).join("SKILL.md")
}

/// Parses the ephemeral child's reply text (`AgentResult::summary`) into a
/// [`ProposalOutcome`] -- the ONE place this decision is made, shared by
/// both the interactive `/conway.skills.propose` path and the automatic
/// trigger (this module's own doc). Never panics on malformed input: every
/// failure to parse lands on [`ProposalOutcome::Malformed`] with a reason a
/// caller can show verbatim.
pub fn parse_proposal_reply(text: &str) -> ProposalOutcome {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case(NONE_SENTINEL) {
        return ProposalOutcome::NoneWarranted;
    }
    let unfenced = strip_code_fence(trimmed);
    let unfenced = unfenced.trim();
    let mut lines = unfenced.lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => {
            return ProposalOutcome::Malformed {
                reason: "expected a SKILL.md document starting with `---` frontmatter, or the \
                         single word NONE"
                    .to_string(),
            }
        }
    }

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut frontmatter_closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            frontmatter_closed = true;
            break;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(unquote(rest.trim()));
        } else if let Some(rest) = line.strip_prefix("description:") {
            description = Some(unquote(rest.trim()));
        }
    }
    if !frontmatter_closed {
        return ProposalOutcome::Malformed {
            reason: "the frontmatter's closing `---` was never found".to_string(),
        };
    }
    let Some(name) = name else {
        return ProposalOutcome::Malformed {
            reason: "the frontmatter is missing a `name` field".to_string(),
        };
    };
    if !valid_skill_name(&name) {
        return ProposalOutcome::Malformed {
            reason: format!(
                "`{name}` is not a valid skill name -- only lowercase letters, digits, and \
                 hyphens are allowed, and it must start and end with one of those"
            ),
        };
    }
    let body: String = lines.collect::<Vec<_>>().join("\n");
    if body.trim().is_empty() {
        return ProposalOutcome::Malformed {
            reason: "the skill body is empty".to_string(),
        };
    }
    let description = description.filter(|d| !d.is_empty());
    ProposalOutcome::Proposal {
        name,
        description,
        content: format!("{unfenced}\n"),
    }
}

/// Strips one layer of matching `"`/`'` quotes a model may have wrapped a
/// frontmatter scalar in (common, legal YAML, and `serde_yaml` would strip
/// it too) -- this module does its own tiny frontmatter read rather than
/// depending on `serde_yaml` (not already a dependency of this crate, and
/// C-04 asks for no new one when the two fields this needs -- `name`/
/// `description` -- are this cheap to read directly).
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_approval_parse_accepts_the_two_documented_values() {
        assert_eq!(WriteApproval::parse("ask"), Some(WriteApproval::Ask));
        assert_eq!(WriteApproval::parse("never"), Some(WriteApproval::Never));
    }

    /// There is deliberately no `always` -- see [`WriteApproval`]'s own doc.
    #[test]
    fn write_approval_parse_refuses_always_and_everything_else() {
        assert_eq!(WriteApproval::parse("always"), None);
        assert_eq!(WriteApproval::parse("Ask"), None);
        assert_eq!(WriteApproval::parse(""), None);
    }

    #[test]
    fn write_approval_from_config_defaults_to_ask_when_absent() {
        assert_eq!(write_approval_from_config(None), WriteApproval::Ask);
        assert_eq!(
            write_approval_from_config(Some(&serde_json::json!({}))),
            WriteApproval::Ask
        );
    }

    #[test]
    fn write_approval_from_config_reads_never() {
        assert_eq!(
            write_approval_from_config(Some(&serde_json::json!({"write_approval": "never"}))),
            WriteApproval::Never
        );
    }

    /// A build-time-invalid value here (never possible in practice --
    /// `TriggerState::configure` would already have failed the build) still
    /// degrades to the safe default rather than panicking.
    #[test]
    fn write_approval_from_config_falls_back_to_ask_on_garbage() {
        assert_eq!(
            write_approval_from_config(Some(&serde_json::json!({"write_approval": "always"}))),
            WriteApproval::Ask
        );
        assert_eq!(
            write_approval_from_config(Some(&serde_json::json!("not an object"))),
            WriteApproval::Ask
        );
    }

    #[test]
    fn none_sentinel_is_recognized_case_insensitively_and_trimmed() {
        assert_eq!(parse_proposal_reply("NONE"), ProposalOutcome::NoneWarranted);
        assert_eq!(
            parse_proposal_reply("  none\n"),
            ProposalOutcome::NoneWarranted
        );
        assert_eq!(parse_proposal_reply("None"), ProposalOutcome::NoneWarranted);
    }

    /// P-15: a reply that merely STARTS WITH "none" but continues is not a
    /// refusal -- it falls through to the frontmatter parse (and is
    /// reported malformed here, since it is not a real SKILL.md either).
    #[test]
    fn a_reply_merely_starting_with_none_is_not_treated_as_a_refusal() {
        let outcome = parse_proposal_reply("none of this generalizes, but here is some text");
        assert!(matches!(outcome, ProposalOutcome::Malformed { .. }));
    }

    #[test]
    fn a_well_formed_proposal_parses_name_description_and_content() {
        let reply = "---\nname: git-commit-messages\ndescription: How to write a good commit \
                     message.\n---\n\n## git-commit-messages\n\nBody text.\n";
        match parse_proposal_reply(reply) {
            ProposalOutcome::Proposal {
                name,
                description,
                content,
            } => {
                assert_eq!(name, "git-commit-messages");
                assert_eq!(
                    description.as_deref(),
                    Some("How to write a good commit message.")
                );
                assert!(content.starts_with("---\n"));
                assert!(content.contains("Body text."));
            }
            other => panic!("expected Proposal, got {other:?}"),
        }
    }

    #[test]
    fn a_proposal_with_no_description_parses_with_none() {
        let reply = "---\nname: no-desc\n---\n\nBody.\n";
        match parse_proposal_reply(reply) {
            ProposalOutcome::Proposal { description, .. } => assert_eq!(description, None),
            other => panic!("expected Proposal, got {other:?}"),
        }
    }

    #[test]
    fn a_leading_and_trailing_code_fence_is_stripped() {
        let reply = "```markdown\n---\nname: fenced\n---\n\nBody.\n```";
        match parse_proposal_reply(reply) {
            ProposalOutcome::Proposal { name, content, .. } => {
                assert_eq!(name, "fenced");
                assert!(!content.contains("```"), "{content}");
            }
            other => panic!("expected Proposal, got {other:?}"),
        }
    }

    #[test]
    fn missing_name_is_malformed() {
        let reply = "---\ndescription: no name here\n---\n\nBody.\n";
        assert!(matches!(
            parse_proposal_reply(reply),
            ProposalOutcome::Malformed { .. }
        ));
    }

    #[test]
    fn an_unclosed_frontmatter_is_malformed() {
        let reply = "---\nname: unclosed\n\nBody.\n";
        assert!(matches!(
            parse_proposal_reply(reply),
            ProposalOutcome::Malformed { .. }
        ));
    }

    #[test]
    fn missing_leading_delimiter_is_malformed() {
        assert!(matches!(
            parse_proposal_reply("just some prose, no frontmatter at all"),
            ProposalOutcome::Malformed { .. }
        ));
    }

    #[test]
    fn an_empty_body_is_malformed() {
        let reply = "---\nname: empty-body\n---\n\n";
        assert!(matches!(
            parse_proposal_reply(reply),
            ProposalOutcome::Malformed { .. }
        ));
    }

    /// P-15/the security note on [`valid_skill_name`]'s own doc: a
    /// hallucinated or adversarial path-traversal name is refused, never
    /// silently sanitized or truncated into something else.
    #[test]
    fn a_path_traversal_name_is_rejected_as_malformed() {
        for bad in ["../../etc/cron.d/x", "/etc/passwd", "a/b", "..", "."] {
            let reply = format!("---\nname: {bad}\n---\n\nBody.\n");
            assert!(
                matches!(
                    parse_proposal_reply(&reply),
                    ProposalOutcome::Malformed { .. }
                ),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn valid_skill_name_accepts_kebab_case_and_rejects_edges_and_bad_chars() {
        assert!(valid_skill_name("git-commit"));
        assert!(valid_skill_name("a"));
        assert!(valid_skill_name("a1-b2"));
        assert!(!valid_skill_name(""));
        assert!(!valid_skill_name("-leading"));
        assert!(!valid_skill_name("trailing-"));
        assert!(!valid_skill_name("Upper"));
        assert!(!valid_skill_name("has space"));
        assert!(!valid_skill_name("has_underscore"));
        assert!(!valid_skill_name("has.dot"));
        assert!(!valid_skill_name("has/slash"));
    }

    #[test]
    fn skill_md_path_joins_the_conventional_layout() {
        let root = Path::new("/tmp/root/.conway/skills");
        assert_eq!(
            skill_md_path(root, "git-commit"),
            Path::new("/tmp/root/.conway/skills/git-commit/SKILL.md")
        );
    }

    /// A quoted frontmatter scalar (legal YAML) round-trips without the
    /// quotes -- proves [`unquote`] is exercised via the public parse, not
    /// merely present.
    #[test]
    fn a_quoted_frontmatter_value_is_unquoted() {
        let reply =
            "---\nname: \"quoted-name\"\ndescription: 'a quoted description'\n---\n\nBody.\n";
        match parse_proposal_reply(reply) {
            ProposalOutcome::Proposal {
                name, description, ..
            } => {
                assert_eq!(name, "quoted-name");
                assert_eq!(description.as_deref(), Some("a quoted description"));
            }
            other => panic!("expected Proposal, got {other:?}"),
        }
    }
}
