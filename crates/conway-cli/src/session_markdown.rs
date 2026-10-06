//! Markdown rendering of a session transcript -- `conway sessions export
//! --format markdown` (`commands/sessions.rs`) and the TUI's `/export`
//! (`tui/commands.rs`) both call [`render`], the one function in this
//! crate that turns a transcript into Markdown. Board item
//! `01M1YVW7JEYZ9VPR5FX3CZN7WQ`: a session log is right for machines and
//! forking, wrong for pasting into a PR, chat, or note -- this is the
//! human-facing rendering, built on top of the SAME `LogRecord` ->
//! [`crate::tui::state::Entry`] mapping the TUI's own `/resume` backfill
//! already uses (`crate::tui::state::backfill_entries`), not a second,
//! independent reading of a session's records. [`render`] itself takes
//! `&[Entry]`, never `&[LogRecord]` -- every caller runs `backfill_entries`
//! (or, for the TUI's live `AppState::transcript`, already has an
//! identically-built `Vec<Entry>` on hand, see `tui/commands.rs`'s own
//! `/export` arm) before reaching this module, so there is exactly one
//! place a record is ever turned into a transcript line, and this module
//! owns only the second, Markdown-specific step: Entry -> text.
//!
//! [`Header`]'s own fields (session id, name, model(s), token totals) are
//! NOT derived from `Entry` -- an `Entry::Assistant`'s `model` field is
//! `None` on every backfilled entry (`Entry::Assistant`'s own doc), so a
//! resumed session's header would read "model: (unknown)" for every
//! session that matters most to export. [`models_from_records`]/
//! [`usage_from_records`] instead read the ORIGINAL `LogRecord::Assistant`
//! records directly (each one carries its own `model`/`usage` fields,
//! never `None`), and every caller already has that slice on hand (it is
//! what `backfill_entries` itself was called with).
//!
//! ## Markdown safety
//!
//! Tool output and model text are untrusted strings that may contain their
//! own Markdown fences or raw backticks. `fenced_block` counts the
//! longest run of consecutive backticks already present in the content and
//! wraps it in a fence one backtick longer (floor 3) -- a fence the
//! content's own backtick runs can never prematurely close. Every string
//! this module writes is also run through `sanitize`, this module's OWN
//! control-character sanitizer -- NOT [`conway::sanitize_control_chars`]
//! unchanged, see that function's own doc for the one deliberate
//! difference: a raw ANSI escape, a bidirectional override, or any other
//! character [`conway::is_laundered_char`] names still cannot reach a
//! pasted Markdown file as a live control/format byte, but a plain `\n` is
//! left alone. `conway::sanitize_control_chars`'s own hazard -- a forged
//! control sequence surviving a later paste into a terminal -- never
//! applied to a bare line break at all, and multi-line tool output (the
//! overwhelmingly common case -- a file listing, a diff, build output)
//! NEEDS its line breaks to render as more than one visually garbled line
//! inside a fenced block. [`sanitize`] reaches the SAME [`conway::
//! is_laundered_char`] predicate `conway::sanitize_control_chars` is built
//! on for every OTHER character, rather than re-deriving a second copy of
//! its table.
//!
//! ## Out of scope
//!
//! Redaction of sensitive tool output (credentials, tokens a command
//! happened to print) is NOT performed here -- this module renders exactly
//! what the transcript already holds, the same policy `conway sessions
//! show`/`export --format jsonl` and the TUI pane itself already have. An
//! operator exporting a session for a PR or a chat is responsible for
//! reviewing it first, same as they would a terminal scrollback.

use conway::{LogRecord, SessionId, Usage};

use crate::tui::state::{Entry, ToolStatus};

