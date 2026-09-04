//! Wire-identity pin for `SubagentSpec` (board item `01M1FSPP9D80FNXH9QFZ9927DH`,
//! "shared agent knobs by composition, conversions become spreads").
//!
//! `SubagentSpec` is the one of the six specs this item touches that is
//! actually `Serialize`/`Deserialize` (`ForkSpec`/`SpawnSpec`/`SessionSpec`/
//! `RootSpec`/`ResumeSpec` derive none of the three) — confirmed by grepping
//! the whole tree for `serde_json::to_value`/`to_string` near any of the six
//! type names, and by reading each struct's own `#[derive(..)]` line
//! directly. So this is the one wire-compatibility surface this refactor
//! (folding `agent_def`/`role`/`pin`/`tools`/`budget`/`result_contract`/
//! `keep_alive` into a shared `AgentKnobs` embedded via `#[serde(flatten)]`)
//! can silently break.
//!
//! **Committed BEFORE the refactor** (this item's own sequencing
//! requirement): this fixture and test are written and passing against the
//! PRE-refactor `SubagentSpec` shape (every field declared directly on the
//! struct) in their own commit; the refactor commit that follows must leave
//! this file byte-for-byte unchanged and still green.
//!
//! `serde_json::Value` equality (not raw string equality) is the comparison:
//! JSON key order is not part of the wire contract any real consumer reads
//! (an object's key order is semantically insignificant in JSON, and
//! `#[serde(flatten)]` is free to interleave the flattened struct's keys
//! differently than a flat struct would) -- what must not change is the
//! actual key/value *content*. `serde_json::Value`'s `Map` is a `BTreeMap`
//! by default in this workspace (no `preserve_order` feature enabled on the
//! `serde_json` dependency), so equality here is already order-independent
//! by construction, not merely by choice of assertion style.
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use conway_core::agent::{
    AgentDefRef, Budget, SubagentMode, SubagentSpec, ToolSelector,
};
use conway_core::ids::{LogSeq, ModelRef, RoleAlias, SessionId};
use conway_core::log::AskOrigin;
use conway_core::path::RecordRef;
use conway_core::ports::PluginConfig;

const FIXTURE: &str = include_str!("fixtures/subagent_spec_full.json");

/// Every field set to a concrete, non-default value -- the point of a
/// wire-identity fixture is to exercise every key, not just the ones a
/// constructor happens to populate.
fn fully_populated_spec() -> SubagentSpec {
    let deadline: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
    let pin: ModelRef = "anthropic/claude-haiku".parse().unwrap();
    let mut plugin_values = serde_json::Map::new();
    plugin_values.insert("conway.fs.root".to_string(), serde_json::json!("/scoped"));
    let schema: schemars::schema::RootSchema = serde_json::from_value(serde_json::json!({
        "type": "object",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"],
    }))
    .unwrap();

    SubagentSpec {
        mode: SubagentMode::Fork,
        prompt: "do the thing".to_string(),
        agent_def: Some(AgentDefRef("reviewer".into())),
        role: Some(RoleAlias::new("planner")),
        pin: Some(pin),
        tools: Some(ToolSelector::Only(vec!["read".into()])),
        budget: Budget {
            max_steps: 7,
            deadline: Some(deadline),
            max_tokens: Some(100),
            max_tool_calls: Some(3),
        },
        result_contract: Some(schema),
        keep_alive: false,
        ephemeral: true,
        ask_origin: Some(AskOrigin::ToolAsk),
        cwd: Some(PathBuf::from("/tmp/child")),
        root: Some(PathBuf::from("/tmp/root")),
        tag: Some("my-tag".to_string()),
        plugin_config: Some(PluginConfig {
            values: plugin_values,
        }),
        context: Some(vec![RecordRef {
            session: "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse::<SessionId>().unwrap(),
            seq: LogSeq(3),
        }]),
    }
}

#[test]
fn fully_populated_spec_serializes_to_the_committed_fixture() {
    let spec = fully_populated_spec();
    let actual = serde_json::to_value(&spec).expect("SubagentSpec must serialize");
    let expected: serde_json::Value =
        serde_json::from_str(FIXTURE).expect("fixture must itself be valid JSON");
    assert_eq!(
        actual, expected,
        "SubagentSpec's wire form changed -- if this refactor is what changed it, that is \
         exactly the regression this test exists to catch: fix the struct/serde attributes so \
         the wire form matches, do not regenerate the fixture to match the new output"
    );
}

#[test]
fn the_committed_fixture_still_deserializes_back_to_the_same_spec() {
    let spec = fully_populated_spec();
    let round_tripped: SubagentSpec =
        serde_json::from_str(FIXTURE).expect("fixture must deserialize as SubagentSpec");
    assert_eq!(round_tripped, spec);
}
