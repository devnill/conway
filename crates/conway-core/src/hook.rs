//! Domain types for one hook invocation -- the EVENT NAME + PAYLOAD a hook is invoked
//! with, and the ANSWER it may return -- kept deliberately separable from
//! the INVOCATION MODALITY that actually delivers them. See
//! [`crate::ports::HookRunner`] for the port that performs an invocation
//! (a PORT, not a type here, because performing one is I/O -- this crate
//! does none).
//!
//! **Today's modality: one-shot.** A
//! runner spawns the hook's configured command fresh per event, writes
//! [`HookEvent`] to its stdin as JSON, and reads a [`HookAnswer`] from its
//! stdout plus its exit status. This is deliberately NOT the long-lived
//! NDJSON JSON-RPC protocol the remote plugin transport uses (requiring
//! that would mean no plain shell script could ever be a hook), and other
//! invocation modalities are anticipated -- nothing in this module encodes
//! "one-shot" as part of what a hook conceptually receives or returns; that
//! is [`HookInvocation`]'s `command`/`timeout_ms` fields alone, and the
//! *outcome* an invocation reports is a
//! [`crate::error::HookFailure`]/[`HookAnswer`] pair, not "an exit code."

use serde::{Deserialize, Serialize};

/// The event name plus payload one hook invocation carries -- independent
/// of how it is delivered (module doc).
///
/// `#[non_exhaustive]`, with [`Self::new`] as the construction path. The
/// decision (board item finding 9, harness gap review 2026-09-01): this
/// type has only two fields, so a constructor costs the same one line a
/// literal would have, and it is nested inside [`HookInvocation`] at
/// nearly every external construction site already -- paying for growth
/// room here is the same payment [`HookInvocation`] itself has to make, not
/// an extra one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HookEvent {
    /// E.g. `"pre_tool_use"`, or a plugin-namespaced `"myplugin.foo"` --
    /// `crate::event_name::validate_event_name`'s vocabulary. **Not
    /// validated here**: that is the config-load-time (subscriber side) and
    /// declaration-time (plugin side) concern two OTHER own --
    /// see that function's own module doc. This runner is invoked with
    /// whatever name its caller already resolved.
    pub name: String,
    /// What the hook actually receives for this event. Untyped
    /// (`serde_json::Value`) because the concrete shape is event-specific,
    /// decided by whichever later item wires a concrete event onto this
    /// runner -- this item ships no event, so nothing here constrains the
    /// shape beyond "valid JSON."
    pub payload: serde_json::Value,
}

impl HookEvent {
    /// Construct one directly -- the ONLY way from outside this crate, now
    /// that `#[non_exhaustive]` forbids the struct-literal form (including
    /// `..` functional-update syntax) across the crate boundary. Two
    /// fields, positional, no builder: see the type's own doc for why that
    /// is enough here.
    pub fn new(name: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            payload,
        }
    }
}

/// What one hook invocation spawns and how long it is allowed to run.
///
/// `command` is an argv vector (program, then its arguments) -- never a
/// single shell string, matching `crates/conway/src/config/schema.rs`'s
/// `HookEntry::command` shape exactly, so no shell-quoting ambiguity exists
/// between config and what actually gets spawned.
///
/// `#[non_exhaustive]`, with [`Self::new`] as the construction path -- the
/// same decision [`HookEvent`]'s own doc explains, for the same reason:
/// three fields is still cheap for a constructor to name positionally, and
/// this is the type external `HookRunner` implementors build most often
/// (once per invocation), so the growth-room protection matters here more
/// than almost anywhere else in this module -- a hidden field added later
/// would otherwise break every third-party `HookRunner`'s call site, not
/// merely its own definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HookInvocation {
    pub command: Vec<String>,
    pub timeout_ms: u64,
    pub event: HookEvent,
}

impl HookInvocation {
    /// Construct one directly -- the ONLY way from outside this crate, now
    /// that `#[non_exhaustive]` forbids the struct-literal form. Three
    /// fields, positional, no builder.
    pub fn new(command: Vec<String>, timeout_ms: u64, event: HookEvent) -> Self {
        Self {
            command,
            timeout_ms,
            event,
        }
    }
}

