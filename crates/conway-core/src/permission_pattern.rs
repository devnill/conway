//! Prefix-matching permission patterns (V2), and the metacharacter gate
//! that makes them safe.
//!
//! ## Why prefix matching, and not regex
//!
//! The dominant failure mode of a pattern-based permission grant is a user
//! granting broader authority than they realized. Regex is *designed* for
//! expressive matching, which is exactly the wrong property for a safety
//! boundary: `git .*` reads as tight, but `.` matches `;`, so it would
//! authorize `git status; <anything>`.
//!
//! A prefix is predictable by reading. It also composes with how shell
//! commands are actually written -- subcommand first, flags after -- so
//! `git status` naturally covers `git status --short` without covering
//! `git push`. And it is explainable inline at the moment of granting:
//! "always allow commands starting with `git status`" is a sentence a user
//! can evaluate before saying yes.
//!
//! ## Why a prefix alone is NOT enough
//!
//! `git status && <anything>` starts with `git status`. So does
//! `git status; <anything>`, and `git status $(<anything>)`. A pattern
//! grant that only checked the prefix would authorize all of them.
//!
//! [`contains_shell_metacharacters`] is therefore a **hard gate**: a
//! command carrying any shell metacharacter can never be satisfied by a
//! pattern, no matter what patterns exist. It always falls through to the
//! operator. This is checked in [`PatternRule::matches_render`] itself --
//! not at rule-creation time, and not at a call site that could be
//! forgotten -- so there is no path from a pattern to an allow decision
//! that skips it.
//!
//! The gate is deliberately conservative in the "re-prompt too often"
//! direction. A command with a `|` in it re-prompts even when that pipe is
//! completely benign, because the cost of an unnecessary prompt is a
//! keystroke and the cost of a missed one is arbitrary execution.
//!
//! ## The gate only means something for a SHELL command (board item
//! 01KYT3NSWRHMPEAXVXRJ73KDYR)
//!
//! The gate exists to stop a chained/substituted *shell command* from
//! riding a matched prefix past what was granted. That reasoning has
//! nothing to say about a tool like `read`, whose `Tool::render` is a JSON
//! debug dump (`read({"path":"src/main.rs"})`) that is never handed to a
//! shell -- yet that dump's own `(`, `)`, `{` trip the SAME gate, on sight,
//! for every call. Applying a shell-injection gate to a string no shell
//! will ever see is a category error, and it is what made every pattern
//! grant except `bash`'s inert: `read:*`, `write:*`, `edit:*`, `grep:*`, and
//! every third-party tool's wildcard matched nothing, ever.
//!
//! [`PatternRule::matches_render`] therefore takes the tool's own
//! [`conway_core::ports::RenderKind`] declaration (`conway_core::ports::
//! Tool::render_kind`) and applies the metacharacter gate only when that
//! declaration says the rendered text IS a shell command. [`PatternRule::
//! matches`] (no `RenderKind` parameter) is the conservative convenience
//! this module's own tests and any caller without a tool's declaration in
//! hand can still reach -- it always gates, exactly as this module behaved
//! before this distinction existed. Every production caller
//! (`conway_runtime::permission::PermissionBroker`) uses
//! [`Self::matches_render`], fed the real tool's real declaration, so a
//! chained shell command is gated exactly as before while a `read`/`write`/
//! `edit`/... wildcard now actually grants.
//!
//! **The gate itself -- the code path that rejects a chained command -- is
//! unchanged.** What changed is only whether it is CONSULTED for a given
//! tool, decided by a declaration the tool makes about itself, never by
//! this module inspecting a tool's name.

use serde::{Deserialize, Serialize};

use crate::ports::RenderKind;

