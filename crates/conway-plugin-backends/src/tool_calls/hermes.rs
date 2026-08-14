//! Inline `<tool_call>...</tool_call>` text scanner for the `VllmHermes`
//! dialect's text-based tool-call fallback (vllm#31871): some
//! vLLM/Hermes servers, when `tools` are supplied but the request does not
//! force structured output, emit a tool call as raw text inside
//! `delta.content` — `<tool_call>{"name":...,"arguments":{...}}</tool_call>`
//! — with no `tool_calls` field on the delta at all.
//!
//! This module is a pure text scanner; it never decides *whether* it is in
//! scope for a given stream (that's [`super::ToolCallAccumulator`]'s job —
//! see `push_content_delta`, which only routes through this scanner while
//! `ToolCallStyle::HermesTextFallback` has not yet seen a structured
//! `delta.tool_calls` entry, per the "structured-path passthrough
//! when structured tool_calls appear" handoff).
//!
//! # Suppression algorithm
//!
//! Tag detection is retroactive: the scanner cannot know a `<` starts a
//! `<tool_call>` tag until it has seen the whole opening tag. So plain text
//! is held back (not yet returned to the caller for emission as a
//! `TextDelta`) exactly as long as its trailing suffix could still grow
//! into a prefix of `<tool_call>`; once a full tag is matched, the
//! held-back suffix is discarded (it was tag syntax, not content) and the
//! scanner switches into "inside a tag" mode, buffering raw text until
//! `</tool_call>` closes it and yields one parsed call. `finish` flushes
//! any residual held-back plain-text suffix (it never actually grew into a
//! tag) and errors if a tag was left open (an unterminated `<tool_call>` at
//! stream end).

use conway_core::error::BackendError;
use serde::Deserialize;
use serde_json::Value;

use super::truncate_chars;

const OPEN_TAG: &str = "<tool_call>";
const CLOSE_TAG: &str = "</tool_call>";

/// One parsed `<tool_call>...</tool_call>` block: a synthesized `call_{n}`
/// id, the tool name, and its arguments.
pub(crate) type HermesCall = (String, String, Value);

/// The result of feeding one `delta.content` text fragment into
/// [`HermesTextScanner::feed`]: `text` is plain content safe to emit as a
/// `TextDelta` (empty when everything fed was held back or suppressed),
/// and `calls` is every `<tool_call>` block that closed during this feed,
/// in order.
#[derive(Debug)]
pub(crate) struct HermesFeed {
    pub(crate) text: String,
    pub(crate) calls: Vec<HermesCall>,
}

/// Streaming scanner state for the `VllmHermes` inline-text fallback. See
/// the module docs for the suppression algorithm.
pub(crate) struct HermesTextScanner {
    /// Plain-text buffer: content not yet known to be safe to flush (it
    /// may still grow into an in-progress prefix of `<tool_call>`), valid
    /// only while `!in_tag`.
    buf: String,
    /// Raw text accumulated since the most recent opening tag, valid only
    /// while `in_tag`.
    tag_buf: String,
    in_tag: bool,
    call_count: u32,
}

impl HermesTextScanner {
    pub(crate) fn new() -> Self {
        Self {
            buf: String::new(),
            tag_buf: String::new(),
            in_tag: false,
            call_count: 0,
        }
    }

    /// Whether at least one `<tool_call>` block has been fully parsed —
    /// the signal `ToolCallAccumulator::stop_override` uses to force
    /// `StopReason::ToolUse` (vllm#31871: these servers commonly still
    /// report `finish_reason:"stop"` alongside inline tool-call text).
    pub(crate) fn saw_any_call(&self) -> bool {
        self.call_count > 0
    }

    /// Feeds one `delta.content` text fragment, returning any plain text
    /// now safe to emit and every `<tool_call>` block that closed as a
    /// result. A block whose inner text fails to parse as
    /// `{"name":...,"arguments":...}` is a `ToolParse` naming the bounded
    /// excerpt.
    pub(crate) fn feed(&mut self, text: &str) -> Result<HermesFeed, BackendError> {
        if self.in_tag {
            self.tag_buf.push_str(text);
        } else {
            self.buf.push_str(text);
        }

        let mut out_text = String::new();
        let mut calls = Vec::new();
        loop {
            if self.in_tag {
                match self.tag_buf.find(CLOSE_TAG) {
                    Some(pos) => {
                        let inner = self.tag_buf[..pos].to_string();
                        let rest = self.tag_buf[pos + CLOSE_TAG.len()..].to_string();
                        self.tag_buf.clear();
                        self.in_tag = false;
                        self.buf.push_str(&rest);
                        let (name, arguments) = parse_call(&inner)?;
                        let id = format!("call_{}", self.call_count);
                        self.call_count += 1;
                        calls.push((id, name, arguments));
                    }
                    None => break,
                }
            } else {
                match self.buf.find(OPEN_TAG) {
                    Some(pos) => {
                        out_text.push_str(&self.buf[..pos]);
                        let rest = self.buf[pos + OPEN_TAG.len()..].to_string();
                        self.buf.clear();
                        self.in_tag = true;
                        self.tag_buf.push_str(&rest);
                    }
                    None => {
                        let hold = held_back_suffix_len(&self.buf);
                        let flush_len = self.buf.len() - hold;
                        out_text.push_str(&self.buf[..flush_len]);
                        self.buf = self.buf[flush_len..].to_string();
                        break;
                    }
                }
            }
        }
        Ok(HermesFeed {
            text: out_text,
            calls,
        })
    }

