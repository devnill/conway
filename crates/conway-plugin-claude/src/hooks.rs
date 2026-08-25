//! `hooks/hooks.json` -- Claude Code's own declarative hook configuration.
//! CLOSE, lossy vocabulary translation (the spec's own framing): both
//! sides run "when `event` happens, run `command`," with the same
//! stdin-JSON/stdout-JSON protocol shape, but the event VOCABULARY differs,
//! and a rule naming an event with no conway counterpart must be reported
//! by name, never silently dropped (P-13).
//!
//! **Name-level mapping only -- deliberately NOT wired to actually dispatch
//! (a scope decision, stated here because it is the one place a reader
//! might expect otherwise).** Even where an event NAME corresponds
//! (`PreToolUse` <-> `pre_tool_use`), the two sides' JSON PAYLOAD shapes do
//! not: Claude Code's hook script reads fields like `tool_name`/`tool_input`
//! on stdin, while conway's dispatcher sends its own `HookInvocation`/
//! `HookEvent` shape (`docs/plugins/hooks.md` point 13). A foreign script,
//! unmodified, handed conway's payload would not understand it, and
//! whatever it printed on stdout would not parse as conway's own
//! `HookAnswer` either -- silently wiring such a rule into a running
//! session's dispatch table would be exactly the "claims to be reached but
//! isn't" failure this whole item exists to prevent, just moved one layer
//! down (a hook that LOOKS installed but never meaningfully answers).
//! So this module reports which Claude Code events HAVE a same-named
//! conway counterpart (informational: this is the translation an operator
//! would still need to hand-adapt the script for) and which do not (a
//! harder, unconditional gap); it never appends anything to a
//! `conway`-side `HooksConfig`.

use std::path::Path;

use crate::error::ClaudeCompatError;
use crate::fsutil::read_bounded;
use crate::unsupported::UnsupportedItem;

/// One `hooks/hooks.json` rule, after event-name translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookTranslation {
    pub claude_event: String,
    pub matcher: Option<String>,
    /// The Claude Code hook's own command STRING, verbatim -- carried for
    /// operator reference only (this crate never runs it). `conway`'s own
    /// `HookEntry::command` is an argv `Vec<String>`, never a shell string
    /// (`docs/plugins/hooks.md`: "a `run` string only becomes a command
    /// once something decides where the words break, and deciding that
    /// means predicting a shell"); this crate does not guess that split
    /// either, since it never constructs a runnable conway hook rule at
    /// all (see this module's own doc for why).
    pub command: String,
    pub outcome: HookMapOutcome,
}

/// Whether a translated rule's event has a same-named conway counterpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookMapOutcome {
    /// `conway_event` is the conway-side event name this rule's Claude Code
    /// event corresponds to BY NAME. NOT a claim that the rule is wired or
    /// runnable -- see this module's own doc.
    Mapped {
        conway_event: &'static str,
    },
    Unmapped {
        reason: String,
    },
}

/// The four Claude Code core events with a same-named conway counterpart --
/// the spec's own accounting (nine Claude Code events total; `Stop`,
/// `SubagentStop`, `Notification`, `PreCompact`, and `SessionEnd`, five, have
/// none -- these four are the complement).
const EVENT_MAP: &[(&str, &str)] = &[
    ("PreToolUse", "pre_tool_use"),
    ("PostToolUse", "post_tool_use"),
    ("UserPromptSubmit", "prompt_submitted"),
    ("SessionStart", "session_starting"),
];

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
                            HookMapOutcome::Mapped { conway_event }
                        }
                    }
                    None => {
                        let reason = unmapped_reason(claude_event);
                        unsupported
                            .push(UnsupportedItem::hook(claude_event.clone(), reason.clone()));
                        HookMapOutcome::Unmapped { reason }
                    }
                };

                translations.push(HookTranslation {
                    claude_event: claude_event.clone(),
                    matcher: matcher.clone(),
                    command: command.to_string(),
                    outcome,
                });
            }
        }
    }
    Ok(translations)
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
                "SessionStart": [{"hooks":[{"type":"command","command":"echo start"}]}]
            }}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert!(unsupported.is_empty(), "{unsupported:?}");
        assert_eq!(translations.len(), 4);
        let mapped_to = |claude: &str| {
            translations
                .iter()
                .find(|t| t.claude_event == claude)
                .and_then(|t| match &t.outcome {
                    HookMapOutcome::Mapped { conway_event } => Some(*conway_event),
                    HookMapOutcome::Unmapped { .. } => None,
                })
        };
        assert_eq!(mapped_to("PreToolUse"), Some("pre_tool_use"));
        assert_eq!(mapped_to("PostToolUse"), Some("post_tool_use"));
        assert_eq!(mapped_to("UserPromptSubmit"), Some("prompt_submitted"));
        assert_eq!(mapped_to("SessionStart"), Some("session_starting"));
    }

    /// The spec's own named list, checked one by one: these five Claude
    /// Code events have no conway counterpart at all.
    #[test]
    fn the_five_events_with_no_conway_counterpart_are_all_reported_unmapped() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{
                "Stop": [{"hooks":[{"type":"command","command":"a"}]}],
                "SubagentStop": [{"hooks":[{"type":"command","command":"b"}]}],
                "Notification": [{"hooks":[{"type":"command","command":"c"}]}],
                "PreCompact": [{"hooks":[{"type":"command","command":"d"}]}],
                "SessionEnd": [{"hooks":[{"type":"command","command":"e"}]}]
            }}"#,
        );
        let mut unsupported = Vec::new();
        let translations = read_hooks(dir.path(), &mut unsupported).unwrap();
        assert_eq!(translations.len(), 5);
        assert!(translations
            .iter()
            .all(|t| matches!(t.outcome, HookMapOutcome::Unmapped { .. })));
        assert_eq!(unsupported.len(), 5);
        for event in [
            "Stop",
            "SubagentStop",
            "Notification",
            "PreCompact",
            "SessionEnd",
        ] {
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
}