/// Shell metacharacters that disqualify a command from *ever* being
/// satisfied by a pattern grant.
///
/// Each one can extend a command beyond the part the pattern matched:
/// `;`/newline sequence, `&`/`|` chain or background, backtick/`$(`
/// substitute, `<`/`>` redirect. The list is intentionally broad -- an
/// unnecessary re-prompt is cheap; a missed one is not.
const SHELL_METACHARACTERS: &[char] = &[
    ';', '&', '|', '`', '$', '\n', '\r', '<', '>', '(', ')', '{', '}',
];

/// The character a sanitized `rendered` string carries in place of a
/// control byte (see `conway-runtime`'s `sanitize_rendered`).
///
/// The gate MUST treat this as disqualifying. `rendered` is sanitized for
/// display safety BEFORE it reaches [`PatternRule::matches`], and that
/// sanitization rewrites `\n`/`\r` -- two of the [`SHELL_METACHARACTERS`]
/// above -- into this placeholder. Without this, `git status \n rm -rf /`
/// would arrive as `git status <U+FFFD> rm -rf /`: the newline evidence
/// destroyed, the gate satisfied, and the replacement char consumed as its
/// own whitespace-delimited token by [`prefix_matches`] -- silently
/// auto-approving a chained command under a `bash:git status` grant.
const SANITIZED_CONTROL_PLACEHOLDER: char = '\u{FFFD}';

/// Whether `command` contains anything that could extend it past a matched
/// prefix. See this module's doc for why this is a hard gate rather than a
/// heuristic.
///
/// Disqualifies three classes, all in the "re-prompt too often" direction:
/// the [`SHELL_METACHARACTERS`] themselves; any control character (so an
/// UNSANITIZED string carrying a raw `\n`/`\x1b` is caught here even if it
/// never passed through a sanitizer); and [`SANITIZED_CONTROL_PLACEHOLDER`]
/// (so a SANITIZED string whose control char was already rewritten is
/// caught too). The gate must not depend on where in the pipeline it is
/// called from -- it is the security boundary, so it assumes nothing about
/// what upstream did or did not do to the string.
pub fn contains_shell_metacharacters(command: &str) -> bool {
    command.chars().any(|c| {
        SHELL_METACHARACTERS.contains(&c) || c.is_control() || c == SANITIZED_CONTROL_PLACEHOLDER
    })
}

/// One persisted grant: a tool name plus a command prefix.
///
/// `command_prefix` of `*` means "any invocation of this tool" -- the only
/// wildcard the language has, and deliberately the only one. It exists for
/// tools whose `rendered` form is not a shell command at all (`read`,
/// `grep`), where a prefix would be meaningless.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternRule {
    pub tool: String,
    pub command_prefix: String,
}

impl PatternRule {
    /// Parses the wire form `tool:prefix` (e.g. `bash:git status`,
    /// `read:*`). Returns `None` for anything malformed -- an unparseable
    /// rule is DROPPED rather than guessed at, because a
    /// half-understood permission rule is worse than a missing one.
    ///
    /// The prefix may itself contain `:` (`bash:ssh host:path` is a
    /// sensible rule), so only the FIRST `:` separates tool from prefix.
    pub fn parse(raw: &str) -> Option<Self> {
        let (tool, prefix) = raw.split_once(':')?;
        let tool = tool.trim();
        let prefix = prefix.trim();
        if tool.is_empty() || prefix.is_empty() {
            return None;
        }
        Some(Self {
            tool: tool.to_string(),
            command_prefix: prefix.to_string(),
        })
    }

    /// Whether this rule authorizes `tool` running `rendered`, assuming the
    /// CONSERVATIVE [`RenderKind::ShellCommand`] -- i.e. the metacharacter
    /// gate always applies, exactly as this module behaved before
    /// [`RenderKind`] existed.
    ///
    /// This is the right call when the caller has no tool declaration in
    /// hand (this module's own tests; any consumer that only has a bare
    /// string). A caller that DOES have the real tool's declaration --
    /// every production caller does -- should use [`Self::matches_render`]
    /// instead, so a `Structured`-rendering tool's wildcard/prefix grants
    /// are not held gated for no reason. See this module's own doc for why
    /// that distinction exists at all.
    pub fn matches(&self, tool: &str, rendered: &str) -> bool {
        self.matches_render(tool, rendered, RenderKind::ShellCommand)
    }

