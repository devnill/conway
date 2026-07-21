//! Segment → OpenAI-compatible chat message mapping (`generate`/`stream`
//! request bodies) and chat-completion response → `GenerateResponse`
//! mapping (architecture §"Module: conway-backends", WI-019).
//!
//! `PromptSegment.cache_hint` is never read anywhere in this module — that
//! omission, not a positive check, is what makes `CacheMode::ImplicitPrefix`
//! a wire no-op (§4.1): stripping every `cache_hint` from a request's
//! segments cannot change a single byte of the body this module produces.

use conway_core::content::{ContentBlock, Role, StopReason, ToolSpec, Usage};
use conway_core::error::BackendError;
use conway_core::ports::{GenerateRequest, GenerateResponse};
use conway_core::segment::PromptSegment;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::config::Dialect;
use crate::tool_calls::{truncate_chars, ToolCallAccumulator};

/// Builds the JSON request body for `POST {base}/chat/completions`.
/// `parallel_tool_calls` is the resolved `Capabilities::parallel_tool_calls`
/// for `req.model` — the field is only emitted when it is `true` **and**
/// `dialect` is `Dialect::OpenAi` (Implementation Notes: "other servers 400
/// on the unknown field").
pub(crate) fn build_request_body(
    req: &GenerateRequest,
    dialect: Dialect,
    parallel_tool_calls: bool,
    stream: bool,
) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.as_str()));
    body.insert(
        "messages".into(),
        Value::Array(segments_to_messages(&req.segments, dialect)),
    );

    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|spec| {
                json!({
                    "type": "function",
                    "function": {
                        "name": spec.name.as_str(),
                        "description": spec.description,
                        "parameters": spec.schema,
                    }
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        body.insert("tool_choice".into(), json!("auto"));
        if parallel_tool_calls && dialect.sends_parallel_tool_calls() {
            body.insert("parallel_tool_calls".into(), json!(true));
        }
    }

    if let Some(temperature) = req.params.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = req.params.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if let Some(max_tokens) = req.params.max_tokens {
        let key = if matches!(dialect, Dialect::OpenAi) {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body.insert(key.into(), json!(max_tokens));
    }
    if !req.params.stop.is_empty() {
        body.insert("stop".into(), json!(req.params.stop));
    }

    if stream {
        body.insert("stream".into(), json!(true));
        if dialect.supports_stream_options() {
            body.insert("stream_options".into(), json!({ "include_usage": true }));
        }
    }

    Value::Object(body)
}

/// Maps every segment to zero or more chat messages, in order. Segment
/// order is load-bearing (§5.3) and is preserved exactly; segments are
/// never merged or reordered (§8), and this function reads nothing from
/// `segment.cache_hint`.
fn segments_to_messages(segments: &[PromptSegment], dialect: Dialect) -> Vec<Value> {
    segments
        .iter()
        .flat_map(|segment| segment_to_messages(segment, dialect))
        .collect()
}

fn segment_to_messages(segment: &PromptSegment, dialect: Dialect) -> Vec<Value> {
    match segment.role {
        Role::System => vec![system_message(&segment.content)],
        Role::User => vec![user_message(&segment.content, dialect)],
        Role::Assistant => vec![assistant_message(&segment.content)],
        Role::ToolResult => tool_result_messages(&segment.content),
        // `Role` is `#[non_exhaustive]`; no fifth variant exists today.
        _ => Vec::new(),
    }
}

/// Every `ContentBlock::Text` in `content`, concatenated verbatim (no
/// separator) — `ContentBlock::Thinking` and every other block kind are
/// omitted from the request for both dialects (Implementation Notes).
fn concat_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn system_message(content: &[ContentBlock]) -> Value {
    json!({ "role": "system", "content": concat_text(content) })
}

fn user_message(content: &[ContentBlock], dialect: Dialect) -> Value {
    let blocks: Vec<&str> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let content_value = match blocks.as_slice() {
        [] => Value::String(String::new()),
        [single] => Value::String((*single).to_string()),
        multiple if dialect.flatten_multiblock_user() => Value::String(multiple.join("\n\n")),
        multiple => Value::Array(
            multiple
                .iter()
                .map(|text| json!({ "type": "text", "text": text }))
                .collect(),
        ),
    };
    json!({ "role": "user", "content": content_value })
}

fn assistant_message(content: &[ContentBlock]) -> Value {
    let text = concat_text(content);
    let tool_calls: Vec<Value> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                call_id,
                name,
                arguments,
            } => Some(json!({
                "id": call_id,
                "type": "function",
                "function": {
                    "name": name.as_str(),
                    "arguments": serde_json::to_string(arguments).unwrap_or_default(),
                }
            })),
            _ => None,
        })
        .collect();

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            json!(text)
        },
    );
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    Value::Object(message)
}

