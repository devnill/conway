//! [`AnthropicBackendFactory`]/[`OpenAiCompatBackendFactory`]: this crate's
//! two [`conway_core::ports::BackendFactory`] implementations -- name [`ANTHROPIC_KIND`]/
//! [`OPENAI_COMPAT_KIND`] up front so `ConwayBuilder::with_backend_factory`
//! (or, for the shipped binary, `conway-cli`'s own default-on backend arm --
//! see that crate's `src/first_party_plugins.rs` module doc for exactly what
//! makes each attach without any `[plugins].install` entry) can select one
//! before any `[backends.<id>]` entry is resolved, then build the SAME
//! [`crate::anthropic::AnthropicBackend`]/[`crate::openai_compat::OpenAiCompatBackend`]
//! `crates/conway/src/builder.rs`'s own `build_anthropic`/`build_openai_compat`
//! used to assemble directly, before those functions moved here -- relocated,
//! not reimplemented, down to `resolve_profile`'s kebab-case translation
//! table and `probe_openai_compat_backends`'s RESTRICT-eligible discovery
//! step (now [`OpenAiCompatBackendFactory::probe_capabilities`]).
//!
//! Both kind ids are the exact strings `crates/conway/src/builder.rs` used
//! to hardcode as its (now-removed) compiled-in fallback
//! (`BUILTIN_BACKEND_KIND_ANTHROPIC`/`BUILTIN_BACKEND_KIND_OPENAI_COMPAT`):
//! `"anthropic"`, `"openai-compat"` -- so an existing `[backends.<id>].kind`
//! value in any operator's `settings.json` keeps resolving unchanged, to a
//! REGISTERED factory now rather than a compiled-in fallback. `conway`
//! itself no longer hardcodes either string into its own resolution path
//! (`resolve_backend_factory`); `kind` there is an open name matched only
//! against whichever factories are registered.
//!
//! `[plugins].default_backends` (`conway::config::schema::PluginsConfig`,
//!) is what makes both kinds attach
//! WITHOUT an explicit `[plugins].install` entry -- unlike every other
//! first-party plugin/router kind, whose install list is empty by contract
//! (`PluginsConfig::install`'s own doc: "the tier's whole point is that
//! nothing in it runs unasked"). A backend absent from a fresh install
//! leaves `conway` inert (no model is ever reachable at all), which is a
//! materially different failure mode than routing's honest degenerate
//! `MinimalRouter` fallback -- so this one first-party pair ships attached
//! by default, and an operator declines a specific kind by removing its id
//! from that list (a later's decline-mechanism UX, not this
//! item's job -- see `PluginsConfig::default_backends`'s own doc for the
//! exact default value and precedence).

use std::collections::BTreeMap;
use std::sync::Arc;

use conway_core::capabilities::Capabilities;
use conway_core::error::ConwayError;
use conway_core::ids::ModelId;
use conway_core::ports::{Backend, BackendBuildContext, BackendFactory};

use crate::config::{AnthropicConfig, OpenAiCompatConfig, SecretString};
use crate::probe::{CapabilityProbe, DISCOVERY_TIMEOUT};
use crate::profile::{Profile, ProfileStore};

/// This crate's published Anthropic-dialect kind id -- the `[backends.<id>]
/// .kind` value that selects [`AnthropicBackendFactory`]. Unchanged from the
/// string `crates/conway/src/builder.rs` used to hardcode before this item.
pub const ANTHROPIC_KIND: &str = "anthropic";

/// This crate's published OpenAI-compatible-dialect kind id -- the
/// `[backends.<id>].kind` value that selects [`OpenAiCompatBackendFactory`].
/// Unchanged from the string `crates/conway/src/builder.rs` used to
/// hardcode before this item.
pub const OPENAI_COMPAT_KIND: &str = "openai-compat";

/// This crate's Anthropic-dialect [`BackendFactory`]. Zero-sized -- every
/// input it needs arrives through [`BackendBuildContext`] at `build()` time.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnthropicBackendFactory;

impl BackendFactory for AnthropicBackendFactory {
    fn id(&self) -> &str {
        ANTHROPIC_KIND
    }

