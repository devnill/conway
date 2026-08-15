//! Prefix-matching permission patterns (V2) -- and why a
//! [`RenderKind::ShellCommand`] tool cannot be granted one.
//!
//! ## The requirement this module exists to satisfy
//!
//! `PHILOSOPHY.md`, "Constraining a child: its tool set":
//!
//! > conway does not inspect a call and judge what it would do. Judging a
//! > shell command means predicting what a shell will make of a string, and
//! > a filter built on pattern matching fails in both directions. Loose
//! > enough to permit ordinary work, it misses the cases it was written
//! > for. Tight enough to catch those, it rejects enough routine commands
//! > that the people relying on it turn it off.
//!
//! GP-13 names this exact module as the cautionary example of the
//! anti-pattern the page rules out: "when a dangerous tool surface needs
//! constraining, the answer is a containment primitive or the tool's
//! absence -- NOT more policy machinery inside the permission gate. The
//! shell-metacharacter blocklist in `permission_pattern.rs` is the
//! cautionary example." Its measured cost is on record: strengthening it
//! measured a 68% false-positive rate against this repository's own logged
//! `bash` commands.
//!
//! **AMENDED (board item `01KZDDPC5MMD49F6JPV9CW4TVM`, superseding
//! `01KZ73M5ZD07RQTE47RWX0YYDK`'s "leave the gate unchanged, fix the
//! documentation instead" ruling): the gate is REMOVED for
//! [`RenderKind::ShellCommand`], not strengthened, narrowed, or
//! re-documented.** This module used to carry `contains_shell_metacharacters`
//! (still present, now used only by the unrelated one-shot `--allowed-tools`
//! gate in `conway::gates` -- see that module's own doc), a scan of the
//! rendered command text for `;`, `&`, `|`, backtick, `$(`, newlines and
//! friends, run as a hard gate inside `PatternRule::matches_render`/
//! `Rule::matches_allow_render` before any prefix comparison. That scan
//! was exactly the thing the requirement above forbids: conway reading a
//! call's text and judging, from it, whether the call could do something
//! the grant did not intend. Its own history proved the requirement
//! right -- the identical scan, applied by mistake to every tool's
//! rendering rather than only shell commands, silently made every
//! non-`bash` pattern grant inert (fixed in `68ea9b1`) -- a filter that
//! fails in a direction its own author did not expect is precisely the
//! failure mode the page describes.
//!
//! **The resolution taken is the first of the two the item posed: a
//! durable prefix/wildcard ALLOW grant is the wrong mechanism for a
//! [`RenderKind::ShellCommand`] tool, full stop, so it no longer exists for
//! one.** [`Rule::gate_allows`] now reads only the tool's *static*
//! [`RenderKind`] declaration -- never the call's rendered text -- and
//! refuses every allow `when` (`Always`, `CommandPrefix`, `CategoryIn`,
//! `PathsUnder`) whenever that declaration is
//! [`RenderKind::ShellCommand`], regardless of what the command contains.
//! A `bash:git status` grant does not merely fail to cover a chained
//! command anymore; it does not cover ANYTHING, including the literal,
//! unchained `git status` it names. Reading `render_kind` is not the thing
//! the page forbids: it is a fixed fact a tool declares about itself once,
//! at registration -- not a judgment formed by inspecting the particular
//! call in front of it. No two calls to the same tool can ever disagree
//! about it, and nothing about a specific command's text changes the
//! answer, which is the property a fail-closed, unparseable-input-proof
//! check needs and a text scan can never have.
//!
//! What remains available for a shell command, per the item's own framing:
//! allow it once (an interactive "yes" that grants nothing durable), deny
//! it, prompt on it, or confine what it can reach with a containment
//! primitive (`conway.fs`'s root; PHILOSOPHY.md's "Constraining a child:
//! its tool set"). A *durable, remembered* grant for `bash` (or any other
//! [`RenderKind::ShellCommand`] tool) is no longer expressible through this
//! module at all. [`suggested_rule`]'s own doc covers the operator-facing
//! half of this: the prompt no longer OFFERS a pattern for a shell command
//! either, so an operator is never invited to create a grant this module
//! would then silently never honor.
//!
//! `deny`/`prompt` are UNCHANGED by this item -- see "The `allow`/`deny`
//! asymmetry" below. Narrowing has no failure mode worth removing a
//! mechanism over; only `allow` (a grant of durable authority) does.
//!
//! [`PatternRule`]/[`Rule`] prefix and wildcard ALLOW grants continue to
//! work unchanged for a [`RenderKind::Structured`] tool (`read`, `write`,
//! `grep`, ...): no shell is ever involved in running one, so there is
//! nothing here for the page's requirement to say anything about, and
//! nothing in this module reads a `Structured` call's rendered text to
//! decide allow coverage either -- prefix comparison there matches literal
//! JSON tokens, never shell syntax.
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
//! can evaluate before saying yes. This reasoning is why the prefix
//! language exists and stays the ergonomic default for `deny`/`prompt`
//! (every tool) and for `allow` (`RenderKind::Structured` tools only, per
//! the amendment above).
//!
//! ## The `allow`/`deny` asymmetry
//!
//! [`PermissionFile`] has two halves, and they are deliberately NOT
//! symmetric. `allow` is authority: granting it to a project file that a
//! cloned repository controls, with no consent, is a live fail-open
//! security gap -- so an
//! `allow` rule loaded from a project file only takes effect once the
//! CALLER (`conway`'s facade, `conway-cli`'s startup loader) has confirmed
//! an explicit, recorded trust decision for that exact file's bytes. This
//! module has no idea whether the caller did that -- it is not this
//! module's job to gate on trust, only to make the vocabulary expressive
//! enough that a caller CAN.
//!
//! `deny` is the opposite: a rule that only ever narrows what is
//! authorized has no failure mode worth gating (the worst case is an extra
//! prompt), so it applies immediately, from any file, trusted or not, for
//! EVERY [`RenderKind`] including [`RenderKind::ShellCommand`].
//! [`PatternRule::matches_deny`] is the deliberately UNGATED sibling of
//! [`PatternRule::matches_render`] -- it does not consult `render_kind` at
//! all, because gating it the way `allow` is gated would defeat the very
//! rule it is supposed to protect: `deny bash:curl` refused the same way
//! `allow` refuses `ShellCommand` calls would let `curl x; y` slip past
//! simply for being a shell command. Composition is most-restrictive-wins:
//! a deny beats every allow, independent of authorship or order.
//!
//! **The honest limit, stated rather than papered over:** prefix matching
//! is not a containment boundary in either direction. `deny bash:git push`
//! does not catch `foo; git push`. What makes the composition sound anyway
//! is `allow`'s OWN refusal: NO command -- chained or not -- can be
//! satisfied by a pattern grant for a [`RenderKind::ShellCommand`] tool,
//! regardless of what patterns exist, so any command reaching a `bash`
//! call at all falls through to whatever the mode does unaided -- `deny`
//! is a seatbelt for the obvious case, not a boundary.
//!
//! Be precise about what that fallthrough means, because the earlier
//! wording here ("so the chained form always reaches the human operator")
//! was FALSE and is the kind of claim an operator would reasonably rely
//! on. In `PermissionBroker`, the `AutoAllow` short-circuit sits AFTER the
//! (now unconditionally-refusing, for `ShellCommand`) `pattern_allows`
//! check and BEFORE `gate.check`, and is itself ungated -- so under
//! `AutoAllow`, a `bash` call with no `deny`/`prompt` rule and no
//! confinement root is allowed silently, never reaching a human, exactly
//! as it was before this item (this item removes a grant SURFACE, not
//! `AutoAllow`'s own reach). What DOES force the gate regardless of mode
//! is `must_reach_gate`: a `PathArgs::Unconfinable` call under a root, or
//! a matching `prompt` rule. Anything that must never happen belongs in
//! the confinement root, not in a `deny` prefix -- and not in this
//! module's allow gate either, which no longer has any content to gate on
//! in the first place.
//!
//! ## Sanitizer laundering was a second, DIFFERENT hole
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
//! - **`rendered`, not `arguments`.** the extension design warns that `rendered` is sanitized and lossy and must not be the
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

