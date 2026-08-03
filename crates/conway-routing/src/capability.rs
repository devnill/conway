//! The `(backend, model) -> Capabilities` lookup built at startup, and the
//! pure predicate the router uses for capability filtering (WI-032, amended:
//! headroom-aware context gate).
//!
//! Reconciliation with `conway-core` (documented choice): core's
//! `RequiredCaps::satisfied_by(&Capabilities, est_tokens)` already enforces
//! the same admission rule using the `headroom_tokens` field carried *inside*
//! `RequiredCaps`, with core's own message wording. Routing's skip reasons
//! are bound to the amendment's exact strings, and the router resolves the
//! effective headroom per role and passes it explicitly, so [`satisfies`]
//! here is a self-contained rendering of the same rule over four scalars.
//! The two formulations are pinned together by
//! `satisfies_agrees_with_core_on_accept_reject` below: for identical
//! inputs they must agree on ACCEPT vs REJECT (messages may differ; the
//! decision may not).

use std::collections::HashMap;
use std::sync::Arc;

use conway_core::capabilities::{
    Capabilities, ReliabilityTier, RequiredCaps, StructuredOutput, ToolCallSupport,
};
use conway_core::ids::{BackendId, ModelId, ModelRef};
use conway_core::ports::Backend;

/// Immutable `(backend, model) -> Capabilities` lookup. Built once at
/// startup; capability refresh is a rebuild (owned by the facade).
#[derive(Debug, Clone, Default)]
pub struct CapabilityIndex {
    map: HashMap<(BackendId, ModelId), Capabilities>,
}

/// Builder for [`CapabilityIndex`].
#[derive(Debug, Default)]
pub struct CapabilityIndexBuilder {
    map: HashMap<(BackendId, ModelId), Capabilities>,
}

impl CapabilityIndexBuilder {
    pub fn insert(
        mut self,
        backend: BackendId,
        model: ModelId,
        caps: Capabilities,
    ) -> CapabilityIndexBuilder {
        self.map.insert((backend, model), caps);
        self
    }

    pub fn build(self) -> CapabilityIndex {
        CapabilityIndex { map: self.map }
    }
}

impl CapabilityIndex {
    pub fn builder() -> CapabilityIndexBuilder {
        CapabilityIndexBuilder::default()
    }

    /// Reopens a built index as a [`CapabilityIndexBuilder`] so a caller can
    /// layer more entries on top (e.g. the facade's optional startup probe
    /// overlay) without re-querying every already-resolved pair.
    pub fn into_builder(self) -> CapabilityIndexBuilder {
        CapabilityIndexBuilder { map: self.map }
    }

    /// O(1) `HashMap` lookup — no scan.
    pub fn get(&self, model_ref: &ModelRef) -> Option<&Capabilities> {
        self.map
            .get(&(model_ref.backend.clone(), model_ref.model.clone()))
    }

    /// Builds the index by asking each backend for its capabilities, once
    /// per `(backend, model)` pair in `refs`. Refs whose backend id is not
    /// present in `backends` are silently omitted. Synchronous —
    /// `Backend::capabilities` performs no I/O.
    ///
    /// This is the *only* place the facade should populate a
    /// `CapabilityIndex` from real backends: routing this way (rather than
    /// recomputing `Capabilities` independently from the same source
    /// metadata) is what pins the router's admission decisions to exactly
    /// what `Backend::capabilities()` — and therefore
    /// `conway_runtime::attempt::AttemptEngine`'s T-1 gate — will actually
    /// see. A second, parallel `models.json` → `Capabilities` conversion is
    /// the divergence bug class WI-123 closes; don't reintroduce one.
    pub fn from_backends(backends: &[Arc<dyn Backend>], refs: &[ModelRef]) -> CapabilityIndex {
        let by_id: HashMap<BackendId, &Arc<dyn Backend>> =
            backends.iter().map(|b| (b.id(), b)).collect();
        let mut map = HashMap::new();
        for r in refs {
            if let Some(backend) = by_id.get(&r.backend) {
                map.entry((r.backend.clone(), r.model.clone()))
                    .or_insert_with(|| backend.capabilities(&r.model));
            }
        }
        CapabilityIndex { map }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Rank helpers over core's `#[non_exhaustive]` enums. Unknown future
/// variants rank as their documented-lowest peer; never panic.
fn structured_rank(s: &StructuredOutput) -> u8 {
    match s {
        StructuredOutput::None => 0,
        StructuredOutput::JsonSchema => 1,
        StructuredOutput::Grammar => 2,
        _ => 0,
    }
}

fn reliability_rank(t: &ReliabilityTier) -> u8 {
    match t {
        ReliabilityTier::Unknown => 0,
        ReliabilityTier::Community => 1,
        ReliabilityTier::Verified => 2,
        _ => 0,
    }
}

fn tool_support_name(t: &ToolCallSupport) -> &'static str {
    match t {
        ToolCallSupport::None => "None",
        ToolCallSupport::NonStreamingOnly => "NonStreamingOnly",
        ToolCallSupport::Streaming { validated: false } => "Streaming",
        ToolCallSupport::Streaming { validated: true } => "Streaming(validated)",
        _ => "Unknown",
    }
}

fn structured_name(s: &StructuredOutput) -> &'static str {
    match s {
        StructuredOutput::None => "None",
        StructuredOutput::JsonSchema => "JsonSchema",
        StructuredOutput::Grammar => "Grammar",
        _ => "Unknown",
    }
}

