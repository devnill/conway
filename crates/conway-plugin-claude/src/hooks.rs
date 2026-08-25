//! `hooks/hooks.json` -- Claude Code's own declarative hook configuration.
//! CLOSE, lossy vocabulary translation (the spec's own framing): both
//! sides run "when `event` happens, run `command`," with the same
//! stdin-JSON/stdout-JSON protocol shape, but the event VOCABULARY differs,
//! and a rule naming an event with no conway counterpart must be reported
//! by name, never silently dropped (P-13).
//!
//! **Board item `01M0X1FCQ80C9ET97HENXSAW2K`: this module now produces
//! real, dispatchable registrations for every `Mapped` rule**
//! ([`HookTranslation::registration`],
//! [`crate::ClaudeCompatReport::hook_registrations`]) -- an earlier item
//! (`01M0VR89FB1F3Q4FQ8852K2A5E`) deliberately stopped at name-level
//! mapping; this one carries that
//! translation the rest of the way, on the operator ruling's own "best
//! effort, labelled" appetite (see this file's own doc on the six mapped
//! events, two of them `approximate`). What is STILL true, unconditionally,
//! for every registration this module hands back: the two sides' JSON
//! PAYLOAD shapes do not match even where the event NAME does. Claude
//! Code's hook script reads fields like `tool_name`/`tool_input` on stdin,
//! while conway's dispatcher sends its own `HookInvocation`/`HookEvent`
//! shape (`docs/plugins/hooks.md` point 13). A foreign script, unmodified,
//! handed conway's payload would not understand it, and whatever it
//! printed on stdout would not parse as conway's own `HookAnswer` either.
//! **"Dispatches" is not the same claim as "behaves identically to running
//! under real Claude Code."** A registration built here really runs its
//! command when the matching conway event fires; whether that command's
//! own stdin-reading, stdout-writing logic still makes sense fed a
//! different payload is the operator's own call to make, per hook, the
//! same as any hand-adapted script.
//!
//! This module still reports which Claude Code events have no conway
//! counterpart at all -- a harder, unconditional gap, always named in
//! [`UnsupportedItem`], never silently dropped.

use std::path::Path;

use crate::error::ClaudeCompatError;
use crate::fsutil::read_bounded;
use crate::unsupported::UnsupportedItem;

/// Mirrors `crate::plugin::DEFAULT_TIMEOUT_MS` in `conway`'s own schema
/// (`crates/conway/src/config/schema.rs`'s `default_hook_timeout_ms`) --
/// this module does not depend on `conway` in production code (see this
/// crate's own top-level doc, "Read-at-runtime"), so the value is
/// duplicated rather than imported. Used only when a Claude Code hook
/// entry sets no `"timeout"` of its own.
const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// The exact token Claude Code substitutes with its own plugin directory
/// when it spawns a hook command -- both real plugins this module has been
/// checked against (`beepboop` 1.4.0, `ideate` 3.2.2) use it in every
/// single `hooks.json` command. [`HookTranslation::registration`]
/// substitutes it with the discovered plugin directory's own absolute path
/// before wrapping the command for a shell -- without this, a translated
/// registration would dispatch reliably and just as reliably fail "no such
/// file," which is worse than not dispatching at all (looks installed,
/// never meaningfully runs). No other environment variable is substituted
/// or set; a translated rule's command inherits the parent process's own
/// environment unfiltered, same as any other `[hooks].rules[]` entry.
const PLUGIN_ROOT_TOKEN: &str = "${CLAUDE_PLUGIN_ROOT}";

