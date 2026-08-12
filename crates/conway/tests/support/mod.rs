//! Shared helpers for the `config_*` integration tests: fixture path
//! resolution and unique scratch directories (no external tempfile crate
//! dependency — each test gets its own subdirectory under
//! `std::env::temp_dir()`, disambiguated by an atomic counter so parallel
//! test threads never collide).

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use conway_core::capabilities::{Capabilities, ProbeReport};
use conway_core::error::BackendError;
use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{Backend, BoxStream, GenerateRequest, GenerateResponse, StreamChunk};

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config")
}

pub fn unique_temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "conway-config-test-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create unique temp dir");
    dir
}

/// The hermetic `env` map every `config::load` call in this crate's tests
/// should pass instead of a bare `HashMap::new()`.
///
/// `merge::load` always consults the XDG layer, regardless of
/// `explicit_path` (it is merged in *before* the explicit/project layer, not
/// instead of it -- see `merge::load`'s own body). `discovery::xdg_config_path`
/// falls back to the invoking user's real `~/.conway/settings.json` whenever
/// `XDG_CONFIG_HOME` is absent from the `env` map. So a test that builds
/// `HashMap::new()` and calls `load` does not test "no XDG layer" -- it
/// tests "whatever happens to be on the machine running the suite," CI or a
/// developer's laptop alike.
///
/// This is not hypothetical (board item 01KZVQCQWFMH90APGFTXBAMEVC): eight
/// `config_headroom.rs` tests built exactly this way went red on one
/// laptop, and only that laptop, when an unrelated change removed four
/// `[health]` keys that the operator's real `settings.json` happened to
/// name -- `HealthSection`'s `#[serde(deny_unknown_fields)]` rejected it.
/// Nothing in the repository explained the failure.
///
/// Points `XDG_CONFIG_HOME` at a fresh scratch directory (via
/// [`unique_temp_dir`]) that this call is the sole owner of and that never
/// contains a `conway/settings.json` -- so the XDG layer resolves to
/// "absent" deterministically, the same outcome `HashMap::new()` was
/// presumably intended to produce. A test that genuinely wants to exercise
/// the XDG layer itself builds its own `env` map naming `XDG_CONFIG_HOME`
/// explicitly (as `config_precedence.rs`'s five-source test and several
/// `config_headroom.rs` tests already do) rather than calling this helper --
/// this is the one hermetic idiom for "I don't care about the XDG layer,"
/// not a second competing pattern.
pub fn isolated_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        unique_temp_dir("isolated-xdg-home")
            .to_string_lossy()
            .to_string(),
    );
    env
}

/// A `Backend` double whose `capabilities()` can be mutated *after*
/// construction — and, therefore, after `ConwayBuilder::build()` has
/// already taken its one-time `CapabilityIndex::from_backends` snapshot.
/// Every other in-tree `Backend` double bakes its capabilities in at
/// construction (`conway_core::fakes::FakeBackend::with_capabilities`,
/// `ScriptedBackend::with_capabilities`), which cannot express the one
/// divergence the architecture still permits: the router's index is a
/// point-in-time read (`builder.rs` step 5), while
/// `conway_runtime::attempt::AttemptEngine`'s T-1 gate re-reads
/// `Backend::capabilities()` live on every attempt. This double exists to
/// witness exactly that gap, and nothing else.
pub struct MutableCapsBackend {
    id: BackendId,
    caps: Mutex<Capabilities>,
    response: GenerateResponse,
}

impl MutableCapsBackend {
    pub fn new(id: BackendId, caps: Capabilities, response: GenerateResponse) -> Self {
        Self {
            id,
            caps: Mutex::new(caps),
            response,
        }
    }

    /// Overwrites the capabilities every subsequent `capabilities()` call
    /// returns.
    pub fn set_capabilities(&self, caps: Capabilities) {
        *self.caps.lock().unwrap() = caps;
    }
}

#[async_trait]
impl Backend for MutableCapsBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        self.caps.lock().unwrap().clone()
    }

    async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        Ok(self.response.clone())
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.generate(req).await?;
        Ok(Box::pin(futures_util_free_stream(vec![StreamChunk::Done(
            response,
        )])))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        Ok(ProbeReport {
            ok: true,
            latency_ms: 1,
            models: vec![],
            detail: None,
            at: chrono::Utc::now(),
        })
    }
}

/// A minimal, `Send`, already-ready `Stream` over a fixed `Vec<StreamChunk>`
/// — `conway-core`'s own `VecStream` (in `fakes.rs`) is private to that
/// module, and this crate has no dependency on `futures`/`futures-util` to
/// reach for a combinator instead.
fn futures_util_free_stream(
    items: Vec<StreamChunk>,
) -> impl futures_core::Stream<Item = Result<StreamChunk, BackendError>> + Send + 'static {
    struct Once(std::collections::VecDeque<StreamChunk>);
    impl futures_core::Stream for Once {
        type Item = Result<StreamChunk, BackendError>;
        fn poll_next(
            self: core::pin::Pin<&mut Self>,
            _cx: &mut core::task::Context<'_>,
        ) -> core::task::Poll<Option<Self::Item>> {
            core::task::Poll::Ready(self.get_mut().0.pop_front().map(Ok))
        }
    }
    Once(items.into())
}
