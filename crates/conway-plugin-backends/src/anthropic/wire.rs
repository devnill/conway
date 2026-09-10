//! Segment → Anthropic Messages API request mapping (`generate`/`stream`
//! request bodies) and Messages API response → `GenerateResponse` mapping
//! (architecture §"Module: conway-backends").
//!
//! `build_request_body` never reads `PromptSegment::cache_hint` — that
//! omission is what makes the byte-identity invariant hold (§4.1):
//! stripping every `cache_hint` from a request's segments cannot change a
//! single byte of the body this function produces. [`BreakpointTarget`] is a
//! side channel this module emits alongside the body so `cache.rs` can
//! attach `cache_control` as a strictly additive post-pass, without
//! re-deriving the segment→JSON mapping itself.
//!
//! `Provenance::ToolRegistry` segments never produce a `system` entry (board
//! item): `conway-runtime`'s `ContextBuilder`
//! stopped putting the tool-schema JSON in that segment's `content` at all
//! — the native `tools` array below is the only copy — so
//! `segments_to_body_parts` skips it entirely and records
//! [`BreakpointTarget::Tools`] instead of a `System`/`Message` placement.
//! See `cache.rs` for where that target actually attaches `cache_control`.

use conway_core::content::{CacheAccounting, ContentBlock, Role, StopReason, ToolSpec, Usage};
use conway_core::error::BackendError;
use conway_core::ports::{GenerateRequest, GenerateResponse};
use conway_core::provenance::Provenance;
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
    /// `body["tools"]`'s LAST entry — Anthropic's documented anchor for
    /// caching tool definitions ("Tool definitions can be cached by placing
    /// `cache_control` on the last tool in your `tools` array. All tools
    /// defined before and including that tool are cached as a single
    /// prefix.", Anthropic docs, "Prompt caching" > "Caching tool
    /// definitions", verified 2026-08-08). Recorded for the
    /// `Provenance::ToolRegistry` segment in place of a `System`/`Message`
    /// placement, since that segment no longer contributes any body content
    /// of its own. A cache hint that lands here is dropped (same as `None`)
    /// when `body["tools"]` is absent or empty.
    Tools,
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
    if let Some(budget_tokens) = reasoning_budget_tokens(req) {
        body.insert(
            "thinking".into(),
            json!({ "type": "enabled", "budget_tokens": budget_tokens }),
        );
    }
    warn_ignored_params(req);

    if stream {
        body.insert("stream".into(), json!(true));
    }

    (Value::Object(body), placements)
}

/// Reads a caller-supplied extended-thinking token budget out of
/// `params.extra["reasoning_budget_tokens"]` and serializes it as
/// Anthropic's `thinking: {type:"enabled", budget_tokens}`.
///
/// `GenerateRequest` has no dedicated reasoning-effort/budget field yet —
/// that caller-facing knob and its plumbing into `params.extra` is a
/// a separate concern (`SessionSpec`/runtime wiring), outside this
/// module's scope. `extra` is the only existing field on the request that
/// reaches this wire layer, so it is the wire contract this key targets.
fn reasoning_budget_tokens(req: &GenerateRequest) -> Option<u32> {
    req.params
        .extra
        .get("reasoning_budget_tokens")
        .and_then(Value::as_u64)
        .and_then(|tokens| u32::try_from(tokens).ok())
}

