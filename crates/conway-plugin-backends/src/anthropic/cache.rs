//! `CacheMode::ExplicitBreakpoints` cache-hint → `cache_control` mapping
//! (architecture §"Module: conway-backends").
//!
//! [`apply_cache_hints`] is the ONLY place in this crate that writes
//! `cache_control`. It is a strictly additive post-pass over the body
//! `wire::build_request_body` produces — that function never reads
//! `cache_hint` at all, so the byte-identity invariant (§4.1: "if a hint is
//! dropped, output must be byte-for-byte the same request content") holds
//! by construction: a request with every `cache_hint` stripped never enters
//! this function's retained-breakpoint set, so its body is never mutated.

use conway_core::capabilities::CacheTtl;
use conway_core::segment::PromptSegment;
use serde_json::{json, Value};

use super::wire::BreakpointTarget;

/// Attaches `cache_control` to the last content block of every segment
/// whose `cache_hint.breakpoint` is `true`, capped at `max_breakpoints`.
///
/// Selection: collect the indices of breakpointed segments; if there are
/// more than `max_breakpoints`, drop the EARLIEST ones and keep the last
/// `max_breakpoints` in segment order (§5.3 trim priority: B, the fork
/// boundary, is later in segment order than A, the tool-schema boundary, so
/// "keep the last N" implements "B > A > everything else").
pub(crate) fn apply_cache_hints(
    body: &mut Value,
    segments: &[PromptSegment],
    placements: &[BreakpointTarget],
    max_breakpoints: u8,
) {
    let candidate_indices: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter_map(|(index, segment)| {
            segment
                .cache_hint
                .as_ref()
                .and_then(|hint| hint.breakpoint.then_some(index))
        })
        .collect();

    let max = max_breakpoints as usize;
    let retained = if candidate_indices.len() > max {
        &candidate_indices[candidate_indices.len() - max..]
    } else {
        candidate_indices.as_slice()
    };

    for &index in retained {
        let hint = segments[index]
            .cache_hint
            .as_ref()
            .expect("index came from a filter over segments with cache_hint.breakpoint == true");
        let cache_control = cache_control_json(hint.ttl);
        match placements.get(index) {
            Some(BreakpointTarget::System(entry_index)) => {
                if let Some(Value::Object(entry)) =
                    body.get_mut("system").and_then(|s| s.get_mut(*entry_index))
                {
                    entry.insert("cache_control".into(), cache_control);
                }
            }
            Some(BreakpointTarget::Message {
                message_index,
                block_index,
            }) => {
                if let Some(Value::Object(block)) = body
                    .get_mut("messages")
                    .and_then(|m| m.get_mut(*message_index))
                    .and_then(|m| m.get_mut("content"))
                    .and_then(|c| c.get_mut(*block_index))
                {
                    block.insert("cache_control".into(), cache_control);
                }
            }
            // Anthropic docs, "Prompt caching" > "Caching tool
            // definitions": "placing `cache_control` on the last tool in
            // your `tools` array" caches every tool defined before and
            // including it as a single prefix -- so this targets the LAST
            // element regardless of which segment's `cache_hint` requested
            // it (there is only ever one `Provenance::ToolRegistry`
            // segment/breakpoint A). No-op when `tools` is absent or empty,
            // same as `None`.
            Some(BreakpointTarget::Tools) => {
                if let Some(Value::Object(last_tool)) = body
                    .get_mut("tools")
                    .and_then(Value::as_array_mut)
                    .and_then(|tools| tools.last_mut())
                {
                    last_tool.insert("cache_control".into(), cache_control);
                }
            }
            Some(BreakpointTarget::None) | None => {}
        }
    }
}

/// `CacheTtl::OneHour` → `{"type":"ephemeral","ttl":"1h"}`;
/// `CacheTtl::FiveMinutes` → `{"type":"ephemeral"}` (no `ttl` key). Any
/// other (future, `CacheTtl` is `#[non_exhaustive]`) value downgrades to the
/// nearest currently-supported value — `FiveMinutes` — rather than erroring.
fn cache_control_json(ttl: CacheTtl) -> Value {
    match ttl {
        CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
        CacheTtl::FiveMinutes => json!({"type": "ephemeral"}),
        _ => json!({"type": "ephemeral"}),
    }
}

