//! Ollama's NATIVE `/api/chat` endpoint — the dialect split board item
//! (context-window declaration honesty, num_ctx) required to actually
//! REQUEST a context window from Ollama, not merely assume one.
//!
//! # Why this module exists at all
//!
//! Every other quirk this crate handles for the `"ollama"` profile fits
//! inside the ordinary OpenAI-compatible request/response shape
//! `openai_compat/wire.rs` already builds. A resolved context window does
//! not. Empirically confirmed against a live local Ollama 0.32.13
//! (2026-08-30):
//!
//! - `POST {base}/v1/chat/completions` (the endpoint `wire.rs`/`stream.rs`
//!   use for every profile) **silently ignores** a passed `options` object
//!   — sent as `{"options":{"num_ctx":16384}}` or as a bare top-level
//!   `"num_ctx":16384`, either shape, the server answers `200` and `GET
//!   /api/ps` afterward reports the server's own unrequested default
//!   (observed: `32768`, exactly `default_max_context_tokens()`'s value —
//!   apparently coincidence, not a documented contract, so this crate does
//!   not rely on the two staying equal), never the value that was sent. A
//!   native pre-warm trick (loading the model natively with the desired
//!   `num_ctx` immediately before the OpenAI-compatible call) does not
//!   survive either: the OpenAI-compatible endpoint reloads the model to
//!   its own default regardless of what is already resident.
//! - `POST {base_origin}/api/chat` (Ollama's NATIVE endpoint, not versioned
//!   under the configured `base_url`, exactly like `/api/tags`/`/api/show`
//!   in `probe.rs`) **does** honour `options.num_ctx` — confirmed by `GET
//!   /api/ps` reporting exactly the requested figure (`8192`, `16384`,
//!   `131072` all round-tripped exactly) immediately after a request that
//!   set it.
//!
//! So the only way to make Ollama actually arrange the window conway
//! intends to admit against is to speak its native endpoint. That is a
//! **genuinely different wire format**, not a parameter this profile could
//! flip: different endpoint, different request options placement
//! (`options.num_ctx`/`options.num_predict`/`options.temperature`/... in
//! place of OpenAI's top-level `temperature`/`max_tokens`/...), different
//! non-streaming response envelope (`message` at the top level, not nested
//! under `choices[0]`; `done_reason` in place of `finish_reason`;
//! `prompt_eval_count`/`eval_count` in place of `usage.prompt_tokens`/
//! `completion_tokens`), different streaming framing (raw
//! newline-delimited JSON objects, one per generated increment, each with
//! a `done: bool` — never SSE `data: `-prefixed events, never a `[DONE]`
//! sentinel), and different assistant tool-call replay shape
//! (`function.arguments` must be a real JSON object; the OpenAI-canonical
//! stringified-JSON `arguments` `wire.rs` sends everywhere else is a loud
//! `400` here — confirmed empirically, see `wire::assistant_message`'s own
//! doc).
//!
//! # The cost of this split, stated plainly
//!
//! This is real, new, parallel production code most of this crate's other
//! quirks avoid needing: a second request-body builder, a second
//! non-streaming response mapper, and a second (NDJSON, not SSE) streaming
//! driver — none of it exercised by `wire.rs`'s/`stream.rs`'s own tests.
//! What keeps the blast radius small:
//!
//! - **Reused, not reimplemented, wherever the shapes genuinely agree**:
//!   `wire::segments_to_messages` (via its `native` parameter) builds every
//!   non-assistant message identically; `crate::tool_calls::
//!   ToolCallAccumulator` — including `tool_calls/ollama.rs`'s existing
//!   tolerant delta parser, which ALREADY accepted an object-valued
//!   `arguments` before this item, for unrelated ollama#12557 reasons —
//!   validates and accumulates tool calls from both endpoints unchanged.
//! - **Scoped to exactly the case that needs it.** `OpenAiCompatBackend`
//!   only routes through this module when BOTH `profile.id == "ollama"`
//!   AND a real context window was actually resolved to request (`Some`,
//!   not [`crate::capabilities::ContextTokensSource::Unverified`]) — see
//!   `openai_compat/mod.rs`. Every session that has not yet established a
//!   real window for its model (today, that is every session — the
//!   setup-time discover-or-ask flow this item also adds is what starts
//!   populating it) takes the ORIGINAL, unchanged OpenAI-compatible path,
//!   byte-for-byte identical to before this item. The new, less-exercised
//!   code only activates once conway has something worth asking for.
//! - **`"ollama"` only.** No other built-in profile sets
//!   [`crate::profile::Profile::sends_num_ctx`], so no other dialect's
//!   request path is touched at all.

