//! `[roles.<alias>.tools]` actually narrowing a live agent's announced tool
//! set (board item `01M1YS138H8T0HNV5YMZ6KD767`, part 2). `conway::config::
//! schema::RoleToolsConfig::resolve`'s own pure include/exclude AND is
//! tested in that crate; this file exercises the OTHER half -- that
//! `Runtime::set_role_tools`'s resolved `ToolSelector` per role actually
//! reaches the `GenerateRequest` a `start_root`'d agent's first turn sends,
//! via `conway_runtime::runtime::root::narrow_tools_for_role`
//! (`start_root`'s own call site; `resume_root` and `subagent.rs`'s
//! `SubagentHost::start` call the identical function).
//!
//! Deliberately built from `conway-testkit` fakes only, mirroring
//! `runtime_api.rs`'s own compile-check criterion note: no
//! `conway-plugin-backends`/`conway-tools` dependency, and (unlike a real
//! build) no `conway-plugin-toolindex` installed either -- this file counts
//! tools directly off one `FakePlugin`'s own three-tool spec, so nothing
//! here needs the "`describe_tool` adds one" adjustment a real-registry
//! count would (see the item's own process-record note on that trap).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{AgentKnobs, Budget, PermissionDecision, ToolSelector};
use conway_core::capabilities::HeadroomPolicy;
use conway_core::content::{
    ContentBlock, PermissionClass, StopReason, ToolCategory, ToolSpec, Usage,
};
use conway_core::error::ToolError;
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{
    FakeGate, FakeHealth, FakePlugin, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};
use futures::StreamExt;

fn text_response(text: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

fn schema_any_object() -> schemars::schema::RootSchema {
    serde_json::from_value(serde_json::json!({"type": "object"})).unwrap()
}

fn tool_spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name),
        description: "test tool".into(),
        schema: schema_any_object(),
        category: ToolCategory::Read,
        permission: PermissionClass::Safe,
    }
}

/// A tool that does nothing but exist, so a plugin can register several
/// distinctly-named tools and this test can assert on which of their names
/// actually reach a `GenerateRequest`.
struct NamedTool(ToolName);

#[async_trait]
impl conway_core::ports::Tool for NamedTool {
    fn spec(&self) -> ToolSpec {
        tool_spec(self.0.as_str())
    }

    async fn invoke(
        &self,
        _call: conway_core::content::ToolCall,
        _ctx: conway_core::ports::ToolCtx,
    ) -> Result<conway_core::ports::ToolOutput, ToolError> {
        unreachable!("no test here drives a tool call")
    }
}

/// Three tools from one plugin: two under the `mcp_` namespace an
/// `exclude: ["mcp_*"]` role should strip, one (`fs_read`) it should not.
fn three_tool_plugin() -> Arc<dyn conway_core::ports::Plugin> {
    Arc::new(FakePlugin::new(vec![
        Arc::new(NamedTool(ToolName::new("mcp_search"))),
        Arc::new(NamedTool(ToolName::new("mcp_write"))),
        Arc::new(NamedTool(ToolName::new("fs_read"))),
    ]))
}

fn build_runtime(
    backend: Arc<dyn conway_core::ports::Backend>,
    plugins: Vec<Arc<dyn conway_core::ports::Plugin>>,
) -> (Arc<Runtime>, Arc<dyn conway_core::ports::SessionStore>) {
    let store: Arc<dyn conway_core::ports::SessionStore> = Arc::new(FakeStore::new());
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn conway_core::ports::Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn conway_core::ports::Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let runtime = Runtime::new(RuntimeDeps {
        store: store.clone(),
        path_store: Arc::new(conway_testkit::FakePathStore::new()),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins,
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        instructions: Vec::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
        tool_result_bound: Arc::new(conway_core::capabilities::ToolResultBoundPolicy::default()),
        session_discovery: Arc::new(conway_testkit::FakeSessionDiscoveryHost::new()),
        capabilities: Arc::new(conway_core::ports::CapabilityRegistry::default()),
    });
    (runtime, store)
}

