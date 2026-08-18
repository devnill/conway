//! `Backend::admit`: per-dialect fit
//! verdicts over `wiremock`, an oversized request refused with a typed
//! error naming the input size/headroom/window, and a zero-request
//! assertion proving admission never touches the network — no credentials,
//! no live provider, matching the item's own acceptance anchor.
//!
//! `estimate_wire_tokens` (`crate::admission`, `pub(crate)`) is not
//! reachable from this external test crate, so the fixtures below pick
//! prompt sizes and configured windows generously (a one-word prompt vs. a
//! multi-thousand-character one) rather than pinning an exact token count —
//! the heuristic's exact output is not this item's contract owns
//! calibrating it.

use std::collections::BTreeMap;
use std::path::Path;

use conway_core::content::{
    ContentBlock, PermissionClass, Role, SamplingParams, ToolCategory, ToolSpec,
};
use conway_core::error::BackendError;
use conway_core::ids::{BackendId, ModelId, ToolName};
use conway_core::ports::{Backend, GenerateRequest, TokenCountFidelity};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::anthropic::AnthropicBackend;
use conway_plugin_backends::config::{
    AnthropicConfig, Dialect, ModelOverrides, OpenAiCompatConfig, SecretString,
};
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use serde_json::json;
use wiremock::MockServer;

const TINY_WINDOW: u32 = 200;
const ROOMY_WINDOW: u32 = 1_000_000;
const HEADROOM: u32 = 32;

fn overrides_with_window(model: &str, max_context_tokens: u32) -> BTreeMap<String, ModelOverrides> {
    let mut map = BTreeMap::new();
    map.insert(
        model.to_string(),
        ModelOverrides {
            stream_tools: None,
            max_context_tokens: Some(max_context_tokens),
            reliability_tier: None,
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        },
    );
    map
}

fn anthropic_backend(base_url: &str, max_context_tokens: u32) -> AnthropicBackend {
    AnthropicBackend::new(AnthropicConfig {
        id: BackendId::new("anthropic"),
        api_key: SecretString::new("sk-ant-api03-test-key"),
        base_url: base_url.parse().unwrap(),
        anthropic_version: "2023-06-01".into(),
        timeout: None,
        models: overrides_with_window("claude-sonnet-4-6", max_context_tokens),
    })
    .expect("backend construction")
}

fn openai_backend(base_url: &str, max_context_tokens: u32) -> OpenAiCompatBackend {
    OpenAiCompatBackend::new(OpenAiCompatConfig {
        id: BackendId::new("local"),
        base_url: base_url.parse().unwrap(),
        api_key: None,
        profile: Dialect::OpenAi.profile(),
        timeout: None,
        metadata_path: None,
        models: overrides_with_window("gpt-5", max_context_tokens),
    })
    .expect("backend construction")
}

fn segment(text: &str) -> PromptSegment {
    PromptSegment::new(
        Role::User,
        vec![ContentBlock::Text { text: text.into() }],
        Provenance::UserPrompt,
    )
}

fn request(model: &str, text: &str) -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new(model),
        segments: vec![segment(text)],
        tools: vec![],
        params: SamplingParams::default(),
        prefix_key: None,
    }
}

fn weather_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("get_weather"),
        description: "Get the current weather for a city".into(),
        schema: serde_json::from_value(json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }))
        .unwrap(),
        category: ToolCategory::Fetch,
        permission: PermissionClass::Safe,
    }
}

// ---------------------------------------------------------------------
// Per-dialect fit verdicts, both over a `wiremock::MockServer` that never
// receives a request during admission.
// ---------------------------------------------------------------------

#[tokio::test]
async fn anthropic_admits_a_small_request_that_fits_the_window() {
    let server = MockServer::start().await;
    let backend = anthropic_backend(&server.uri(), ROOMY_WINDOW);

    let admission = backend
        .admit(&request("claude-sonnet-4-6", "hello"), HEADROOM)
        .expect("a one-word prompt against a 1,000,000-token window must fit");

    assert!(admission.fits());
    assert_eq!(admission.headroom_tokens, HEADROOM);
    assert_eq!(admission.max_context_tokens, ROOMY_WINDOW);

    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "admit() must never touch the network"
    );
}