    /// Whether this rule authorizes `tool` running `rendered`, given that
    /// tool's own [`RenderKind`] declaration.
    ///
    /// The metacharacter gate is checked HERE, before any prefix
    /// comparison, so every path to a pattern-based allow passes through
    /// it -- but ONLY when `render_kind` is [`RenderKind::ShellCommand`].
    /// For [`RenderKind::Structured`], `rendered` is never handed to a
    /// shell, so a metacharacter in it (JSON's own `(){}`) is not
    /// command-injection risk, and gating on it would only ever produce a
    /// false rejection -- see this module's own doc for the full
    /// reasoning. A `*` rule is gated too whenever the check applies at
    /// all: "any invocation of this tool" still must not mean "any
    /// invocation, including chained ones" for a tool whose rendering
    /// genuinely is a shell command.
    pub fn matches_render(&self, tool: &str, rendered: &str, render_kind: RenderKind) -> bool {
        if self.tool != tool {
            return false;
        }
        // The hard gate. Deliberately first, and deliberately applied to
        // the wildcard case as well -- but only when this tool's own
        // rendering could ever reach a shell.
        if render_kind == RenderKind::ShellCommand && contains_shell_metacharacters(rendered) {
            return false;
        }
        if self.command_prefix == "*" {
            return true;
        }
        prefix_matches(&self.command_prefix, rendered)
    }

    /// A human-readable description of what this rule permits, for the
    /// prompt that offers it. A user must be able to evaluate the grant
    /// before accepting it, not discover its breadth afterward.
    pub fn describe(&self) -> String {
        if self.command_prefix == "*" {
            format!("any `{}` call", self.tool)
        } else {
            format!(
                "`{}` commands starting with `{}`",
                self.tool, self.command_prefix
            )
        }
    }

    /// The wire form, round-tripping [`Self::parse`].
    pub fn to_wire(&self) -> String {
        format!("{}:{}", self.tool, self.command_prefix)
    }
}

