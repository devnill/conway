//! Measures the real per-request wire cost of conway's own built-in
//! subagent tools -- `conway_fork`/`conway_ask`/`conway_cancel`/
//! `conway_spawn`/`conway_steer`/`conway_await` -- the "largest fixed cost"
//! DOGFOOD 3 (2026-09-30) identified: all six are announced on EVERY
//! request (`conway-plugin-toolindex` never defers a built-in -- see that
//! crate's own module doc), so unlike an MCP server's tools they are not a
//! cost an operator can opt out of paying.
//!
//! `docs/plugins/toolindex.md`'s own "The problem, measured" section
//! carries the numbers this test asserts, by name, so a description/schema
//! edit that meaningfully changes one of these costs is required to update
//! that doc in the same change (this test's own failure is the prompt).
//!
//! No `test-fakes` feature needed: every assertion here reads `Tool::spec()`
//! alone, which takes no `ToolCtx` and touches no subagent host.
//!
//! # Budget, not an exact pin (board item 01M41BC4KJAE8J1X3GAA36ZW6Y)
//!
//! Before this item, this test covered only five of the six tools
//! (`conway_await` was never included) and pinned each one's cost to an
//! exact figure (2,602/2,544/2,380/2,188/840, total 10,554) -- a snapshot
//! that forced an update here on every wording change, with no signal for
//! WHICH direction a change should go.
//!
//! The operator's ruling (2026-10-03, DOGFOOD 3's ~2.6k-token finding) was
//! to shorten these descriptions/schemas to the model-facing contract
//! alone -- moving rationale and caching-mechanics prose that does not
//! change how the tool is called into `docs/agents.md`/`docs/tools.md`
//! instead -- and to drop the default session's `tool_registry` context
//! segment (`conway_runtime::context::builder`'s own `[2] ToolSchemas`,
//! `estimate_tool_schemas_tokens`) by at least 1,500 estimated
//! (`heuristic-chars4`) tokens, i.e. at least 6,000 wire chars among these
//! six tools (since `conway.toolindex`'s `always_announced_names` keeps
//! every one of them fully announced always -- see that crate's own module
//! doc -- they are exactly and only what that segment costs for a session
//! with no other tools installed). This is now enforced two ways, both
//! against the REAL baseline reconstructed below (`conway_await` included,
//! total 11,379 -- not the five-tool, pre-`conway_await` 10,554 the old pin
//! quoted):
//!   1. a per-tool ceiling (`BUDGETS`), each comfortably above the
//!      post-shortening figure measured when these budgets were set, to
//!      absorb a small future wording tweak without churn, while staying
//!      far below the original figure in `BASELINE_TOTAL_CHARS_BY_TOOL`
//!      below -- a regression back toward the old prose trips this long
//!      before reaching the original number;
//!   2. the six-tool total must still clear the ruling's own
//!      [`MIN_REDUCTION_CHARS`] drop from [`BASELINE_TOTAL_CHARS`], a
//!      stricter aggregate backstop that catches creep even if no single
//!      tool individually breaches its own budget.
//!
//! A future edit may still shrink a tool's wording further (both checks
//! only cap growth); growing back past either one is the regression this
//! test exists to catch.
use conway_core::content::ToolSpec;
use conway_core::ports::Tool;
use conway_tools::subagent::{AskTool, AwaitTool, CancelTool, ForkTool, SpawnTool, SteerTool};

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

/// Pre-shortening baseline, by tool, reconstructed by hand from each
/// tool's real pre-edit description/schema and verified to reproduce the
/// exact five-tool figures this test used to pin to (2,602/2,544/2,380/
/// 2,188/840) before `conway_await` was added to this test's scope.
/// `docs/plugins/toolindex.md`'s own "The problem, measured" table still
/// quotes the five-tool subset of this.
const BASELINE_TOTAL_CHARS_BY_TOOL: [(&str, usize); 6] = [
    ("conway_fork", 2_602),
    ("conway_ask", 2_544),
    ("conway_cancel", 2_380),
    ("conway_spawn", 2_188),
    ("conway_steer", 840),
    ("conway_await", 825),
];