/// One hook's answer to an invocation that succeeded (fail-closed:
/// everything else is a [`crate::error::HookFailure`] instead -- see
/// [`crate::ports::HookRunner`]'s own doc).
///
/// **Structurally cannot express "replace computed context wholesale"**
/// (a later decision, which supersedes an earlier one
/// whose reasoning was cacheability -- NOT the basis here). The only way to
/// change context is [`ContextDelta`]: append, or exclude by identifier,
/// never substitute. The load-bearing reason is **reconstructability**: the
/// prior state must remain recoverable from what was persisted, which a
/// free-form replacement value would destroy (there would be no way to
/// recover what a hook overwrote). Cacheability is a secondary consequence
/// of the same shape, not the justification -- caching is the inference
/// plugin's own responsibility, not this runner's.
///
/// **`permission`: a second,
/// independent axis, unrelated to context.** Only `pre_tool_use` dispatch
/// (`conway_runtime::permission::PermissionBroker::decide`) ever reads it;
/// every other event ignores the field completely, exactly as a hook that
/// never touches `context` already leaves that axis at its own default. See
/// [`HookPermissionVerdict`]'s own doc for why this field can narrow a
/// permission decision but can never widen one.
///
/// The default (no fields set) is the correct answer for a hook that has
/// nothing to say about context OR permission -- e.g. empty stdout on a
/// zero exit (see the implementing crate's parse rule) -- and is
/// indistinguishable from a hook that explicitly returned
/// `{"context":{},"permission":"no_opinion"}`.
///
/// `#[non_exhaustive]`, with [`Self::new`] as the construction path --
/// this is the type most likely to grow a THIRD axis someday (the module
/// doc above already frames `context`/`permission` as two independent
/// axes; a later one is exactly the kind of change this attribute exists
/// to make non-breaking for an external `HookRunner` implementor). This
/// changes only Rust-side struct-literal construction, not the wire
/// format: `Deserialize` is untouched by `#[non_exhaustive]`, so a hook
/// script's stdout keeps parsing exactly as before -- absent/partial JSON
/// still resolves through each field's own `#[serde(default)]` (or the
/// whole-struct `Default` for empty stdout), and an unknown JSON key is
/// still silently ignored (serde's ordinary non-`deny_unknown_fields`
/// leniency, see `an_unknown_replace_shaped_key_is_ignored_not_interpreted_as_a_replacement`
/// below).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HookAnswer {
    #[serde(default)]
    pub context: ContextDelta,
    #[serde(default)]
    pub permission: HookPermissionVerdict,
}

impl HookAnswer {
    /// Construct one directly -- the ONLY way from outside this crate to
    /// build a non-default value, now that `#[non_exhaustive]` forbids the
    /// struct-literal form (including `..HookAnswer::default()` functional
    /// update). For the common "everything defaulted" case, prefer
    /// [`HookAnswer::default`] directly; `new` is for a `HookRunner`
    /// implementation that has an actual opinion on one or both fields.
    pub fn new(context: ContextDelta, permission: HookPermissionVerdict) -> Self {
        Self {
            context,
            permission,
        }
    }
}

/// A `pre_tool_use` hook's opinion on whether the call it was invoked for
/// should be allowed to proceed.
///
/// **No `Allow` variant exists, anywhere in this type -- not "we only check
/// whether it denied."** Decision: a hook may
/// only NARROW a permission verdict, never widen one; it cannot grant an
/// allow that was not already going to happen, only add another way to say
/// no. `crate::permission_pattern::Then` enforces the same rule one layer
/// down for plugin-contributed pattern rules, but does so by *rejecting*
/// `Then::Allow` at admission time (`PermissionBroker::remember_pattern_rule`,
/// a runtime check a future edit could get wrong) -- this type takes the
/// strictly stronger option the same's spec asked for: there is
/// no `Allow` variant for a runtime check to fail to reject, so
/// `PermissionBroker::decide` structurally cannot treat a hook's answer as
/// a grant, independent of whether every call site keeps checking that
/// correctly.
///
/// `#[non_exhaustive]`: a future variant here is a live safety concern, not
/// a mere API nicety. [`Self::denies`] is the ONE place that decides what
/// an UNRECOGNIZED variant means -- fail closed, treated the same as
/// [`Self::Deny`] (an unknown answer from a hook is exactly as
/// untrustworthy as an unreachable one). Every reader of this type that
/// needs to know whether a verdict blocks a call -- today
/// `conway_runtime::permission::PermissionBroker::pre_tool_use_hook_denial`
/// (`pre_tool_use`) and `conway_runtime::hook_dispatch::
/// HookDispatcher::dispatch_deny_only` (`prompt_submitted` and any other
/// deny-only event) -- calls [`Self::denies`] rather than re-deriving the
/// judgment per call site: a safety-critical classification with exactly
/// one implementation. The two sites still format their own denial messages
/// (they differ in wording and in which hook/event they name), and the
/// `Deny` variant's own `reason` is still theirs to read directly when
/// present -- only the "does this verdict block the call at all,
/// including one this build has never seen" question is centralized here.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPermissionVerdict {
    /// This hook has no opinion on this call -- `decide()` proceeds exactly
    /// as it would have without this hook at all. The default: an absent
    /// `permission` key on the wire, and an explicit
    /// `{"permission":"no_opinion"}` are indistinguishable, exactly like
    /// [`HookAnswer`]'s own default/explicit-empty equivalence above.
    #[default]
    NoOpinion,
    /// Refuse the call outright. `reason` is folded into the rendered
    /// denial `PermissionBroker::decide` returns, alongside which hook
    /// produced it -- mirroring the phrasing its existing deny-pattern
    /// branch already uses for the identical purpose.
    Deny { reason: String },
}

