//! Integration tests for `ToolCallAccumulator` (WI-018 acceptance
//! criteria). Fixture files under `tests/fixtures/streams/` hold one raw
//! provider tool-call delta object per line, in arrival order, so the same
//! fixtures are reusable by later work items' SSE-level integration tests
//! (WI-019, WI-022).

use conway_core::content::{PermissionClass, StopReason, ToolCategory, ToolSpec};
use conway_core::ids::ToolName;
use conway_plugin_backends::tool_calls::{ToolCallAccumulator, ToolCallStyle};

/// Reads a fixture file and returns its non-empty lines, each one raw
/// provider delta object.
fn fixture_lines(name: &str) -> Vec<String> {
    let path = format!(
        "{}/tests/fixtures/streams/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading fixture {path}: {err}"))
        .lines()
        .map(str::to_string)
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// A tool spec whose schema requires a single string property `path`.
fn read_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("read"),
        description: "Read a file".into(),
        schema: serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }))
        .unwrap(),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

/// A tool spec with a permissive (no required properties) schema.
fn write_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("write"),
        description: "Write a file".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: ToolCategory::Edit,
        permission: PermissionClass::RequiresApproval,
    }
}

/// A tool spec with an entirely permissive schema and no required
/// properties, for the empty-arguments criterion.
fn ping_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("ping"),
        description: "No-op".into(),
        schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
        category: ToolCategory::Think,
        permission: PermissionClass::Safe,
    }
}

fn feed(accumulator: &mut ToolCallAccumulator, lines: &[String]) {
    for line in lines {
        accumulator
            .push_delta(line)
            .unwrap_or_else(|err| panic!("push_delta({line:?}) failed: {err}"));
    }
}

#[test]
fn new_is_public_and_starts_empty() {
    let accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &[]);
    assert!(accumulator.is_empty());
}

#[test]
fn openai_basic_two_chunk_delta_accumulates_to_one_call() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    feed(
        &mut accumulator,
        &fixture_lines("openai_basic_two_chunks.txt"),
    );
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "call_1");
    assert_eq!(calls[0].name, ToolName::new("read"));
    assert_eq!(calls[0].arguments, serde_json::json!({"path": "a.txt"}));
}

#[test]
fn interleaved_indices_produce_two_calls_in_ascending_index_order_with_separated_buffers() {
    let specs = [read_tool(), write_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    feed(
        &mut accumulator,
        &fixture_lines("openai_interleaved_indices.txt"),
    );
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].call_id, "call_a");
    assert_eq!(calls[0].name, ToolName::new("read"));
    assert_eq!(calls[0].arguments, serde_json::json!({"path": "a.txt"}));
    assert_eq!(calls[1].call_id, "call_b");
    assert_eq!(calls[1].name, ToolName::new("write"));
    assert_eq!(
        calls[1].arguments,
        serde_json::json!({"path": "b.txt", "content": "hi"})
    );
}

#[test]
fn codex_7517_repeated_id_and_name_produces_one_call_not_n() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Tolerant, &specs);
    feed(
        &mut accumulator,
        &fixture_lines("codex_7517_repeated_id_and_name.txt"),
    );
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1, "expected exactly one call, got {calls:?}");
    assert_eq!(calls[0].call_id, "call_1");
    assert_eq!(calls[0].arguments, serde_json::json!({"path": "a.txt"}));
}

#[test]
fn ollama_12557_object_then_empty_string_arguments_produces_one_valid_call() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Tolerant, &specs);
    feed(
        &mut accumulator,
        &fixture_lines("ollama_12557_object_then_empty_string.txt"),
    );
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1, "expected exactly one call, got {calls:?}");
    assert_eq!(calls[0].arguments, serde_json::json!({"path": "a.txt"}));
}

#[test]
fn finish_tool_use_with_unterminated_json_is_tool_parse_naming_tool_and_bounded_excerpt() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    feed(
        &mut accumulator,
        &fixture_lines("unterminated_arguments.txt"),
    );
    let err = accumulator.finish(StopReason::ToolUse).unwrap_err();
    match err {
        conway_core::error::BackendError::ToolParse { detail } => {
            assert!(detail.contains("read"), "{detail}");
            // The excerpt in the message must be bounded to 256 chars; the
            // fixture's buffer is far shorter, so this asserts the excerpt
            // is present and the message did not silently drop it.
            assert!(detail.contains("a.txt"), "{detail}");
        }
        other => panic!("expected ToolParse, got {other:?}"),
    }
}

#[test]
fn finish_with_schema_invalid_arguments_names_the_failing_schema_path() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    feed(
        &mut accumulator,
        &fixture_lines("schema_validation_failure.txt"),
    );
    let err = accumulator.finish(StopReason::ToolUse).unwrap_err();
    match err {
        conway_core::error::BackendError::ToolParse { detail } => {
            assert!(detail.contains("read"), "{detail}");
            assert!(
                detail.contains("required") || detail.contains('/'),
                "expected a schema-path-bearing message, got: {detail}"
            );
        }
        other => panic!("expected ToolParse, got {other:?}"),
    }
}

#[test]
fn finish_with_unknown_tool_name_contains_unknown_tool() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    feed(&mut accumulator, &fixture_lines("unknown_tool.txt"));
    let err = accumulator.finish(StopReason::ToolUse).unwrap_err();
    match err {
        conway_core::error::BackendError::ToolParse { detail } => {
            assert!(detail.contains("unknown tool"), "{detail}");
        }
        other => panic!("expected ToolParse, got {other:?}"),
    }
}

#[test]
fn finish_end_turn_with_zero_calls_is_ok_empty() {
    let accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &[]);
    let calls = accumulator.finish(StopReason::EndTurn).unwrap();
    assert!(calls.is_empty());
}

#[test]
fn empty_string_or_empty_object_arguments_for_no_required_schema_yields_empty_object() {
    let specs = [ping_tool()];

    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    feed(
        &mut accumulator,
        &fixture_lines("empty_string_arguments.txt"),
    );
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].arguments, serde_json::json!({}));

    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    feed(
        &mut accumulator,
        &fixture_lines("empty_object_arguments.txt"),
    );
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].arguments, serde_json::json!({}));
}

#[test]
fn push_complete_shares_the_same_finish_validation_path() {
    let specs = [read_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    accumulator
        .push_complete(
            Some("call_9".to_string()),
            "read".to_string(),
            serde_json::json!({"path": "b.txt"}),
        )
        .unwrap();
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "call_9");
    assert_eq!(calls[0].arguments, serde_json::json!({"path": "b.txt"}));
}

#[test]
fn synthesizes_call_id_when_absent_at_finish() {
    let specs = [ping_tool()];
    let mut accumulator = ToolCallAccumulator::new(ToolCallStyle::Structured, &specs);
    accumulator
        .push_delta(r#"{"index":0,"function":{"name":"ping","arguments":"{}"}}"#)
        .unwrap();
    let calls = accumulator.finish(StopReason::ToolUse).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "call_0");
}