#[tokio::test]
async fn openai_compat_admits_a_small_request_that_fits_the_window() {
    let server = MockServer::start().await;
    let backend = openai_backend(&server.uri(), ROOMY_WINDOW);

    let admission = backend
        .admit(&request("gpt-5", "hello"), HEADROOM)
        .expect("a one-word prompt against a 1,000,000-token window must fit");

    assert!(admission.fits());
    assert_eq!(admission.headroom_tokens, HEADROOM);
    assert_eq!(admission.max_context_tokens, ROOMY_WINDOW);

    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "admit() must never touch the network"
    );
}

// ---------------------------------------------------------------------
// An oversized request is refused with the typed error, naming the input
// size, the resolved headroom, and the window -- never trimmed, never
// silently retried against a bigger model.
// ---------------------------------------------------------------------

#[tokio::test]
async fn anthropic_refuses_an_oversized_request_with_the_typed_error() {
    let server = MockServer::start().await;
    let backend = anthropic_backend(&server.uri(), TINY_WINDOW);
    let huge = "x ".repeat(4_000); // far larger than TINY_WINDOW once estimated

    let err = backend
        .admit(&request("claude-sonnet-4-6", &huge), HEADROOM)
        .expect_err("a multi-thousand-character prompt cannot fit a 200-token window");

    let BackendError::ContextTooLarge {
        model,
        est_tokens,
        headroom_tokens,
        required_tokens,
        max_context_tokens,
        shortfall_tokens,
    } = err
    else {
        panic!("expected BackendError::ContextTooLarge, got a different variant");
    };
    assert_eq!(model, ModelId::new("claude-sonnet-4-6"));
    assert_eq!(headroom_tokens, HEADROOM);
    assert_eq!(max_context_tokens, TINY_WINDOW);
    assert_eq!(required_tokens, est_tokens.saturating_add(HEADROOM));
    assert_eq!(
        shortfall_tokens,
        required_tokens.saturating_sub(TINY_WINDOW)
    );
    assert!(est_tokens > 0, "the input size must be named, not zero");

    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a refused request must never reach the network -- not trimmed, not sent anyway"
    );
}

#[tokio::test]
async fn openai_compat_refuses_an_oversized_request_with_the_typed_error() {
    let server = MockServer::start().await;
    let backend = openai_backend(&server.uri(), TINY_WINDOW);
    let huge = "x ".repeat(4_000);

    let err = backend
        .admit(&request("gpt-5", &huge), HEADROOM)
        .expect_err("a multi-thousand-character prompt cannot fit a 200-token window");

    let BackendError::ContextTooLarge {
        model,
        headroom_tokens,
        max_context_tokens,
        est_tokens,
        required_tokens,
        shortfall_tokens,
    } = err
    else {
        panic!("expected BackendError::ContextTooLarge, got a different variant");
    };
    assert_eq!(model, ModelId::new("gpt-5"));
    assert_eq!(headroom_tokens, HEADROOM);
    assert_eq!(max_context_tokens, TINY_WINDOW);
    assert_eq!(required_tokens, est_tokens.saturating_add(HEADROOM));
    assert_eq!(
        shortfall_tokens,
        required_tokens.saturating_sub(TINY_WINDOW)
    );
    assert!(est_tokens > 0, "the input size must be named, not zero");

    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a refused request must never reach the network -- not trimmed, not sent anyway"
    );
}

// ---------------------------------------------------------------------
// The two dialects estimate their OWN wire body, not shared raw content:
// identical logical content (same segments + a tool) yields different
// `est_tokens` because Anthropic's and an OpenAI-compatible server's wire
// bodies are different byte sequences.
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_two_dialects_estimate_different_numbers_for_identical_content() {
    let anthropic_server = MockServer::start().await;
    let openai_server = MockServer::start().await;
    let anthropic = anthropic_backend(&anthropic_server.uri(), ROOMY_WINDOW);
    let openai = openai_backend(&openai_server.uri(), ROOMY_WINDOW);

    let mut anthropic_req = request("claude-sonnet-4-6", "What's the weather in Paris?");
    anthropic_req.tools = vec![weather_tool()];
    let mut openai_req = request("gpt-5", "What's the weather in Paris?");
    openai_req.tools = vec![weather_tool()];

    let anthropic_admission = anthropic.admit(&anthropic_req, HEADROOM).unwrap();
    let openai_admission = openai.admit(&openai_req, HEADROOM).unwrap();

    assert_ne!(
        anthropic_admission.est_tokens, openai_admission.est_tokens,
        "each dialect must measure its OWN wire body, not share one estimate"
    );
}

