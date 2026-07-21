//! `ReportPlugin`: the `report` tool — explicit `AgentResult` finalization.

use std::sync::Arc;

use conway_core::ports::{Plugin, PluginManifest, Tool};

pub mod report_tool;

pub use report_tool::ReportTool;

/// The `report` plugin: `report`.
pub struct ReportPlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl ReportPlugin {
    pub fn new() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(ReportTool::new())];
        Self { tools }
    }
}

impl Default for ReportPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ReportPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "conway.report".into(),
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
        let plugin = ReportPlugin::new();
        let manifest = plugin.manifest();
        assert_eq!(manifest.id, "conway.report");
        assert!(manifest.required_host_caps.is_empty());

        let names: Vec<String> = plugin
            .tools()
            .iter()
            .map(|t| t.spec().name.as_str().to_string())
            .collect();
        assert_eq!(names, vec!["report"]);
    }
}
