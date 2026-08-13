//! `CdTool`: the `cd` tool -- changes the agent's working directory for
//! subsequent tool calls, via [`conway_core::ports::CwdHandle`] (S1's
//! capability; this slice is its first caller).

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy};
use conway_core::error::{CwdError, ToolError};
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, error_text, parse_args, resolve_path, text_output};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CdArgs {
    /// Directory path, absolute or relative to cwd
    path: String,
}

/// Changes the working directory subsequent tool calls resolve relative
/// paths against. See [`Self::spec`]'s description for the semantics the
/// model needs (next-batch effect, the one-off `cwd` alternative, and the
/// session-start-cwd invariant); this crate's own doc restates them for
/// the human reader.
#[derive(Debug, Default)]
pub struct CdTool;

impl CdTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CdTool {
    /// `CdArgs::path` is the only argument, and it is a path.
    fn path_args(&self) -> PathArgs {
        PathArgs::Named(&["path"])
    }

    /// `cd` never overrides `render`, so its rendering is always the
    /// trait's own default JSON dump -- never a shell command. Board item
    /// 01KYT3NSWRHMPEAXVXRJ73KDYR.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("cd"),
            description: "Change the working directory for subsequent tool calls. \
                A `cd` takes effect starting the NEXT batch of tool calls, not the \
                current one -- a `cd` issued alongside a `read` in the same batch \
                does not affect that `read`. For a one-off move (run this single \
                command somewhere else, then return), use the per-call `cwd` \
                argument on `bash`/`glob`/`grep` instead -- that applies \
                immediately, like a `(cd X && cmd)` subshell. Use `cd` only for a \
                persistent move. `cd` never changes where the session started; a \
                resumed session returns to its original spawn directory."
                .into(),
            schema: schemars::schema_for!(CdArgs),
            category: ToolCategory::Move,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: CdArgs = parse_args(&call)?;
        let path = resolve_path(&ctx, &args.path)?;

        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata,
            // Model-recoverable: the model chose a path that isn't there.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(error_text(format!(
                    "directory not found: {err} (path: {})",
                    path.display()
                )));
            }
            // Host-level (permission denied, I/O failure, ...): matches
            // read.rs/edit.rs's discipline for the same distinction.
            Err(err) => {
                return Err(ToolError::Io {
                    detail: format!("failed to stat {}: {err}", path.display()),
                });
            }
        };

        if !metadata.is_dir() {
            return Ok(error_text(format!("not a directory: {}", path.display())));
        }

        check_cancel(&ctx)?;

        ctx.chdir.set(path.clone()).map_err(|err| match err {
            CwdError::Poisoned => ToolError::Internal {
                detail: format!("cwd handle poisoned: {err}"),
            },
            // `CwdError` is `#[non_exhaustive]`: a future variant must map
            // to a typed `ToolError` deliberately, not fall through to a
            // panic on untrusted input.
            other => ToolError::Internal {
                detail: format!("cwd handle set failed: {other}"),
            },
        })?;

        Ok(text_output(
            format!("cwd is now {} (takes effect next batch)", path.display()),
            TruncationPolicy::None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;
    use conway_core::content::ContentBlock;
    use tempfile::TempDir;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "tc_1".into(),
            name: ToolName::new("cd"),
            arguments,
        }
    }

    fn text_of(out: &ToolOutput) -> &str {
        let ContentBlock::Text { text } = &out.blocks[0] else {
            panic!("expected text block");
        };
        text
    }

    #[test]
    fn spec_has_expected_name_category_permission() {
        let spec = CdTool::new().spec();
        assert_eq!(spec.name.as_str(), "cd");
        assert_eq!(spec.category, ToolCategory::Move);
        assert_eq!(spec.permission, PermissionClass::Safe);
    }

    #[test]
    fn schema_required_and_properties() {
        let spec = CdTool::new().spec();
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
        assert_eq!(json["additionalProperties"], false);
    }

    #[tokio::test]
    async fn invoke_valid_subdir_sets_the_cwd_handle_and_names_it_in_output() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());

        let out = CdTool::new()
            .invoke(call(serde_json::json!({"path": "sub"})), ctx.clone())
            .await
            .unwrap();

        assert!(!out.is_error);
        let expected = dir.path().join("sub");
        assert!(text_of(&out).contains(&expected.display().to_string()));
        assert_eq!(ctx.chdir.current(), expected);
    }

    #[tokio::test]
    async fn invoke_absolute_path_works() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("abs-sub");
        std::fs::create_dir(&sub).unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());

        let out = CdTool::new()
            .invoke(
                call(serde_json::json!({"path": sub.display().to_string()})),
                ctx.clone(),
            )
            .await
            .unwrap();

        assert!(!out.is_error);
        assert_eq!(ctx.chdir.current(), sub);
    }

    #[tokio::test]
    async fn invoke_nonexistent_path_is_recoverable_error_and_cwd_unchanged() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());

        let out = CdTool::new()
            .invoke(call(serde_json::json!({"path": "missing"})), ctx.clone())
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(text_of(&out).contains("missing"));
        assert_eq!(ctx.chdir.current(), dir.path());
    }

    #[tokio::test]
    async fn invoke_file_target_is_recoverable_error_and_cwd_unchanged() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hi").unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());

        let out = CdTool::new()
            .invoke(call(serde_json::json!({"path": "f.txt"})), ctx.clone())
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(text_of(&out).contains("not a directory"));
        assert_eq!(ctx.chdir.current(), dir.path());
    }

    #[tokio::test]
    async fn invoke_pre_cancelled_returns_cancelled_without_touching_chdir() {
        let dir = TempDir::new().unwrap();
        let (ctx, handles) = test_ctx(dir.path().to_path_buf());
        handles.cancel.cancel();

        let err = CdTool::new()
            .invoke(call(serde_json::json!({"path": "."})), ctx.clone())
            .await
            .unwrap_err();

        assert!(matches!(err, ToolError::Cancelled));
        assert_eq!(ctx.chdir.current(), dir.path());
    }

    /// Pins S1's next-batch contract from the tool side: `ToolCtx::cwd` is a
    /// snapshot taken once per batch, so a `cd`'s effect on `ctx.chdir`
    /// (which subsequent batches read) must NOT retroactively change the
    /// `cwd` a `ToolCtx` already built for this same batch resolves
    /// against -- exactly what a `read` invoked with the *same* `ToolCtx`
    /// (same-batch dispatch) would observe.
    #[tokio::test]
    async fn cd_in_same_batch_does_not_affect_a_read_sharing_that_batchs_ctx() {
        use crate::fs::ReadTool;

        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("f.txt"), "in sub").unwrap();
        std::fs::write(dir.path().join("f.txt"), "in root").unwrap();

        let (ctx, _h) = test_ctx(dir.path().to_path_buf());

        // Both calls share the same `ToolCtx` -- exactly what "the same
        // batch" means: `ToolRunner::run_batch` builds one `ToolCtx` (with
        // one frozen `cwd` snapshot) and hands clones of it to every task
        // dispatched in that batch.
        let cd_out = CdTool::new()
            .invoke(call(serde_json::json!({"path": "sub"})), ctx.clone())
            .await
            .unwrap();
        assert!(!cd_out.is_error);

        let read_out = ReadTool::new()
            .invoke(
                ToolCall {
                    call_id: "tc_2".into(),
                    name: ToolName::new("read"),
                    arguments: serde_json::json!({"path": "f.txt"}),
                },
                ctx.clone(),
            )
            .await
            .unwrap();

        // The `read` used this same `ctx.cwd` (the batch's frozen
        // snapshot, still the temp dir root) to resolve `f.txt`, not the
        // `chdir` cell the `cd` call mutated -- it reads the root file, not
        // the one in `sub/`.
        assert!(!read_out.is_error, "{}", text_of(&read_out));
        assert!(text_of(&read_out).contains("in root"));

        // The mutation is still visible via the handle itself (what the
        // NEXT batch's snapshot would pick up).
        assert_eq!(ctx.chdir.current(), dir.path().join("sub"));
    }

    /// Board item 01KZVZ56SBPSTZHAXXGYCNETNX: driven through this tool's
    /// production `invoke` entry point, not `resolve_path` in isolation.
    #[tokio::test]
    async fn invoke_rejects_nul_byte_in_path() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let err = CdTool::new()
            .invoke(call(serde_json::json!({"path": "a\0b"})), ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }
}
