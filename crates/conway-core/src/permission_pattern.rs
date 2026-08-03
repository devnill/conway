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
//!
//! ## The `allow`/`deny` asymmetry (board item 01KYT8SGX32CP56PRJNG72V2W5)
//!
//! [`PermissionFile`] has two halves, and they are deliberately NOT
//! symmetric. `allow` is authority: granting it to a project file that a
//! cloned repository controls, with no consent, is a live fail-open
//! security gap (`.design/d4-trust-model.md` §1, §11) -- so an
//! `allow` rule loaded from a project file only takes effect once the
//! CALLER (`conway`'s facade, `conway-cli`'s startup loader) has confirmed
//! an explicit, recorded trust decision for that exact file's bytes. This
//! module has no idea whether the caller did that -- it is not this
//! module's job to gate on trust, only to make the vocabulary expressive
//! enough that a caller CAN.
//!
//! `deny` is the opposite: a rule that only ever narrows what is
//! authorized has no failure mode worth gating (the worst case is an extra
//! prompt), so it applies immediately, from any file, trusted or not.
//! [`PatternRule::matches_deny`] is the deliberately UNGATED sibling of
//! [`PatternRule::matches_render`] -- it does not consult
//! [`contains_shell_metacharacters`] at all, for any [`RenderKind`],
//! because inverted the gate would defeat the very rule it is supposed to
//! protect: `deny bash:curl` gated the same way `allow` is would let
//! `curl x; y` slip past simply for carrying a `;`. Composition is
//! most-restrictive-wins: a deny beats every allow, independent of
//! authorship or order.
//!
//! **The honest limit, stated rather than papered over:** prefix matching
//! is not a containment boundary in either direction. `deny bash:git push`
//! does not catch `foo; git push`. What makes the composition sound anyway
//! is `allow`'s OWN gate: a command carrying a metacharacter can never be
//! auto-allowed regardless of what patterns exist, so the chained form
//! always reaches the human operator -- `deny` is a seatbelt for the
//! obvious case, not a boundary. Anything that must never happen belongs
//! in the confinement root, not in a `deny` prefix.
//!
//! ## Sanitizer laundering was a second, DIFFERENT hole (board item
//! 01KYTMA306JH81R083Y8K9PWCR)
//!
//! "Ungated" was correctly not the same thing as "immune to the sanitizer
//! that runs upstream of it." `rendered` reaches `matches_deny` already
//! passed through `conway_core::text::sanitize_control_chars` (the shared
//! replace-semantics sanitizer the runtime's `rendered` seam also calls),
//! which rewrites every control character to
//! [`SANITIZED_CONTROL_PLACEHOLDER`] -- and a leading tab (also a control
//! character) is invisible to every POSIX shell, so `\tcurl http://evil`
//! runs identically to `curl http://evil` while its SANITIZED form,
//! `\u{FFFD}curl http://evil`, tokenizes as one fused token that a bare
//! `prefix_matches` cannot align with a `curl` prefix. This is not the
//! documented chaining limit above (the command is not being EXTENDED past
//! what the rule names) -- it is evidence a correct comparison depends on
//! being destroyed before the comparison runs. [`PatternRule::
//! matches_deny`]'s own doc has the fix and the reasoning for why it does
//! not also widen to the documented limit.
//!
//! **Two decisions made explicit, per this item's own instruction to state
//! them rather than leave them implicit:**
//!
//! - **`rendered`, not `arguments`.** `.design/extension-architecture.md`
//!   §5.3 warns that `rendered` is sanitized and lossy and must not be the
//!   basis of a security decision -- which is exactly what motivated
//!   [`crate::containment`]'s root check (`conway_runtime::permission::
//!   PermissionBroker::check_root`) to read `arguments` instead. `deny`
//!   stays on `rendered` anyway: `arguments` is an opaque
//!   tool-specific JSON value, and "the command string" is only
//!   extractable from it via per-tool knowledge `matches_deny` has no
//!   access to (unlike `check_root`, which is handed a declared, named
//!   list of path arguments via `PathArgs`). `rendered` is the one place
//!   every tool already agrees on producing a single comparable string --
//!   it is what makes `deny bash:curl` meaningful and tool-agnostic at
//!   all. The fix above is the alternative to abandoning `rendered`:
//!   instead of trusting it blindly, `matches_deny` now recognizes the ONE
//!   way it lies (control-character laundering) and fails toward the deny
//!   when it sees that evidence, rather than trusting the string's
//!   tokenization at face value.
//! - **`AutoAllow` already consults `deny` (`PermissionBroker::decide`
//!   checks it before the mode branch), and that is correct as designed --
//!   not an oversight this item needed to add.** The gap this item closes
//!   is narrower: a deny rule that was MISSED (via laundering) had nothing
//!   behind it in `AutoAllow`, because that mode's entire point is to skip
//!   the operator's gate. `AutoAllow` is the mode where a `deny` rule is
//!   the LAST remaining guardrail -- there is no human on the other end of
//!   a miss to catch it -- which is precisely why closing the laundering
//!   gap matters most there, even though the fix itself lives in
//!   `matches_deny` and benefits every mode equally.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::content::ToolCategory;
use crate::ports::RenderKind;
#[cfg(test)]
use crate::text::sanitize_control_chars;
use crate::text::SANITIZED_CONTROL_PLACEHOLDER;

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
///
/// The placeholder must be disqualifying specifically because `rendered` is
/// sanitized for display safety BEFORE it reaches [`PatternRule::matches`]:
/// that sanitization (now `conway_core::text::sanitize_control_chars`, the
/// shared home this gate and the runtime's `rendered` seam both call)
/// rewrites `\n`/`\r` -- two of the [`SHELL_METACHARACTERS`] above -- into
/// [`SANITIZED_CONTROL_PLACEHOLDER`]. Without this gate treating the
/// placeholder as disqualifying, `git status \n rm -rf /` would arrive as
/// `git status <U+FFFD> rm -rf /`: the newline evidence destroyed, the gate
/// satisfied, and the replacement char consumed as its own
/// whitespace-delimited token by [`prefix_matches`] -- silently
/// auto-approving a chained command under a `bash:git status` grant. The
/// constant lives in `conway_core::text` so the sanitizer and this gate
/// literally share one source of truth.
pub fn contains_shell_metacharacters(command: &str) -> bool {
    command.chars().any(|c| {
        SHELL_METACHARACTERS.contains(&c) || c.is_control() || c == SANITIZED_CONTROL_PLACEHOLDER
    })
}