impl HookPermissionVerdict {
    /// Whether this verdict blocks the call -- `false` only for
    /// [`Self::NoOpinion`]; `true` for [`Self::Deny`] AND for any variant
    /// added after this build shipped. The single implementation of the
    /// fail-closed-on-unrecognized-variant judgment this type's own doc
    /// requires -- a safety-critical classification with exactly one
    /// implementation -- every caller that needs to know whether a
    /// verdict blocks a call goes through this method rather than
    /// re-deriving the answer with its own `match`/`if let`.
    pub fn denies(&self) -> bool {
        !matches!(self, HookPermissionVerdict::NoOpinion)
    }
}

/// A `pre_tool_use` hook registration's own policy for what happens when
/// THIS hook's runner cannot be consulted at all -- a missing script, a
/// timeout, or stdout that failed to parse as a [`HookAnswer`]
/// (`crate::error::HookFailure`) -- as opposed to when the hook ran and
/// returned an explicit [`HookPermissionVerdict::Deny`].
///
/// **Those are two structurally different facts -- "the guard is down"
/// versus "the guard said no" -- and this type exists so a caller
/// (`conway_runtime::permission::PermissionBroker::decide`) never has to
/// collapse them into the same value again.** See
/// `docs/vision/DESIGN-permission-modes.md` §3a/§3c for the full argument;
/// the short version is that fail-closed is correct for an
/// operator-authored policy script (its breakage is the operator's own) but
/// wrong for a guard backed by infrastructure the operator does not
/// directly control (e.g. a local model server) -- there, every outage
/// should present as "the guard is unreachable," not as an unbroken stream
/// of per-call denials that look identical to the guard doing its job.
///
/// **No `Allow` variant exists, anywhere in this type -- the identical
/// guarantee [`HookPermissionVerdict`] makes for a hook's own verdict,
/// extended to its outage.** `Prompt` is not a widening: it forces the
/// operator's own gate, exactly the narrowing effect
/// `crate::permission_pattern::Then::Prompt` and the extension design's
/// `PluginPermissionVerdict::Prompt` already have -- and the operator's own
/// `Deny` rules and plan-mode refusal still outrank it (see
/// `PermissionBroker::decide`'s own ordering doc for exactly why the
/// existing step order makes that hold by construction, not merely by
/// convention).
///
/// `#[non_exhaustive]`, the same reasoning [`HookPermissionVerdict`] gets:
/// this is an outage-classification type a hook script's REGISTRATION
/// picks, so a future variant is exactly as safety-relevant as a future
/// verdict. `conway_runtime::permission::
/// PermissionBroker::pre_tool_use_hook_denial` is the one cross-crate
/// exhaustive `match` on this type, and its wildcard arm fails closed
/// (treated as [`Self::Deny`]) for the identical reason.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOnFailure {
    /// The runner's failure is treated exactly as an explicit refusal --
    /// today's only behavior, and still the default, so an existing
    /// registration that never sets this field denies on outage byte-for-
    /// byte unchanged: the operator wrote the hook, its breakage is theirs.
    #[default]
    Deny,
    /// The runner's failure forces the operator's own gate instead of
    /// denying outright. The safe resting state for a guard whose
    /// availability the operator does not fully control: when it cannot be
    /// consulted, the operator ends up exactly where they would be had they
    /// never installed the guard at all -- asked, never silently blocked
    /// and never silently widened.
    Prompt,
}

