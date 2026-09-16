// Compiled fresh into every `tests/*.rs` binary that declares `mod common;`
// -- a fixture only one binary in this directory reaches for looks like
// dead code from every other binary's own point of view (the same
// per-binary blindness `common::mock_backend`'s own module doc states for
// itself). Exempted here rather than scattering `#[allow(dead_code)]`
// across every item.
#![allow(dead_code)]

//! A second, INDEPENDENT OpenAI-compatible mock server, sibling to
//! `common::mock_backend`, existing for exactly one reason:
//! `mock_backend::{Chunk, Script}` has no slot for a wire `usage` object at
//! all -- neither `write_sse_response` nor `write_json_chat_response` ever
//! emits one -- so no test built on the shared mock can ever produce a
//! backend response `conway` reads as `CacheAccounting::Reported` (board
//! item `01M1ZJTXRWRA93KRWZSZBA745G`, gate 5). Adding that capability to
//! the shared module is out of this writer's fence (`common/mock_backend.rs`
//! is not in it); this is a small, self-contained sibling instead, built the
//! same way (a raw `tokio::net::TcpListener` speaking real HTTP/1.1
//! chunked-transfer SSE, parsed with `httparse`) but trimmed to exactly what
//! gate 5 needs: a scripted assistant TEXT reply, optionally paired with a
//! scripted `usage` object.
//!
//! **Wire shape, and why it lands where `conway` actually reads it.**
//! `conway-plugin-backends::openai_compat::stream::spawn` (the real
//! decoder) checks EVERY SSE `data:` event's parsed JSON object for a
//! top-level `"usage"` key (`chunk.get("usage")`), independent of
//! `choices` -- exactly the shape real OpenAI's `stream_options.
//! include_usage: true` produces: content deltas first, then one trailing
//! event carrying `usage` (and empty/absent `choices`). This mock's own
//! final SSE event, when a turn's `usage` is scripted `Some`, is written in
//! that same shape. `conway_plugin_backends::openai_compat::wire::
//! map_usage` (the field-to-`Usage` mapper) marks `cache_accounting:
//! Reported` only when `usage.prompt_tokens_details.cached_tokens` (or the
//! top-level Kimi-shaped `usage.cached_tokens`) is PRESENT, even as `0` --
//! `NotReported` when the whole `usage` object is absent, or present
//! without that inner field. [`CacheUsage::cached_tokens`] mirrors that
//! `Option`-shaped presence/absence distinction on purpose: `None` omits
//! `prompt_tokens_details` entirely (the "usage object present, cache
//! breakdown absent" case -- still `NotReported`, not zero), `Some(n)`
//! writes `prompt_tokens_details: { cached_tokens: n }`.
//!
//! Handles only the streaming path (`POST /v1/chat/completions` with
//! `"stream": true`) -- every root session in this suite has the full
//! builtin toolset available, and `Dialect::OpenAi`'s
//! `Streaming{validated:true}` default always takes that path regardless of
//! whether the scripted turn itself calls a tool (`common::mock_backend`'s
//! own module doc states the identical fact for the shared mock). No
//! `ToolCall`/`Delay`/`Hang`/`HttpError` support: gate 5 needs none of them,
//! and adding unused vocabulary here would just be dead code with extra
//! steps.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// The cache-relevant slice of a scripted turn's `usage` object -- see this
/// module's own doc for exactly how [`Self::cached_tokens`]'s `Option`
/// shape maps onto `CacheAccounting::{Reported,NotReported}`.
#[derive(Clone, Copy, Debug)]
pub struct CacheUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// `Some(n)` -> the response's `usage.prompt_tokens_details.
    /// cached_tokens` field is present, set to `n` (a real provider sends
    /// `0` for "cache-eligible but missed", not absence -- `Some(0)` is
    /// that case). `None` -> `usage.prompt_tokens_details` is omitted
    /// entirely, the "this backend's own `usage` object carried no cache
    /// breakdown" case.
    pub cached_tokens: Option<u32>,
}

/// One scripted assistant reply: the text content, plus an optional
/// `usage` object trailing it (see this module's own doc for the wire
/// shape). `usage: None` -> no `usage` key anywhere in this turn's SSE
/// stream at all, matching a dialect/provider that never sends the field.
#[derive(Clone, Debug)]
pub struct CacheTurn {
    pub text: &'static str,
    pub usage: Option<CacheUsage>,
}

pub struct CacheMockHandle {
    pub base_url: String,
    pub model: String,
    accept_task: JoinHandle<()>,
}

impl Drop for CacheMockHandle {
    fn drop(&mut self) {
        // Same rationale as `mock_backend::MockHandle`'s own `Drop`: stop
        // accepting; any connection still parked is reaped when this
        // test's `#[tokio::test]` runtime itself shuts down.
        self.accept_task.abort();
    }
}

pub struct CacheMockBackend;

