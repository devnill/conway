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

use super::wire::{reasoning_effort_candidate, segments_to_messages};

/// Builds the JSON request body for `POST {base_origin}/api/chat` — the
/// native counterpart of `wire::build_request_body`, restricted to what the
/// `"ollama"` profile ever actually sends (no `parallel_tool_calls` request
/// hint, no `stream_options`: `Dialect::Ollama`'s own profile already gates
/// both off).
///
/// `think` -- Ollama's native reasoning-level field, e.g. `"low"` /
/// `"medium"` / a model-specific level string, OR a plain JSON `true`/
/// `false` for a model whose own `/api/show` `"thinking"` capability is
/// boolean-only (confirmed live, 2026-09-30: `gemma4:e4b`'s own
/// `"thinking"` capability is `{"values":[false,true],"default":true}`,
/// never a level string) -- is read from the SAME
/// `params.extra["reasoning_effort"]` key `wire::reasoning_effort` reads
/// for the OpenAI-compatible endpoint (see [`reasoning_effort_candidate`]'s
/// own doc): one caller-facing setting, two wire names, because
/// `OpenAiCompatBackend` picks this endpoint or the other per request
/// depending on whether a context window has been resolved yet
/// (`openai_compat/mod.rs::use_native_ollama_chat`), not on anything the
/// caller controls. A configured JSON boolean is accepted HERE but not by
/// `wire::reasoning_effort`'s own OpenAI-compatible `reasoning_effort`
/// field: that field is confirmed live to be strictly typed as a STRING on
/// Ollama's OpenAI-compatible endpoint (a boolean there answers a
/// structured `400` naming the field), so a boolean is forwarded only on
/// this native endpoint, which genuinely accepts one.
///
/// VERIFIED 2026-09-30 against a live local Ollama 0.35.0: `POST
/// /api/chat` with a top-level `"think"` string produces a
/// `message.thinking` field in the response, and an unrecognized level
/// string for the loaded model does not 400 (falls back to the model's
/// default), so this sends the caller's string/bool through verbatim, with
/// no per-model level-validation table. A model DECLARED (`model_reasoning
/// == Some(false)`) not to support thinking at all is a different failure
/// mode entirely -- confirmed live the same day to answer a loud `400`
/// (`"<model>" does not support thinking"`) regardless of value type or
/// shape, so [`reasoning_effort_candidate`]'s own per-model gate applies
/// here identically to the OpenAI-compatible path; see that function's doc
/// for the full three-way (`Some(true)`/`Some(false)`/`None`) policy.
///
/// Every generation parameter (`temperature`, `top_p`, `stop`, `seed`, and
/// `max_tokens`-as-`num_predict`) plus `context_window`-as-`num_ctx` is
/// folded into ONE native `options` object — unlike the OpenAI-compatible
/// shape, native Ollama has no top-level equivalents for any of them.
/// `think` is NOT one of them: it is Ollama's own top-level request field,
/// not an `options` entry (confirmed empirically the same day).
pub(crate) fn build_native_request_body(
    req: &GenerateRequest,
    profile: &Profile,
    stream: bool,
    context_window: Option<u32>,
    model_reasoning: Option<bool>,
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

    let think_value = reasoning_effort_native(req, profile, model_reasoning);
    if let Some(think) = &think_value {
        body.insert("think".into(), think.clone());
    }
    warn_ignored_extra(req, profile, model_reasoning, think_value.is_some());

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

/// Ollama's native `think` field accepts a JSON `String` OR `Bool`
/// (confirmed live, 2026-09-30: see [`build_native_request_body`]'s own
/// doc) -- unlike `wire::reasoning_effort`, which extracts a `String` only.
/// Shares [`reasoning_effort_candidate`]'s per-profile/per-model gate
/// exactly; only the accepted JSON TYPE differs between the two endpoints.
fn reasoning_effort_native(
    req: &GenerateRequest,
    profile: &Profile,
    model_reasoning: Option<bool>,
) -> Option<Value> {
    match reasoning_effort_candidate(req, profile, model_reasoning)? {
        value @ (Value::String(_) | Value::Bool(_)) => Some(value.clone()),
        _ => None,
    }
}

/// Warns about an `extra` key this NATIVE body builder has no mapping for --
/// the native counterpart of `wire::warn_ignored_params`, scoped
/// differently in one deliberate way: `seed` is never checked here, because
/// (unlike the OpenAI-compatible path) THIS builder's own `options.seed`
/// above DOES send it -- the opposite of `wire.rs`'s own doc on that point.
///
/// `reasoning_effort_sent` is whether [`reasoning_effort_native`] put a
/// `think` value in the body for THIS request -- `false` covers "this
/// profile does not send it", "the configured value was not a string or
/// boolean", and "this exact model is DECLARED (`model_reasoning ==
/// Some(false)`) not to support it" -- the last of which gets its own
/// message naming the model, same split `wire::warn_ignored_params` makes
/// for its own `reasoning_effort` check and for the identical reason (board
/// item dogfood-02/reasoning-effort-crashes-a-non-thinking-model).
fn warn_ignored_extra(
    req: &GenerateRequest,
    profile: &Profile,
    model_reasoning: Option<bool>,
    reasoning_effort_sent: bool,
) {
    if req.params.extra.contains_key("reasoning_effort") && !reasoning_effort_sent {
        if model_reasoning == Some(false) {
            let model = req.model.as_str();
            tracing::warn!(
                field = "reasoning_effort",
                backend = "openai-compat",
                model,
                endpoint = "ollama-native",
                "params.extra field \"reasoning_effort\" ignored by backend \"openai-compat\" \
                 (Ollama native endpoint): model \"{model}\" is declared not to support \
                 reasoning/thinking"
            );
        } else {
            let profile_id = profile.id.as_str();
            tracing::warn!(
                field = "reasoning_effort",
                backend = "openai-compat",
                profile = profile_id,
                endpoint = "ollama-native",
                "params.extra field \"reasoning_effort\" ignored by backend \"openai-compat\" \
                 (profile \"{profile_id}\", Ollama native endpoint): this profile does not send \
                 a think field, or the configured value is not a string or boolean"
            );
        }
    }
    for key in req.params.extra.keys() {
        if key == "reasoning_effort" {
            continue; // handled above, sent or explained
        }
        tracing::warn!(
            field = %key,
            backend = "openai-compat",
            endpoint = "ollama-native",
            "params.extra field \"{key}\" ignored by backend \"openai-compat\" (Ollama native \
             endpoint): no wire mapping reads this key, so the provider never receives it"
        );
    }
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
        Ok(outcome) => {
            // Board item `01M23SDCE6T85Z48CRQ8NBY6PV`: surface each
            // coercion durably BEFORE the terminal `Done` chunk, so a
            // subscriber sees it as real content arriving, exactly like
            // `Event::StreamRestarted`'s own precedent.
            for coercion in &outcome.coercions {
                if tx
                    .send(Ok(StreamChunk::ToolArgumentCoerced {
                        tool: coercion.tool.clone(),
                        call_id: coercion.call_id.clone(),
                        argument_path: coercion.argument_path.clone(),
                    }))
                    .is_err()
                {
                    return;
                }
            }
            let mut content = Vec::new();
            if !state.text_buffer.is_empty() {
                content.push(ContentBlock::Text {
                    text: state.text_buffer,
                });
            }
            let _ = tx.send(Ok(StreamChunk::Done(GenerateResponse {
                content,
                tool_calls: outcome.calls,
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
            None,
        );
        assert_eq!(body["options"]["num_ctx"], 131_072);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn build_native_request_body_omits_options_entirely_when_nothing_is_set() {
        let body = build_native_request_body(
            &minimal_request(),
            &Dialect::Ollama.profile(),
            false,
            None,
            None,
        );
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
        let body = build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, None);
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
        assert_eq!(
            generated.usage.cache_accounting,
            CacheAccounting::NotReported
        );
    }

    /// Same rationale as the non-streaming test above, for the NDJSON
    /// streaming path: `process_native_line` must mark `cache_accounting`
    /// `NotReported` on every line that carries usage stats.
    #[test]
    fn process_native_line_marks_cache_accounting_not_reported() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut state = NativeDriverState {
            accumulator: ToolCallAccumulator::new(Dialect::Ollama.profile().tool_call_style, &[]),
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

    // -----------------------------------------------------------------
    // `think` (board item dogfood-02/thinking-role silently dropped) --
    // same minimal, dependency-free tracing WARN capture as `wire.rs`'s/
    // `anthropic::wire`'s own tests.
    // -----------------------------------------------------------------

    fn extra_request(extra: serde_json::Map<String, Value>) -> GenerateRequest {
        GenerateRequest {
            params: SamplingParams {
                extra,
                ..SamplingParams::default()
            },
            ..minimal_request()
        }
    }

    /// VERIFIED 2026-09-30 against a live local Ollama 0.35.0: native
    /// `POST /api/chat` with a top-level `"think"` string field produces a
    /// `message.thinking` response field -- this is the body-level proof
    /// that `reasoning_effort` reaches the Ollama dialect's native endpoint
    /// too, under its own wire name.
    #[test]
    fn build_native_request_body_sends_think_when_reasoning_effort_is_set() {
        let mut extra = serde_json::Map::new();
        extra.insert("reasoning_effort".into(), json!("low"));
        let req = extra_request(extra);

        let body = build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, None);
        assert_eq!(body["think"], "low");
        // `think` is a top-level field, never folded into `options`.
        assert!(body.get("options").is_none());
    }

    /// No `extra.reasoning_effort` set: no `think` field at all, not even
    /// an empty/default one.
    #[test]
    fn build_native_request_body_omits_think_when_reasoning_effort_is_unset() {
        let body = build_native_request_body(
            &minimal_request(),
            &Dialect::Ollama.profile(),
            false,
            None,
            None,
        );
        assert!(body.get("think").is_none());
    }

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

    /// A dropped `extra.reasoning_effort` on a profile that does not send
    /// it (every built-in profile this native path is NOT scoped to today,
    /// since only `"ollama"` ever reaches this builder -- exercised here
    /// directly, independent of that routing restriction) warns exactly
    /// like `wire::warn_ignored_params`'s identical case.
    #[test]
    fn reasoning_effort_dropped_by_a_profile_that_does_not_send_think_logs_a_warning() {
        let (log, _guard) = install_capture();
        let mut extra = serde_json::Map::new();
        extra.insert("reasoning_effort".into(), json!("high"));
        let req = extra_request(extra);

        build_native_request_body(&req, &Dialect::VllmHermes.profile(), false, None, None);

        assert_eq!(log.count(), 1);
        assert!(log.contains("reasoning_effort"));
        assert!(log.contains("openai-compat"));
    }

    /// The same key, same value, against `ollama` (which DOES send it as
    /// `think`): no warning, because nothing was dropped.
    #[test]
    fn reasoning_effort_sent_as_think_logs_no_warning() {
        let (log, _guard) = install_capture();
        let mut extra = serde_json::Map::new();
        extra.insert("reasoning_effort".into(), json!("high"));
        let req = extra_request(extra);

        let body = build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, None);

        assert_eq!(body["think"], "high");
        assert_eq!(log.count(), 0);
    }

    // -----------------------------------------------------------------
    // Per-MODEL gating (board item dogfood-02/reasoning-effort-crashes-
    // a-non-thinking-model) -- the native `think` counterpart of
    // `wire.rs`'s own tests of the same name; see that module for the full
    // live-evidence citation and the "unknown -> send" policy rationale.
    // -----------------------------------------------------------------

    fn extra_request_for_model(
        model: &str,
        extra: serde_json::Map<String, Value>,
    ) -> GenerateRequest {
        GenerateRequest {
            model: ModelId::new(model),
            ..extra_request(extra)
        }
    }

    /// A model DECLARED reasoning-capable: `think` is sent.
    #[test]
    fn think_is_sent_when_the_model_is_declared_reasoning_capable() {
        let mut extra = serde_json::Map::new();
        extra.insert("reasoning_effort".into(), json!("high"));
        let req = extra_request_for_model("qwen3.8:27b-mlx", extra);

        let body =
            build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, Some(true));

        assert_eq!(body["think"], "high");
    }

    /// **ACCEPTANCE (the critical fix)**: a model DECLARED NOT
    /// reasoning-capable never gets `think` at all, and the dropped setting
    /// is warned about by name -- confirmed live, 2026-09-30: `POST
    /// /api/chat {"model":"ministral-3:8b",...,"think":"high"}` answers a
    /// structured `400`, `"\"ministral-3:8b\" does not support thinking"`.
    #[test]
    fn think_is_skipped_and_warned_when_the_model_is_declared_not_reasoning_capable() {
        let (log, _guard) = install_capture();
        let mut extra = serde_json::Map::new();
        extra.insert("reasoning_effort".into(), json!("high"));
        let req = extra_request_for_model("ministral-3:8b", extra);

        let body =
            build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, Some(false));

        assert!(
            body.get("think").is_none(),
            "a model declared not to support reasoning must never receive think: {body}"
        );
        assert_eq!(log.count(), 1);
        assert!(
            log.contains("ministral-3:8b"),
            "the warning must name the model, not just the field/backend"
        );
        assert!(log.contains("reasoning_effort"));
        assert!(log.contains("openai-compat"));
    }

    /// The "unknown" policy: no declaration at all (`None`) still sends
    /// `think`, exactly like `Some(true)`.
    #[test]
    fn think_is_sent_when_the_model_has_no_reasoning_declaration_at_all() {
        let mut extra = serde_json::Map::new();
        extra.insert("reasoning_effort".into(), json!("high"));
        let req = extra_request_for_model("qwen3.8:27b-mlx", extra);

        let body = build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, None);

        assert_eq!(body["think"], "high");
    }

    /// A configured JSON BOOLEAN is forwarded on the native `think` field
    /// (the significant finding this review round raised: `gemma4:e4b`'s
    /// own `/api/show` reports a boolean-only `"thinking"` capability,
    /// `{"values":[false,true]}`, never a level string) -- unlike
    /// `wire::reasoning_effort`'s OpenAI-compatible `reasoning_effort`
    /// field, which is string-only (see that function's own doc for the
    /// live 400 that makes a bool there unsafe to send).
    #[test]
    fn a_configured_boolean_is_forwarded_to_the_native_think_field() {
        let mut extra = serde_json::Map::new();
        extra.insert("reasoning_effort".into(), json!(true));
        let req = extra_request_for_model("gemma4:e4b", extra);

        let body = build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, None);

        assert_eq!(body["think"], true);
    }

    /// Any other `extra` key this native builder has no mapping for at all
    /// is warned about too, naming the field and the backend.
    #[test]
    fn an_unrecognized_extra_key_logs_a_warning_naming_the_field_and_backend() {
        let (log, _guard) = install_capture();
        let mut extra = serde_json::Map::new();
        extra.insert("some_unrecognized_key".into(), json!("value"));
        let req = extra_request(extra);

        build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, None);

        assert_eq!(log.count(), 1);
        assert!(log.contains("some_unrecognized_key"));
        assert!(log.contains("openai-compat"));
    }

    /// `seed` is never warned about here -- unlike `wire.rs`'s own path,
    /// THIS builder's `options.seed` above sends it for real.
    #[test]
    fn seed_param_logs_no_warning_here_because_native_actually_sends_it() {
        let (log, _guard) = install_capture();
        let req = GenerateRequest {
            params: SamplingParams {
                seed: Some(7),
                ..SamplingParams::default()
            },
            ..minimal_request()
        };

        let body = build_native_request_body(&req, &Dialect::Ollama.profile(), false, None, None);

        assert_eq!(body["options"]["seed"], 7);
        assert_eq!(log.count(), 0);
    }
}
