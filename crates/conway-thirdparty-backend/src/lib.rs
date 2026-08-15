//! A third-party-shaped `Backend` + `BackendFactory` -- the proof that a stranger outside this
//! repository can genuinely author a provider adapter, install it the way
//! a stranger would install one, and answer a real turn -- not merely that
//! the machinery for doing so compiles inside `conway`'s own test suite.
//!
//! ## Two distinct properties, not one test
//!
//! **1. Authoring parity, guarded at build time.** This crate's own
//! `[dependencies]` (`Cargo.toml`) name exactly ONE workspace crate:
//! `conway`, the public facade. There is no `conway-core` dependency to
//! stray into at all -- not "by convention", not "by a lint", but because
//! Cargo cannot resolve `conway_core::anything` in this crate's source: no
//! such crate is declared, so `use conway_core::...` anywhere below would
//! be a hard `error[E0433]: failed to resolve: use of undeclared crate or
//! module `conway_core``, exactly the "shrink is a COMPILE failure, not a
//! runtime assertion" property the item's own acceptance criteria demand.
//! This is the stronger of the two options the item's spec left open (a
//! test file inside `crates/conway/tests/` would sit in a crate whose
//! *dev*-dependency graph already includes `conway-core` and
//! `conway-plugin-backends`, so a stray import there would compile and
//! silently weaken the proof -- see `crates/conway/tests/backend_parity.rs`'s
//! own doc comment for how that file polices itself instead: an explicit
//! rule stated in prose, not a structural impossibility). This crate mirrors
//! `crates/conway-plugin-skeleton`'s identical choice for the `Plugin`/`Tool`
//! authoring surface, one extension point over.
//!
//! `ThirdPartyBackend`/`ThirdPartyBackendFactory` below are written using
//! only `conway::backend::*`/`conway::*` imports -- see each item's own
//! doc comment for exactly which name it needs and why.
//!
//! **2. Installation and service, end to end.** [`fixture::write_settings`]
//! renders a real `settings.json` (plus the `.conway/models.json` every
//! `[backends.<id>]` entry needs a routable model declared in) naming
//! `kind = "thirdparty-stub"`; [`build_conway`] loads it through
//! `conway::config::load` (the same five-source loader a real `conway`
//! invocation uses) and installs `ThirdPartyBackendFactory` through
//! `ConwayBuilder::with_backend_factory` -- the identical public builder
//! channel `conway-plugin-backends`' own
//! `tests/builder_end_to_end.rs` installs the shipped `openai-compat`
//! factory through (first-party and third-party share one
//! mechanism). `tests/end_to_end.rs` asserts on the completed turn's own
//! **returned text**, not on `ThirdPartyBackendFactory::build` having been
//! called -- a factory invoked and discarded produces the same call count
//! as one that works (the item's own warning); only the text distinguishes
//! them.
//!
//! ## Credential-free and network-free, without exception
//!
//! `ThirdPartyBackend::generate`/`::stream`/`::probe` perform no I/O of any
//! kind -- there is no real dialect to reach, so this crate does not
//! borrow `conway-testkit`'s doubles (reachable only via `conway`'s own
//! `testkit` feature, which this crate does not enable -- there is no
//! `conway-testkit` dependency here at all) or `wiremock`. It
//! writes its own trivial, hand-rolled responses, which is itself part of
//! the proof that the public surface is sufficient: nothing in
//! `conway::backend` was needed to make that decision for it.
//!
//! `fixture::write_settings`'s rendered config sets `permissions.mode =
//! "allowlist"` and `[build_conway]` points `XDG_CONFIG_HOME` at the
//! fixture's own temp directory (which has no `conway/settings.json`
//! inside it) -- so a real `~/.conway/settings.json` on the machine
//! running this test (verified present on at least one development
//! machine at the time this crate was written) can never merge into and
//! corrupt these fixtures, the identical isolation
//! `crates/conway-cli/tests/common/mod.rs`'s own `command` helper already
//! documents for the compiled-binary suite one crate over.

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use futures_core::Stream;