/// Where a dispatched hook rule came from -- an operator's own merged
/// `[hooks].rules[]` entry, or an installed plugin's own
/// `Plugin::hooks()` declaration (board item
/// `01M129QW0GV90QTQS6B3BY3DAR`).
///
/// **Why this exists at all: a plugin's hooks reach dispatch at the SAME
/// tier a config-declared one does -- no privileged, no second-class
/// surface -- but that means a downloaded plugin's hook can
/// now deny a real tool call or a submitted prompt exactly as an
/// operator-authored rule can, with nothing distinguishing the two once
/// they are both just entries in a dispatch list.** Before this type
/// existed, `crates/conway/src/conway.rs`'s `HookRuleView::origin` had
/// exactly one honest value to report (`"settings.json (merged config)"`)
/// because `[hooks].rules[]` really was the only place a hook rule could
/// come from -- see that constant's own doc for the reasoning this type's
/// addition supersedes. Once a plugin can register a hook directly, that
/// claim would silently become false for a plugin-contributed rule unless
/// something threads the real source through to the same review surface;
/// this is that something. An operator must be able to inspect every
/// active rule, including one contributed by an untrusted repo, which
/// is the reason this is a real field carried on every dispatched hook
/// spec, not a comment.
///
/// **No variant here can ever be more permissive than another** -- this
/// type carries provenance only, never a verdict; `HookPermissionVerdict`/
/// `HookOnFailure` (both above) are what a hook or its outage may narrow,
/// and this type is orthogonal to both.
///
/// `#[non_exhaustive]`: a third provenance tier is plausible (e.g. a
/// remote/marketplace source distinct from a locally installed plugin),
/// and this is a pure labeling type -- no permission implication rides on
/// a variant here, so the one cross-crate exhaustive `match` this attribute
/// forces a wildcard onto (`conway`'s own `conway.rs::hook_origin_label`)
/// renders an honest "unrecognized origin" label rather than guessing --
/// never a security decision.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookOrigin {
    /// This rule reached dispatch from the operator's own merged
    /// `[hooks].rules[]` config -- unchanged from every hook rule that
    /// existed before this type did.
    Operator,
    /// This rule reached dispatch because an installed plugin declared it
    /// via `Plugin::hooks()`. Carries that plugin's own
    /// `PluginManifest::id`, so an operator inspecting the review list sees
    /// WHICH plugin contributed it, not merely that some hook did --
    /// mirroring the "an author never picks their own namespace, but the
    /// host still names them" attribution `declared_plugin_events`/
    /// `CommandRegistry::build` already perform for event/command names.
    Plugin(String),
}

impl Default for HookOrigin {
    /// `Operator` -- every hook rule that reached dispatch before this type
    /// existed came from `[hooks].rules[]`, so a caller that never sets
    /// this explicitly (every construction site that predates board item
    /// `01M129QW0GV90QTQS6B3BY3DAR`) keeps reporting exactly that,
    /// byte-for-byte.
    fn default() -> Self {
        HookOrigin::Operator
    }
}

/// An append-only edit to computed context: items to append, and
/// identifiers to exclude. **There is no "replace" variant anywhere in this
/// type** -- see [`HookAnswer`]'s own doc for why that omission is the
/// point, not an oversight.
///
/// `#[non_exhaustive]`, with [`Self::new`] as the construction path. This
/// is the type most likely to grow a THIRD axis alongside
/// `appends`/`excludes` (an ordering hint, a per-append target segment,
/// etc., as the design matures past "prove the shape is representable")
/// and it already has nine external construction sites across the
/// workspace's own test suites -- exactly the case a two-argument
/// constructor exists to make cheap. Unaffected: `Deserialize`, which
/// keeps accepting a partial/absent JSON object via each field's own
/// `#[serde(default)]`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ContextDelta {
    /// Opaque content this hook is appending. Left untyped here
    /// (`serde_json::Value`): the concrete per-item shape (`{role,
    /// blocks}`, mirroring the extension design's
    /// `context.hook/1`) is a later item's concern, once a concrete context
    /// event is actually wired onto this runner -- this item proves the
    /// SHAPE (append, never replace) is representable, nothing more.
    #[serde(default)]
    pub appends: Vec<serde_json::Value>,
    /// Identifiers (e.g. a stringified segment/log-seq identity) this hook
    /// wants excluded. Set semantics: composing two hooks' exclusion sets
    /// into one union (§16.3) is a later consumer's job, not this type's --
    /// recorded here only as the shape that composition needs.
    #[serde(default)]
    pub excludes: Vec<String>,
}