impl CacheMockBackend {
    pub async fn start(model: &str, script: Vec<CacheTurn>) -> CacheMockHandle {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral cache-mock port");
        let port = listener.local_addr().expect("local_addr").port();

        let script_entries: Arc<Mutex<std::collections::VecDeque<CacheTurn>>> =
            Arc::new(Mutex::new(script.into_iter().collect()));
        let model_owned = model.to_string();
        let call_id_counter = Arc::new(AtomicU64::new(1));

        let accept_task = tokio::spawn({
            let script_entries = script_entries.clone();
            let model_owned = model_owned.clone();
            async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let script_entries = script_entries.clone();
                    let model_owned = model_owned.clone();
                    let call_id_counter = call_id_counter.clone();
                    tokio::spawn(async move {
                        let _ =
                            handle_connection(stream, script_entries, model_owned, call_id_counter)
                                .await;
                    });
                }
            }
        });

        CacheMockHandle {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: model.to_string(),
            accept_task,
        }
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    script_entries: Arc<Mutex<std::collections::VecDeque<CacheTurn>>>,
    model: String,
    call_id_counter: Arc<AtomicU64>,
) -> std::io::Result<()> {
    let Some((path, method, _body)) = read_request(&mut stream).await? else {
        return Ok(());
    };

    if method == "GET" && path.starts_with("/v1/models") {
        let body = serde_json::json!({ "data": [{ "id": model }] }).to_string();
        return write_json_response(&mut stream, &body).await;
    }

    if method == "POST" && path.starts_with("/v1/chat/completions") {
        let entry = script_entries.lock().unwrap().pop_front();
        return write_sse_response(&mut stream, entry, &call_id_counter).await;
    }

    let body = b"not found";
    let head = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// Identical shape/contract to `mock_backend::read_request` -- see that
/// function's own doc. Duplicated rather than shared: the two mocks are
/// deliberately independent sibling modules (this module's own top doc).
async fn read_request(
    stream: &mut TcpStream,
) -> std::io::Result<Option<(String, String, Vec<u8>)>> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let headers_end = loop {
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut req = httparse::Request::new(&mut headers);
        match req.parse(&buf) {
            Ok(httparse::Status::Complete(len)) => break len,
            Ok(httparse::Status::Partial) => {
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    return Ok(None);
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            Err(_) => return Ok(None),
        }
    };

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    req.parse(&buf)
        .expect("re-parse of already-complete headers");
    let method = req.method.unwrap_or("GET").to_string();
    let path = req.path.unwrap_or("/").to_string();
    let content_length: usize = req
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-length"))
        .and_then(|h| std::str::from_utf8(h.value).ok())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);

    let mut body = buf[headers_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);

    Ok(Some((path, method, body)))
}

async fn write_json_response(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await
}

/// Streams `entry` as chunked SSE: one text delta, one `finish_reason:
/// "stop"` event, then -- only when `entry.usage` is `Some` -- one FINAL
/// event carrying `"usage"` at the top level alongside an empty `choices`
/// array, the exact shape `stream::spawn`'s own `chunk.get("usage")` check
/// reads (this module's own top doc). `entry: None` (script exhausted) ->
/// a bare `Finish("stop")`-equivalent with no text and no usage, mirroring
/// `mock_backend`'s own graceful-default contract for an unscripted
/// request.
async fn write_sse_response(
    stream: &mut TcpStream,
    entry: Option<CacheTurn>,
    _call_id_counter: &AtomicU64,
) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await?;

    let (text, usage) = match entry {
        Some(turn) => (turn.text, turn.usage),
        None => ("", None),
    };

    if !text.is_empty() {
        let delta = serde_json::json!({
            "choices": [{"delta": {"content": text}, "finish_reason": null}]
        });
        write_sse_event(stream, &delta).await?;
    }

    let finish = serde_json::json!({
        "choices": [{"delta": {}, "finish_reason": "stop"}]
    });
    write_sse_event(stream, &finish).await?;

    if let Some(usage) = usage {
        let mut usage_obj = serde_json::Map::new();
        usage_obj.insert("prompt_tokens".into(), Value::from(usage.prompt_tokens));
        usage_obj.insert(
            "completion_tokens".into(),
            Value::from(usage.completion_tokens),
        );
        if let Some(cached) = usage.cached_tokens {
            usage_obj.insert(
                "prompt_tokens_details".into(),
                serde_json::json!({ "cached_tokens": cached }),
            );
        }
        let usage_event = serde_json::json!({
            "choices": [],
            "usage": Value::Object(usage_obj),
        });
        write_sse_event(stream, &usage_event).await?;
    }

    write_http_chunk(stream, b"data: [DONE]\n\n").await?;
    stream.write_all(b"0\r\n\r\n").await?;
    stream.flush().await
}

async fn write_sse_event(stream: &mut TcpStream, event: &Value) -> std::io::Result<()> {
    let line = format!("data: {}\n\n", event);
    write_http_chunk(stream, line.as_bytes()).await
}

async fn write_http_chunk(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    let header = format!("{:x}\r\n", data.len());
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(data).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await
}
