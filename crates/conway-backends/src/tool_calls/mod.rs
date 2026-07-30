//! `ToolCallAccumulator` — dialect-parameterized streaming tool-call delta
//! accumulation and validation (architecture §"Module: conway-backends",
//! WI-018).
//!
//! Isolates the single most bug-prone surface of the OpenAI-compatible
//! streaming wire format so it is unit-testable without a server: providers
//! stream a tool call as a sequence of partial deltas (an `index`/`id` seen
//! once or repeatedly, and `arguments` arriving as JSON-fragment text that
//! is only valid once concatenated in full), and several real-world servers
//! deviate from the OpenAI-canonical shape in observed, reproducible ways
//! (research-backends: codex#7517, ollama#12557, vllm#31871).
//!
//! # Accumulation model
//!
//! [`ToolCallAccumulator`] holds one [`Slot`] per in-flight tool call, keyed
//! by `u32` in a [`BTreeMap`] so `finish` drains them in ascending —
//! i.e. arrival — order. A delta is routed to a slot by [`resolve_key`]:
//!
//! - When the delta carries an `index`, that index IS the key (the
//!   canonical OpenAI/vLLM shape).
//! - Otherwise, when the delta carries a non-empty `id` already seen, the
//!   slot that `id` first opened is reused — this is the primary
//!   codex#7517 mitigation: some servers (documented in codex#7517) resend
//!   the full `id`+`name` on every chunk instead of only the first; keying
//!   on the first-seen `id` collapses every repeat back onto one slot
//!   rather than the N a naive "non-null name marks a new call" parser
//!   would produce.
//! - Otherwise (`index` and `id` both absent, or `id` present but never
//!   seen before), a new slot is opened only when a non-empty `name`
//!   arrives while the current (most recently touched) slot already has a
//!   name AND a syntactically complete argument buffer — i.e. only when the
//!   current slot looks finished. Any other delta appends to the current
//!   slot. "Syntactically complete" is operationalized as "parses as a
//!   complete `serde_json::Value`" ([`is_complete_json`]) — an
//!   implementation choice within the spec's "syntactically complete"
//!   wording, since the accumulator has no independent signal (e.g. a
//!   provider-declared fragment count) to consult.
//!
//! `id`/`name` are latched on first non-empty occurrence; a later identical
//! value is a silent no-op; a later *different* non-empty `name` for the
//! same slot is a hard error (`ToolParse`, "conflicting tool name"), since
//! that can only mean two calls collided onto one slot.
//!
//! Validation (schema compilation and instance checking) happens **only**
//! in [`ToolCallAccumulator::finish`], never per-delta — partial JSON is
//! the expected, normal state of a slot mid-stream.
//!
//! # Style dispatch
//!
//! `push_delta` dispatches to a style-specific parser that turns one raw
//! provider delta object into style-independent [`DeltaParts`]:
//! [`openai::parse_delta`] for the OpenAI-canonical shape, reused unchanged
//! for `ToolCallStyle::HermesTextFallback`'s structured (non-text) tool-call
//! path; and [`ollama::parse_delta`], a tolerant superset parser, for
//! `ToolCallStyle::Tolerant`.
//!
//! # Declarative provider profiles: `ToolCallStyle`
//!
//! This module used to dispatch directly on `crate::config::Dialect` (added
//! by WI-016). The declarative-provider-profiles item replaced that
//! five-variant match with [`ToolCallStyle`], a three-value enum that names
//! *which parsing strategy* a provider needs rather than which of five
//! fixed dialects it is — the same accumulator now serves any
//! `crate::profile::Profile`, built-in or user-supplied, with no recompile.
//! `crate::profile::Profile::tool_call_style` is the field a caller reads to
//! obtain one; `Dialect`'s own predicate methods (kept for source
//! compatibility) resolve to the same built-in profile data.
//!
//! # The `VllmHermes` inline-text fallback (originally WI-022)
//!
//! `ToolCallStyle::HermesTextFallback` structured deltas (a well-formed
//! `delta.tool_calls` entry) go through the same `push_delta`/
//! `openai::parse_delta` path as `ToolCallStyle::Structured` — no text
//! scanning involved. But some vLLM/Hermes servers (vllm#31871) instead
//! emit a tool call as raw text inside `delta.content`:
//! `<tool_call>{"name":...,"arguments":{...}}</tool_call>`, with no
//! `tool_calls` field on the delta at all. [`ToolCallAccumulator::push_content_delta`]
//! is the entry point for that path: it routes `delta.content` text
//! through [`hermes::HermesTextScanner`] while `style` is
//! `HermesTextFallback` and no structured `delta.tool_calls` entry has
//! arrived yet (`structured_seen`) — the "structured-path passthrough when
//! structured tool_calls appear" rule. Every other style (and
//! `HermesTextFallback` once a structured call has appeared) is a pure
//! passthrough: the text is returned unchanged for the caller to emit as a
//! `TextDelta`.
//!
//! [`ToolCallAccumulator::stop_override`] exposes whether the Hermes
//! scanner parsed at least one inline block, so a caller can force
//! `StopReason::ToolUse` even when the provider reports `finish_reason:
//! "stop"` alongside the inline tool-call text (vllm#31871 is explicit that
//! these servers commonly do exactly that).