    /// Builds an [`crate::anthropic::AnthropicBackend`] from `ctx` -- exactly
    /// what `crates/conway/src/builder.rs`'s own (now-removed)
    /// `build_anthropic` assembled directly: an empty `ctx.base_url` falls
    /// back to Anthropic's own hosted endpoint (`ctx.dialect` is unused by
    /// this kind -- there is no "dialect" concept for the Anthropic wire
    /// format), and `ctx.api_key` (empty when `None`) is validated by
    /// [`AnthropicConfig::validate`] before construction, so a missing key
    /// is a named [`ConwayError::Config`], never a panic or a silent empty
    /// credential reaching the wire.
    fn build(&self, ctx: BackendBuildContext) -> Result<Arc<dyn Backend>, ConwayError> {
        use crate::anthropic::AnthropicBackend;

        let base_url = if ctx.base_url.is_empty() {
            url::Url::parse("https://api.anthropic.com")
                .expect("hardcoded default Anthropic base URL must be valid")
        } else {
            url::Url::parse(&ctx.base_url).map_err(|e| ConwayError::Config {
                detail: format!("backend '{}': invalid base_url: {e}", ctx.id),
            })?
        };

        let cfg = AnthropicConfig {
            api_key: SecretString::new(ctx.api_key.unwrap_or_default()),
            id: ctx.id.clone(),
            base_url,
            // Not exposed by the facade schema; mirrors this crate's own
            // (private) default literal, unchanged from before this item.
            anthropic_version: "2023-06-01".to_string(),
            timeout: None,
            models: ctx.models,
        };
        cfg.validate().map_err(|e| ConwayError::Config {
            detail: format!("backend '{}': {e}", ctx.id),
        })?;

        let backend = AnthropicBackend::new(cfg).map_err(|e| ConwayError::Config {
            detail: format!("backend '{}': {e}", ctx.id),
        })?;
        Ok(Arc::new(backend))
    }

    // No `probe_capabilities` override: the Anthropic wire format has no
    // server-side model-listing endpoint this crate speaks (`probe()`'s own
    // `ProbeReport` carries no capability data to overlay -- see
    // `crates/conway/src/builder.rs`'s pre-relocation module doc, which
    // disclosed this same limitation for the compiled-in adapter). The
    // trait's default (an empty map) is exactly today's behavior: `probe_
    // on_startup` never affected an `"anthropic"`-kind entry before this
    // item, and does not after it either.
}

/// This crate's OpenAI-compatible-dialect [`BackendFactory`]. Zero-sized --
/// every input it needs arrives through [`BackendBuildContext`] at `build()`/
/// [`Self::probe_capabilities`] time.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiCompatBackendFactory;

impl OpenAiCompatBackendFactory {
    /// Resolves a full [`ProfileStore`]: this crate's own compile-time
    /// [`ProfileStore::built_ins`] layered under every file in
    /// `ctx.profile_file_paths`, in order -- the exact assembly
    /// `crates/conway/src/builder.rs`'s own (now-removed)
    /// `load_provider_profiles` performed, just relocated: the FACADE still
    /// owns discovering *which files* (`config::discovery::
    /// provider_profile_file_paths`, resolved once per `build()` call and
    /// copied onto every `BackendBuildContext` -- see that field's own doc),
    /// this crate owns *parsing and merging* them.
    fn resolve_profile_store(ctx: &BackendBuildContext) -> Result<ProfileStore, ConwayError> {
        let mut store = ProfileStore::built_ins();
        for path in &ctx.profile_file_paths {
            store = store.merge_file(path).map_err(|e| ConwayError::Config {
                detail: format!(
                    "failed to load provider profiles from {}: {e}",
                    path.display()
                ),
            })?;
        }
        Ok(store)
    }

