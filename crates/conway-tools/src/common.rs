//! Shared helpers used by every tool in this crate: argument parsing, path
//! resolution, output construction, and cooperative cancellation.
//!
//! Error discipline (applied crate-wide, architecture "Module:
//! conway-tools"): **model-recoverable** conditions (file not found, no
//! regex match, non-zero exit code, ambiguous edit) return
//! `Ok(ToolOutput { is_error: true, .. })` so the model can adapt.
//! **Host/infrastructure** conditions (cancellation, permission denied by
//! the OS, spawn failure, an unreachable `SubagentHost`) return
//! `Err(ToolError::..)`. Every tool in this crate follows this rule.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use conway_core::content::{ContentBlock, ToolCall, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ports::{ToolCtx, ToolOutput};

/// Resolves a tool-supplied path argument against `ctx.cwd`.
///
/// Relative inputs are joined onto `ctx.cwd`; absolute inputs are returned
/// unchanged. A path containing a NUL byte is rejected as a host-level
/// `InvalidArguments` error (the OS path APIs cannot represent it).
///
/// Performs **no** containment or escape checks (GP-08: no sandboxing in
/// this layer) and does **not** canonicalize (canonicalizing would fail for
/// paths that don't exist yet, e.g. a `write` target).
pub fn resolve_path(ctx: &ToolCtx, path: &str) -> Result<PathBuf, ToolError> {
    if path.contains('\0') {
        return Err(ToolError::InvalidArguments {
            detail: format!("path contains a NUL byte: {path:?}"),
        });
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        Ok(candidate.to_path_buf())
    } else {
        Ok(ctx.cwd.join(candidate))
    }
}

/// Deserializes a tool call's `arguments` into `T`. A shape mismatch is a
/// host-level error (the runtime already validated the call against
/// `spec().schema`; a mismatch here means the schema and the args type have
/// drifted) surfaced as `ToolError::InvalidArguments` carrying the
/// underlying serde error text.
pub fn parse_args<T: DeserializeOwned>(call: &ToolCall) -> Result<T, ToolError> {
    serde_json::from_value(call.arguments.clone()).map_err(|e| ToolError::InvalidArguments {
        detail: e.to_string(),
    })
}

/// Builds a successful `ToolOutput` from a single text block.
pub fn text_output(text: String, truncation: TruncationPolicy) -> ToolOutput {
    ToolOutput {
        blocks: vec![ContentBlock::Text { text }],
        is_error: false,
        truncation,
        artifacts: Vec::new(),
    }
}

/// Builds a model-recoverable error `ToolOutput`: a single text block,
/// `is_error: true`, no truncation, no artifacts.
pub fn error_text(text: String) -> ToolOutput {
    ToolOutput {
        blocks: vec![ContentBlock::Text { text }],
        is_error: true,
        truncation: TruncationPolicy::None,
        artifacts: Vec::new(),
    }
}

/// Cooperative cancellation check: every tool calls this (at minimum) at the
/// start of `invoke`. Returns `Err(ToolError::Cancelled)` once `ctx.cancel`
/// (or any ancestor token) has been cancelled.
pub fn check_cancel(ctx: &ToolCtx) -> Result<(), ToolError> {
    if ctx.cancel.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;

    fn call_with_args(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "tc_1".into(),
            name: conway_core::ids::ToolName::new("test"),
            arguments,
        }
    }

    #[test]
    fn resolve_path_joins_relative_onto_cwd() {
        let (ctx, _handles) = test_ctx(PathBuf::from("/tmp/x"));
        let resolved = resolve_path(&ctx, "a/b").unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/x/a/b"));
    }

    #[test]
    fn resolve_path_passes_absolute_through_unchanged() {
        let (ctx, _handles) = test_ctx(PathBuf::from("/tmp/x"));
        let resolved = resolve_path(&ctx, "/etc/hosts").unwrap();
        assert_eq!(resolved, PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn resolve_path_rejects_nul_byte() {
        let (ctx, _handles) = test_ctx(PathBuf::from("/tmp/x"));
        let err = resolve_path(&ctx, "a\0b").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[derive(Debug, serde::Deserialize)]
    struct Args {
        #[allow(dead_code)]
        path: String,
    }

    #[test]
    fn parse_args_surfaces_serde_error_text() {
        // `path` is required and must be a string; supply neither.
        let call = call_with_args(serde_json::json!({}));
        let err = parse_args::<Args>(&call).unwrap_err();
        match err {
            ToolError::InvalidArguments { detail } => {
                assert!(detail.contains("path"), "detail was {detail:?}");
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_succeeds_on_matching_shape() {
        let call = call_with_args(serde_json::json!({"path": "a.txt"}));
        let args: Args = parse_args(&call).unwrap();
        assert_eq!(args.path, "a.txt");
    }

    #[test]
    fn text_output_is_not_an_error() {
        let out = text_output("ok".into(), TruncationPolicy::None);
        assert!(!out.is_error);
        assert_eq!(out.blocks, vec![ContentBlock::Text { text: "ok".into() }]);
        assert_eq!(out.truncation, TruncationPolicy::None);
        assert!(out.artifacts.is_empty());
    }

    #[test]
    fn error_text_sets_is_error_and_none_truncation() {
        let out = error_text("boom".into());
        assert!(out.is_error);
        assert_eq!(out.blocks.len(), 1);
        assert!(matches!(out.blocks[0], ContentBlock::Text { .. }));
        assert_eq!(out.truncation, TruncationPolicy::None);
        assert!(out.artifacts.is_empty());
    }

    #[test]
    fn check_cancel_ok_when_not_cancelled() {
        let (ctx, _handles) = test_ctx(PathBuf::from("/tmp/x"));
        assert!(check_cancel(&ctx).is_ok());
    }

    #[test]
    fn check_cancel_errs_when_cancelled() {
        let (ctx, handles) = test_ctx(PathBuf::from("/tmp/x"));
        handles.cancel.cancel();
        assert!(matches!(check_cancel(&ctx), Err(ToolError::Cancelled)));
    }
}