impl ContextDelta {
    /// Construct one directly -- the ONLY way from outside this crate, now
    /// that `#[non_exhaustive]` forbids the struct-literal form. Two
    /// fields, positional, no builder.
    pub fn new(appends: Vec<serde_json::Value>, excludes: Vec<String>) -> Self {
        Self { appends, excludes }
    }
}

/// Whether `tool` satisfies a `pre_tool_use`/`post_tool_use` rule's `match`
/// pattern (; `"match"` on the wire --
/// see `crates/conway/src/config/schema.rs`'s `HookEntry::match_tool`,
/// which is the only producer of `pattern` in practice).
///
/// **Two forms, deliberately no more** (that item's own ACCEPTANCE: "exact
/// plus glob covers the page's two [`PHILOSOPHY.md` §5] examples... do not
/// build a regex dialect without a stated need"):
/// - `pattern` contains no `*`: exact string equality against `tool`. This
///   is the only form either of `PHILOSOPHY.md`'s own examples (`"bash"`,
///   `"fs.write"`) needs.
/// - `pattern` contains `*`: a shell-style glob where `*` matches any run of
///   zero or more characters (any number of `*`s, no other wildcard
///   syntax -- no `?`, no character classes) against the WHOLE of `tool`,
///   not a substring search. `tool` itself can never legitimately contain a
///   literal `*` (`conway_core::ids::ToolName` is a plugin-chosen
///   identifier, never operator input), so there is no ambiguity to guard
///   against the way [`crate::permission_pattern`]'s shell-metacharacter
///   gate has to for a rendered command string.
///
/// An empty `pattern` matches only an empty `tool` (exact-equality
/// fallthrough) -- `merge::validate` rejects an empty `HookEntry::id`, but
/// nothing here assumes `pattern` itself is non-empty, since this function
/// has no access to the rule it came from to report a config error; a
/// pattern that can never usefully match is the caller's problem to have
/// prevented, not this function's to special-case.
pub fn tool_matcher_matches(pattern: &str, tool: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == tool;
    }
    glob_match(pattern.as_bytes(), tool.as_bytes())
}