/// Prefix comparison on whitespace-delimited tokens.
///
/// Token-wise rather than byte-wise deliberately: a byte prefix would let
/// `git status` match `git statusfoo`, since the latter genuinely starts
/// with the former. Comparing tokens means the prefix must align with a
/// real argument boundary, so `git status` covers `git status --short`
/// but not `git statusfoo` and not `git push`.
fn prefix_matches(prefix: &str, rendered: &str) -> bool {
    let mut pattern_tokens = prefix.split_whitespace();
    let mut command_tokens = rendered.split_whitespace();
    loop {
        match (pattern_tokens.next(), command_tokens.next()) {
            // Pattern exhausted: every one of its tokens matched, so the
            // command is this prefix plus (possibly) extra arguments.
            (None, _) => return true,
            // Command exhausted before the pattern: too short to match.
            (Some(_), None) => return false,
            (Some(p), Some(c)) if p == c => continue,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the hard gate ----

    /// THE most important test in this module. A grant for `git status`
    /// must not authorize a chained command that merely begins with it.
    #[test]
    fn a_chained_command_is_never_matched_even_when_its_prefix_was_granted() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");

        assert!(
            rule.matches("bash", "git status"),
            "the plain granted command must still match"
        );

        for chained in [
            "git status && rm -rf /",
            "git status; rm -rf /",
            "git status || curl evil.example",
            "git status | tee /etc/passwd",
            "git status `whoami`",
            "git status $(whoami)",
            "git status > /etc/passwd",
            "git status\nrm -rf /",
        ] {
            assert!(
                !rule.matches("bash", chained),
                "a command carrying a shell metacharacter must never be \
                 satisfied by a pattern grant: {chained:?}"
            );
        }
    }

    /// Regression: a SANITIZED chained command must still be gated.
    ///
    /// `rendered` is sanitized for display safety (control chars -> U+FFFD)
    /// BEFORE it reaches `matches`. That rewrite destroys `\n`/`\r` -- two
    /// of the gate's own metacharacters -- so a gate that only looked for
    /// the literal characters would see nothing wrong, and `prefix_matches`
    /// would consume the replacement char as its own whitespace-delimited
    /// token. `git status \n rm -rf /` would then be silently auto-approved
    /// under a `bash:git status` grant.
    ///
    /// Both forms are pinned here: the raw string (as an unsanitized caller
    /// would pass it) and the sanitized string (as the production
    /// `ToolRunner` seam actually produces).
    #[test]
    fn a_sanitized_chained_command_is_still_gated() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");

        // The spacing variants matter: a U+FFFD flanked by spaces becomes
        // its own token, which is the case that slipped past prefix_matches.
        for raw in [
            "git status\nrm -rf /tmp/x",
            "git status \nrm -rf /tmp/x",
            "git status\n rm -rf /tmp/x",
            "git status \n rm -rf /tmp/x",
            "git status\rrm -rf /tmp/x",
            "git status \r\n rm -rf /tmp/x",
        ] {
            assert!(
                !rule.matches("bash", raw),
                "raw chained command must be gated: {raw:?}"
            );

            // KEEP IN SYNC with `conway_runtime::tools::runner::
            // sanitize_rendered`. This is a hand-copy, not a call: crate
            // layering forbids `conway-core` from depending on
            // `conway-runtime`, so the real function is unreachable here.
            // If its replacement character or its predicate ever changes,
            // update `SANITIZED_CONTROL_PLACEHOLDER` above and this copy
            // together, or the gate silently stops covering the real
            // sanitizer's output. The end-to-end test that CANNOT drift is
            // `a_newline_chained_command_still_reaches_the_operator_through_
            // the_real_render_seam` in `conway/tests/permission_pattern_seam.rs`,
            // which runs the genuine sanitizer in the genuine pipeline.
            let sanitized: String = raw
                .chars()
                .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
                .collect();
            assert!(
                !rule.matches("bash", &sanitized),
                "SANITIZED chained command must be gated too -- the control \
                 char was rewritten, but the command is no less chained: \
                 {raw:?} -> {sanitized:?}"
            );
        }

        // The benign command still matches, so the hardening did not simply
        // break pattern grants outright.
        assert!(
            rule.matches("bash", "git status --short"),
            "an ordinary granted command must still match"
        );
    }

    /// The gate applies to the wildcard rule too -- "any bash call" must
    /// not become "any bash call, including chained ones".
    #[test]
    fn the_wildcard_rule_is_gated_by_metacharacters_as_well() {
        let rule = PatternRule::parse("bash:*").expect("valid rule");
        assert!(rule.matches("bash", "ls -la"));
        assert!(
            !rule.matches("bash", "ls -la && rm -rf /"),
            "the wildcard must not bypass the metacharacter gate"
        );
    }

    #[test]
    fn metacharacter_detection_covers_the_documented_set() {
        for c in [";", "&", "|", "`", "$", "<", ">", "(", ")", "{", "}"] {
            assert!(
                contains_shell_metacharacters(&format!("echo hi{c}")),
                "{c:?} must be treated as a metacharacter"
            );
        }
        assert!(contains_shell_metacharacters("echo hi\nrm -rf /"));
        assert!(!contains_shell_metacharacters("git status --short"));
    }

    // ---- RenderKind (board item 01KYT3NSWRHMPEAXVXRJ73KDYR) ----

    /// **The headline fix.** A `Structured` tool's JSON-dump rendering
    /// (the trait's own default `render`) carries `(){}`s that would trip
    /// the metacharacter gate on sight -- but for a `Structured` tool that
    /// text is never handed to a shell, so the gate must not apply, and a
    /// `read:*`-shaped wildcard must actually grant.
    #[test]
    fn a_structured_tools_wildcard_matches_its_own_json_dump_rendering() {
        let rule = PatternRule::parse("read:*").expect("valid rule");
        let rendered = r#"read({"path":"src/main.rs"})"#;

        assert!(
            !rule.matches(rule.tool.as_str(), rendered),
            "sanity: the CONSERVATIVE `matches` (no RenderKind) must still gate this -- \
             proves the fix is specifically about `matches_render`, not a weakening of \
             `matches` itself"
        );
        assert!(
            rule.matches_render(rule.tool.as_str(), rendered, RenderKind::Structured),
            "a `Structured` tool's own JSON-dump rendering must be granted by its wildcard -- \
             its parens and braces are JSON syntax here, not shell metacharacters"
        );
    }

    /// The prefix form (not just `*`) also grants for a `Structured` tool
    /// once its metacharacter-carrying rendering is no longer gated --
    /// still token-wise, exactly as it is for `bash`. A string ARGUMENT
    /// containing a literal space (e.g. `report`'s free-form `summary`)
    /// gives the JSON dump real whitespace token boundaries to prefix
    /// against, even though the surrounding `{}`/`:`/`,` never split a
    /// token by themselves.
    #[test]
    fn a_structured_tools_prefix_rule_matches_token_wise_despite_json_syntax() {
        let rule =
            PatternRule::parse(r#"report:report({"summary":"build"#).expect("valid rule");

        assert!(
            rule.matches_render(
                "report",
                r#"report({"summary":"build finished ok"})"#,
                RenderKind::Structured,
            ),
            "a summary that starts with the granted prefix must match"
        );
        assert!(
            !rule.matches_render(
                "report",
                r#"report({"summary":"tests failed"})"#,
                RenderKind::Structured,
            ),
            "a summary starting differently must not match -- prefix matching still \
             requires an actual token-wise prefix, RenderKind only changed whether the \
             metacharacter gate runs first"
        );
    }

    /// `ShellCommand` is the behavior every existing gate test above already
    /// pins via the conservative `matches` -- this test only makes explicit
    /// that `matches_render(.., ShellCommand)` agrees with `matches` (which
    /// is defined IN TERMS OF `matches_render(.., ShellCommand)`), so the
    /// two can never silently drift.
    #[test]
    fn shell_command_render_kind_behaves_identically_to_the_conservative_matches() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");
        for rendered in ["git status", "git status --short", "git status && rm -rf /"] {
            assert_eq!(
                rule.matches("bash", rendered),
                rule.matches_render("bash", rendered, RenderKind::ShellCommand),
                "matches() and matches_render(.., ShellCommand) must never disagree: {rendered:?}"
            );
        }
    }

    /// `Structured` does NOT disable prefix matching itself -- only the
    /// metacharacter gate. A `Structured` tool's wildcard still only
    /// matches ITS OWN tool name, and a non-wildcard prefix still requires
    /// an actual token-wise prefix match.
    #[test]
    fn structured_render_kind_only_widens_the_gate_not_the_matching_rules() {
        let rule = PatternRule::parse("read:*").expect("valid rule");
        assert!(!rule.matches_render(
            "write",
            r#"write({"path":"a"})"#,
            RenderKind::Structured,
        ));

        let prefix_rule = PatternRule::parse("read:read(specific)").expect("valid rule");
        assert!(!prefix_rule.matches_render(
            "read",
            r#"read({"path":"other.rs"})"#,
            RenderKind::Structured,
        ));
    }

    // ---- adversarial prefix cases ----

    /// A grant for `git status` must not permit a different subcommand,
    /// however similar.
    #[test]
    fn a_prefix_grant_does_not_permit_a_different_subcommand() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");

        assert!(rule.matches("bash", "git status"));
        assert!(rule.matches("bash", "git status --short"));

        assert!(
            !rule.matches("bash", "git push --force"),
            "a different subcommand must not be covered"
        );
        assert!(
            !rule.matches("bash", "git stat"),
            "a shorter token must not be covered"
        );
    }

    /// Token-wise, not byte-wise: `git status` must not match
    /// `git statusfoo`, even though that string does start with it.
    #[test]
    fn prefix_matching_respects_token_boundaries_not_raw_bytes() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");
        assert!(
            !rule.matches("bash", "git statusfoo"),
            "a byte-prefix match would wrongly allow this"
        );
        assert!(
            !rule.matches("bash", "gitstatus"),
            "token boundaries must be respected on the first token too"
        );
    }

    #[test]
    fn a_rule_never_matches_a_different_tool() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");
        assert!(!rule.matches("edit", "git status"));
    }

    // ---- parsing ----

    #[test]
    fn parse_accepts_the_wire_form_and_round_trips() {
        let rule = PatternRule::parse("bash:git status").expect("valid");
        assert_eq!(rule.tool, "bash");
        assert_eq!(rule.command_prefix, "git status");
        assert_eq!(rule.to_wire(), "bash:git status");
    }

    /// A prefix may contain `:` -- only the first one separates.
    #[test]
    fn parse_splits_on_the_first_colon_only() {
        let rule = PatternRule::parse("bash:ssh host:/path").expect("valid");
        assert_eq!(rule.tool, "bash");
        assert_eq!(rule.command_prefix, "ssh host:/path");
    }

    /// A malformed rule is DROPPED, not guessed at. A half-understood
    /// permission rule is worse than a missing one.
    #[test]
    fn parse_rejects_malformed_rules_rather_than_guessing() {
        for bad in ["", "bash", ":", "bash:", ":git status", "   :   "] {
            assert!(
                PatternRule::parse(bad).is_none(),
                "malformed rule must be dropped: {bad:?}"
            );
        }
    }

    #[test]
    fn describe_is_readable_enough_to_evaluate_before_granting() {
        let rule = PatternRule::parse("bash:git status").expect("valid");
        assert_eq!(
            rule.describe(),
            "`bash` commands starting with `git status`"
        );
        let wildcard = PatternRule::parse("read:*").expect("valid");
        assert_eq!(wildcard.describe(), "any `read` call");
    }
}