/// Sum of [`BASELINE_TOTAL_CHARS_BY_TOOL`] -- all six tools, the real
/// pre-shortening floor the ruling measured against.
const BASELINE_TOTAL_CHARS: usize = 11_379;

/// The ruling's own target: the tool-registry segment must drop by at
/// least 1,500 `heuristic-chars4`-estimated tokens (`chars.div_ceil(4)`,
/// `conway_runtime::context::builder::estimate_tool_schemas_tokens`) --
/// i.e. at least 6,000 wire chars, across these six tools.
const MIN_REDUCTION_CHARS: usize = 6_000;

/// Per-tool ceilings -- see this file's own module doc, point 1.
const BUDGETS: [(&str, usize); 6] = [
    ("conway_fork", 1_600),
    ("conway_ask", 1_300),
    ("conway_cancel", 1_000),
    ("conway_spawn", 1_600),
    ("conway_steer", 600),
    ("conway_await", 600),
];

#[test]
fn every_subagent_tools_wire_cost_is_measured_and_bounded() {
    let costs: Vec<(&str, usize)> = vec![
        ("conway_fork", wire_json_chars(&ForkTool::new().spec())),
        ("conway_ask", wire_json_chars(&AskTool::new().spec())),
        ("conway_cancel", wire_json_chars(&CancelTool::new().spec())),
        ("conway_spawn", wire_json_chars(&SpawnTool::new().spec())),
        ("conway_steer", wire_json_chars(&SteerTool::new().spec())),
        ("conway_await", wire_json_chars(&AwaitTool::new().spec())),
    ];
    assert_eq!(costs.len(), BUDGETS.len());
    assert_eq!(costs.len(), BASELINE_TOTAL_CHARS_BY_TOOL.len());
    // `BASELINE_TOTAL_CHARS` must itself be the sum of the per-tool baseline
    // table above it -- guards the two constants against drifting apart.
    let baseline_sum: usize = BASELINE_TOTAL_CHARS_BY_TOOL.iter().map(|(_, n)| n).sum();
    assert_eq!(baseline_sum, BASELINE_TOTAL_CHARS);

    for i in 0..BUDGETS.len() {
        let (name, cost) = costs[i];
        let (budget_name, budget) = BUDGETS[i];
        let (baseline_name, baseline) = BASELINE_TOTAL_CHARS_BY_TOOL[i];
        assert_eq!(
            name, budget_name,
            "costs and BUDGETS must list tools in the same order"
        );
        assert_eq!(
            name, baseline_name,
            "costs and BASELINE_TOTAL_CHARS_BY_TOOL must list tools in the same order"
        );
        assert!(
            cost <= budget,
            "{name}'s wire cost ({cost} chars) exceeds its budget ({budget} chars) -- \
             re-shorten the description/schema, or raise the budget here AND update \
             docs/plugins/toolindex.md's own \"The problem, measured\" section together \
             with a note on why the growth is justified"
        );
        assert!(
            cost < baseline,
            "{name}'s wire cost ({cost} chars) did not shrink from its pre-shortening \
             baseline ({baseline} chars) -- this tool's own description/schema regressed \
             back toward (or past) its original size"
        );
    }

    let total: usize = costs.iter().map(|(_, n)| n).sum();
    assert!(
        total > 2_500,
        "sanity: six real tool descriptions plus schemas should comfortably exceed 2,500 \
         wire chars on their own (got {total}) -- a near-zero total would mean this test is \
         measuring something other than the real announced tools"
    );
    let reduction = BASELINE_TOTAL_CHARS.saturating_sub(total);
    assert!(
        reduction >= MIN_REDUCTION_CHARS,
        "the ruling's own target (board item 01M41BC4KJAE8J1X3GAA36ZW6Y) was a >={MIN_REDUCTION_CHARS} \
         wire-char drop from the {BASELINE_TOTAL_CHARS}-char baseline; today's total is {total} \
         chars, a drop of only {reduction} -- do not relax MIN_REDUCTION_CHARS without a new \
         operator ruling"
    );
}
