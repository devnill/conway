//! Integration tests for one-shot `--image` -- run against the real
//! compiled `conway` binary and a hand-rolled OpenAI-compatible mock
//! server (`common::mock_backend`), so the captured request body is the
//! actual wire bytes a real backend would have received, not a
//! hand-built fixture value that bypasses the production
//! attach-encode-assemble-send pipeline.
//!
//! Pure parsing/bounding/chip-rendering unit tests for the attach
//! mechanism itself live in `src/image_attach.rs`'s own `#[cfg(test)] mod
//! tests`; this file exercises the CLI surface (`--image`, `sessions
//! show`) those units are wired into.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{open_conway, run_conway, write_fixture, Fixture};
use conway::{SessionFilter, SessionId};

/// The one session a freshly-populated fixture has created so far --
/// mirrors every sibling suite's own helper of the same name (see
/// `session_names.rs`'s doc comment for why this is duplicated per file
/// rather than shared).
async fn only_session_id(fixture: &Fixture) -> SessionId {
    let conway = open_conway(fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 1, "expected exactly one session so far");
    sessions[0].id
}

fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

/// RFC 4648 base64, duplicated from `conway_cli::image_attach::
/// encode_base64` for the same "each `tests/*.rs` file is its own
/// independent crate" reason every sibling helper here is duplicated --
/// needed so this file can compute the EXACT base64 payload a real attach
/// would have produced, to assert its absence from `sessions show`'s
/// rendered output.
fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(ALPHABET[(n >> 18 & 0x3F) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// A minimal, structurally valid PNG: the 8-byte signature plus an IHDR
/// chunk declaring `width`x`height` -- everything
/// `conway_cli::image_attach::sniff_media_type`/`decode_dimensions` need,
/// without a real encoder (mirrors `image_attach.rs`'s own `fixture_png`
/// test helper, duplicated here for the same "each `tests/*.rs` file is
/// its own independent crate" reason every sibling suite's helpers are).
fn fixture_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

/// Acceptance: `--image shot.png` works headlessly, and the assembled
/// request the mock backend actually received contains an image block
/// naming the right media type -- the full real pipeline (`--image` parse
/// -> `image_attach::load_image_path` -> base64 -> `SessionHandle::
/// prompt_with_images` -> `LogRecord::UserImage` -> the context builder's
/// `ContentBlock::Image` -> the OpenAI-compatible wire adapter's
/// `image_url` mapping), never a hand-built request that bypasses any of
/// those steps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_flag_attaches_a_png_and_the_wire_request_carries_it() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let image_path = fixture.dir.path().join("shot.png");
    std::fs::write(&image_path, fixture_png(1280, 800)).expect("write fixture PNG");

    let out = run_conway(
        &[
            "-p",
            "what's broken here?",
            "--image",
            image_path.to_str().unwrap(),
        ],
        &fixture,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one /chat/completions request");
    let body = &requests[0];
    let messages = body["messages"].as_array().expect("messages array");
    let user_message = messages
        .iter()
        .find(|m| m["role"] == "user")
        .expect("a user message must be present");
    let content = user_message["content"]
        .as_array()
        .expect("user content must be an array once an image is attached");
    let image_entry = content
        .iter()
        .find(|c| c["type"] == "image_url")
        .unwrap_or_else(|| {
            panic!("no image_url block in the assembled request; full body: {body}")
        });
    let url = image_entry["image_url"]["url"]
        .as_str()
        .expect("image_url.url must be a string");
    assert!(
        url.starts_with("data:image/png;base64,"),
        "must carry the right media type: {url}"
    );
}

/// Acceptance: a `--image` path that is not a recognized image format is a
/// usage error (exit code 2), naming the file -- never a silent drop, and
/// never an agent turn that starts without the image.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_flag_rejects_an_unrecognized_format_as_a_usage_error() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let bogus_path = fixture.dir.path().join("notes.txt");
    std::fs::write(&bogus_path, b"just some text").expect("write fixture");

    let out = run_conway(
        &[
            "-p",
            "what is this?",
            "--image",
            bogus_path.to_str().unwrap(),
        ],
        &fixture,
    );
    assert_eq!(out.status.code(), Some(2), "usage error exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("notes.txt"),
        "usage error must name the offending file: {stderr}"
    );
    assert!(mock.requests().is_empty(), "no agent turn must ever start");
}

/// Acceptance: `conway sessions show` renders an attached image as the
/// one-line `[image #N · WxH · size]` chip, never the raw base64 payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_show_renders_the_image_chip_not_the_raw_bytes() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let image_path = fixture.dir.path().join("shot.png");
    std::fs::write(&image_path, fixture_png(1280, 800)).expect("write fixture PNG");

    let created = run_conway(
        &[
            "-p",
            "what's this?",
            "--image",
            image_path.to_str().unwrap(),
        ],
        &fixture,
    );
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let sid = only_session_id(&fixture).await;

    let show_out = run_conway(&["sessions", "show", &sid.to_string()], &fixture);
    assert!(show_out.status.success());
    let stdout = String::from_utf8_lossy(&show_out.stdout);
    assert!(
        stdout.contains("[image #1"),
        "must render the chip: {stdout}"
    );
    assert!(
        stdout.contains("1280\u{d7}800"),
        "chip must carry decoded dimensions: {stdout}"
    );
    // The whole point: the exact base64 payload a real attach of this
    // fixture would carry never leaks into the rendered output.
    let raw_base64 = encode_base64(&fixture_png(1280, 800));
    assert!(
        !stdout.contains(&raw_base64),
        "must not render the raw base64 payload: {stdout}"
    );
}