use conway::backend::{
    async_trait, check_admission, Admission, Backend, BackendError, BackendId, BoxStream,
    CacheMode, Capabilities, ContentBlock, GenerateRequest, GenerateResponse, ModelId, ProbeReport,
    ReliabilityTier, StopReason, StreamChunk, StructuredOutput, ToolCallSupport, Usage,
};
use conway::{BackendBuildContext, BackendFactory, CoreConwayError, ModelOverrides};

/// The `[backends.<id>]` JSON key `fixture::write_settings` renders, and
/// the `Backend::id()` `ThirdPartyBackendFactory::build` gives back.
pub const BACKEND_ID: &str = "thirdparty";
/// The `kind` `fixture::write_settings` names -- an open string
/// (`ThirdPartyBackendFactory::id()` returns the identical value), never a
/// closed enum variant.
pub const BACKEND_KIND: &str = "thirdparty-stub";
/// The model id `fixture::write_settings`'s `roles.coder.chain` and
/// `.conway/models.json` both name.
pub const MODEL_ID: &str = "stub-model";
/// `ThirdPartyBackend::respond`'s canned reply -- the exact string
/// `tests/end_to_end.rs` and `src/bin/thirdparty_backend_demo.rs` both
/// assert the completed turn's text equals. Deliberately mentions neither
/// "mock" nor "fake": this really is the backend's own, real, hand-written
/// answer, not a stand-in for one. Given back only when the entry set no
/// [`GREETING_KEY`] -- see that constant's own doc for the reply this
/// backend gives instead when one is set.
pub const REPLY_TEXT: &str =
    "hello from the third-party backend, installed through settings.json alone";

/// The `[backends.<id>]` custom key this stand-in reads out of
/// [`BackendBuildContext::extra`] --
/// this crate's proof that a third-party kind's own configuration genuinely
/// reaches its factory, rather than being captured at config-load time and
/// discarded before any factory ever sees it (`docs/providers.md`'s
/// "Writing your own adapter" section used to promise the former and
/// deliver the latter; this is what makes the promise true).
/// `fixture::write_settings_with_greeting` is what sets it;
/// `ThirdPartyBackendFactory::build` is what reads it;
/// `ThirdPartyBackend::respond`'s reply text is where the value becomes
/// observable -- `tests/custom_key.rs` asserts on that reply text, not on
/// `extra` merely containing the key, and removing the wiring anywhere
/// along that chain makes it fail.
pub const GREETING_KEY: &str = "greeting";

/// A complete, in-process [`Backend`]: no network I/O, deterministic,
/// hand-written -- exactly the shape a real stranger's own adapter for an
/// in-process or otherwise network-free dialect would take, and the shape
/// this crate's own module doc explains it deliberately is not borrowing
/// from `conway-testkit` for.
pub struct ThirdPartyBackend {
    id: BackendId,
    max_context_tokens: u32,
    /// `ctx.extra.get(GREETING_KEY)`'s string value, read once by
    /// `ThirdPartyBackendFactory::build` -- `None` when the entry set no
    /// `greeting` key, in which case `respond()` gives back [`REPLY_TEXT`]
    /// unchanged, exactly as it did before this field existed.
    greeting: Option<String>,
}

/// The dialect-neutral request-size estimate `ThirdPartyBackend::admit`
/// hands to [`check_admission`] -- mirrors
/// `crates/conway/tests/backend_parity.rs`'s own `estimate_tokens` (this
/// crate cannot see `conway-core`'s own `default_estimate_tokens`, the same
/// reason that file restates one instead of reusing it).
fn estimate_tokens(req: &GenerateRequest) -> u32 {
    let mut total: u32 = 0;
    for segment in &req.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                total =
                    total.saturating_add(u32::try_from(text.len()).unwrap_or(u32::MAX).div_ceil(4));
            }
        }
    }
    total.saturating_add(
        u32::try_from(req.tools.len())
            .unwrap_or(u32::MAX)
            .saturating_mul(16),
    )
}

