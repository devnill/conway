//! Reads THIS agent's `conway.fs` root -- the identical root `conway.fs`
//! itself enforces `read`/`write`/`edit`/`cd`/`glob`/`grep` against, so the
//! two tools can never disagree about the boundary (this crate's own module
//! doc, "Design question: the same root as `conway.fs`").
//!
//! **Why this crate reads `conway.fs`'s own config key rather than
//! declaring a `NarrowingRule` of its own.** `Plugin::narrowable_keys`'s own
//! doc: the host prefixes each declared key with the DECLARING plugin's own
//! manifest id, so `"root"` declared here would land at
//! `"conway.confine.root"` -- a second, independently-set boundary an
//! operator (or a fork/spawn caller) could set differently from `conway.fs`'s
//! own, exactly the split-boundary hazard this design question exists to
//! rule out. Reading an already-set key declared narrowable by ANOTHER
//! plugin needs no ownership of its own: `conway_core::ports::
//! Plugin::narrowable_keys` governs who may WRITE a per-agent override for
//! a key, never who may READ `ToolCtx::config` -- every tool's `invoke`
//! already reads whatever config values are present, regardless of which
//! plugin declared them narrowable (`conway_tools::fs::beneath::resolve`
//! is the read this module mirrors).
//!
//! **The exact string `"conway.fs.root"` is duplicated here, not imported.**
//! `conway_tools::fs`'s own `FULL_ROOT_CONFIG_KEY` is `pub(crate)`, and this
//! crate cannot depend on `conway-tools` directly (the plugin tier depends
//! on `conway::plugin` only). `conway_runtime::permission::
//! CONWAY_FS_ROOT_CONFIG_KEY` already carries the identical duplication, for
//! the identical reason -- `conway_tools::fs`'s own module doc calls this
//! out explicitly as "a `const`, not restated logic," bounded because
//! nothing about the STRING can silently drift (a typo here fails every
//! call with a `Denied` naming the wrong key, loudly, not a silent
//! unconfined pass-through).

use conway::plugin::ToolCtx;
use conway::plugin::ToolError;

/// Duplicated from `conway_tools::fs::FULL_ROOT_CONFIG_KEY` -- see this
/// module's own doc for why.
pub(crate) const CONWAY_FS_ROOT_CONFIG_KEY: &str = "conway.fs.root";

/// Resolves this call's confinement root, or a `Denied` error naming
/// `--root` -- WHAT TO BUILD point 2: "No root → each call returns an error
/// naming `--root`." Never falls back to running unconfined (unlike
/// `conway.fs`'s own opt-in-only `Access::Unconfined` default): the entire
/// point of `conway.confine` is that a call it accepts is confined, so an
/// absent root is refused outright rather than silently treated as "nothing
/// to confine."
pub(crate) fn resolve_root(ctx: &ToolCtx) -> Result<String, ToolError> {
    let Some(value) = ctx.config.values.get(CONWAY_FS_ROOT_CONFIG_KEY) else {
        return Err(ToolError::Denied {
            reason: "conway.confine has no confinement root for this agent -- pass --root at \
                     startup (ConwayBuilder::with_root), which is what populates the \
                     conway.fs.root this tool also reads, before calling confined_bash"
                .into(),
        });
    };
    let Some(root_str) = value.as_str() else {
        return Err(ToolError::Denied {
            reason: format!(
                "conway.fs.root is configured but is not a string ({value}); conway.confine \
                 refuses to run any command without a real, nameable root -- pass --root"
            ),
        });
    };
    Ok(root_str.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use conway::plugin::PluginConfig;
    use conway::AgentId;
    use conway_testkit::{CollectingEventSink, FakeSubagentHost};

    use super::*;

    fn ctx_with_config(values: serde_json::Map<String, serde_json::Value>) -> ToolCtx {
        let agent_id = AgentId::new();
        let base = ToolCtx::for_test(
            agent_id,
            std::env::temp_dir(),
            Arc::new(FakeSubagentHost::new(agent_id)),
            Arc::new(CollectingEventSink::new()),
        );
        ToolCtx {
            config: Arc::new(PluginConfig { values }),
            ..base
        }
    }

    #[test]
    fn resolve_root_errors_naming_root_flag_when_absent() {
        let ctx = ctx_with_config(serde_json::Map::new());
        let err = resolve_root(&ctx).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("--root"), "{text:?}");
    }

    #[test]
    fn resolve_root_errors_when_the_configured_value_is_not_a_string() {
        let mut values = serde_json::Map::new();
        values.insert(CONWAY_FS_ROOT_CONFIG_KEY.to_string(), serde_json::json!(5));
        let ctx = ctx_with_config(values);
        let err = resolve_root(&ctx).unwrap_err();
        assert!(matches!(err, ToolError::Denied { .. }));
    }

    #[test]
    fn resolve_root_reads_the_conway_fs_root_key_exactly() {
        let mut values = serde_json::Map::new();
        values.insert(
            CONWAY_FS_ROOT_CONFIG_KEY.to_string(),
            serde_json::json!("/tmp/some-root"),
        );
        let ctx = ctx_with_config(values);
        assert_eq!(resolve_root(&ctx).unwrap(), "/tmp/some-root");
    }
}
