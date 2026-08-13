//! Validates the core-vs-plugin event name namespace convention decided at
//! `.design/extension-architecture.md` §16.6 (board item
//! 01KZRZWXQP9QXP6BYN636Z3DCZ): a core, built-in event name is a bare
//! identifier (`pre_tool_use`, `post_tool_use`, ...); a plugin-declared
//! event name always begins with the declaring plugin's
//! [`crate::ports::PluginManifest::id`], then a single `.`, then the
//! event's own name (`myplugin.compaction_decided`) — never bare, and never
//! split out of the string by guessing where the id ends, since a plugin id
//! may itself be chosen freely by whoever wrote the plugin.
//!
//! **Forward declaration — this function has no caller yet.** Nothing may
//! claim to be reached that isn't, so this says so plainly. Two
//! later items each own one call site, and neither has wired it up:
//! - the `[hooks]` schema item's already-landed `HookEntry::event`
//!   (`crates/conway/src/config/schema.rs`, `merge.rs` check 9) is the
//!   *subscriber* side — `validate_event_name(event, None)` checks an
//!   operator-written `event` string is well-formed;
//! - the plugin-declared-events item (not yet a board item; tracked in
//!   `.design/philosophy-debt.md` §1's "registration for plugin-declared
//!   events" bullet) is the *declaration* side —
//!   `validate_event_name(name, Some(&manifest.id))` checks a plugin
//!   actually prefixed its own emitted event with its own id.
//!
//! Nothing in this crate, or anywhere else in the tree, calls this function.
//! Writing it now gives those two items an unambiguous rule to wire up
//! rather than a paragraph of prose to re-derive into code by hand — see
//! §16.6 for the full reasoning, including why the reserved core set is an
//! open structural rule rather than a hardcoded list, and why a plugin id
//! containing the separator is excluded outright rather than resolved by
//! splitting on the first occurrence (unlike
//! [`crate::permission_pattern::PatternRule::parse`]'s `tool:prefix`, where
//! that split is safe only because `tool` is drawn from a small, closed,
//! engine-known vocabulary that never needs the separator in the first
//! place).

/// The separator between a declaring plugin's id and its event's own name.
/// Decided in §16.6 point 1: dot, not colon (already a different wire form,
/// `PatternRule::parse`'s `tool:prefix`) or slash (reads as a path/URI
/// hierarchy this design does not intend).
pub const EVENT_NAMESPACE_SEPARATOR: char = '.';

