//! SSE streaming for `AnthropicBackend::stream`: drains the
//! `eventsource_stream::EventStream` on a spawned task, translating each
//! Anthropic `content_block_*`/`message_*` event into `StreamChunk`s and
//! feeding tool-call deltas to a `ToolCallAccumulator` — the same
//! accumulator construction `wire.rs`'s non-streaming path uses.
//!
//! Anthropic's streaming tool-call shape differs from OpenAI's: `id`+`name`
//! arrive once, in `content_block_start`, and `input` arrives as
//! `input_json_delta` fragments in subsequent `content_block_delta` events,
//! keyed by the block's top-level `index` rather than embedded per-delta.
//! This module synthesizes the `{"index":..,"id":..,"function":{"name":..,
//! "arguments":..}}` shape `ToolCallStyle::Structured`'s parser expects (`synth_*`
//! below), so the shared accumulator needs no Anthropic-specific parser and
//! `src/tool_calls/*` (owned by an earlier item/ an earlier item) is untouched.
//!
//! `spawn` is only ever called with an already-`200`-classified
//! `reqwest::Response` (see `AnthropicBackend::stream`): a mid-body
//! transport error or a truncated tool-call argument buffer at
//! `message_stop` is surfaced as a stream item and never retried (module
//! boundary rule).

use std::future::poll_fn;
use std::pin::Pin;
use std::task::{Context, Poll};

use conway_core::content::{ContentBlock, StopReason, ToolSpec, Usage};
use conway_core::error::BackendError;
use conway_core::ports::{BoxStream, GenerateResponse, StreamChunk};
use eventsource_stream::Eventsource;
use futures_core::Stream;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::new_tool_call_accumulator;
use super::wire::{map_stop_reason, map_usage, UsageWire};

/// Sends the response body's SSE stream into a spawned driver task and
/// returns a `Stream` reading the task's output.
pub(crate) fn spawn(
    response: reqwest::Response,
    tools: Vec<ToolSpec>,
) -> BoxStream<'static, Result<StreamChunk, BackendError>> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(drive(response, tools, tx));
    Box::pin(ChunkStream(rx))
}

struct ChunkStream(mpsc::UnboundedReceiver<Result<StreamChunk, BackendError>>);

impl Stream for ChunkStream {
    type Item = Result<StreamChunk, BackendError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// Synthesizes the delta shape `ToolCallStyle::Structured`'s parser expects for a
/// `content_block_start` (`type:"tool_use"`) event: seeds a slot with `id`
/// and `name`, no argument content yet.
fn synth_tool_use_start(index: u32, id: &str, name: &str) -> String {
    json!({ "index": index, "id": id, "function": { "name": name } }).to_string()
}

/// Synthesizes the delta shape for an `input_json_delta` event: appends
/// `partial_json` to the slot at `index`'s argument buffer.
fn synth_input_json_delta(index: u32, partial_json: &str) -> String {
    json!({ "index": index, "function": { "arguments": partial_json } }).to_string()
}

/// Maps an `error` SSE event's body to a `BackendError`:
/// `error.type == "overloaded_error"` → `ServerError`,
/// `error.type == "rate_limit_error"` → `RateLimit`, anything else →
/// `BadRequest` naming the provider message (Implementation Notes).
fn classify_error_event(chunk: &Value) -> BackendError {
    let error = chunk.get("error");
    let error_type = error
        .and_then(|e| e.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("stream error")
        .to_string();
    match error_type {
        "overloaded_error" => BackendError::ServerError {
            status: 529,
            detail: message,
        },
        "rate_limit_error" => BackendError::RateLimit {
            retry_after_secs: None,
        },
        _ => BackendError::BadRequest { detail: message },
    }
}

/// Drives one SSE response body to completion, sending each translated
/// `StreamChunk`/`BackendError` into `tx` as it is produced. Returns as
/// soon as the receiving end is dropped (a cancelled/abandoned stream on
/// the caller's side).
async fn drive(
    response: reqwest::Response,
    tools: Vec<ToolSpec>,
    tx: mpsc::UnboundedSender<Result<StreamChunk, BackendError>>,
) {
    let mut events = Box::pin(response.bytes_stream().eventsource());
    let mut accumulator = new_tool_call_accumulator(&tools);
    let mut text_buffer = String::new();
    let mut thinking_buffer = String::new();
    let mut signature_buffer = String::new();
    let mut usage = Usage::default();
    let mut stop = StopReason::EndTurn;

    loop {
        let next = poll_fn(|cx| events.as_mut().poll_next(cx)).await;
        let event = match next {
            Some(Ok(event)) => event,
            Some(Err(err)) => {
                let _ = tx.send(Err(BackendError::Transport {
                    detail: err.to_string(),
                }));
                return;
            }
            None => break,
        };

        let Ok(chunk) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        let Some(kind) = chunk.get("type").and_then(Value::as_str) else {
            continue;
        };

        match kind {
            "message_start" => {
                if let Some(usage_value) = chunk.get("message").and_then(|m| m.get("usage")) {
                    if let Ok(wire_usage) = serde_json::from_value::<UsageWire>(usage_value.clone())
                    {
                        usage = map_usage(Some(wire_usage));
                    }
                }
            }
            "content_block_start" => {
                let index = chunk.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                if let Some(content_block) = chunk.get("content_block") {
                    if content_block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let id = content_block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let name = content_block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let raw = synth_tool_use_start(index, id, name);
                        if let Err(err) = accumulator.push_delta(&raw) {
                            let _ = tx.send(Err(err));
                            return;
                        }
                    }
                }
            }
            "content_block_delta" => {
                let index = chunk.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                let Some(delta) = chunk.get("delta") else {
                    continue;
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text_buffer.push_str(text);
                                if tx
                                    .send(Ok(StreamChunk::TextDelta(text.to_string())))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            if !text.is_empty() {
                                thinking_buffer.push_str(text);
                                if tx
                                    .send(Ok(StreamChunk::ThinkingDelta(text.to_string())))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(sig) = delta.get("signature").and_then(Value::as_str) {
                            signature_buffer.push_str(sig);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            let raw = synth_input_json_delta(index, partial);
                            if let Err(err) = accumulator.push_delta(&raw) {
                                let _ = tx.send(Err(err));
                                return;
                            }
                            if tx
                                .send(Ok(StreamChunk::ToolCallDelta { index, raw }))
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {}
            "message_delta" => {
                if let Some(reason) = chunk
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    stop = map_stop_reason(Some(reason));
                }
                if let Some(output_tokens) = chunk
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64)
                {
                    usage.output_tokens = output_tokens as u32;
                }
            }
            "message_stop" => break,
            "ping" => {}
            "error" => {
                let _ = tx.send(Err(classify_error_event(&chunk)));
                return;
            }
            _ => {}
        }
    }

    match accumulator.finish(stop) {
        Ok(tool_calls) => {
            let mut content = Vec::new();
            if !thinking_buffer.is_empty() {
                content.push(ContentBlock::Thinking {
                    text: thinking_buffer,
                    signature: if signature_buffer.is_empty() {
                        None
                    } else {
                        Some(signature_buffer)
                    },
                });
            }
            if !text_buffer.is_empty() {
                content.push(ContentBlock::Text { text: text_buffer });
            }
            let _ = tx.send(Ok(StreamChunk::Done(GenerateResponse {
                content,
                tool_calls,
                stop,
                usage,
            })));
        }
        Err(err) => {
            let _ = tx.send(Err(err));
        }
    }
}