/// One `hooks/hooks.json` rule, after event-name translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookTranslation {
    pub claude_event: String,
    pub matcher: Option<String>,
    /// The Claude Code hook's own command STRING, verbatim -- carried for
    /// operator reference, and also the source [`Self::registration`]
    /// translates into a real argv (see that method's own doc for how).
    /// `conway`'s own `HookEntry::command` is an argv `Vec<String>`, never a
    /// shell string (`docs/plugins/hooks.md`: "a `run` string only becomes
    /// a command once something decides where the words break, and
    /// deciding that means predicting a shell") -- this module does not
    /// guess that split; see [`Self::registration`] for the non-guessing
    /// way it still produces a real argv.
    pub command: String,
    /// Milliseconds from the Claude Code hook entry's own `"timeout"` key
    /// (Claude Code spells it in SECONDS; this field is already converted
    /// to milliseconds, `conway`'s own unit for
    /// `HookEntry::timeout_ms`). `None` when the key is absent or not a
    /// non-negative number -- [`Self::registration`] falls back to
    /// `DEFAULT_TIMEOUT_MS` in that case, the identical default an
    /// operator-authored `[hooks].rules[]` entry with no `timeout_ms` gets.
    pub timeout_ms: Option<u64>,
    pub outcome: HookMapOutcome,
}

/// Whether a translated rule's event has a same-named conway counterpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookMapOutcome {
    /// `conway_event` is the conway-side event name this rule's Claude Code
    /// event corresponds to BY NAME -- and, as of board item
    /// `01M0X1FCQ80C9ET97HENXSAW2K`, a rule with this outcome can become a
    /// real, dispatchable `[hooks].rules[]`-shaped registration via
    /// [`HookTranslation::registration`].
    ///
    /// `approximate` is true for the two pairs whose semantics are known to
    /// diverge from Claude Code's own firing conditions in at least one
    /// way (`SubagentStart`->`child_spawned`, `SubagentStop`->
    /// `child_reported`) rather than merely being unverified against every
    /// firing condition -- see `APPROXIMATE_CLAUDE_EVENTS`'s own doc for
    /// the one divergence this module already knows about for
    /// `child_reported`, and `docs/plugins/claude-compat.md`'s own coverage
    /// table for the operator-visible label.
    Mapped {
        conway_event: &'static str,
        approximate: bool,
    },
    Unmapped {
        reason: String,
    },
}

/// The six Claude Code events with a same-named conway counterpart -- board
/// item `01M0X1FCQ80C9ET97HENXSAW2K`'s own measured table, checked against
/// both real plugins' `hooks.json` this module has been run against
/// (`beepboop` 1.4.0, `ideate` 3.2.2). Four are exact by name
/// (`PreToolUse`/`PostToolUse`/`UserPromptSubmit`/`SessionStart`); two are
/// `APPROXIMATE_CLAUDE_EVENTS`.
const EVENT_MAP: &[(&str, &str)] = &[
    ("PreToolUse", "pre_tool_use"),
    ("PostToolUse", "post_tool_use"),
    ("UserPromptSubmit", "prompt_submitted"),
    ("SessionStart", "session_starting"),
    ("SubagentStart", "child_spawned"),
    ("SubagentStop", "child_reported"),
];

/// The subset of `EVENT_MAP` whose mapping is labelled `approximate`
/// rather than exact, per the operator ruling's own "best effort, and
/// disclosed" appetite -- mapped and USABLE, but not verified against every
/// one of Claude Code's own firing conditions.
///
/// **The one divergence already known, for `child_reported` specifically**
/// (`crates/conway/src/config/schema.rs`'s own `HooksConfig` doc): conway's
/// `child_reported` fires once per agent that HAS a parent, for both a
/// normal completion AND a supervisor-synthesized terminal result (a panic,
/// or a task still unresponsive past its grace window) -- Claude Code's own
/// `SubagentStop` may or may not fire for that second, synthesized case the
/// same way. Per the operator ruling: mapped, labelled, and not chased
/// further here -- a beepboop smoke test is what surfaces whether it
/// actually bites.
const APPROXIMATE_CLAUDE_EVENTS: &[&str] = &["SubagentStart", "SubagentStop"];

