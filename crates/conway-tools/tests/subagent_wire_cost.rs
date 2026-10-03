//! Measures the real per-request wire cost of conway's own built-in
//! subagent tools -- `conway_fork`/`conway_ask`/`conway_cancel`/
//! `conway_spawn`/`conway_steer` -- the "largest fixed cost" DOGFOOD 3
//! (2026-09-30) identified: these five are announced on EVERY request
//! (`conway-plugin-toolindex` never defers a built-in -- see that crate's
//! own module doc), so unlike an MCP server's tools they are not a cost an
//! operator can opt out of paying.
//!
//! `docs/plugins/toolindex.md`'s own "The problem, measured" section
//! carries the numbers this test asserts, by name, so a description/schema
//! edit that meaningfully changes one of these costs is required to update
//! that doc in the same change (this test's own failure is the prompt).
//!
//! No `test-fakes` feature needed: every assertion here reads `Tool::spec()`
//! alone, which takes no `ToolCtx` and touches no subagent host.

use conway_core::content::ToolSpec;
use conway_core::ports::Tool;
use conway_tools::subagent::{AskTool, CancelTool, ForkTool, SpawnTool, SteerTool};

/// The exact wire shape `OpenAiCompatBackend` sends for one tool
/// (`crates/conway-plugin-backends/src/openai_compat/wire.rs`'s own
/// `build_request`): `{"type":"function","function":{"name":...,
/// "description":...,"parameters":...}}`. Reproduced here rather than
/// reached through that crate (which `conway-tools` must not depend on --
/// this crate's own module doc, "architecture boundary rule") -- the exact
/// JSON shape, not an approximation, since DOGFOOD 3's own figures were
/// read off the real wire in this exact form.
fn wire_json_chars(spec: &ToolSpec) -> usize {
    let value = serde_json::json!({
        "type": "function",
        "function": {
            "name": spec.name.as_str(),
            "description": spec.description,
            "parameters": spec.schema,
        }
    });
    serde_json::to_string(&value)
        .expect("a real ToolSpec always serializes")
        .len()
}

#[test]
fn every_subagent_tools_wire_cost_is_measured_and_pinned() {
    let costs: Vec<(&str, usize)> = vec![
        ("conway_fork", wire_json_chars(&ForkTool::new().spec())),
        ("conway_ask", wire_json_chars(&AskTool::new().spec())),
        ("conway_cancel", wire_json_chars(&CancelTool::new().spec())),
        ("conway_spawn", wire_json_chars(&SpawnTool::new().spec())),
        ("conway_steer", wire_json_chars(&SteerTool::new().spec())),
    ];

    // Pinned to the measured figures `docs/plugins/toolindex.md`'s own
    // "The problem, measured" section quotes -- a failure here means that
    // doc's numbers are now stale and must be re-measured and updated
    // together with whatever changed a tool's description/schema.
    let expected: Vec<(&str, usize)> = vec![
        ("conway_fork", 2_602),
        ("conway_ask", 2_544),
        ("conway_cancel", 2_380),
        ("conway_spawn", 2_188),
        ("conway_steer", 840),
    ];
    assert_eq!(
        costs, expected,
        "a subagent tool's wire cost changed -- re-measure and update \
         docs/plugins/toolindex.md's own \"The problem, measured\" section to match"
    );

    let total: usize = costs.iter().map(|(_, n)| n).sum();
    assert!(
        total > 3_000,
        "sanity: five real tool descriptions plus schemas should comfortably exceed 3,000 \
         wire chars on their own (got {total}) -- a near-zero total would mean this test is \
         measuring something other than the real announced tools"
    );
}