#[cfg(test)]
mod tests {
    use conway_core::content::{ContentBlock, Role};
    use conway_core::ids::PrefixKey;
    use conway_core::provenance::Provenance;
    use conway_core::segment::CacheHint;

    use super::*;

    fn segment_with_hint(text: &str, breakpoint: bool, ttl: CacheTtl) -> PromptSegment {
        PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text { text: text.into() }],
            Provenance::AgentDef { name: "r".into() },
        )
        .with_cache_hint(CacheHint {
            breakpoint,
            ttl,
            prefix_key: "k".parse::<PrefixKey>().unwrap(),
        })
    }

    #[test]
    fn cache_control_json_matches_ttl_table() {
        assert_eq!(
            cache_control_json(CacheTtl::OneHour),
            json!({"type": "ephemeral", "ttl": "1h"})
        );
        let five_min = cache_control_json(CacheTtl::FiveMinutes);
        assert_eq!(five_min, json!({"type": "ephemeral"}));
        assert!(five_min.get("ttl").is_none());
    }

    #[test]
    fn six_breakpointed_segments_retain_only_the_last_four() {
        let segments: Vec<PromptSegment> = (0..6)
            .map(|i| segment_with_hint(&format!("s{i}"), true, CacheTtl::FiveMinutes))
            .collect();
        let placements: Vec<BreakpointTarget> = (0..6).map(BreakpointTarget::System).collect();
        let mut body = json!({
            "system": (0..6).map(|i| json!({"type":"text","text": format!("s{i}")})).collect::<Vec<_>>()
        });

        apply_cache_hints(&mut body, &segments, &placements, 4);

        let system = body["system"].as_array().unwrap();
        let has_cache_control: Vec<bool> = system
            .iter()
            .map(|entry| entry.get("cache_control").is_some())
            .collect();
        assert_eq!(
            has_cache_control,
            vec![false, false, true, true, true, true]
        );
    }

    #[test]
    fn non_breakpointed_segments_are_never_touched() {
        let segments = vec![segment_with_hint("s0", false, CacheTtl::FiveMinutes)];
        let placements = vec![BreakpointTarget::System(0)];
        let mut body = json!({ "system": [{"type": "text", "text": "s0"}] });
        apply_cache_hints(&mut body, &segments, &placements, 4);
        assert!(body["system"][0].get("cache_control").is_none());
    }

    /// Anthropic's documented anchor
    /// for caching tool definitions is `cache_control` on the LAST entry of
    /// `body["tools"]` -- `BreakpointTarget::Tools` must land there, never
    /// in `body["system"]` (there is nothing to attach to there since the
    /// `ToolRegistry` segment contributes no system entry at all).
    #[test]
    fn tools_breakpoint_attaches_cache_control_to_the_last_tool() {
        let segments = vec![segment_with_hint("", true, CacheTtl::FiveMinutes)];
        let placements = vec![BreakpointTarget::Tools];
        let mut body = json!({
            "tools": [
                {"name": "search", "description": "d", "input_schema": {}},
                {"name": "fetch", "description": "d", "input_schema": {}},
            ]
        });

        apply_cache_hints(&mut body, &segments, &placements, 4);

        assert!(
            body["tools"][0].get("cache_control").is_none(),
            "only the LAST tool gets the marker"
        );
        assert_eq!(
            body["tools"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    /// No `tools` array (or an empty one) means there is no addressable
    /// block -- the hint is silently dropped, same as `BreakpointTarget::
    /// None`, not a panic or a spurious mutation.
    #[test]
    fn tools_breakpoint_is_a_no_op_when_tools_is_absent_or_empty() {
        let segments = vec![segment_with_hint("", true, CacheTtl::FiveMinutes)];
        let placements = vec![BreakpointTarget::Tools];

        let mut no_tools_body = json!({});
        apply_cache_hints(&mut no_tools_body, &segments, &placements, 4);
        assert_eq!(no_tools_body, json!({}));

        let mut empty_tools_body = json!({"tools": []});
        apply_cache_hints(&mut empty_tools_body, &segments, &placements, 4);
        assert_eq!(empty_tools_body, json!({"tools": []}));
    }
}