/// Shell metacharacters that can extend a command past what a pattern
/// named. `;`/newline sequence, `&`/`|` chain or background, backtick/`$(`
/// substitute, `<`/`>` redirect.
///
/// **No longer consulted by this module's own allow gate** ([`Rule::
/// gate_allows`] refuses every allow rule for a [`RenderKind::ShellCommand`]
/// tool outright now, without reading `rendered` at all -- see this
/// module's own doc, "AMENDED" section, for why a text scan was removed
/// rather than kept or strengthened). [`contains_shell_metacharacters`]
/// remains, unit-tested, because `conway::gates::AllowListGate` (the
/// stateless `-p`/`--allowed-tools` one-shot gate, a DIFFERENT mechanism
/// from the durable pattern grants this module implements) still calls it
/// directly; see that module's own doc for the scope in which it survives.
const SHELL_METACHARACTERS: &[char] = &[
    ';', '&', '|', '`', '$', '\n', '\r', '<', '>', '(', ')', '{', '}',
];

/// Whether `command` contains anything that could extend it past a matched
/// prefix.
///
/// Disqualifies three classes, all in the "re-prompt too often" direction:
/// the `SHELL_METACHARACTERS` themselves; any control character (so an
/// UNSANITIZED string carrying a raw `\n`/`\x1b` is caught here even if it
/// never passed through a sanitizer); and [`SANITIZED_CONTROL_PLACEHOLDER`]
/// (so a SANITIZED string whose control char was already rewritten is
/// caught too).
///
/// See `SHELL_METACHARACTERS`'s own doc: this function is no longer part
/// of THIS module's allow decision. It survives as a standalone predicate
/// for `conway::gates::AllowListGate`'s own, narrower gate over the
/// `-p`/`--allowed-tools` one-shot mode.
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