    /// Resolves `ctx.dialect` to a [`Profile`] against `profiles` -- exactly
    /// `crates/conway/src/builder.rs`'s own (now-removed) `resolve_profile`:
    /// the three dialects whose documented facade spelling is kebab-case
    /// (`"vllm-hermes"`, `"lm-studio"`, `"llamacpp-server"`) are translated
    /// to their snake_case built-in profile ids first -- preserving every
    /// existing config file unchanged -- then looked up verbatim; every
    /// other string (`"openai"`, `"ollama"`, `"kimi"`, or any id a
    /// `.conway/profiles.toml` file declares) is looked up as-is.
    fn resolve_profile(
        id: &conway_core::ids::BackendId,
        raw: &str,
        profiles: &ProfileStore,
    ) -> Result<Profile, ConwayError> {
        let canonical = match raw {
            "vllm-hermes" => "vllm_hermes",
            "lm-studio" => "lm_studio",
            "llamacpp-server" => "llama_cpp_server",
            other => other,
        };
        profiles
            .get(canonical)
            .cloned()
            .ok_or_else(|| ConwayError::Config {
                detail: format!(
                    "backend '{id}': unknown dialect/profile '{raw}' (no built-in or loaded \
                     profile named '{canonical}')"
                ),
            })
    }
}

impl BackendFactory for OpenAiCompatBackendFactory {
    fn id(&self) -> &str {
        OPENAI_COMPAT_KIND
    }

    /// Builds an [`crate::openai_compat::OpenAiCompatBackend`] from `ctx` --
    /// exactly what `crates/conway/src/builder.rs`'s own (now-removed)
    /// `build_openai_compat` assembled directly.
    fn build(&self, ctx: BackendBuildContext) -> Result<Arc<dyn Backend>, ConwayError> {
        use crate::openai_compat::OpenAiCompatBackend;

        let dialect_raw = ctx.dialect.as_deref().ok_or_else(|| ConwayError::Config {
            detail: format!(
                "backend '{}': kind 'openai-compat' requires 'dialect'",
                ctx.id
            ),
        })?;
        let profiles = Self::resolve_profile_store(&ctx)?;
        let profile = Self::resolve_profile(&ctx.id, dialect_raw, &profiles)?;
        let base_url = url::Url::parse(&ctx.base_url).map_err(|e| ConwayError::Config {
            detail: format!("backend '{}': invalid base_url: {e}", ctx.id),
        })?;

        let cfg = OpenAiCompatConfig {
            id: ctx.id.clone(),
            base_url,
            api_key: ctx.api_key.map(SecretString::new),
            profile,
            timeout: None,
            metadata_path: None,
            models: ctx.models,
        };

        let backend = OpenAiCompatBackend::new(cfg).map_err(|e| ConwayError::Config {
            detail: format!("backend '{}': {e}", ctx.id),
        })?;
        Ok(Arc::new(backend))
    }

    /// Runs a startup [`CapabilityProbe`] for this configured instance --
    /// exactly what `crates/conway/src/builder.rs`'s own (now-removed)
    /// `probe_openai_compat_backends` did per `openai-compat` entry, with
    /// the RESTRICT-eligibility filter (never introduce a pair `models.json`
    /// didn't declare) now applied generically by the caller against
    /// `ctx.models` -- see [`BackendFactory::probe_capabilities`]'s own doc.
    /// A missing/invalid `dialect`/`base_url` is a `tracing::warn` and an
    /// empty map, mirroring `probe_openai_compat_backends`'s own
    /// "probe failure is always a warning, never fatal" contract -- never a
    /// hard error, since [`Self::build`] (above) already validates both
    /// fields as hard errors when the backend itself is constructed, and a
    /// probe is best-effort on top of a backend that already exists.
    fn probe_capabilities(&self, ctx: &BackendBuildContext) -> BTreeMap<ModelId, Capabilities> {
        let Some(dialect_raw) = ctx.dialect.as_deref() else {
            tracing::warn!(
                backend = %ctx.id,
                "probe_on_startup: skipping backend with no 'dialect'"
            );
            return BTreeMap::new();
        };
        let profiles = match Self::resolve_profile_store(ctx) {
            Ok(profiles) => profiles,
            Err(e) => {
                tracing::warn!(backend = %ctx.id, error = %e, "probe_on_startup: failed to load provider profiles");
                return BTreeMap::new();
            }
        };
        let Ok(profile) = Self::resolve_profile(&ctx.id, dialect_raw, &profiles) else {
            tracing::warn!(
                backend = %ctx.id,
                dialect = %dialect_raw,
                "probe_on_startup: skipping backend with unknown dialect/profile"
            );
            return BTreeMap::new();
        };
        let Ok(base_url) = url::Url::parse(&ctx.base_url) else {
            tracing::warn!(backend = %ctx.id, "probe_on_startup: skipping backend with invalid base_url");
            return BTreeMap::new();
        };
        let auth = ctx.api_key.clone().map(SecretString::new);

        let probe = CapabilityProbe::new(
            base_url,
            profile,
            auth,
            DISCOVERY_TIMEOUT,
            // Matches the backend's own store (`openai_compat/mod.rs`'s
            // `metadata_path: None`) -- the facade's `models.json` reaches
            // the probe exclusively through `ctx.models`, not this store.
            crate::model_metadata::ModelMetadataStore::defaults(),
            ctx.models.clone(),
        );
        let result = block_on(probe.discover_result());
        if result.degraded {
            tracing::warn!(
                backend = %ctx.id,
                "probe_on_startup: capability discovery observed no models; keeping file-derived \
                 metadata"
            );
            return BTreeMap::new();
        }
        result.capabilities
    }
}