/// The pattern Conway OFFERS an operator for a given command (V2b).
///
/// Returns `None` when no sensible offer exists — an empty command, or one
/// carrying shell metacharacters (offering a grant that the metacharacter
/// gate would then refuse to honor would be actively confusing).
///
/// ## Why two tokens
///
/// The offer is deliberately narrow. `git status` is a useful grant;
/// `git` alone would silently include `git push --force`, and an operator
/// skimming a prompt could easily accept the latter believing they got the
/// former. Two tokens captures the near-universal `<command> <subcommand>`
/// shape (`git status`, `cargo test`, `npm run`) without reaching past it.
///
/// A single-token command (`ls`, `pwd`) offers just that token, since
/// there is no subcommand to bound it with.
///
/// An operator who wants something broader can add it to
/// `permissions.json` by hand, having thought about it. That asymmetry is
/// the point: granting more should take deliberate effort, granting less
/// should be the default. You can always grant again; you cannot
/// un-authorize what already ran.
pub fn suggested_rule(tool: &str, rendered: &str) -> Option<PatternRule> {
    if contains_shell_metacharacters(rendered) {
        return None;
    }
    let tokens: Vec<&str> = rendered.split_whitespace().take(2).collect();
    if tokens.is_empty() {
        return None;
    }
    Some(PatternRule {
        tool: tool.to_string(),
        command_prefix: tokens.join(" "),
    })
}