/// conway core events that carry a tool name in their payload -- the only
/// two a `match`/`matcher` can ever apply to
/// (`crates/conway/src/config/schema.rs`'s own `HookEntry::match_tool` doc:
/// "`match` on any event that carries no tool name ... is a load-time
/// config error"). A translated rule whose matcher would land on a
/// non-tool-carrying conway event is reclassified `Unmapped` rather than
/// silently dropping the matcher (P-13).
const TOOL_CARRYING_CONWAY_EVENTS: &[&str] = &["pre_tool_use", "post_tool_use"];

fn map_event(claude_event: &str) -> Option<&'static str> {
    EVENT_MAP
        .iter()
        .find(|(claude, _)| *claude == claude_event)
        .map(|(_, conway)| *conway)
}

fn unmapped_reason(claude_event: &str) -> String {
    if claude_event == "PreCompact" {
        "conway has no compaction mechanism yet for this event to fire from -- the one \
         first-party capability still unbuilt"
            .to_string()
    } else {
        format!("no conway hook event corresponds to Claude Code's \"{claude_event}\"")
    }
}

/// Reads `<dir>/hooks/hooks.json`, translating every rule's event name and
/// collecting an [`UnsupportedItem::hook`] for every rule whose Claude Code
/// event, or whose non-`"command"`-typed hook entry, has no conway
/// counterpart. `Ok(vec![])` when the file is absent.
pub(crate) fn read_hooks(
    dir: &Path,
    unsupported: &mut Vec<UnsupportedItem>,
) -> Result<Vec<HookTranslation>, ClaudeCompatError> {
    let path = dir.join("hooks").join("hooks.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = read_bounded(&path)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| ClaudeCompatError::MalformedJson {
            path: path.clone(),
            source,
        })?;

    let mut translations = Vec::new();
    let Some(events) = value.get("hooks").and_then(|v| v.as_object()) else {
        return Ok(translations);
    };
    for (claude_event, groups) in events {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let matcher = group
                .get("matcher")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let Some(hook_entries) = group.get("hooks").and_then(|v| v.as_array()) else {
                continue;
            };
            for hook_entry in hook_entries {
                let hook_type = hook_entry.get("type").and_then(|v| v.as_str());
                if hook_type != Some("command") {
                    unsupported.push(UnsupportedItem::hook(
                        claude_event.clone(),
                        format!(
                            "hook type {} has no conway counterpart -- only command-type \
                             hooks translate",
                            hook_type
                                .map(|t| format!("\"{t}\""))
                                .unwrap_or_else(|| "(absent)".to_string())
                        ),
                    ));
                    continue;
                }
                let Some(command) = hook_entry.get("command").and_then(|v| v.as_str()) else {
                    unsupported.push(UnsupportedItem::hook(
                        claude_event.clone(),
                        "a command-type hook entry with no string \"command\" field",
                    ));
                    continue;
                };

                let outcome = match map_event(claude_event) {
                    Some(conway_event) => {
                        if matcher.is_some() && !TOOL_CARRYING_CONWAY_EVENTS.contains(&conway_event)
                        {
                            let reason = format!(
                                "conway's \"{conway_event}\" event carries no tool name for a \
                                 \"matcher\" to narrow against"
                            );
                            unsupported
                                .push(UnsupportedItem::hook(claude_event.clone(), reason.clone()));
                            HookMapOutcome::Unmapped { reason }
                        } else {
                            HookMapOutcome::Mapped {
                                conway_event,
                                approximate: APPROXIMATE_CLAUDE_EVENTS
                                    .contains(&claude_event.as_str()),
                            }
                        }
                    }
                    None => {
                        let reason = unmapped_reason(claude_event);
                        unsupported
                            .push(UnsupportedItem::hook(claude_event.clone(), reason.clone()));
                        HookMapOutcome::Unmapped { reason }
                    }
                };

                // Claude Code spells this key in SECONDS; conway's own
                // `HookEntry::timeout_ms` (and this translation's own
                // `timeout_ms`) is milliseconds throughout.
                let timeout_ms = hook_entry
                    .get("timeout")
                    .and_then(|v| v.as_u64())
                    .map(|seconds| seconds.saturating_mul(1_000));

                translations.push(HookTranslation {
                    claude_event: claude_event.clone(),
                    matcher: matcher.clone(),
                    command: command.to_string(),
                    timeout_ms,
                    outcome,
                });
            }
        }
    }
    Ok(translations)
}