mod hermes;
mod ollama;
mod openai;
mod validate;

use std::collections::{BTreeMap, HashMap};

use conway_core::content::{StopReason, ToolCall, ToolSpec};
use conway_core::error::BackendError;
use conway_core::ids::ToolName;
use serde::Deserialize;
use serde_json::Value;

use hermes::HermesTextScanner;
use validate::SchemaValidator;

/// Which streamed tool-call parsing strategy a provider needs — the
/// declarative-provider-profiles replacement for the old
/// `matches!(dialect, Dialect::X)` dispatch in this module. Carried by
/// `crate::profile::Profile::tool_call_style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStyle {
    /// OpenAI-canonical structured `delta.tool_calls` parsing
    /// ([`openai::parse_delta`]); no inline-text fallback. Also the right
    /// choice for a server whose structured tool-calls otherwise follow the
    /// canonical shape but whose provider is otherwise undocumented (e.g. a
    /// new provider profile with no observed quirks yet).
    Structured,
    /// A tolerant superset parser ([`ollama::parse_delta`]) that
    /// additionally accepts a complete-object `arguments` value instead of
    /// only a string fragment (ollama#12557, codex#7517); no inline-text
    /// fallback. The conservative default for an unfamiliar server.
    Tolerant,
    /// Structured deltas (parsed the same way as `Structured`) PLUS the
    /// Hermes inline `<tool_call>...</tool_call>` text-content fallback
    /// (vllm#31871) for servers that sometimes emit a tool call as raw text
    /// instead of a structured `delta.tool_calls` entry.
    HermesTextFallback,
}

impl Default for ToolCallStyle {
    /// The most permissive parser and no inline-text scanning — the safe
    /// choice for a profile that does not name a style explicitly (P-10: a
    /// missing field must never silently enable behavior a provider didn't
    /// ask for, and `Tolerant` is a superset of `Structured`'s accepted
    /// shapes).
    fn default() -> Self {
        ToolCallStyle::Tolerant
    }
}

/// The dialect-independent parse of one raw provider tool-call delta
/// object — the output of [`openai::parse_delta`]/[`ollama::parse_delta`]
/// and the input to [`ToolCallAccumulator`]'s core accumulation logic.
#[derive(Debug, Clone, Default)]
pub(crate) struct DeltaParts {
    /// The delta's `index` field, when present. Present slot key.
    pub(crate) index: Option<u32>,
    /// The delta's `id` field, when present and non-empty.
    pub(crate) id: Option<String>,
    /// The delta's `function.name` field, when present and non-empty.
    pub(crate) name: Option<String>,
    /// A JSON-fragment substring of `function.arguments` to append
    /// verbatim to the slot's argument buffer (the canonical, streaming
    /// case).
    pub(crate) arguments_fragment: Option<String>,
    /// A complete `function.arguments` JSON value (the Ollama-quirk case:
    /// arguments arrive as an object, not a string fragment). Overwrites
    /// rather than appends.
    pub(crate) arguments_value: Option<Value>,
}

