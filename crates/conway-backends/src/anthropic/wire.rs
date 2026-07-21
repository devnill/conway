//! Segment → Anthropic Messages API request mapping (`generate`/`stream`
//! request bodies) and Messages API response → `GenerateResponse` mapping
//! (architecture §"Module: conway-backends", WI-021).
//!
//! `build_request_body` never reads `PromptSegment::cache_hint` — that
//! omission is what makes the byte-identity invariant hold (§4.1):
//! stripping every `cache_hint` from a request's segments cannot change a
//! single byte of the body this function produces. [`BreakpointTarget`] is a
//! side channel this module emits alongside the body so `cache.rs` can
//! attach `cache_control` as a strictly additive post-pass, without
//! re-deriving the segment→JSON mapping itself.

use conway_core::content::{ContentBlock, Role, StopReason, ToolSpec, Usage};
use conway_core::error::BackendError;
use conway_core::ports::{GenerateRequest, GenerateResponse};
use conway_core::segment::PromptSegment;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::new_tool_call_accumulator;

/// Where, in the request body [`build_request_body`] produces, the segment
/// at the corresponding index landed — the last content block `cache.rs`
/// may attach a `cache_control` marker to. One entry per input segment, in
/// segment order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BreakpointTarget {
    /// `body["system"][entry_index]` — the system entry itself is the
    /// addressable block (no nested `content` array for system entries).
    System(usize),
    /// `body["messages"][message_index]["content"][block_index]` — the
    /// last content block this segment contributed, possibly to a message
    /// shared with an earlier merged `ToolResult` segment.
    Message {
        message_index: usize,
        block_index: usize,
    },
    /// The segment produced no addressable content block (e.g. empty
    /// content) — a cache hint on such a segment is silently dropped.
    None,
}

