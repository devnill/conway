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

use std::path::PathBuf;

use serde::de::DeserializeOwned;

use conway_core::content::{ContentBlock, ToolCall, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ports::{ToolCtx, ToolOutput};

/// Resolves a tool-supplied path argument against `ctx.cwd`.
///
/// `path` beginning with exactly `~` or a leading `~/` expands against the
/// process's home directory; any other absolute input is returned
/// unchanged; a relative input is joined onto `ctx.cwd`. A path containing
/// a NUL byte, or one beginning with `~` in a form this crate does not
/// expand (e.g. `~user/...`), is rejected as a host-level
/// `InvalidArguments` error naming the specific reason -- see
/// [`conway_core::containment::ResolveError`]'s own doc.
///
/// Performs **no** containment or escape checks (no sandboxing in
/// this layer) and does **not** canonicalize (canonicalizing would fail for
/// paths that don't exist yet, e.g. a `write` target).
///
/// **A thin wrapper around the one shared implementation,
/// [`conway_core::containment::resolve_candidate`]
///.** This function used to carry its own
/// restated copy of "absolute -> as-is, relative -> join cwd, NUL -> reject"
/// -- kept in sync with `conway_runtime::permission::
/// resolve_like_the_tool_will`'s identical copy only by a doc comment
/// demanding lockstep edits, never enforced by the compiler. It is now a
/// direct call into the shared core function, only translating its typed
/// `Err` into this crate's own `ToolError` (never re-deriving the message),
/// so the two crates' wrappers can no longer independently drift (two
/// inlined copies of this exact rule already dropped the NUL guard once, in
/// `conway-runtime`
///) -- and tilde expansion, landed
/// in that same shared function, cannot drift between them either (board
/// item `01M10HSENWKTEE4G691XJXBH6T`).
pub fn resolve_path(ctx: &ToolCtx, path: &str) -> Result<PathBuf, ToolError> {
    conway_core::containment::resolve_candidate(&ctx.cwd, path).map_err(|err| {
        ToolError::InvalidArguments {
            detail: err.to_string(),
        }
    })
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

    /// A leading `~/` expands against the home directory rather than being
    /// joined under `ctx.cwd` the way an ordinary relative path is -- this
    /// is what makes the assertion below discriminating: if `~` were still
    /// passed through untouched (this item's own defect), the result would
    /// equal `ctx.cwd.join("~/target.txt")` and would carry a literal `~`
    /// path component, which this test explicitly rules out without
    /// depending on what the real home directory happens to be (so it needs
    /// no `HOME`/`USERPROFILE` override, and cannot race any other test
    /// mutating those).
    #[test]
    fn resolve_path_expands_a_leading_tilde_slash_rather_than_joining_it_under_cwd() {
        let (ctx, _handles) = test_ctx(PathBuf::from("/tmp/x"));
        let resolved = resolve_path(&ctx, "~/target.txt").unwrap();
        assert!(resolved.is_absolute());
        assert!(
            !resolved.components().any(|c| c.as_os_str() == "~"),
            "a leading `~/` must be expanded to the home directory, never carried through as a \
             literal path component: {resolved:?}"
        );
        assert_ne!(
            resolved,
            PathBuf::from("/tmp/x/~/target.txt"),
            "must not be joined under cwd the way an ordinary relative path would be: {resolved:?}"
        );
    }

    /// P-15: the discriminating observable for "anchored, never a substring
    /// replace" -- a `~` that is NOT the first character of the whole
    /// argument (here, mid-path) must resolve to the exact literal path a
    /// naive `raw.replace('~', home)` would NOT produce (that would splice
    /// the home directory in the middle of `sub/`). Exact-path equality,
    /// not merely "no panic" or "still under cwd", so a substring-replace
    /// regression fails this test even though it, too, "resolves to
    /// something under `/tmp/x`".
    #[test]
    fn resolve_path_treats_a_non_leading_tilde_as_a_literal_filename_character() {
        let (ctx, _handles) = test_ctx(PathBuf::from("/tmp/x"));
        let resolved = resolve_path(&ctx, "sub/~name/file.txt").unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/x/sub/~name/file.txt"));
    }

    /// The ruling's other named case: `~user/...` is a tilde FORM this
    /// crate does not expand, and must be a named refusal (INTENT.md §8.3),
    /// not a silent literal pass-through and not a generic "not found" once
    /// something downstream tries to use it.
    #[test]
    fn resolve_path_rejects_a_tilde_user_form_it_does_not_expand_naming_tilde() {
        let (ctx, _handles) = test_ctx(PathBuf::from("/tmp/x"));
        let err = resolve_path(&ctx, "~bob/secret.txt").unwrap_err();
        match err {
            ToolError::InvalidArguments { detail } => {
                assert!(
                    detail.contains('~'),
                    "the denial must name tilde explicitly: {detail:?}"
                );
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
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