fn reliability_name(t: &ReliabilityTier) -> &'static str {
    match t {
        ReliabilityTier::Unknown => "Unknown",
        ReliabilityTier::Community => "Community",
        ReliabilityTier::Verified => "Verified",
        _ => "Unknown",
    }
}

/// THE headroom gate, in one place. `Some(shortfall)` when the request does
/// not fit this model's window; `None` when it fits.
///
/// All consumers of the gate call this rather than restating the formula:
/// [`satisfies`] (to decide whether to push its `"context: ..."` entry),
/// `router::DeclarativeRouter::check_candidate` (to decide whether a
/// rejection is attributable to context size alone, and therefore whether
/// `resolve` may return `RoutingError::ContextTooLarge`), and
/// `conway_runtime::attempt::AttemptEngine::execute` (the T-1 pre-flight
/// backstop that partitions candidate routes by whether the declared
/// window covers the prompt plus reserved headroom -- board item
/// 01KZ00VV3F3EBZ9WQSB292TBJZ, the founding case for P-14: a third
/// consumer was restating the arithmetic until that item). `pub` for the
/// cross-crate consumer; `pub(crate)` would hide it from `conway-runtime`.
///
/// Keeping it here is load-bearing, not tidiness. Board item
/// `01KYXNAHN64YMADZPQDQC0CPTJ` originally landed with this arithmetic
/// duplicated in `router.rs`. Nothing tied the two copies together, so a
/// later edit to one -- `>=` instead of `>`, a safety margin, different
/// rounding -- would have silently desynchronized them, and the failure is
/// quiet in both directions: a genuinely headroom-only rejection
/// misclassified as mixed (regressing P-9 back to `NoCandidate`, the exact
/// bug that item existed to fix), or a non-context failure misreported as
/// `ContextTooLarge`, which blames the window for an unrelated defect.
///
/// Saturating: `u32::MAX` inputs produce a rejection, never a wrap or panic.
/// Inclusive bound: `est_tokens + headroom == max_context_tokens` fits
/// (`None`) -- the same inclusive bound `AttemptEngine::execute` documented
/// for its own restated copy.
pub fn context_shortfall(
    caps: &Capabilities,
    est_tokens: u32,
    headroom_tokens: u32,
) -> Option<u32> {
    let needed = est_tokens.saturating_add(headroom_tokens);
    (needed > caps.max_context_tokens).then(|| needed.saturating_sub(caps.max_context_tokens))
}

