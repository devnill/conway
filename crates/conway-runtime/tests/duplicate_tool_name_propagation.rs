//! Acceptance test for board item `01M2PJM777FFSZW8GZWD46X49B`:
//! `Runtime::new` used to `.expect()` away `PluginRegistry::from_plugins`'s
//! own `Err` for a duplicate tool name across two plugins, turning an
//! operator-caused, operator-fixable condition (two configured MCP plugins
//! that happen to register a tool with the same name) into a raw Rust
//! panic. `PluginRegistry::from_plugins`'s own message construction (naming
//! the colliding tool and both plugin ids) is ALREADY pinned by
//! `crates/conway-runtime/tests/tool_runner.rs::
//! duplicate_tool_name_errors_naming_both_plugins` -- this file does not
//! repeat that coverage. What is new, and what this file exists to prove,
//! is that [`Runtime::try_new`] actually PROPAGATES that `Err` up through
//! `Runtime`'s own construction (rather than a second, independent
//! duplicate-detection path), while [`Runtime::new`] -- kept for every
//! existing caller in this crate's own test suite that still treats a
//! malformed injected plugin set as a panic-worthy bug -- still panics on
//! the identical input, unchanged.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, PermissionClass, ToolCall, ToolCategory, ToolSpec};
use conway_core::error::ToolError;
use conway_core::ids::{BackendId, ModelId, ModelRef, ToolName};
use conway_core::ports::{Plugin, Tool, ToolCtx, ToolOutput};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{Runtime, RuntimeDeps};
use conway_testkit::{FakeGate, FakeHealth, FakePathStore, FakePlugin, FakeRouter, FakeStore};

/// A tool that does nothing when invoked -- `try_new`/`new` never reach the
/// point of calling it; only its declared name/schema matter here.
struct StubTool(ToolName);

#[async_trait]
impl Tool for StubTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.0.clone(),
            description: "stub".into(),
            schema: serde_json::json!({"type": "object"}),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "unreachable".into(),
            }],
            is_error: false,
            truncation: conway_core::content::TruncationPolicy::None,
            artifacts: vec![],
        })
    }
}

/// Two plugins, distinct ids, both declaring a tool named `"sleep"` --
/// board item `01M2PJM777FFSZW8GZWD46X49B`'s own traced shape (two
/// `[plugins].mcp[]` entries whose servers both happen to declare the same
/// tool name).
fn colliding_plugins() -> Vec<Arc<dyn Plugin>> {
    let a: Arc<dyn Plugin> = Arc::new(FakePlugin::with_id(
        "mcp.dogfood-mcp-0",
        vec![Arc::new(StubTool(ToolName::new("sleep")))],
    ));
    let b: Arc<dyn Plugin> = Arc::new(FakePlugin::with_id(
        "mcp.dogfood-mcp-1",
        vec![Arc::new(StubTool(ToolName::new("sleep")))],
    ));
    vec![a, b]
}

/// The exact `RuntimeDeps` literal `runtime_api.rs::build_runtime` already
/// establishes as this crate's own minimal-fake shape, parameterized only
/// on `plugins` -- reproduced here rather than shared, since each
/// `tests/*.rs` file is its own independent integration-test crate (no
/// cross-file `mod` reuse across this directory).
fn deps(plugins: Vec<Arc<dyn Plugin>>) -> RuntimeDeps {
    let store: Arc<dyn conway_core::ports::SessionStore> = Arc::new(FakeStore::new());
    let model = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("m"),
    };
    RuntimeDeps {
        store,
        path_store: Arc::new(FakePathStore::new()),
        router: Arc::new(FakeRouter::single(model)),
        health: Arc::new(FakeHealth::new()),
        backends: HashMap::new(),
        plugins,
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        instructions: Vec::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(conway_core::capabilities::HeadroomPolicy::default()),
        tool_result_bound: Arc::new(conway_core::capabilities::ToolResultBoundPolicy::default()),
        session_discovery: Arc::new(conway_testkit::FakeSessionDiscoveryHost::new()),
        capabilities: Arc::new(conway_core::ports::CapabilityRegistry::default()),
    }
}

/// **The headline claim.** `Runtime::try_new` returns `Err`, never panics,
/// for two plugins declaring the same tool name -- fails against HEAD (the
/// `.expect()` this item replaces) by aborting the test process instead of
/// returning.
#[test]
fn two_plugins_with_the_same_tool_name_return_err_not_a_panic() {
    let err = match Runtime::try_new(deps(colliding_plugins())) {
        Ok(_) => panic!("expected try_new to return Err for a duplicate tool name"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(message.contains("sleep"), "{message}");
    assert!(message.contains("mcp.dogfood-mcp-0"), "{message}");
    assert!(message.contains("mcp.dogfood-mcp-1"), "{message}");
}

/// A non-colliding plugin set still constructs a real, usable `Runtime`
/// through `try_new` -- the fallible path is not merely "always Err on any
/// input."
#[test]
fn a_non_colliding_plugin_set_still_succeeds_through_try_new() {
    let plugin: Arc<dyn Plugin> = Arc::new(FakePlugin::with_id(
        "mcp.only-one",
        vec![Arc::new(StubTool(ToolName::new("sleep")))],
    ));
    let result = Runtime::try_new(deps(vec![plugin]));
    assert!(
        result.is_ok(),
        "a single, non-colliding plugin must still build a Runtime"
    );
}

/// **The backward-compatibility guarantee.** `Runtime::new` -- the thin,
/// panicking wrapper every pre-existing caller in this crate's own test
/// suite still calls -- keeps panicking on the identical colliding input,
/// unchanged by this item. `conway::ConwayBuilder::build` is the one
/// caller this item actually moved onto `try_new`; every other caller was
/// deliberately left alone (`Runtime::new`'s own doc has the full
/// reasoning).
#[test]
#[should_panic(expected = "RuntimeDeps.plugins must register without duplicate tool names")]
fn runtime_new_still_panics_on_the_same_colliding_input() {
    let _ = Runtime::new(deps(colliding_plugins()));
}
