//! [`ProcessHookRunner`]: the one-shot exec implementation of
//! `conway_core::ports::HookRunner` (board item 01KZRZY1MNM872BZ6AKEBG3SKE,
//! decision 01KZRZBQ2ACF40QGK8E9AVGMT3). Spawns the hook's configured
//! command fresh per invocation, writes the event as JSON to stdin, and
//! reads the answer from stdout plus the process's exit status --
//! deliberately NOT the long-lived NDJSON JSON-RPC transport the remote
//! plugin protocol uses: requiring that would mean no plain shell script
//! could be a hook.
//!
//! Reuses `crate::process::unix::kill_group` -- the identical
//! process-group-kill machinery `crate::shell::bash`'s `unix::run` uses --
//! so a hook that backgrounds a grandchild before exiting does not outlive
//! its own timeout -- one implementation, never restated.

use async_trait::async_trait;

use conway_core::error::HookFailure;
use conway_core::hook::{HookAnswer, HookInvocation};
use conway_core::ports::HookRunner;

/// One-shot, process-spawning [`HookRunner`]. No sandboxing, no allow/deny
/// list, no argument sanitization here (isolation belongs to tools, not the harness:
/// this is execution plumbing,
/// not a security boundary) -- mirrors `crate::shell::bash::BashTool`'s own
/// "no sandboxing" note. An operator's review of `[hooks].rules[].command`
/// (or a future permission point over it) is the control point, not this
/// type.
#[derive(Debug, Default)]
pub struct ProcessHookRunner;

impl ProcessHookRunner {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HookRunner for ProcessHookRunner {
    async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
        #[cfg(unix)]
        return unix::run(invocation).await;

        #[cfg(not(unix))]
        {
            let _ = invocation;
            Err(HookFailure::Spawn {
                detail: "hook runner requires a unix host".into(),
            })
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::process::{ExitStatus, Stdio};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::{Child, Command};
    use tokio::time::{Duration, Instant};

    use conway_core::error::HookFailure;
    use conway_core::hook::{HookAnswer, HookInvocation};

    use crate::process::unix::kill_group;

    pub(super) async fn run(invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
        let (program, args) =
            invocation
                .command
                .split_first()
                .ok_or_else(|| HookFailure::Spawn {
                    detail: "hook command is empty".into(),
                })?;

        let payload = serde_json::to_vec(&invocation.event).map_err(|err| HookFailure::Spawn {
            detail: format!("failed to serialize hook event payload: {err}"),
        })?;

        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);

        let mut child = command.spawn().map_err(|err| HookFailure::Spawn {
            detail: format!("failed to spawn '{program}': {err}"),
        })?;

        // `process_group(0)` makes the child its own group leader, so its
        // pid doubles as the pgid every termination path signals -- same
        // invariant `crate::shell::bash::unix::run` relies on.
        let pgid = child.id().ok_or_else(|| HookFailure::Spawn {
            detail: "spawned hook child exited before its pid could be read".into(),
        })? as i32;

        let deadline = Instant::now() + Duration::from_millis(invocation.timeout_ms);

        match tokio::time::timeout_at(deadline, drive(&mut child, &payload)).await {
            Ok(Ok((status, stdout))) => {
                if !status.success() {
                    return Err(HookFailure::NonzeroExit {
                        code: status.code(),
                    });
                }
                parse_answer(&stdout)
            }
            Ok(Err(detail)) => Err(HookFailure::Spawn { detail }),
            Err(_elapsed) => {
                kill_group(&mut child, pgid).await;
                Err(HookFailure::TimedOut {
                    after_ms: invocation.timeout_ms,
                })
            }
        }
    }

    /// Writes `payload` to the child's stdin (closing it afterward so a
    /// well-behaved hook sees EOF), then reads stdout/stderr to completion
    /// and waits for exit -- all CONCURRENTLY, and all inside the caller's
    /// `timeout_at`, so a hook that never reads stdin, or that fills the OS
    /// pipe buffer with stdout/stderr before being read, cannot deadlock
    /// against its own exit, and a hook that simply hangs is still bounded
    /// by the same deadline that governs everything else about this call.
    /// Stderr is drained but discarded: a hook's diagnostic output has
    /// nowhere principled to land yet (this item wires no event, hence no
    /// log/event sink to hand it to).
    async fn drive(child: &mut Child, payload: &[u8]) -> Result<(ExitStatus, Vec<u8>), String> {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let mut stdout_pipe = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");

        let write_fut = async {
            let _ = stdin.write_all(payload).await;
            let _ = stdin.shutdown().await;
            drop(stdin);
        };
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let stdout_fut = stdout_pipe.read_to_end(&mut stdout_buf);
        let stderr_fut = stderr_pipe.read_to_end(&mut stderr_buf);

        let (_, stdout_result, stderr_result, status) =
            tokio::join!(write_fut, stdout_fut, stderr_fut, child.wait());
        let _ = stdout_result;
        let _ = stderr_result;
        let status = status.map_err(|err| format!("failed to wait for hook child: {err}"))?;
        Ok((status, stdout_buf))
    }

    /// Exit 0's stdout must be either empty (accepted as the deliberately
    /// minimal `HookAnswer::default()` -- no context change proposed) or
    /// valid JSON matching `HookAnswer`'s shape; anything else is
    /// `HookFailure::UnparseableAnswer`, fail-closed exactly like every
    /// other failure this runner reports (never a panic, never a silently
    /// substituted default for genuinely malformed output).
    fn parse_answer(stdout: &[u8]) -> Result<HookAnswer, HookFailure> {
        let trimmed = stdout.trim_ascii();
        if trimmed.is_empty() {
            return Ok(HookAnswer::default());
        }
        serde_json::from_slice(trimmed).map_err(|err| HookFailure::UnparseableAnswer {
            detail: err.to_string(),
        })
    }
}