/// The header block [`render`] writes before any transcript content --
/// session identity, which model(s) served it, and its cumulative token
/// spend. Built by the caller (never by this module) from the ORIGINAL
/// `LogRecord`s -- see this module's own doc for why.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub session_id: SessionId,
    /// The operator-bound name (`conway sessions name`), if any -- `None`
    /// renders no `name:` line at all, never a placeholder.
    pub name: Option<String>,
    /// Every distinct model that served an `Assistant` turn, in first-seen
    /// order -- see [`models_from_records`].
    pub models: Vec<String>,
    /// The session's cumulative token spend -- see [`usage_from_records`].
    pub usage: Usage,
}

/// Every distinct `ModelRef` that served an `Assistant` turn in
/// `records`, rendered with `ModelRef`'s own `backend/model` `Display`,
/// in first-seen order (never re-sorted -- the order a session actually
/// switched models in is itself informative, e.g. after a `/model`
/// mid-session switch).
pub fn models_from_records(records: &[LogRecord]) -> Vec<String> {
    let mut seen = Vec::new();
    for record in records {
        if let LogRecord::Assistant { model, .. } = record {
            let rendered = model.to_string();
            if !seen.contains(&rendered) {
                seen.push(rendered);
            }
        }
    }
    seen
}

/// The session's cumulative token spend: every `Assistant` record's own
/// `usage` field, summed -- mirrors `conway::SessionHandle::session_usage`'s
/// identical fold over the same record kind, so this header's total always
/// agrees with the facade's own answer for the same session.
pub fn usage_from_records(records: &[LogRecord]) -> Usage {
    records
        .iter()
        .filter_map(|record| match record {
            LogRecord::Assistant { usage, .. } => Some(*usage),
            _ => None,
        })
        .fold(Usage::default(), |acc, usage| acc + usage)
}

/// Renders `entries` (and `header`) as one Markdown document. `tool_lines`
/// caps a collapsed `Entry::Tool`'s output preview, mirroring the TUI
/// pane's own `tool_preview_lines` cap/affordance exactly (see
/// `fold_lines`) -- the CLI caller clamps an operator-supplied
/// `--tool-lines` the same way `tui::state::clamp_tool_preview_lines` does;
/// the TUI's own `/export` passes `AppState::tool_preview_lines` straight
/// through, so a hand-adjusted live cap is reflected in the export too.
pub fn render(entries: &[Entry], header: &Header, tool_lines: u32) -> String {
    let mut out = String::new();
    render_header(header, &mut out);
    for entry in entries {
        render_entry(entry, tool_lines, &mut out);
    }
    out
}

fn render_header(header: &Header, out: &mut String) {
    out.push_str(&format!("# Session {}\n\n", header.session_id));
    if let Some(name) = &header.name {
        out.push_str(&format!("- name: {}\n", sanitize(name)));
    }
    if !header.models.is_empty() {
        out.push_str(&format!("- model(s): {}\n", header.models.join(", ")));
    }
    let u = &header.usage;
    let total = u
        .input_tokens
        .saturating_add(u.output_tokens)
        .saturating_add(u.cache_read_tokens)
        .saturating_add(u.cache_write_tokens)
        .saturating_add(u.reasoning_tokens);
    out.push_str(&format!(
        "- tokens: {} in / {} out / {} cache-read / {} cache-write / {} reasoning (total {})\n",
        u.input_tokens,
        u.output_tokens,
        u.cache_read_tokens,
        u.cache_write_tokens,
        u.reasoning_tokens,
        total,
    ));
    out.push('\n');
}