/// One in-flight (or, at `finish`, complete) tool call's accumulated state.
#[derive(Debug, Default)]
struct Slot {
    id: Option<String>,
    name: Option<String>,
    /// Concatenated `arguments` string fragments, verbatim — never
    /// trimmed, never re-encoded.
    args: String,
    /// A complete `arguments` value delivered directly (Ollama quirk).
    /// When present, takes priority over `args` at `finish`.
    args_value: Option<Value>,
}

/// Accumulates streamed tool-call deltas (or fully-formed non-streaming
/// calls) into complete, schema-validated [`ToolCall`] values.
///
/// `new` never fails even though schema compilation can: a schema that
/// fails to compile is deferred and surfaced as a `BackendError::BadRequest`
/// from the first call to `push_delta`/`push_complete`/`finish` — the
/// `new(Dialect, &[ToolSpec]) -> Self` signature in this item's spec is
/// infallible, so the "compile once at `new()`" requirement (owned by
/// `validate.rs`) is satisfied by compiling eagerly inside `new` and
/// storing the `Result`, rather than by making `new` itself fallible.
pub struct ToolCallAccumulator {
    style: ToolCallStyle,
    specs: HashMap<ToolName, ToolSpec>,
    validator: Result<SchemaValidator, BackendError>,
    slots: BTreeMap<u32, Slot>,
    /// First-seen `id` -> slot key, so a repeated `id` (codex#7517) always
    /// resolves back to the slot it first opened.
    id_to_key: HashMap<String, u32>,
    next_index: u32,
    /// The key most recently written to, used as the fallback "current
    /// slot" when a delta carries neither `index` nor a previously-seen
    /// `id`.
    last_key: Option<u32>,
    /// The Hermes inline-text scanner (WI-022), `Some` only for
    /// `ToolCallStyle::HermesTextFallback`.
    hermes: Option<HermesTextScanner>,
    /// Whether a structured `delta.tool_calls` entry has been seen while
    /// `style` is `ToolCallStyle::HermesTextFallback` — once true,
    /// `push_content_delta` stops routing through `hermes` (the
    /// "structured-path passthrough" rule).
    structured_seen: bool,
}

impl ToolCallAccumulator {
    /// Builds an accumulator for `style`, compiling every `spec.schema`
    /// once up front (see the struct docs for why compile errors are
    /// deferred rather than returned here).
    pub fn new(style: ToolCallStyle, specs: &[ToolSpec]) -> Self {
        let validator = SchemaValidator::compile(specs);
        let specs = specs
            .iter()
            .map(|spec| (spec.name.clone(), spec.clone()))
            .collect();
        let hermes =
            matches!(style, ToolCallStyle::HermesTextFallback).then(HermesTextScanner::new);
        Self {
            style,
            specs,
            validator,
            slots: BTreeMap::new(),
            id_to_key: HashMap::new(),
            next_index: 0,
            last_key: None,
            hermes,
            structured_seen: false,
        }
    }

    /// Feeds one raw provider delta object (the element of
    /// `choices[0].delta.tool_calls`). For `ToolCallStyle::HermesTextFallback`,
    /// this also marks `structured_seen`, disabling the Hermes inline-text
    /// fallback for the remainder of the stream (the "structured-path
    /// passthrough when structured tool_calls appear" rule).
    pub fn push_delta(&mut self, raw: &str) -> Result<(), BackendError> {
        let parts = self.parse(raw)?;
        if matches!(self.style, ToolCallStyle::HermesTextFallback) {
            self.structured_seen = true;
        }
        self.apply(parts)
    }

    /// Feeds one `delta.content` text fragment (WI-022). While `style` is
    /// `ToolCallStyle::HermesTextFallback` and no structured
    /// `delta.tool_calls` entry has arrived yet, this routes `text` through
    /// the Hermes inline `<tool_call>...</tool_call>` scanner: plain text is
    /// returned for the caller to emit as a `TextDelta` (`None`/empty when
    /// everything fed was suppressed), and any completed `<tool_call>`
    /// block is fed to [`Self::push_complete`] with a synthesized
    /// `call_{n}` id. Every other style (and `HermesTextFallback` once a
    /// structured call has appeared) is a pure passthrough.
    pub fn push_content_delta(&mut self, text: &str) -> Result<Option<String>, BackendError> {
        if self.structured_seen {
            return Ok(Some(text.to_string()));
        }
        let Some(scanner) = self.hermes.as_mut() else {
            return Ok(Some(text.to_string()));
        };
        let result = scanner.feed(text)?;
        for (id, name, arguments) in result.calls {
            self.push_complete(Some(id), name, arguments)?;
        }
        Ok(if result.text.is_empty() {
            None
        } else {
            Some(result.text)
        })
    }