/// Warns, once per request, about a `SamplingParams` field this adapter's
/// wire body construction above never reads. A `roles.<alias>.params`
/// setting that reaches this far and then does nothing is the
/// declaration-honesty defect this project keeps paying for -- see
/// `conway::config::schema::RoleParams`'s own doc. `temperature`, `top_p`,
/// `max_tokens`, and `stop` are all read above (unconditionally, or via
/// `default_max_tokens` for `max_tokens`); `seed` is the one typed field
/// with no Anthropic Messages API equivalent, so it is the one checked
/// here. Extra keys are exempt by design (`RoleParams::extra`'s own doc):
/// `extra` is a free map for provider-specific keys precisely because no
/// fixed struct can enumerate them, so a key this adapter does not
/// recognize among `extra` is not, on its own, evidence of a
/// misconfiguration the way an ignored TYPED field is.
fn warn_ignored_params(req: &GenerateRequest) {
    if req.params.seed.is_some() {
        tracing::warn!(
            field = "seed",
            backend = "anthropic",
            "params field \"seed\" ignored by backend \"anthropic\": the Anthropic Messages \
             API has no seed parameter"
        );
    }
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
        // `Provenance::ToolRegistry` carries no body content of its own
        // anymore -- the native `tools` array below is the only copy (see
        // this module's doc) -- so it contributes no `system` entry, and
        // its placement points at `tools` instead of `system`.
        if matches!(segment.provenance, Provenance::ToolRegistry { .. }) {
            placements.push(BreakpointTarget::Tools);
            prev_role = Some(segment.role);
            continue;
        }
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
///
/// `redacted_thinking` round-trip: `ContentBlock` has no dedicated
/// redacted-thinking variant (out of this module's file scope to add one),
/// so `to_generate_response` below encodes a `redacted_thinking` response
/// block as `Thinking { text: "", signature: Some(data) }` — empty text is
/// not a shape Anthropic ever sends for a real (non-redacted) thinking
/// block, so it is an unambiguous sentinel. Re-emitting it here as
/// `{"type":"redacted_thinking","data":...}` (instead of `"thinking"`) is
/// what makes the round trip lossless: sending a redacted block back
/// tagged `"thinking"` would be rejected by the API.
fn assistant_content_blocks(content: &[ContentBlock]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for block in content {
        if let ContentBlock::Thinking {
            text,
            signature: Some(signature),
        } = block
        {
            if text.is_empty() {
                blocks.push(json!({ "type": "redacted_thinking", "data": signature }));
            } else {
                blocks
                    .push(json!({ "type": "thinking", "thinking": text, "signature": signature }));
            }
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
    /// Anthropic's contract for extended thinking under prompt redaction:
    /// `data` is an opaque, encrypted payload with no plaintext reasoning
    /// — it must be passed back verbatim on the next turn, never inspected
    ///. See `assistant_content_blocks` for the encoding used to
    /// carry it through `ContentBlock::Thinking` without a dedicated
    /// `ContentBlock` variant.
    RedactedThinking {
        data: String,
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
///
/// Unlike the OpenAI-compatible dialect, `UsageWire`'s two cache fields are
/// plain `u32`, not `Option<u32>`: Anthropic's Messages API contract always
/// includes `cache_read_input_tokens`/`cache_creation_input_tokens` in
/// `usage` (zeroed when caching was not used), never omits them, so field
/// PRESENCE carries no information here -- `#[serde(default)]` exists only
/// to tolerate a malformed/partial payload, not to encode "not reported".
/// `cache_accounting` is therefore `Reported` whenever a `usage` object was
/// present at all, `NotReported` only when the whole object is absent.
pub(crate) fn map_usage(usage: Option<UsageWire>) -> Usage {
    match usage {
        Some(usage) => Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
            reasoning_tokens: 0,
            cache_accounting: CacheAccounting::Reported,
        },
        None => Usage {
            cache_accounting: CacheAccounting::NotReported,
            ..Usage::default()
        },
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
            ResponseBlock::RedactedThinking { data } => {
                content.push(ContentBlock::Thinking {
                    text: String::new(),
                    signature: Some(data),
                });
            }
            ResponseBlock::ToolUse { id, name, input } => {
                accumulator.push_complete(Some(id), name, input)?;
            }
            ResponseBlock::Other => {}
        }
    }
    let outcome = accumulator.finish(stop)?;
    // Non-streaming path: `GenerateResponse` carries no channel for a
    // coercion record (unlike the streaming path's `StreamChunk::
    // ToolArgumentCoerced` -- that type's own doc), so this is `tracing`
    // only, a disclosed gap (board item `01M23SDCE6T85Z48CRQ8NBY6PV`).
    for coercion in &outcome.coercions {
        tracing::info!(
            tool = %coercion.tool,
            call_id = %coercion.call_id,
            argument_path = %coercion.argument_path,
            "coerced a stringified tool argument to its parsed JSON value \
             (non-streaming generate(): not yet surfaced as a durable Event)"
        );
    }

    Ok(GenerateResponse {
        content,
        tool_calls: outcome.calls,
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

    /// A `Provenance::ToolRegistry`
    /// segment contributes NO `system` entry -- the native `tools` array is
    /// the only copy of the schema text -- and its placement is
    /// `BreakpointTarget::Tools`, not `System`, even when (as `build`
    /// always produces) its `content` is empty.
    #[test]
    fn tool_registry_segment_produces_no_system_entry_and_targets_tools() {
        let segments = vec![
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
                Role::System,
                Vec::new(),
                Provenance::ToolRegistry {
                    hash: "deadbeef".into(),
                },
            ),
            PromptSegment::new(
                Role::User,
                vec![ContentBlock::Text { text: "hi".into() }],
                Provenance::UserPrompt,
            ),
        ];

        let (system, _messages, placements) = segments_to_body_parts(&segments);

        assert_eq!(
            Value::Array(system),
            json!([{"type": "text", "text": "You are a helpful assistant."}]),
            "the ToolRegistry segment must not add a second system entry"
        );
        assert_eq!(placements[1], BreakpointTarget::Tools);
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
    fn thinking_block_with_signature_survives_round_trip_through_assistant_content_blocks() {
        let content = vec![ContentBlock::Thinking {
            text: "step one, then step two".into(),
            signature: Some("sig-abc123".into()),
        }];
        let blocks = assistant_content_blocks(&content);
        assert_eq!(
            Value::Array(blocks),
            json!([{"type": "thinking", "thinking": "step one, then step two", "signature": "sig-abc123"}])
        );
    }

    #[test]
    fn thinking_block_without_signature_is_omitted_not_sent_unsigned() {
        let content = vec![ContentBlock::Thinking {
            text: "unsigned reasoning".into(),
            signature: None,
        }];
        assert!(assistant_content_blocks(&content).is_empty());
    }

    #[test]
    fn redacted_thinking_response_block_round_trips_through_assistant_content_blocks() {
        let response: MessagesResponse = serde_json::from_value(json!({
            "content": [{"type": "redacted_thinking", "data": "opaque-ciphertext"}],
            "stop_reason": "end_turn"
        }))
        .unwrap();
        let generated = to_generate_response(response, &[]).unwrap();
        assert_eq!(
            generated.content,
            vec![ContentBlock::Thinking {
                text: String::new(),
                signature: Some("opaque-ciphertext".into()),
            }]
        );

        // Re-sending it on the next (tool) turn must tag it
        // `redacted_thinking`, not `thinking` — the API rejects a redacted
        // payload sent back under the wrong tag.
        let blocks = assistant_content_blocks(&generated.content);
        assert_eq!(
            Value::Array(blocks),
            json!([{"type": "redacted_thinking", "data": "opaque-ciphertext"}])
        );
    }

    #[test]
    fn reasoning_budget_tokens_serializes_into_thinking_param_when_set() {
        let mut req = GenerateRequest {
            model: ModelId::new("claude-sonnet-4-6"),
            segments: vec![],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: None,
        };
        let (body, _) = build_request_body(&req, 8192, false);
        assert!(body.get("thinking").is_none());

        req.params
            .extra
            .insert("reasoning_budget_tokens".into(), json!(4096));
        let (body, _) = build_request_body(&req, 8192, false);
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 4096})
        );
    }

    // -----------------------------------------------------------------
    // `warn_ignored_params` -- minimal, dependency-free tracing WARN
    // capture. `tracing-test` is not a dependency of this crate; this
    // reuses the same hand-rolled `Subscriber` pattern
    // `conway-session/tests/recovery_tests.rs`/`index_tests.rs` already
    // established for the identical need.
    // -----------------------------------------------------------------

    #[derive(Clone, Default)]
    struct CaptureLog {
        entries: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl CaptureLog {
        fn contains(&self, needle: &str) -> bool {
            self.entries
                .lock()
                .unwrap()
                .iter()
                .any(|m| m.contains(needle))
        }
        fn count(&self) -> usize {
            self.entries.lock().unwrap().len()
        }
    }

    struct CaptureSubscriber {
        log: CaptureLog,
    }

    struct MessageVisitor(String);

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl tracing::Subscriber for CaptureSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = MessageVisitor(String::new());
            event.record(&mut visitor);
            self.log.entries.lock().unwrap().push(visitor.0);
        }
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    fn install_capture() -> (CaptureLog, tracing::subscriber::DefaultGuard) {
        let log = CaptureLog::default();
        let guard = tracing::subscriber::set_default(CaptureSubscriber { log: log.clone() });
        (log, guard)
    }

    fn seed_request(seed: Option<u64>) -> GenerateRequest {
        GenerateRequest {
            model: ModelId::new("claude-sonnet-4-6"),
            segments: vec![],
            tools: vec![],
            params: SamplingParams {
                seed,
                ..SamplingParams::default()
            },
            prefix_key: None,
        }
    }

    /// ACCEPTANCE: `params.seed` -- a typed `SamplingParams` field this
    /// adapter's wire body never reads -- logs exactly one WARN naming
    /// both the ignored field and this backend. Fires from body
    /// construction itself (once per `build_request_body` call), not from
    /// anywhere per-segment or per-chunk, so one request produces exactly
    /// one warning even though the request may carry many segments.
    #[test]
    fn seed_param_ignored_by_anthropic_logs_exactly_one_warning_naming_field_and_backend() {
        let (log, _guard) = install_capture();
        let req = seed_request(Some(42));
        build_request_body(&req, 8192, false);

        assert_eq!(
            log.count(),
            1,
            "exactly one warning must be logged for one ignored field"
        );
        assert!(log.contains("seed"), "warning must name the ignored field");
        assert!(log.contains("anthropic"), "warning must name the backend");
    }

    /// A request naming no `seed` logs no warning at all -- the check is
    /// conditional on the field actually being set, never unconditional
    /// noise on every request.
    #[test]
    fn no_seed_param_logs_no_warning() {
        let (log, _guard) = install_capture();
        let req = seed_request(None);
        build_request_body(&req, 8192, false);
        assert_eq!(log.count(), 0);
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
        assert_eq!(
            map_usage(None),
            Usage {
                cache_accounting: CacheAccounting::NotReported,
                ..Usage::default()
            }
        );
    }

    /// Anthropic's `usage` object always carries both cache fields
    /// (zeroed, never omitted) once caching went GA -- unlike the
    /// OpenAI-compatible dialect, field presence is not the honesty
    /// signal here. `cache_accounting` is `Reported` whenever `usage`
    /// itself was present, including an all-zero usage (no caching used
    /// this turn, but the wire still reported that fact); `NotReported`
    /// only when the whole `usage` object is missing from the response.
    #[test]
    fn map_usage_marks_cache_accounting_reported_whenever_usage_object_present() {
        let zeroed = map_usage(Some(UsageWire {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }));
        assert_eq!(zeroed.cache_read_tokens, 0);
        assert_eq!(zeroed.cache_accounting, CacheAccounting::Reported);

        assert_eq!(
            map_usage(None).cache_accounting,
            CacheAccounting::NotReported
        );
    }

    // -----------------------------------------------------------------
    // Byte stability (board item A5.7, acceptance criterion 4): the
    // precondition every prefix cache depends on. Churn at the FRONT of a
    // conversation -- a reordered field, a reformatted earlier message, an
    // extra segment -- breaks a provider's prefix match silently; conway's
    // own request construction must never be the source of that churn.
    // -----------------------------------------------------------------

    fn shared_conversation_prefix() -> Vec<PromptSegment> {
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
                    text: "what's 2+2?".into(),
                }],
                Provenance::UserPrompt,
            ),
            PromptSegment::new(
                Role::Assistant,
                vec![ContentBlock::Text { text: "4".into() }],
                Provenance::SystemNote {
                    reason: "turn".into(),
                },
            ),
        ]
    }

    /// **The load-bearing byte-stability check.** Two conversations sharing
    /// an identical prefix, differing ONLY in the LAST user turn, must
    /// render byte-identical `system`/`messages` content up to (not
    /// including) that turn -- proven at the level of the wire body
    /// `build_request_body` actually produces, not a proxy for it.
    #[test]
    fn two_conversations_differing_only_in_the_final_user_turn_render_byte_identical_up_to_that_turn(
    ) {
        let mut conversation_a = shared_conversation_prefix();
        conversation_a.push(PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "and 3+3?".into(),
            }],
            Provenance::UserPrompt,
        ));
        let mut conversation_b = shared_conversation_prefix();
        conversation_b.push(PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "what about 10-1?".into(),
            }],
            Provenance::UserPrompt,
        ));

        let (system_a, messages_a, _) = segments_to_body_parts(&conversation_a);
        let (system_b, messages_b, _) = segments_to_body_parts(&conversation_b);

        assert_eq!(
            system_a, system_b,
            "the system array must never depend on the final turn's content"
        );
        assert_eq!(
            messages_a.len(),
            messages_b.len(),
            "both conversations have the same NUMBER of turns -- only the last one's content \
             differs"
        );
        let (last_a, prefix_a) = messages_a.split_last().expect("at least one message");
        let (last_b, prefix_b) = messages_b.split_last().expect("at least one message");
        assert_eq!(
            prefix_a, prefix_b,
            "every message strictly before the final user turn must be byte-identical on the \
             wire -- this is the exact precondition prefix caching depends on: a=({prefix_a:?}) \
             b=({prefix_b:?})"
        );
        // Sanity: the two conversations actually differ where this test
        // claims they do, or the assertions above would pass vacuously.
        assert_ne!(
            last_a, last_b,
            "the two conversations must genuinely differ in their final turn"
        );
    }

    /// **Companion to the test above, proving it is discriminating rather
    /// than tautological** ("a check is not established until it has been
    /// shown to fail" -- board item A5.7's own verification anchor).
    /// Two conversations differing in the FIRST user turn (not the last)
    /// must NOT compare equal on the messages-strictly-before-the-final-turn
    /// prefix the sibling test above asserts on: churn at the FRONT of a
    /// conversation is exactly the failure mode that test exists to catch,
    /// and this proves the comparison technique actually catches it rather
    /// than passing regardless of what changed.
    #[test]
    fn the_prefix_comparison_actually_catches_front_churn_not_just_a_changed_last_turn() {
        let mut conversation_a = shared_conversation_prefix();
        conversation_a[1] = PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "what's 2+2? (variant A)".into(),
            }],
            Provenance::UserPrompt,
        );
        conversation_a.push(PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "and 3+3?".into(),
            }],
            Provenance::UserPrompt,
        ));

        let mut conversation_b = shared_conversation_prefix();
        conversation_b[1] = PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "what's 2+2? (variant B)".into(),
            }],
            Provenance::UserPrompt,
        );
        conversation_b.push(PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: "and 3+3?".into(),
            }],
            Provenance::UserPrompt,
        ));

        let (_system_a, messages_a, _) = segments_to_body_parts(&conversation_a);
        let (_system_b, messages_b, _) = segments_to_body_parts(&conversation_b);
        let (_last_a, prefix_a) = messages_a.split_last().expect("at least one message");
        let (_last_b, prefix_b) = messages_b.split_last().expect("at least one message");

        assert_ne!(
            prefix_a, prefix_b,
            "a change at the FRONT of the conversation (the first user turn, not the last) must \
             be visible in the shared-prefix comparison -- if this ever started passing, the \
             sibling byte-stability test above would no longer be proving anything"
        );
    }
}