fn root_spec(role: &str, prompt: &str) -> RootSpec {
    RootSpec {
        session: None,
        knobs: AgentKnobs {
            agent_def: None,
            role: Some(RoleAlias::new(role)),
            model: None,
            tools: None,
            budget: Budget::default(),
            result_contract: None,
            keep_alive: false,
        },
        cwd: PathBuf::from("/tmp"),
        root: None,
        prompt: Some(prompt.to_string()),
        system_prompt_override: None,
        labels: Vec::new(),
    }
}

async fn wait_for_agent_finished(stream: &mut conway_runtime::events::EventStream, agent: AgentId) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent {
                if let Event::AgentFinished { .. } = envelope.event {
                    return;
                }
            }
        }
    })
    .await
    .expect("agent never finished");
}

/// A role with `tools.exclude: ["mcp_*"]` (resolved, per `RoleToolsConfig::
/// resolve`'s own committed contract, into a concrete `ToolSelector::
/// Only(["fs_read"])` against this fixture's three-tool universe) announces
/// NONE of the `mcp_*` tools -- only `fs_read` reaches the `GenerateRequest`
/// the backend actually receives. Catches a `narrow_tools_for_role` that
/// fails to filter, filters the wrong direction, or drops a survivor it
/// should keep.
#[tokio::test]
async fn role_exclude_narrows_announced_tools_to_survivors_only() {
    let backend = Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        text_response("hi"),
    )]));
    let calls_handle = backend.clone();
    let (runtime, _store) = build_runtime(backend, vec![three_tool_plugin()]);

    // What `conway::ConwayBuilder::build` would compute for
    // `[roles.narrowed.tools] exclude = ["mcp_*"]` against this fixture's
    // universe {mcp_search, mcp_write, fs_read}: `RoleToolsConfig::
    // resolve` always returns a concrete `Only(survivors)`, never bare
    // `Except` -- see that method's own doc for why.
    runtime.set_role_tools(HashMap::from([(
        RoleAlias::new("narrowed"),
        ToolSelector::Only(vec!["fs_read".to_string()]),
    )]));

    let mut stream = runtime.subscribe();
    let agent_id = runtime
        .start_root(root_spec("narrowed", "hello"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, agent_id).await;

    let calls = calls_handle.calls();
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one turn sent to the backend"
    );
    let announced: Vec<String> = calls[0]
        .tools
        .iter()
        .map(|spec| spec.name.as_str().to_string())
        .collect();
    assert_eq!(
        announced,
        vec!["fs_read".to_string()],
        "a role narrowed by tools.exclude must announce ONLY its survivors, got {announced:?}"
    );
}

/// The paired negative: a role with no entry in `Runtime::role_tools` at
/// all (exactly what every role with no `[roles.<alias>.tools]` table
/// resolves to -- `RoleToolsConfig::is_unset` contributes nothing) still
/// announces every registered tool, unchanged. Catches a narrowing that
/// fires UNCONDITIONALLY (e.g. treating a missing role entry as "select
/// nothing" rather than "leave the base selector alone").
#[tokio::test]
async fn role_with_no_tools_entry_still_announces_everything() {
    let backend = Arc::new(ScriptedBackend::new(vec![ScriptedTurn::Respond(
        text_response("hi"),
    )]));
    let calls_handle = backend.clone();
    let (runtime, _store) = build_runtime(backend, vec![three_tool_plugin()]);

    // Deliberately: `set_role_tools` is called with a table that names a
    // DIFFERENT role, never "open" -- proving the absence of an entry, not
    // merely the absence of any call to `set_role_tools` at all (a real
    // build calls it unconditionally, once, for every configured role).
    runtime.set_role_tools(HashMap::from([(
        RoleAlias::new("narrowed"),
        ToolSelector::Only(vec!["fs_read".to_string()]),
    )]));

    let mut stream = runtime.subscribe();
    let agent_id = runtime
        .start_root(root_spec("open", "hello"))
        .await
        .unwrap();
    wait_for_agent_finished(&mut stream, agent_id).await;

    let calls = calls_handle.calls();
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one turn sent to the backend"
    );
    let mut announced: Vec<String> = calls[0]
        .tools
        .iter()
        .map(|spec| spec.name.as_str().to_string())
        .collect();
    announced.sort();
    assert_eq!(
        announced,
        vec![
            "fs_read".to_string(),
            "mcp_search".to_string(),
            "mcp_write".to_string(),
        ],
        "a role absent from role_tools must announce every registered tool unchanged, got {announced:?}"
    );
}