/// Runs `fut` to completion on a fresh OS thread with its own throwaway
/// current-thread `tokio` runtime -- the identical bridge `crates/conway/
/// src/builder.rs`'s own private `block_on` helper uses (that module's own doc
/// explains why: `Handle::current().block_on` panics when called from inside an
/// already-running `tokio` task, which `ConwayBuilder::build` commonly is).
/// Duplicated here rather than shared across the crate boundary because
/// [`BackendFactory::probe_capabilities`] is, like [`BackendFactory::build`], a
/// synchronous method a third-party kind is equally free to bridge this same
/// way, since a built-in gets no privileged API -- nothing about this being a
/// first-party kind reaches a private mechanism.
fn block_on<F>(fut: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect(
                        "build temporary tokio runtime for probe_capabilities's sync/async bridge",
                    )
                    .block_on(fut)
            })
            .join()
            .expect("probe_capabilities's blocking-bridge thread panicked")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::ids::BackendId;

    fn ctx(id: &str, base_url: &str, dialect: Option<&str>) -> BackendBuildContext {
        BackendBuildContext {
            id: BackendId::new(id),
            base_url: base_url.to_string(),
            api_key: None,
            dialect: dialect.map(|d| d.to_string()),
            models: BTreeMap::new(),
            profile_file_paths: Vec::new(),
            // neither shipped
            // dialect reads `extra` -- this helper's callers all assert on
            // `base_url`/`dialect`/`profile_file_paths` behavior, unaffected
            // by this field's addition.
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn kind_ids_are_the_published_constants() {
        assert_eq!(AnthropicBackendFactory.id(), ANTHROPIC_KIND);
        assert_eq!(OpenAiCompatBackendFactory.id(), OPENAI_COMPAT_KIND);
        assert_eq!(ANTHROPIC_KIND, "anthropic");
        assert_eq!(OPENAI_COMPAT_KIND, "openai-compat");
    }

    #[test]
    fn anthropic_build_defaults_the_base_url_when_empty() {
        let mut c = ctx("anthropic", "", None);
        c.api_key = Some("sk-ant-api03-test".to_string());
        let backend = AnthropicBackendFactory
            .build(c)
            .expect("a non-empty api_key must build");
        assert_eq!(backend.id(), BackendId::new("anthropic"));
    }

    /// `Arc<dyn Backend>` (the `Ok` type) is not `Debug`, so `expect_err`
    /// (which requires `T: Debug`) cannot be used on this `Result` -- match
    /// directly instead, the same workaround `crates/conway/tests/
    /// builder.rs`'s own `expect_build_err` documents for the identical
    /// reason on `Conway`.
    #[test]
    fn anthropic_build_rejects_a_missing_api_key() {
        let c = ctx("anthropic", "", None);
        let err = match AnthropicBackendFactory.build(c) {
            Err(err) => err,
            Ok(_) => panic!("an empty api_key must be rejected"),
        };
        assert!(matches!(err, ConwayError::Config { .. }));
    }

    #[test]
    fn openai_compat_build_requires_a_dialect() {
        let c = ctx("local", "http://localhost:11434/v1", None);
        let err = match OpenAiCompatBackendFactory.build(c) {
            Err(err) => err,
            Ok(_) => panic!("a missing dialect must be rejected"),
        };
        match err {
            ConwayError::Config { detail } => assert!(detail.contains("dialect")),
            other => panic!("expected ConwayError::Config, got {other:?}"),
        }
    }

    /// Declarative provider profiles: every existing documented dialect
    /// string (both plain and the three kebab-case spellings) resolves, a
    /// brand-new built-in profile (`kimi`) resolves by name with no
    /// special-casing, and an unknown name is a named, typed error rather
    /// than a panic -- ported from `crates/conway/src/builder.rs`'s own
    /// (now-removed) `resolve_profile_accepts_every_documented_dialect_
    /// string_and_new_built_ins`/`resolve_profile_names_the_unknown_dialect_
    /// in_a_typed_error`, since `resolve_profile` itself relocated here.
    #[test]
    fn resolve_profile_accepts_every_documented_dialect_string_and_new_built_ins() {
        let profiles = ProfileStore::built_ins();
        let id = BackendId::new("test");
        for (raw, expected_id) in [
            ("openai", "openai"),
            ("ollama", "ollama"),
            ("vllm-hermes", "vllm_hermes"),
            ("lm-studio", "lm_studio"),
            ("llamacpp-server", "llama_cpp_server"),
            ("kimi", "kimi"),
        ] {
            let profile = OpenAiCompatBackendFactory::resolve_profile(&id, raw, &profiles)
                .unwrap_or_else(|e| panic!("'{raw}' must resolve: {e}"));
            assert_eq!(profile.id, expected_id);
        }
    }

    #[test]
    fn resolve_profile_names_the_unknown_dialect_in_a_typed_error() {
        let profiles = ProfileStore::built_ins();
        let id = BackendId::new("mybackend");
        let err = OpenAiCompatBackendFactory::resolve_profile(&id, "totally-unknown", &profiles)
            .expect_err("an unknown dialect/profile must be rejected");
        match err {
            ConwayError::Config { detail } => {
                assert!(detail.contains("mybackend"), "{detail}");
                assert!(detail.contains("totally-unknown"), "{detail}");
            }
            other => panic!("expected ConwayError::Config, got {other:?}"),
        }
    }

    /// Declarative provider profiles: `ctx.profile_file_paths` is genuinely
    /// consulted, not merely accepted and ignored -- a user-supplied profile
    /// at a path named there resolves with no recompile, ported from
    /// `crates/conway/src/builder.rs`'s own (now-removed) `resolve_profile_
    /// resolves_a_user_supplied_profile_with_no_recompile`.
    #[test]
    fn build_resolves_a_user_supplied_profile_named_in_profile_file_paths() {
        let dir = std::env::temp_dir().join(format!(
            "conway-plugin-backends-factory-profile-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("profiles.toml");
        std::fs::write(
            &path,
            r#"
            [[profile]]
            id = "my-vendor"
            chat_path = "/chat/completions"
            "#,
        )
        .unwrap();

        let mut c = ctx("local", "http://localhost:1234", Some("my-vendor"));
        c.profile_file_paths = vec![path];
        let backend = OpenAiCompatBackendFactory
            .build(c)
            .expect("a profile loaded from profile_file_paths must resolve");
        assert_eq!(backend.id(), BackendId::new("local"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// [`OpenAiCompatBackendFactory::probe_capabilities`]'s degraded path:
    /// no server reachable at all (an unbound loopback port) returns an
    /// empty map rather than erroring -- mirrors `CapabilityProbe`'s own
    /// "a probe failure is always a warning, never fatal" contract.
    #[test]
    fn probe_capabilities_returns_empty_on_an_unreachable_server() {
        let c = ctx("local", "http://127.0.0.1:9", Some("openai"));
        let discovered = OpenAiCompatBackendFactory.probe_capabilities(&c);
        assert!(discovered.is_empty());
    }

    /// The Anthropic kind has no discovery mechanism at all -- the trait's
    /// default applies unchanged.
    #[test]
    fn anthropic_probe_capabilities_is_always_empty() {
        let c = ctx("anthropic", "", None);
        assert!(AnthropicBackendFactory.probe_capabilities(&c).is_empty());
    }
}