/// A ready-to-append `[hooks].rules[]`-shaped registration, produced only
/// from a [`HookMapOutcome::Mapped`] [`HookTranslation`]
/// ([`HookTranslation::registration`],
/// [`crate::ClaudeCompatReport::hook_registrations`]).
///
/// **Mirrors `conway::config::schema::HookEntry`'s own five fields
/// exactly, field name for field name -- deliberately NOT that literal
/// type.** This crate does not depend on `conway` in production code (see
/// this crate's own top-level doc, "Read-at-runtime" -- the identical
/// reason [`crate::mcp::TranslatedMcpServer`] produces a
/// `conway_plugin_mcp::McpPluginSpec` rather than a `conway`-owned type: no
/// second, heavier dependency for a thin translation layer to carry). A
/// caller wiring this into a real `conway::config::schema::HooksConfig`
/// converts field-by-field -- see this crate's own end-to-end test for the
/// one-line conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRegistration {
    /// Caller-assigned; see
    /// [`crate::ClaudeCompatReport::hook_registrations`] for the scheme
    /// this crate itself uses when it assigns one.
    pub id: String,
    pub event: &'static str,
    pub match_tool: Option<String>,
    /// Always `["/bin/sh", "-c", <command>]` -- see
    /// [`HookTranslation::registration`]'s own doc for why a real shell,
    /// never a guessed word-split, is what turns Claude Code's command
    /// STRING into conway's own argv `Vec<String>`.
    pub command: Vec<String>,
    pub timeout_ms: u64,
    pub enabled: bool,
}