/// One [`Entry`] -> zero or more Markdown lines, appended to `out`.
///
/// `Entry::QueuedUser` is the one variant deliberately skipped entirely --
/// it is a message typed while an agent was busy that was never actually
/// delivered as a turn (`Entry::QueuedUser`'s own doc: "best-effort,
/// LIVE-only ... never persisted"). Rendering it as if it were an ordinary
/// `User` turn would misrepresent what the model actually saw. Every other
/// variant gets its own section, including the ones `backfill_entries`
/// itself never produces (`Entry::Agent`/`Error`/`SecurityNotice`/`Shell`)
/// -- those CAN appear in the TUI's own live `AppState::transcript`, which
/// `/export` renders directly (this module's own doc), so leaving them
/// unhandled would silently drop real content from a live export.
fn render_entry(entry: &Entry, tool_lines: u32, out: &mut String) {
    match entry {
        Entry::User(text) => {
            if text.is_empty() {
                return;
            }
            out.push_str("## User\n\n");
            push_prose(out, text);
        }
        Entry::Assistant { text, model, .. } => {
            if text.is_empty() {
                return;
            }
            match model {
                Some(m) => out.push_str(&format!("## Assistant (`{}`)\n\n", sanitize(m))),
                None => out.push_str("## Assistant\n\n"),
            }
            push_prose(out, text);
        }
        Entry::Reasoning { text, .. } => {
            if text.is_empty() {
                return;
            }
            out.push_str("## Reasoning\n\n");
            push_blockquote(out, text);
        }
        Entry::Tool {
            name,
            status,
            preview,
            args,
            ..
        } => {
            out.push_str(&format!(
                "## Tool call: `{}` ({})\n\n",
                sanitize(name),
                tool_status_label(*status)
            ));
            if !args.is_empty() {
                out.push_str("Arguments:\n\n");
                fenced_block(out, args, "json");
            }
            if !preview.is_empty() {
                let (shown, hidden) = fold_lines(preview, tool_lines);
                out.push_str("Output:\n\n");
                fenced_block(out, &shown, "");
                if hidden > 0 {
                    out.push_str(&format!(
                        "_(+{hidden} more line{} not shown)_\n\n",
                        if hidden == 1 { "" } else { "s" }
                    ));
                }
            }
        }
        Entry::Agent { label, status, .. } => {
            let label = sanitize(label);
            out.push_str(&format!("## Agent: {label} ({status:?})\n\n"));
        }
        Entry::Notice { text } => push_notice_block(out, "Notice", text),
        Entry::Error { text, fatal } => {
            push_notice_block(out, if *fatal { "Fatal error" } else { "Error" }, text)
        }
        Entry::SecurityNotice { text } => push_notice_block(out, "Security notice", text),
        Entry::PermissionDecision { text, .. } => push_notice_block(out, "Permission", text),
        Entry::Shell {
            command,
            output,
            exit_code,
            ..
        } => {
            let status = match exit_code {
                Some(code) => format!("exit {code}"),
                None => "killed/timed out".to_string(),
            };
            out.push_str(&format!("## Shell command ({status})\n\n"));
            fenced_block(out, command, "sh");
            if !output.is_empty() {
                let (shown, hidden) = fold_lines(output, tool_lines);
                fenced_block(out, &shown, "");
                if hidden > 0 {
                    out.push_str(&format!(
                        "_(+{hidden} more line{} not shown)_\n\n",
                        if hidden == 1 { "" } else { "s" }
                    ));
                }
            }
        }
        // `Entry::QueuedUser` -- deliberately skipped, see this function's
        // own doc.
        Entry::QueuedUser { .. } => {}
    }
}

fn tool_status_label(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Proposed => "proposed",
        ToolStatus::AwaitingPermission => "awaiting permission",
        ToolStatus::Running => "running",
        ToolStatus::Finished { is_error: false } => "done",
        ToolStatus::Finished { is_error: true } => "failed",
    }
}

fn push_prose(out: &mut String, text: &str) {
    let sanitized = sanitize(text);
    out.push_str(&sanitized);
    if !sanitized.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
}

