//! Tolerant parser for the `Ollama` dialect's documented tool-call delta
//! quirks (research-backends: ollama#12557 and related reports), also
//! reused unchanged for `LmStudio` and `LlamaCppServer` (handoff —
//! see `mod.rs`):
//!
//! - `index` may be missing.
//! - `id` may be missing.
//! - a delta may be a bare `{"function":{...}}` object with no wrapper
//!   (already the general shape this parser accepts, since `index`/`id`
//!   are both `Option`).
//! - `function.arguments` may arrive as a JSON **object** rather than a
//!   string — in which case it is a complete value to set directly, never
//!   a fragment to append (ollama#12557: a complete-object chunk is
//!   sometimes followed by a spurious empty-string `arguments` chunk for
//!   the same call; the empty-string chunk must be a no-op, not an
//!   overwrite).
//!
//! Anything unparseable as this (deliberately permissive) shape is a
//! `ToolParse` carrying the raw delta, bounded to 256 chars.

use conway_core::error::BackendError;
use serde::Deserialize;
use serde_json::Value;

use super::{truncate_chars, DeltaParts};

#[derive(Debug, Deserialize)]
struct RawDelta {
    index: Option<u32>,
    id: Option<String>,
    function: Option<RawFunction>,
}

#[derive(Debug, Deserialize)]
struct RawFunction {
    name: Option<String>,
    arguments: Option<ArgumentsWire>,
}

/// `function.arguments` is either a JSON-encoded string fragment (the
/// OpenAI-canonical shape, still accepted here) or a complete JSON value
/// (the Ollama quirk). `serde(untagged)` tries each variant against the
/// raw JSON in order; a JSON string always matches `Fragment` first, a JSON
/// object/array/number/bool/null matches `Complete`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ArgumentsWire {
    Fragment(String),
    Complete(Value),
}

pub(super) fn parse_delta(raw: &str) -> Result<DeltaParts, BackendError> {
    let parsed: RawDelta = serde_json::from_str(raw).map_err(|_| BackendError::ToolParse {
        detail: format!("unparseable tool-call delta: {}", truncate_chars(raw, 256)),
    })?;
    let mut name = None;
    let mut arguments_fragment = None;
    let mut arguments_value = None;
    if let Some(function) = parsed.function {
        name = function.name;
        match function.arguments {
            Some(ArgumentsWire::Fragment(fragment)) => arguments_fragment = Some(fragment),
            Some(ArgumentsWire::Complete(value)) => arguments_value = Some(value),
            None => {}
        }
    }
    Ok(DeltaParts {
        index: parsed.index,
        id: parsed.id,
        name,
        arguments_fragment,
        arguments_value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_delta_without_index_or_id() {
        let parts =
            parse_delta(r#"{"function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}"#)
                .unwrap();
        assert!(parts.index.is_none());
        assert!(parts.id.is_none());
        assert_eq!(parts.name.as_deref(), Some("read"));
        assert_eq!(
            parts.arguments_fragment.as_deref(),
            Some(r#"{"path":"a.txt"}"#)
        );
    }

    #[test]
    fn accepts_object_valued_arguments_as_a_complete_value() {
        let parts = parse_delta(
            r#"{"id":"call_1","function":{"name":"read","arguments":{"path":"a.txt"}}}"#,
        )
        .unwrap();
        assert_eq!(
            parts.arguments_value,
            Some(serde_json::json!({"path":"a.txt"}))
        );
        assert!(parts.arguments_fragment.is_none());
    }

    #[test]
    fn accepts_empty_string_arguments_as_a_fragment() {
        let parts = parse_delta(r#"{"id":"call_1","function":{"arguments":""}}"#).unwrap();
        assert_eq!(parts.arguments_fragment.as_deref(), Some(""));
        assert!(parts.arguments_value.is_none());
    }

    #[test]
    fn unparseable_delta_is_tool_parse_with_bounded_excerpt() {
        let raw = "definitely not json";
        let err = parse_delta(raw).unwrap_err();
        match err {
            BackendError::ToolParse { detail } => assert!(detail.contains(raw)),
            other => panic!("expected ToolParse, got {other:?}"),
        }
    }
}