// ---------------------------------------------------------------------
// No count-tokens (or any other) network call on the admission path,
// across a sequence of both fitting and oversized checks for both
// dialects -- the `wiremock` expectation is exactly zero requests, ever.
// ---------------------------------------------------------------------

#[tokio::test]
async fn no_network_call_of_any_kind_occurs_across_a_mix_of_admission_checks() {
    let server = MockServer::start().await;
    let anthropic = anthropic_backend(&server.uri(), TINY_WINDOW);
    let openai = openai_backend(&server.uri(), TINY_WINDOW);
    let huge = "x ".repeat(4_000);

    let _ = anthropic.admit(&request("claude-sonnet-4-6", "hi"), HEADROOM);
    let _ = anthropic.admit(&request("claude-sonnet-4-6", &huge), HEADROOM);
    let _ = openai.admit(&request("gpt-5", "hi"), HEADROOM);
    let _ = openai.admit(&request("gpt-5", &huge), HEADROOM);

    let requests = server.received_requests().await.unwrap_or_default();
    assert!(
        requests.is_empty(),
        "expected zero requests (including any count-tokens call) on the admission path, got: {requests:?}"
    );
}

// ---------------------------------------------------------------------
// Board item 01M0AP4ADTGJWF3GFMCFWFF1ZQ, Part 1+2: both shipped dialects
// override `admit` with their own wire-body estimator, but neither has a
// vendored tokenizer or a measured calibration factor -- so both must
// DECLARE `TokenCountFidelity::Heuristic` through `Backend::token_fidelity`
// rather than silently inheriting a default a reader would have to infer
// from the absence of a tokenizer dependency.
// ---------------------------------------------------------------------

#[test]
fn anthropic_declares_heuristic_token_fidelity() {
    let server_uri = "http://127.0.0.1:1"; // never dialed; construction only
    let backend = anthropic_backend(server_uri, ROOMY_WINDOW);
    assert_eq!(backend.token_fidelity(), TokenCountFidelity::Heuristic);
}

#[test]
fn openai_compat_declares_heuristic_token_fidelity() {
    let server_uri = "http://127.0.0.1:1"; // never dialed; construction only
    let backend = openai_backend(server_uri, ROOMY_WINDOW);
    assert_eq!(backend.token_fidelity(), TokenCountFidelity::Heuristic);
}

// ---------------------------------------------------------------------
//: exactly one implementation of the headroom arithmetic and fit
// comparison in this crate -- both dialects must call the shared
// `conway_core::ports::check_admission` rather than restating
// `est_tokens + headroom_tokens <= max_context_tokens` (or any saturating
// variant of it) inline. A textual scan, not a behavioral test: the
// failure mode under test is DUPLICATION of source, which a well-chosen
// input cannot always distinguish from a merely-correct-by-coincidence
// restatement.
// ---------------------------------------------------------------------

#[test]
fn no_adapter_restates_the_headroom_arithmetic() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);

    let mut hits = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source file");
        for line in text.lines() {
            // `check_admission`'s own definition (in conway-core, not this
            // crate) is the only legitimate place `max_context_tokens` is
            // compared against a saturating sum -- this crate must never
            // contain a second such comparison.
            if line.contains("max_context_tokens")
                && (line.contains("saturating_add") || line.contains(">= ") || line.contains("<= "))
                && !line.trim_start().starts_with("//")
            {
                hits.push(format!("{}: {}", file.display(), line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "headroom arithmetic appears to be restated outside the shared \
         conway_core::ports::check_admission helper: {hits:#?}"
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}
