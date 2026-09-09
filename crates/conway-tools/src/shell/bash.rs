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
/// directory. [`BashTool::new`]'s own launcher (`default_launcher`) is
/// `/bin/bash -c <command>`, `current_dir(cwd)`; the run loop
/// (`unix::run`) wires stdin/stdout/stderr and the process group onto
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
/// `Event::ToolProgress`. The call returns as soon as the launched `bash -c`
/// process itself exits — it does not wait for a backgrounded (`cmd &`)
/// grandchild that inherited its pipes to release them (see `unix::run`'s
/// own doc for why waiting for that would be wrong). The whole process
/// group — including any backgrounded children — is killed only on
/// cancellation or timeout; a normal, successful return leaves backgrounded
/// children running and names them in the result text instead. See
/// `docs/tools.md`'s "What `&` does inside `bash`" section for the full
/// contract.
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
    /// Plain, unconfined `bash` -- `default_launcher`. Byte-for-byte the
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

    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    use tokio::io::{AsyncBufReadExt, BufReader, Lines};
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
        /// The launched `bash -c` process itself exited. Its stdout/stderr
        /// may still be held open by a backgrounded grandchild — that is
        /// not this call's problem to wait out (see `run`'s own doc).
        Completed(ExitStatus),
        Cancelled,
        TimedOut,
        /// `child.wait()` itself returned an OS-level error (not the child
        /// exiting — the wait syscall failing). Rare, and not something
        /// this item's fix introduces, but left unhandled it used to spin
        /// the loop until the deadline; treating it as a definite failure
        /// (after a defensive group kill, since the child's true state is
        /// unknown) is strictly safer than that.
        WaitFailed(std::io::Error),
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

        let deadline = Instant::now() + Duration::from_millis(args.timeout_ms);

        // `ctx.cancel` is a poll-based flag (conway-core cannot depend on
        // tokio, so it has no async `.cancelled()` future) — this loop polls
        // it, and the deadline, at least once per `POLL_INTERVAL` tick.
        //
        // **Why this loop no longer waits for `stdout_done && stderr_done`
        // before declaring the call complete.** It used to: the previous
        // shape only broke to `Completed` once BOTH the child had exited AND
        // both pipes had reached EOF. A backgrounded grandchild (`cmd &`)
        // inherits these same pipe fds — non-interactive `bash -c` has job
        // control off, so `&` does not fork a new process group, and the
        // fds are inherited exactly like any other open descriptor across
        // `fork` — so the pipe's write end stays open, and EOF never
        // arrives, until either the grandchild exits or this call's own
        // deadline does. That made a deliberately-backgrounded `sleep 30 &`
        // hold the tool to its FULL timeout and then, on the timeout path,
        // `kill_group` the very job the model asked to keep running. The
        // fix: the launched `bash -c` child's own exit (`child.wait()`
        // resolving) IS this call's completion, full stop — reached for
        // below the moment it's ready, not gated on the pipes too.
        let outcome = loop {
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
                status = child.wait() => {
                    match status {
                        Ok(status) => break Outcome::Completed(status),
                        Err(err) => break Outcome::WaitFailed(err),
                    }
                }
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        };

        match outcome {
            Outcome::Completed(status) => {
                // The shell itself is done; take whatever is ALREADY sitting
                // in the pipe buffers (a zero-wait drain — see
                // `drain_available`'s own doc for why this is deterministic,
                // not a race), then stop reading. A backgrounded grandchild
                // that still holds these pipes open keeps them open — this
                // only closes conway's OWN read ends, which is not a signal
                // to the grandchild in any way.
                drain_available(&mut stdout_lines, &mut stdout_buf, call_id, &ctx).await;
                drain_available(&mut stderr_lines, &mut stderr_buf, call_id, &ctx).await;
                drop(stdout_lines);
                drop(stderr_lines);

                // Never killed on a successful return (that remains
                // exclusively a timeout/cancellation behavior, below) — so
                // any OTHER live member of the process group is a
                // deliberately backgrounded job the model started and this
                // result names rather than silently drops on the floor.
                let still_running = still_running_group_members(pgid).await;
                let (code, is_error) = describe_exit(status);
                Ok(finish(
                    &stdout_buf,
                    &stderr_buf,
                    Some(&code),
                    is_error,
                    None,
                    &still_running,
                ))
            }
            Outcome::Cancelled => {
                kill_group(&mut child, pgid).await;
                Err(ToolError::Cancelled)
            }
            Outcome::TimedOut => {
                // The whole group dies here — this is the ONE case (with
                // `Cancelled`, above) where a backgrounded child does not
                // survive the call, per this tool's documented safety
                // property. The killed process's own exit code/signal is
                // deliberately NOT reported alongside `timed out after
                // ...ms`: a call that timed out never validly completed, so
                // there is no exit code to report — reporting the SIGTERM/
                // SIGKILL that ended it would just be a second, misleading
                // way of saying the same "it didn't finish" fact this
                // result already states once.
                kill_group(&mut child, pgid).await;
                Ok(finish(
                    &stdout_buf,
                    &stderr_buf,
                    None,
                    true,
                    Some(args.timeout_ms),
                    &[],
                ))
            }
            Outcome::WaitFailed(err) => {
                kill_group(&mut child, pgid).await;
                Err(ToolError::Io {
                    detail: format!("failed to wait for bash: {err}"),
                })
            }
        }
    }

    /// Reads whatever lines are ALREADY buffered on `lines`, right now,
    /// without waiting for more to arrive.
    ///
    /// `tokio::time::timeout(Duration::ZERO, fut)` is deterministic here, not
    /// a "usually fast enough" race: `Timeout::poll` polls the wrapped
    /// future FIRST and returns its result immediately if it's `Ready`,
    /// checking the deadline only when it isn't (see
    /// `tokio::time::timeout`'s own implementation) — so if the OS already
    /// delivered a line into the pipe buffer, this returns it every time; if
    /// it hasn't, this returns promptly rather than blocking on a
    /// grandchild that may never write again.
    async fn drain_available<R>(
        lines: &mut Lines<R>,
        buf: &mut Vec<String>,
        call_id: &str,
        ctx: &ToolCtx,
    ) where
        R: tokio::io::AsyncBufRead + Unpin,
    {
        while let Ok(Ok(Some(text))) = tokio::time::timeout(Duration::ZERO, lines.next_line()).await
        {
            ctx.events.emit(Event::ToolProgress {
                call_id: call_id.to_string(),
                note: text.clone(),
            });
            buf.push(text);
        }
    }

    /// Lists the pids of every OTHER live member of process group `pgid` —
    /// i.e. a backgrounded (`cmd &`) grandchild still running after the
    /// launched `bash -c` leader itself has exited and been reaped.
    ///
    /// Two-step, cheapest-first: `kill(-pgid, 0)` is a single syscall that
    /// fails with `ESRCH` the instant nothing is left in the group — the
    /// overwhelmingly common case (nothing was backgrounded), so the vast
    /// majority of calls never do more than that. Only when the group DOES
    /// still have a live member does this shell out to `ps` for the actual
    /// pid(s), because listing "which pids share this pgid" has no portable
    /// syscall — `/proc` would need a Linux-only branch, and this runs on
    /// the operator's macOS dev box too (CI is Linux-only; this box is not).
    /// `ps -eo pid=,pgid=` is the one flag set both `ps` implementations
    /// (BSD `ps` on macOS, procps-ng on Linux) answer identically —
    /// verified against both, not assumed.
    async fn still_running_group_members(pgid: i32) -> Vec<i32> {
        if kill(Pid::from_raw(-pgid), None).is_err() {
            return Vec::new();
        }

        let Ok(output) = tokio::process::Command::new("ps")
            .args(["-eo", "pid=,pgid="])
            .output()
            .await
        else {
            return Vec::new();
        };

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid: i32 = fields.next()?.parse().ok()?;
                let group: i32 = fields.next()?.parse().ok()?;
                (group == pgid).then_some(pid)
            })
            .collect()
    }

    fn describe_exit(status: ExitStatus) -> (String, bool) {
        match status.code() {
            Some(code) => (code.to_string(), code != 0),
            None => (format!("signal {}", status.signal().unwrap_or(-1)), true),
        }
    }

    /// Builds the result text. `exit_code` and `timed_out_after_ms` are
    /// mutually exclusive by construction — every call site above passes
    /// exactly one as `Some` — because a call that reports an exit code
    /// completed, and a call that timed out never validly completed to
    /// report one; the two facts are never true at once, so this never
    /// prints both (the bug this item exists to fix: a backgrounded call
    /// used to report `exit code: 0` AND `timed out after ...ms` on the
    /// same line, describing an outcome that never happened).
    fn finish(
        stdout: &[String],
        stderr: &[String],
        exit_code: Option<&str>,
        is_error: bool,
        timed_out_after_ms: Option<u64>,
        still_running: &[i32],
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
        let mut text = format!("stdout:\n{stdout_body}\n\nstderr:\n{stderr_body}\n\n");
        match (exit_code, timed_out_after_ms) {
            (Some(code), None) => text.push_str(&format!("exit code: {code}")),
            (None, Some(ms)) => text.push_str(&format!("timed out after {ms}ms")),
            (Some(_), Some(_)) | (None, None) => {
                unreachable!("bash.rs: exit_code and timed_out_after_ms are mutually exclusive")
            }
        }
        if !still_running.is_empty() {
            let pids = still_running
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!(
                "\n{} background process(es) still running: pid {pids}",
                still_running.len()
            ));
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
