//! `SubagentPlugin`: `conway_subagent`, `conway_ask`, `conway_steer`,
//! `conway_await`, `conway_cancel` — pure wrappers over `ToolCtx::subagents`.

use std::sync::Arc;

use conway_core::ports::{Plugin, PluginManifest, Tool};

pub mod ask;
pub mod control;
pub mod tools;

pub use ask::AskTool;
pub use control::{AwaitTool, CancelTool, SteerTool};
pub use tools::SubagentTool;

/// The `subagent` plugin: `conway_subagent`, `conway_ask`, `conway_steer`,
/// `conway_await`, `conway_cancel`.
pub struct SubagentPlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl SubagentPlugin {
    pub fn new() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(SubagentTool::new()),
            Arc::new(AskTool::new()),
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
                "conway_ask",
                "conway_await",
                "conway_cancel",
                "conway_steer",
                "conway_subagent"
            ]
        );
    }

    /// SIG-1 regression: `deadline_from_secs` (shared by `conway_ask` and
    /// `conway_subagent`) range-checks the model-supplied deadline per P-10 --
    /// out-of-range maps to a typed `InvalidArguments`, never the
    /// `Duration::seconds` overflow panic the previous
    /// `i64::try_from(..).unwrap_or(i64::MAX)` saturation caused.
    #[test]
    fn deadline_from_secs_accepts_sane_and_rejects_overflow_as_invalid_arguments() {
        use conway_core::error::ToolError;
        use super::tools::{deadline_from_secs, MAX_DEADLINE_SECS};

        // A sane deadline is accepted and resolves to a future instant.
        let sane = deadline_from_secs(120).expect("120s deadline accepted");
        assert!(sane > chrono::Utc::now());

        // The boundary value is accepted (the check is `>`, not `>=`).
        let _ = deadline_from_secs(MAX_DEADLINE_SECS).expect("MAX_DEADLINE_SECS accepted");

        // One past the max -> a typed InvalidArguments error, never a panic.
        let err = deadline_from_secs(MAX_DEADLINE_SECS + 1).unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArguments { .. }),
            "expected InvalidArguments, got {err:?}"
        );

        // The extreme u64::MAX that previously panicked now errors cleanly.
        let err = deadline_from_secs(u64::MAX).unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArguments { .. }),
            "expected InvalidArguments for u64::MAX, got {err:?}"
        );
    }
}