use conway_core::content::{CacheAccounting, ContentBlock, StopReason, ToolSpec, Usage};
use conway_core::error::BackendError;
use conway_core::ports::{BoxStream, GenerateRequest, GenerateResponse, StreamChunk};
use futures_core::Stream;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::future::poll_fn;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;

use crate::profile::Profile;
use crate::tool_calls::ToolCallAccumulator;

use super::wire::segments_to_messages;

/// Builds the JSON request body for `POST {base_origin}/api/chat` — the
/// native counterpart of `wire::build_request_body`, restricted to what the
/// `"ollama"` profile ever actually sends (no `parallel_tool_calls` request
/// hint, no `reasoning_effort`, no `stream_options`: `Dialect::Ollama`'s own
/// profile already gates all three off).
///
/// Every generation parameter (`temperature`, `top_p`, `stop`, `seed`, and
/// `max_tokens`-as-`num_predict`) plus `context_window`-as-`num_ctx` is
/// folded into ONE native `options` object — unlike the OpenAI-compatible
/// shape, native Ollama has no top-level equivalents for any of them.
pub(crate) fn build_native_request_body(
    req: &GenerateRequest,
    profile: &Profile,
    stream: bool,
    context_window: Option<u32>,
) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model.as_str()));
    body.insert(
        "messages".into(),
        Value::Array(segments_to_messages(&req.segments, profile, true)),
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
    }

    let mut options = Map::new();
    if let Some(window) = context_window {
        options.insert("num_ctx".into(), json!(window));
    }
    if let Some(max_tokens) = req.params.max_tokens {
        options.insert("num_predict".into(), json!(max_tokens));
    }
    if let Some(temperature) = req.params.temperature {
        options.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = req.params.top_p {
        options.insert("top_p".into(), json!(top_p));
    }
    if !req.params.stop.is_empty() {
        options.insert("stop".into(), json!(req.params.stop));
    }
    if let Some(seed) = req.params.seed {
        options.insert("seed".into(), json!(seed));
    }
    if !options.is_empty() {
        body.insert("options".into(), Value::Object(options));
    }

    if stream {
        body.insert("stream".into(), json!(true));
    } else {
        body.insert("stream".into(), json!(false));
    }

    Value::Object(body)
}

// --- Non-streaming response mapping ------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct NativeChatResponse {
    pub(crate) message: NativeMessage,
    #[serde(default)]
    pub(crate) done_reason: Option<String>,
    #[serde(default)]
    pub(crate) prompt_eval_count: u32,
    #[serde(default)]
    pub(crate) eval_count: u32,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct NativeMessage {
    #[serde(default)]
    pub(crate) content: Option<String>,
    /// Reasoning-model native trace field — the streaming and
    /// non-streaming counterpart of `wire::ResponseMessage::
    /// reasoning_content`, named differently on this endpoint.
    #[serde(default)]
    pub(crate) thinking: Option<String>,
    #[serde(default)]
    pub(crate) tool_calls: Vec<NativeToolCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NativeToolCall {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) function: NativeFunctionCall,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NativeFunctionCall {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) arguments: Value,
}

/// `done_reason` → `StopReason`. Unlike the OpenAI-compatible shape, native
/// Ollama reports `"stop"` even for a turn that produced tool calls
/// (confirmed 2026-08-30) — `to_generate_response_native` therefore never
/// calls this when `tool_calls` is non-empty; see its own call site.
fn map_native_finish_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("length") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    }
}

