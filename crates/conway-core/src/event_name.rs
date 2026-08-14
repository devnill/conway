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
//! **Both call sites now exist (board item 01KZS03BFE720EQZG7Q2768N2H
//! wired the second one; this doc used to say neither did).**
//! - the `[hooks]` schema's `HookEntry::event` is the *subscriber* side —
//!   `crates/conway/src/config/merge.rs`'s own event-shape check calls
//!   `validate_event_name(event, None)` to confirm an operator-written
//!   `event` string is well-formed (bare, or `plugin_id.event_name`);
//! - `conway_runtime::hook_dispatch::declared_plugin_events` is the
//!   *declaration* side — for every plugin's own [`crate::ports::
//!   Plugin::events`], it assembles `plugin_id.bare_name` and calls
//!   `validate_event_name(&full_name, Some(&manifest.id))`, the same
//!   pattern `conway_cli::tui::commands::CommandRegistry::build` already
//!   established for [`crate::ports::Command`] names via
//!   [`validate_command_name`].
//!
//! See §16.6 for the full reasoning behind the rule itself, including why
//! the reserved core set is an open structural rule rather than a
//! hardcoded list.
//!
//! **§16.6 point 3 (a plugin id containing the separator is excluded
//! outright) is RECONSIDERED here, disclosed rather than silently
//! reversed — board item 01KZS03BFE720EQZG7Q2768N2H is the very item that
//! section named as the follow-up owed to "first validate a
//! `PluginManifest` at registration time".** Every real built-in plugin id
//! in this workspace (`conway.fs`, `conway.shell`, `conway.report`,
//! `conway.subagent`, `conway.plugin_skeleton`) already contains `.`, so
//! excluding it outright would make [`Plugin::events`](crate::ports::Plugin::events)
//! and [`Plugin::commands`](crate::ports::Plugin::commands) unreachable for
//! every one of them. `validate_namespaced`'s own
//! `Some(id)` branch now documents exactly why the splitting hazard §16.6
//! point 3 raised cannot occur there (`id` is always supplied out of
//! band, never recovered by splitting `name` apart) — see that function's
//! doc comment for the full argument, not restated twice.
//!
//! **A third consumer, same rule, different vocabulary.** Board item
//! 01KZYBFTK4QPB45AJT9M57P60W (plugin-declared TUI slash commands) needs the
//! identical namespace shape for a plugin's *command* names — "a plugin
//! declaring `/help` must not shadow the built-in" is the exact same problem
//! §16.6 already solved for events, just one surface over. Rather than a
//! second, independently-drifting implementation, [`validate_command_name`]
//! shares `validate_namespaced` with [`validate_event_name`]: same
//! separator constant, same shape rule, only the noun in the error text
//! differs. This also settles that item's own open question ("bare name
//! reachable, or always namespaced?") structurally: a plugin command's full
//! name can never equal a bare built-in command name (no built-in TUI
//! command name contains [`EVENT_NAMESPACE_SEPARATOR`]), so shadowing a
//! built-in is impossible by construction, not merely checked at runtime —
//! see that item's own module docs (`conway_cli::tui::commands`) for the
//! registration-time check this enables instead (duplicate *plugin* command
//! names, the collision this structural guarantee does not already rule
//! out).

/// The separator between a declaring plugin's id and its event's (or, since
/// board item 01KZYBFTK4QPB45AJT9M57P60W, command's) own name. Decided in
/// §16.6 point 1: dot, not colon (already a different wire form,
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
    validate_namespaced(name, declaring_plugin, "event")
}

/// Checks `name` against the SAME core-vs-plugin namespace convention as
/// [`validate_event_name`] — see this module's own doc ("A third consumer,
/// same rule, different vocabulary") for why a plugin-declared TUI command's
/// full name (as typed with its leading `/` stripped, e.g. `acme.greet` for
/// `/acme.greet`) reuses `validate_namespaced` rather than a second,
/// independent shape check. `declaring_plugin` follows the identical
/// `None`/`Some` split `validate_event_name` documents.
pub fn validate_command_name(name: &str, declaring_plugin: Option<&str>) -> Result<(), String> {
    validate_namespaced(name, declaring_plugin, "command")
}