/// Builds the JSON request body for `POST {base}/v1/messages`, plus one
/// [`BreakpointTarget`] per `req.segments` entry (same order) for `cache.rs`
/// to consume. `default_max_tokens` is used when `req.params.max_tokens` is
/// unset — Anthropic's `max_tokens` field is required by the API.
pub(crate) fn build_request_body(
    req: &GenerateRequest,
    default_max_tokens: u32,
    stream: bool,
) -> (Value, Vec<BreakpointTarget>) {
    let (system_entries, messages, placements) = segments_to_body_parts(&req.segments);

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.as_str()));
    body.insert(
        "max_tokens".into(),
        json!(req.params.max_tokens.unwrap_or(default_max_tokens)),
    );
    if !system_entries.is_empty() {
        body.insert("system".into(), Value::Array(system_entries));
    }
    body.insert("messages".into(), Value::Array(messages));

    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|spec| {
                json!({
                    "name": spec.name.as_str(),
                    "description": spec.description,
                    "input_schema": spec.schema,
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }

    if let Some(temperature) = req.params.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = req.params.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if !req.params.stop.is_empty() {
        body.insert("stop_sequences".into(), json!(req.params.stop));
    }

    if stream {
        body.insert("stream".into(), json!(true));
    }

    (Value::Object(body), placements)
}

/// Maps every segment to zero-or-more `system`/`messages` entries, in
/// order, tracking each segment's [`BreakpointTarget`]. Segment order is
/// load-bearing (§5.3) and preserved exactly; nothing here reads
/// `segment.cache_hint`.
fn segments_to_body_parts(
    segments: &[PromptSegment],
) -> (Vec<Value>, Vec<Value>, Vec<BreakpointTarget>) {
    let mut system_entries: Vec<Value> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut placements: Vec<BreakpointTarget> = Vec::with_capacity(segments.len());
    let mut prev_role: Option<Role> = None;

    for segment in segments {
        match segment.role {
            Role::System => {
                let text = concat_text(&segment.content);
                system_entries.push(json!({ "type": "text", "text": text }));
                placements.push(BreakpointTarget::System(system_entries.len() - 1));
            }
            Role::User => {
                push_message(
                    &mut messages,
                    &mut placements,
                    "user",
                    user_content_blocks(&segment.content),
                );
            }
            Role::Assistant => {
                push_message(
                    &mut messages,
                    &mut placements,
                    "assistant",
                    assistant_content_blocks(&segment.content),
                );
            }
            Role::ToolResult => {
                let blocks = tool_result_blocks(&segment.content);
                // API requires consecutive tool results to share one user
                // message (Implementation Notes).
                if matches!(prev_role, Some(Role::ToolResult)) {
                    append_to_last_message(&mut messages, &mut placements, blocks);
                } else {
                    push_message(&mut messages, &mut placements, "user", blocks);
                }
            }
            // `Role` is `#[non_exhaustive]`; no fifth variant exists today.
            _ => placements.push(BreakpointTarget::None),
        }
        prev_role = Some(segment.role);
    }

    (system_entries, messages, placements)
}

/// Pushes a new `{"role":role,"content":blocks}` message and records the
/// placement of its last block (or `None` when `blocks` is empty).
fn push_message(
    messages: &mut Vec<Value>,
    placements: &mut Vec<BreakpointTarget>,
    role: &str,
    blocks: Vec<Value>,
) {
    let message_index = messages.len();
    let target = if blocks.is_empty() {
        BreakpointTarget::None
    } else {
        BreakpointTarget::Message {
            message_index,
            block_index: blocks.len() - 1,
        }
    };
    messages.push(json!({ "role": role, "content": Value::Array(blocks) }));
    placements.push(target);
}

/// Appends `blocks` to the most recently pushed message's `content` array —
/// the consecutive-`ToolResult`-merge rule — and records the placement of
/// the LAST block this specific segment contributed (the message's overall
/// last block, at the moment this segment finishes contributing to it).
fn append_to_last_message(
    messages: &mut [Value],
    placements: &mut Vec<BreakpointTarget>,
    blocks: Vec<Value>,
) {
    let message_index = messages.len() - 1;
    if blocks.is_empty() {
        placements.push(BreakpointTarget::None);
        return;
    }
    let content = messages[message_index]["content"]
        .as_array_mut()
        .expect("messages are always constructed with an array content field");
    content.extend(blocks);
    placements.push(BreakpointTarget::Message {
        message_index,
        block_index: content.len() - 1,
    });
}

/// Every `ContentBlock::Text` in `content`, concatenated verbatim (no
/// separator).
fn concat_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn user_content_blocks(content: &[ContentBlock]) -> Vec<Value> {
    let text = concat_text(content);
    if text.is_empty() {
        Vec::new()
    } else {
        vec![json!({ "type": "text", "text": text })]
    }
}

/// Thinking blocks (with a signature) first, then concatenated text, then
/// one `tool_use` block per `ContentBlock::ToolUse`, in `content` order — a
/// `Thinking` block without a signature is omitted (Implementation Notes).
fn assistant_content_blocks(content: &[ContentBlock]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for block in content {
        if let ContentBlock::Thinking {
            text,
            signature: Some(signature),
        } = block
        {
            blocks.push(json!({ "type": "thinking", "thinking": text, "signature": signature }));
        }
    }
    let text = concat_text(content);
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    for block in content {
        if let ContentBlock::ToolUse {
            call_id,
            name,
            arguments,
        } = block
        {
            blocks.push(json!({
                "type": "tool_use",
                "id": call_id,
                "name": name.as_str(),
                "input": arguments,
            }));
        }
    }
    blocks
}

fn tool_result_blocks(content: &[ContentBlock]) -> Vec<Value> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResultBlock {
                call_id,
                blocks,
                is_error,
            } => Some(json!({
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": concat_text(blocks),
                "is_error": is_error,
            })),
            _ => None,
        })
        .collect()
}

// --- Response mapping -------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct MessagesResponse {
    #[serde(default)]
    pub(crate) content: Vec<ResponseBlock>,
    #[serde(default)]
    pub(crate) stop_reason: Option<String>,
    #[serde(default)]
    pub(crate) usage: Option<UsageWire>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UsageWire {
    #[serde(default)]
    pub(crate) input_tokens: u32,
    #[serde(default)]
    pub(crate) output_tokens: u32,
    #[serde(default)]
    pub(crate) cache_read_input_tokens: u32,
    #[serde(default)]
    pub(crate) cache_creation_input_tokens: u32,
}

