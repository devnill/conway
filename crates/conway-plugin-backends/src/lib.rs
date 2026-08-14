//! `conway-plugin-backends` — the two provider-adapter dialects `conway`
//! ships, installed as a first-party plugin rather
//! than compiled into the facade directly.
//!
//! Owns wire-format translation, tool-call parsing, cache-hint mapping, and
//! capability declaration for the [`anthropic::AnthropicBackend`] and
//! [`openai_compat::OpenAiCompatBackend`] adapters, plus, since this item,
//! the [`BackendFactory`](conway_core::ports::BackendFactory)
//! implementations ([`AnthropicBackendFactory`], [`OpenAiCompatBackendFactory`]
//! — see `factory`'s own module doc for exactly what makes each attach by
//! default) that used to be `crates/conway/src/builder.rs`'s own
//! `build_anthropic`/`build_openai_compat`/`resolve_profile`/
//! `load_provider_profiles`/`probe_openai_compat_backends` — relocated here,
//! not reimplemented. This crate performs no routing/policy decisions and no
//! retry across backends: a single `generate`/`stream` call targets one
//! endpoint, and the bounded transport-retry policy in `http` retries at
//! most twice against that same endpoint (module boundary rule).
//!
//! [`openai_compat`] was the first adapter; [`anthropic`]
//! is the second. The remaining `openai_compat` dialects are declarative
//! [`profile::Profile`] data, not additional Rust adapters.
//!
//! [`tool_calls`] has no HTTP client of its own and is shared, unmodified,
//! by both the `anthropic` and `openai-compat` adapters' delta-accumulation
//! paths.

pub(crate) mod admission;
pub mod anthropic;
pub mod capabilities;
pub mod config;
pub mod error;
mod factory;
pub(crate) mod http;
pub mod model_metadata;
pub mod openai_compat;
pub mod probe;
pub mod profile;
pub mod tool_calls;

pub use factory::{
    AnthropicBackendFactory, OpenAiCompatBackendFactory, ANTHROPIC_KIND, OPENAI_COMPAT_KIND,
};
