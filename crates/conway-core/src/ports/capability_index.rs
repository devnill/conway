//! The `(backend, model) -> Capabilities` lookup built at startup.
//!
//! Lives beside the `Backend` port (,
//! "the backend side"): [`CapabilityIndex::from_backends`] reads directly
//! from `Backend::capabilities`, and nothing about this type is
//! routing-policy-specific -- it is a plain projection over whatever
//! backends a caller hands it. Previously lived in `conway-routing`'s
//! `capability.rs`; moved here once that module's own reason for owning it
//! (bundling it alongside the headroom-gate predicate, `satisfies`, which
//! stays in `conway-routing` since it is genuinely routing policy) stopped
//! applying. `conway-routing::CapabilityIndex` / `CapabilityIndexBuilder`
//! remain usable as re-exports of these same types (see that crate's
//! `lib.rs`), so no downstream import changes.

use std::collections::HashMap;
use std::sync::Arc;

use crate::capabilities::Capabilities;
use crate::ids::{BackendId, ModelId, ModelRef};

use super::Backend;

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
    /// present in `backends` are silently omitted. Synchronous --
    /// `Backend::capabilities` performs no I/O.
    ///
    /// This is the *only* place a caller should populate a
    /// `CapabilityIndex` from real backends: routing this way (rather than
    /// recomputing `Capabilities` independently from the same source
    /// metadata) is what pins a router's admission decisions to exactly
    /// what `Backend::capabilities()` -- and therefore
    /// `conway_runtime::attempt::AttemptEngine`'s own `Backend::admit`-based
    /// gate -- will actually see. A second, parallel `models.json` ->
    /// `Capabilities` conversion is the divergence bug class an earlier item closes;
    /// don't reintroduce one.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{CacheMode, ReliabilityTier, StructuredOutput, ToolCallSupport};
    use crate::error::BackendError;
    use crate::ports::{BoxStream, GenerateRequest, GenerateResponse, StreamChunk};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static_assertions::assert_impl_all!(CapabilityIndex: Send, Sync, Clone);

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

    /// A minimal `Backend` double that counts `capabilities()` calls and
    /// returns a fixed value. Implemented directly (not via
    /// `conway_core::fakes::FakeBackend`, which requires the `fakes`
    /// feature) so this test compiles under a plain `cargo test -p
    /// conway-core` with no extra feature flag -- matching
    /// `backend.rs`'s own `DefaultAdmitBackend` test-helper pattern.
    struct CountingBackend {
        id: BackendId,
        caps: Capabilities,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Backend for CountingBackend {
        fn id(&self) -> BackendId {
            self.id.clone()
        }
        fn capabilities(&self, _model: &ModelId) -> Capabilities {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.caps.clone()
        }
        async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
            unimplemented!("not exercised by this test")
        }
        async fn stream(
            &self,
            _req: GenerateRequest,
        ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
            unimplemented!("not exercised by this test")
        }
        async fn probe(&self) -> Result<crate::capabilities::ProbeReport, BackendError> {
            unimplemented!("not exercised by this test")
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
            caps: caps(1000),
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
}