impl ThirdPartyBackend {
    /// The one real response this backend ever gives, regardless of what
    /// `req` asked -- deliberately ignores `req.tools`/`req.segments`
    /// entirely (this stub never calls a tool), so the same text always
    /// comes back through `generate`/`stream` alike. What varies is
    /// `self.greeting`: `REPLY_TEXT` verbatim when it is `None`, a reply
    /// naming the value when it is `Some` -- the observable this crate's
    /// `tests/custom_key.rs` asserts on to prove `BackendBuildContext::
    /// extra` reached this factory-built instance.
    fn respond(&self) -> GenerateResponse {
        let text = match &self.greeting {
            Some(greeting) => format!(
                "hello, {greeting} -- from the third-party backend, installed through \
                 settings.json alone, with a custom `greeting` key read from \
                 BackendBuildContext::extra"
            ),
            None => REPLY_TEXT.to_string(),
        };
        GenerateResponse {
            content: vec![ContentBlock::Text { text: text.clone() }],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 0,
                output_tokens: u32::try_from(text.len()).unwrap_or(u32::MAX).div_ceil(4),
                ..Usage::default()
            },
        }
    }
}

#[async_trait]
impl Backend for ThirdPartyBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            cache: CacheMode::ImplicitPrefix {
                min_prefix_tokens: 0,
            },
            parallel_tool_calls: false,
            structured_output: StructuredOutput::None,
            max_context_tokens: self.max_context_tokens,
            reasoning: false,
            reliability_tier: ReliabilityTier::Community,
        }
    }

    async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        Ok(self.respond())
    }

    async fn stream(
        &self,
        _req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.respond();
        let first_delta = match response.content.first() {
            Some(ContentBlock::Text { text }) => StreamChunk::TextDelta(text.clone()),
            _ => StreamChunk::TextDelta(String::new()),
        };
        let items = vec![Ok(first_delta), Ok(StreamChunk::Done(response))];
        Ok(Box::pin(VecStream {
            items: items.into_iter().collect(),
        }))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        Ok(ProbeReport {
            ok: true,
            latency_ms: 0,
            models: vec![ModelId::new(MODEL_ID)],
            detail: None,
            at: chrono::Utc::now(),
        })
    }

    /// Overridden (not left at the trait's default), and calls
    /// [`check_admission`] for the fits/shortfall arithmetic rather than
    /// restating it -- one implementation -- the property `crates/conway/tests/
    /// backend_parity.rs`'s own `StubBackend::admit` already establishes,
    /// re-proven here from a crate that genuinely cannot see
    /// `conway-core::ports::backend::default_estimate_tokens` at all.
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

/// A hand-rolled `Stream` over a fixed queue -- this crate's own analogue
/// of `crates/conway/tests/backend_parity.rs`'s identical `VecStream`,
/// needed because `Backend::stream`'s return type requires a real
/// `futures_core::Stream` impl, not merely the `BoxStream` alias.
struct VecStream {
    items: std::collections::VecDeque<Result<StreamChunk, BackendError>>,
}

impl Stream for VecStream {
    type Item = Result<StreamChunk, BackendError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.get_mut().items.pop_front())
    }
}

/// A [`BackendFactory`] whose `id()` matches [`BACKEND_KIND`] and whose
/// `build` reads [`BackendBuildContext::models`] (the same table a real
/// config-derived backend's capabilities are projected from) for a
/// [`MODEL_ID`] override, exactly the way `conway-plugin-backends`' own two
/// shipped factories do -- proving a genuinely third-party factory reads
/// the identical context a first-party one does. Also reads
/// [`BackendBuildContext::extra`] for [`GREETING_KEY`] -- the same context
/// field, read the same way, proving the catch-all channel a third-party
/// kind's own configuration travels through is genuinely reachable too.
pub struct ThirdPartyBackendFactory;