/// Whether `command` carries evidence that its own tokenization cannot be
/// trusted: a raw control character, or [`SANITIZED_CONTROL_PLACEHOLDER`]
/// (what a control character becomes once the real sanitizer has run).
///
/// This is the narrower half of [`contains_shell_metacharacters`] --
/// deliberately excluding [`SHELL_METACHARACTERS`] itself. See
/// [`PatternRule::matches_deny`]'s own doc for why: the two callers of this
/// predicate need different things from it. `matches_deny` needs "was
/// something erased that would have changed where the tokens fall", which
/// is exactly what a control character (raw or laundered into the
/// placeholder) means; it must NOT mean "does this contain `;`", because
/// `;` doesn't erase anything -- `prefix_matches` sees it exactly as
/// written, and a command it doesn't align with (`foo; git push` against a
/// `git push` prefix) is the module's own documented, accepted
/// prefix-match limit, not a bug this predicate exists to paper over.
fn rendered_evidence_is_untrustworthy(command: &str) -> bool {
    command
        .chars()
        .any(|c| c.is_control() || c == SANITIZED_CONTROL_PLACEHOLDER)
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

    /// Whether this rule, used as a `deny` rule, refuses `tool` running
    /// `rendered` -- prefix comparison only, deliberately WITHOUT
    /// [`Self::matches_render`]'s metacharacter gate, and deliberately
    /// with no [`RenderKind`] parameter at all: a deny rule's whole job is
    /// to be hard to evade, so it must match identically regardless of
    /// what the matched tool's rendering happens to look like. See this
    /// module's own doc for why gating a `deny` prefix would defeat it.
    ///
    /// ## Sanitizer laundering (board item 01KYTMA306JH81R083Y8K9PWCR)
    ///
    /// Omitting the metacharacter gate is not the same as trusting
    /// `prefix_matches` to tokenize `rendered` correctly no matter what is
    /// in it. A leading tab/newline/CR/escape is invisible to every POSIX
    /// shell -- `\tcurl http://evil` runs exactly like `curl http://evil`
    /// -- but by the time `rendered` reaches this method it has already
    /// been through `conway_core::text::sanitize_control_chars` (the
    /// shared sanitizer the runtime's `rendered` seam calls),
    /// which rewrites every control character to
    /// [`SANITIZED_CONTROL_PLACEHOLDER`]. That placeholder is not Unicode
    /// whitespace, so it fuses onto (`\tcurl` -> `\u{FFFD}curl`, one
    /// token) or displaces (a leading escape's printable remainder, e.g.
    /// `[0m`, becomes its OWN leading token) the very token
    /// `prefix_matches` compares first -- silently defeating a deny rule
    /// that would otherwise have matched. This is not a chaining
    /// evasion (the command is not extended past what the rule names);
    /// it is the evidence a correct comparison needs being destroyed
    /// upstream, so a naive prefix comparison sees nothing wrong.
    ///
    /// [`rendered_evidence_is_untrustworthy`] catches this: `rendered`
    /// carrying a raw control character (in case a caller ever hands this
    /// method an unsanitized string directly) or the sanitizer's
    /// placeholder is treated as MATCHING any deny rule for this tool,
    /// rather than as failing to match -- fail TOWARD the deny, never
    /// away from it. This is deliberately narrower than
    /// [`contains_shell_metacharacters`]: it does NOT fire on
    /// [`SHELL_METACHARACTERS`] (`;`, `&`, `|`, ...), which are real,
    /// visible shell syntax `prefix_matches` already reads correctly.
    /// Firing on those too would silently "fix" -- i.e. narrow away --
    /// this module's own DOCUMENTED prefix-match limit (`deny bash:git
    /// push` not catching `foo; git push`, this module's own doc, "a
    /// seatbelt, not a boundary"), which this item deliberately leaves
    /// alone.
    pub fn matches_deny(&self, tool: &str, rendered: &str) -> bool {
        if self.tool != tool {
            return false;
        }
        if self.command_prefix == "*" {
            return true;
        }
        if rendered_evidence_is_untrustworthy(rendered) {
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

// =====================================================================
// F12: the structured rule form -- `Rule { select, when, then }`.
//
// The flat string form (`"bash:git status"`, parsed by [`PatternRule::parse`])
// is the SURFACE SYNTAX for this structured form. Both parse into one
// internal [`Rule`], evaluated by one evaluator: [`PatternRule::to_rule`]
// desugars a flat rule into a [`Rule`] whose `when` is [`When::Always`] (for
// the `tool:*` wildcard) or [`When::CommandPrefix`] (for a real prefix), and
// [`Rule::matches_allow_render`]/[`Rule::matches_deny_render`] below are the
// single evaluator path every admission takes. See this module's own doc and
// `.design/extension-architecture.md` §5 for why there is one language, not
// two.
//
// The structured form is an ADDITIVE SUPERSET: it can express what the flat
// form can (`tools([t]) + command_prefix(p)`), plus what the flat form cannot
// (`paths_under` for resolved-path containment, `categories` for
// category-scoped rules, `category_in` for a category condition). The flat
// form stays the ergonomic default and keeps working forever.
// =====================================================================

/// What a [`Rule`] selects: which tool calls it can apply to at all.
///
/// `Tools` matches by tool name, with one limited wildcard: a single trailing
/// `*` (`"bash"`, `"re*"`, `"*"`). There is no other wildcard -- the same
/// "predictable by reading" property [`PatternRule`]'s prefix language has.
/// `Categories` matches by [`ToolCategory`], so a plugin can scope a rule to
/// "every Edit/Delete tool" without naming them.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Select {
    /// Match calls to any of these tool names. A pattern is either an exact
    /// name (`"bash"`) or a single trailing `*` (`"re*"`, `"*"`); no other
    /// wildcard is recognized, so `*` is the only metacharacter and it can
    /// only appear once, at the end.
    Tools(Vec<String>),
    /// Match calls to any tool whose declared [`ToolCategory`] is in this
    /// list.
    Categories(Vec<ToolCategory>),
}

/// The condition under which a selected call matches a [`Rule`]. See
/// [`Rule`] for how `select` + `when` + `then` compose.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// No condition beyond `select`: every call the select matches is
    /// matched. The flat `tool:*` wildcard desugars to `Tools([tool]) +
    /// Always`. The allow-side metacharacter gate still applies (see
    /// [`Rule::matches_allow_render`]), so `Always` on a `ShellCommand` tool
    /// does NOT authorize a chained command -- it is "any invocation, the
    /// gate still gating", not "any invocation, including chained ones".
    Always,
    /// Today's [`PatternRule`] semantics: a token-wise prefix over the call's
    /// `rendered` string. A registration error against a tool whose
    /// `render_kind` is [`RenderKind::Structured`] (see
    /// [`RuleRegistrationReason::CommandPrefixOnStructuredTool`]) -- matching
    /// a JSON dump by prefix is fragile (key order, spacing, escaping) and
    /// the operator will not notice a rule that never matches.
    CommandPrefix(String),
    /// Resolved-path containment: the call's declared path arguments (per
    /// the tool's `Tool::path_args` declaration) are resolved exactly as
    /// `conway_runtime::permission::PermissionBroker::check_root` resolves
    /// them -- via `resolve_like_the_tool_will`, from `arguments`, never
    /// from `rendered` -- and every one must be contained under `prefix`
    /// (canonicalized once at install). An `Unconfinable` tool NEVER
    /// satisfies this (fail closed, the same asymmetry root confinement
    /// uses); a tool with `PathArgs::None` never satisfies it either (no
    /// paths to confine). The allow-side metacharacter gate still applies
    /// for a `ShellCommand` tool.
    PathsUnder(String),
    /// A category condition: the call's declared [`ToolCategory`] must be
    /// in this list. Composes with `select`: a `Tools(["bash"]) + CategoryIn
    /// ([Execute])` rule matches a `bash` call only when bash's category is
    /// `Execute` (it is), which is redundant for `bash` but useful for
    /// third-party tools whose name you select but whose category you also
    /// want to pin.
    CategoryIn(Vec<ToolCategory>),
}

/// The effect a [`Rule`] has when its `select` + `when` both match.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Then {
    /// Authorize the call without consulting the operator's gate (subject to
    /// the allow-side metacharacter gate and root confinement). An `allow`
    /// rule is a durable grant; grants belong to the operator, so only
    /// operator-owned config (the trusted permissions file, or an interactive
    /// "always allow") may author one. Plugin-contributed rules may only be
    /// [`Then::Deny`] or [`Then::Prompt`] (narrowing); this invariant is
    /// encoded at admission -- extension-architecture.md §5.5 stage 1 -- so a
    /// future plugin transport cannot retrofit an `allow` without crossing
    /// it.
    Allow,
    /// Force the call to the operator's gate even in `AutoAllow` mode and
    /// even over a matching `allow` grant (most-restrictive-wins). Narrows;
    /// admits unconditionally.
    Prompt,
    /// Refuse the call outright, before any allow path is consulted. Beats
    /// every `prompt` and every `allow` (most-restrictive-wins). Narrows;
    /// admits unconditionally.
    Deny,
}

/// One structured permission rule: a [`Select`], a [`When`], and a [`Then`].
///
/// The flat string `"bash:cargo test"` IS `Rule { select: Tools(["bash"]),
/// when: CommandPrefix("cargo test"), then: Allow }` -- see
/// [`PatternRule::to_rule`] for the desugaring. The two forms parse into this
/// one type and are evaluated by [`Rule::matches_allow_render`] /
/// [`Rule::matches_deny_render`] (for the render-based `when` clauses) plus
/// the broker's resolved-path check (for [`When::PathsUnder`], which needs
/// the call's `arguments` and `cwd` and so lives where `check_root` lives).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub select: Select,
    pub when: When,
    pub then: Then,
}

