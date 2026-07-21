//! Per-dialect wire differences for `OpenAiCompatBackend`: the chat
//! endpoint path, whether `stream_options` is sent, and whether a
//! multi-block user message is flattened to a single string.
//!
//! `Dialect` itself is `crate::config::Dialect` (WI-016); this module adds
//! inherent methods to that same type rather than redefining it, matching
//! the precedent `tool_calls::mod` already set for this crate's
//! single-`Dialect`-type invariant. `dialect` is a private module — these
//! methods are still reachable from anywhere `Dialect` is visible (inherent
//! impls are resolved through the type, not the defining module's path).
//!
//! Only the `OpenAi` and `Ollama` arms are exercised by this item's tests;
//! `VllmHermes`, `LmStudio`, and `LlamaCppServer` get the same conservative
//! wire behavior as `Ollama` (flattened multi-block user content, no
//! `stream_options`) and their own WI-017 `dialect_defaults()` entry, and
//! are exercised by WI-022.
//!
//! WI-022 adds two more predicates for the same reason: `wire.rs`
//! consults `sends_parallel_tool_calls()` (only `OpenAi` — other servers
//! 400 on the unknown `parallel_tool_calls` field) instead of matching on
//! `Dialect::OpenAi` inline, and `uses_hermes_text_fallback()` (only
//! `VllmHermes`) names the predicate a future streaming integration would
//! consult to route `delta.content` through `tool_calls::hermes`
//! (vllm#31871); this item wires and tests the fallback at the
//! `ToolCallAccumulator` level (`push_content_delta`/`stop_override`) only,
//! per the WI-022 scope (`src/openai_compat/stream.rs` is out of scope).

use crate::capabilities::{dialect_defaults, DialectDefaults};
use crate::config::Dialect;

impl Dialect {
    /// This dialect's baseline `DialectDefaults` (WI-017).
    pub fn defaults(self) -> DialectDefaults {
        dialect_defaults(self)
    }

    /// The chat-completions endpoint path, relative to `base_url`. Every
    /// dialect in this item's scope speaks the same OpenAI-shaped
    /// `/chat/completions` endpoint.
    pub fn chat_path(self) -> &'static str {
        "/chat/completions"
    }

    /// Whether to send `"stream_options":{"include_usage":true}` on a
    /// streamed request. `OpenAi` and `Ollama` both honor it; the other
    /// three dialects are untested servers this item does not exercise, so
    /// the conservative choice is to omit it.
    pub fn supports_stream_options(self) -> bool {
        matches!(self, Dialect::OpenAi | Dialect::Ollama)
    }

    /// Whether a multi-text-block `User` segment is flattened to one
    /// `\n\n`-joined string (`true`) or kept as an OpenAI-shaped
    /// `[{"type":"text","text":...}]` array (`false`, `OpenAi` only).
    pub fn flatten_multiblock_user(self) -> bool {
        !matches!(self, Dialect::OpenAi)
    }

    /// Whether `dialect` sends `"parallel_tool_calls"` on a tools-bearing
    /// request (Implementation Notes: "other servers 400 on the unknown
    /// field") — `OpenAi` only.
    pub fn sends_parallel_tool_calls(self) -> bool {
        matches!(self, Dialect::OpenAi)
    }

    /// Whether `dialect` uses the Hermes inline
    /// `<tool_call>...</tool_call>` text fallback (`tool_calls::hermes`)
    /// for tool calls a server emits as raw text rather than structured
    /// `delta.tool_calls` (vllm#31871) — `VllmHermes` only.
    pub fn uses_hermes_text_fallback(self) -> bool {
        matches!(self, Dialect::VllmHermes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_path_is_the_same_for_every_dialect() {
        for dialect in [
            Dialect::OpenAi,
            Dialect::Ollama,
            Dialect::VllmHermes,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            assert_eq!(dialect.chat_path(), "/chat/completions");
        }
    }

    #[test]
    fn only_openai_and_ollama_support_stream_options() {
        assert!(Dialect::OpenAi.supports_stream_options());
        assert!(Dialect::Ollama.supports_stream_options());
        assert!(!Dialect::VllmHermes.supports_stream_options());
        assert!(!Dialect::LmStudio.supports_stream_options());
        assert!(!Dialect::LlamaCppServer.supports_stream_options());
    }

    #[test]
    fn only_openai_keeps_the_multiblock_user_array() {
        assert!(!Dialect::OpenAi.flatten_multiblock_user());
        assert!(Dialect::Ollama.flatten_multiblock_user());
        assert!(Dialect::VllmHermes.flatten_multiblock_user());
    }

    #[test]
    fn only_openai_sends_parallel_tool_calls() {
        assert!(Dialect::OpenAi.sends_parallel_tool_calls());
        for dialect in [
            Dialect::Ollama,
            Dialect::VllmHermes,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            assert!(!dialect.sends_parallel_tool_calls(), "{dialect:?}");
        }
    }

    #[test]
    fn only_vllm_hermes_uses_the_hermes_text_fallback() {
        assert!(Dialect::VllmHermes.uses_hermes_text_fallback());
        for dialect in [
            Dialect::OpenAi,
            Dialect::Ollama,
            Dialect::LmStudio,
            Dialect::LlamaCppServer,
        ] {
            assert!(!dialect.uses_hermes_text_fallback(), "{dialect:?}");
        }
    }

    #[test]
    fn defaults_matches_capabilities_dialect_defaults() {
        assert_eq!(
            Dialect::Ollama.defaults(),
            dialect_defaults(Dialect::Ollama)
        );
        assert_eq!(
            Dialect::OpenAi.defaults(),
            dialect_defaults(Dialect::OpenAi)
        );
    }
}