/// The classic two-pointer wildcard matcher (`*` only, no `?`): tracks the
/// most recent `*` seen (`star`) and the text position it last committed to
/// consuming from (`match_from`), backtracking there -- one character
/// further each time -- whenever a later literal fails to match, rather than
/// exploring every possible split with recursion. `O(pattern.len() +
/// tool.len())` amortized, no allocation.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut match_from = 0usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'*' || pattern[p] == text[t]) {
            if pattern[p] == b'*' {
                star = Some(p);
                match_from = t;
                p += 1;
            } else {
                p += 1;
                t += 1;
            }
        } else if let Some(star_p) = star {
            // Backtrack: the last `*` swallows one more character than it
            // did last time, and matching resumes right after it.
            p = star_p + 1;
            match_from += 1;
            t = match_from;
        } else {
            return false;
        }
    }
    // Any trailing `*`s match the empty remainder; anything else pending in
    // `pattern` does not.
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_event_round_trips() {
        let event = HookEvent {
            name: "pre_tool_use".into(),
            payload: serde_json::json!({"tool": "bash"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: HookEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn hook_invocation_round_trips() {
        let invocation = HookInvocation {
            command: vec!["/usr/bin/env".into(), "true".into()],
            timeout_ms: 5_000,
            event: HookEvent {
                name: "pre_tool_use".into(),
                payload: serde_json::json!(null),
            },
        };
        let json = serde_json::to_string(&invocation).unwrap();
        let back: HookInvocation = serde_json::from_str(&json).unwrap();
        assert_eq!(invocation, back);
    }

    /// `#[non_exhaustive]` forbids struct-literal construction from
    /// outside this crate -- `HookEvent::new`/`HookInvocation::new` are the
    /// replacement, and this pins that each produces the byte-identical
    /// value the old literal did (same JSON, same equality), not merely
    /// that it compiles.
    #[test]
    fn hook_event_and_invocation_constructors_match_the_equivalent_literal() {
        let via_new = HookInvocation::new(
            vec!["/usr/bin/env".into(), "true".into()],
            5_000,
            HookEvent::new("pre_tool_use", serde_json::json!(null)),
        );
        let via_literal = HookInvocation {
            command: vec!["/usr/bin/env".into(), "true".into()],
            timeout_ms: 5_000,
            event: HookEvent {
                name: "pre_tool_use".into(),
                payload: serde_json::json!(null),
            },
        };
        assert_eq!(via_new, via_literal);
    }

    #[test]
    fn denies_is_false_only_for_no_opinion() {
        assert!(!HookPermissionVerdict::NoOpinion.denies());
        assert!(HookPermissionVerdict::Deny {
            reason: "no".into()
        }
        .denies());
    }

    #[test]
    fn default_hook_answer_has_an_empty_context_delta() {
        let answer = HookAnswer::default();
        assert!(answer.context.appends.is_empty());
        assert!(answer.context.excludes.is_empty());
    }

    #[test]
    fn hook_answer_round_trips_with_appends_and_excludes() {
        let answer = HookAnswer {
            context: ContextDelta {
                appends: vec![serde_json::json!({"role": "system", "text": "note"})],
                excludes: vec!["seg-1".to_string()],
            },
            permission: HookPermissionVerdict::default(),
        };
        let json = serde_json::to_string(&answer).unwrap();
        let back: HookAnswer = serde_json::from_str(&json).unwrap();
        assert_eq!(answer, back);
    }

    /// The same equivalence pin as
    /// `hook_event_and_invocation_constructors_match_the_equivalent_literal`,
    /// for `HookAnswer::new`/`ContextDelta::new`.
    #[test]
    fn hook_answer_and_context_delta_constructors_match_the_equivalent_literal() {
        let via_new = HookAnswer::new(
            ContextDelta::new(
                vec![serde_json::json!({"role": "system", "text": "note"})],
                vec!["seg-1".to_string()],
            ),
            HookPermissionVerdict::default(),
        );
        let via_literal = HookAnswer {
            context: ContextDelta {
                appends: vec![serde_json::json!({"role": "system", "text": "note"})],
                excludes: vec!["seg-1".to_string()],
            },
            permission: HookPermissionVerdict::default(),
        };
        assert_eq!(via_new, via_literal);
    }

    /// The structural proof behind "cannot express wholesale replacement":
    /// `HookAnswer`'s `context` field is a `ContextDelta`, and
    /// `ContextDelta`'s only fields are `appends`/`excludes` -- there is no
    /// field anywhere on `HookAnswer` (its sibling `permission` field is a
    /// wholly separate axis -- see that field's own doc) this JSON shape
    /// could parse a `"replace"`/`"new_payload"`-shaped key into even if a
    /// hook script tried to send one; an unknown key is simply ignored by
    /// serde's default (non-`deny_unknown_fields`) leniency, never
    /// interpreted as a replacement instruction.
    #[test]
    fn an_unknown_replace_shaped_key_is_ignored_not_interpreted_as_a_replacement() {
        let json = serde_json::json!({
            "context": {"appends": [], "excludes": []},
            "replace": {"segments": ["anything"]},
        });
        let answer: HookAnswer = serde_json::from_value(json).unwrap();
        assert_eq!(answer, HookAnswer::default());
    }

    #[test]
    fn default_hook_permission_verdict_is_no_opinion() {
        assert_eq!(
            HookPermissionVerdict::default(),
            HookPermissionVerdict::NoOpinion
        );
    }

    #[test]
    fn hook_permission_verdict_deny_round_trips() {
        let verdict = HookPermissionVerdict::Deny {
            reason: "touches a path this hook refuses".to_string(),
        };
        let json = serde_json::to_string(&verdict).unwrap();
        let back: HookPermissionVerdict = serde_json::from_str(&json).unwrap();
        assert_eq!(verdict, back);
    }

    /// The structural proof behind "no `Allow` variant, full stop": every
    /// JSON shape this enum's own `Deserialize` impl accepts is enumerated
    /// here (`"no_opinion"` and `{"deny":{"reason":...}}`) -- neither can
    /// decode to anything but `NoOpinion`/`Deny`, and no third shape exists
    /// for a hook script to spell an allow into. This is the type-level
    /// proof `HookPermissionVerdict`'s own doc claims; a broken build here
    /// (a new variant added without updating this test) is the guard.
    #[test]
    fn no_json_shape_decodes_to_an_allow_because_no_allow_variant_exists() {
        let no_opinion: HookPermissionVerdict = serde_json::from_str("\"no_opinion\"").unwrap();
        assert_eq!(no_opinion, HookPermissionVerdict::NoOpinion);

        let deny: HookPermissionVerdict =
            serde_json::from_str(r#"{"deny":{"reason":"no"}}"#).unwrap();
        assert_eq!(
            deny,
            HookPermissionVerdict::Deny {
                reason: "no".to_string()
            }
        );

        // Anything else -- including a hypothetical `"allow"` -- is a
        // deserialize error, not a silently-accepted third variant.
        assert!(serde_json::from_str::<HookPermissionVerdict>("\"allow\"").is_err());
    }

    #[test]
    fn default_hook_on_failure_is_deny() {
        assert_eq!(HookOnFailure::default(), HookOnFailure::Deny);
    }

    #[test]
    fn hook_on_failure_round_trips_both_variants() {
        for variant in [HookOnFailure::Deny, HookOnFailure::Prompt] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: HookOnFailure = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, back);
        }
    }

    /// The structural proof behind "no `Allow` variant, full stop" for THIS
    /// type too -- the identical proof shape
    /// `no_json_shape_decodes_to_an_allow_because_no_allow_variant_exists`
    /// already gives `HookPermissionVerdict`, extended to the outage-policy
    /// type: only `"deny"` and `"prompt"` decode to anything, and a
    /// hypothetical `"allow"` is a deserialize error, never a silently
    /// accepted third variant.
    #[test]
    fn no_json_shape_decodes_to_an_allow_hook_on_failure_because_no_allow_variant_exists() {
        let deny: HookOnFailure = serde_json::from_str("\"deny\"").unwrap();
        assert_eq!(deny, HookOnFailure::Deny);

        let prompt: HookOnFailure = serde_json::from_str("\"prompt\"").unwrap();
        assert_eq!(prompt, HookOnFailure::Prompt);

        assert!(serde_json::from_str::<HookOnFailure>("\"allow\"").is_err());
    }

    /// ACCEPTANCE: both of
    /// `PHILOSOPHY.md` §5's own example patterns are exact matches, and each
    /// matches only its own tool.
    #[test]
    fn tool_matcher_matches_exact_names_from_the_philosophy_examples() {
        assert!(tool_matcher_matches("bash", "bash"));
        assert!(!tool_matcher_matches("bash", "fs.write"));
        assert!(tool_matcher_matches("fs.write", "fs.write"));
        assert!(!tool_matcher_matches("fs.write", "fs.read"));
    }

    #[test]
    fn tool_matcher_matches_a_prefix_glob() {
        assert!(tool_matcher_matches("fs.*", "fs.write"));
        assert!(tool_matcher_matches("fs.*", "fs.read"));
        assert!(!tool_matcher_matches("fs.*", "bash"));
        // `fs.` alone, no trailing anything, still satisfies `fs.*` -- `*`
        // matches zero characters too.
        assert!(tool_matcher_matches("fs.*", "fs."));
    }

    #[test]
    fn tool_matcher_matches_a_suffix_and_infix_glob() {
        assert!(tool_matcher_matches("*.write", "fs.write"));
        assert!(!tool_matcher_matches("*.write", "fs.read"));
        assert!(tool_matcher_matches("*write*", "fs.write.tmp"));
    }

    #[test]
    fn tool_matcher_bare_star_matches_every_tool_including_empty() {
        assert!(tool_matcher_matches("*", "bash"));
        assert!(tool_matcher_matches("*", ""));
    }

    #[test]
    fn tool_matcher_multiple_stars_backtrack_correctly() {
        assert!(tool_matcher_matches("*a*b*", "xaxbx"));
        assert!(tool_matcher_matches("*a*b*", "ab"));
        assert!(!tool_matcher_matches("*a*b*", "ba"));
    }
}