/// Checks `name` against the core-vs-plugin event namespace convention.
///
/// - `declaring_plugin: None` — validates `name` as a name that may appear
///   in an operator's hook subscription: either a bare core-shaped
///   identifier (no [`EVENT_NAMESPACE_SEPARATOR`]) or a correctly-formed
///   `plugin_id.event_name`. This does **not** check that `name` matches
///   any event conway actually dispatches or any plugin actually loaded —
///   only that its shape is well-formed. That closed-vocabulary check
///   needs the runner and does not exist yet (§16.6 point 2).
/// - `declaring_plugin: Some(id)` — validates `name` as the event a plugin
///   with manifest id `id` is declaring for itself: `name` must equal `id`,
///   then [`EVENT_NAMESPACE_SEPARATOR`], then a non-empty remainder. `id`
///   itself must not contain the separator (§16.6 point 3); if it does,
///   this returns an error naming the id's own defect, not a shape defect
///   in `name`.
///
/// Both branches also reject an empty `name`.
pub fn validate_event_name(name: &str, declaring_plugin: Option<&str>) -> Result<(), String> {
    if name.is_empty() {
        return Err("event name must not be empty".to_string());
    }

    match declaring_plugin {
        None => {
            // A subscriber-side name: bare (core-shaped) or a well-formed
            // `plugin_id.event_name` are both acceptable; only the
            // structural shape is checked here (§16.6 point 2).
            if !name.contains(EVENT_NAMESPACE_SEPARATOR) {
                return Ok(());
            }
            let (prefix, rest) = name
                .split_once(EVENT_NAMESPACE_SEPARATOR)
                .expect("contains() just confirmed the separator is present");
            if prefix.is_empty() || rest.is_empty() {
                return Err(format!(
                    "event name '{name}' contains '{EVENT_NAMESPACE_SEPARATOR}' but is not a \
                     well-formed 'plugin_id{EVENT_NAMESPACE_SEPARATOR}event_name'"
                ));
            }
            Ok(())
        }
        Some(id) => {
            // A declaration-side name: must be exactly `id` + separator +
            // a non-empty remainder. Checked by direct prefix comparison
            // against the known `id`, never by splitting `name` apart --
            // see this module's own doc for why splitting is unsafe here.
            if id.is_empty() {
                return Err("declaring plugin id must not be empty".to_string());
            }
            if id.contains(EVENT_NAMESPACE_SEPARATOR) {
                return Err(format!(
                    "plugin id '{id}' contains '{EVENT_NAMESPACE_SEPARATOR}', the event \
                     namespace separator -- plugin ids must not contain it (§16.6 point 3)"
                ));
            }
            let after_id = name
                .strip_prefix(id)
                .and_then(|rest| rest.strip_prefix(EVENT_NAMESPACE_SEPARATOR));
            let starts_with_id_dot = matches!(after_id, Some(remainder) if !remainder.is_empty());
            if !starts_with_id_dot {
                return Err(format!(
                    "event name '{name}' declared by plugin '{id}' must be prefixed with \
                     '{id}{EVENT_NAMESPACE_SEPARATOR}' -- a plugin may never declare a bare \
                     event name, which is reserved for conway's own core events"
                ));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §16.6 point 1 (separator) + the "bare `pre_tool_use` from core is
    /// valid" reader question the VERIFICATION ANCHOR names directly.
    #[test]
    fn validate_event_name_accepts_bare_core_shaped_name_with_no_declaring_plugin() {
        assert_eq!(validate_event_name("pre_tool_use", None), Ok(()));
    }

    /// The "is `myplugin.foo` valid?" reader question, subscriber side.
    #[test]
    fn validate_event_name_accepts_well_formed_plugin_prefixed_name_with_no_declaring_plugin() {
        assert_eq!(
            validate_event_name("myplugin.compaction_decided", None),
            Ok(())
        );
    }

    /// Acceptance criterion: "a bare core-looking name from a plugin is
    /// rejected." The "is bare `foo` valid coming from a plugin?" reader
    /// question, declaration side.
    #[test]
    fn validate_event_name_rejects_bare_name_declared_by_a_plugin() {
        let err = validate_event_name("compaction_decided", Some("myplugin"))
            .expect_err("a plugin may never declare a bare event name");
        assert!(
            err.contains("myplugin"),
            "error should name the plugin: {err}"
        );
        assert!(
            err.contains("compaction_decided"),
            "error should name the rejected event: {err}"
        );
    }

    /// Acceptance criterion: "a correctly prefixed plugin event is
    /// accepted." The "is `myplugin.foo` valid?" reader question,
    /// declaration side.
    #[test]
    fn validate_event_name_accepts_correctly_prefixed_plugin_event() {
        assert_eq!(
            validate_event_name("myplugin.compaction_decided", Some("myplugin")),
            Ok(())
        );
    }

    /// Acceptance criterion: "a plugin id containing the separator behaves
    /// per whichever splitting rule was chosen" -- §16.6 point 3 chose
    /// exclusion, not splitting, so this must be rejected regardless of
    /// what `name` says.
    #[test]
    fn validate_event_name_rejects_plugin_id_containing_the_separator() {
        let err = validate_event_name("my.plugin.compaction_decided", Some("my.plugin"))
            .expect_err("a plugin id containing the separator must be rejected, not split");
        assert!(
            err.contains("my.plugin"),
            "error should name the offending id: {err}"
        );
    }

    /// A plugin declaring a name that IS its own id plus separator, but
    /// with nothing after it, is not a valid event name -- the remainder
    /// must be non-empty.
    #[test]
    fn validate_event_name_rejects_plugin_declared_name_with_empty_remainder() {
        assert!(validate_event_name("myplugin.", Some("myplugin")).is_err());
    }

    /// A plugin declaring an event name that merely starts with its id as a
    /// literal string, without the separator immediately following, is not
    /// prefixed -- `myplugincompaction_decided` is not
    /// `myplugin.compaction_decided`.
    #[test]
    fn validate_event_name_rejects_plugin_declared_name_missing_the_separator() {
        assert!(validate_event_name("myplugincompaction_decided", Some("myplugin")).is_err());
    }

    /// A subscriber-side name with an empty prefix or empty remainder
    /// around the separator is malformed, not a valid bare-or-prefixed
    /// shape.
    #[test]
    fn validate_event_name_rejects_subscriber_side_name_with_empty_segment_around_separator() {
        assert!(validate_event_name(".compaction_decided", None).is_err());
        assert!(validate_event_name("myplugin.", None).is_err());
    }

    /// Empty names are rejected on both branches.
    #[test]
    fn validate_event_name_rejects_empty_name_on_both_branches() {
        assert!(validate_event_name("", None).is_err());
        assert!(validate_event_name("", Some("myplugin")).is_err());
    }
}