impl HookTranslation {
    /// `Some` only when [`Self::outcome`] is [`HookMapOutcome::Mapped`] --
    /// `None` for an `Unmapped` rule, which has no conway event to
    /// register against (already named in [`UnsupportedItem`] by
    /// `read_hooks`).
    ///
    /// **The one lossy step, done here rather than left to a caller to
    /// improvise differently each time.** Claude Code's own `command` is a
    /// single shell STRING (may contain `&&`, quoting,
    /// `${CLAUDE_PLUGIN_ROOT}` interpolation); conway's own
    /// `HookEntry::command` is an argv
    /// `Vec<String>`. Rather than GUESS where the string's words break
    /// (the earlier item's own stated reason for refusing to wire dispatch
    /// at all -- see this module's own top doc), this hands the WHOLE
    /// string, unmodified, to a real shell: `["/bin/sh", "-c", <string>]`
    /// -- the identical shape `crates/conway-tools/src/shell/bash.rs`'s own
    /// `BashArgs::command` already uses for the same reason
    /// (`crates/conway/src/config/schema.rs`'s own `HookEntry::command` doc
    /// draws the same contrast). This preserves the string's own semantics
    /// exactly; it does NOT repair the stdin/stdout payload-shape mismatch
    /// this module's own top doc already discloses.
    ///
    /// `${CLAUDE_PLUGIN_ROOT}` (see `PLUGIN_ROOT_TOKEN`) is substituted
    /// with `plugin_dir`'s own absolute path before wrapping.
    /// `timeout_ms` falls back to `DEFAULT_TIMEOUT_MS` when the source
    /// entry set none. `enabled` is always `true` -- a translated rule that
    /// should not run yet is simply not translated at all (there is no
    /// Claude Code counterpart to a disabled rule).
    pub fn registration(
        &self,
        id: impl Into<String>,
        plugin_dir: &Path,
    ) -> Option<HookRegistration> {
        let HookMapOutcome::Mapped { conway_event, .. } = &self.outcome else {
            return None;
        };
        let resolved = self
            .command
            .replace(PLUGIN_ROOT_TOKEN, &plugin_dir.display().to_string());
        Some(HookRegistration {
            id: id.into(),
            event: conway_event,
            match_tool: self.matcher.clone(),
            command: vec!["/bin/sh".to_string(), "-c".to_string(), resolved],
            timeout_ms: self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS),
            enabled: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_hooks_json(dir: &Path, contents: &str) {
        std::fs::create_dir_all(dir.join("hooks")).unwrap();
        std::fs::write(dir.join("hooks").join("hooks.json"), contents).unwrap();
    }

    #[test]
    fn maps_every_claude_event_with_a_same_named_conway_counterpart() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{
                "PreToolUse": [{"matcher":"Bash","hooks":[{"type":"command","command":"echo pre"}]}],
                "PostToolUse": [{"hooks":[{"type":"command","command":"echo post"}]}],
                "UserPromptSubmit": [{"hooks":[{"type":"command","command":"echo prompt"}]}],
                "SessionStart": [{"hooks":[{"type":"command","command":"echo start"}]}],
                "SubagentStart": [{"hooks":[{"type":"command","command":"echo sub-start"}]}],
                "SubagentStop": [{"hooks":[{"type":"command","command":"echo sub-stop"}]}]
            }}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert!(unsupported.is_empty(), "{unsupported:?}");
        assert_eq!(translations.len(), 6);
        let mapped_to = |claude: &str| {
            translations
                .iter()
                .find(|t| t.claude_event == claude)
                .and_then(|t| match &t.outcome {
                    HookMapOutcome::Mapped {
                        conway_event,
                        approximate,
                    } => Some((*conway_event, *approximate)),
                    HookMapOutcome::Unmapped { .. } => None,
                })
        };
        assert_eq!(mapped_to("PreToolUse"), Some(("pre_tool_use", false)));
        assert_eq!(mapped_to("PostToolUse"), Some(("post_tool_use", false)));
        assert_eq!(
            mapped_to("UserPromptSubmit"),
            Some(("prompt_submitted", false))
        );
        assert_eq!(mapped_to("SessionStart"), Some(("session_starting", false)));
        // The approximate pair: mapped and usable, but flagged.
        assert_eq!(mapped_to("SubagentStart"), Some(("child_spawned", true)));
        assert_eq!(mapped_to("SubagentStop"), Some(("child_reported", true)));
    }