/// The `category` [`Self::matches_render`]/[`Self::matches_deny`] pass to
/// [`Rule::matches_allow_render`]/[`Rule::matches_deny_render`] on behalf of
/// a flat rule, which carries no category of its own. Any variant would do
/// -- [`Self::to_rule`] only ever produces `Select::Tools` (whose
/// `select_matches` arm never reads `category`) paired with `When::Always`
/// or `When::CommandPrefix` (neither of whose match arms read it either);
/// `When::CategoryIn` and `Select::Categories`, the only places `category`
/// matters, are structured-only and never reachable from a desugared flat
/// rule. `Execute` is picked arbitrarily; the choice is pinned as truly
/// inert (not merely "happens not to matter for `Execute`") by
/// `flat_matches_render_result_is_independent_of_the_placeholder_category`.
const FLAT_FORM_CATEGORY_PLACEHOLDER: ToolCategory = ToolCategory::Execute;

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
    /// CONSERVATIVE [`RenderKind::ShellCommand`] -- i.e. this ALWAYS
    /// returns `false`, exactly as [`Self::matches_render`] does for any
    /// [`RenderKind::ShellCommand`] call.
    ///
    /// This is the right call when the caller has no tool declaration in
    /// hand (this module's own tests; any consumer that only has a bare
    /// string). A caller that DOES have the real tool's declaration --
    /// every production caller does -- should use [`Self::matches_render`]
    /// instead, so a `Structured`-rendering tool's wildcard/prefix grants
    /// are not held refused for no reason. See this module's own doc for
    /// why that distinction exists at all.
    pub fn matches(&self, tool: &str, rendered: &str) -> bool {
        self.matches_render(tool, rendered, RenderKind::ShellCommand)
    }

    /// Whether this rule authorizes `tool` running `rendered`, given that
    /// tool's own [`RenderKind`] declaration.
    ///
    /// The gate is checked HERE, before any prefix comparison, so every
    /// path to a pattern-based allow passes through it -- and it is an
    /// UNCONDITIONAL refusal whenever `render_kind` is
    /// [`RenderKind::ShellCommand`], regardless of `rendered`'s content.
    /// For [`RenderKind::Structured`], `rendered` is never handed to a
    /// shell, so nothing here refuses it at all -- see this module's own
    /// doc for the full reasoning. A `*` rule is refused too whenever the
    /// gate applies: "any invocation of this tool" still must not mean
    /// "any invocation" for a tool whose rendering genuinely is a shell
    /// command -- it means no invocation, via this mechanism, at all.
    ///
    /// **A thin delegate, not a restatement
    ///.** This used to carry its own full copy
    /// of the gate-then-prefix logic, kept "in sync" with
    /// [`Rule::matches_allow_render`] only by a doc comment claiming
    /// byte-identical behavior and a test pinning that claim (see this
    /// module's own byte-identical-decisions test) -- but NO production
    /// caller ever reached this copy (`PermissionBroker` only ever
    /// installs the desugared [`Rule`], via [`Self::to_rule`], before
    /// `decide()` sees it), so a future edit to the gate here could drift
    /// from `Rule::matches_allow_render` with every existing test of
    /// *this* function still green -- the exact "restated, not called"
    /// shape this item exists to close. `category` has no real value for a
    /// flat rule ([`Self::to_rule`] always desugars to `Select::Tools` +
    /// `When::Always`/`CommandPrefix`, and NEITHER `select_matches`'
    /// `Tools` arm nor those two `when` arms ever read it); this passes a
    /// fixed placeholder, pinned as truly inert by
    /// `flat_matches_render_result_is_independent_of_the_placeholder_category`
    /// below.
    pub fn matches_render(&self, tool: &str, rendered: &str, render_kind: RenderKind) -> bool {
        self.to_rule(Then::Allow).matches_allow_render(
            tool,
            FLAT_FORM_CATEGORY_PLACEHOLDER,
            rendered,
            render_kind,
        )
    }

    /// Whether this rule, used as a `deny` rule, refuses `tool` running
    /// `rendered` -- prefix comparison only, deliberately WITHOUT
    /// [`Self::matches_render`]'s [`RenderKind`]-based refusal, and
    /// deliberately with no [`RenderKind`] parameter at all: a deny rule's
    /// whole job is to be hard to evade, so it must match identically
    /// regardless of what the matched tool's rendering happens to look
    /// like, INCLUDING a `ShellCommand` tool. See this module's own doc
    /// for why refusing a `deny` prefix the way `allow` is refused would
    /// defeat it.
    ///
    /// ## Sanitizer laundering
    ///
    /// Being ungated on `RenderKind` is not the same as trusting
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
    /// `rendered_evidence_is_untrustworthy` catches this: `rendered`
    /// carrying a raw control character (in case a caller ever hands this
    /// method an unsanitized string directly) or the sanitizer's
    /// placeholder is treated as MATCHING any deny rule for this tool,
    /// rather than as failing to match -- fail TOWARD the deny, never
    /// away from it. This is deliberately narrower than
    /// [`contains_shell_metacharacters`]: it does NOT fire on
    /// `SHELL_METACHARACTERS` (`;`, `&`, `|`, ...), which are real,
    /// visible shell syntax `prefix_matches` already reads correctly.
    /// Firing on those too would silently "fix" -- i.e. narrow away --
    /// this module's own DOCUMENTED prefix-match limit (`deny bash:git
    /// push` not catching `foo; git push`, this module's own doc, "a
    /// seatbelt, not a boundary"), which this item deliberately leaves
    /// alone.
    ///
    /// **A thin delegate, not a restatement
    ///.** Same shape and same reasoning as
    /// [`Self::matches_render`]'s own doc: this used to carry its own full
    /// copy of the ungated, laundering-aware comparison, reachable by NO
    /// production caller (`PermissionBroker::deny_matches`/`prompt_matches`
    /// only ever see the desugared [`Rule`]), so it could drift from
    /// [`Rule::matches_deny_render`] with every test of *this* function
    /// still green. `category`'s placeholder is inert for the identical
    /// reason given there.
    pub fn matches_deny(&self, tool: &str, rendered: &str) -> bool {
        self.to_rule(Then::Deny)
            .matches_deny_render(tool, FLAT_FORM_CATEGORY_PLACEHOLDER, rendered)
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
// the extension design for why there is one language, not
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
    /// Always`. The allow-side gate still applies (see [`Rule::
    /// matches_allow_render`]), so `Always` on a `ShellCommand` tool does
    /// NOT authorize anything at all -- it is "any invocation, still
    /// subject to the render-kind refusal", never "any invocation,
    /// including a shell command".
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
    /// paths to confine). The allow-side gate still applies too: a
    /// `ShellCommand` tool can never satisfy `PathsUnder` either, since the
    /// gate refuses it before the path check ever runs.
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
    /// the allow-side `RenderKind` gate and root confinement). An `allow`
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

    /// The allow-side gate, as an associated fn so the broker's
    /// `paths_under` path applies it identically to the render-based path.
    ///
    /// **Reads only the tool's static [`RenderKind`] declaration -- never
    /// `rendered`.** A [`RenderKind::ShellCommand`] tool can never be
    /// authorized by ANY pattern grant, full stop; a [`RenderKind::
    /// Structured`] tool is never gated here at all. See this module's own
    /// doc, "AMENDED" section, for why this changed from a scan of the
    /// command's text (`contains_shell_metacharacters`) to an unconditional
    /// refusal keyed on the tool's declaration alone: the requirement is
    /// that conway not judge a call from its text, and `render_kind` is a
    /// fact the tool states about itself once, not a judgment formed by
    /// reading the particular call in front of it.
    pub fn gate_allows(render_kind: RenderKind) -> bool {
        render_kind != RenderKind::ShellCommand
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
        // The gate: allow keeps it; deny skips it. Applied before any
        // `when` predicate, and for every `when` (not just
        // `CommandPrefix`) -- `Always` on a `ShellCommand` tool must NOT
        // authorize ANY call, chained or not. `rendered` is intentionally
        // not read here -- see `Self::gate_allows`'s own doc.
        if !Self::gate_allows(render_kind) {
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
    /// `RenderKind` gate (the deny/prompt asymmetry: a `ShellCommand`
    /// rendering must not defeat a deny/prompt the way it defeats an
    /// allow), and with the
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
/// the operator (untrusted input -> typed errors, never panics). A rule
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
    /// expected to be refused instead goes through -- a narrowing rule must
    /// fail closed, never silently match nothing. For `None` the rule is a
    /// no-op rather than fail-open (the tool takes no paths, so it cannot reach
    /// the prefix), but the loader still refuses to install it silently so the
    /// operator learns the rule does nothing -- a no-op deny is a trap worth
    /// surfacing. In both cases the loader surfaces this error so the operator
    /// can rewrite the rule (e.g. scope it to the tool's `command_prefix`, or
    /// drop the unconfinable tool from the select). For `then: allow` the same
    /// inertness is fail-CLOSED (the broker simply never matches it and the
    /// call falls through to the operator's gate), so this is NOT raised for
    /// allow rules.
    PathsUnderOnUnconfinedTool,
    /// `when: paths_under` named a prefix that FAILS to canonicalize -- the
    /// directory does not exist on disk (a typo, or a repo/subdirectory not
    /// yet cloned/checked out), or it contains a NUL byte, or
    /// `resolve_like_the_tool_will` could not resolve it. The broker's
    /// `remember_*_rule` drops such a rule (fail closed: a rule whose
    /// boundary cannot be established confers no boundary), returning
    /// `false` -- and the loader surfaces that here instead of silently
    /// swallowing the `bool`, so the operator learns the rule was never
    /// installed. This is the mirror of the `68ea9b1` `read:*`-matched-nothing
    /// bug: a rule that can never match is a lie the operator will not
    /// notice. For `then: deny`/`prompt` the hazard is sharpest -- the
    /// operator believes a `paths_under` deny is protecting them when it was
    /// never installed (fail-OPEN against the operator's expectation); for
    /// `then: allow` the call simply falls through to the gate (fail-CLOSED),
    /// but the operator still deserves to know their rule did nothing.
    /// Distinct from [`Self::PathsUnderOnUnconfinedTool`]: that fires when
    /// the prefix canonicalizes FINE but the selected tool's `PathArgs` can
    /// never be confined; this fires when the prefix ITSELF cannot be
    /// canonicalized, regardless of the tool.
    PathsUnderPrefixUncanonicalizable,
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
            RuleRegistrationReason::PathsUnderPrefixUncanonicalizable => {
                "`paths_under` names a prefix that does not resolve on disk (a typo, or \
                 a directory not yet cloned/checked out); the rule was never installed, \
                 so it protects nothing -- fix the prefix or create the directory"
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

    // ---- no allow pattern ever matches a ShellCommand tool ----

    /// THE most important test in this module. A `bash:git status` grant
    /// authorizes NOTHING -- not the chained forms it was always meant to
    /// exclude, and not even the literal, unchained `git status` it names.
    /// This is the headline behavior change this module's own doc
    /// ("AMENDED" section) records: a durable pattern grant no longer
    /// exists for a `RenderKind::ShellCommand` tool at all, so there is
    /// nothing left to widen by chaining -- proving a previously-refused
    /// command stays refused is necessary but not sufficient here; this
    /// test also proves a previously-ALLOWED command (the plain `git
    /// status`) is refused now too, which is the actual shape of this
    /// item's fix.
    #[test]
    fn no_shell_command_is_ever_matched_by_a_pattern_grant_plain_or_chained() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");

        assert!(
            !rule.matches("bash", "git status"),
            "even the exact, unchained command the rule names must no \
             longer be auto-approved -- a durable grant for a shell \
             command does not exist any more, not a narrower one"
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
                "a chained command must never be satisfied by a pattern \
                 grant either: {chained:?}"
            );
        }
    }

    /// Regression, restated for the new behavior: sanitization (or the lack
    /// of it) makes no difference, because nothing about `rendered`'s
    /// content is read at all any more. Both the raw string (as an
    /// unsanitized caller would pass it) and the sanitized string (as the
    /// production `ToolRunner` seam actually produces) are pinned here,
    /// alongside a WHOLLY benign command with no laundering involved --
    /// all refused identically, which is the proof that the refusal is
    /// content-independent rather than merely "still catches laundering".
    #[test]
    fn shell_command_refusal_is_independent_of_sanitization_or_content() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");

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
                "raw form must be refused: {raw:?}"
            );

            // The real shared sanitizer -- `conway_core::text::
            // sanitize_control_chars`, which the production `ToolRunner`
            // seam also calls. The end-to-end test that drives the genuine
            // pipeline seam is `a_newline_chained_command_still_reaches_
            // the_operator_through_the_real_render_seam` in
            // `conway/tests/permission_pattern_seam.rs`.
            let sanitized = sanitize_control_chars(raw);
            assert!(
                !rule.matches("bash", &sanitized),
                "sanitized form must be refused too: {raw:?} -> {sanitized:?}"
            );
        }

        // The formerly-benign, unchained command is refused identically --
        // this module no longer distinguishes "benign" from "dangerous"
        // shell command text at all, which is the point.
        assert!(
            !rule.matches("bash", "git status --short"),
            "an ordinary command carrying no metacharacter at all must be \
             refused exactly like a chained one -- the refusal no longer \
             depends on content"
        );
    }

    /// The wildcard rule authorizes nothing for a `ShellCommand` tool
    /// either -- "any bash call" is exactly as inert as a specific prefix.
    #[test]
    fn the_wildcard_rule_matches_no_shell_command_at_all() {
        let rule = PatternRule::parse("bash:*").expect("valid rule");
        assert!(
            !rule.matches("bash", "ls -la"),
            "even the wildcard must not authorize an ordinary, benign command"
        );
        assert!(!rule.matches("bash", "ls -la && rm -rf /"));
    }

    #[test]
    fn metacharacter_detection_covers_the_documented_set() {
        // `contains_shell_metacharacters` is no longer part of THIS
        // module's own allow decision (see `SHELL_METACHARACTERS`'s own
        // doc) -- it remains unit-tested here because `conway::gates::
        // AllowListGate` still calls it directly for the unrelated
        // `-p`/`--allowed-tools` one-shot gate.
        for c in [";", "&", "|", "`", "$", "<", ">", "(", ")", "{", "}"] {
            assert!(
                contains_shell_metacharacters(&format!("echo hi{c}")),
                "{c:?} must be treated as a metacharacter"
            );
        }
        assert!(contains_shell_metacharacters("echo hi\nrm -rf /"));
        assert!(!contains_shell_metacharacters("git status --short"));
    }

    /// The sanitizer's placeholder is itself a metacharacter, for
    /// [`contains_shell_metacharacters`]'s surviving caller (`conway::
    /// gates::AllowListGate`, the `-p`/`--allowed-tools` one-shot gate --
    /// see that function's own doc). A control character laundered into
    /// [`SANITIZED_CONTROL_PLACEHOLDER`] by `conway_core::text::
    /// sanitize_control_chars` must still register as disqualifying there.
    #[test]
    fn the_sanitizer_placeholder_is_treated_as_a_metacharacter() {
        assert!(
            contains_shell_metacharacters("git status\u{FFFD} rm -rf /"),
            "the sanitizer's placeholder must be detected, or a laundered \
             control char slips past a caller relying on this predicate"
        );
        // Sanity: the placeholder constant is the same one the sanitizer
        // emits -- if this ever drifts, the assertion above would still
        // pass on a different char and stop protecting the real seam.
        assert_eq!(
            sanitize_control_chars("a\nb"),
            format!("a{SANITIZED_CONTROL_PLACEHOLDER}b")
        );
    }

    // ---- RenderKind ----

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
        let rule = PatternRule::parse(r#"report:report({"summary":"build"#).expect("valid rule");

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

    /// `ShellCommand` is the behavior every existing test above already
    /// pins via the conservative `matches` -- this test only makes explicit
    /// that `matches_render(.., ShellCommand)` agrees with `matches` (which
    /// is defined IN TERMS OF `matches_render(.., ShellCommand)`), so the
    /// two can never silently drift. Both sides are `false` for every case
    /// -- content no longer distinguishes them.
    #[test]
    fn shell_command_render_kind_behaves_identically_to_the_conservative_matches() {
        let rule = PatternRule::parse("bash:git status").expect("valid rule");
        for rendered in ["git status", "git status --short", "git status && rm -rf /"] {
            assert_eq!(
                rule.matches("bash", rendered),
                rule.matches_render("bash", rendered, RenderKind::ShellCommand),
                "matches() and matches_render(.., ShellCommand) must never disagree: {rendered:?}"
            );
            assert!(
                !rule.matches("bash", rendered),
                "and that agreed-upon answer must be `false`, for every case: {rendered:?}"
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
        assert!(!rule.matches_render("write", r#"write({"path":"a"})"#, RenderKind::Structured,));

        let prefix_rule = PatternRule::parse("read:read(specific)").expect("valid rule");
        assert!(!prefix_rule.matches_render(
            "read",
            r#"read({"path":"other.rs"})"#,
            RenderKind::Structured,
        ));
    }

    // ---- adversarial prefix cases ----
    //
    // A `bash`/`ShellCommand` grant no longer matches ANYTHING (see above),
    // so prefix-matching semantics -- subcommand mismatch, token-boundary
    // discipline, tool-name discipline -- are no longer meaningfully
    // exercisable through it; testing them against a `bash` rule here would
    // trivially pass without exercising `prefix_matches`'s actual logic
    // (every case is `false` regardless). These cases move to a
    // `RenderKind::Structured` tool, where prefix matching still functions
    // exactly as documented, plus a direct exercise of the private
    // `prefix_matches` helper itself so the semantics stay pinned
    // independent of any `RenderKind`.

    /// A grant for a two-token prefix must not permit a different second
    /// token, however similar, nor a shorter one -- proven directly against
    /// `prefix_matches`, the function every `CommandPrefix` evaluation
    /// (`bash`'s included, when it was still reachable) delegates to.
    #[test]
    fn prefix_matches_does_not_permit_a_different_or_shorter_token() {
        assert!(prefix_matches("git status", "git status"));
        assert!(prefix_matches("git status", "git status --short"));
        assert!(
            !prefix_matches("git status", "git push --force"),
            "a different subcommand must not be covered"
        );
        assert!(
            !prefix_matches("git status", "git stat"),
            "a shorter token must not be covered"
        );
    }

    /// Token-wise, not byte-wise: `git status` must not match
    /// `git statusfoo`, even though that string does start with it -- also
    /// proven directly against `prefix_matches`.
    #[test]
    fn prefix_matches_respects_token_boundaries_not_raw_bytes() {
        assert!(
            !prefix_matches("git status", "git statusfoo"),
            "a byte-prefix match would wrongly allow this"
        );
        assert!(
            !prefix_matches("git status", "gitstatus"),
            "token boundaries must be respected on the first token too"
        );
    }

    // The identical properties, exercised through a `Structured` tool's
    // `PatternRule` (the one `RenderKind` where a prefix grant still
    // exists at all), are already pinned by
    // `a_structured_tools_prefix_rule_matches_token_wise_despite_json_syntax`
    // above.

    /// A `RenderKind::Structured` grant still never matches a different
    /// tool, and a `RenderKind::ShellCommand` grant matches no tool at
    /// all -- the latter following trivially from "matches nothing", but
    /// pinned explicitly so a future change narrowing the refusal by tool
    /// name (rather than leaving it total) would be caught here.
    #[test]
    fn a_rule_never_matches_a_different_tool() {
        let structured = PatternRule::parse("read:*").expect("valid rule");
        assert!(!structured.matches_render(
            "write",
            r#"write({"path":"a"})"#,
            RenderKind::Structured
        ));

        let shell = PatternRule::parse("bash:git status").expect("valid rule");
        assert!(!shell.matches("edit", "git status"));
        assert!(!shell.matches("bash", "git status"));
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

    // ---- deny: the asymmetric half ----

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
    /// no such parameter, so a `Structured` tool's JSON-dump rendering is
    /// denied without hesitation, unaffected by the ALLOW-side gate that
    /// (via the conservative `matches`, which always assumes
    /// `ShellCommand`) refuses the same rendering on the allow side.
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
        // consider this rendering -- it always assumes `ShellCommand`,
        // which this module's allow gate refuses unconditionally -- proving
        // the asymmetry is real, not just documented.
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

    // ---- sanitizer laundering ----

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
/// ## `ShellCommand`: no offer at all
///
/// **Always returns `None`.** A durable pattern grant no longer exists for
/// a [`RenderKind::ShellCommand`] tool at all (see this module's own doc,
/// "AMENDED" section) -- [`Rule::gate_allows`] refuses every one
/// unconditionally, regardless of what the offered prefix would have
/// named. Offering a grant this module would then always refuse to honor
/// would be worse than merely "confusing": it is the exact shape of bug
/// this codebase has already shipped once (`68ea9b1`, a `read:*` grant
/// that matched nothing, ever) -- a rule the operator believes they
/// installed doing nothing, silently. The operator's remaining options for
/// a shell command are the ones offered elsewhere in the prompt: allow it
/// once, deny it, prompt on it, or confine what it can reach with a
/// containment primitive; none of those are a [`PatternRule`] this
/// function has any business returning.
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
pub fn suggested_rule(
    tool: &str,
    // Kept in the signature (rather than dropped) so this function's shape
    // stays aligned with `PatternRule::matches_render`'s and every caller
    // (the TUI's `offered_permission_rule`/permission-prompt view) keeps
    // compiling unchanged -- see this function's own doc, "ShellCommand: no
    // offer at all". No `RenderKind` branch reads the call's rendered text
    // to decide the offer any more: `Structured` never did, and
    // `ShellCommand` no longer does either.
    _rendered: &str,
    render_kind: RenderKind,
) -> Option<PatternRule> {
    if render_kind == RenderKind::Structured {
        return Some(PatternRule {
            tool: tool.to_string(),
            command_prefix: "*".to_string(),
        });
    }
    // `ShellCommand`, and any future `RenderKind` variant (`RenderKind` is
    // `#[non_exhaustive]`): never offer a pattern grant. See this
    // function's own doc.
    None
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
///
/// ## Deliberately NOT `#[serde(deny_unknown_fields)]`
///
/// The type that must reject a misspelled key loudly is
/// `parse_permission_file`'s inner `RawPermissionFile`, which is what
/// `parse_rules`/`parse_deny_rules`/`parse_prompt_rules` (the LOAD path --
/// `Conway::load_permission_files` calls them, gated further by
/// `permission_file_unknown_field_error`) actually deserialize. `PermissionFile`
/// itself is not that path: it is the ROUND-TRIP type `Conway`'s revoke
/// rewrite (`rewrite_permission_file_removing[_structured]`) and the TUI's
/// best-effort "always allow" append (`persist_permission_rule`) parse an
/// EXISTING, already-on-disk file back into before writing it out again --
/// and both are conway-authored writers reading conway-authored files back,
/// exactly the version-skew case `docs/plugins/compatibility.md` calls out
/// for a file exchanged across binary versions, not the hand-typed-key case
/// `deny_unknown_fields` protects against. The append path is the sharper
/// hazard: it already treats ANY parse failure as "file was empty" and
/// OVERWRITES it with just the new rule (its own doc states this
/// explicitly); if this type rejected an unrecognized field, a newer
/// conway build's added field, read back by an older build appending one
/// grant, would silently discard every OTHER rule the file held --
/// including its `deny` rules -- a strictly worse outcome than the gap this
/// item closes (a field this type doesn't know about is not itself
/// preserved through the round trip either way -- neither variant has a
/// catch-all for it -- so leniency here buys "the append does not nuke the
/// rest of the file", not "the unknown field survives"). Keeping
/// `PermissionFile` tolerant of an unrecognized field avoids that data
/// loss, at the cost of not itself catching a typo in a file only ever
/// reached through this type -- which does not happen: every load path
/// reaches the file through `RawPermissionFile` first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionFile {
    /// Wire-form rules that AUTHORIZE. Malformed entries are dropped on
    /// load, not guessed at. From a project-scoped file, a caller MUST
    /// confirm a recorded trust decision before installing these -- see
    /// this module's own doc, and the trust-model design
    #[serde(default)]
    pub allow: Vec<String>,
    /// Wire-form rules that REFUSE, applied immediately regardless of
    /// trust --. `#[serde(default)]`
    /// so every file written before this field existed keeps parsing
    /// unchanged.
    #[serde(default)]
    pub deny: Vec<String>,
    /// F12: structured rules, each carrying its own `then` (`allow`/`prompt`/
    /// `deny`). The flat `allow`/`deny` lists above are the surface syntax for
    /// the `Tools([t]) + (Always | CommandPrefix) + (Allow | Deny)` subset;
    /// this array is the superset, expressing `paths_under`, `categories`,
    /// `category_in`, and the `prompt` effect the flat form has no syntax for.
    /// `allow` entries from this array are subject to the SAME trust decision
    /// as the flat `allow` list (a project file's `then: allow` rules install
    /// only once the caller confirms trust); `deny` and `prompt` entries apply
    /// immediately, trusted or not, the same asymmetry the flat `deny` list has
    /// always had. A structurally malformed entry is dropped, not guessed at --
    /// the file is untrusted input; a rule whose `then` is an unrecognized
    /// variant is dropped the same way (`#[non_exhaustive]` fail-closed).
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
    parse_permission_file(contents)
        .map(|f| f.allow)
        .unwrap_or_default()
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
    parse_permission_file(contents)
        .map(|f| f.deny)
        .unwrap_or_default()
}

/// Parses a rules file's PROMPT rules -- structured `rules` entries whose
/// `then` is `Prompt`. The flat form has no prompt syntax (a flat `deny`
/// was the only narrowing effect the flat language could express before
/// F12), so this is drawn entirely from the structured `rules` array. Same
/// fail-closed posture as [`parse_rules`]/[`parse_deny_rules`].
pub fn parse_prompt_rules(contents: &str) -> Vec<Rule> {
    parse_permission_file(contents)
        .map(|f| f.prompt)
        .unwrap_or_default()
}

/// Shared parse used by [`parse_rules`], [`parse_deny_rules`], and
/// [`parse_prompt_rules`], so the three halves can never read the JSON
/// differently.
struct ParsedPermissionFile {
    allow: Vec<Rule>,
    deny: Vec<Rule>,
    prompt: Vec<Rule>,
    /// Top-level keys `RawPermissionFile` does not recognize, captured by
    /// its `#[serde(flatten)]` catch-all rather than detected from a
    /// `serde_json::Error`'s message text -- see
    /// [`permission_file_unknown_field_error`]'s own doc for why the
    /// structural signal replaced the string one. Non-empty here means the
    /// whole file is rejected: `allow`/`deny`/`prompt` above are left at
    /// their empty default rather than populated from whatever the
    /// correctly-spelled keys said, matching `Conway::load_permission_files`'s
    /// "a typo rejects the whole file" contract even for a caller that
    /// invokes [`parse_rules`]/[`parse_deny_rules`]/[`parse_prompt_rules`]
    /// directly, bypassing that gate.
    unknown_keys: Vec<String>,
}

/// Parses the top-level shape of a permissions file, **failing closed on every
/// error** -- the file is untrusted input. Still returns `Result` (not just the
/// parsed value) because a `serde_json::Error` is possible for two reasons this
/// function's callers must keep telling apart: content that is not valid JSON
/// at all, and a type mismatch on a RECOGNIZED field (`"allow":
/// "not-an-array"`) -- both keep the existing SILENT fail-closed posture. A
/// misspelled key is NOT one of those `Err` cases any more: it is reported
/// structurally, via [`ParsedPermissionFile::unknown_keys`], not by matching a
/// `serde_json::Error`'s message text -- see
/// [`permission_file_unknown_field_error`]'s own doc for why the string match
/// this replaced was fragile.
///
/// Parses `rules` as an array of opaque JSON values, then deserializes each
/// one individually: a single structurally malformed `rules` entry is
/// DROPPED, not guessed at, and the rest of the array plus the flat
/// `allow`/`deny` lists survive. Parsing the whole document as
/// `PermissionFile` (whose `rules: Vec<Rule>` is all-or-nothing under serde)
/// would reject the entire file on one bad `rules` entry -- a louder but
/// less useful failure, and one the field's own doc disclaims. The flat
/// `allow`/`deny` lists are already drop-per-entry by construction
/// (`PatternRule::parse` returns `Option`); this gives `rules` the same
/// granular fail-closed posture.
///
/// `RawPermissionFile` -- unlike the sibling public [`PermissionFile`] --
/// carries a `#[serde(flatten)]` catch-all `extra` map instead of
/// `#[serde(deny_unknown_fields)]` (the two are mutually incompatible in
/// serde, and the catch-all is what lets an unknown key be reported
/// structurally rather than by string-matching the error `deny_unknown_fields`
/// would have produced). This inner type is the one that ACTUALLY
/// deserializes a permissions file being loaded for installation (via
/// [`parse_rules`]/[`parse_deny_rules`]/[`parse_prompt_rules`], which
/// `Conway::load_permission_files` calls), so it is the type where a
/// misspelled `"denys"` must be caught, not [`PermissionFile`] itself (see
/// that type's own doc for why IT stays lenient).
fn parse_permission_file(contents: &str) -> Result<ParsedPermissionFile, serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct RawPermissionFile {
        #[serde(default)]
        allow: Vec<String>,
        #[serde(default)]
        deny: Vec<String>,
        #[serde(default)]
        rules: Vec<serde_json::Value>,
        // Catches every top-level key above does not name. Non-empty is the
        // ENTIRE signal `permission_file_unknown_field_error` acts on --
        // structural, so it survives a wording change in serde/serde_json's
        // own error text (the fragility's
        // review found in the string-matching predecessor of this field).
        #[serde(flatten)]
        extra: serde_json::Map<String, serde_json::Value>,
    }
    let file: RawPermissionFile = serde_json::from_str(contents)?;
    let mut unknown_keys: Vec<String> = file.extra.keys().cloned().collect();
    unknown_keys.sort();
    if !unknown_keys.is_empty() {
        // Fail closed on the whole file, same as the `deny_unknown_fields`
        // `Err` this replaced: a file naming an unrecognized key installs
        // NOTHING, not even its correctly-spelled `allow`/`deny`/`rules` --
        // matching `Conway::load_permission_files`'s "a typo rejects the
        // whole file" contract even for a caller that reaches
        // `parse_rules`/`parse_deny_rules`/`parse_prompt_rules` directly.
        return Ok(ParsedPermissionFile {
            allow: Vec::new(),
            deny: Vec::new(),
            prompt: Vec::new(),
            unknown_keys,
        });
    }
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
    Ok(ParsedPermissionFile {
        allow,
        deny,
        prompt,
        unknown_keys,
    })
}

/// Whether `contents` fails to parse specifically because it names a
/// top-level key this schema does not recognize (`"denys"` for `"deny"`, or
/// any other typo) --.
///
/// Deliberately narrower than "any parse failure": content that is not valid
/// JSON at all, or that names the RIGHT key with the WRONG shape (`"allow":
/// "not-an-array"`), keeps the existing SILENT fail-closed posture
/// [`parse_rules`]/[`parse_deny_rules`]/[`parse_prompt_rules`] have always had
/// for those cases (the floor for untrusted input: a bad file
/// authorizes/denies/ prompts nothing, which costs at most an extra prompt). A
/// MISSPELLED key is a different failure in kind, not degree: it is not
/// "malformed input" but an operator who wrote a `deny` rule, typo'd the one
/// word that makes it apply, and would otherwise never learn the rule was never
/// installed
/// -- exactly the asymmetry this module's own doc states (`allow` requires
/// trust; `deny` always applies, so a `deny` that silently never matches is
/// the specific failure mode that guarantee exists to rule out).
///
/// `Conway::load_permission_files` calls this for every candidate file and,
/// when it returns `Some`, skips installing ANY rule from that file --
/// allow, deny, AND prompt -- rather than partially installing around the
/// typo, matching `settings.json`'s own "a bad key rejects the whole file"
/// precedent (`crates/conway/tests/config_precedence.rs`'s
/// `typo_d_key_is_rejected_by_deny_unknown_fields`).
///
/// The signal is STRUCTURAL, not textual: `RawPermissionFile`'s
/// `#[serde(flatten)] extra` catch-all collects every key the schema does
/// not name, and this function reports `Some` exactly when that map is
/// non-empty. Earlier this discriminated the typo case by matching the
/// literal string `"unknown field"` inside a `serde_json::Error`'s message
/// -- wording that is neither serde's nor serde_json's semver contract, so a
/// future dependency bump changing it would silently fall this function back
/// to `None` for a genuinely typo'd file, restoring the exact silent-zero-
/// deny-rules bug exists to close. The
/// catch-all field depends on no error text at all.
pub fn permission_file_unknown_field_error(contents: &str) -> Option<String> {
    let file = parse_permission_file(contents).ok()?;
    if file.unknown_keys.is_empty() {
        return None;
    }
    let quoted: Vec<String> = file
        .unknown_keys
        .iter()
        .map(|key| format!("`{key}`"))
        .collect();
    Some(format!(
        "unknown field{} {}, expected one of `allow`, `deny`, `rules`",
        if file.unknown_keys.len() == 1 {
            ""
        } else {
            "s"
        },
        quoted.join(", "),
    ))
}

/// Where an installed [`PatternRule`] grant came from. Required so a rule
/// set is inspectable — `PermissionBroker::active_patterns()`'s own doc
/// already states the principle: "a rule set nobody can inspect is a
/// trap."
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
    /// the trust-model design and this module's own doc). A
    /// DENY rule with this origin may have come from an UNTRUSTED file:
    /// deny applies regardless (§3).
    File(PathBuf),
    /// A4: contributed by a plugin. An allow rule is a durable grant, and
    /// grants belong to the operator, so a plugin may only contribute
    /// NARROWING rules (`deny`/`prompt`) -- extension-architecture.md §5.5
    /// stage 1. The broker's `remember_pattern_rule` (the allow admission)
    /// rejects `Then::Allow` for this origin as a STRUCTURAL guard, so a
    /// future plugin transport that reuses `PatternOrigin::Plugin` to call
    /// the allow path with `Then::Allow` is refused at the broker boundary
    /// rather than silently installing -- the invariant rests on a guard,
    /// not on the absence of a transport. `deny`/`prompt` rules with this
    /// origin install unconditionally (they only narrow). Carries no file
    /// path: a plugin-contributed rule has no on-disk permissions file to
    /// rewrite on revoke (and revocation is an allow-rule surface anyway,
    /// which a plugin rule can never be).
    Plugin,
}

impl PatternOrigin {
    /// A short label for a review surface (`/settings`'s grant list is the
    /// only consumer today).
    pub fn describe(&self) -> String {
        match self {
            PatternOrigin::Interactive => "interactive".to_string(),
            PatternOrigin::File(path) => path.display().to_string(),
            PatternOrigin::Plugin => "plugin".to_string(),
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

    /// Untrusted input, with the bias that matters here: a corrupt file must fail
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

    // ---- typo'd keys ----

    /// **The headline regression.** A misspelled `"denys"` key must not
    /// silently install zero deny rules with nothing telling the operator
    /// -- `permission_file_unknown_field_error` must catch it, naming the
    /// key, so `Conway::load_permission_files` can refuse the whole file
    /// loudly instead of quietly enforcing nothing.
    ///
    /// A check is not established until it has been shown to fail, so this is
    /// paired with `a_correctly_spelled_deny_key_installs_the_
    /// rule_the_typo_would_have_dropped`, a CONTROL case whose deny list is
    /// non-empty on success -- so "zero rules installed" here is evidence
    /// of the typo being caught, not evidence the fixture never had any
    /// rules to install in the first place.
    #[test]
    fn a_misspelled_deny_key_is_reported_rather_than_silently_installing_nothing() {
        let contents = r#"{"denys": ["bash:curl"]}"#;

        // The observable an operator would see: the rule they wrote never
        // installs.
        assert!(
            parse_deny_rules(contents).is_empty(),
            "the typo'd key must not be guessed at -- `deny` stays at its default"
        );

        // The observable that makes the miss LOUD instead of silent.
        let err = permission_file_unknown_field_error(contents)
            .expect("a misspelled top-level key must be reported, not swallowed");
        assert!(
            err.contains("denys"),
            "the error must name the offending key: {err}"
        );
    }

    /// The control case that makes the guard above a real check: the SAME
    /// rule, correctly spelled,
    /// actually installs -- proving the typo test above is distinguishing
    /// "silently dropped" from "there was nothing to install", not just
    /// asserting an empty `Vec` that would be empty either way.
    #[test]
    fn a_correctly_spelled_deny_key_installs_the_rule_the_typo_would_have_dropped() {
        let contents = r#"{"deny": ["bash:curl"]}"#;
        let deny = parse_deny_rules(contents);
        assert_eq!(
            deny.len(),
            1,
            "the correctly spelled key must install its rule"
        );
        assert_eq!(deny[0].to_wire(), "bash:curl");
        assert!(
            permission_file_unknown_field_error(contents).is_none(),
            "a file with only recognized keys must not be reported as bad"
        );
    }

    /// Not special-cased to `deny` -- any unrecognized top-level key is
    /// caught the same way, including one that would otherwise silently
    /// widen `allow` instead of narrowing `deny`.
    #[test]
    fn any_unrecognized_top_level_key_is_reported_not_just_denys() {
        for (contents, bad_key) in [
            (r#"{"allows": ["bash:git status"]}"#, "allows"),
            (r#"{"rulez": []}"#, "rulez"),
            (
                r#"{"deny": ["bash:curl"], "extra_field": true}"#,
                "extra_field",
            ),
        ] {
            let err = permission_file_unknown_field_error(contents)
                .unwrap_or_else(|| panic!("must be reported: {contents:?}"));
            assert!(err.contains(bad_key), "error must name {bad_key:?}: {err}");
        }
    }

    /// A file with only recognized keys -- including one that predates
    /// `rules` and only ever set `allow`/`deny` -- must never be flagged.
    /// The strictness this item adds must not regress a document example
    /// or a file written before a later field existed.
    #[test]
    fn a_well_formed_file_is_never_reported_as_having_an_unknown_field() {
        for contents in [
            "{}",
            r#"{"allow": ["bash:cargo test", "read:*"]}"#,
            r#"{"deny": ["bash:curl", "bash:ssh"]}"#,
            r#"{"allow": ["bash:cargo test"], "deny": ["bash:curl"]}"#,
            r#"{"allow": ["bash:cargo test"], "rules": [
                {"select": {"tools": ["read"]}, "when": "always", "then": "allow"}
            ]}"#,
        ] {
            assert!(
                permission_file_unknown_field_error(contents).is_none(),
                "a well-formed file must not be reported: {contents}"
            );
        }
    }

    /// Genuinely corrupt JSON (not a typo'd key) keeps its EXISTING silent
    /// posture -- `permission_file_unknown_field_error` must not fire for
    /// it, or every one of `a_corrupt_file_authorizes_nothing_rather_than_
    /// everything`'s cases would newly become loud load errors, which is a
    /// behavior change this item does not make.
    #[test]
    fn genuinely_malformed_json_is_not_reported_as_an_unknown_field() {
        for corrupt in [
            "",
            "not json at all",
            "{",
            r#"{"allow": "not-an-array"}"#,
            r#"{"allow": [123]}"#,
            "null",
        ] {
            assert!(
                permission_file_unknown_field_error(corrupt).is_none(),
                "not the failure mode this function reports on: {corrupt:?}"
            );
        }
    }

    // ---- V2b: the offered rule ----

    /// **The headline offer-side fix.** No pattern is ever offered for a
    /// `ShellCommand` call any more -- not for an ordinary two-token
    /// command, not for a single-token command, not for an empty one, and
    /// not for a chained one. Offering a grant this module would then
    /// always refuse to honor would be worse than confusing; see this
    /// function's own doc, "ShellCommand: no offer at all".
    #[test]
    fn no_pattern_is_ever_offered_for_a_shell_command() {
        for rendered in [
            "git status --short",
            "pwd",
            "",
            "git status && rm -rf /",
            "git status",
        ] {
            assert!(
                suggested_rule("bash", rendered, RenderKind::ShellCommand).is_none(),
                "a ShellCommand tool must never be offered a pattern grant: {rendered:?}"
            );
        }
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
    /// must not suppress the offer (unlike `ShellCommand`, where the offer
    /// is suppressed unconditionally, for every rendering, regardless of
    /// content -- see `no_pattern_is_ever_offered_for_a_shell_command`
    /// above).
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
    /// conservative "offer nothing" behavior (offer less, never more),
    /// which is exactly what the `_` arm delivers -- pinned here so the
    /// fallback is a decision, not an accident.
    #[test]
    fn the_offer_falls_back_to_offering_nothing_for_unknown_kinds() {
        // `ShellCommand` today exercises the same `_` arm any future
        // non-`Structured` variant would fall into; if a third kind ever
        // appears this test keeps compiling and keeps meaning "no offer".
        let kind = RenderKind::ShellCommand;
        assert!(suggested_rule("bash", "git status && rm -rf /", kind).is_none());
        assert!(suggested_rule("bash", "git status --short", kind).is_none());
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
        let r = PatternRule::parse("read:*")
            .expect("valid")
            .to_rule(Then::Allow);
        assert_eq!(r.select, Select::Tools(vec!["read".to_string()]));
        assert_eq!(r.when, When::Always);
        assert_eq!(r.then, Then::Allow);
    }

    #[test]
    fn prefix_flat_rule_desugars_to_command_prefix() {
        let r = PatternRule::parse("bash:git status")
            .expect("valid")
            .to_rule(Then::Deny);
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
    /// render kinds. turned this from
    /// an OBSERVED equivalence (two independent implementations, pinned by
    /// this test) into a STRUCTURAL one: `PatternRule::matches_render` now
    /// literally calls `Rule::matches_allow_render` (via `to_rule`), so this
    /// test is now a tautology by construction -- kept anyway as a
    /// regression guard against a future edit un-inlining the delegation.
    #[test]
    fn flat_and_structured_produce_byte_identical_allow_decisions() {
        // A matrix of (rule, tool, rendered, render_kind): ordinary matches,
        // subcommand mismatches, chained commands (gated), Structured
        // wildcards, a non-wildcard prefix on a Structured tool.
        let cases: &[(&str, &str, &str, RenderKind, ToolCategory)] = &[
            (
                "bash:git status",
                "bash",
                "git status",
                RenderKind::ShellCommand,
                ToolCategory::Execute,
            ),
            (
                "bash:git status",
                "bash",
                "git status --short",
                RenderKind::ShellCommand,
                ToolCategory::Execute,
            ),
            (
                "bash:git status",
                "bash",
                "git push --force",
                RenderKind::ShellCommand,
                ToolCategory::Execute,
            ),
            (
                "bash:git status",
                "bash",
                "git status && rm -rf /",
                RenderKind::ShellCommand,
                ToolCategory::Execute,
            ),
            (
                "bash:git status",
                "bash",
                "git status\nrm -rf /",
                RenderKind::ShellCommand,
                ToolCategory::Execute,
            ),
            (
                "bash:*",
                "bash",
                "ls -la",
                RenderKind::ShellCommand,
                ToolCategory::Execute,
            ),
            (
                "bash:*",
                "bash",
                "ls -la && rm -rf /",
                RenderKind::ShellCommand,
                ToolCategory::Execute,
            ),
            (
                "read:*",
                "read",
                r#"read({"path":"a.rs"})"#,
                RenderKind::Structured,
                ToolCategory::Read,
            ),
            (
                "read:*",
                "write",
                r#"write({"path":"a.rs"})"#,
                RenderKind::Structured,
                ToolCategory::Edit,
            ),
            (
                r#"report:report({"summary":"build"#,
                "report",
                r#"report({"summary":"build finished"})"#,
                RenderKind::Structured,
                ToolCategory::Think,
            ),
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
    /// ungated, laundering-aware evaluator is one, not two. Same
    /// tautology-by-construction note as the allow-side test above applies
    /// here: `matches_deny` now literally calls `matches_deny_render`.
    #[test]
    fn flat_and_structured_produce_byte_identical_deny_decisions() {
        let cases: &[(&str, &str, &str, ToolCategory)] = &[
            (
                "bash:curl",
                "bash",
                "curl https://example.com",
                ToolCategory::Execute,
            ),
            (
                "bash:curl",
                "bash",
                "curl x; rm -rf /",
                ToolCategory::Execute,
            ),
            (
                "bash:curl",
                "bash",
                "\tcurl http://evil",
                ToolCategory::Execute,
            ),
            (
                "bash:*",
                "bash",
                "ls -la && rm -rf /",
                ToolCategory::Execute,
            ),
            (
                "write:*",
                "write",
                r#"write({"path":"/etc/passwd"})"#,
                ToolCategory::Edit,
            ),
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

    /// Pins `FLAT_FORM_CATEGORY_PLACEHOLDER`'s own justification: for every
    /// shape `PatternRule::to_rule` can ever produce (`Select::Tools` +
    /// `When::Always`/`CommandPrefix`), `matches_render`/`matches_deny`'s
    /// result must not depend on WHICH `ToolCategory` the delegation passes
    /// through -- if it ever did, the placeholder would be silently wrong
    /// for at least one category, and this is the test that would catch it.
    #[test]
    fn flat_matches_render_result_is_independent_of_the_placeholder_category() {
        let all_categories = [
            ToolCategory::Read,
            ToolCategory::Edit,
            ToolCategory::Delete,
            ToolCategory::Move,
            ToolCategory::Search,
            ToolCategory::Execute,
            ToolCategory::Think,
            ToolCategory::Fetch,
            ToolCategory::Delegate,
        ];
        let cases: &[(&str, &str, &str, RenderKind)] = &[
            (
                "bash:git status",
                "bash",
                "git status",
                RenderKind::ShellCommand,
            ),
            (
                "bash:git status",
                "bash",
                "git status && rm -rf /",
                RenderKind::ShellCommand,
            ),
            (
                "read:*",
                "read",
                r#"read({"path":"a.rs"})"#,
                RenderKind::Structured,
            ),
        ];
        for (wire, tool, rendered, rk) in cases {
            let rule = PatternRule::parse(wire).expect("valid");
            let allow = rule.to_rule(Then::Allow);
            let deny = rule.to_rule(Then::Deny);
            let expected_allow =
                allow.matches_allow_render(tool, ToolCategory::Read, rendered, *rk);
            let expected_deny = deny.matches_deny_render(tool, ToolCategory::Read, rendered);
            for cat in all_categories {
                assert_eq!(
                    allow.matches_allow_render(tool, cat, rendered, *rk),
                    expected_allow,
                    "allow decision for {wire:?} vs {rendered:?} must not depend on category \
                     ({cat:?} disagreed)"
                );
                assert_eq!(
                    deny.matches_deny_render(tool, cat, rendered),
                    expected_deny,
                    "deny decision for {wire:?} vs {rendered:?} must not depend on category \
                     ({cat:?} disagreed)"
                );
            }
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
        assert!(allow.iter().any(|r| matches!(r.when, When::Always)
            && matches!(r.select, Select::Tools(ref t) if t == &["read".to_string()])));
        assert!(allow
            .iter()
            .any(|r| matches!(r.when, When::CommandPrefix(_))));

        let deny = parse_deny_rules(contents);
        assert_eq!(deny.len(), 1, "one structured deny");
        assert!(matches!(deny[0].select, Select::Categories(_)));
        assert!(matches!(deny[0].when, When::PathsUnder(_)));

        let prompt = parse_prompt_rules(contents);
        assert_eq!(prompt.len(), 1, "one structured prompt");
        assert!(matches!(prompt[0].then, Then::Prompt));
    }

    /// A structurally malformed `rules` entry is dropped, not guessed at
    /// (untrusted input) -- the rest of the array and the flat lists survive.
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
        assert!(
            !tool_pattern_matches("re*", "grep"),
            "trailing * is a prefix, not infix"
        );
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
