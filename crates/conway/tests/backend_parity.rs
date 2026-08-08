//! Board item 01KZHEZF8XCD0TMDYZQP06J2KH's parity proof, mirroring
//! `plugin_builtin_parity.rs`'s mechanism for `conway::plugin`: a small but
//! genuinely complete `Backend` implementation, written using only `use
//! conway::backend::...`/`use conway::...`, that exercises every name
//! `conway::backend` exports. A shrink in that curated export list is a
//! BUILD failure here, not a runtime assertion.
//!
//! **This file must never import `conway_core`, and does not enable
//! `conway-core`'s `fakes` feature** — neither is available to a third
//! party (the same rule `plugin_surface.rs`/`plugin_builtin_parity.rs`
//! state for themselves).
//!
//! Unlike `plugin_builtin_parity.rs` (compile-only: its replicas are never
//! actually invoked, since a real `ToolCtx` needs `conway-core`'s `fakes`
//! feature), `StubBackend` below really runs end to end.
//! `generate`/`stream`/`probe` perform no network I/O by construction —
//! there is no real dialect to reach, so calling them for real here is
//! possible, and doing so is a stronger proof than a compile-only one: it
//! is exactly the shape a real embedder's own in-process test double for
//! `Backend` would take (an embedder writing an integration test for code
//! that calls a `Backend` needs precisely this kind of stub, not a mock
//! crate — `docs/embedding.md`'s own `FakeBackend` example is the
//! in-workspace precedent for the same idea).
//!
//! `admit` is overridden (not left at the trait's default) and calls
//! `check_admission` for its arithmetic (P-14) — the property the item's
//! own brief calls out as non-optional: an author who cannot name
//! `check_admission` cannot honour `Backend::admit`'s contract.
//!
//! Five "second-level" field types this file needs to construct a
//! `ToolSpec`/`PromptSegment` (`Role`, `PermissionClass`, `ToolCategory`,
//! `ToolName`, `Provenance`) are deliberately imported from where they
//! already live (`conway::plugin`/`conway::` root) rather than from
//! `conway::backend` — see that module's own doc, "Deliberately NOT here",
//! for why they are not duplicated a third time.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use conway::backend::{
    async_trait, check_admission, Admission, Backend, BackendError, BackendId, BoxStream,
    CacheMode, Capabilities, ContentBlock, GenerateRequest, GenerateResponse, ModelId, PrefixKey,
    ProbeReport, PromptSegment, ReliabilityTier, SamplingParams, StopReason, StreamChunk,
    StructuredOutput, ToolCall, ToolCallSupport, ToolSpec, Usage,
};
use conway::plugin::{PermissionClass, Role};
use conway::{Provenance, ToolCategory, ToolName};

/// The marker this test's own "please call a tool" scenario looks for in
/// the last user segment's text, mirroring how a real dialect adapter
/// decides whether the model asked for a tool call — except here it's the
/// fixture deciding, deterministically, with no model in the loop.
const TOOL_TRIGGER: &str = "please use the tool";

/// A small, complete, in-process `Backend`: no network I/O, deterministic,
/// synchronous logic wherever the trait allows it. Exactly the shape a
/// third-party embedder's own test double would take.
struct StubBackend {
    id: BackendId,
    max_context_tokens: u32,
}

/// The dialect-neutral request-size estimate this stub's `admit` override
/// uses — deliberately NOT `conway_core`'s own default estimator (this
/// file cannot see it; it lives in `conway-core`, not `conway`), but the
/// same shape: walk the segments' text and count the tools. Real dialect
/// adapters (`conway-backends`) build their actual wire body first and
/// measure that instead — this stub has no wire format to build.
fn estimate_tokens(req: &GenerateRequest) -> u32 {
    let mut total: u32 = 0;
    for segment in &req.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                total = total.saturating_add(
                    u32::try_from(text.len()).unwrap_or(u32::MAX).div_ceil(4),
                );
            }
        }
    }
    total.saturating_add(
        u32::try_from(req.tools.len())
            .unwrap_or(u32::MAX)
            .saturating_mul(16),
    )
}

