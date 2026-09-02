//! SSE streaming for `OpenAiCompatBackend::stream`: drains the
//! `eventsource_stream::EventStream` on a spawned task, translating each
//! `choices[0].delta` into `StreamChunk`s and feeding tool-call deltas to a
//! `ToolCallAccumulator` — the same accumulator type `wire.rs`
//! uses for the non-streaming path.
//!
//! `spawn` is only ever called with an already-`200`-classified
//! `reqwest::Response` (see `OpenAiCompatBackend::stream`): the module
//! boundary rule ("streaming requests use `send_with_retry` only for the
//! *initial* response") is enforced structurally by that call site, not by
//! this module — everything here, a mid-body transport error or a
//! truncated tool-call argument buffer at `[DONE]`, is surfaced as a stream
//! item and never retried.

use std::future::poll_fn;
use std::pin::Pin;
use std::task::{Context, Poll};

use conway_core::content::{CacheAccounting, ContentBlock, StopReason, ToolSpec, Usage};
use conway_core::error::BackendError;
use conway_core::ports::{BoxStream, GenerateResponse, StreamChunk};
use eventsource_stream::Eventsource;
use futures_core::Stream;
use serde_json::Value;
use tokio::sync::mpsc;

use super::wire::{map_finish_reason, map_usage, UsageWire};
use crate::profile::Profile;
use crate::tool_calls::ToolCallAccumulator;

const DONE_MARKER: &str = "[DONE]";

/// Sends the response body's SSE stream into a spawned driver task and
/// returns a `Stream` reading the task's output. `stream()` itself only
/// ever awaits the initial `send_with_retry`; everything from here on runs
/// off that task, decoupled from the caller's poll loop. `profile` is
/// owned (not borrowed): the spawned task outlives this call's stack frame.
pub(crate) fn spawn(
    response: reqwest::Response,
    profile: Profile,
    tools: Vec<ToolSpec>,
) -> BoxStream<'static, Result<StreamChunk, BackendError>> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(drive(response, profile, tools, tx));
    Box::pin(ChunkStream(rx))
}

/// A `futures_core::Stream` reading from an `UnboundedReceiver` — the
/// bridge between the spawned `drive` task and the `BoxStream` this
/// module's caller returns.
struct ChunkStream(mpsc::UnboundedReceiver<Result<StreamChunk, BackendError>>);

impl Stream for ChunkStream {
    type Item = Result<StreamChunk, BackendError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// Drives one SSE response body to completion, sending each translated
/// `StreamChunk`/`BackendError` into `tx` as it is produced. Terminates as
/// soon as the receiving end is dropped, even while blocked waiting for the
/// next SSE event or while the server emits content-free keep-alive chunks:
/// every await races `tx.closed()` (incremental review S1, cycle 1), so
/// dropping the returned stream proactively releases the task and its HTTP
/// connection.
async fn drive(
    response: reqwest::Response,
    profile: Profile,
    tools: Vec<ToolSpec>,
    tx: mpsc::UnboundedSender<Result<StreamChunk, BackendError>>,
) {
    let mut events = Box::pin(response.bytes_stream().eventsource());
    let mut accumulator = ToolCallAccumulator::new(profile.tool_call_style, &tools);
    let mut text_buffer = String::new();
    let mut stop = None;
    // Neutral sentinel until a real `usage` frame arrives: dialects with
    // `supports_stream_options = false` (vllm_hermes, lm_studio,
    // llama_cpp_server) may never send one, and `Usage::default()`'s
    // `cache_accounting` now defaults to `Reported` -- leaving it there
    // would persist a false "provider reported zero cache" fact.
    let mut usage = Usage {
        cache_accounting: CacheAccounting::NotReported,
        ..Usage::default()
    };

    loop {
        let next = tokio::select! {
            biased;
            () = tx.closed() => return,
            next = poll_fn(|cx| events.as_mut().poll_next(cx)) => next,
        };
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

        if event.data == DONE_MARKER {
            break;
        }

        let Ok(chunk) = serde_json::from_str::<Value>(&event.data) else {
            // Not every SSE line a real server sends is guaranteed to be a
            // JSON chat-completion chunk (some proxies emit `: keepalive`
            // comments as an empty-data `message` event); skip anything
            // that doesn't parse rather than failing the whole stream.
            continue;
        };

        if let Some(usage_value) = chunk.get("usage") {
            if let Ok(wire_usage) = serde_json::from_value::<UsageWire>(usage_value.clone()) {
                usage = map_usage(Some(wire_usage));
            }
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            continue;
        };

        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    // Route through the accumulator: for VllmHermes it holds
                    // back inline `<tool_call>` text (vllm#31871) and yields
                    // only genuinely-emittable prose; every other dialect
                    // (and Hermes post-structured_seen) is a passthrough
                    // (rework, cycle 1).
                    match accumulator.push_content_delta(text) {
                        Ok(Some(emit)) if !emit.is_empty() => {
                            text_buffer.push_str(&emit);
                            if tx.send(Ok(StreamChunk::TextDelta(emit))).is_err() {
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(err) => {
                            let _ = tx.send(Err(err));
                            return;
                        }
                    }
                }
            }

            if let Some(reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
            {
                if !reasoning.is_empty()
                    && tx
                        .send(Ok(StreamChunk::ThinkingDelta(reasoning.to_string())))
                        .is_err()
                {
                    return;
                }
            }

            if let Some(tool_call_deltas) = delta.get("tool_calls").and_then(Value::as_array) {
                for (position, item) in tool_call_deltas.iter().enumerate() {
                    let index = item
                        .get("index")
                        .and_then(Value::as_u64)
                        .map(|value| value as u32)
                        .unwrap_or(position as u32);
                    let raw = serde_json::to_string(item).unwrap_or_default();
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
        }

        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            stop = Some(map_finish_reason(Some(reason)));
        }
    }

    // The Hermes fallback overrides the stop reason when an inline tool
    // call was parsed (stop_override is &self; finish consumes self, so
    // read the override first).
    let stop = accumulator
        .stop_override()
        .unwrap_or(stop.unwrap_or(StopReason::EndTurn));
    match accumulator.finish(stop) {
        Ok(tool_calls) => {
            let mut content = Vec::new();
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