/// Maps a complete (non-streamed) native `/api/chat` response to a
/// `GenerateResponse` — the native counterpart of
/// `wire::to_generate_response`, sharing the same
/// validate-through-`ToolCallAccumulator` path.
pub(crate) fn to_generate_response_native(
    response: NativeChatResponse,
    profile: &Profile,
    tools: &[ToolSpec],
) -> Result<GenerateResponse, BackendError> {
    let has_tool_calls = !response.message.tool_calls.is_empty();
    let stop = if has_tool_calls {
        StopReason::ToolUse
    } else {
        map_native_finish_reason(response.done_reason.as_deref())
    };

    let mut content = Vec::new();
    if let Some(thinking) = response.message.thinking.filter(|text| !text.is_empty()) {
        content.push(ContentBlock::Thinking {
            text: thinking,
            signature: None,
        });
    }
    if let Some(text) = response.message.content.filter(|text| !text.is_empty()) {
        content.push(ContentBlock::Text { text });
    }

    let mut accumulator = ToolCallAccumulator::new(profile.tool_call_style, tools);
    for tool_call in response.message.tool_calls {
        accumulator.push_complete(
            tool_call.id,
            tool_call.function.name,
            tool_call.function.arguments,
        )?;
    }
    let tool_calls = accumulator.finish(stop)?;

    Ok(GenerateResponse {
        content,
        tool_calls,
        stop,
        usage: Usage {
            input_tokens: response.prompt_eval_count,
            output_tokens: response.eval_count,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            // Ollama's native `/api/chat` response carries no cache field
            // at all -- see this module's own doc. `0` here is a
            // zero-filled placeholder, not an observation.
            cache_accounting: CacheAccounting::NotReported,
        },
    })
}

// --- Streaming (NDJSON, not SSE) ----------------------------------------

/// Sends the response body's newline-delimited-JSON stream into a spawned
/// driver task — the native counterpart of `stream::spawn`. Native Ollama
/// framing has no SSE envelope at all (no `data: ` prefix, no `[DONE]`
/// sentinel); each line is a complete JSON object, the last one carrying
/// `"done": true` plus the same stats `to_generate_response_native` reads.
pub(crate) fn spawn_native(
    response: reqwest::Response,
    profile: Profile,
    tools: Vec<ToolSpec>,
) -> BoxStream<'static, Result<StreamChunk, BackendError>> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(drive_native(response, profile, tools, tx));
    Box::pin(ChunkStream(rx))
}

struct ChunkStream(mpsc::UnboundedReceiver<Result<StreamChunk, BackendError>>);

impl Stream for ChunkStream {
    type Item = Result<StreamChunk, BackendError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// One driver task's running state, threaded through [`process_native_line`]
/// so the outer polling loop in [`drive_native`] and the per-line parser
/// below share exactly one mutable accumulation site each — never two
/// separately-updated copies of `usage`/`done_reason`/`saw_tool_calls`.
struct NativeDriverState {
    accumulator: ToolCallAccumulator,
    text_buffer: String,
    usage: Usage,
    done_reason: Option<String>,
    saw_tool_calls: bool,
}

/// Drives one native NDJSON response body to completion. `state.text_buffer`
/// accumulates every `message.content` fragment for the final `Done` chunk's
/// `GenerateResponse.content`, mirroring `stream::drive`'s own `text_buffer`
/// exactly. Every await races `tx.closed()`, same early-drop contract as
/// `stream::drive`.
///
/// Native Ollama framing has no event boundary of its own beyond `\n` — a
/// single `bytes_stream()` poll can (and, for a real chunked-transfer body,
/// often does) deliver more than one complete line, or less than one; `buf`
/// carries any trailing partial line across polls, the same way a plain
/// `BufRead::lines()` would over a socket.
async fn drive_native(
    response: reqwest::Response,
    profile: Profile,
    tools: Vec<ToolSpec>,
    tx: mpsc::UnboundedSender<Result<StreamChunk, BackendError>>,
) {
    let mut bytes = Box::pin(response.bytes_stream());
    let mut buf: Vec<u8> = Vec::new();
    let mut state = NativeDriverState {
        accumulator: ToolCallAccumulator::new(profile.tool_call_style, &tools),
        text_buffer: String::new(),
        usage: Usage::default(),
        done_reason: None,
        saw_tool_calls: false,
    };

    loop {
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len().saturating_sub(1)])
                .into_owned();
            if line.trim().is_empty() {
                continue;
            }
            if !process_native_line(&line, &tx, &mut state) {
                return;
            }
        }

        let next = tokio::select! {
            biased;
            () = tx.closed() => return,
            next = poll_fn(|cx| bytes.as_mut().poll_next(cx)) => next,
        };
        match next {
            Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
            Some(Err(err)) => {
                let _ = tx.send(Err(BackendError::Transport {
                    detail: err.to_string(),
                }));
                return;
            }
            None => break,
        }
    }

    // EOF: one final line with no trailing `\n` is still real data (native
    // Ollama does terminate every line with `\n` in practice, but a body
    // ending mid-line must not be silently dropped).
    if buf.iter().any(|b| *b != b'\n') {
        let line = String::from_utf8_lossy(&buf).into_owned();
        if !line.trim().is_empty() && !process_native_line(&line, &tx, &mut state) {
            return;
        }
    }

    let stop = if state.saw_tool_calls {
        StopReason::ToolUse
    } else {
        map_native_finish_reason(state.done_reason.as_deref())
    };
    match state.accumulator.finish(stop) {
        Ok(tool_calls) => {
            let mut content = Vec::new();
            if !state.text_buffer.is_empty() {
                content.push(ContentBlock::Text {
                    text: state.text_buffer,
                });
            }
            let _ = tx.send(Ok(StreamChunk::Done(GenerateResponse {
                content,
                tool_calls,
                stop,
                usage: state.usage,
            })));
        }
        Err(err) => {
            let _ = tx.send(Err(err));
        }
    }
}