fn last_user_text(req: &GenerateRequest) -> String {
    req.segments
        .iter()
        .rev()
        .find(|segment| segment.role == Role::User)
        .into_iter()
        .flat_map(|segment| segment.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl StubBackend {
    /// The real response-construction logic, shared by `generate` and
    /// `stream` (a streamed reply is "the same answer, chunked" for this
    /// stub — exactly how a real dialect's non-streaming/streaming paths
    /// agree on content while differing only in delivery).
    fn respond(&self, req: &GenerateRequest) -> GenerateResponse {
        let text = last_user_text(req);
        if text.contains(TOOL_TRIGGER) {
            if let Some(tool) = req.tools.first() {
                return GenerateResponse {
                    content: vec![],
                    tool_calls: vec![ToolCall {
                        call_id: "call-1".to_string(),
                        name: tool.name.clone(),
                        arguments: serde_json::json!({}),
                    }],
                    stop: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: estimate_tokens(req),
                        output_tokens: 4,
                        ..Usage::default()
                    },
                };
            }
        }
        GenerateResponse {
            content: vec![ContentBlock::Text {
                text: format!("echo: {text}"),
            }],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage {
                input_tokens: estimate_tokens(req),
                output_tokens: u32::try_from(text.len()).unwrap_or(u32::MAX).div_ceil(4),
                ..Usage::default()
            },
        }
    }
}

#[async_trait]
impl Backend for StubBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            cache: CacheMode::ImplicitPrefix {
                min_prefix_tokens: 256,
            },
            parallel_tool_calls: true,
            structured_output: StructuredOutput::JsonSchema,
            max_context_tokens: self.max_context_tokens,
            reasoning: false,
            reliability_tier: ReliabilityTier::Verified,
        }
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        Ok(self.respond(&req))
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.respond(&req);
        let first_delta = match response.content.first() {
            Some(ContentBlock::Text { text }) => StreamChunk::TextDelta(text.clone()),
            _ => StreamChunk::TextDelta(String::new()),
        };
        let items: VecDeque<Result<StreamChunk, BackendError>> =
            VecDeque::from(vec![Ok(first_delta), Ok(StreamChunk::Done(response))]);
        Ok(Box::pin(VecStream { items }))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        Ok(ProbeReport {
            ok: true,
            latency_ms: 0,
            models: vec![ModelId::new("stub-model")],
            detail: None,
            at: chrono::Utc::now(),
        })
    }

    /// Overridden, not left at the trait's default: every override MUST
    /// call [`check_admission`] for the fits/shortfall arithmetic rather
    /// than restating it (P-14) — this is that call.
    fn admit(
        &self,
        req: &GenerateRequest,
        headroom_tokens: u32,
    ) -> Result<Admission, BackendError> {
        let est_tokens = estimate_tokens(req);
        check_admission(
            req.model.clone(),
            est_tokens,
            headroom_tokens,
            self.max_context_tokens,
        )
    }
}

/// A hand-rolled `Stream` over a fixed `VecDeque` — this file's own
/// analogue of `conway-core`'s test-only `Empty` stream in
/// `ports/backend.rs`'s `box_stream_type_is_usable`, extended to actually
/// yield items (that test only proves the alias is usable; this one proves
/// a real `Backend::stream` implementation can be driven end to end).
struct VecStream {
    items: VecDeque<Result<StreamChunk, BackendError>>,
}

impl Stream for VecStream {
    type Item = Result<StreamChunk, BackendError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.get_mut().items.pop_front())
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn user_segment(text: &str) -> PromptSegment {
    PromptSegment::new(
        Role::User,
        vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        Provenance::UserPrompt,
    )
}

/// Only used through `schemars::schema_for!` below, never deserialized —
/// this stub never actually parses `ToolCall::arguments`, so the field
/// itself is unread; `#[allow(dead_code)]` says so rather than deleting the
/// field the real `lookup` tool's schema would have.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct LookupArgs {
    #[allow(dead_code)]
    query: String,
}

