//! `conway-backends` — concrete `Backend` implementations (architecture §7,
//! Module: conway-backends).
//!
//! Owns wire-format translation, tool-call parsing, cache-hint mapping, and
//! capability declaration for the `AnthropicBackend` (feature `anthropic`)
//! and `OpenAiCompatBackend` (feature `openai-compat`) adapters. This crate
//! performs no routing/policy decisions and no retry across backends: a
//! single `generate`/`stream` call targets one endpoint, and the bounded
//! transport-retry policy in [`http`] retries at most twice against that
//! same endpoint (module boundary rule).
//!
//! [`config`] and [`error`] are feature-independent: the configuration types
//! and the HTTP-status → `BackendError` classification table have no HTTP
//! client of their own and compile under `--no-default-features`. The HTTP
//! transport wrapper in `http` is only compiled when at least one adapter
//! feature (`anthropic` or `openai-compat`) is enabled, since it is the only
//! module in this crate that depends on `reqwest`.
//!
//! [`openai_compat`] (WI-019) is the first adapter; `anthropic` and the
//! remaining `openai_compat` dialects are added by later work items
//! (WI-020 … WI-022).
//!
//! [`tool_calls`] is feature-independent like `config`/`error`: it has no
//! HTTP client of its own and is shared, unmodified, by both the
//! `anthropic` and `openai-compat` adapters' delta-accumulation paths.

pub mod capabilities;
pub mod config;
pub mod error;
pub mod model_metadata;
pub mod tool_calls;

#[cfg(any(feature = "anthropic", feature = "openai-compat"))]
pub(crate) mod http;

#[cfg(feature = "openai-compat")]
pub mod openai_compat;