    /// Whether the Hermes inline-text fallback (`VllmHermes` only) has
    /// parsed at least one `<tool_call>` block. When `Some`, the caller
    /// should treat the stream's stop reason as `StopReason::ToolUse`
    /// regardless of the provider-reported `finish_reason` (vllm#31871: a
    /// server emitting tool calls as inline text commonly still reports
    /// `finish_reason:"stop"`). Callable before `finish` consumes `self`.
    pub fn stop_override(&self) -> Option<StopReason> {
        if self
            .hermes
            .as_ref()
            .is_some_and(HermesTextScanner::saw_any_call)
        {
            Some(StopReason::ToolUse)
        } else {
            None
        }
    }

    /// Feeds a fully-formed non-streaming tool call (shared path with
    /// `generate`). Always opens its own slot: a complete call is never a
    /// fragment of another.
    pub fn push_complete(
        &mut self,
        id: Option<String>,
        name: String,
        arguments: Value,
    ) -> Result<(), BackendError> {
        let key = self.next_index;
        self.next_index += 1;
        self.slots.insert(
            key,
            Slot {
                id,
                name: Some(name),
                args: String::new(),
                args_value: Some(arguments),
            },
        );
        self.last_key = Some(key);
        Ok(())
    }

    /// Validates and drains every accumulated slot, in ascending
    /// (arrival) key order. Called on `finish_reason`/`stop_reason`
    /// arrival.
    ///
    /// `stop` gates nothing here: some servers report a stop reason other
    /// than `ToolUse` (e.g. `stop`/`length`) alongside genuine tool calls,
    /// so accumulated slots are always validated and returned regardless of
    /// `stop`'s value. Zero accumulated slots always yields `Ok(vec![])`,
    /// independent of `stop`.
    pub fn finish(self, stop: StopReason) -> Result<Vec<ToolCall>, BackendError> {
        let _ = stop;
        if let Some(scanner) = self.hermes {
            if !self.structured_seen {
                // Flushes any residual held-back text and errors on an
                // unterminated `<tool_call>` block (vllm#31871: a
                // truncated inline tool call must not be silently
                // dropped). The flushed text itself has no consumer at
                // `finish` time — any prior `TextDelta` already carried
                // whatever was safe to emit as it arrived.
                scanner.finish()?;
            }
        }
        let validator = self.validator?;
        let specs = self.specs;
        let mut calls = Vec::with_capacity(self.slots.len());
        for (index, slot) in self.slots.into_iter() {
            let name = slot.name.ok_or_else(|| BackendError::ToolParse {
                detail: format!("tool call at index {index} never received a name"),
            })?;
            let tool_name = ToolName::new(name.clone());
            if !specs.contains_key(&tool_name) {
                return Err(BackendError::ToolParse {
                    detail: format!("unknown tool `{name}` at index {index}"),
                });
            }
            let arguments = if let Some(value) = slot.args_value {
                value
            } else if slot.args.trim().is_empty() {
                Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str::<Value>(&slot.args).map_err(|_| {
                    let excerpt = truncate_chars(&slot.args, 256);
                    BackendError::ToolParse {
                        detail: format!(
                            "tool `{name}`: unterminated JSON arguments (truncated to 256 chars): {excerpt}"
                        ),
                    }
                })?
            };
            validator.validate(&tool_name, &arguments)?;
            let call_id = slot.id.unwrap_or_else(|| format!("call_{index}"));
            calls.push(ToolCall {
                call_id,
                name: tool_name,
                arguments,
            });
        }
        Ok(calls)
    }