/// The on-disk shape of `.conway/permissions.json`.
///
/// Deliberately a flat list of wire-form strings (`"bash:git status"`)
/// rather than a nested structure: the file is meant to be read and edited
/// by a human reviewing what they have authorized, and diffed in a pull
/// request. A structure that needs a schema reference to interpret would
/// undercut that.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionFile {
    /// Wire-form rules. Malformed entries are dropped on load, not
    /// guessed at.
    #[serde(default)]
    pub allow: Vec<String>,
}

/// Parses a rules file's contents into rules, **failing closed**.
///
/// Every failure mode returns fewer rules, never more:
/// - unparseable JSON → no rules at all (the operator is asked about
///   everything, which is Conway's default behavior anyway)
/// - a malformed entry → that entry dropped, the rest kept
///
/// This asymmetry is the whole point. A corrupt permissions file must
/// never be able to *widen* what is authorized — the worst outcome of a
/// bad file is extra prompting, never a missed one.
pub fn parse_rules(contents: &str) -> Vec<PatternRule> {
    let file: PermissionFile = match serde_json::from_str(contents) {
        Ok(file) => file,
        // Fail closed: an unreadable file authorizes nothing.
        Err(_) => return Vec::new(),
    };
    file.allow
        .iter()
        .filter_map(|raw| PatternRule::parse(raw))
        .collect()
}

