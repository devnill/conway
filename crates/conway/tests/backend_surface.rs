//! Pins `conway::backend`'s curated export list , mirroring `public_api_surface.rs`'s own
//! "must be nameable" idiom for the crate root.
//!
//! `backend_parity.rs` proves the set is *sufficient* by implementing a
//! real `Backend` with it, which also happens to use every name here. This
//! file is the independent, minimal pin: if a future edit ever narrowed
//! that parity test in a way that stopped using one of these names, this
//! file still catches the export's removal on its own.

use conway::backend::{
    async_trait, check_admission, Admission, Backend, BackendError, BackendId, BoxStream,
    CacheMode, Capabilities, ContentBlock, GenerateRequest, GenerateResponse, ModelId, PrefixKey,
    ProbeReport, PromptSegment, ReliabilityTier, SamplingParams, StopReason, StreamChunk,
    StructuredOutput, ToolCall, ToolCallSupport, ToolSpec, Usage,
};

/// Every re-exported *type* in `conway::backend` must be nameable at this
/// path. Never called: the compiler type-checking the signature is the
/// assertion (mirrors `public_api_surface.rs::assert_types_nameable`).
#[allow(dead_code, clippy::too_many_arguments)]
fn assert_types_nameable(
    _: Option<BackendId>,
    _: Option<ModelId>,
    _: Option<Capabilities>,
    _: Option<ToolCallSupport>,
    _: Option<CacheMode>,
    _: Option<StructuredOutput>,
    _: Option<ReliabilityTier>,
    _: Option<GenerateRequest>,
    _: Option<GenerateResponse>,
    _: Option<StreamChunk>,
    _: Option<ProbeReport>,
    _: Option<BackendError>,
    _: Option<Admission>,
    _: Option<SamplingParams>,
    _: Option<PrefixKey>,
    _: Option<StopReason>,
    _: Option<Usage>,
    _: Option<ContentBlock>,
    _: Option<ToolCall>,
    _: Option<ToolSpec>,
    _: Option<PromptSegment>,
) {
}

/// The re-exported `Backend` trait must be nameable and usable as a trait
/// object at this path — already pinned at the root in
/// `public_api_surface.rs::assert_traits_object_safe`; pinned again here
/// against `conway::backend::Backend` specifically, since that is the
/// second, independent path this module promises.
#[allow(dead_code)]
fn assert_backend_trait_object_safe(_: &dyn Backend) {}

/// `BoxStream` is a lifetime-parameterized type alias, not a plain type —
/// it gets its own nameability check rather than folding into
/// `assert_types_nameable`'s `Option<T>` pattern.
#[allow(dead_code)]
fn assert_box_stream_nameable(_: BoxStream<'static, Result<StreamChunk, BackendError>>) {}

/// `check_admission` is a free function, not a type: naming it as a value
/// of its own function-pointer type is the equivalent assertion.
#[allow(dead_code)]
fn assert_check_admission_nameable() {
    let _: fn(ModelId, u32, u32, u32) -> Result<Admission, BackendError> = check_admission;
}

/// `async_trait` is the attribute macro `Backend` itself is transformed
/// with; naming it here and applying it to a throwaway trait is the
/// assertion that it, too, is reachable through `conway::backend`.
#[allow(dead_code)]
#[async_trait]
trait AsyncTraitIsReachableThroughBackend {
    async fn noop(&self) {}
}

#[test]
fn backend_surface_present() {
    // The assertion is that this file compiles: every name in the `use`
    // statement above resolved, and the signatures/impls above type-checked.
}