fn push_blockquote(out: &mut String, text: &str) {
    let sanitized = sanitize(text);
    for line in sanitized.split('\n') {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
}

fn push_notice_block(out: &mut String, label: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    out.push_str(&format!("## {label}\n\n"));
    push_prose(out, text);
}

/// Caps `text` to its first `cap` lines (`cap` floored at 1, mirroring
/// `tui/view/transcript.rs::tool_lines`'s own `cap.max(1)`), returning the
/// shown text and the count of hidden trailing lines (`0` when nothing was
/// cut).
fn fold_lines(text: &str, cap: u32) -> (String, usize) {
    let cap = cap.max(1) as usize;
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() <= cap {
        return (text.to_string(), 0);
    }
    (lines[..cap].join("\n"), lines.len() - cap)
}

/// Writes `content` as one fenced code block (language tag `lang`, empty
/// for none), sized so the content's own longest run of consecutive
/// backticks can never prematurely close the fence -- see this module's
/// own doc, "Markdown safety".
fn fenced_block(out: &mut String, content: &str, lang: &str) {
    let sanitized = sanitize(content);
    let fence = "`".repeat(fence_len(&sanitized));
    out.push_str(&fence);
    out.push_str(lang);
    out.push('\n');
    out.push_str(&sanitized);
    if !sanitized.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push_str("\n\n");
}

/// The backtick-fence length `fenced_block` must use for `content`: one
/// longer than the longest run of consecutive backticks `content` already
/// contains, floored at 3 (the shortest valid Markdown fence).
fn fence_len(content: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for c in content.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    (longest + 1).max(3)
}

/// This module's own control-character sanitizer -- deliberately NOT a bare
/// call to [`conway::sanitize_control_chars`]: that shared sentinel treats a
/// `\n` as just another laundered character (its own doc: the single source
/// of truth the permission gate's laundering-recognition and the runtime's
/// `rendered` seam both depend on, so it must never drift for EITHER of
/// them), which is correct there -- an embedded newline inside a
/// single-line shell-command DISPLAY or a permission-pattern match is itself
/// suspicious -- but wrong here: a session export is a static FILE, written
/// once, not a live terminal or a pattern-matching input, and this module's
/// own fenced-code-block output (see this module's own doc, "Markdown
/// safety") is multi-line by nature (a file listing, a diff, build output).
/// Replacing every embedded `\n` with a placeholder character would turn
/// ordinary multi-line tool output into a single garbled line -- this
/// function keeps `\n` as a real line break and launders every OTHER
/// character [`conway::is_laundered_char`] names (a raw control byte, a
/// bidirectional override, a line/paragraph separator, ...) -- the SAME
/// predicate [`conway::sanitize_control_chars`] is built on, so this
/// module's own rule can never drift from a second, independently-written
/// table. Same replacement character, `'\u{FFFD}'`, by convention -- not
/// importable here: `conway` does not re-export
/// `SANITIZED_CONTROL_PLACEHOLDER`, and `conway-core` is a test-only
/// dependency of this crate, never a production one.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\n' || !conway::is_laundered_char(c) {
                c
            } else {
                '\u{FFFD}'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::backfill_entries;
    use conway::backend::{ModelId, StopReason};
    use conway::plugin::ContentBlock;
    use conway::ModelRef;
    use conway_core::content::ToolResult;
    use conway_core::ids::{BackendId, LogSeq};

    fn ts() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn model() -> ModelRef {
        ModelRef {
            backend: BackendId::new("anthropic"),
            model: ModelId::new("claude-test"),
        }
    }

    fn usage(input: u32, output: u32) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cache_accounting: Default::default(),
        }
    }

    /// A minimal, realistic fixture session: one user turn, one assistant
    /// reply that calls a tool, the tool's result, and a second assistant
    /// reply with no further tool use.
    fn fixture_records() -> Vec<LogRecord> {
        vec![
            LogRecord::UserTurn {
                seq: LogSeq(0),
                ts: ts(),
                text: "list the files in /tmp".to_string(),
                prov: conway::Provenance::UserPrompt,
            },
            LogRecord::Assistant {
                seq: LogSeq(1),
                ts: ts(),
                content: vec![
                    ContentBlock::Text {
                        text: "Sure, let me check.".to_string(),
                    },
                    ContentBlock::ToolUse {
                        call_id: "tc_1".to_string(),
                        name: conway::ToolName::new("bash"),
                        arguments: serde_json::json!({"command": "ls /tmp"}),
                    },
                ],
                model: model(),
                route_reason: serde_json::json!({}),
                usage: usage(100, 20),
                stop: StopReason::ToolUse,
            },
            LogRecord::ToolResultRecord {
                seq: LogSeq(2),
                ts: ts(),
                result: ToolResult {
                    call_id: "tc_1".to_string(),
                    tool: conway::ToolName::new("bash"),
                    blocks: vec![ContentBlock::Text {
                        text: "a.txt\nb.txt".to_string(),
                    }],
                    is_error: false,
                    truncated: None,
                },
            },
            LogRecord::Assistant {
                seq: LogSeq(3),
                ts: ts(),
                content: vec![ContentBlock::Text {
                    text: "Found two files: a.txt and b.txt.".to_string(),
                }],
                model: model(),
                route_reason: serde_json::json!({}),
                usage: usage(50, 10),
                stop: StopReason::EndTurn,
            },
        ]
    }

    /// The golden test: a fixture session's exact Markdown rendering,
    /// compared byte for byte. Any change to the rendering shape must
    /// update this string deliberately, not accidentally.
    #[test]
    fn sessions_export_markdown_golden_fixture_renders_exact_markdown() {
        let records = fixture_records();
        let entries = backfill_entries(&records);
        let header = Header {
            session_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            name: Some("my-session".to_string()),
            models: models_from_records(&records),
            usage: usage_from_records(&records),
        };

        let got = render(&entries, &header, 3);

        let expected = "\
# Session 01ARZ3NDEKTSV4RRFFQ69G5FAV

- name: my-session
- model(s): anthropic/claude-test
- tokens: 150 in / 30 out / 0 cache-read / 0 cache-write / 0 reasoning (total 180)

## User

list the files in /tmp

## Assistant

Sure, let me check.

## Tool call: `bash` (done)

Arguments:

```json
{\"command\":\"ls /tmp\"}
```

Output:

```
a.txt
b.txt
```

## Assistant

Found two files: a.txt and b.txt.

";

        assert_eq!(got, expected, "got:\n{got}");
    }

    #[test]
    fn sessions_export_markdown_models_dedupe_in_first_seen_order() {
        let records = fixture_records();
        assert_eq!(
            models_from_records(&records),
            vec!["anthropic/claude-test".to_string()]
        );
    }

    #[test]
    fn sessions_export_markdown_usage_sums_every_assistant_record() {
        let records = fixture_records();
        let total = usage_from_records(&records);
        assert_eq!(total.input_tokens, 150);
        assert_eq!(total.output_tokens, 30);
    }

    /// The fence-length rule in isolation: content with no backticks gets
    /// the minimum 3-backtick fence.
    #[test]
    fn sessions_export_markdown_fence_len_floors_at_three_with_no_backticks() {
        assert_eq!(fence_len("plain text, no backticks at all"), 3);
    }

    /// Content containing a run of 3 backticks (an embedded Markdown fence)
    /// gets a 4-backtick fence -- one longer, so the embedded run can never
    /// close it early.
    #[test]
    fn sessions_export_markdown_fence_len_grows_past_embedded_triple_backtick_run() {
        assert_eq!(fence_len("before ```embedded fence``` after"), 4);
    }

    /// A longer embedded run (5) gets a 6-backtick fence -- the rule scales
    /// with the longest run present, not a fixed one-size-up from 3.
    #[test]
    fn sessions_export_markdown_fence_len_scales_with_longest_run_present() {
        assert_eq!(fence_len("`````` six backticks above this line"), 7);
    }

    /// `fenced_block`'s own output actually uses the grown fence, and the
    /// embedded run survives unescaped inside it -- the direct proof that
    /// the written block round-trips as one fenced region, not two.
    #[test]
    fn sessions_export_markdown_fenced_block_uses_fence_content_cannot_prematurely_close() {
        let mut out = String::new();
        fenced_block(&mut out, "has ```a fence``` inside it", "");
        assert!(out.starts_with("````\n"), "{out}");
        assert!(out.contains("has ```a fence``` inside it"), "{out}");
        // The closing fence is the SAME length as the opening one.
        assert!(out.trim_end().ends_with("````"), "{out}");
    }

    /// `fold_lines` caps at `cap` lines and reports how many were hidden --
    /// the shape `/export`'s `Output:` block depends on to show the
    /// trailing `_(+N more lines not shown)_` note.
    #[test]
    fn sessions_export_markdown_fold_lines_caps_and_reports_hidden_count() {
        let (shown, hidden) = fold_lines("a\nb\nc\nd\ne", 2);
        assert_eq!(shown, "a\nb");
        assert_eq!(hidden, 3);
    }

    #[test]
    fn sessions_export_markdown_fold_lines_reports_no_hidden_lines_under_cap() {
        let (shown, hidden) = fold_lines("a\nb", 5);
        assert_eq!(shown, "a\nb");
        assert_eq!(hidden, 0);
    }

    /// A control character embedded in tool output is replaced, never
    /// passed through raw -- the same guarantee `tui/view/transcript.rs`
    /// already proves for the live pane, now proven for the Markdown
    /// export too.
    #[test]
    fn sessions_export_markdown_control_character_in_tool_output_is_sanitized() {
        let entries = vec![Entry::Tool {
            call_id: "tc_1".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview: "before\x1b[31mred\x1b[0mafter".to_string(),
            args: String::new(),
            progress: String::new(),
            expanded: false,
            ts: None,
        }];
        let header = Header {
            session_id: SessionId::new(),
            name: None,
            models: Vec::new(),
            usage: Usage::default(),
        };
        let got = render(&entries, &header, 10);
        assert!(!got.contains('\x1b'), "{got}");
        assert!(got.contains('\u{FFFD}'), "{got}");
    }

    /// A bidirectional-override character embedded in tool output is
    /// replaced too, not just a `Cc` control byte -- `sanitize` reaches the
    /// same [`conway::is_laundered_char`] table [`conway::
    /// sanitize_control_chars`] is built on (board item
    /// `01M3SJC96P99V9KNT7TDJBWZ66`'s widened sanitizer).
    #[test]
    fn sessions_export_markdown_bidi_override_in_tool_output_is_sanitized() {
        let entries = vec![Entry::Tool {
            call_id: "tc_1".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview: "echo safe\u{202E}fr- mr\u{2066}".to_string(),
            args: String::new(),
            progress: String::new(),
            expanded: false,
            ts: None,
        }];
        let header = Header {
            session_id: SessionId::new(),
            name: None,
            models: Vec::new(),
            usage: Usage::default(),
        };
        let got = render(&entries, &header, 10);
        assert!(!got.contains('\u{202E}') && !got.contains('\u{2066}'), "{got}");
        assert!(got.contains('\u{FFFD}'), "{got}");
    }

    /// Multi-line user and assistant text keeps its line breaks: `sanitize`
    /// leaves `\n` alone, so a reply's paragraphs and lists survive export.
    #[test]
    fn sessions_export_markdown_multi_line_prose_keeps_its_line_breaks() {
        let mut out = String::new();
        push_prose(&mut out, "first line\n\n- a\n- b");
        assert_eq!(out, "first line\n\n- a\n- b\n\n");
        assert!(!out.contains('\u{FFFD}'), "{out}");
    }

    /// `Entry::QueuedUser` is skipped entirely -- see `render_entry`'s own
    /// doc for why.
    #[test]
    fn sessions_export_markdown_queued_user_entry_is_skipped() {
        let entries = vec![Entry::QueuedUser {
            text: "not actually sent yet".to_string(),
            steer: false,
        }];
        let header = Header {
            session_id: SessionId::new(),
            name: None,
            models: Vec::new(),
            usage: Usage::default(),
        };
        let got = render(&entries, &header, 3);
        assert!(!got.contains("not actually sent yet"), "{got}");
    }
}