impl BackendFactory for ThirdPartyBackendFactory {
    fn id(&self) -> &str {
        BACKEND_KIND
    }

    fn build(&self, ctx: BackendBuildContext) -> Result<Arc<dyn Backend>, CoreConwayError> {
        let max_context_tokens = ctx
            .models
            .get(MODEL_ID)
            .and_then(|overrides: &ModelOverrides| overrides.max_context_tokens)
            .unwrap_or(32_000);
        // `extra` is the entry's own keys beyond `kind` and the five typed
        // fields `BackendEntry` recognizes -- absent when the entry sets no
        // `greeting`, in which case `ThirdPartyBackend::respond` gives back
        // `REPLY_TEXT` unchanged.
        let greeting = ctx
            .extra
            .get(GREETING_KEY)
            .and_then(|value| value.as_str())
            .map(str::to_string);
        Ok(Arc::new(ThirdPartyBackend {
            id: ctx.id,
            max_context_tokens,
            greeting,
        }))
    }
}

/// Wires a real [`conway::Conway`] from a rendered `settings.json`,
/// installing [`ThirdPartyBackendFactory`] through
/// `ConwayBuilder::with_backend_factory` -- the SAME public builder channel
/// `conway-plugin-backends::OpenAiCompatBackendFactory` installs through in
/// `crates/conway-plugin-backends/tests/builder_end_to_end.rs`. `dir` is
/// both the fixture root ([`fixture::write_settings`]'s own return value's
/// parent) and the `XDG_CONFIG_HOME` isolation point: it has no
/// `conway/settings.json` inside it, so `conway::config::load`'s
/// XDG-scoped lookup can never merge in a real
/// `~/.conway/settings.json` from the machine running this (this module's
/// own doc comment says why that hazard is real, not hypothetical).
///
/// Shared by `tests/end_to_end.rs` (the library-embedder demonstration) and
/// `src/bin/thirdparty_backend_demo.rs` (the compiled-binary one) so both
/// exercise byte-identical config-loading and builder-wiring code -- this
/// function itself is convenience plumbing for this item's own harness,
/// **not** part of the `Backend` authoring surface claim above it.
pub fn build_conway(dir: &Path, config_path: &Path) -> conway::Result<conway::Conway> {
    let mut env = std::collections::HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        dir.to_string_lossy().into_owned(),
    );
    let outcome = conway::config::load(conway::config::LoadOptions {
        cwd: dir.to_path_buf(),
        explicit_path: Some(config_path.to_path_buf()),
        env,
        cli_overrides: conway::config::CliOverrides::default(),
        model_metadata_refresh: false,
    })?;
    conway::ConwayBuilder::from_parts(outcome.config)
        .with_backend_factory(Arc::new(ThirdPartyBackendFactory))
        .build()
}

/// The `settings.json` + `.conway/models.json` this item's own harness
/// renders -- convenience fixture plumbing shared by the library and
/// binary demonstrations, not part of the `Backend` authoring surface
/// itself (see [`build_conway`]'s own doc comment for the same
/// disclaimer).
pub mod fixture {
    use std::path::PathBuf;

    use super::{BACKEND_ID, BACKEND_KIND, GREETING_KEY, MODEL_ID};