    /// The spec's own named list, checked one by one: these four Claude
    /// Code events have no conway counterpart at all (`SubagentStop` moved
    /// into the mapped-but-approximate set above; `Stop`, `Notification`,
    /// `PreCompact`, `SessionEnd` remain declined).
    #[test]
    fn the_four_remaining_events_with_no_conway_counterpart_are_all_reported_unmapped() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{
                "Stop": [{"hooks":[{"type":"command","command":"a"}]}],
                "Notification": [{"hooks":[{"type":"command","command":"c"}]}],
                "PreCompact": [{"hooks":[{"type":"command","command":"d"}]}],
                "SessionEnd": [{"hooks":[{"type":"command","command":"e"}]}]
            }}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert_eq!(translations.len(), 4);
        assert!(translations
            .iter()
            .all(|t| matches!(t.outcome, HookMapOutcome::Unmapped { .. })));
        assert_eq!(unsupported.len(), 4);
        for event in ["Stop", "Notification", "PreCompact", "SessionEnd"] {
            assert!(
                unsupported.iter().any(|u| u.name == event),
                "{event} must be named in the unsupported report: {unsupported:?}"
            );
        }
        // PreCompact gets its own, more specific reason (compaction is
        // unbuilt), not the generic "no counterpart" wording.
        let precompact = unsupported.iter().find(|u| u.name == "PreCompact").unwrap();
        assert!(precompact.reason.contains("compaction"), "{precompact:?}");
    }

    /// `SessionEnd` is declined and SETTLED by operator ruling (recorded in
    /// `docs/vision/DESIGN-permission-modes.md` §9) -- checked directly so
    /// a future accidental re-mapping is caught here, not just in a doc.
    #[test]
    fn session_end_is_declined_not_mapped() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"SessionEnd":[{"hooks":[{"type":"command","command":"a"}]}]}}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert_eq!(translations.len(), 1);
        assert!(matches!(
            translations[0].outcome,
            HookMapOutcome::Unmapped { .. }
        ));
        assert!(translations[0].registration("id", dir.path()).is_none());
    }

    #[test]
    fn a_matcher_on_a_non_tool_carrying_event_is_reclassified_unmapped() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"SessionStart":[{"matcher":"weird","hooks":[{"type":"command","command":"x"}]}]}}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert_eq!(translations.len(), 1);
        assert!(matches!(
            translations[0].outcome,
            HookMapOutcome::Unmapped { .. }
        ));
        assert_eq!(unsupported.len(), 1);
    }

    #[test]
    fn a_non_command_type_hook_is_reported_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"prompt","prompt":"ask the model"}]}]}}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert!(translations.is_empty());
        assert_eq!(unsupported.len(), 1);
        assert!(unsupported[0].reason.contains("command-type"));
    }

    #[test]
    fn an_absent_hooks_json_is_a_true_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert!(translations.is_empty());
        assert!(unsupported.is_empty());
    }

    // ---- HookTranslation::registration ----

    /// `registration` returns `None` for an `Unmapped` translation --
    /// there is no conway event for it to register against.
    #[test]
    fn registration_is_none_for_an_unmapped_translation() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert_eq!(translations.len(), 1);
        assert!(translations[0].registration("stop-1", dir.path()).is_none());
    }

    /// The headline shape claim: a `Mapped` translation becomes
    /// `["/bin/sh", "-c", <command with `${CLAUDE_PLUGIN_ROOT}` resolved>]`
    /// -- never a guessed word-split.
    #[test]
    fn registration_wraps_the_command_string_for_a_real_shell_and_resolves_the_plugin_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/scripts/play-sound.sh SessionStart && echo done","timeout":5}]}]}}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert_eq!(translations.len(), 1);
        let registration = translations[0]
            .registration("session-start-1", dir.path())
            .expect("SessionStart maps to session_starting");
        assert_eq!(registration.id, "session-start-1");
        assert_eq!(registration.event, "session_starting");
        assert_eq!(registration.match_tool, None);
        assert_eq!(registration.timeout_ms, 5_000);
        assert!(registration.enabled);
        assert_eq!(registration.command[0], "/bin/sh");
        assert_eq!(registration.command[1], "-c");
        let resolved_script = registration.command[2].as_str();
        assert!(
            !resolved_script.contains("${CLAUDE_PLUGIN_ROOT}"),
            "the literal token must be gone: {resolved_script:?}"
        );
        assert!(
            resolved_script.starts_with(&dir.path().display().to_string()),
            "the plugin directory's own absolute path must replace it: {resolved_script:?}"
        );
        assert!(
            resolved_script.ends_with("&& echo done"),
            "the rest of the command string must survive untouched: {resolved_script:?}"
        );
    }

    /// A `matcher` carries through onto the registration's own `match_tool`
    /// for a tool-carrying event.
    #[test]
    fn registration_carries_the_matcher_through_for_a_tool_carrying_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo pre"}]}]}}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        let registration = translations[0]
            .registration("pre-1", dir.path())
            .expect("PreToolUse maps to pre_tool_use");
        assert_eq!(registration.event, "pre_tool_use");
        assert_eq!(registration.match_tool.as_deref(), Some("Bash"));
    }

    /// No `"timeout"` key in the source entry falls back to the same
    /// default an operator-authored `[hooks].rules[]` entry gets.
    #[test]
    fn registration_falls_back_to_the_default_timeout_when_the_source_sets_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo start"}]}]}}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert_eq!(translations[0].timeout_ms, None);
        let registration = translations[0].registration("start-1", dir.path()).unwrap();
        assert_eq!(registration.timeout_ms, DEFAULT_TIMEOUT_MS);
    }
}
