//! OpenAI canonical tool-call delta shape:
//! `{"index":0,"id":"call_abc","type":"function","function":{"name":"read","arguments":"…"}}`.
//!
//! `index` and `id` are present from the first chunk of a well-behaved
//! OpenAI stream; only `function.arguments` fragments across subsequent
//! chunks for the same `index`. This parser is also reused, unchanged, for
//! the `VllmHermes` dialect's structured tool-call path (WI-022): a
//! vLLM/Hermes server that emits well-formed `delta.tool_calls` (rather
//! than inline `<tool_call>…</tool_call>` text) uses exactly this shape.

use conway_core::error::BackendError;
use serde::Deserialize;

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
    arguments: Option<String>,
}

/// Parses one raw provider delta object — the element of
/// `choices[0].delta.tool_calls` — into dialect-independent [`DeltaParts`].
/// Anything unparseable as this shape is a `ToolParse` carrying the raw
/// delta, bounded to 256 chars.
pub(super) fn parse_delta(raw: &str) -> Result<DeltaParts, BackendError> {
    let parsed: RawDelta = serde_json::from_str(raw).map_err(|_| BackendError::ToolParse {
        detail: format!(
            "unparseable OpenAI tool-call delta: {}",
            truncate_chars(raw, 256)
        ),
    })?;
    let (name, arguments_fragment) = match parsed.function {
        Some(function) => (function.name, function.arguments),
        None => (None, None),
    };
    Ok(DeltaParts {
        index: parsed.index,
        id: parsed.id,
        name,
        arguments_fragment,
        arguments_value: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_shape() {
        let parts =
            parse_delta(r#"{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}"#)
                .unwrap();
        assert_eq!(parts.index, Some(0));
        assert_eq!(parts.id.as_deref(), Some("call_1"));
        assert_eq!(parts.name.as_deref(), Some("read"));
        assert_eq!(
            parts.arguments_fragment.as_deref(),
            Some(r#"{"path":"a.txt"}"#)
        );
        assert!(parts.arguments_value.is_none());
    }

    #[test]
    fn parses_argument_only_continuation_chunk() {
        let parts =
            parse_delta(r#"{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}"#).unwrap();
        assert!(parts.id.is_none());
        assert!(parts.name.is_none());
        assert_eq!(parts.arguments_fragment.as_deref(), Some(r#"th":"a.txt"}"#));
    }

    #[test]
    fn unparseable_delta_is_tool_parse_with_bounded_excerpt() {
        let raw = "not json at all";
        let err = parse_delta(raw).unwrap_err();
        match err {
            BackendError::ToolParse { detail } => assert!(detail.contains(raw)),
            other => panic!("expected ToolParse, got {other:?}"),
        }
    }
}
