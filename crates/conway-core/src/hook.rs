//! Domain types for one hook invocation (board item
//! 01KZRZY1MNM872BZ6AKEBG3SKE): the EVENT NAME + PAYLOAD a hook is invoked
//! with, and the ANSWER it may return -- kept deliberately separable from
//! the INVOCATION MODALITY that actually delivers them. See
//! [`crate::ports::HookRunner`] for the port that performs an invocation
//! (a PORT, not a type here, because performing one is I/O -- this crate
//! does none).
//!
//! **Today's modality (decision 01KZRZBQ2ACF40QGK8E9AVGMT3): one-shot.** A
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookEvent {
    /// e.g. `"pre_tool_use"`, or a plugin-namespaced `"myplugin.foo"` --
    /// `crate::event_name::validate_event_name`'s vocabulary. **Not
    /// validated here**: that is the config-load-time (subscriber side) and
    /// declaration-time (plugin side) concern two OTHER board items own --
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

/// What one hook invocation spawns and how long it is allowed to run.
///
/// `command` is an argv vector (program, then its arguments) -- never a
/// single shell string, matching `crates/conway/src/config/schema.rs`'s
/// `HookEntry::command` shape exactly, so no shell-quoting ambiguity exists
/// between config and what actually gets spawned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookInvocation {
    pub command: Vec<String>,
    pub timeout_ms: u64,
    pub event: HookEvent,
}

/// One hook's answer to an invocation that succeeded (fail-closed:
/// everything else is a [`crate::error::HookFailure`] instead -- see
/// [`crate::ports::HookRunner`]'s own doc).
///
/// **Structurally cannot express "replace computed context wholesale"**
/// (decision 01KZSDRZ2HYPGGJVDPZHGPMD5G, which supersedes an earlier one
/// whose reasoning was cacheability -- NOT the basis here). The only way to
/// change context is [`ContextDelta`]: append, or exclude by identifier,
/// never substitute. The load-bearing reason is **reconstructability**: the
/// prior state must remain recoverable from what was persisted, which a
/// free-form replacement value would destroy (there would be no way to
/// recover what a hook overwrote). Cacheability is a secondary consequence
/// of the same shape, not the justification -- caching is the inference
/// plugin's own responsibility, not this runner's.
///
/// **`permission` (board item 01KZS00JP5QNBJSSHNFP9C47GM): a second,
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HookAnswer {
    #[serde(default)]
    pub context: ContextDelta,
    #[serde(default)]
    pub permission: HookPermissionVerdict,
}

/// A `pre_tool_use` hook's opinion on whether the call it was invoked for
/// should be allowed to proceed (board item 01KZS00JP5QNBJSSHNFP9C47GM).
///
/// **No `Allow` variant exists, anywhere in this type -- not "we only check
/// whether it denied."** Decision 01KZRZAFD8T3GX407MZC8P1W1E: a hook may
/// only NARROW a permission verdict, never widen one; it cannot grant an
/// allow that was not already going to happen, only add another way to say
/// no. `crate::permission_pattern::Then` enforces the same rule one layer
/// down for plugin-contributed pattern rules, but does so by *rejecting*
/// `Then::Allow` at admission time (`PermissionBroker::remember_pattern_rule`,
/// a runtime check a future edit could get wrong) -- this type takes the
/// strictly stronger option the same board item's spec asked for: there is
/// no `Allow` variant for a runtime check to fail to reject, so
/// `PermissionBroker::decide` structurally cannot treat a hook's answer as
/// a grant, independent of whether every call site keeps checking that
/// correctly.
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

/// An append-only edit to computed context: items to append, and
/// identifiers to exclude. **There is no "replace" variant anywhere in this
/// type** -- see [`HookAnswer`]'s own doc for why that omission is the
/// point, not an oversight.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextDelta {
    /// Opaque content this hook is appending. Left untyped here
    /// (`serde_json::Value`): the concrete per-item shape (`{role,
    /// blocks}`, mirroring `.design/extension-architecture.md` §16.5's
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
        assert_eq!(HookPermissionVerdict::default(), HookPermissionVerdict::NoOpinion);
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
}