fn tool_result_messages(content: &[ContentBlock]) -> Vec<Value> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResultBlock {
                call_id, blocks, ..
            } => Some(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": concat_text(blocks),
            })),
            _ => None,
        })
        .collect()
}

// --- Response mapping -------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ChatCompletionResponse {
    #[serde(default)]
    pub(crate) choices: Vec<Choice>,
    #[serde(default)]
    pub(crate) usage: Option<UsageWire>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Choice {
    pub(crate) message: ResponseMessage,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponseMessage {
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<ResponseToolCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponseToolCall {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) function: ResponseFunctionCall,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponseFunctionCall {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) arguments: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UsageWire {
    #[serde(default)]
    pub(crate) prompt_tokens: u32,
    #[serde(default)]
    pub(crate) completion_tokens: u32,
    #[serde(default)]
    pub(crate) prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PromptTokensDetails {
    #[serde(default)]
    pub(crate) cached_tokens: Option<u32>,
}

/// `finish_reason` → `StopReason`: `stop`→`EndTurn`,
/// `tool_calls`/`function_call`→`ToolUse`, `length`→`MaxTokens`,
/// `content_filter`→`Refusal`, unknown/null→`EndTurn`.
pub(crate) fn map_finish_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

/// `usage.prompt_tokens`/`completion_tokens` → `input_tokens`/
/// `output_tokens`; `usage.prompt_tokens_details.cached_tokens` (when
/// present) → `cache_read_tokens`, else `0` (`Usage`'s fields are `u32`,
/// not `Option<u32>`).
pub(crate) fn map_usage(usage: Option<UsageWire>) -> Usage {
    match usage {
        Some(usage) => Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            cache_read_tokens: usage
                .prompt_tokens_details
                .and_then(|details| details.cached_tokens)
                .unwrap_or(0),
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        },
        None => Usage::default(),
    }
}

/// Maps a complete (non-streamed) chat-completion response to a
/// `GenerateResponse`. `choices[0].message.tool_calls` is fed to a fresh
/// `ToolCallAccumulator` via `push_complete` then `finish`, sharing exactly
/// the same validation path `stream.rs` uses.
pub(crate) fn to_generate_response(
    response: ChatCompletionResponse,
    dialect: Dialect,
    tools: &[ToolSpec],
) -> Result<GenerateResponse, BackendError> {
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| BackendError::BadRequest {
            detail: "chat completion response has no choices".into(),
        })?;

    let stop = map_finish_reason(choice.finish_reason.as_deref());

    let mut content = Vec::new();
    if let Some(text) = choice.message.content.filter(|text| !text.is_empty()) {
        content.push(ContentBlock::Text { text });
    }

    let mut accumulator = ToolCallAccumulator::new(dialect, tools);
    for tool_call in choice.message.tool_calls {
        let arguments = if tool_call.function.arguments.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(&tool_call.function.arguments).map_err(|_| {
                BackendError::ToolParse {
                    detail: format!(
                        "tool `{}`: unterminated JSON arguments (truncated to 256 chars): {}",
                        tool_call.function.name,
                        truncate_chars(&tool_call.function.arguments, 256)
                    ),
                }
            })?
        };
        accumulator.push_complete(tool_call.id, tool_call.function.name, arguments)?;
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
    use conway_core::ids::{ModelId, PrefixKey, ToolName};
    use conway_core::provenance::Provenance;
    use conway_core::segment::{strip_cache_hints, CacheHint, CacheTtl};

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
    fn segment_to_message_mapping_matches_golden_json_for_the_four_segment_fixture() {
        let segments = fixture_segments();
        let messages = segments_to_messages(&segments, Dialect::OpenAi);
        let golden = json!([
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "What's the weather in Paris?"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}"
                    }
                }]
            },
            {"role": "tool", "tool_call_id": "call_1", "content": "22C, sunny"}
        ]);
        assert_eq!(Value::Array(messages), golden);
    }

    #[test]
    fn cache_hint_never_changes_the_serialized_request_body() {
        let mut with_hint = fixture_segments();
        let hinted = with_hint.remove(0).with_cache_hint(CacheHint {
            breakpoint: true,
            ttl: CacheTtl::FiveMinutes,
            prefix_key: "deadbeef".parse::<PrefixKey>().unwrap(),
        });
        with_hint.insert(0, hinted);

        let mut without_hint = with_hint.clone();
        strip_cache_hints(&mut without_hint);
        assert!(with_hint[0].cache_hint.is_some());
        assert!(without_hint[0].cache_hint.is_none());

        let req_with_hint = GenerateRequest {
            model: ModelId::new("test-model"),
            segments: with_hint,
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: None,
        };
        let req_without_hint = GenerateRequest {
            segments: without_hint,
            ..req_with_hint.clone()
        };

        let with_hint_body = build_request_body(&req_with_hint, Dialect::OpenAi, true, false);
        let without_hint_body = build_request_body(&req_without_hint, Dialect::OpenAi, true, false);
        assert_eq!(
            serde_json::to_vec(&with_hint_body).unwrap(),
            serde_json::to_vec(&without_hint_body).unwrap()
        );
    }

    #[test]
    fn max_tokens_field_name_is_dialect_specific() {
        let req = GenerateRequest {
            model: ModelId::new("m"),
            segments: vec![],
            tools: vec![],
            params: SamplingParams {
                max_tokens: Some(256),
                ..SamplingParams::default()
            },
            prefix_key: None,
        };
        let openai_body = build_request_body(&req, Dialect::OpenAi, false, false);
        assert_eq!(openai_body["max_completion_tokens"], 256);
        assert!(openai_body.get("max_tokens").is_none());

        let ollama_body = build_request_body(&req, Dialect::Ollama, false, false);
        assert_eq!(ollama_body["max_tokens"], 256);
        assert!(ollama_body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn parallel_tool_calls_is_emitted_only_for_openai_with_tools_present() {
        let tool = ToolSpec {
            name: ToolName::new("read"),
            description: "read a file".into(),
            schema: serde_json::from_value(json!({"type": "object"})).unwrap(),
            category: conway_core::content::ToolCategory::Read,
            permission: conway_core::content::PermissionClass::Safe,
        };
        let req = GenerateRequest {
            model: ModelId::new("m"),
            segments: vec![],
            tools: vec![tool],
            params: SamplingParams::default(),
            prefix_key: None,
        };

        let openai_body = build_request_body(&req, Dialect::OpenAi, true, false);
        assert_eq!(openai_body["parallel_tool_calls"], true);

        let ollama_body = build_request_body(&req, Dialect::Ollama, true, false);
        assert!(ollama_body.get("parallel_tool_calls").is_none());

        let openai_body_false_cap = build_request_body(&req, Dialect::OpenAi, false, false);
        assert!(openai_body_false_cap.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn stream_flag_and_stream_options_are_dialect_gated() {
        let req = GenerateRequest {
            model: ModelId::new("m"),
            segments: vec![],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: None,
        };
        let openai_body = build_request_body(&req, Dialect::OpenAi, false, true);
        assert_eq!(openai_body["stream"], true);
        assert_eq!(openai_body["stream_options"]["include_usage"], true);

        let vllm_body = build_request_body(&req, Dialect::VllmHermes, false, true);
        assert_eq!(vllm_body["stream"], true);
        assert!(vllm_body.get("stream_options").is_none());
    }

    #[test]
    fn map_finish_reason_table() {
        assert_eq!(map_finish_reason(Some("stop")), StopReason::EndTurn);
        assert_eq!(map_finish_reason(Some("tool_calls")), StopReason::ToolUse);
        assert_eq!(
            map_finish_reason(Some("function_call")),
            StopReason::ToolUse
        );
        assert_eq!(map_finish_reason(Some("length")), StopReason::MaxTokens);
        assert_eq!(
            map_finish_reason(Some("content_filter")),
            StopReason::Refusal
        );
        assert_eq!(
            map_finish_reason(Some("unknown-thing")),
            StopReason::EndTurn
        );
        assert_eq!(map_finish_reason(None), StopReason::EndTurn);
    }

    #[test]
    fn map_usage_reads_cached_tokens_when_present() {
        let usage = map_usage(Some(UsageWire {
            prompt_tokens: 10,
            completion_tokens: 5,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(4),
            }),
        }));
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 4);

        let usage_no_details = map_usage(Some(UsageWire {
            prompt_tokens: 1,
            completion_tokens: 1,
            prompt_tokens_details: None,
        }));
        assert_eq!(usage_no_details.cache_read_tokens, 0);

        assert_eq!(map_usage(None), Usage::default());
    }
}