/// `stop_reason` → `StopReason`: `tool_use`→`ToolUse`, `max_tokens`→
/// `MaxTokens`, `stop_sequence`→`StopSequence`, `refusal`→`Refusal`,
/// `end_turn`/unknown/null→`EndTurn`.
pub(crate) fn map_stop_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some("refusal") => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

/// `input_tokens`/`output_tokens`/`cache_read_input_tokens`/
/// `cache_creation_input_tokens` → `Usage`.
pub(crate) fn map_usage(usage: Option<UsageWire>) -> Usage {
    match usage {
        Some(usage) => Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
            reasoning_tokens: 0,
        },
        None => Usage::default(),
    }
}

/// Maps a complete (non-streamed) Messages API response to a
/// `GenerateResponse`. `tool_use` blocks are fed to a fresh
/// `ToolCallAccumulator` via `push_complete` then `finish`, sharing exactly
/// the same validation path `stream.rs` uses.
pub(crate) fn to_generate_response(
    response: MessagesResponse,
    tools: &[ToolSpec],
) -> Result<GenerateResponse, BackendError> {
    let stop = map_stop_reason(response.stop_reason.as_deref());

    let mut content = Vec::new();
    let mut accumulator = new_tool_call_accumulator(tools);
    for block in response.content {
        match block {
            ResponseBlock::Text { text } => {
                if !text.is_empty() {
                    content.push(ContentBlock::Text { text });
                }
            }
            ResponseBlock::Thinking {
                thinking,
                signature,
            } => {
                content.push(ContentBlock::Thinking {
                    text: thinking,
                    signature,
                });
            }
            ResponseBlock::ToolUse { id, name, input } => {
                accumulator.push_complete(Some(id), name, input)?;
            }
            ResponseBlock::Other => {}
        }
    }
    let tool_calls = accumulator.finish(stop)?;

    Ok(GenerateResponse {
        content,
        tool_calls,
        stop,
        usage: map_usage(response.usage),
    })
}

#[cfg(test)]
mod tests {
    use conway_core::content::SamplingParams;
    use conway_core::ids::{ModelId, ToolName};
    use conway_core::ports::GenerateRequest;
    use conway_core::provenance::Provenance;

    use super::*;

