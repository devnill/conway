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
//! # Dialect dispatch
//!
//! `push_delta` dispatches to a dialect-specific parser that turns one raw
//! provider delta object into dialect-independent [`DeltaParts`]:
//! [`openai::parse_delta`] for the OpenAI-canonical shape, reused unchanged
//! for `VllmHermes`'s structured (non-text) tool-call path; and
//! [`ollama::parse_delta`], a tolerant superset parser, for `Ollama` and
//! (per the WI-022 handoff below) `LmStudio`/`LlamaCppServer`.
//!
//! `Dialect` itself is **not** defined in this module. It already exists as
//! `crate::config::Dialect` (added by WI-016, the crate skeleton item) with
//! exactly the five variants this work item's spec called for
//! (`OpenAi | Ollama | VllmHermes | LmStudio | LlamaCppServer`), so no
//! second definition is introduced here — see this work item's completion
//! report for the resolved ambiguity.
//!
//! # WI-022 handoff
//!
//! This item (WI-018) implements and tests only the `OpenAi` and `Ollama`
//! dialect arms. The `VllmHermes`, `LmStudio`, and `LlamaCppServer` arms in
//! [`ToolCallAccumulator::parse`] below are wired to a reasonable existing
//! parser (matching the WI-022 module notes: `LmStudio`/`LlamaCppServer`
//! use the `ollama.rs` tolerant parser; `VllmHermes` uses `openai.rs` for
//! its structured path) so the type compiles for all five `Dialect`
//! variants, but none of the three are exercised by this item's tests.
//! WI-022 additionally owns the Hermes inline-text (`<tool_call>…`)
//! fallback path (`hermes.rs`, not present here) and is expected to revisit
//! the `VllmHermes` arm to add that fallback when the dialect's
//! `delta.tool_calls` is empty.

mod ollama;
mod openai;
mod validate;

use std::collections::{BTreeMap, HashMap};

use conway_core::content::{StopReason, ToolCall, ToolSpec};
use conway_core::error::BackendError;
use conway_core::ids::ToolName;
use serde_json::Value;

use crate::config::Dialect;
use validate::SchemaValidator;

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
    dialect: Dialect,
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
}

impl ToolCallAccumulator {
    /// Builds an accumulator for `dialect`, compiling every `spec.schema`
    /// once up front (see the struct docs for why compile errors are
    /// deferred rather than returned here).
    pub fn new(dialect: Dialect, specs: &[ToolSpec]) -> Self {
        let validator = SchemaValidator::compile(specs);
        let specs = specs
            .iter()
            .map(|spec| (spec.name.clone(), spec.clone()))
            .collect();
        Self {
            dialect,
            specs,
            validator,
            slots: BTreeMap::new(),
            id_to_key: HashMap::new(),
            next_index: 0,
            last_key: None,
        }
    }

    /// Feeds one raw provider delta object (the element of
    /// `choices[0].delta.tool_calls`).
    pub fn push_delta(&mut self, raw: &str) -> Result<(), BackendError> {
        let parts = self.parse(raw)?;
        self.apply(parts)
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

    /// Dialect-dispatched delta parse. See the module-level "WI-022
    /// handoff" docs for the three arms this item does not exercise.
    fn parse(&self, raw: &str) -> Result<DeltaParts, BackendError> {
        match self.dialect {
            Dialect::OpenAi | Dialect::VllmHermes => openai::parse_delta(raw),
            Dialect::Ollama | Dialect::LmStudio | Dialect::LlamaCppServer => {
                ollama::parse_delta(raw)
            }
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