fn lookup_tool() -> ToolSpec {
    ToolSpec {
        name: ToolName::new("lookup"),
        description: "Looks something up.".to_string(),
        schema: schemars::schema_for!(LookupArgs),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

fn request(text: &str, tools: Vec<ToolSpec>) -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new("stub-model"),
        segments: vec![user_segment(text)],
        tools,
        params: SamplingParams {
            temperature: Some(0.7),
            max_tokens: Some(512),
            ..SamplingParams::default()
        },
        prefix_key: Some(PrefixKey::from_blake3(blake3::hash(text.as_bytes()))),
    }
}

fn backend(max_context_tokens: u32) -> StubBackend {
    StubBackend {
        id: BackendId::new("stub"),
        max_context_tokens,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn capabilities_declares_the_shape_this_stub_promises() {
    let caps = backend(200_000).capabilities(&ModelId::new("stub-model"));
    assert_eq!(
        caps.tool_calling,
        ToolCallSupport::Streaming { validated: true }
    );
    assert_eq!(
        caps.cache,
        CacheMode::ImplicitPrefix {
            min_prefix_tokens: 256
        }
    );
    assert!(caps.parallel_tool_calls);
    assert_eq!(caps.structured_output, StructuredOutput::JsonSchema);
    assert_eq!(caps.max_context_tokens, 200_000);
    assert!(!caps.reasoning);
    assert_eq!(caps.reliability_tier, ReliabilityTier::Verified);
}

#[test]
fn id_returns_the_configured_identity() {
    assert_eq!(backend(1_000).id(), BackendId::new("stub"));
}

#[test]
fn admit_delegates_to_check_admission_and_admits_a_small_request() {
    let req = request("hello", vec![]);
    let admission = backend(1_000_000).admit(&req, 8_192).unwrap();
    assert!(admission.fits());
    assert_eq!(admission.max_context_tokens, 1_000_000);
}

#[test]
fn admit_rejects_through_the_same_verdict_check_admission_produces_directly() {
    let req = request("hello", vec![]);
    let via_admit = backend(10).admit(&req, 8_192).unwrap_err();
    let est = estimate_tokens(&req);
    let via_direct = check_admission(req.model.clone(), est, 8_192, 10).unwrap_err();
    match (via_admit, via_direct) {
        (
            BackendError::ContextTooLarge {
                required_tokens: a, ..
            },
            BackendError::ContextTooLarge {
                required_tokens: b, ..
            },
        ) => assert_eq!(
            a, b,
            "admit must produce the same verdict check_admission does directly"
        ),
        other => panic!("expected ContextTooLarge on both sides: {other:?}"),
    }
}

#[tokio::test]
async fn generate_echoes_the_last_user_message() {
    let req = request("hello there", vec![]);
    let response = backend(200_000).generate(req).await.unwrap();
    assert_eq!(response.stop, StopReason::EndTurn);
    assert_eq!(
        response.content,
        vec![ContentBlock::Text {
            text: "echo: hello there".to_string()
        }]
    );
    assert!(response.tool_calls.is_empty());
}

#[tokio::test]
async fn generate_returns_a_tool_call_when_the_request_offers_one_and_asks_for_it() {
    let req = request(&format!("{TOOL_TRIGGER} now"), vec![lookup_tool()]);
    let response = backend(200_000).generate(req).await.unwrap();
    assert_eq!(response.stop, StopReason::ToolUse);
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, ToolName::new("lookup"));
}

#[tokio::test]
async fn stream_yields_a_delta_then_the_same_response_generate_would_return() {
    let req = request("hi", vec![]);
    let mut stream = backend(200_000).stream(req).await.unwrap();
    let mut chunks = Vec::new();
    loop {
        let next = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
        match next {
            Some(item) => chunks.push(item.unwrap()),
            None => break,
        }
    }
    assert_eq!(chunks.len(), 2);
    assert!(matches!(chunks[0], StreamChunk::TextDelta(_)));
    assert!(matches!(chunks[1], StreamChunk::Done(_)));
}

#[tokio::test]
async fn probe_reports_ok_with_no_network_access() {
    let report = backend(200_000).probe().await.unwrap();
    assert!(report.ok);
    assert_eq!(report.models, vec![ModelId::new("stub-model")]);
}