impl Rule {
    /// Whether `select` matches this call's `(tool, category)`. Shared by the
    /// allow and deny/prompt evaluators, and by the broker's `paths_under`
    /// path, so select semantics cannot drift between them.
    pub fn select_matches(&self, tool: &str, category: ToolCategory) -> bool {
        match &self.select {
            Select::Tools(patterns) => patterns.iter().any(|p| tool_pattern_matches(p, tool)),
            Select::Categories(cats) => cats.contains(&category),
        }
    }

    /// The allow-side metacharacter gate, as an associated fn so the broker's
    /// `paths_under` path applies it identically to the render-based path.
    /// Unchanged in behavior from [`PatternRule::matches_render`]: a
    /// `ShellCommand` rendering carrying any metacharacter (or any control
    /// character, or the sanitizer's placeholder) is refused; a `Structured`
    /// rendering is never gated here.
    pub fn gate_allows(rendered: &str, render_kind: RenderKind) -> bool {
        !(render_kind == RenderKind::ShellCommand && contains_shell_metacharacters(rendered))
    }

    /// Whether this rule, as an ALLOW rule, would authorize `(tool, category)`
    /// running `rendered` under `render_kind` -- for the render-based `when`
    /// clauses (`Always`, `CommandPrefix`, `CategoryIn`). Returns `false` for
    /// [`When::PathsUnder`]: that clause needs the call's `arguments` and
    /// `cwd` (resolved exactly as `check_root` resolves them), which only the
    /// broker has, so the broker evaluates `PathsUnder` itself and reaches
    /// this method only for the other clauses.
    ///
    /// This is the single evaluator the flat and structured forms share:
    /// `PatternRule::to_rule(Then::Allow).matches_allow_render(...)` is
    /// byte-identical to `PatternRule::matches_render(...)` (pinned by the
    /// byte-identical equivalence test in this module).
    pub fn matches_allow_render(
        &self,
        tool: &str,
        category: ToolCategory,
        rendered: &str,
        render_kind: RenderKind,
    ) -> bool {
        if !self.select_matches(tool, category) {
            return false;
        }
        // The hard gate, unchanged from `PatternRule::matches_render`: allow
        // keeps it; deny skips it. Applied before any `when` predicate, and
        // for every `when` (not just `CommandPrefix`) -- `Always` on a
        // `ShellCommand` tool must NOT authorize a chained command.
        if !Self::gate_allows(rendered, render_kind) {
            return false;
        }
        match &self.when {
            When::Always => true,
            When::CommandPrefix(p) => prefix_matches(p, rendered),
            When::CategoryIn(cats) => cats.contains(&category),
            When::PathsUnder(_) => false,
        }
    }

    /// Whether this rule, as a DENY (or PROMPT) rule, matches `(tool,
    /// category)` running `rendered` -- deliberately WITHOUT the
    /// metacharacter gate (the deny/prompt asymmetry: a `;` must not defeat a
    /// deny/prompt the way it would defeat an allow), and with the
    /// laundering fallback for `CommandPrefix` (see
    /// [`PatternRule::matches_deny`]'s own doc). Returns `false` for
    /// [`When::PathsUnder`]: the broker evaluates that clause itself.
    pub fn matches_deny_render(&self, tool: &str, category: ToolCategory, rendered: &str) -> bool {
        if !self.select_matches(tool, category) {
            return false;
        }
        match &self.when {
            When::Always => true,
            When::CommandPrefix(p) => {
                if rendered_evidence_is_untrustworthy(rendered) {
                    return true;
                }
                prefix_matches(p, rendered)
            }
            When::CategoryIn(cats) => cats.contains(&category),
            When::PathsUnder(_) => false,
        }
    }

    /// The flat wire form, for rules that are the desugarable subset
    /// (`Tools([t]) + (Always | CommandPrefix(p))`). Returns `None` for
    /// anything the flat language cannot express (`Categories`,
    /// `PathsUnder`, `CategoryIn`, multiple tools), so a caller matching
    /// against the flat `allow`/`deny` lists in a permissions file can tell
    /// the two apart. This is the bridge that keeps [`PatternRule`]-shaped
    /// review surfaces working unchanged for flat rules while structured
    /// rules live alongside them.
    pub fn to_pattern_rule(&self) -> Option<PatternRule> {
        let tool = match &self.select {
            Select::Tools(ts) if ts.len() == 1 => ts[0].as_str(),
            _ => return None,
        };
        match &self.when {
            When::Always => Some(PatternRule {
                tool: tool.to_string(),
                command_prefix: "*".to_string(),
            }),
            When::CommandPrefix(p) => Some(PatternRule {
                tool: tool.to_string(),
                command_prefix: p.clone(),
            }),
            _ => None,
        }
    }

