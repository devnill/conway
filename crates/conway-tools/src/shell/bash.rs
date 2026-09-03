//! `BashTool`: the `bash` tool — streamed, cancellable, process-group-killing
//! command execution (architecture "Module: conway-tools").

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::process::Command;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

#[cfg(not(unix))]
use crate::common::error_text;
use crate::common::{check_cancel, parse_args};

/// Builds the argv this tool spawns and streams, given the shell command
/// text (verbatim, untouched -- never parsed) and the resolved working
/// directory. [`BashTool::new`]'s own launcher ([`default_launcher`]) is
/// `/bin/bash -c <command>`, `current_dir(cwd)`; the run loop
/// ([`unix::run`]) wires stdin/stdout/stderr and the process group onto
/// whatever [`Command`] this returns, uniformly, regardless of which
/// launcher built it.
///
/// **Why this seam exists.** `conway-plugin-confine`'s own bash-equivalent
/// tool needs the IDENTICAL streaming/cancellation/timeout run loop this
/// module already implements, wrapping the SAME `/bin/bash -c <command>`
/// invocation in an OS containment primitive (`sandbox-exec` on macOS,
/// `bwrap` on Linux) rather than running it bare. [`BashTool::with_launcher`]
/// is the seam that makes that ONE implementation, not a second copy of the
/// run loop -- see that constructor's own doc.
pub type Launcher = Arc<dyn Fn(&str, &Path) -> Command + Send + Sync>;

/// The default [`Launcher`]: plain `/bin/bash -c <command>` in `cwd`, no
/// containment of any kind -- byte-for-byte what this tool did before
/// [`Launcher`] existed.
fn default_launcher(command: &str, cwd: &Path) -> Command {
    let mut cmd = Command::new("/bin/bash");
    cmd.arg("-c").arg(command).current_dir(cwd);
    cmd
}

/// Applied when the caller omits `timeout_ms`.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Conway-core's `HeadTail` variant is `{ head_bytes, tail_bytes }`, not the
/// `{ max_bytes }` shape the module plan sketched (assumption 1: use
/// conway-core's names with the same semantics rather than inventing a
/// field). Split the plan's 30_000-byte budget evenly to preserve the same
/// total-retained-bytes semantics.
const TRUNCATION: TruncationPolicy = TruncationPolicy::HeadTail {
    head_bytes: 15_000,
    tail_bytes: 15_000,
};

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BashArgs {
    /// Shell command executed with bash -c
    command: String,
    /// Kill the command if it hasn't finished after this many milliseconds
    #[serde(default = "default_timeout_ms")]
    #[schemars(range(min = 1))]
    timeout_ms: u64,
    /// Working directory; default the agent cwd
    cwd: Option<String>,
}

/// Executes `command` with `bash -c`, streaming stdout/stderr line-by-line as
/// `Event::ToolProgress` and killing the whole process group — including any
/// backgrounded children — on cancellation or timeout.
///
/// No sandboxing, no command allow/deny list, no argument sanitization
/// (process-group setup here is execution plumbing, not a security
/// boundary; the `PermissionGate` is the control point) -- for [`Self::new`]
/// specifically. [`Self::with_launcher`] is the seam a caller substitutes a
/// containment-wrapping [`Launcher`] through instead; see that
/// constructor's own doc.
pub struct BashTool {
    launcher: Launcher,
}

impl std::fmt::Debug for BashTool {
    /// `launcher` is an `Arc<dyn Fn>`, which carries no useful `Debug`
    /// representation -- named but not printed, mirroring how this crate's
    /// other closure-carrying fields are handled elsewhere.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashTool")
            .field("launcher", &"<fn>")
            .finish()
    }
}

impl BashTool {
    /// Plain, unconfined `bash` -- [`default_launcher`]. Byte-for-byte the
    /// pre-[`Launcher`] behavior of this tool.
    pub fn new() -> Self {
        Self {
            launcher: Arc::new(default_launcher),
        }
    }

