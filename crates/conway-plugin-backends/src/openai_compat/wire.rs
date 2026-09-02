//! Segment → OpenAI-compatible chat message mapping (`generate`/`stream`
//! request bodies) and chat-completion response → `GenerateResponse`
//! mapping (architecture §"Module: conway-backends").
//!
//! `PromptSegment.cache_hint` is never read anywhere in this module — that
//! omission, not a positive check, is what makes `CacheMode::ImplicitPrefix`
//! a wire no-op (§4.1): stripping every `cache_hint` from a request's
//! segments cannot change a single byte of the body this module produces.
//!
//! `Provenance::ToolRegistry` segments produce no chat message -- `conway-runtime`'s `ContextBuilder` stopped
//! putting the tool-schema JSON in that segment's `content` at all — the
//! native `tools` array below is the only copy. OpenAI-compatible dialects
//! have no `cache_control` equivalent to redirect a breakpoint to (unlike
//! `anthropic::wire`'s `BreakpointTarget::Tools`), so this is a pure size
//! reduction: one fewer `system` message, nothing else changes.

use conway_core::content::{CacheAccounting, ContentBlock, Role, StopReason, ToolSpec, Usage};
use conway_core::error::BackendError;
use conway_core::ports::{GenerateRequest, GenerateResponse};
use conway_core::provenance::Provenance;
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
///
/// `context_window` is the resolved context window conway intends to admit
/// this request against, `None` when
/// [`crate::capabilities::ContextTokensSource::Unverified`] (conway never
/// asks a server to arrange a window built on a number it did not actually
/// establish — board item context-window-declaration-honesty/num_ctx).
/// Emitted only when BOTH `context_window` is `Some` and
/// `profile.sends_num_ctx` — this is the OpenAI-compatible body's own
/// `options.num_ctx` field, which every dialect this crate confirmed
/// (2026-08-30, live Ollama 0.32.13) ignores over THIS endpoint; it is kept
/// here (never removed) as the honest, inert shape for a future profile
/// whose OpenAI-compatible surface DOES honour it, and — for `ollama`
/// specifically, `sends_num_ctx = true` — as the value
/// `openai_compat/ollama_native.rs`'s own native-endpoint body construction
/// reads instead of recomputing.
pub(crate) fn build_request_body(
    req: &GenerateRequest,
    profile: &Profile,
    parallel_tool_calls: bool,
    stream: bool,
    context_window: Option<u32>,
) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.as_str()));
    body.insert(
        "messages".into(),
        Value::Array(segments_to_messages(&req.segments, profile, false)),
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
    if profile.sends_num_ctx {
        if let Some(window) = context_window {
            body.insert("options".into(), json!({ "num_ctx": window }));
        }
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
/// OpenAI `reasoning_effort` chat-completion field, e.g. `"low"` /
/// `"medium"` / `"high"`.
///
/// `GenerateRequest` has no dedicated reasoning-effort field yet — that
/// caller-facing knob and its plumbing into `params.extra` is a
/// a separate concern, outside this module's scope; `extra` is the only
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
///
/// `pub(crate)`, not private: `openai_compat/ollama_native.rs` calls this
/// directly for `system`/`user`/`tool` messages (identical for both
/// endpoints) via `native = true`, the ONE branch point this shares with
/// [`assistant_message`] — see that function's own doc for why only the
/// assistant/tool-call shape actually differs between Ollama's native
/// `/api/chat` and its OpenAI-compatible `/v1/chat/completions`.
pub(crate) fn segments_to_messages(
    segments: &[PromptSegment],
    profile: &Profile,
    native: bool,
) -> Vec<Value> {
    segments
        .iter()
        .flat_map(|segment| segment_to_messages(segment, profile, native))
        .collect()
}

fn segment_to_messages(segment: &PromptSegment, profile: &Profile, native: bool) -> Vec<Value> {
    if matches!(segment.provenance, Provenance::ToolRegistry { .. }) {
        // No body content of its own anymore -- see this module's doc.
        return Vec::new();
    }
    match segment.role {
        Role::System => vec![system_message(&segment.content)],
        Role::User => vec![user_message(&segment.content, profile)],
        Role::Assistant => vec![assistant_message(&segment.content, native)],
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

/// `native`: Ollama's NATIVE `/api/chat` requires `function.arguments` as a
/// real JSON **object** in a replayed assistant tool-call message — a
/// stringified-JSON `arguments` (the OpenAI-canonical shape every other
/// call site here uses) is a loud `400` on that endpoint (confirmed
/// 2026-08-30: `"Value looks like object, but can't find closing '}'
/// symbol"`), not a tolerated quirk. This is the ONE place the native and
/// OpenAI-compatible message shapes actually differ — see
/// `openai_compat/ollama_native.rs`'s module doc for the rest of the
/// dialect split and why only this function needed a branch.
fn assistant_message(content: &[ContentBlock], native: bool) -> Value {
    let text = concat_text(content);
    let tool_calls: Vec<Value> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                call_id,
                name,
                arguments,
            } => {
                let arguments_value = if native {
                    arguments.clone()
                } else {
                    json!(serde_json::to_string(arguments).unwrap_or_default())
                };
                Some(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name.as_str(),
                        "arguments": arguments_value,
                    }
                }))
            }
            _ => None,
        })
        .collect();

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    // send an empty STRING (never `null`) for a tool-call-only
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
    /// its non-streaming counterpart. `reasoning` is accepted as
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
///
/// `cache_accounting` is `Reported` when EITHER shape's `cached_tokens`
/// `Option` is `Some` (present, even if the value itself is `0` — the
/// server said zero), `NotReported` when both are `None` (the wire carried
/// no cache field at all) or when the whole `usage` object is absent.
pub(crate) fn map_usage(usage: Option<UsageWire>) -> Usage {
    match usage {
        Some(usage) => {
            let nested = usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens);
            let cached = nested.or(usage.cached_tokens);
            Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cache_read_tokens: cached.unwrap_or(0),
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cache_accounting: if cached.is_some() {
                    CacheAccounting::Reported
                } else {
                    CacheAccounting::NotReported
                },
            }
        }
        None => Usage {
            cache_accounting: CacheAccounting::NotReported,
            ..Usage::default()
        },
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
        let messages = segments_to_messages(&segments, &Dialect::OpenAi.profile(), false);
        let golden = json!([
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "What's the weather in Paris?"},
            {
                // a tool-call-only assistant turn serializes with an
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

    /// A `Provenance::ToolRegistry`
    /// segment produces no chat message at all -- the native `tools` array
    /// (a separate `GenerateRequest` field, unrelated to `segments`) is the
    /// only copy of the schema text now.
    #[test]
    fn tool_registry_segment_produces_no_message() {
        let mut segments = fixture_segments();
        segments.insert(
            1,
            PromptSegment::new(
                Role::System,
                Vec::new(),
                Provenance::ToolRegistry {
                    hash: "deadbeef".into(),
                },
            ),
        );

        let messages = segments_to_messages(&segments, &Dialect::OpenAi.profile(), false);

        assert!(
            messages
                .iter()
                .all(|m| m != &json!({"role": "system", "content": ""})),
            "no spurious empty-content system message for the ToolRegistry segment: {messages:?}"
        );
        assert_eq!(
            messages.len(),
            4,
            "the ToolRegistry segment must not add a fifth message"
        );
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

        let with_hint_body = build_request_body(
            &req_with_hint,
            &Dialect::OpenAi.profile(),
            true,
            false,
            None,
        );
        let without_hint_body = build_request_body(
            &req_without_hint,
            &Dialect::OpenAi.profile(),
            true,
            false,
            None,
        );
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
        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), false, false, None);
        assert_eq!(openai_body["max_completion_tokens"], 256);
        assert!(openai_body.get("max_tokens").is_none());

        let ollama_body = build_request_body(&req, &Dialect::Ollama.profile(), false, false, None);
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

        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), true, false, None);
        assert_eq!(openai_body["parallel_tool_calls"], true);

        let ollama_body = build_request_body(&req, &Dialect::Ollama.profile(), true, false, None);
        assert!(ollama_body.get("parallel_tool_calls").is_none());

        let openai_body_false_cap =
            build_request_body(&req, &Dialect::OpenAi.profile(), false, false, None);
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
        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), false, true, None);
        assert_eq!(openai_body["stream"], true);
        assert_eq!(openai_body["stream_options"]["include_usage"], true);

        let vllm_body = build_request_body(&req, &Dialect::VllmHermes.profile(), false, true, None);
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
        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), false, false, None);
        assert!(openai_body.get("reasoning_effort").is_none());

        req.params
            .extra
            .insert("reasoning_effort".into(), json!("high"));
        let openai_body = build_request_body(&req, &Dialect::OpenAi.profile(), false, false, None);
        assert_eq!(openai_body["reasoning_effort"], "high");

        let ollama_body = build_request_body(&req, &Dialect::Ollama.profile(), false, false, None);
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
            false,
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

        assert_eq!(
            map_usage(None),
            Usage {
                cache_accounting: CacheAccounting::NotReported,
                ..Usage::default()
            }
        );
    }

    /// Declaration honesty: `cache_accounting` is `Reported` only when the
    /// wire actually carried a `cached_tokens` field (in EITHER shape),
    /// including a present-and-zero value, and `NotReported` when neither
    /// shape's `Option` is `Some` -- a server that never sends the field at
    /// all must not be indistinguishable from one that sent `0`.
    #[test]
    fn map_usage_marks_cache_accounting_from_field_presence_not_value() {
        let present_and_zero = map_usage(Some(UsageWire {
            prompt_tokens: 10,
            completion_tokens: 5,
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(0),
            }),
            cached_tokens: None,
        }));
        assert_eq!(present_and_zero.cache_read_tokens, 0);
        assert_eq!(present_and_zero.cache_accounting, CacheAccounting::Reported);

        let absent = map_usage(Some(UsageWire {
            prompt_tokens: 10,
            completion_tokens: 5,
            prompt_tokens_details: None,
            cached_tokens: None,
        }));
        assert_eq!(absent.cache_read_tokens, 0);
        assert_eq!(absent.cache_accounting, CacheAccounting::NotReported);

        let top_level_present = map_usage(Some(UsageWire {
            prompt_tokens: 10,
            completion_tokens: 5,
            prompt_tokens_details: None,
            cached_tokens: Some(3),
        }));
        assert_eq!(
            top_level_present.cache_accounting,
            CacheAccounting::Reported
        );

        assert_eq!(
            map_usage(None).cache_accounting,
            CacheAccounting::NotReported
        );
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

    // ---- `native` assistant-message argument shape (board item: ----
    // ---- context-window declaration honesty, num_ctx) ----

    /// Empirically confirmed 2026-08-30 against a live local Ollama
    /// 0.32.13: its NATIVE `/api/chat` rejects the OpenAI-canonical
    /// stringified `arguments` this module sends everywhere else, with a
    /// loud 400 (`"Value looks like object, but can't find closing '}'
    /// symbol"`) -- `native = true` must serialize `arguments` as the real
    /// JSON object, never a string.
    #[test]
    fn native_assistant_message_serializes_tool_call_arguments_as_a_json_object_not_a_string() {
        let segments = fixture_segments();
        let native_messages = segments_to_messages(&segments, &Dialect::Ollama.profile(), true);
        let assistant = native_messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("fixture has an assistant turn");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            json!({"city": "Paris"}),
            "native must send a real JSON object, not a stringified fragment"
        );

        // The OpenAI-compatible shape (`native = false`) is unchanged --
        // still a stringified fragment, exactly as every non-native dialect
        // requires.
        let compat_messages = segments_to_messages(&segments, &Dialect::Ollama.profile(), false);
        let assistant = compat_messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("fixture has an assistant turn");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            json!("{\"city\":\"Paris\"}")
        );
    }

    // ---- `options.num_ctx` (board item: context-window declaration ----
    // ---- honesty, num_ctx) ----

    fn minimal_request() -> GenerateRequest {
        GenerateRequest {
            model: ModelId::new("m"),
            segments: vec![],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: None,
        }
    }

    /// `sends_num_ctx = true` (the `ollama` profile) and a resolved
    /// `context_window`: `options.num_ctx` is emitted, and nothing else
    /// this dialect wouldn't otherwise send changes.
    #[test]
    fn ollama_emits_options_num_ctx_when_a_context_window_is_resolved() {
        let body = build_request_body(
            &minimal_request(),
            &Dialect::Ollama.profile(),
            false,
            false,
            Some(131_072),
        );
        assert_eq!(body["options"]["num_ctx"], 131_072);
    }

    /// `context_window: None` (an `Unverified` resolution, per this
    /// function's own doc): conway never asks a server to arrange a window
    /// built on a number it never established -- no `options` field at all,
    /// not even an empty one.
    #[test]
    fn no_options_field_is_emitted_when_context_window_is_unresolved() {
        let body = build_request_body(
            &minimal_request(),
            &Dialect::Ollama.profile(),
            false,
            false,
            None,
        );
        assert!(
            body.get("options").is_none(),
            "an Unverified/unresolved context window must never be sent as a request field"
        );
    }

    /// A dialect with `sends_num_ctx = false` (every built-in profile
    /// except `ollama`) never emits `options.num_ctx` even when a resolved
    /// `context_window` is passed in -- `profile.sends_num_ctx` gates it,
    /// not merely whether the caller happened to have a number.
    #[test]
    fn a_dialect_without_sends_num_ctx_never_emits_options_even_with_a_resolved_window() {
        for profile in [
            Dialect::OpenAi.profile(),
            Dialect::VllmHermes.profile(),
            Dialect::LmStudio.profile(),
            Dialect::LlamaCppServer.profile(),
        ] {
            let body =
                build_request_body(&minimal_request(), &profile, false, false, Some(131_072));
            assert!(
                body.get("options").is_none(),
                "{} must never emit options.num_ctx",
                profile.id
            );
        }
    }
}