    /// Whether no slot has been opened yet.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Style-dispatched delta parse.
    fn parse(&self, raw: &str) -> Result<DeltaParts, BackendError> {
        match self.style {
            ToolCallStyle::Structured | ToolCallStyle::HermesTextFallback => {
                openai::parse_delta(raw)
            }
            ToolCallStyle::Tolerant => ollama::parse_delta(raw),
        }
    }

    /// Resolves `parts` to a slot key (opening a fresh slot when
    /// warranted) and applies its content to that slot.
    fn apply(&mut self, parts: DeltaParts) -> Result<(), BackendError> {
        let key = self.resolve_key(&parts);
        let slot = self.slots.entry(key).or_default();

        if let Some(id) = parts.id.filter(|value| !value.is_empty()) {
            if slot.id.is_none() {
                slot.id = Some(id);
            }
        }

        if let Some(name) = parts.name.filter(|value| !value.is_empty()) {
            match &slot.name {
                None => slot.name = Some(name),
                Some(existing) if *existing == name => {}
                Some(_) => {
                    return Err(BackendError::ToolParse {
                        detail: format!("conflicting tool name for index {key}"),
                    });
                }
            }
        }

        if let Some(value) = parts.arguments_value {
            slot.args_value = Some(value);
        } else if let Some(fragment) = parts.arguments_fragment {
            if !fragment.is_empty() {
                slot.args.push_str(&fragment);
            }
        }

        Ok(())
    }

    /// See the module-level "Accumulation model" docs.
    fn resolve_key(&mut self, parts: &DeltaParts) -> u32 {
        let key = if let Some(index) = parts.index {
            self.next_index = self.next_index.max(index + 1);
            index
        } else if let Some(id) = parts.id.as_deref().filter(|value| !value.is_empty()) {
            if let Some(&existing) = self.id_to_key.get(id) {
                existing
            } else {
                self.next_key_or_current(parts)
            }
        } else {
            self.next_key_or_current(parts)
        };

        if let Some(id) = parts.id.as_deref().filter(|value| !value.is_empty()) {
            self.id_to_key.entry(id.to_string()).or_insert(key);
        }
        self.last_key = Some(key);
        key
    }

    /// The "open a new slot only when the current one looks finished"
    /// fallback used when `index`/a known `id` did not already determine
    /// the key.
    fn next_key_or_current(&mut self, parts: &DeltaParts) -> u32 {
        if self.should_open_new_slot(parts) {
            let key = self.next_index;
            self.next_index += 1;
            key
        } else if let Some(current) = self.last_key {
            current
        } else {
            let key = self.next_index;
            self.next_index += 1;
            key
        }
    }

    fn should_open_new_slot(&self, parts: &DeltaParts) -> bool {
        let name_non_empty = parts.name.as_deref().is_some_and(|name| !name.is_empty());
        if !name_non_empty {
            return false;
        }
        let Some(last_key) = self.last_key else {
            return false;
        };
        let Some(slot) = self.slots.get(&last_key) else {
            return false;
        };
        let has_name = slot.name.is_some();
        let args_complete = slot.args_value.is_some() || is_complete_json(&slot.args);
        has_name && args_complete
    }
}

/// Whether `s` parses as a complete JSON value on its own — the
/// operationalization of "syntactically complete argument buffer" used by
/// [`ToolCallAccumulator::should_open_new_slot`].
fn is_complete_json(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty() && serde_json::from_str::<Value>(trimmed).is_ok()
}

/// Truncates `s` to at most `max` `char`s (not bytes — delta payloads are
/// embedded in an error `String` for a human, so a `char` bound is the more
/// useful contract than a byte bound), used for the two "bounded to 256
/// chars" requirements: an unparseable raw delta, and an unterminated
/// argument-buffer excerpt at `finish`.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_bounds_at_max() {
        let long = "a".repeat(300);
        assert_eq!(truncate_chars(&long, 256).chars().count(), 256);
        assert_eq!(truncate_chars("short", 256), "short");
    }

    #[test]
    fn is_complete_json_rejects_partial_and_empty() {
        assert!(!is_complete_json(""));
        assert!(!is_complete_json("   "));
        assert!(!is_complete_json(r#"{"path":"a"#));
        assert!(is_complete_json(r#"{"path":"a.txt"}"#));
        assert!(is_complete_json("{}"));
    }
}
