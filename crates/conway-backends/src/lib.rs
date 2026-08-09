//! `conway-backends` — concrete `Backend` implementations (architecture §7,
//! Module: conway-backends).
//!
//! Owns wire-format translation, tool-call parsing, cache-hint mapping, and
//! capability declaration for the [`anthropic::AnthropicBackend`] and
//! [`openai_compat::OpenAiCompatBackend`] adapters. This crate performs no
//! routing/policy decisions and no retry across backends: a single
//! `generate`/`stream` call targets one endpoint, and the bounded
//! transport-retry policy in [`http`] retries at most twice against that
//! same endpoint (module boundary rule).
//!
//! [`openai_compat`] (WI-019) was the first adapter; [`anthropic`] (WI-021)
//! is the second. The remaining `openai_compat` dialects are added by a
//! later work item (WI-022).
//!
//! [`tool_calls`] has no HTTP client of its own and is shared, unmodified,
//! by both the `anthropic` and `openai-compat` adapters' delta-accumulation
//! paths.

pub(crate) mod admission;
pub mod anthropic;
pub mod capabilities;
pub mod config;
pub mod error;
pub(crate) mod http;
pub mod model_metadata;
pub mod openai_compat;
pub mod probe;
pub mod profile;
pub mod tool_calls;