    /// Flushes any residual held-back plain-text buffer at stream end. A
    /// still-open tag (an unterminated `<tool_call>` at `[DONE]`) is a
    /// `ToolParse` naming the bounded excerpt — a truncated inline tool
    /// call must not be silently dropped.
    pub(crate) fn finish(self) -> Result<String, BackendError> {
        if self.in_tag {
            return Err(BackendError::ToolParse {
                detail: format!(
                    "unterminated <tool_call> block at stream end: {}",
                    truncate_chars(&self.tag_buf, 256)
                ),
            });
        }
        Ok(self.buf)
    }
}

/// The longest suffix of `buf` that is a proper prefix of `<tool_call>`
/// (i.e. could still grow into a full opening tag on the next `feed`) —
/// held back rather than flushed as plain text.
fn held_back_suffix_len(buf: &str) -> usize {
    let max_k = (OPEN_TAG.len() - 1).min(buf.len());
    for k in (1..=max_k).rev() {
        if buf.ends_with(&OPEN_TAG[..k]) {
            return k;
        }
    }
    0
}

#[derive(Debug, Deserialize)]
struct HermesCallWire {
    name: String,
    #[serde(default)]
    arguments: Value,
}

fn parse_call(inner: &str) -> Result<(String, Value), BackendError> {
    let parsed: HermesCallWire =
        serde_json::from_str(inner.trim()).map_err(|_| BackendError::ToolParse {
            detail: format!(
                "unparseable hermes <tool_call> block: {}",
                truncate_chars(inner, 256)
            ),
        })?;
    Ok((parsed.name, parsed.arguments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_with_no_tag_passes_through_unchanged() {
        let mut scanner = HermesTextScanner::new();
        let feed = scanner.feed("hello world").unwrap();
        assert_eq!(feed.text, "hello world");
        assert!(feed.calls.is_empty());
    }

    #[test]
    fn a_single_tag_block_is_suppressed_and_parsed() {
        let mut scanner = HermesTextScanner::new();
        let feed = scanner
            .feed(r#"before <tool_call>{"name":"read","arguments":{"path":"a.txt"}}</tool_call> after"#)
            .unwrap();
        assert_eq!(feed.text, "before  after");
        assert_eq!(feed.calls.len(), 1);
        assert_eq!(feed.calls[0].0, "call_0");
        assert_eq!(feed.calls[0].1, "read");
        assert_eq!(feed.calls[0].2, serde_json::json!({"path": "a.txt"}));
    }

    #[test]
    fn tag_split_across_multiple_feed_calls_is_still_detected() {
        let mut scanner = HermesTextScanner::new();
        let mut all_text = String::new();
        let mut all_calls = Vec::new();
        for chunk in [
            "before <tool",
            "_call>{\"name\":\"read\",",
            "\"arguments\":{}}</tool_call>",
            " after",
        ] {
            let feed = scanner.feed(chunk).unwrap();
            all_text.push_str(&feed.text);
            all_calls.extend(feed.calls);
        }
        assert_eq!(all_text, "before  after");
        assert_eq!(all_calls.len(), 1);
        assert_eq!(all_calls[0].1, "read");
    }

    #[test]
    fn two_sequential_tag_blocks_each_get_a_distinct_synthesized_id() {
        let mut scanner = HermesTextScanner::new();
        let feed = scanner
            .feed(
                r#"<tool_call>{"name":"read","arguments":{}}</tool_call><tool_call>{"name":"write","arguments":{}}</tool_call>"#,
            )
            .unwrap();
        assert_eq!(feed.calls.len(), 2);
        assert_eq!(feed.calls[0].0, "call_0");
        assert_eq!(feed.calls[1].0, "call_1");
    }

    #[test]
    fn unterminated_tag_at_finish_is_tool_parse() {
        let mut scanner = HermesTextScanner::new();
        scanner.feed(r#"<tool_call>{"name":"read","argum"#).unwrap();
        let err = scanner.finish().unwrap_err();
        match err {
            BackendError::ToolParse { detail } => assert!(detail.contains("unterminated")),
            other => panic!("expected ToolParse, got {other:?}"),
        }
    }

    #[test]
    fn finish_flushes_residual_held_back_prefix() {
        let mut scanner = HermesTextScanner::new();
        let feed = scanner.feed("trailing <tool").unwrap();
        assert_eq!(feed.text, "trailing ");
        assert_eq!(scanner.finish().unwrap(), "<tool");
    }

    #[test]
    fn malformed_inner_json_is_tool_parse() {
        let mut scanner = HermesTextScanner::new();
        let err = scanner.feed("<tool_call>not json</tool_call>").unwrap_err();
        match err {
            BackendError::ToolParse { detail } => assert!(detail.contains("not json")),
            other => panic!("expected ToolParse, got {other:?}"),
        }
    }
}
