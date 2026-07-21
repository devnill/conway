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
//! Adapter modules (`anthropic`, `openai_compat`, `model_metadata`, and
//! friends) are added by later work items (WI-017 … WI-022); this crate
//! intentionally declares only the modules this work item owns.

pub mod config;
pub mod error;

#[cfg(any(feature = "anthropic", feature = "openai-compat"))]
pub(crate) mod http;