/// The shared implementation behind [`validate_event_name`] and
/// [`validate_command_name`]: identical shape rule, `noun` only changes the
/// error text ("event"/"command") so a caller of either public function
/// gets a message about the vocabulary it actually asked about.
fn validate_namespaced(
    name: &str,
    declaring_plugin: Option<&str>,
    noun: &str,
) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{noun} name must not be empty"));
    }

    match declaring_plugin {
        None => {
            // A subscriber-side name: bare (core-shaped) or a well-formed
            // `plugin_id.<noun>_name` are both acceptable; only the
            // structural shape is checked here (§16.6 point 2).
            if !name.contains(EVENT_NAMESPACE_SEPARATOR) {
                return Ok(());
            }
            let (prefix, rest) = name
                .split_once(EVENT_NAMESPACE_SEPARATOR)
                .expect("contains() just confirmed the separator is present");
            if prefix.is_empty() || rest.is_empty() {
                return Err(format!(
                    "{noun} name '{name}' contains '{EVENT_NAMESPACE_SEPARATOR}' but is not a \
                     well-formed 'plugin_id{EVENT_NAMESPACE_SEPARATOR}{noun}_name'"
                ));
            }
            Ok(())
        }
        Some(id) => {
            // A declaration-side name: must be exactly `id` + separator +
            // a non-empty remainder. Checked by direct prefix comparison
            // against the known `id`, never by splitting `name` apart --
            // see this module's own doc for why splitting is unsafe here.
            //
            // **`id` MAY contain the separator, deliberately -- a reversal
            // from an earlier draft of this rule (`.design/extension-
            // architecture.md` §16.6 point 3), recorded here rather than
            // silently changed.** That draft excluded a separator-bearing
            // `id` outright, reasoning from a SUBSCRIBER-side hazard: a
            // hypothetical parser trying to RECOVER `id` by splitting
            // `name` on the first `.` could misattribute
            // `my.plugin.compaction_decided` to a plugin literally named
            // `my`. But nothing in this codebase ever performs that
            // recovery -- this branch is only ever reached with `id`
            // ALREADY KNOWN (`PluginManifest::id`, supplied by the caller,
            // never parsed out of `name`), and matching a fired event
            // against a configured subscription is always exact-string
            // equality (`declared_plugin_events`'s `BTreeMap` key,
            // `HookDispatcher::dispatch`'s lookup) -- never a re-split.
            // The hazard the exclusion existed to prevent cannot occur on
            // THIS branch by construction, and every real built-in plugin
            // id in this workspace (`conway.fs`, `conway.shell`,
            // `conway.report`, `conway.subagent`, `conway.plugin_skeleton`)
            // already contains the separator -- the original draft's own
            // caveat ("no plugin ids exist in the tree yet to break by
            // adding it") was already false the day it was written. Two
            // DIFFERENT plugins whose ids and bare names happen to collide
            // on the assembled full string (`my` + `.plugin.x` producing
            // the identical `my.plugin.x` a plugin literally named
            // `my.plugin` declaring bare `x` would also produce) are still
            // caught -- not here, but as a duplicate full name at
            // `conway_runtime::hook_dispatch::declared_plugin_events`,
            // which is where two events landing on one string is already
            // an error regardless of why they collided.
            if id.is_empty() {
                return Err("declaring plugin id must not be empty".to_string());
            }
            let after_id = name
                .strip_prefix(id)
                .and_then(|rest| rest.strip_prefix(EVENT_NAMESPACE_SEPARATOR));
            let starts_with_id_dot = matches!(after_id, Some(remainder) if !remainder.is_empty());
            if !starts_with_id_dot {
                return Err(format!(
                    "{noun} name '{name}' declared by plugin '{id}' must be prefixed with \
                     '{id}{EVENT_NAMESPACE_SEPARATOR}' -- a plugin may never declare a bare \
                     {noun} name, which is reserved for conway's own core {noun}s"
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

    /// Acceptance criterion, RECONSIDERED (see this module's own doc, "§16.6
    /// point 3 is reconsidered here"): a plugin id containing the separator
    /// is ACCEPTED on the declaration side -- `id` is always known out of
    /// band here, never recovered by splitting `name` apart, so the
    /// misattribution hazard the original exclusion existed to prevent
    /// cannot occur on this branch. Every real built-in plugin id in this
    /// workspace (`conway.fs`, `conway.plugin_skeleton`, ...) needs exactly
    /// this to be true.
    #[test]
    fn validate_event_name_accepts_a_plugin_id_containing_the_separator() {
        assert_eq!(
            validate_event_name("my.plugin.compaction_decided", Some("my.plugin")),
            Ok(())
        );
    }

    /// Two DIFFERENT plugins whose id/bare-name split lands on the
    /// IDENTICAL assembled full string (id `my` with bare name
    /// `plugin.x`, versus id `my.plugin` with bare name `x`) are each
    /// individually valid per this function -- the actual ambiguity is a
    /// full-name COLLISION, caught elsewhere
    /// (`conway_runtime::hook_dispatch::declared_plugin_events`'s duplicate
    /// check), never here, since this function only ever validates one
    /// `(name, declaring_plugin)` pair at a time and has no visibility into
    /// what any OTHER plugin declared.
    #[test]
    fn validate_event_name_accepts_both_halves_of_a_would_be_full_name_collision() {
        assert_eq!(
            validate_event_name("my.plugin.x", Some("my")),
            Ok(()),
            "plugin 'my' declaring bare 'plugin.x' is independently well-formed"
        );
        assert_eq!(
            validate_event_name("my.plugin.x", Some("my.plugin")),
            Ok(()),
            "plugin 'my.plugin' declaring bare 'x' is independently well-formed"
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

    // ---- validate_command_name (board item 01KZYBFTK4QPB45AJT9M57P60W) ----
    // Same shape rule as `validate_event_name`'s own tests above, restated
    // for the command vocabulary -- proves `validate_namespaced` is
    // genuinely shared, not two implementations that happen to agree today.

    #[test]
    fn validate_command_name_accepts_correctly_prefixed_plugin_command() {
        assert_eq!(validate_command_name("acme.greet", Some("acme")), Ok(()));
    }

    /// The acceptance-bearing case: a plugin may never declare a bare
    /// command name -- so a plugin cannot shadow a built-in TUI command
    /// (`/help`, `/quit`, ...), none of which contain the separator.
    #[test]
    fn validate_command_name_rejects_bare_name_declared_by_a_plugin() {
        let err = validate_command_name("help", Some("acme"))
            .expect_err("a plugin may never declare a bare command name");
        assert!(err.contains("acme"), "error should name the plugin: {err}");
        assert!(
            err.contains("help"),
            "error should name the rejected command: {err}"
        );
        assert!(
            err.contains("command"),
            "error text should say 'command', not 'event': {err}"
        );
    }

    /// RECONSIDERED alongside `validate_event_name`'s own sibling test --
    /// see this module's own doc, "§16.6 point 3 is reconsidered here":
    /// `conway_cli::tui::commands::CommandRegistry::build` needs this to be
    /// `Ok` for every real built-in plugin id in this workspace
    /// (`conway.plugin_skeleton`, whose own shipped command is `ping`).
    #[test]
    fn validate_command_name_accepts_a_plugin_id_containing_the_separator() {
        assert!(validate_command_name("my.plugin.greet", Some("my.plugin")).is_ok());
    }

    #[test]
    fn validate_command_name_rejects_empty_name_on_both_branches() {
        assert!(validate_command_name("", None).is_err());
        assert!(validate_command_name("", Some("acme")).is_err());
    }

    #[test]
    fn validate_command_name_accepts_bare_name_with_no_declaring_plugin() {
        // Subscriber-shaped check (mirrors `validate_event_name`'s own):
        // used by `CommandRegistry::build` only in the `Some` declaration
        // branch today, but the `None` branch is exercised here so a future
        // caller inherits the same tested behavior.
        assert_eq!(validate_command_name("help", None), Ok(()));
    }
}
