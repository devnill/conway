//! `ReadTool`: the `read` tool — `cat -n`-style file reading with binary
//! sniffing and offset/limit windowing.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, error_text, parse_args, resolve_path, text_output};

/// The first N bytes inspected for a NUL byte to decide whether a file is
/// binary (matches the `TruncationPolicy::Head { max_bytes }` window below).
const SNIFF_BYTES: usize = 8192;

/// Lines returned when the caller doesn't supply `limit`.
const DEFAULT_LIMIT: u32 = 2000;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    /// File path, absolute or relative to cwd
    path: String,
    /// 1-based first line to read
    #[schemars(range(min = 1))]
    offset: Option<u32>,
    /// Max lines to read; default 2000
    #[schemars(range(min = 1))]
    limit: Option<u32>,
}

/// Reads a file, `cat -n` style: each line prefixed with its 1-based
/// absolute line number, right-aligned to width 6, then a TAB.
#[derive(Debug, Default)]
pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ReadTool {
    /// `ReadArgs::path` is the only path argument (`offset`/`limit` are
    /// numeric). Confinable: a root check can evaluate it statically.
    fn path_args(&self) -> PathArgs {
        PathArgs::Named(&["path"])
    }

    /// `read` never overrides `render`, so its rendering is always the
    /// trait's own default JSON dump -- never a shell command.
    ///.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("read"),
            description: "Read a file's contents, cat -n style".into(),
            schema: schemars::schema_for!(ReadArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: ReadArgs = parse_args(&call)?;
        let path = resolve_path(&ctx, &args.path)?;

        // `[S1.5]`/(retirement): open-relative, so the
        // containment check and the actual read are one step -- see
        // `crate::fs::beneath`'s own doc. Unconfined (no root configured)
        // is byte-for-byte the pre-existing `tokio::fs::read`.
        let bytes = match crate::fs::beneath::read_file(&ctx, &path).await? {
            crate::fs::beneath::ReadOutcome::Bytes(bytes) => bytes,
            // Model-recoverable: the model chose a path that isn't there.
            crate::fs::beneath::ReadOutcome::NotFound => {
                return Ok(error_text(format!("file not found: {}", path.display())));
            }
        };

        check_cancel(&ctx)?;

        let sniff_len = bytes.len().min(SNIFF_BYTES);
        if bytes[..sniff_len].contains(&0u8) {
            return Ok(error_text("binary file; not read".into()));
        }

        Ok(text_output(
            render(&bytes, args.offset, args.limit),
            TruncationPolicy::Head { max_bytes: 65_536 },
        ))
    }
}

/// Renders the `cat -n`-style windowed body for `invoke`, split out for unit
/// testing without a `ToolCtx`.
fn render(bytes: &[u8], offset: Option<u32>, limit: Option<u32>) -> String {
    let content = String::from_utf8_lossy(bytes);
    if content.is_empty() {
        return "(empty file)".into();
    }

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start_idx = offset.unwrap_or(1).saturating_sub(1) as usize;
    let limit = limit.unwrap_or(DEFAULT_LIMIT) as usize;

    if start_idx >= total {
        return "(empty file)".into();
    }

    let end_idx = start_idx.saturating_add(limit).min(total);
    let mut out = String::new();
    for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let n = start_idx + i + 1;
        out.push_str(&format!("{n:>6}\t{line}"));
    }

    let remaining = total - end_idx;
    if remaining > 0 {
        out.push_str(&format!("\n… ({remaining} more lines; use offset/limit)"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;
    use conway_core::content::ContentBlock;
    use std::path::PathBuf;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "tc_1".into(),
            name: ToolName::new("read"),
            arguments,
        }
    }

    #[test]
    fn spec_has_expected_name_category_permission() {
        let spec = ReadTool::new().spec();
        assert_eq!(spec.name.as_str(), "read");
        assert_eq!(spec.category, ToolCategory::Read);
        assert_eq!(spec.permission, PermissionClass::Safe);
    }

    #[test]
    fn schema_required_and_properties() {
        let spec = ReadTool::new().spec();
        let json = serde_json::to_value(&spec.schema).unwrap();
        let required: Vec<&str> = json["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["path"]);
        let props = json["properties"].as_object().unwrap();
        assert!(props.contains_key("path"));
        assert!(props.contains_key("offset"));
        assert!(props.contains_key("limit"));
        assert_eq!(json["additionalProperties"], false);
    }

    #[test]
    fn render_five_lines_no_window() {
        let content = "a\nb\nc\nd\ne\n";
        let out = render(content.as_bytes(), None, None);
        assert_eq!(out, "     1\ta\n     2\tb\n     3\tc\n     4\td\n     5\te");
    }

    #[test]
    fn render_offset_and_limit() {
        let content = "a\nb\nc\nd\ne\n";
        let out = render(content.as_bytes(), Some(3), Some(2));
        assert_eq!(
            out,
            "     3\tc\n     4\td\n… (1 more lines; use offset/limit)"
        );
    }

    #[test]
    fn render_empty_is_empty_file() {
        assert_eq!(render(b"", None, None), "(empty file)");
    }

    #[tokio::test]
    async fn invoke_nonexistent_path_is_recoverable_error() {
        let (ctx, _h) = test_ctx(PathBuf::from("/nonexistent-conway-dir-xyz"));
        let out = ReadTool::new()
            .invoke(call(serde_json::json!({"path": "missing.txt"})), ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        let ContentBlock::Text { text } = &out.blocks[0] else {
            panic!("expected text block");
        };
        assert!(text.contains("missing.txt"));
    }

    #[tokio::test]
    async fn invoke_pre_cancelled_returns_cancelled_without_touching_fs() {
        let (ctx, handles) = test_ctx(PathBuf::from("/tmp"));
        handles.cancel.cancel();
        let err = ReadTool::new()
            .invoke(call(serde_json::json!({"path": "whatever.txt"})), ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
    }

    /// A NUL byte in `path` is
    /// rejected reaching THIS tool's production `invoke` entry point, not
    /// merely by `resolve_path` in isolation -- proving `read` actually
    /// calls the shared resolver rather than restating it.
    #[tokio::test]
    async fn invoke_rejects_nul_byte_in_path() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
        let err = ReadTool::new()
            .invoke(call(serde_json::json!({"path": "a\0b"})), ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    /// An unsupported `~user`-style tilde form is refused reaching THIS
    /// tool's production `invoke` entry point, not merely by `resolve_path`
    /// in isolation -- same rationale as `invoke_rejects_nul_byte_in_path`
    /// immediately above, applied to board item
    /// `01M10HSENWKTEE4G691XJXBH6T`'s own guard. The denial text must name
    /// tilde explicitly (the acceptance criterion this test anchors): the
    /// pre-fix behavior for ANY `~`-prefixed path was a generic "could not
    /// be found" that named nothing about `~` at all.
    #[tokio::test]
    async fn invoke_rejects_unsupported_tilde_form_naming_tilde() {
        let (ctx, _h) = test_ctx(PathBuf::from("/tmp"));
        let err = ReadTool::new()
            .invoke(
                call(serde_json::json!({"path": "~bob/secret.txt"})),
                ctx,
            )
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidArguments { detail } => {
                assert!(
                    detail.contains('~'),
                    "the denial reaching the model must name tilde explicitly: {detail:?}"
                );
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }
}
