//! `ShellPlugin`: the `bash` tool — streamed, cancellable,
//! process-group-killing command execution.

use std::sync::Arc;

use conway_core::ports::{Plugin, PluginManifest, Tool};

pub mod bash;

pub use bash::BashTool;

/// The `shell` plugin: `bash`.
pub struct ShellPlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl ShellPlugin {
    pub fn new() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(BashTool::new())];
        Self { tools }
    }
}

impl Default for ShellPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ShellPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "conway.shell".into(),
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
        let plugin = ShellPlugin::new();
        let manifest = plugin.manifest();
        assert_eq!(manifest.id, "conway.shell");
        assert!(manifest.required_host_caps.is_empty());

        let names: Vec<String> = plugin
            .tools()
            .iter()
            .map(|t| t.spec().name.as_str().to_string())
            .collect();
        assert_eq!(names, vec!["bash"]);
    }
}
