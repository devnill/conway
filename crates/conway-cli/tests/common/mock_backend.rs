//! A hand-rolled OpenAI-compatible mock HTTP server for WI-113's one-shot
//! integration tests.
//!
//! This is deliberately not `wiremock`: `wiremock::ResponseTemplate` has no
//! way to stream a response body with a *mid-body* pause (`set_delay` only
//! delays time-to-first-byte, then flushes the whole static body at once)
//! or to hold a connection open indefinitely without ever completing it —
//! both of which `oneshot.rs`'s own binding notes require (`Chunk::Delay`
//! for the "streams incrementally, before the mock's final chunk" test;
//! `Chunk::Hang` for the SIGINT tests). A raw `tokio::net::TcpListener`
//! writing real HTTP/1.1 chunked-transfer-encoded bytes, with a real
//! `tokio::time::sleep` between chunks, gives genuine control over wire
//! timing that a declarative mock cannot. This matches the module's own
//! Implementation Notes, which name `hyper or axum` (not `wiremock`) as the
//! dependency for this exact mock.
//!
//! Request parsing uses `httparse` (already resolved in the workspace's
//! `Cargo.lock` as a transitive dependency of `reqwest`/`hyper`) rather
//! than hand-rolled header scanning.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// One simulated SSE delta in a scripted chat-completions response.
///
/// `#[allow(dead_code)]`: each `tests/*.rs` integration file compiles this
/// module fresh as part of its own independent crate, so which variants
/// count as "constructed" is evaluated per test binary -- a file that only
/// ever needs `Text`/`Finish` (e.g. `continuity.rs`) makes `ToolCall`/
/// `Delay`/`Hang` look unused *for that one binary*, even though other
/// suites in this same directory (`oneshot.rs`) do construct them. Scoped
/// here rather than to any one consuming file, since the variants
/// themselves are genuinely live, shared harness surface.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum Chunk {
    /// An assistant text delta: `delta.content = text`.
    Text(&'static str),
    /// A tool-call delta: id/name/full-arguments in one `tool_calls[0]`
    /// entry (`arguments` is JSON-encoded as a string, matching the wire
    /// shape every OpenAI-compatible server uses). Always paired with a
    /// following [`Chunk::Finish`] (e.g. `"tool_calls"`) by the script
    /// author -- this variant alone does not end the turn.
    ToolCall { name: &'static str, args: Value },
    /// The terminal `finish_reason` for this request (`"stop"`,
    /// `"tool_calls"`, ...). Always the last non-`Hang` chunk in an entry.
    Finish(&'static str),
    /// Pauses this connection for `Duration` before continuing to the next
    /// chunk in the same entry -- the barrier the incremental-streaming
    /// test uses to prove a line is readable before the mock's own final
    /// chunk is sent.
    Delay(Duration),
    /// Stops responding entirely (after flushing everything written so
    /// far) and never closes the connection -- used by the SIGINT tests,
    /// which need a request that never naturally completes.
    Hang,
    /// Answers the whole request with this HTTP error status and body
    /// instead of an SSE stream -- only meaningful as the FIRST (and
    /// normally only) chunk of an entry, since the 200 SSE head has
    /// already gone out otherwise. Drives the backend adapters'
    /// status-to-`BackendError` classification table (401/403 -> `Auth`,
    /// 429 -> `RateLimit`, 5xx -> `ServerError`, ...).
    HttpError { status: u16, body: &'static str },
}

/// One request's whole scripted response is `Vec<Chunk>`; `Script`'s outer
/// `Vec` has one entry per successive `/chat/completions` request the CLI
/// makes against this mock. A request beyond the script's length gets a
/// single default `Finish("stop")` with no text -- see [`MockBackend`]'s
/// doc for why a graceful default (rather than a panic or a hang) is the
/// right behavior for an unscripted request.
pub struct Script(pub Vec<Vec<Chunk>>);

/// A running mock server plus everything a test needs to assert against
/// it.
pub struct MockHandle {
    /// `http://127.0.0.1:<port>/v1` -- the value a `[backends.*]` entry's
    /// `base_url` should be set to; `OpenAiCompatBackend::chat_url()`
    /// appends `/chat/completions` to this, and the capability probe
    /// appends `/models`.
    pub base_url: String,
    pub model: String,
    /// Read only through [`MockHandle::requests`], which carries the
    /// per-binary `dead_code` rationale for both.
    #[allow(dead_code)]
    requests: Arc<Mutex<Vec<Value>>>,
    accept_task: JoinHandle<()>,
}

impl MockHandle {
    /// Every `/chat/completions` request body received so far, in arrival
    /// order, parsed as JSON.
    ///
    /// `#[allow(dead_code)]` for the same reason [`Chunk`] carries one: each
    /// `tests/*.rs` file compiles this module fresh as its own crate, so a
    /// suite that asserts only on a process's exit code and stderr (e.g.
    /// `decline_backend_kind.rs`) never calls this and makes it look unused
    /// *for that one binary*, while `oneshot.rs` and `continuity.rs` do call
    /// it. Scoped here rather than to any one consuming file, since the
    /// accessor is genuinely live, shared harness surface.
    #[allow(dead_code)]
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockHandle {
    fn drop(&mut self) {
        // Stops accepting new connections. Already-spawned per-connection
        // tasks (including any parked forever in a `Hang`) are cleaned up
        // when the test's `#[tokio::test]` runtime itself is dropped at the
        // end of the test function -- tokio aborts every task still
        // spawned on a runtime that is being shut down.
        self.accept_task.abort();
    }
}

pub struct MockBackend;

impl MockBackend {
    /// Starts the mock on an ephemeral loopback port with model id
    /// `"mock-model"`. See [`Self::start_with_model`] for a custom id.
    pub async fn start(script: Script) -> MockHandle {
        Self::start_with_model(script, "mock-model").await
    }

    pub async fn start_with_model(script: Script, model: &str) -> MockHandle {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral mock port");
        let port = listener.local_addr().expect("local_addr").port();

        let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let script_entries: Arc<Mutex<std::collections::VecDeque<Vec<Chunk>>>> =
            Arc::new(Mutex::new(script.0.into_iter().collect()));
        let model_owned = model.to_string();
        let call_id_counter = Arc::new(AtomicU64::new(1));

        let accept_task = tokio::spawn({
            let requests = requests.clone();
            let script_entries = script_entries.clone();
            let model_owned = model_owned.clone();
            let call_id_counter = call_id_counter.clone();
            async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let requests = requests.clone();
                    let script_entries = script_entries.clone();
                    let model_owned = model_owned.clone();
                    let call_id_counter = call_id_counter.clone();
                    tokio::spawn(async move {
                        let _ = handle_connection(
                            stream,
                            requests,
                            script_entries,
                            model_owned,
                            call_id_counter,
                        )
                        .await;
                    });
                }
            }
        });

        MockHandle {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: model.to_string(),
            requests,
            accept_task,
        }
    }
}

/// Reads one HTTP/1.1 request off `stream` (headers via `httparse`, then
/// exactly `Content-Length` more body bytes), routes it, and writes a
/// response. Returns once the response is fully written (or the connection
/// is deliberately left open forever, for `Chunk::Hang`).
async fn handle_connection(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<Value>>>,
    script_entries: Arc<Mutex<std::collections::VecDeque<Vec<Chunk>>>>,
    model: String,
    call_id_counter: Arc<AtomicU64>,
) -> std::io::Result<()> {
    let Some((path, method, body)) = read_request(&mut stream).await? else {
        return Ok(());
    };

    if method == "GET" && path.starts_with("/v1/models") {
        let body = serde_json::json!({ "data": [{ "id": model }] }).to_string();
        write_json_response(&mut stream, &body).await?;
        return Ok(());
    }

    if method == "POST" && path.starts_with("/v1/chat/completions") {
        if let Ok(value) = serde_json::from_slice::<Value>(&body) {
            requests.lock().unwrap().push(value);
        }
        let entry = script_entries.lock().unwrap().pop_front();
        write_sse_response(&mut stream, entry, &call_id_counter).await?;
        return Ok(());
    }

    // Unrecognized path/method: a plain 404, closing the connection.
    let body = b"not found";
    let head = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

/// Reads headers into a growing buffer until `httparse` parses them
/// completely, then reads exactly `Content-Length` more bytes for the body
/// (any body bytes already pulled into the buffer past the header
/// terminator are accounted for). Returns `None` on a client disconnect
/// before a complete request line ever arrived (e.g. a stray probe
/// connection) -- fine for the mock to just drop silently.
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

    // Re-parse to pull out method/path/content-length from the now-complete
    // header block (httparse borrows `buf`, so this is a fresh, short-lived
    // parse rather than trying to carry the first one across the loop's
    // mutable borrow of `buf`).
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
    stream.flush().await?;
    Ok(())
}

/// Streams `entry` as chunked-transfer-encoded SSE, one HTTP chunk per
/// `data:` line, flushing after each so a client genuinely observes bytes
/// as they are produced rather than all at once at the end. `entry` is
/// `None` (script exhausted) -> a single `Finish("stop")`, matching
/// [`Script`]'s doc on the graceful default for an unscripted request.
async fn write_sse_response(
    stream: &mut TcpStream,
    entry: Option<Vec<Chunk>>,
    call_id_counter: &AtomicU64,
) -> std::io::Result<()> {
    let chunks = entry.unwrap_or_else(|| vec![Chunk::Finish("stop")]);
    // A leading `HttpError` replaces the whole response: the SSE head must
    // not go out first (see the variant's doc).
    if let Some(Chunk::HttpError { status, body }) = chunks.first() {
        let reason = match status {
            401 => "Unauthorized",
            403 => "Forbidden",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body.as_bytes()).await?;
        stream.flush().await?;
        return Ok(());
    }
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await?;

    for chunk in chunks {
        match chunk {
            Chunk::Text(text) => {
                let event = serde_json::json!({
                    "choices": [{"delta": {"content": text}, "finish_reason": null}]
                });
                write_sse_event(stream, &event).await?;
            }
            Chunk::ToolCall { name, args } => {
                let call_id = format!("call_{}", call_id_counter.fetch_add(1, Ordering::SeqCst));
                let first = serde_json::json!({
                    "choices": [{
                        "delta": {"tool_calls": [{
                            "index": 0,
                            "id": call_id,
                            "function": {"name": name, "arguments": ""}
                        }]},
                        "finish_reason": null
                    }]
                });
                write_sse_event(stream, &first).await?;
                let second = serde_json::json!({
                    "choices": [{
                        "delta": {"tool_calls": [{
                            "index": 0,
                            "function": {"arguments": args.to_string()}
                        }]},
                        "finish_reason": null
                    }]
                });
                write_sse_event(stream, &second).await?;
            }
            Chunk::Finish(reason) => {
                let event = serde_json::json!({
                    "choices": [{"delta": {}, "finish_reason": reason}]
                });
                write_sse_event(stream, &event).await?;
            }
            Chunk::Delay(duration) => {
                tokio::time::sleep(duration).await;
            }
            Chunk::Hang => {
                std::future::pending::<()>().await;
            }
            // Handled before the SSE head is written (above); mid-stream
            // the 200 head is already out, so there is no coherent error
            // response left to send -- treat it as a no-op.
            Chunk::HttpError { .. } => {}
        }
    }

    write_http_chunk(stream, b"data: [DONE]\n\n").await?;
    // Terminating zero-length chunk.
    stream.write_all(b"0\r\n\r\n").await?;
    stream.flush().await?;
    Ok(())
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
    stream.flush().await?;
    Ok(())
}