/// Parses and applies one complete native NDJSON line, mutating `state` and
/// sending the corresponding `StreamChunk`s. Returns `false` when the
/// caller must stop entirely (the receiver was dropped mid-send, or the
/// line was unparseable) — mirrors `stream.rs::drive`'s own
/// early-return-on-closed-channel contract. A malformed line from an
/// otherwise-`200` native stream is surfaced as a transport error, never
/// silently skipped: unlike an SSE keep-alive comment (`stream.rs`'s own
/// tolerated case), native framing has no non-JSON line shape at all, so
/// this always indicates a real parse failure worth reporting.
fn process_native_line(
    line: &str,
    tx: &mpsc::UnboundedSender<Result<StreamChunk, BackendError>>,
    state: &mut NativeDriverState,
) -> bool {
    let chunk: NativeChatResponse = match serde_json::from_str(line) {
        Ok(chunk) => chunk,
        Err(err) => {
            let _ = tx.send(Err(BackendError::Transport {
                detail: format!("malformed native ollama stream line: {err}"),
            }));
            return false;
        }
    };

    if let Some(thinking) = chunk
        .message
        .thinking
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        if tx
            .send(Ok(StreamChunk::ThinkingDelta(thinking.to_string())))
            .is_err()
        {
            return false;
        }
    }
    if let Some(text) = chunk
        .message
        .content
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        state.text_buffer.push_str(text);
        if tx
            .send(Ok(StreamChunk::TextDelta(text.to_string())))
            .is_err()
        {
            return false;
        }
    }
    for (position, tool_call) in chunk.message.tool_calls.iter().enumerate() {
        state.saw_tool_calls = true;
        let raw = json!({
            "id": tool_call.id,
            "function": {
                "name": tool_call.function.name,
                "arguments": tool_call.function.arguments,
            }
        })
        .to_string();
        if let Err(err) = state.accumulator.push_delta(&raw) {
            let _ = tx.send(Err(err));
            return false;
        }
        if tx
            .send(Ok(StreamChunk::ToolCallDelta {
                index: position as u32,
                raw,
            }))
            .is_err()
        {
            return false;
        }
    }

    state.usage = Usage {
        input_tokens: chunk.prompt_eval_count,
        output_tokens: chunk.eval_count,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: 0,
        // Same rationale as the non-streaming path above: native Ollama's
        // NDJSON frames never carry a cache field.
        cache_accounting: CacheAccounting::NotReported,
    };
    state.done_reason = chunk.done_reason.or_else(|| state.done_reason.clone());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::content::SamplingParams;
    use conway_core::ids::ModelId;

    use crate::config::Dialect;

    fn minimal_request() -> GenerateRequest {
        GenerateRequest {
            model: ModelId::new("gemma4:e4b"),
            segments: vec![],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: None,
        }
    }

    #[test]
    fn build_native_request_body_sends_options_num_ctx_when_resolved() {
        let body = build_native_request_body(
            &minimal_request(),
            &Dialect::Ollama.profile(),
            false,
            Some(131_072),
        );
        assert_eq!(body["options"]["num_ctx"], 131_072);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn build_native_request_body_omits_options_entirely_when_nothing_is_set() {
        let body =
            build_native_request_body(&minimal_request(), &Dialect::Ollama.profile(), false, None);
        assert!(body.get("options").is_none());
    }

    #[test]
    fn build_native_request_body_folds_max_tokens_into_num_predict() {
        let req = GenerateRequest {
            params: SamplingParams {
                max_tokens: Some(256),
                ..SamplingParams::default()
            },
            ..minimal_request()
        };
        let body = build_native_request_body(&req, &Dialect::Ollama.profile(), false, None);
        assert_eq!(body["options"]["num_predict"], 256);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn to_generate_response_native_maps_prompt_and_eval_counts_to_usage() {
        let response: NativeChatResponse = serde_json::from_value(json!({
            "message": {"role": "assistant", "content": "hi"},
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 18,
            "eval_count": 4
        }))
        .unwrap();
        let generated =
            to_generate_response_native(response, &Dialect::Ollama.profile(), &[]).unwrap();
        assert_eq!(generated.usage.input_tokens, 18);
        assert_eq!(generated.usage.output_tokens, 4);
        assert_eq!(generated.stop, StopReason::EndTurn);
        assert_eq!(
            generated.content,
            vec![ContentBlock::Text { text: "hi".into() }]
        );
    }

    /// Ollama's native `/api/chat` response carries no cache field at all
    /// (see this module's own doc). The non-streaming decode path must
    /// mark `cache_accounting` `NotReported`, not silently claim the
    /// zero-filled `cache_read_tokens`/`cache_write_tokens` are real
    /// observations.
    #[test]
    fn to_generate_response_native_marks_cache_accounting_not_reported() {
        let response: NativeChatResponse = serde_json::from_value(json!({
            "message": {"role": "assistant", "content": "hi"},
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 18,
            "eval_count": 4
        }))
        .unwrap();
        let generated =
            to_generate_response_native(response, &Dialect::Ollama.profile(), &[]).unwrap();
        assert_eq!(generated.usage.cache_accounting, CacheAccounting::NotReported);
    }

    /// Same rationale as the non-streaming test above, for the NDJSON
    /// streaming path: `process_native_line` must mark `cache_accounting`
    /// `NotReported` on every line that carries usage stats.
    #[test]
    fn process_native_line_marks_cache_accounting_not_reported() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut state = NativeDriverState {
            accumulator: ToolCallAccumulator::new(
                Dialect::Ollama.profile().tool_call_style,
                &[],
            ),
            text_buffer: String::new(),
            usage: Usage::default(),
            done_reason: None,
            saw_tool_calls: false,
        };
        let line = json!({
            "message": {"role": "assistant", "content": "hi"},
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 18,
            "eval_count": 4
        })
        .to_string();
        assert!(process_native_line(&line, &tx, &mut state));
        assert_eq!(state.usage.cache_accounting, CacheAccounting::NotReported);
    }

    fn get_weather_tool() -> ToolSpec {
        ToolSpec {
            name: conway_core::ids::ToolName::new("get_weather"),
            description: "get weather".into(),
            schema: serde_json::from_value(json!({"type": "object"})).unwrap(),
            category: conway_core::content::ToolCategory::Read,
            permission: conway_core::content::PermissionClass::Safe,
        }
    }

    /// Native reports `done_reason: "stop"` even when tool calls are
    /// present (confirmed empirically 2026-08-30) -- the response mapper
    /// must not trust that field once `tool_calls` is non-empty.
    #[test]
    fn to_generate_response_native_reports_tool_use_when_tool_calls_are_present_despite_stop_reason(
    ) {
        let response: NativeChatResponse = serde_json::from_value(json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "get_weather", "arguments": {"city": "Paris"}}
                }]
            },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 10,
            "eval_count": 5
        }))
        .unwrap();
        let tools = [get_weather_tool()];
        let generated =
            to_generate_response_native(response, &Dialect::Ollama.profile(), &tools).unwrap();
        assert_eq!(generated.stop, StopReason::ToolUse);
        assert_eq!(generated.tool_calls.len(), 1);
        assert_eq!(generated.tool_calls[0].arguments, json!({"city": "Paris"}));
    }
}