    /// A human-readable description of what this rule matches, for the review
    /// surface. The desugarable subset reproduces [`PatternRule::describe`]
    /// verbatim, so a flat rule and its structured equivalent render
    /// identically to the operator.
    pub fn describe(&self) -> String {
        let select_label = match &self.select {
            Select::Tools(ts) if ts.len() == 1 => ts[0].clone(),
            Select::Tools(ts) => format!("[{}]", ts.join(", ")),
            Select::Categories(cats) => format!(
                "categories [{}]",
                cats.iter()
                    .map(|c| format!("{c:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        match (&self.select, &self.when) {
            (Select::Tools(ts), When::Always) if ts.len() == 1 => format!("any `{}` call", ts[0]),
            (Select::Tools(ts), When::CommandPrefix(p)) if ts.len() == 1 => {
                format!("`{}` commands starting with `{}`", ts[0], p)
            }
            (_, When::Always) => format!("{select_label} (any call)"),
            (_, When::CommandPrefix(p)) => {
                format!("{select_label} commands starting with `{p}`")
            }
            (_, When::PathsUnder(prefix)) => format!("{select_label} under `{prefix}`"),
            (_, When::CategoryIn(cats)) => format!(
                "{select_label} in categories [{}]",
                cats.iter()
                    .map(|c| format!("{c:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// The flat wire form for desugarable rules, or a structured JSON
    /// serialization otherwise. Used by review surfaces that need a stable
    /// string identity for a rule; the flat `allow`/`deny` list
    /// read-modify-write in `Conway::rewrite_permission_file_removing`
    /// matches against this (structured rules are not in that flat list, so
    /// this is only load-bearing for the desugarable subset).
    pub fn to_wire(&self) -> String {
        match (&self.select, &self.when) {
            (Select::Tools(ts), When::Always) if ts.len() == 1 => format!("{}:*", ts[0]),
            (Select::Tools(ts), When::CommandPrefix(p)) if ts.len() == 1 => {
                format!("{}:{}", ts[0], p)
            }
            _ => serde_json::to_string(self).unwrap_or_else(|_| "<unserializable rule>".into()),
        }
    }
}

/// Whether a `Select::Tools` pattern matches a tool name. A pattern is exact,
/// or ends with a single `*` (then its prefix must match); no other wildcard.
fn tool_pattern_matches(pattern: &str, tool: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        // A trailing `*` is a prefix match. An embedded `*` (not trailing) is
        // NOT a wildcard -- it is treated literally, so the operator cannot
        // accidentally grant more than they read.
        !pattern[..pattern.len() - 1].contains('*') && tool.starts_with(prefix)
    } else {
        pattern == tool
    }
}

/// A typed registration error for a [`Rule`] loaded from config, surfaced to
/// the operator (P-10: untrusted input -> typed errors, never panics). A rule
/// that can never match is a lie the operator will not notice (the mirror of
/// the `read:*`-matched-nothing bug fixed in `68ea9b1`); this type is how the
/// loader refuses to install one silently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleRegistrationError {
    /// The rule as parsed. Carried whole so the operator can see exactly what
    /// was rejected, not just a reason.
    pub rule: Rule,
    /// Why this rule cannot be installed.
    pub reason: RuleRegistrationReason,
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleRegistrationReason {
    /// `when: command_prefix` was paired with a tool whose `render_kind` is
    /// `Structured`. A `command_prefix` matches the call's `rendered` string
    /// token-wise; for a `Structured` tool that string is a JSON dump whose
    /// token boundaries depend on key order, spacing, and escaping the
    /// operator cannot predict, so the rule is fragile. Use `when: always`
    /// (the flat `tool:*` form) for "any invocation", or `when: paths_under`
    /// for path-scoped rules.
    CommandPrefixOnStructuredTool,
    /// `when: paths_under` was paired with a `then: deny` (or `prompt`) rule
    /// selecting a tool whose `PathArgs` is not `Named` -- i.e.
    /// `Unconfinable` (such as `bash`: a free-form shell command the broker
    /// cannot statically confine) or `None` (no path arguments at all). A
    /// `paths_under` predicate can never be satisfied for such a tool, so the
    /// rule is silently inert. For `Unconfinable` that inertness is fail-OPEN
    /// -- the command can still reach the prefix, so the call the operator
    /// expected to be refused instead goes through (P-13). For `None` the
    /// rule is a no-op rather than fail-open (the tool takes no paths, so it
    /// cannot reach the prefix), but the loader still refuses to install it
    /// silently so the operator learns the rule does nothing -- a no-op deny
    /// is a trap worth surfacing. In both cases the loader surfaces this
    /// error so the operator can rewrite the rule (e.g. scope it to the
    /// tool's `command_prefix`, or drop the unconfinable tool from the
    /// select). For `then: allow` the same inertness is fail-CLOSED (the
    /// broker simply never matches it and the call falls through to the
    /// operator's gate), so this is NOT raised for allow rules.
    PathsUnderOnUnconfinedTool,
}

impl RuleRegistrationReason {
    /// A human-readable explanation for the operator.
    pub fn describe(&self) -> &'static str {
        match self {
            RuleRegistrationReason::CommandPrefixOnStructuredTool => {
                "`command_prefix` cannot be used against a tool whose rendering is \
                 structured (a JSON dump); use `always` (the `tool:*` flat form) or \
                 `paths_under` instead"
            }
            RuleRegistrationReason::PathsUnderOnUnconfinedTool => {
                "`paths_under` cannot confine a tool whose path arguments are not \
                 statically declared (e.g. `bash`, whose `command` can reach anywhere); \
                 a `deny`/`prompt` rule selecting it would be silently inert -- fail \
                 open. Use `command_prefix` to scope the unconfinable tool, or drop it \
                 from the `select`"
            }
        }
    }
}

impl PatternRule {
    /// Desugars this flat-form rule into the structured [`Rule`] it IS, with
    /// the given [`Then`]. `command_prefix == "*"` (the flat wildcard, "any
    /// invocation") desugars to [`When::Always`]; a real prefix desugars to
    /// [`When::CommandPrefix`]. This is the single bridge between the two
    /// syntaxes: both produce a [`Rule`], and one evaluator decides both.
    pub fn to_rule(&self, then: Then) -> Rule {
        let when = if self.command_prefix == "*" {
            When::Always
        } else {
            When::CommandPrefix(self.command_prefix.clone())
        };
        Rule {
            select: Select::Tools(vec![self.tool.clone()]),
            when,
            then,
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

            // The real shared sanitizer -- `conway_core::text::
            // sanitize_control_chars`. This used to be a hand-copy kept in
            // sync with `conway_runtime::tools::runner::sanitize_rendered`
            // (crate layering forbade a call); the two now share the single
            // home in `conway_core::text`, so this can no longer drift. The
            // end-to-end test that drives the genuine pipeline seam is
            // `a_newline_chained_command_still_reaches_the_operator_through_
            // the_real_render_seam` in `conway/tests/permission_pattern_seam.rs`.
            let sanitized = sanitize_control_chars(raw);
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

    /// The sanitizer's placeholder is itself a metacharacter. This is the
    /// load-bearing property: a control character laundered into
    /// `SANITIZED_CONTROL_PLACEHOLDER` by `conway_core::text::
    /// sanitize_control_chars` must NOT be able to pass the gate, or the
    /// v0.5.0 `git status \n rm -rf /` chaining hole reopens. The constant
    /// now lives in `conway_core::text` and is re-used here precisely so
    /// this test pins the agreement the old "keep in sync" comment asserted.
    #[test]
    fn the_sanitizer_placeholder_is_treated_as_a_metacharacter() {
        // A string that is otherwise clean but carries the placeholder the
        // sanitizer produces for a rewritten control char.
        assert!(
            contains_shell_metacharacters("git status\u{FFFD} rm -rf /"),
            "the sanitizer's placeholder must be gated, or a laundered \
             control char slips past: {:?}",
            "git status\u{FFFD} rm -rf /"
        );
        // And specifically, a `bash:git status` grant must NOT authorize the
        // sanitized form of the chained command -- the end-to-end property
        // the gate + sanitizer agreement exists to guarantee.
        let rule = PatternRule::parse("bash:git status").expect("valid rule");
        assert!(
            !rule.matches("bash", "git status\u{FFFD} rm -rf /"),
            "a sanitized chained command must not be auto-allowed"
        );
        // Sanity: the placeholder constant is the same one the sanitizer
        // emits -- if this ever drifts, the assertions above would still
        // pass on a different char and stop protecting the real seam.
        assert_eq!(
            sanitize_control_chars("a\nb"),
            format!("a{SANITIZED_CONTROL_PLACEHOLDER}b")
        );
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

    // ---- deny: the asymmetric half (board item 01KYT8SGX32CP56PRJNG72V2W5) ----

    /// The headline property: a `deny` prefix rule still matches a chained
    /// command that carries a metacharacter -- unlike `matches`/
    /// `matches_render`, which would refuse to consider it at all. If deny
    /// gated the same way allow does, appending `;` to a command would
    /// DEFEAT the deny rule, which is backwards for a rule whose entire job
    /// is to be hard to evade.
    #[test]
    fn deny_matches_a_chained_command_that_allow_would_refuse_to_even_consider() {
        let rule = PatternRule::parse("bash:curl").expect("valid rule");

        assert!(rule.matches_deny("bash", "curl https://example.com"));
        assert!(
            rule.matches_deny("bash", "curl x; rm -rf /"),
            "a deny prefix must still catch a chained command carrying it"
        );
        // Contrast: the ALLOW-side gate refuses to authorize this at all --
        // proving the asymmetry is real, not just documented.
        assert!(!rule.matches("bash", "curl x; rm -rf /"));
    }

    /// Deny is independent of `RenderKind` entirely -- `matches_deny` takes
    /// no such parameter, so a `Structured` tool's JSON-dump rendering
    /// (whose own `(){}`s would trip the ALLOW-side gate on sight, exactly
    /// as `a_structured_tools_wildcard_matches_its_own_json_dump_rendering`
    /// pins for the allow side) is denied without hesitation.
    #[test]
    fn deny_matching_does_not_depend_on_render_kind() {
        let rule = PatternRule::parse("write:*").expect("valid rule");
        let rendered = r#"write({"path":"/etc/passwd"})"#;

        assert!(
            rule.matches_deny("write", rendered),
            "deny must refuse a Structured tool's own JSON-dump rendering, \
             with no RenderKind gate in the way at all"
        );
        // Contrast: the conservative ALLOW-side `matches` refuses to even
        // consider this rendering, because its own JSON syntax trips the
        // metacharacter gate `matches` always applies -- proving the
        // asymmetry is real, not just documented.
        assert!(!rule.matches("write", rendered));
    }

    /// The wildcard form works for deny the same way it does for allow.
    #[test]
    fn a_deny_wildcard_matches_any_call_to_the_tool() {
        let rule = PatternRule::parse("bash:*").expect("valid rule");
        assert!(rule.matches_deny("bash", "ls -la && rm -rf /"));
        assert!(!rule.matches_deny("edit", "anything"));
    }

    /// Deny still respects a token boundary and tool identity -- it is
    /// UNGATED, not unconstrained.
    #[test]
    fn deny_still_requires_an_actual_prefix_and_tool_match() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");
        assert!(!rule.matches_deny("bash", "git statusfoo"));
        assert!(!rule.matches_deny("bash", "git push --force"));
        assert!(!rule.matches_deny("edit", "git status"));
    }

    // ---- sanitizer laundering (board item 01KYTMA306JH81R083Y8K9PWCR) ----

    /// **The headline regression.** A leading tab is invisible to every
    /// POSIX shell, but by the time `rendered` reaches `matches_deny` it
    /// has been laundered into `SANITIZED_CONTROL_PLACEHOLDER`, which fuses
    /// onto the token a naive `prefix_matches` would otherwise compare.
    /// Both the RAW form (a caller that, unlike every production caller,
    /// hands this method an unsanitized string) and the SANITIZED form (as
    /// the real `render_call` seam actually produces) must be caught.
    #[test]
    fn deny_catches_a_leading_control_character_laundered_past_a_naive_prefix_match() {
        let rule = PatternRule::parse("bash:curl").expect("valid rule");

        for raw in [
            "\tcurl http://evil",
            "\ncurl http://evil",
            "\rcurl http://evil",
            "\x1b[0m curl http://evil",
        ] {
            assert!(
                rule.matches_deny("bash", raw),
                "the RAW laundering vector must be caught: {raw:?}"
            );

            // The real shared sanitizer -- see
            // `a_sanitized_chained_command_is_still_gated`'s own comment
            // above for why this is now a call rather than a hand-copy.
            let sanitized = sanitize_control_chars(raw);
            assert!(
                rule.matches_deny("bash", &sanitized),
                "the SANITIZED form (what the real pipeline actually \
                 produces) must be caught too: {raw:?} -> {sanitized:?}"
            );
        }
    }

    /// The fallback is scoped by tool identity exactly like every other
    /// branch of `matches_deny` -- laundering noise in a DIFFERENT tool's
    /// rendering must not trip a `bash` deny rule.
    #[test]
    fn the_laundering_fallback_still_requires_the_tool_to_match() {
        let rule = PatternRule::parse("bash:curl").expect("valid rule");
        assert!(!rule.matches_deny("edit", "\u{FFFD}curl http://evil"));
    }

    /// The DOCUMENTED chaining limit (this module's own doc, "a seatbelt,
    /// not a boundary") must not be accidentally narrowed away by this fix.
    /// `;` is real, visible shell syntax -- not laundered noise -- so it
    /// must NOT trip the laundering fallback the way a control character
    /// does.
    #[test]
    fn the_laundering_fallback_does_not_widen_the_documented_chaining_limit() {
        let rule = PatternRule::parse("bash:git push").expect("valid rule");
        assert!(
            !rule.matches_deny("bash", "foo; git push"),
            "a bare `;` must not trigger the laundering fallback -- that is \
             the module's own documented prefix-match limit, which this \
             item narrows LAUNDERING around, not the chaining limit itself"
        );
    }
}

/// The pattern Conway OFFERS an operator for a given call (V2b), given
/// that tool's own [`RenderKind`] declaration -- the same declaration
/// [`PatternRule::matches_render`] evaluates against, so the offer surface
/// and the evaluation surface cannot drift apart: a rule this function
/// offers is always one the broker would both register and match, for
/// exactly the rendering the operator is looking at.
///
/// ## `ShellCommand`: the two-token prefix
///
/// Returns `None` when no sensible offer exists — an empty command, or one
/// carrying shell metacharacters (offering a grant that the metacharacter
/// gate would then refuse to honor would be actively confusing).
///
/// ### Why two tokens
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
/// ## `Structured`: the wildcard, and why NOT a prefix
///
/// A `Structured` tool's rendering is a JSON dump, so it nearly always
/// contains `()`/`{}` -- shell metacharacters. Before this function took
/// `render_kind`, the metacharacter check above declined on sight and the
/// prompt simply never offered `[p]` for any `Structured` tool: a whole
/// class of tools had no discoverable pattern grants, not by decision but
/// as a side effect of a check written for shell commands.
///
/// Widening the offer to a two-token prefix over the JSON dump would be
/// the reflexive wrong fix: `when: command_prefix` against a `Structured`
/// tool is a **registration error**
/// ([`RuleRegistrationReason::CommandPrefixOnStructuredTool`]) -- the
/// dump's token boundaries depend on key order, spacing, and escaping the
/// operator cannot predict, so the rule could never reliably match, and
/// the loader refuses to install it. The prompt must not offer to create
/// a rule that cannot be registered.
///
/// The rule shape that CAN register and CAN match is the wildcard
/// `tool:*` ("any invocation of this tool" -- desugars to
/// [`When::Always`], which the registration check admits and
/// [`PatternRule::matches_render`] honors for a `Structured` rendering).
/// It is broader than a prefix -- there is no narrower registerable shape
/// for these tools -- which is exactly why the prompt states the grant in
/// words ("any `report` call") before the operator presses anything, and
/// why `[a]` (this exact call) remains the narrower remembered option.
///
/// An operator who wants something broader can add it to
/// `permissions.json` by hand, having thought about it. That asymmetry is
/// the point: granting more should take deliberate effort, granting less
/// should be the default. You can always grant again; you cannot
/// un-authorize what already ran.
pub fn suggested_rule(tool: &str, rendered: &str, render_kind: RenderKind) -> Option<PatternRule> {
    if render_kind == RenderKind::Structured {
        return Some(PatternRule {
            tool: tool.to_string(),
            command_prefix: "*".to_string(),
        });
    }
    // `ShellCommand`, and any future `RenderKind` variant (`RenderKind` is
    // `#[non_exhaustive]`): the conservative shell-shaped offer, exactly as
    // before -- fail toward offering less, never more.
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
/// The flat `allow`/`deny` lists of wire-form strings (`"bash:git status"`)
/// stay the ergonomic default: the file is meant to be read and edited by a
/// human reviewing what they have authorized, and diffed in a pull request.
/// F12 adds the optional `rules` array for the structured form -- the
/// additive superset a flat string is the surface syntax for -- so a rule
/// the flat form cannot express (`paths_under`, `categories`, `category_in`,
/// a `prompt` effect) has a home without forcing every existing rule into a
/// schema reference. A file written before `rules` existed keeps parsing
/// unchanged (`#[serde(default)]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionFile {
    /// Wire-form rules that AUTHORIZE. Malformed entries are dropped on
    /// load, not guessed at. From a project-scoped file, a caller MUST
    /// confirm a recorded trust decision before installing these -- see
    /// this module's own doc, and `.design/d4-trust-model.md` §3/§11.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Wire-form rules that REFUSE, applied immediately regardless of
    /// trust -- board item 01KYT8SGX32CP56PRJNG72V2W5. `#[serde(default)]`
    /// so every file written before this field existed keeps parsing
    /// unchanged.
    #[serde(default)]
    pub deny: Vec<String>,
    /// F12: structured rules, each carrying its own `then` (`allow`/`prompt`/
    /// `deny`). The flat `allow`/`deny` lists above are the surface syntax
    /// for the `Tools([t]) + (Always | CommandPrefix) + (Allow | Deny)`
    /// subset; this array is the superset, expressing `paths_under`,
    /// `categories`, `category_in`, and the `prompt` effect the flat form
    /// has no syntax for. `allow` entries from this array are subject to the
    /// SAME trust decision as the flat `allow` list (a project file's
    /// `then: allow` rules install only once the caller confirms trust);
    /// `deny` and `prompt` entries apply immediately, trusted or not, the
    /// same asymmetry the flat `deny` list has always had. A structurally
    /// malformed entry is dropped, not guessed at (P-10); a rule whose
    /// `then` is an unrecognized variant is dropped the same way
    /// (`#[non_exhaustive]` fail-closed).
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// Parses a rules file's ALLOW rules -- flat `allow` strings desugared plus
/// structured `rules` entries whose `then` is `Allow` -- **failing closed**.
///
/// Every failure mode returns fewer rules, never more:
/// - unparseable JSON → no rules at all (the operator is asked about
///   everything, which is Conway's default behavior anyway)
/// - a malformed flat entry → that entry dropped, the rest kept
/// - a structurally malformed `rules` entry → that entry dropped, the rest
///   kept
///
/// This asymmetry is the whole point. A corrupt permissions file must
/// never be able to *widen* what is authorized — the worst outcome of a
/// bad file is extra prompting, never a missed one.
///
/// Returns [`Rule`]s (not [`PatternRule`]s) because the structured form
/// can express what the flat form cannot; a flat entry is its desugared
/// [`Rule`] (`PatternRule::to_rule(Then::Allow)`). `Rule::to_wire`
/// round-trips the flat subset, so a caller that round-trips flat rules
/// keeps working.
pub fn parse_rules(contents: &str) -> Vec<Rule> {
    parse_permission_file(contents).allow
}

/// Parses a rules file's DENY rules -- flat `deny` strings desugared plus
/// structured `rules` entries whose `then` is `Deny` -- with the identical
/// fail-closed posture as [`parse_rules`]: a corrupt file yields NO deny
/// rules either -- fewer rules is always the safe failure here too, even
/// though `deny` narrows. A `deny` rule that silently vanished because its
/// file went corrupt would be a false sense of safety, but a `deny` rule
/// GUESSED AT from unparseable content would be worse: a half-understood
/// safety rule inspires false confidence.
pub fn parse_deny_rules(contents: &str) -> Vec<Rule> {
    parse_permission_file(contents).deny
}

/// Parses a rules file's PROMPT rules -- structured `rules` entries whose
/// `then` is `Prompt`. The flat form has no prompt syntax (a flat `deny`
/// was the only narrowing effect the flat language could express before
/// F12), so this is drawn entirely from the structured `rules` array. Same
/// fail-closed posture as [`parse_rules`]/[`parse_deny_rules`].
pub fn parse_prompt_rules(contents: &str) -> Vec<Rule> {
    parse_permission_file(contents).prompt
}

/// Shared parse used by [`parse_rules`], [`parse_deny_rules`], and
/// [`parse_prompt_rules`], so the three halves can never read the JSON
/// differently.
struct ParsedPermissionFile {
    allow: Vec<Rule>,
    deny: Vec<Rule>,
    prompt: Vec<Rule>,
}

fn parse_permission_file(contents: &str) -> ParsedPermissionFile {
    // Parse `rules` as an array of opaque JSON values, then deserialize each
    // one individually: a single structurally malformed `rules` entry is
    // DROPPED, not guessed at (P-10), and the rest of the array plus the flat
    // `allow`/`deny` lists survive. Parsing the whole document as
    // `PermissionFile` (whose `rules: Vec<Rule>` is all-or-nothing under serde)
    // would reject the entire file on one bad entry -- a louder but less
    // useful failure, and one the field's own doc disclaims. The flat
    // `allow`/`deny` lists are already drop-per-entry by construction
    // (`PatternRule::parse` returns `Option`); this gives `rules` the same
    // granular fail-closed posture.
    #[derive(serde::Deserialize)]
    struct RawPermissionFile {
        #[serde(default)]
        allow: Vec<String>,
        #[serde(default)]
        deny: Vec<String>,
        #[serde(default)]
        rules: Vec<serde_json::Value>,
    }
    let file: RawPermissionFile = match serde_json::from_str(contents) {
        Ok(file) => file,
        // Fail closed: an unreadable file authorizes, denies, and prompts
        // nothing.
        Err(_) => {
            return ParsedPermissionFile {
                allow: Vec::new(),
                deny: Vec::new(),
                prompt: Vec::new(),
            }
        }
    };
    // Flat `allow`/`deny` desugar into the same `Rule` the structured form
    // produces -- the second arm of `parse_rules`, not a second home. A
    // malformed flat entry is dropped (PatternRule::parse returns None).
    let flat_allow = file
        .allow
        .iter()
        .filter_map(|raw| PatternRule::parse(raw).map(|p| p.to_rule(Then::Allow)));
    let flat_deny = file
        .deny
        .iter()
        .filter_map(|raw| PatternRule::parse(raw).map(|p| p.to_rule(Then::Deny)));
    let mut allow: Vec<Rule> = flat_allow.collect();
    let mut deny: Vec<Rule> = flat_deny.collect();
    let mut prompt: Vec<Rule> = Vec::new();
    // Structured `rules` carry their own `then`; sort them into the three
    // buckets. `Then` is `#[non_exhaustive]`: an unrecognized `then` is
    // DROPPED (fail closed), never guessed at -- a rule whose effect the
    // loader does not understand authorizes/prompt/denies nothing. A
    // structurally malformed entry (bad `select`/`when` shape, unknown
    // variant) is dropped the same way: `serde_json::from_value::<Rule>`
    // returns `Err`, and we skip it.
    for value in file.rules {
        match serde_json::from_value::<Rule>(value) {
            Ok(rule) => match rule.then {
                Then::Allow => allow.push(rule),
                Then::Deny => deny.push(rule),
                Then::Prompt => prompt.push(rule),
            },
            Err(_) => { /* drop the malformed entry, fail closed */ }
        }
    }
    ParsedPermissionFile { allow, deny, prompt }
}

/// Where an installed [`PatternRule`] grant came from. Required so a rule
/// set is inspectable — `PermissionBroker::active_patterns()`'s own doc
/// already states the principle: "a rule set nobody can inspect is a
/// trap." Board item 01KYT8SGX32CP56PRJNG72V2W5.
///
/// Lives here (not in `conway-runtime`) so `conway`'s facade can re-export
/// it alongside [`PatternRule`]/[`PermissionFile`] without re-exporting a
/// `conway-runtime` type — `crate::lib`'s own stated invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternOrigin {
    /// Approved through the interactive permission gate (an operator's
    /// "always allow"), or installed directly with no file behind it at
    /// all (a test harness, an embedder's own code).
    Interactive,
    /// Loaded from a permissions file at this path.
    ///
    /// Carries no trust flag of its own — an ALLOW rule with this origin
    /// was only ever installed because the CALLER already confirmed it may
    /// load (the operator's own global file, trusted by authorship; or a
    /// project file with a matching recorded trust decision — see
    /// `.design/d4-trust-model.md` §4 and this module's own doc). A
    /// DENY rule with this origin may have come from an UNTRUSTED file:
    /// deny applies regardless (§3).
    File(PathBuf),
}

impl PatternOrigin {
    /// A short label for a review surface (`/settings`'s grant list is the
    /// only consumer today).
    pub fn describe(&self) -> String {
        match self {
            PatternOrigin::Interactive => "interactive".to_string(),
            PatternOrigin::File(path) => path.display().to_string(),
        }
    }
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

    // ---- deny: the asymmetric half ----

    #[test]
    fn deny_parses_independently_of_allow() {
        let contents = r#"{"allow": ["bash:cargo test"], "deny": ["bash:curl", "bash:ssh"]}"#;
        assert_eq!(parse_rules(contents).len(), 1);
        let deny = parse_deny_rules(contents);
        assert_eq!(deny.len(), 2);
        assert_eq!(deny[0].to_wire(), "bash:curl");
        assert_eq!(deny[1].to_wire(), "bash:ssh");
    }

    /// A file written before `deny` existed keeps parsing unchanged --
    /// `#[serde(default)]` earns its keep.
    #[test]
    fn a_file_with_no_deny_key_parses_as_an_empty_deny_list() {
        let contents = r#"{"allow": ["bash:git status"]}"#;
        assert!(parse_deny_rules(contents).is_empty());
        assert_eq!(parse_rules(contents).len(), 1);
    }

    /// Fail-closed applies to `deny` too: a corrupt file authorizes nothing
    /// AND denies nothing, never guessed at.
    #[test]
    fn a_corrupt_file_denies_nothing_either() {
        for corrupt in ["", "not json", "{", r#"{"deny": "not-an-array"}"#] {
            assert!(parse_deny_rules(corrupt).is_empty(), "{corrupt:?}");
        }
    }

    #[test]
    fn a_malformed_deny_entry_is_dropped_and_the_rest_kept() {
        let contents = r#"{"deny": ["bash:curl", "malformed-no-colon", "bash:ssh"]}"#;
        let deny = parse_deny_rules(contents);
        assert_eq!(deny.len(), 2);
        assert!(deny.iter().all(|r| r.to_wire() != "malformed-no-colon"));
    }

    // ---- V2b: the offered rule ----

    /// The offer is two tokens: enough for `<command> <subcommand>`, not
    /// enough to silently include a sibling subcommand.
    #[test]
    fn the_suggested_rule_is_narrow_by_default() {
        let rule = suggested_rule("bash", "git status --short", RenderKind::ShellCommand)
            .expect("offered");
        assert_eq!(rule.command_prefix, "git status");

        // Crucially: the offered grant does NOT cover a different
        // subcommand. An operator accepting this prompt has not
        // accidentally authorized `git push`.
        assert!(rule.matches("bash", "git status --short"));
        assert!(!rule.matches("bash", "git push --force"));
    }

    #[test]
    fn a_single_token_command_offers_just_that_token() {
        let rule = suggested_rule("bash", "pwd", RenderKind::ShellCommand).expect("offered");
        assert_eq!(rule.command_prefix, "pwd");
    }

    /// Offering a grant the metacharacter gate would then refuse to honor
    /// would be confusing, so no offer is made at all.
    #[test]
    fn no_rule_is_offered_for_a_command_the_gate_would_reject() {
        assert!(
            suggested_rule("bash", "git status && rm -rf /", RenderKind::ShellCommand).is_none()
        );
        assert!(suggested_rule("bash", "", RenderKind::ShellCommand).is_none());
    }

    // ---- the offer surface agrees with the evaluation surface (both
    // `RenderKind` variants at the offer site) ----

    /// **The headline `Structured` case.** A `Structured` tool's rendering
    /// is a JSON dump whose own `()`/`{}` are shell metacharacters -- the
    /// old `render_kind`-less offer declined on sight, so no `Structured`
    /// tool ever had a pattern offer at all. The offer that CAN register
    /// (F12 bans `command_prefix` against a `Structured` tool) and CAN
    /// match is the wildcard: "any invocation of this tool".
    #[test]
    fn a_structured_tool_is_offered_the_registrable_wildcard_not_a_prefix() {
        let rendered = r#"report({"summary":"build finished ok"})"#;
        let rule = suggested_rule("report", rendered, RenderKind::Structured)
            .expect("a Structured tool must have SOMETHING honestly offerable");
        assert_eq!(
            rule.command_prefix, "*",
            "the only registerable shape against a Structured tool is the wildcard -- \
             a prefix over a JSON dump is a registration error, so the prompt must \
             never offer one"
        );

        // Offer/evaluation agreement, proven rather than asserted: the
        // offered rule matches THE VERY RENDERING the operator is looking
        // at, under the tool's own declaration...
        assert!(
            rule.matches_render("report", rendered, RenderKind::Structured),
            "a rule the prompt offers must be one the broker would match \
             against this exact call"
        );
        // ...and it desugars to `When::Always`, the shape
        // `Conway::validate_rule_registration` admits for a Structured tool
        // (a `When::CommandPrefix` here would be refused at install).
        assert_eq!(
            rule.to_rule(Then::Allow).when,
            When::Always,
            "the offered rule must survive the F12 registration check"
        );
    }

    /// The `Structured` offer does not depend on the rendering's content at
    /// all -- metacharacters in a JSON dump are not shell risk, so they
    /// must not suppress the offer the way they do for `bash`.
    #[test]
    fn the_structured_offer_is_not_suppressed_by_metacharacters() {
        for rendered in [
            r#"read({"path":"a"})"#,
            r#"write({"path":"x","content":"a && b; c | d"})"#,
        ] {
            assert!(
                suggested_rule("read", rendered, RenderKind::Structured).is_some(),
                "a Structured rendering's metacharacters must not suppress the offer: {rendered:?}"
            );
        }
    }

    /// `RenderKind` is `#[non_exhaustive]`: a future variant must get the
    // conservative shell-shaped behavior (offer less, never more), which
    /// is exactly what the `_` arm delivers -- pinned here so the fallback
    /// is a decision, not an accident.
    #[test]
    fn the_offer_falls_back_to_the_conservative_shell_shape_for_unknown_kinds() {
        // Both non-Structured kinds today agree; if a third kind ever
        // appears this test keeps compiling via the same arm and keeps
        // meaning "the conservative offer".
        let kind = RenderKind::ShellCommand;
        assert!(suggested_rule("bash", "git status && rm -rf /", kind).is_none());
        assert!(suggested_rule("bash", "git status --short", kind).is_some());
    }

}

// =====================================================================
// F12: the structured rule form -- `Rule { select, when, then }`.
// =====================================================================
#[cfg(test)]
mod f12_tests {
    use super::*;
    use crate::content::ToolCategory;
    use crate::ports::RenderKind;

    // ---- desugaring: the flat form IS the structured form ----

    /// `tool:*` (the flat wildcard, "any invocation") desugars to
    /// `Tools([tool]) + Always`, NOT `CommandPrefix("*")`. This is what keeps
    /// `read:*` working under the structured evaluator: `Always` on a
    /// `Structured` tool is a perfectly sensible rule, while `CommandPrefix`
    /// on a `Structured` tool is a registration error.
    #[test]
    fn wildcard_flat_rule_desugars_to_always() {
        let r = PatternRule::parse("read:*").expect("valid").to_rule(Then::Allow);
        assert_eq!(r.select, Select::Tools(vec!["read".to_string()]));
        assert_eq!(r.when, When::Always);
        assert_eq!(r.then, Then::Allow);
    }

    #[test]
    fn prefix_flat_rule_desugars_to_command_prefix() {
        let r = PatternRule::parse("bash:git status").expect("valid").to_rule(Then::Deny);
        assert_eq!(r.select, Select::Tools(vec!["bash".to_string()]));
        assert_eq!(r.when, When::CommandPrefix("git status".to_string()));
        assert_eq!(r.then, Then::Deny);
    }

    /// `to_pattern_rule` is the inverse of `to_rule` for the desugarable
    /// subset, and `None` for anything the flat form cannot express -- so
    /// the broker's `PatternRule`-shaped review surface can carry flat
    /// rules unchanged and defer structured rules to a separate listing.
    #[test]
    fn to_pattern_rule_round_trips_the_desugarable_subset() {
        for (wire, then) in [
            ("bash:git status", Then::Allow),
            ("read:*", Then::Allow),
            ("bash:curl", Then::Deny),
        ] {
            let p = PatternRule::parse(wire).expect("valid");
            let r = p.to_rule(then);
            assert_eq!(r.to_pattern_rule(), Some(p.clone()), "round-trip: {wire}");
        }
    }

    #[test]
    fn to_pattern_rule_returns_none_for_structured_only_rules() {
        let paths_under = Rule {
            select: Select::Tools(vec!["read".into()]),
            when: When::PathsUnder("/repo".into()),
            then: Then::Allow,
        };
        assert!(paths_under.to_pattern_rule().is_none());

        let categories = Rule {
            select: Select::Categories(vec![ToolCategory::Read]),
            when: When::Always,
            then: Then::Prompt,
        };
        assert!(categories.to_pattern_rule().is_none());

        let multi_tool = Rule {
            select: Select::Tools(vec!["read".into(), "grep".into()]),
            when: When::Always,
            then: Then::Allow,
        };
        assert!(multi_tool.to_pattern_rule().is_none());
    }

    /// `Rule::to_wire` round-trips the flat form so the existing flat-list
    /// read-modify-write and the deny error message keep working.
    #[test]
    fn to_wire_round_trips_the_flat_subset() {
        assert_eq!(
            PatternRule::parse("bash:git status")
                .expect("valid")
                .to_rule(Then::Allow)
                .to_wire(),
            "bash:git status"
        );
        assert_eq!(
            PatternRule::parse("read:*")
                .expect("valid")
                .to_rule(Then::Allow)
                .to_wire(),
            "read:*"
        );
        // A structured-only rule serializes to its JSON form (not a flat
        // wire) -- this is what a review surface shows for a rule the flat
        // language cannot name.
        let structured = Rule {
            select: Select::Tools(vec!["read".into()]),
            when: When::PathsUnder("/repo".into()),
            then: Then::Allow,
        };
        let wire = structured.to_wire();
        assert!(wire.contains("paths_under"), "structured wire: {wire}");
    }

    // ---- one evaluator: byte-identical decisions ----

    /// THE headline proof: a flat `PatternRule` and its desugared `Rule`
    /// produce byte-identical ALLOW decisions across a matrix of calls and
    /// render kinds. This is the strongest available evidence there is one
    /// evaluator and not two -- both reach `Rule::matches_allow_render`
    /// (the flat form via `PatternRule::matches_render`'s identical
    /// primitives; the structured form via `to_rule().matches_allow_render`)
    /// and cannot drift.
    #[test]
    fn flat_and_structured_produce_byte_identical_allow_decisions() {
        // A matrix of (rule, tool, rendered, render_kind): ordinary matches,
        // subcommand mismatches, chained commands (gated), Structured
        // wildcards, a non-wildcard prefix on a Structured tool.
        let cases: &[(&str, &str, &str, RenderKind, ToolCategory)] = &[
            ("bash:git status", "bash", "git status", RenderKind::ShellCommand, ToolCategory::Execute),
            ("bash:git status", "bash", "git status --short", RenderKind::ShellCommand, ToolCategory::Execute),
            ("bash:git status", "bash", "git push --force", RenderKind::ShellCommand, ToolCategory::Execute),
            ("bash:git status", "bash", "git status && rm -rf /", RenderKind::ShellCommand, ToolCategory::Execute),
            ("bash:git status", "bash", "git status\nrm -rf /", RenderKind::ShellCommand, ToolCategory::Execute),
            ("bash:*", "bash", "ls -la", RenderKind::ShellCommand, ToolCategory::Execute),
            ("bash:*", "bash", "ls -la && rm -rf /", RenderKind::ShellCommand, ToolCategory::Execute),
            ("read:*", "read", r#"read({"path":"a.rs"})"#, RenderKind::Structured, ToolCategory::Read),
            ("read:*", "write", r#"write({"path":"a.rs"})"#, RenderKind::Structured, ToolCategory::Edit),
            (r#"report:report({"summary":"build"#, "report", r#"report({"summary":"build finished"})"#, RenderKind::Structured, ToolCategory::Think),
        ];
        for (wire, tool, rendered, rk, cat) in cases {
            let flat = PatternRule::parse(wire).expect("valid");
            let structured = flat.to_rule(Then::Allow);
            assert_eq!(
                flat.matches_render(tool, rendered, *rk),
                structured.matches_allow_render(tool, *cat, rendered, *rk),
                "flat and structured must agree: {wire:?} vs {rendered:?} ({rk:?})"
            );
        }
    }

    /// The deny/prompt side: `PatternRule::matches_deny` and the desugared
    /// `Rule::matches_deny_render` agree across the same matrix -- the
    /// ungated, laundering-aware evaluator is one, not two.
    #[test]
    fn flat_and_structured_produce_byte_identical_deny_decisions() {
        let cases: &[(&str, &str, &str, ToolCategory)] = &[
            ("bash:curl", "bash", "curl https://example.com", ToolCategory::Execute),
            ("bash:curl", "bash", "curl x; rm -rf /", ToolCategory::Execute),
            ("bash:curl", "bash", "\tcurl http://evil", ToolCategory::Execute),
            ("bash:*", "bash", "ls -la && rm -rf /", ToolCategory::Execute),
            ("write:*", "write", r#"write({"path":"/etc/passwd"})"#, ToolCategory::Edit),
        ];
        for (wire, tool, rendered, cat) in cases {
            let flat = PatternRule::parse(wire).expect("valid");
            let structured = flat.to_rule(Then::Deny);
            assert_eq!(
                flat.matches_deny(tool, rendered),
                structured.matches_deny_render(tool, *cat, rendered),
                "flat and structured deny must agree: {wire:?} vs {rendered:?}"
            );
        }
    }

    // ---- structured parsing: the `rules` array ----

    /// A structured `rules` entry parses into the `Rule` it names, and the
    /// flat `allow`/`deny` lists still parse alongside it.
    #[test]
    fn structured_rules_array_parses_into_rules() {
        let contents = r#"{
            "allow": ["bash:git status"],
            "rules": [
                {"select": {"tools": ["read"]}, "when": "always", "then": "allow"},
                {"select": {"tools": ["bash"]}, "when": {"command_prefix": "cargo test"}, "then": "allow"},
                {"select": {"categories": ["edit","delete"]}, "when": {"paths_under": "/repo"}, "then": "deny"},
                {"select": {"tools": ["bash"]}, "when": "always", "then": "prompt"}
            ]
        }"#;
        let allow = parse_rules(contents);
        assert_eq!(allow.len(), 3, "flat allow + two structured allow");
        assert!(allow.iter().any(|r| r.to_wire() == "bash:git status"));
        assert!(allow.iter().any(|r| matches!(r.when, When::Always) && matches!(r.select, Select::Tools(ref t) if t == &["read".to_string()])));
        assert!(allow.iter().any(|r| matches!(r.when, When::CommandPrefix(_))));

        let deny = parse_deny_rules(contents);
        assert_eq!(deny.len(), 1, "one structured deny");
        assert!(matches!(deny[0].select, Select::Categories(_)));
        assert!(matches!(deny[0].when, When::PathsUnder(_)));

        let prompt = parse_prompt_rules(contents);
        assert_eq!(prompt.len(), 1, "one structured prompt");
        assert!(matches!(prompt[0].then, Then::Prompt));
    }

    /// A structurally malformed `rules` entry is dropped, not guessed at
    /// (P-10) -- the rest of the array and the flat lists survive.
    #[test]
    fn a_malformed_structured_entry_is_dropped_and_the_rest_kept() {
        let contents = r#"{
            "rules": [
                {"select": {"tools": ["read"]}, "when": "always", "then": "allow"},
                {"select": "not-an-object", "when": "always"},
                {"select": {"tools": ["bash"]}, "when": "always", "then": "bogus_effect"}
            ],
            "allow": ["bash:git status"]
        }"#;
        let allow = parse_rules(contents);
        // Only the first structured rule (valid, allow) and the flat rule
        // survive; the malformed entry and the unknown-`then` entry are
        // both dropped (fail closed).
        assert_eq!(allow.len(), 2, "two valid allow rules survive");
    }

    #[test]
    fn a_file_with_no_rules_key_parses_as_before() {
        let contents = r#"{"allow": ["bash:git status"], "deny": ["bash:curl"]}"#;
        assert_eq!(parse_rules(contents).len(), 1);
        assert_eq!(parse_deny_rules(contents).len(), 1);
        assert!(parse_prompt_rules(contents).is_empty());
    }

    // ---- select / when predicates ----

    #[test]
    fn tools_pattern_matches_exact_and_trailing_wildcard_only() {
        assert!(tool_pattern_matches("bash", "bash"));
        assert!(!tool_pattern_matches("bash", "bashfoo"));
        assert!(tool_pattern_matches("re*", "read"));
        assert!(tool_pattern_matches("re*", "report"));
        assert!(!tool_pattern_matches("re*", "grep"), "trailing * is a prefix, not infix");
        assert!(tool_pattern_matches("*", "anything"));
        // An embedded `*` (not trailing) is literal, not a wildcard.
        assert!(!tool_pattern_matches("a*b", "axb"));
        assert!(tool_pattern_matches("a*b", "a*b"));
    }

    #[test]
    fn category_in_predicate_uses_the_calls_category() {
        let r = Rule {
            select: Select::Tools(vec!["bash".into()]),
            when: When::CategoryIn(vec![ToolCategory::Execute]),
            then: Then::Allow,
        };
        // `Always` carries the gate; for a Structured render there is no gate,
        // so this tests the category condition directly.
        assert!(r.matches_allow_render(
            "bash",
            ToolCategory::Execute,
            "bash echo",
            RenderKind::Structured,
        ));
        assert!(!r.matches_allow_render(
            "bash",
            ToolCategory::Read,
            "bash echo",
            RenderKind::Structured,
        ));
    }

    #[test]
    fn paths_under_returns_false_in_the_render_evaluator() {
        // The render-based evaluator cannot resolve paths (it has no
        // `arguments`/`cwd`); `paths_under` is the broker's job. Returning
        // `false` here means a `paths_under` allow rule never fires through
        // the render path -- the broker is the only place it can match.
        let r = Rule {
            select: Select::Tools(vec!["read".into()]),
            when: When::PathsUnder("/repo".into()),
            then: Then::Allow,
        };
        assert!(!r.matches_allow_render(
            "read",
            ToolCategory::Read,
            r#"read({"path":"/repo/a.rs"})"#,
            RenderKind::Structured,
        ));
    }

    // ---- registration error ----

    #[test]
    fn registration_error_describes_itself() {
        let err = RuleRegistrationError {
            rule: Rule {
                select: Select::Tools(vec!["read".into()]),
                when: When::CommandPrefix("read".into()),
                then: Then::Allow,
            },
            reason: RuleRegistrationReason::CommandPrefixOnStructuredTool,
        };
        let d = err.reason.describe();
        assert!(d.contains("command_prefix"), "{d}");
        assert!(d.contains("structured"), "{d}");
    }
}