    fn fixture_segments() -> Vec<PromptSegment> {
        vec![
            PromptSegment::new(
                Role::System,
                vec![ContentBlock::Text {
                    text: "You are a helpful assistant.".into(),
                }],
                Provenance::AgentDef {
                    name: "assistant".into(),
                },
            ),
            PromptSegment::new(
                Role::User,
                vec![ContentBlock::Text {
                    text: "What's the weather in Paris?".into(),
                }],
                Provenance::UserPrompt,
            ),
            PromptSegment::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    call_id: "call_1".into(),
                    name: ToolName::new("get_weather"),
                    arguments: json!({"city": "Paris"}),
                }],
                Provenance::SystemNote {
                    reason: "turn".into(),
                },
            ),
            PromptSegment::new(
                Role::ToolResult,
                vec![ContentBlock::ToolResultBlock {
                    call_id: "call_1".into(),
                    blocks: vec![ContentBlock::Text {
                        text: "22C, sunny".into(),
                    }],
                    is_error: false,
                }],
                Provenance::ToolResult {
                    call_id: "call_1".into(),
                    tool: ToolName::new("get_weather"),
                },
            ),
        ]
    }

    #[test]
    fn golden_four_segment_fixture_maps_to_expected_system_and_messages() {
        let (system, messages, _) = segments_to_body_parts(&fixture_segments());
        assert_eq!(
            Value::Array(system),
            json!([{"type": "text", "text": "You are a helpful assistant."}])
        );
        assert_eq!(
            Value::Array(messages),
            json!([
                {"role": "user", "content": [{"type": "text", "text": "What's the weather in Paris?"}]},
                {
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"city": "Paris"}}]
                },
                {
                    "role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "22C, sunny", "is_error": false}]
                }
            ])
        );
    }

    #[test]
    fn consecutive_tool_result_segments_merge_into_one_user_message() {
        let segments = vec![
            PromptSegment::new(
                Role::ToolResult,
                vec![ContentBlock::ToolResultBlock {
                    call_id: "call_1".into(),
                    blocks: vec![ContentBlock::Text { text: "a".into() }],
                    is_error: false,
                }],
                Provenance::ToolResult {
                    call_id: "call_1".into(),
                    tool: ToolName::new("t1"),
                },
            ),
            PromptSegment::new(
                Role::ToolResult,
                vec![ContentBlock::ToolResultBlock {
                    call_id: "call_2".into(),
                    blocks: vec![ContentBlock::Text { text: "b".into() }],
                    is_error: false,
                }],
                Provenance::ToolResult {
                    call_id: "call_2".into(),
                    tool: ToolName::new("t2"),
                },
            ),
        ];
        let (_, messages, placements) = segments_to_body_parts(&segments);
        assert_eq!(messages.len(), 1, "must merge into a single message");
        assert_eq!(
            messages[0],
            json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "a", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "call_2", "content": "b", "is_error": false}
                ]
            })
        );
        assert_eq!(
            placements[0],
            BreakpointTarget::Message {
                message_index: 0,
                block_index: 0
            }
        );
        assert_eq!(
            placements[1],
            BreakpointTarget::Message {
                message_index: 0,
                block_index: 1
            }
        );
    }

    #[test]
    fn cache_hint_never_changes_the_segments_to_body_parts_output() {
        use conway_core::ids::PrefixKey;
        use conway_core::segment::{strip_cache_hints, CacheHint, CacheTtl};

        let mut hinted = fixture_segments();
        let replaced = hinted.remove(0).with_cache_hint(CacheHint {
            breakpoint: true,
            ttl: CacheTtl::FiveMinutes,
            prefix_key: "deadbeef".parse::<PrefixKey>().unwrap(),
        });
        hinted.insert(0, replaced);
        let mut unhinted = hinted.clone();
        strip_cache_hints(&mut unhinted);

        let (system_a, messages_a, _) = segments_to_body_parts(&hinted);
        let (system_b, messages_b, _) = segments_to_body_parts(&unhinted);
        assert_eq!(system_a, system_b);
        assert_eq!(messages_a, messages_b);
    }

    #[test]
    fn max_tokens_defaults_when_params_unset() {
        let req = GenerateRequest {
            model: ModelId::new("claude-sonnet-4-6"),
            segments: vec![],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: None,
        };
        let (body, _) = build_request_body(&req, 8192, false);
        assert_eq!(body["max_tokens"], 8192);

        let req_with_max = GenerateRequest {
            params: SamplingParams {
                max_tokens: Some(512),
                ..SamplingParams::default()
            },
            ..req
        };
        let (body, _) = build_request_body(&req_with_max, 8192, false);
        assert_eq!(body["max_tokens"], 512);
    }

    #[test]
    fn map_stop_reason_table() {
        assert_eq!(map_stop_reason(Some("end_turn")), StopReason::EndTurn);
        assert_eq!(map_stop_reason(Some("tool_use")), StopReason::ToolUse);
        assert_eq!(map_stop_reason(Some("max_tokens")), StopReason::MaxTokens);
        assert_eq!(
            map_stop_reason(Some("stop_sequence")),
            StopReason::StopSequence
        );
        assert_eq!(map_stop_reason(Some("refusal")), StopReason::Refusal);
        assert_eq!(map_stop_reason(Some("unknown")), StopReason::EndTurn);
        assert_eq!(map_stop_reason(None), StopReason::EndTurn);
    }

    #[test]
    fn map_usage_reads_all_four_wire_fields() {
        let usage = map_usage(Some(UsageWire {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 2,
        }));
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 3);
        assert_eq!(usage.cache_write_tokens, 2);
        assert_eq!(map_usage(None), Usage::default());
    }
}