/// Pure, synchronous admission predicate over four scalars: no `&self`, no
/// clock, no registry. Returns `Ok(())` (allocation-free) when every
/// requirement holds, else `Err` listing **every** unmet requirement in the
/// fixed order: tool_calling, structured_output, parallel_tool_calls,
/// reasoning, reliability_tier, min_context, context (headroom gate last).
///
/// The caller (the router, WI-034) resolves the effective headroom once per
/// role and passes it in — headroom policy lives in exactly one place.
pub fn satisfies(
    caps: &Capabilities,
    required: &RequiredCaps,
    est_tokens: u32,
    headroom_tokens: u32,
) -> Result<(), Vec<String>> {
    let mut missing: Vec<String> = Vec::new();

    if let Some(required_tc) = &required.tool_calling {
        if caps.tool_calling.rank() < required_tc.rank() {
            missing.push(format!(
                "tool_calling: requires {}, has {}",
                tool_support_name(required_tc),
                tool_support_name(&caps.tool_calling)
            ));
        }
    }

    if let Some(required_so) = &required.structured_output {
        if structured_rank(&caps.structured_output) < structured_rank(required_so) {
            missing.push(format!(
                "structured_output: requires {}, has {}",
                structured_name(required_so),
                structured_name(&caps.structured_output)
            ));
        }
    }

    if required.parallel_tool_calls == Some(true) && !caps.parallel_tool_calls {
        missing.push("parallel_tool_calls: required".to_string());
    }

    if required.reasoning == Some(true) && !caps.reasoning {
        missing.push("reasoning: required".to_string());
    }

    if let Some(required_tier) = &required.min_reliability {
        if reliability_rank(&caps.reliability_tier) < reliability_rank(required_tier) {
            missing.push(format!(
                "reliability_tier: requires {}, has {}",
                reliability_name(required_tier),
                reliability_name(&caps.reliability_tier)
            ));
        }
    }

    if let Some(min_context) = required.min_context {
        if caps.max_context_tokens < min_context {
            missing.push(format!(
                "min_context: requires >= {min_context}, has {}",
                caps.max_context_tokens
            ));
        }
    }

    // The headroom gate, last (amendment ordering). The predicate itself
    // lives in `context_shortfall` so the router's headroom-only
    // classification cannot drift from it.
    if context_shortfall(caps, est_tokens, headroom_tokens).is_some() {
        let needed = est_tokens.saturating_add(headroom_tokens);
        missing.push(format!(
            "context: needs {est_tokens} input + {headroom_tokens} headroom = {needed}, \
             model max_context_tokens is {}",
            caps.max_context_tokens
        ));
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::capabilities::CacheMode;
    use conway_core::fakes::FakeBackend;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static_assertions::assert_impl_all!(CapabilityIndex: Send, Sync, Clone);

    /// Pins the invariant `DeclarativeRouter::check_candidate` depends on to
    /// classify a rejection as headroom-only: a failing headroom gate
    /// contributes EXACTLY ONE entry to `missing`, so `missing.len() == 1`
    /// combined with `context_shortfall(..).is_some()` means "the headroom
    /// gate failed and nothing else did".
    ///
    /// Without this, the router's classification rests on an unstated
    /// property of this function. If someone splits the context entry into
    /// two strings, this test fails here rather than silently turning every
    /// headroom-only rejection back into a `NoCandidate` (board item
    /// `01KYXNAHN64YMADZPQDQC0CPTJ`, P-9).
    #[test]
    fn a_headroom_only_failure_contributes_exactly_one_missing_entry() {
        let fits_everything_but_context = caps(40_000);
        let required = RequiredCaps::default();

        let missing = satisfies(&fits_everything_but_context, &required, 34_000, 16_000)
            .expect_err("50000 needed against a 40000 window must be rejected");

        assert_eq!(
            missing.len(),
            1,
            "a headroom-only failure must contribute exactly one entry, got: {missing:?}"
        );
        assert!(
            missing[0].starts_with("context:"),
            "the sole entry must be the context one, got: {}",
            missing[0]
        );
        assert!(
            context_shortfall(&fits_everything_but_context, 34_000, 16_000).is_some(),
            "context_shortfall must agree that this is a headroom failure"
        );
    }

    /// The complement: when the headroom gate passes, `context_shortfall`
    /// reports nothing and no `context:` entry appears — so a single-entry
    /// `missing` caused by some OTHER requirement can never be mistaken for
    /// a headroom-only rejection.
    #[test]
    fn a_non_context_failure_is_never_classified_as_headroom() {
        let roomy_but_weak = weak_caps();
        let required = RequiredCaps {
            reasoning: Some(true),
            ..RequiredCaps::default()
        };

        let missing = satisfies(&roomy_but_weak, &required, 10, 10)
            .expect_err("weak_caps has reasoning=false, so this must be rejected");

        assert!(
            !missing.iter().any(|m| m.starts_with("context:")),
            "the window is ample here; no context entry expected, got: {missing:?}"
        );
        assert!(
            context_shortfall(&roomy_but_weak, 10, 10).is_none(),
            "context_shortfall must not report a shortfall when the window is ample"
        );
    }

    fn caps(max_context_tokens: u32) -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            cache: CacheMode::None,
            parallel_tool_calls: true,
            structured_output: StructuredOutput::Grammar,
            max_context_tokens,
            reasoning: true,
            reliability_tier: ReliabilityTier::Verified,
        }
    }

    fn weak_caps() -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::None,
            cache: CacheMode::None,
            parallel_tool_calls: false,
            structured_output: StructuredOutput::None,
            max_context_tokens: 32_768,
            reasoning: false,
            reliability_tier: ReliabilityTier::Community,
        }
    }

    fn demanding() -> RequiredCaps {
        RequiredCaps {
            tool_calling: Some(ToolCallSupport::NonStreamingOnly),
            structured_output: Some(StructuredOutput::JsonSchema),
            parallel_tool_calls: Some(true),
            reasoning: Some(true),
            min_reliability: Some(ReliabilityTier::Verified),
            min_context: Some(40_000),
            ..RequiredCaps::default()
        }
    }

    #[test]
    fn builder_insert_get_and_unknown_ref() {
        let index = CapabilityIndex::builder()
            .insert(BackendId::new("local"), ModelId::new("m1"), caps(1000))
            .build();
        let hit = index.get(&"local/m1".parse().unwrap());
        assert_eq!(hit.map(|c| c.max_context_tokens), Some(1000));
        assert!(index.get(&"local/other".parse().unwrap()).is_none());
        assert!(index.get(&"remote/m1".parse().unwrap()).is_none());
    }

    /// Counting backend: delegates capability values, counts calls.
    struct CountingBackend {
        id: BackendId,
        inner: FakeBackend,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Backend for CountingBackend {
        fn id(&self) -> BackendId {
            self.id.clone()
        }
        fn capabilities(&self, model: &ModelId) -> Capabilities {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.capabilities(model)
        }
        async fn generate(
            &self,
            req: conway_core::ports::GenerateRequest,
        ) -> Result<conway_core::ports::GenerateResponse, conway_core::error::BackendError>
        {
            self.inner.generate(req).await
        }
        async fn stream(
            &self,
            req: conway_core::ports::GenerateRequest,
        ) -> Result<
            conway_core::ports::BoxStream<
                'static,
                Result<conway_core::ports::StreamChunk, conway_core::error::BackendError>,
            >,
            conway_core::error::BackendError,
        > {
            self.inner.stream(req).await
        }
        async fn probe(
            &self,
        ) -> Result<conway_core::capabilities::ProbeReport, conway_core::error::BackendError>
        {
            self.inner.probe().await
        }
    }

    #[test]
    fn into_builder_preserves_existing_entries_for_further_layering() {
        let index = CapabilityIndex::builder()
            .insert(BackendId::new("local"), ModelId::new("m1"), caps(1000))
            .build();
        let rebuilt = index
            .into_builder()
            .insert(BackendId::new("local"), ModelId::new("m2"), caps(2000))
            .build();
        assert_eq!(
            rebuilt
                .get(&"local/m1".parse().unwrap())
                .map(|c| c.max_context_tokens),
            Some(1000)
        );
        assert_eq!(
            rebuilt
                .get(&"local/m2".parse().unwrap())
                .map(|c| c.max_context_tokens),
            Some(2000)
        );
    }

    #[test]
    fn from_backends_calls_once_per_pair_and_omits_absent_backends() {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend: Arc<dyn Backend> = Arc::new(CountingBackend {
            id: BackendId::new("local"),
            inner: FakeBackend::with_capabilities(caps(1000)),
            calls: Arc::clone(&calls),
        });
        let refs: Vec<ModelRef> = vec![
            "local/m1".parse().unwrap(),
            "local/m2".parse().unwrap(),
            "local/m1".parse().unwrap(), // duplicate: must not re-query
            "absent/m3".parse().unwrap(),
        ];
        let index = CapabilityIndex::from_backends(&[backend], &refs);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "once per unique pair");
        assert_eq!(index.len(), 2);
        assert!(index.get(&"absent/m3".parse().unwrap()).is_none());
    }

    #[test]
    fn ordering_lists_every_unmet_requirement_context_last() {
        let err = satisfies(&weak_caps(), &demanding(), 34_000, 16_000).unwrap_err();
        let prefixes: Vec<&str> = err.iter().map(|s| s.split(':').next().unwrap()).collect();
        assert_eq!(
            prefixes,
            vec![
                "tool_calling",
                "structured_output",
                "parallel_tool_calls",
                "reasoning",
                "reliability_tier",
                "min_context",
                "context"
            ]
        );
    }

    #[test]
    fn golden_strings_match_exactly() {
        let err = satisfies(&weak_caps(), &demanding(), 34_000, 16_000).unwrap_err();
        assert_eq!(err[0], "tool_calling: requires NonStreamingOnly, has None");
        assert_eq!(err[1], "structured_output: requires JsonSchema, has None");
        assert_eq!(err[2], "parallel_tool_calls: required");
        assert_eq!(err[3], "reasoning: required");
        assert_eq!(err[4], "reliability_tier: requires Verified, has Community");
        assert_eq!(err[5], "min_context: requires >= 40000, has 32768");
        assert_eq!(
            err[6],
            "context: needs 34000 input + 16000 headroom = 50000, \
             model max_context_tokens is 32768"
        );
    }

    #[test]
    fn golden_context_string_verbatim_per_amendment() {
        // The amendment's exact fixture: max 40000, est 34000, headroom 16000.
        let err = satisfies(&caps(40_000), &RequiredCaps::default(), 34_000, 16_000).unwrap_err();
        assert_eq!(
            err,
            vec!["context: needs 34000 input + 16000 headroom = 50000, \
                 model max_context_tokens is 40000"
                .to_string()]
        );
    }

    #[test]
    fn headroom_rejects_candidate_that_fits_raw_input() {
        // Fits the raw input (34000 <= 40000) but not input + headroom.
        assert!(satisfies(&caps(40_000), &RequiredCaps::default(), 34_000, 16_000).is_err());
        assert!(satisfies(&caps(40_000), &RequiredCaps::default(), 34_000, 0).is_ok());
    }

    #[test]
    fn boundary_exact_fit_passes_one_over_fails() {
        assert!(satisfies(&caps(50_000), &RequiredCaps::default(), 34_000, 16_000).is_ok());
        assert!(satisfies(&caps(49_999), &RequiredCaps::default(), 34_000, 16_000).is_err());
    }

    #[test]
    fn saturating_arithmetic_never_panics() {
        // Saturation, not wrap: at a u32::MAX window the saturated sum
        // equals the window, so the gate admits — the point is no panic
        // and no wrap-to-small-number false admission.
        let ok = satisfies(&caps(u32::MAX), &RequiredCaps::default(), u32::MAX, 16_000);
        assert!(ok.is_ok(), "saturated sum == window is admissible");
        let err = satisfies(&caps(100), &RequiredCaps::default(), u32::MAX, u32::MAX);
        assert!(err.is_err(), "saturated sum still rejects a real window");
    }

    #[test]
    fn min_context_and_headroom_gate_are_independent() {
        let required = RequiredCaps {
            min_context: Some(200_000),
            ..RequiredCaps::default()
        };
        let err = satisfies(&caps(50_000), &required, 40_000, 16_000).unwrap_err();
        assert_eq!(err.len(), 2);
        assert!(err[0].starts_with("min_context:"));
        assert!(err[1].starts_with("context:"));
    }

    /// Reconciliation pin: routing's `satisfies` and core's
    /// `RequiredCaps::satisfied_by` must agree on ACCEPT vs REJECT for
    /// identical inputs (messages may differ; the decision may not). Core
    /// reads headroom from `required.headroom_tokens`, so the pin sets that
    /// field to the value passed to `satisfies`. `structured_output` is
    /// held to `None` here: core uses equality where routing uses rank
    /// ordering (a documented, deliberate difference in requirement
    /// semantics, not in admission arithmetic).
    #[test]
    fn satisfies_agrees_with_core_on_accept_reject() {
        let windows = [10_000u32, 32_768, 50_000, 200_000];
        let inputs = [
            (0u32, 0u32),
            (30_000, 8_192),
            (34_000, 16_000),
            (200_000, 0),
        ];
        for window in windows {
            for (est, headroom) in inputs {
                let required = RequiredCaps {
                    headroom_tokens: headroom,
                    ..RequiredCaps::default()
                };
                let candidate = caps(window);
                let routing_ok = satisfies(&candidate, &required, est, headroom).is_ok();
                let core_ok = required.satisfied_by(&candidate, est).is_ok();
                assert_eq!(
                    routing_ok, core_ok,
                    "decision drift at window={window} est={est} headroom={headroom}"
                );
            }
        }
    }
}
