//! `SubagentPlugin`: `conway_subagent`, `conway_steer`, `conway_await`,
//! `conway_cancel` — a pure wrapper over `ToolCtx::subagents`.

use std::sync::Arc;

use conway_core::ports::{Plugin, PluginManifest, Tool};

pub mod tools;

pub use tools::{AwaitTool, CancelTool, SteerTool, SubagentTool};

/// The `subagent` plugin: `conway_subagent`, `conway_steer`, `conway_await`,
/// `conway_cancel`.
pub struct SubagentPlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl SubagentPlugin {
    pub fn new() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(SubagentTool::new()),
            Arc::new(SteerTool::new()),
            Arc::new(AwaitTool::new()),
            Arc::new(CancelTool::new()),
        ];
        Self { tools }
    }
}

impl Default for SubagentPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for SubagentPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "conway.subagent".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            tools: self.tools.iter().map(|t| t.spec().name).collect(),
            required_host_caps: Vec::new(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_id_and_tool_names() {
        let plugin = SubagentPlugin::new();
        let manifest = plugin.manifest();
        assert_eq!(manifest.id, "conway.subagent");
        assert!(manifest.required_host_caps.is_empty());

        let mut names: Vec<String> = plugin
            .tools()
            .iter()
            .map(|t| t.spec().name.as_str().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "conway_await",
                "conway_cancel",
                "conway_steer",
                "conway_subagent"
            ]
        );
    }
}