#[cfg(test)]
mod store_tests {
    use super::*;

    #[test]
    fn a_valid_file_round_trips() {
        let contents = r#"{"allow": ["bash:git status", "read:*"]}"#;
        let rules = parse_rules(contents);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].to_wire(), "bash:git status");
        assert_eq!(rules[1].to_wire(), "read:*");
    }

    /// P-10, with the bias that matters here: a corrupt file must fail
    /// CLOSED. The worst outcome of an unreadable permissions file is
    /// extra prompting -- never a missed prompt.
    #[test]
    fn a_corrupt_file_authorizes_nothing_rather_than_everything() {
        for corrupt in [
            "",
            "not json at all",
            "{",
            r#"{"allow": "not-an-array"}"#,
            r#"{"allow": [123]}"#,
            "null",
        ] {
            assert!(
                parse_rules(corrupt).is_empty(),
                "a corrupt file must authorize nothing: {corrupt:?}"
            );
        }
    }

    /// One bad entry does not discard the whole file, but it is dropped
    /// rather than guessed at.
    #[test]
    fn a_malformed_entry_is_dropped_and_the_rest_kept() {
        let contents = r#"{"allow": ["bash:git status", "malformed-no-colon", "read:*"]}"#;
        let rules = parse_rules(contents);
        assert_eq!(rules.len(), 2, "the two valid rules survive");
        assert!(rules.iter().all(|r| r.to_wire() != "malformed-no-colon"));
    }

    #[test]
    fn a_missing_allow_key_is_an_empty_rule_set_not_an_error() {
        assert!(parse_rules("{}").is_empty());
    }
    // ---- V2b: the offered rule ----

    /// The offer is two tokens: enough for `<command> <subcommand>`, not
    /// enough to silently include a sibling subcommand.
    #[test]
    fn the_suggested_rule_is_narrow_by_default() {
        let rule = suggested_rule("bash", "git status --short").expect("offered");
        assert_eq!(rule.command_prefix, "git status");

        // Crucially: the offered grant does NOT cover a different
        // subcommand. An operator accepting this prompt has not
        // accidentally authorized `git push`.
        assert!(rule.matches("bash", "git status --short"));
        assert!(!rule.matches("bash", "git push --force"));
    }

    #[test]
    fn a_single_token_command_offers_just_that_token() {
        let rule = suggested_rule("bash", "pwd").expect("offered");
        assert_eq!(rule.command_prefix, "pwd");
    }

    /// Offering a grant the metacharacter gate would then refuse to honor
    /// would be confusing, so no offer is made at all.
    #[test]
    fn no_rule_is_offered_for_a_command_the_gate_would_reject() {
        assert!(suggested_rule("bash", "git status && rm -rf /").is_none());
        assert!(suggested_rule("bash", "").is_none());
    }

}