    /// Builds a bash-equivalent tool with the SAME streaming/cancellation/
    /// timeout run loop [`Self::new`] uses, launched through `launcher`
    /// instead of a plain `/bin/bash -c`.
    ///
    /// **Deliberately does not also let a caller override this tool's own
    /// name, description, `path_args`, `render_kind`, or
    /// `confined_by_tool` answer.** Those stay exactly what [`Self::new`]'s
    /// tool already declares (`"bash"`, `PathArgs::Unconfinable`,
    /// `RenderKind::ShellCommand`, `confined_by_tool() == false`) --
    /// correct answers for THIS type regardless of which launcher builds
    /// its argv, since none of them describe the launcher, they describe
    /// the tool's OWN call shape (a free-form shell command, checkable only
    /// via `cwd`). A caller that wants a differently-NAMED, differently-
    /// DESCRIBED tool whose `confined_by_tool()` answers `true` (the
    /// structural flag the root+unconfinable-shell-tool warning consults --
    /// see `Tool::confined_by_tool`'s own doc) constructs its own `Tool`
    /// wrapping a `BashTool::with_launcher` instance and delegates `invoke`
    /// to it, exactly as `conway-plugin-confine`'s own `ConfinedBashTool`
    /// does -- see that crate's module doc for why delegating `invoke`
    /// alone (not the whole `Tool` impl) is what keeps the run loop a
    /// single implementation while still letting the two tools answer every
    /// OTHER `Tool` method independently and honestly.
    pub fn with_launcher(launcher: Launcher) -> Self {
        Self { launcher }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for BashTool {
    /// Both facts about `bash` at once, which is why `Unconfinable` carries
    /// `checkable` rather than there being two variants:
    ///
    /// - `BashArgs::command` is **unconfinable**. It goes to `/bin/bash -c`
    ///   verbatim, and a shell command reaches any path it likes via
    ///   redirection, substitution, `cd`, or a subprocess. Extracting paths
    ///   from it and concluding "none outside the root, therefore allow"
    ///   would be a transformation of untrusted input whose *failure to
    ///   find* something becomes an authorization -- the same shape as the
    ///   metacharacter-gate bug fixed in 0.5.0. So: never auto-allowed under
    ///   a root; always falls through to the operator's gate.
    /// - `BashArgs::cwd` **is** checkable. It is resolved through
    ///   `resolve_path` and handed to `Command::current_dir`, so a root check
    ///   can evaluate it exactly like any other path argument.
    ///
    /// **Do not "improve" this by parsing `command` for paths.** `cd ..`,
    /// `$HOME/x`, `$(echo /etc)/passwd`, `exec 3</etc/passwd`, a shell
    /// function, and a heredoc all defeat any such scan -- there is no
    /// finite list of shapes to special-case, because the input language is
    /// a full shell. A root confines path *arguments*; it does not, and
    /// cannot, confine what a shell command does. An agent holding `bash`
    /// is not confined by root alone (see `docs/permissions.md`'s
    /// "Confinement" section for the full boundary, including the
    /// composition -- root plus a tool set excluding `bash` -- that IS a
    /// real guarantee; see `docs/tools.md` for the full built-in tool list,
    /// each one's category and permission class, and this same exception
    /// stated in the table itself).
    fn path_args(&self) -> PathArgs {
        PathArgs::Unconfinable {
            checkable: &["cwd"],
        }
    }

    /// `bash` overrides `render` (below) to return the bare `command`
    /// string -- exactly what gets handed to a shell. This is the ONE
    /// built-in tool `RenderKind::ShellCommand` is meaningful for: it is
    /// what `conway_core::permission_pattern::Rule::gate_allows` reads to
    /// refuse EVERY pattern allow for this tool outright, so it is the one
    /// built-in that MUST declare `ShellCommand` explicitly -- this is also
    /// [`RenderKind`]'s own default, restated here (mirroring `path_args`
    /// above, which restates `PathArgs`'s own default too) for the same
    /// reason: a reader should never have to go check what the default is
    /// to know what `bash` does.
    fn render_kind(&self) -> RenderKind {
        RenderKind::ShellCommand
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("bash"),
            description: "Execute a shell command with bash -c. If a confinement root is \
                active, the cwd argument is checked against it, but the command string is \
                not -- it runs verbatim, so a root does not confine what this command does."
                .into(),
            schema: schemars::schema_for!(BashArgs),
            category: ToolCategory::Execute,
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: BashArgs = parse_args(&call)?;

        #[cfg(unix)]
        return unix::run(&call.call_id, args, ctx, self.launcher.clone()).await;

        #[cfg(not(unix))]
        {
            let _ = args;
            return Ok(error_text("bash tool requires a unix host".into()));
        }
    }

    /// The bare shell command, not the generic `bash({"command":...})`
    /// default: `PatternRule` prefix-matches this text against a granted
    /// command prefix (`conway_core::permission_pattern`), which is only
    /// legible when `rendered` IS the command a person would type.
    ///
    /// `args` is untrusted, model-supplied JSON: a missing or
    /// non-string `command` falls back to the trait's default rendering
    /// rather than panicking (no `unwrap`/`expect`/indexing).
    fn render(&self, args: &serde_json::Value) -> String {
        match args.get("command").and_then(serde_json::Value::as_str) {
            Some(command) => command.to_string(),
            None => format!("{}({})", self.spec().name, args),
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Stdio};

    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::time::{Duration, Instant};

    use conway_core::content::ContentBlock;
    use conway_core::error::ToolError;
    use conway_core::event::Event;
    use conway_core::ports::{ToolCtx, ToolOutput};

    use crate::process::unix::kill_group;

    use super::{BashArgs, Launcher, TRUNCATION};

    /// How often the run loop wakes up (absent stdout/stderr/exit activity)
    /// to re-check cancellation and the deadline.
    const POLL_INTERVAL: Duration = Duration::from_millis(50);

    enum Outcome {
        Completed(ExitStatus),
        Cancelled,
        TimedOut,
    }

    pub(super) async fn run(
        call_id: &str,
        args: BashArgs,
        ctx: ToolCtx,
        launcher: Launcher,
    ) -> Result<ToolOutput, ToolError> {
        let cwd = match &args.cwd {
            Some(c) => crate::common::resolve_path(&ctx, c)?,
            None => ctx.cwd.clone(),
        };

        // Built by `launcher`, not a hardcoded `Command::new("/bin/bash")`
        // -- `BashTool::new`'s own default launcher reproduces that
        // literally; `BashTool::with_launcher` substitutes an alternate
        // argv (e.g. `conway-plugin-confine`'s `sandbox-exec`/`bwrap`
        // wrapping of the identical `/bin/bash -c` invocation). Every step
        // below -- stdio wiring, the process group, streaming, cancellation,
        // the timeout deadline -- runs identically regardless of which
        // launcher built this `Command`.
        let mut command = launcher(&args.command, &cwd);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        let mut child = command.spawn().map_err(|err| ToolError::Io {
            detail: format!("failed to spawn bash: {err}"),
        })?;

        // `process_group(0)` makes the child its own group leader, so its
        // pid doubles as the pgid every termination path signals.
        let pgid = child.id().ok_or_else(|| ToolError::Io {
            detail: "spawned bash child exited before its pid could be read".into(),
        })? as i32;

        let mut stdout_lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        let mut stderr_lines = BufReader::new(child.stderr.take().expect("piped stderr")).lines();

        let mut stdout_buf: Vec<String> = Vec::new();
        let mut stderr_buf: Vec<String> = Vec::new();
        let mut stdout_done = false;
        let mut stderr_done = false;
        let mut exit_status: Option<ExitStatus> = None;

        let deadline = Instant::now() + Duration::from_millis(args.timeout_ms);

        // `ctx.cancel` is a poll-based flag (conway-core cannot depend on
        // tokio, so it has no async `.cancelled()` future) — this loop polls
        // it, and the deadline, at least once per `POLL_INTERVAL` tick.
        let outcome = loop {
            if let Some(status) = exit_status {
                if stdout_done && stderr_done {
                    break Outcome::Completed(status);
                }
            }
            if ctx.cancel.is_cancelled() {
                break Outcome::Cancelled;
            }
            if Instant::now() >= deadline {
                break Outcome::TimedOut;
            }

            tokio::select! {
                line = stdout_lines.next_line(), if !stdout_done => {
                    match line {
                        Ok(Some(text)) => {
                            ctx.events.emit(Event::ToolProgress {
                                call_id: call_id.to_string(),
                                note: text.clone(),
                            });
                            stdout_buf.push(text);
                        }
                        _ => stdout_done = true,
                    }
                }
                line = stderr_lines.next_line(), if !stderr_done => {
                    match line {
                        Ok(Some(text)) => {
                            ctx.events.emit(Event::ToolProgress {
                                call_id: call_id.to_string(),
                                note: text.clone(),
                            });
                            stderr_buf.push(text);
                        }
                        _ => stderr_done = true,
                    }
                }
                status = child.wait(), if exit_status.is_none() => {
                    exit_status = status.ok();
                }
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        };

        match outcome {
            Outcome::Completed(status) => {
                let (code, is_error) = describe_exit(status);
                Ok(finish(&stdout_buf, &stderr_buf, &code, is_error, None))
            }
            Outcome::Cancelled => {
                kill_group(&mut child, pgid).await;
                Err(ToolError::Cancelled)
            }
            Outcome::TimedOut => {
                let status = kill_group(&mut child, pgid).await;
                let code = status
                    .map(|s| describe_exit(s).0)
                    .unwrap_or_else(|| "unknown".to_string());
                Ok(finish(
                    &stdout_buf,
                    &stderr_buf,
                    &code,
                    true,
                    Some(args.timeout_ms),
                ))
            }
        }
    }

    fn describe_exit(status: ExitStatus) -> (String, bool) {
        match status.code() {
            Some(code) => (code.to_string(), code != 0),
            None => (format!("signal {}", status.signal().unwrap_or(-1)), true),
        }
    }

    fn finish(
        stdout: &[String],
        stderr: &[String],
        exit_code: &str,
        is_error: bool,
        timed_out_after_ms: Option<u64>,
    ) -> ToolOutput {
        let stdout_body = if stdout.is_empty() {
            "(empty)".to_string()
        } else {
            stdout.join("\n")
        };
        let stderr_body = if stderr.is_empty() {
            "(empty)".to_string()
        } else {
            stderr.join("\n")
        };
        let mut text =
            format!("stdout:\n{stdout_body}\n\nstderr:\n{stderr_body}\n\nexit code: {exit_code}");
        if let Some(ms) = timed_out_after_ms {
            text.push_str(&format!("\ntimed out after {ms}ms"));
        }
        ToolOutput {
            blocks: vec![ContentBlock::Text { text }],
            is_error,
            truncation: TRUNCATION,
            artifacts: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_has_expected_name_category_permission() {
        let spec = BashTool::new().spec();
        assert_eq!(spec.name.as_str(), "bash");
        assert_eq!(spec.category, ToolCategory::Execute);
    }

    #[test]
    fn schema_required_and_properties() {
        let spec = BashTool::new().spec();
        let json = serde_json::to_value(&spec.schema).unwrap();
        let required: Vec<&str> = json["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["command"]);
        let props = json["properties"].as_object().unwrap();
        assert!(props.contains_key("command"));
        assert!(props.contains_key("timeout_ms"));
        assert!(props.contains_key("cwd"));
        assert_eq!(json["additionalProperties"], false);
    }

    // ---- render: the fix for "pattern grants are inert" ----

    /// The whole point of the override: `PatternRule` prefix-matches
    /// `rendered` against a granted command prefix, which only works when
    /// `rendered` IS the bare command -- not `bash({"command":"git status"})`
    /// (the generic default's shape, which the metacharacter gate rejects
    /// outright because of the JSON's own `{`/`}`/`"`).
    #[test]
    fn render_returns_the_bare_command_not_a_json_dump() {
        let rendered = BashTool::new().render(&serde_json::json!({"command": "git status"}));
        assert_eq!(rendered, "git status");
    }

    #[test]
    fn render_ignores_extra_fields_like_timeout_and_cwd() {
        let rendered = BashTool::new().render(&serde_json::json!({
            "command": "ls -la",
            "timeout_ms": 5000,
            "cwd": "/tmp",
        }));
        assert_eq!(rendered, "ls -la");
    }

    /// `args` is untrusted and may not even have a string `command`
    /// (a caller invoking `render` ahead of/without schema validation, or a
    /// future validator bug) -- this must degrade to the generic rendering,
    /// never panic.
    #[test]
    fn render_falls_back_without_panicking_on_a_missing_or_malformed_command() {
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"command": 5}),
            serde_json::json!(null),
            serde_json::json!("not an object"),
            serde_json::json!([1, 2, 3]),
        ] {
            let rendered = BashTool::new().render(&bad);
            assert!(rendered.starts_with("bash("), "{rendered:?}");
        }
    }

    // ---- confined_by_tool / the trait default ----

    /// `BashTool` never overrides `Tool::confined_by_tool` -- it stays the
    /// trait's own `false` default, regardless of which launcher built it,
    /// per `with_launcher`'s own doc: the launcher changes HOW the command
    /// runs, not what this TYPE claims about itself. A tool that wants to
    /// claim `true` wraps `BashTool::with_launcher` and delegates only
    /// `invoke` -- see `conway-plugin-confine`'s own `ConfinedBashTool`.
    #[test]
    fn confined_by_tool_is_false_regardless_of_launcher() {
        assert!(!BashTool::new().confined_by_tool());
        let alt = BashTool::with_launcher(Arc::new(default_launcher));
        assert!(!alt.confined_by_tool());
    }

    // ---- with_launcher: the pluggable-launcher seam ----

    #[cfg(unix)]
    #[tokio::test]
    async fn with_launcher_routes_through_the_supplied_launcher_not_the_default() {
        use conway_core::content::ContentBlock;

        use crate::testing::test_ctx;

        // A launcher that ignores the real command entirely and always
        // spawns something else -- the discriminating property: if
        // `with_launcher`'s own launcher were silently ignored (this seam's
        // own regression), this test would observe the ORIGINAL command's
        // output instead of this marker.
        let launcher: Launcher = Arc::new(|_command: &str, cwd: &Path| {
            let mut cmd = Command::new("/bin/bash");
            cmd.arg("-c").arg("echo launcher-marker").current_dir(cwd);
            cmd
        });
        let tool = BashTool::with_launcher(launcher);
        let (ctx, _handles) = test_ctx(std::env::temp_dir());
        let call = ToolCall {
            call_id: "tc_launcher".into(),
            name: ToolName::new("bash"),
            arguments: serde_json::json!({"command": "echo should-not-run"}),
        };
        let out = tool.invoke(call, ctx).await.unwrap();
        let text = match &out.blocks[0] {
            ContentBlock::Text { text } => text.clone(),
            other => panic!("expected a text block, got {other:?}"),
        };
        assert!(text.contains("launcher-marker"), "{text:?}");
        assert!(!text.contains("should-not-run"), "{text:?}");
    }
}