    /// A fresh, uniquely-named directory under the OS temp dir -- this
    /// crate's own manual stand-in for `tempfile::tempdir()` (not a
    /// dev-dependency here: `[[bin]]` targets under `src/bin/` do not
    /// receive `[dev-dependencies]`, and this helper is shared by one).
    /// Mirrors `crates/conway/src/config/discovery.rs`'s own test-local
    /// `tempfile_dir()` helper for the identical reason.
    pub fn fresh_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "conway-thirdparty-backend-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        dir
    }

    /// Renders a real `settings.json` naming `kind = "thirdparty-stub"`
    /// under `dir` (`cwd` set to `dir` itself so every relative path this
    /// config resolves -- `session.root`, `models.metadata_path` -- stays
    /// inside this one isolated directory regardless of the test process's
    /// own working directory), plus the `.conway/models.json` entry every
    /// `[backends.<id>]` entry needs a routable `(backend, model)` pair
    /// declared in (`ConwayBuilder::build`'s `CapabilityIndex` is built
    /// only from `models.json`-declared pairs -- `crates/conway-cli/tests/
    /// common/mod.rs`'s own `write_fixture` documents the identical
    /// requirement for the compiled-binary suite one crate over). Returns
    /// the `settings.json` path. The `[backends.<id>]` entry names only
    /// `kind` -- no [`GREETING_KEY`] -- so `ThirdPartyBackend::respond`
    /// gives back `REPLY_TEXT` unchanged; see
    /// [`write_settings_with_greeting`] for the variant that exercises
    /// `BackendBuildContext::extra`.
    pub fn write_settings(dir: &std::path::Path) -> PathBuf {
        write_settings_with_backend_entry(dir, serde_json::json!({ "kind": BACKEND_KIND }))
    }

    /// Same as [`write_settings`], except the `[backends.<id>]` entry also
    /// carries a [`GREETING_KEY`] key beyond `kind` -- one of the keys
    /// `BackendEntry` does not itself recognize, captured into its `extra`
    /// map and, since, handed onward
    /// through `BackendBuildContext::extra` to
    /// `ThirdPartyBackendFactory::build`. `tests/custom_key.rs` uses this to
    /// prove the value genuinely reaches the factory-built backend's own
    /// reply text, not merely that `extra` is populated.
    pub fn write_settings_with_greeting(dir: &std::path::Path, greeting: &str) -> PathBuf {
        write_settings_with_backend_entry(
            dir,
            serde_json::json!({ "kind": BACKEND_KIND, (GREETING_KEY): greeting }),
        )
    }

    /// Shared by [`write_settings`] and [`write_settings_with_greeting`] --
    /// only the `[backends.<id>]` entry itself differs between the two, so
    /// this is the one place the rest of the rendered `settings.json` and
    /// its `.conway/models.json` companion are written, keeping the two
    /// fixtures from silently drifting apart on everything else.
    fn write_settings_with_backend_entry(
        dir: &std::path::Path,
        backend_entry: serde_json::Value,
    ) -> PathBuf {
        let chain = format!("{BACKEND_ID}/{MODEL_ID}");
        let settings = serde_json::json!({
            "default_role": "coder",
            "cwd": dir.to_string_lossy(),
            // `permissions.mode = "allowlist"` requires a non-empty
            // `allowed_tools` list (`config::merge::validate`) even though
            // `ThirdPartyBackend` never issues a tool call and this gate is
            // therefore never actually consulted -- `"*"` is a real,
            // syntactically valid `AllowListGate` glob entry (matches any
            // tool name), not a magic sentinel this fixture invented.
            "permissions": { "mode": "allowlist", "allowed_tools": ["*"] },
            "backends": {
                BACKEND_ID: backend_entry
            },
            "roles": {
                "coder": { "chain": [chain] }
            }
        });
        let config_path = dir.join("settings.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec_pretty(&settings).expect("serialize settings.json"),
        )
        .expect("write settings.json");

        let models_dir = dir.join(".conway");
        std::fs::create_dir_all(&models_dir).expect("create .conway dir");
        let models_json = serde_json::json!({
            "models": {
                format!("{BACKEND_ID}/{MODEL_ID}"): {
                    "max_context_tokens": 200_000,
                    "tool_calling": "streaming_validated",
                    "reasoning": false,
                    "reliability_tier": "community",
                }
            }
        });
        std::fs::write(
            models_dir.join("models.json"),
            serde_json::to_vec_pretty(&models_json).expect("serialize models.json"),
        )
        .expect("write models.json");

        config_path
    }
}
