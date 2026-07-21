//! The `Backend` port: one adapter per LLM provider dialect (architecture
//! §4.1).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::capabilities::{Capabilities, ProbeReport};
use crate::content::{ContentBlock, SamplingParams, StopReason, ToolCall, ToolSpec, Usage};
use crate::error::BackendError;
use crate::ids::{BackendId, ModelId, PrefixKey};
use crate::segment::PromptSegment;

/// A boxed, `Send` stream of `T`.
///
/// Defined locally over `futures_core::Stream` so this crate does not need
/// `futures`/`futures-util` as a dependency — `conway-core` performs no I/O
/// and the combinator ecosystem those crates provide is unneeded here.
pub type BoxStream<'a, T> = core::pin::Pin<Box<dyn futures_core::Stream<Item = T> + Send + 'a>>;

/// One adapter for one LLM provider dialect (e.g. Anthropic, an
/// OpenAI-compatible endpoint). Implementations live in `conway-backends`.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// This backend instance's configured identity.
    fn id(&self) -> BackendId;

    /// Capabilities are per `(backend, model)`, not per-backend: quantization
    /// and chat template change tool-call reliability independent of the
    /// server.
    fn capabilities(&self, model: &ModelId) -> Capabilities;

    /// Generate a complete (non-streamed) response.
    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError>;

    /// Generate a streamed response.
    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError>;

    /// Cheap liveness/readiness probe. Distinct from transport errors
    /// encountered during `generate`/`stream`.
    async fn probe(&self) -> Result<ProbeReport, BackendError>;
}

/// A request to generate a response from one model.
///
/// Every field is producer-owned; adapters may reorder nothing (architecture
/// §8).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub model: ModelId,
    /// Order is load-bearing for implicit-prefix caching (architecture
    /// §5.3: static → inherited → volatile). Adapters MUST NOT reorder,
    /// merge, or drop segments.
    pub segments: Vec<PromptSegment>,
    pub tools: Vec<ToolSpec>,
    pub params: SamplingParams,
    /// Reserved for `CacheMode::SlotKv`; adapters that do not support slots
    /// ignore it.
    pub prefix_key: Option<PrefixKey>,
}

/// The result of a completed (non-streamed) generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerateResponse {
    pub content: Vec<ContentBlock>,
    /// Already validated against the requested `ToolSpec` schemas.
    pub tool_calls: Vec<ToolCall>,
    pub stop: StopReason,
    /// Includes `cache_read_tokens`/`cache_write_tokens` when the backend
    /// reports them.
    pub usage: Usage,
}

/// One chunk of a streamed generation.
///
/// Externally tagged (serde's default enum representation): an internal tag
/// (`#[serde(tag = "type")]`) cannot represent the newtype variants
/// (`TextDelta(String)`, `ThinkingDelta(String)`) here, since serde has no
/// way to merge a bare string payload into a tagged object.
#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamChunk {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallDelta { index: u32, raw: String },
    Done(GenerateResponse),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{Role, StopReason};
    use crate::provenance::Provenance;

    fn sample_segment() -> PromptSegment {
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text { text: "hi".into() }],
            Provenance::UserPrompt,
        )
    }

    #[test]
    fn generate_request_round_trips_and_preserves_segment_order() {
        let req = GenerateRequest {
            model: ModelId::new("claude-sonnet-4-6"),
            segments: vec![sample_segment(), sample_segment()],
            tools: vec![],
            params: SamplingParams::default(),
            prefix_key: Some(PrefixKey::from_blake3(blake3::hash(b"x"))),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: GenerateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.segments.len(), req.segments.len());
        assert_eq!(
            back.segments.iter().map(|s| s.id).collect::<Vec<_>>(),
            req.segments.iter().map(|s| s.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn stream_chunk_variants_round_trip() {
        let done = StreamChunk::Done(GenerateResponse {
            content: vec![],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage::default(),
        });
        let cases = vec![
            StreamChunk::TextDelta("hi".into()),
            StreamChunk::ThinkingDelta("hmm".into()),
            StreamChunk::ToolCallDelta {
                index: 0,
                raw: "{}".into(),
            },
            done,
        ];
        for chunk in cases {
            let json = serde_json::to_string(&chunk).unwrap();
            let _back: StreamChunk = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn box_stream_type_is_usable() {
        use futures_core::Stream;

        struct Empty;
        impl Stream for Empty {
            type Item = Result<StreamChunk, BackendError>;
            fn poll_next(
                self: core::pin::Pin<&mut Self>,
                _cx: &mut core::task::Context<'_>,
            ) -> core::task::Poll<Option<Self::Item>> {
                core::task::Poll::Ready(None)
            }
        }

        let _s: BoxStream<'static, Result<StreamChunk, BackendError>> = Box::pin(Empty);
    }
}
