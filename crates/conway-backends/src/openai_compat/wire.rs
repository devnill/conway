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

use crate::profile::Profile;
use crate::tool_calls::{truncate_chars, ToolCallAccumulator};

/// Builds the JSON request body for `POST {base}{profile.chat_path}`.
/// `parallel_tool_calls` is the resolved `Capabilities::parallel_tool_calls`
/// for `req.model` — the field is only emitted when it is `true` **and**
/// `profile.sends_parallel_tool_calls` (Implementation Notes: "other
/// servers 400 on the unknown field").
pub(crate) fn build_request_body(
    req: &GenerateRequest,
    profile: &Profile,
    parallel_tool_calls: bool,
    stream: bool,
) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.as_str()));
    body.insert(
        "messages".into(),
        Value::Array(segments_to_messages(&req.segments, profile)),
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
        if parallel_tool_calls && profile.sends_parallel_tool_calls {
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
        let key = if profile.uses_max_completion_tokens {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body.insert(key.into(), json!(max_tokens));
    }
    if !req.params.stop.is_empty() {
        body.insert("stop".into(), json!(req.params.stop));
    }
    if let Some(effort) = reasoning_effort(req, profile) {
        body.insert("reasoning_effort".into(), json!(effort));
    }

    if stream {
        body.insert("stream".into(), json!(true));
        if profile.supports_stream_options {
            body.insert("stream_options".into(), json!({ "include_usage": true }));
        }
    }

    Value::Object(body)
}

/// Reads a caller-supplied reasoning effort level out of
/// `params.extra["reasoning_effort"]` and serializes it verbatim as the
/// OpenAI `reasoning_effort` chat-completion field (WI-129), e.g. `"low"` /
/// `"medium"` / `"high"`.
///
/// `GenerateRequest` has no dedicated reasoning-effort field yet — that
/// caller-facing knob and its plumbing into `params.extra` is a
/// WI-126/WI-128 concern, outside this module's scope; `extra` is the only
/// existing field that reaches this wire layer. Emitted only when
/// `profile.sends_reasoning_effort`, mirroring the `parallel_tool_calls`
/// gating above: other OpenAI-compatible servers 400 on a field they don't
/// recognize.
fn reasoning_effort(req: &GenerateRequest, profile: &Profile) -> Option<String> {
    if !profile.sends_reasoning_effort {
        return None;
    }
    req.params
        .extra
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Maps every segment to zero or more chat messages, in order. Segment
/// order is load-bearing (§5.3) and is preserved exactly; segments are
/// never merged or reordered (§8), and this function reads nothing from
/// `segment.cache_hint`.
fn segments_to_messages(segments: &[PromptSegment], profile: &Profile) -> Vec<Value> {
    segments
        .iter()
        .flat_map(|segment| segment_to_messages(segment, profile))
        .collect()
}

fn segment_to_messages(segment: &PromptSegment, profile: &Profile) -> Vec<Value> {
    match segment.role {
        Role::System => vec![system_message(&segment.content)],
        Role::User => vec![user_message(&segment.content, profile)],
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

fn user_message(content: &[ContentBlock], profile: &Profile) -> Value {
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
        multiple if profile.flatten_multiblock_user => Value::String(multiple.join("\n\n")),
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
    // WI-122: send an empty STRING (never `null`) for a tool-call-only
    // assistant turn. OpenAI accepts `content: null` when `tool_calls` is
    // present, but Ollama Cloud / glm-5.2 rejects it with
    // `bad request: invalid message content type: <nil>`, which fails every
    // tool-continuation request. `""` is accepted by every dialect (OpenAI
    // included), so it is the safe universal choice.
    message.insert("content".into(), json!(text));
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
    /// Reasoning-model dialects (DeepSeek-R1 style, served via vLLM/Ollama/
    /// LM Studio) return the model's reasoning trace here — `stream.rs`
    /// already surfaces the streamed equivalent as `ThinkingDelta`; this is
    /// its non-streaming counterpart (WI-129). `reasoning` is accepted as
    /// an alias for servers that use that key instead.
    #[serde(default, alias = "reasoning")]
    pub(crate) reasoning_content: Option<String>,
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
    /// Kimi's Moonshot platform API reports `cached_tokens` at the TOP
    /// LEVEL of `usage`, not nested under `prompt_tokens_details` like
    /// OpenAI. `map_usage` reads either shape (nested wins when both are
    /// present) rather than gating this field on a provider profile: it is
    /// strictly permissive — a server that never sends it simply never
    /// populates this field, so accepting it costs nothing and needs no
    /// per-provider knowledge.
    #[serde(default)]
    pub(crate) cached_tokens: Option<u32>,
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
/// `output_tokens`; cached-token count → `cache_read_tokens`, read from
/// EITHER shape a server might send: OpenAI's nested
/// `usage.prompt_tokens_details.cached_tokens`, or Kimi's top-level
/// `usage.cached_tokens` — nested wins when a (hypothetical) server sends
/// both, else whichever is present, else `0` (`Usage`'s fields are `u32`,
/// not `Option<u32>`). Either-shape rather than a per-profile flag: this is
/// strictly more permissive (a server that only ever sends one shape is
/// unaffected either way) and needs no per-provider knowledge to get right.
pub(crate) fn map_usage(usage: Option<UsageWire>) -> Usage {
    match usage {
        Some(usage) => {
            let nested = usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens);
            Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cache_read_tokens: nested.or(usage.cached_tokens).unwrap_or(0),
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            }
        }
        None => Usage::default(),
    }
}

/// Maps a complete (non-streamed) chat-completion response to a
/// `GenerateResponse`. `choices[0].message.tool_calls` is fed to a fresh
/// `ToolCallAccumulator` via `push_complete` then `finish`, sharing exactly
/// the same validation path `stream.rs` uses.
pub(crate) fn to_generate_response(
    response: ChatCompletionResponse,
    profile: &Profile,
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
    if let Some(reasoning) = choice
        .message
        .reasoning_content
        .filter(|text| !text.is_empty())
    {
        // No signature: unlike Anthropic, these dialects have no
        // cross-turn integrity token, and their contract is the opposite —
        // reasoning content must NOT be resent (`assistant_message`
        // already omits every `ContentBlock::Thinking`, so this is
        // preserved in the response for observability without ever
        // reaching the next request body).
        content.push(ContentBlock::Thinking {
            text: reasoning,
            signature: None,
        });
    }
    if let Some(text) = choice.message.content.filter(|text| !text.is_empty()) {
        content.push(ContentBlock::Text { text });
    }

    let mut accumulator = ToolCallAccumulator::new(profile.tool_call_style, tools);
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

    use crate::config::Dialect;

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
        let messages = segments_to_messages(&segments, &Dialect::OpenAi.profile());
        let golden = json!([
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "What's the weather in Paris?"},
            {
                // WI-122: a tool-call-only assistant turn serializes with an
                // empty STRING, never `null` -- Ollama Cloud rejects a null
                // content type on tool-continuation requests.
                "role": "assistant",
                "content": "",
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

        let with_hint_body =
            build_request_body(&req_with_hint, &Dialect::OpenAi.profile(), true, false);
        let without_hint_body =
            build_request_body(&req_without_hint, &Dialect::OpenAi.profile(), true, false);
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
        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), false, false);
        assert_eq!(openai_body["max_completion_tokens"], 256);
        assert!(openai_body.get("max_tokens").is_none());

        let ollama_body = build_request_body(&req, &Dialect::Ollama.profile(), false, false);
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

        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), true, false);
        assert_eq!(openai_body["parallel_tool_calls"], true);

        let ollama_body = build_request_body(&req, &Dialect::Ollama.profile(), true, false);
        assert!(ollama_body.get("parallel_tool_calls").is_none());

        let openai_body_false_cap =
            build_request_body(&req, &Dialect::OpenAi.profile(), false, false);
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
        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), false, true);
        assert_eq!(openai_body["stream"], true);
        assert_eq!(openai_body["stream_options"]["include_usage"], true);

        let vllm_body = build_request_body(&req, &Dialect::VllmHermes.profile(), false, true);
        assert_eq!(vllm_body["stream"], true);
        assert!(vllm_body.get("stream_options").is_none());
    }

    #[test]
    fn reasoning_effort_is_emitted_only_for_openai_dialect_when_set() {
        let mut req = GenerateRequest {
            model: ModelId::new("m"),
            segments: vec![],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: None,
        };
        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), false, false);
        assert!(openai_body.get("reasoning_effort").is_none());

        req.params
            .extra
            .insert("reasoning_effort".into(), json!("high"));
        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), false, false);
        assert_eq!(openai_body["reasoning_effort"], "high");

        let ollama_body = build_request_body(&req, &Dialect::Ollama.profile(), false, false);
        assert!(ollama_body.get("reasoning_effort").is_none());
    }

    #[test]
    fn reasoning_content_is_parsed_into_a_thinking_block_without_a_signature() {
        let response: ChatCompletionResponse = serde_json::from_value(json!({
            "choices": [{
                "message": {
                    "content": "The answer is 4.",
                    "reasoning_content": "2 + 2 = 4"
                },
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
        let generated = to_generate_response(response, &Dialect::OpenAi.profile(), &[]).unwrap();
        assert_eq!(
            generated.content,
            vec![
                ContentBlock::Thinking {
                    text: "2 + 2 = 4".into(),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "The answer is 4.".into(),
                },
            ]
        );

        // The dialect's contract is the opposite of Anthropic's: reasoning
        // must not be resent, so it must not survive back into a request.
        let messages = segments_to_messages(
            &[PromptSegment::new(
                Role::Assistant,
                generated.content,
                Provenance::SystemNote {
                    reason: "turn".into(),
                },
            )],
            &Dialect::OpenAi.profile(),
        );
        assert_eq!(
            messages,
            vec![json!({"role": "assistant", "content": "The answer is 4."})]
        );
    }

    #[test]
    fn reasoning_content_alias_reasoning_is_accepted() {
        let response: ChatCompletionResponse = serde_json::from_value(json!({
            "choices": [{
                "message": {
                    "content": "ok",
                    "reasoning": "thinking via alias key"
                },
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
        let generated = to_generate_response(response, &Dialect::OpenAi.profile(), &[]).unwrap();
        assert_eq!(
            generated.content[0],
            ContentBlock::Thinking {
                text: "thinking via alias key".into(),
                signature: None,
            }
        );
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
            cached_tokens: None,
        }));
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 4);

        let usage_no_details = map_usage(Some(UsageWire {
            prompt_tokens: 1,
            completion_tokens: 1,
            prompt_tokens_details: None,
            cached_tokens: None,
        }));
        assert_eq!(usage_no_details.cache_read_tokens, 0);

        assert_eq!(map_usage(None), Usage::default());
    }

    /// The `cached_tokens` either-shape fix: OpenAI nests it under
    /// `usage.prompt_tokens_details.cached_tokens`; Kimi's Moonshot
    /// platform API reports it at the top level, `usage.cached_tokens`.
    /// `map_usage` must read either shape, and if a (hypothetical) server
    /// sends both, the nested (OpenAI-canonical) value wins.
    #[test]
    fn map_usage_reads_cached_tokens_from_either_shape_nested_or_top_level() {
        // Nested only (OpenAI's shape) — the pre-existing behavior, must
        // still work unchanged.
        let nested_only = map_usage(Some(UsageWire {
            prompt_tokens: 100,
            completion_tokens: 10,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(64),
            }),
            cached_tokens: None,
        }));
        assert_eq!(nested_only.cache_read_tokens, 64);

        // Top-level only (Kimi's shape).
        let top_level_only = map_usage(Some(UsageWire {
            prompt_tokens: 100,
            completion_tokens: 10,
            prompt_tokens_details: None,
            cached_tokens: Some(32),
        }));
        assert_eq!(top_level_only.cache_read_tokens, 32);

        // Neither shape present.
        let neither = map_usage(Some(UsageWire {
            prompt_tokens: 100,
            completion_tokens: 10,
            prompt_tokens_details: None,
            cached_tokens: None,
        }));
        assert_eq!(neither.cache_read_tokens, 0);

        // Both present (hypothetical server): nested wins.
        let both = map_usage(Some(UsageWire {
            prompt_tokens: 100,
            completion_tokens: 10,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(64),
            }),
            cached_tokens: Some(32),
        }));
        assert_eq!(both.cache_read_tokens, 64, "nested must win over top-level");
    }
}
