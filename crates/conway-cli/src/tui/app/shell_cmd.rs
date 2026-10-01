//! `!`/`!>` operator-typed shell commands (board item
//! `01M1YVFRPH0DCE8N0DR5BS5BRT`, "Run a shell command yourself"; see
//! `docs/interactive.md`'s "Running a command yourself" section for the
//! operator-facing contract this module implements).
//!
//! ## The two forms
//!
//! - **`!command`** runs `command` and records it durably
//!   (`Conway::record_operator_shell`, a new, additive
//!   `LogRecord::OperatorShellRecord` -- see that variant's own doc for why
//!   a new record kind, not a `SystemNote`) WITHOUT ever admitting it to
//!   context: zero token cost, by design.
//! - **`!> command`** runs `command` and sends its output to the model as
//!   an ordinary prompt (`SessionHandle::prompt_agent`) -- already durable,
//!   already context-admitted, through the SAME path a typed message
//!   already takes. No new record kind for this form; see `Conway::
//!   record_operator_shell`'s own doc for the division of labor.
//! - **`!` alone, or `!>` alone** (nothing left after trimming): runs
//!   nothing.
//! - **`!!`** re-runs `AppState::last_shell_command`, in whichever form
//!   (`!`/`!>`) it last ran.
//!
//! ## Shell and cwd
//!
//! Spawned via `$SHELL -c <command>` when `$SHELL` is set and non-empty,
//! else `/bin/bash -c <command>` -- unlike `conway_tools::shell::BashTool`
//! (always `/bin/bash`, for determinism across MODEL-issued calls), the
//! OPERATOR'S own typed command runs in the OPERATOR'S own shell, so their
//! aliases/profile-derived `PATH`/etc. behave the way they expect from an
//! ordinary terminal.
//!
//! **cwd is `App::cwd` -- the session's SPAWN-time cwd, not a live,
//! `cd`-tracked one.** A disclosed, accepted gap: `conway_tools::fs::
//! CdTool` lets the MODEL move a session's working directory for
//! subsequent tool calls via a per-agent `conway_core::ports::plugin::
//! CwdHandle` cell, but that cell lives entirely inside the live
//! `conway-runtime` agent-loop task (`AgentLoop::run_inner`'s own `chdir`
//! local) -- no `SessionHandle`/`Conway` facade method reads its CURRENT
//! value from outside that task today. Building one is a real, separable
//! addition (a new facade method threading a read back out of the running
//! loop), not a two-line fix, and out of THIS item's scope. Using the
//! static spawn-time cwd instead is the honest, available approximation:
//! it is already what `AppState::cwd_display` shows the operator in the
//! status line, so a `!` command runs exactly where the status line says
//! the session is, even though a model `cd` earlier in the SAME session
//! would not be reflected here. Follow-up: a facade method exposing an
//! agent's live cwd would let this module read it instead.
//!
//! ## Deny rules, prompt rules, and why they differ for `!`
//!
//! `Conway::deny_rule_for_shell_command` -- a thin facade wrapper over
//! `conway_runtime::permission::PermissionBroker::deny_rule_for_shell_command`
//! -- is consulted BEFORE spawning anything: a `deny` rule targeting `bash`
//! refuses the command outright, naming the rule, exactly as it would a
//! model-issued `bash` call. See that broker method's own doc for the full
//! ruling and reasoning, restated briefly: `prompt` rules, `PermissionMode`,
//! `pre_tool_use` hooks, and the gate itself do NOT apply to a `!`
//! command -- all four exist to mediate the MODEL; the operator typing the
//! command already IS the human in the loop those mechanisms would
//! otherwise insert. `deny` is the one exception: an operator (or a
//! trusted project file) can refuse a whole class of command to
//! themselves too, and this module honors that unconditionally.
//!
//! `--root` does NOT confine a `!` command's string, for the identical
//! reason `BashTool::path_args`'s own doc gives for the model-facing tool:
//! a shell command reaches any path it likes via redirection, `cd`,
//! substitution, or a subprocess, so there is no finite scan that could
//! confine it. If `conway.confine`'s confined shell is installed and the
//! operator has opted into it for the `bash` tool, a FUTURE iteration of
//! this module could route `!` through it too -- not done here (disclosed
//! follow-up, `docs/interactive.md`'s own note): today a `!` command
//! always runs unconfined, through the plain resolved shell above,
//! regardless of `conway.confine`'s installation state.
//!
//! ## Cancellation, output bounds, and why no live streaming
//!
//! `Ctrl-C` kills the command's WHOLE process group, through
//! `conway_tools::process::unix::kill_group` (re-exported as
//! `conway::plugin::kill_group`, gated `cfg(all(unix, feature =
//! "builtin-tools"))`, the SAME re-export `conway_tools::shell::BashTool`
//! itself calls) -- never a second group-kill implementation, and never a
//! signal to the TUI's own process. The cancellation signal crosses from
//! `App::handle_ctrl_c` (a key event, on this loop's own `select!`) to the
//! spawned task via a plain `tokio::sync::oneshot` channel
//! (`App::shell_cancel_tx`) -- mirroring how every other spawned-off-loop
//! task in this crate (`app/await_cmd.rs`, `app/plugin_cmd.rs`) reports
//! back over its OWN dedicated channel rather than sharing one.
//!
//! Output is captured to completion, THEN pushed as one transcript entry
//! -- no live, line-by-line streaming into the transcript the way
//! `Event::ToolProgress` streams a MODEL-issued `bash` call's output.
//! `conway_tools::shell::BashTool`'s own streaming exists to let the model
//! react mid-run; an operator watching their own terminal has no such
//! need, and a bare wait-then-show is materially simpler, with the full
//! output still capped (see [`SHELL_OUTPUT_CAP`]) and the run itself never
//! blocking the event loop (performed inside a `tokio::spawn`ed task,
//! mirroring every other background call in this crate).

use std::path::Path;
use std::process::Stdio;

use chrono::Utc;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::sync::oneshot;
use tokio::time::{Duration, Instant};

use super::App;
use crate::tui::state::Entry;

/// Combined stdout+stderr byte cap. Generous enough for an ordinary
/// `git status`/test-suite run, bounded so a runaway command cannot
/// exhaust memory or flood the transcript -- the acceptance criterion
/// "output capture must be bounded (cap bytes, say when truncated)" made
/// concrete. Not configurable today (disclosed follow-up, same shape as
/// `conway_tools::shell::BashTool`'s own fixed `TRUNCATION` budget).
pub(super) const SHELL_OUTPUT_CAP: usize = 200_000;

/// Mirrors `conway_tools::shell::BashTool`'s own `DEFAULT_TIMEOUT_MS`
/// (120s) -- a `!` command with no operator-visible way to set a longer
/// timeout gets the SAME bound a model-issued `bash` call defaults to, so
/// a wedged `!` command cannot hang this session forever even if the
/// operator never presses `Ctrl-C`.
const SHELL_TIMEOUT: Duration = Duration::from_secs(120);

/// `App::submit`'s own parse of a `!`-prefixed line -- see this module's
/// own doc for the four shapes.
pub(super) enum Bang {
    /// `!`/`!>` alone, or any form whose command is empty once the leading
    /// marker is stripped and the rest is trimmed: "runs nothing".
    Empty,
    /// `!!` exactly: repeat `AppState::last_shell_command`.
    Rerun,
    Run {
        command: String,
        to_model: bool,
    },
}

/// Parses `text` (which must start with `!` -- `App::submit`'s own caller
/// checks this before reaching here) into a [`Bang`]. `!!` is recognized
/// ONLY as the exact two-character string -- `!!foo` is not a rerun, it is
/// the ordinary command `!foo` (bash's own history-expansion syntax is
/// irrelevant here: `bash -c` is never interactive, so this module need
/// not special-case it beyond this exact match).
pub(super) fn parse_bang(text: &str) -> Bang {
    if text == "!!" {
        return Bang::Rerun;
    }
    let (to_model, rest) = match text.strip_prefix("!>") {
        Some(rest) => (true, rest),
        None => (false, &text[1..]),
    };
    let command = rest.trim_start().to_string();
    if command.is_empty() {
        Bang::Empty
    } else {
        Bang::Run { command, to_model }
    }
}

/// One spawned `!` command's eventual reply (this module's own doc).
pub(super) struct ShellDone {
    pub(super) command: String,
    pub(super) output: String,
    pub(super) exit_code: Option<i32>,
    pub(super) truncated: bool,
    pub(super) to_model: bool,
}

/// A completed (or killed/timed-out) run's own captured facts, before
/// `App::apply_shell_done` turns them into a transcript entry and (for the
/// `!>` form) a prompt.
struct ExecResult {
    /// `"stdout:\n{..}\n\nstderr:\n{..}"`, plus one trailing line stating
    /// what happened when it was not a plain exit (`killed (Ctrl-C)`,
    /// `timed out after {ms}ms`, `terminated by signal {n}`, or a spawn
    /// failure) -- mirrors `conway_tools::shell::BashTool`'s own `finish`
    /// text shape, familiar to an operator who has already seen a
    /// model-issued `bash` call's result rendered that way.
    output: String,
    /// `None` exactly when the command did not exit with an ordinary code
    /// -- killed, timed out, terminated by signal, or never spawned.
    exit_code: Option<i32>,
    truncated: bool,
}

impl App {
    /// The `!`/`!>` entry point `App::submit` calls for every [`Bang::Run`]
    /// (directly, or via `Bang::Rerun`'s own lookup). Checks the one-
    /// command-at-a-time guard and the `deny` rule BEFORE spawning
    /// anything -- both refusals are synchronous, local, and push their own
    /// `Entry` immediately, with no task ever started. `AppState::
    /// last_shell_command` is stamped unconditionally, before either
    /// guard, so `!!` repeats exactly what was typed even when it was
    /// refused (see that field's own doc).
    pub(super) fn spawn_shell_command(&mut self, command: String, to_model: bool) {
        self.state.last_shell_command = Some((command.clone(), to_model));
        if self.state.shell_in_flight {
            self.state.transcript.push(Entry::Notice {
                text: "a `!` command is already running".to_string(),
            });
            return;
        }
        if let Some(rule) = self.conway.deny_rule_for_shell_command(&command) {
            // Review round 1 (finding 3): the OPERATOR'S OWN typed command
            // echoed back in a denial notice is still rendered text --
            // sanitized for the identical reason `execute`'s own output
            // capture is (see `append_capped`'s own doc).
            let safe_command = conway::sanitize_control_chars(&command);
            self.state.transcript.push(Entry::Error {
                text: format!("`{safe_command}` is denied by a `deny` rule ({rule})"),
                fatal: false,
            });
            return;
        }
        self.state.shell_in_flight = true;
        let (cancel_tx, cancel_rx) = oneshot::channel();
        self.shell_cancel_tx = Some(cancel_tx);
        let cwd = self.cwd.clone();
        let conway = self.conway.clone();
        let session = self.handle.id();
        let tx = self.shell_tx.clone();
        let command_for_task = command.clone();
        // Review round 1 (SIGNIFICANT finding 1): the `JoinHandle` is kept,
        // not discarded, so `Self::kill_shell_command_for_quit` has
        // something to bound-wait on -- see that method's own doc for why
        // a bare cancel signal, unawaited, is not enough to guarantee this
        // command's process group is actually dead before the process
        // exits.
        let task = tokio::spawn(async move {
            let result = execute(&command_for_task, &cwd, cancel_rx).await;
            // Review round 1 (finding 3): `command_for_task` is the exact
            // text that was actually RUN (needed raw, byte-for-byte, by
            // `execute` just above) -- this sanitized COPY is for every
            // downstream consumer that only ever DISPLAYS it (the durable
            // record, the live transcript entry, the `!>` form's own
            // to-model block), mirroring `append_capped`'s own identical
            // split between "what runs" and "what is shown" for `output`.
            let display_command = conway::sanitize_control_chars(&command_for_task);
            // Board item `01M1YVFRPH0DCE8N0DR5BS5BRT`: the bare `!` form's
            // own durable, non-context-admitted record -- see `Conway::
            // record_operator_shell`'s own doc for why the `!>` form (
            // `to_model`) needs no call here at all (its output becomes an
            // ordinary prompt, already durable, back in `App::run`'s own
            // `shell_rx` arm). Best-effort: a failed append must not lose
            // the operator's own view of what just ran, which `ShellDone`
            // carries regardless.
            if !to_model {
                let _ = conway
                    .record_operator_shell(
                        session,
                        display_command.clone(),
                        result.output.clone(),
                        result.exit_code,
                        result.truncated,
                    )
                    .await;
            }
            // The receiver only goes away once `App::run`'s loop has
            // already exited (quitting) -- nothing left to notify, and a
            // send failure here is silently dropped, mirroring every other
            // spawned task's own reply send in this crate (e.g.
            // `app/await_cmd.rs::run_await`).
            let _ = tx.send(ShellDone {
                command: display_command,
                output: result.output,
                exit_code: result.exit_code,
                truncated: result.truncated,
                to_model,
            });
        });
        self.shell_task = Some(task);
    }

    /// Review round 1 (SIGNIFICANT finding 1, "orphaned child on quit"):
    /// every quit path (`/quit`, `Ctrl-D`, the double-`Ctrl-C` exit, and
    /// every other `Effect::Quit`/`Action::Quit` arm in `app/run.rs`) calls
    /// this -- through `App::purge_open_ask_modal`, which now calls it
    /// unconditionally, same as every other quit-time cleanup step that
    /// method already performs -- BEFORE the app loop returns its exit
    /// code and the process actually exits.
    ///
    /// **Why a bare cancel signal is not enough, and `kill_on_drop(true)`
    /// (added to the `Command` in [`execute`]) is not enough either.** The
    /// spawned task's own `cancel_rx` arm already performs a real
    /// process-GROUP kill (`kill_child`, the SAME `conway_tools::process::
    /// unix::kill_group` call its timeout path uses) -- but sending on
    /// `shell_cancel_tx` only WAKES that `tokio::select!` arm; it does not
    /// block until the kill has actually run. If nothing then awaits the
    /// spawned task, `App::run` returns and the process exits with that
    /// task still mid-flight (or, worse, the whole `tokio::Runtime` tears
    /// down around it, which does not run a task to completion -- it is
    /// simply dropped at whatever `.await` point it was suspended on, and
    /// a dropped future's `tokio::process::Child` field runs tokio's own
    /// `kill_on_drop` -- which kills ONLY the leader PID, not the group,
    /// exactly the gap this finding named: a shell that does not `exec`
    /// into its single command (so the leader is `bash` itself, not the
    /// long-running job) would have its wrapper killed while the actual
    /// job it launched keeps running, orphaned, in the now-leaderless
    /// group). This method closes that gap by taking `App::shell_task`
    /// (the `JoinHandle` `Self::spawn_shell_command` kept for exactly this)
    /// and bound-awaiting it -- a REAL wait for the real group-kill to
    /// finish, not a fire-and-forget signal.
    ///
    /// Bounded (5s, mirroring `App::purge_open_ask_modal`'s own existing
    /// bounded waits elsewhere in this file for the identical reason): the
    /// process is exiting either way, so this can never hang the exit
    /// indefinitely even if the kill itself somehow wedges -- a residue
    /// `kill_on_drop(true)` (belt-and-suspenders for the leader specifically)
    /// and the OS's own init/launchd reparenting still bound the damage.
    pub(super) async fn kill_shell_command_for_quit(&mut self) {
        if let Some(tx) = self.shell_cancel_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.shell_task.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
        }
    }

    /// Applies one [`ShellDone`] reply: clears the in-flight guard and the
    /// (by now stale either way) cancellation sender, pushes the
    /// [`Entry::Shell`] transcript entry, and -- for the `!>` form only --
    /// returns the formatted block `App::run`'s own `shell_rx.recv()` arm
    /// must still send to the model via `SessionHandle::prompt_agent`
    /// (awaited THERE, not here: this method stays synchronous, mirroring
    /// every other `apply_*_done` in this crate, and awaiting it here
    /// would also get the ORDERING wrong -- see this module's own doc:
    /// the `!>` command's OWN entry must render before the live
    /// `Event::UserTurn` the prompt produces, which only holds if the
    /// transcript push below happens before `prompt_agent` is even
    /// called).
    pub(super) fn apply_shell_done(&mut self, done: ShellDone) -> Option<String> {
        self.state.shell_in_flight = false;
        self.shell_cancel_tx = None;
        // The task that just replied has already finished (this message IS
        // its reply) -- drop the now-stale `JoinHandle` so `Self::
        // kill_shell_command_for_quit` never bound-waits on a task that is
        // already done (harmless either way -- an already-finished
        // `JoinHandle` resolves instantly -- but this matches
        // `shell_cancel_tx`'s own "always cleared here" hygiene exactly).
        self.shell_task = None;
        self.state.transcript.push(Entry::Shell {
            command: done.command.clone(),
            output: done.output.clone(),
            exit_code: done.exit_code,
            truncated: done.truncated,
            to_model: done.to_model,
            ts: Some(Utc::now()),
        });
        if !done.to_model {
            return None;
        }
        // Review round 1 (SIGNIFICANT finding 3): **decision -- the `!>`
        // form's to-model block is sanitized too** (`done.command`/`done.
        // output` are already the sanitized copies -- see `Self::
        // spawn_shell_command`'s own `display_command` and `append_capped`
        // -- so this is already true here, not a separate call). A model
        // reading a raw escape sequence is harmless to IT (it has no
        // terminal to corrupt), but this block becomes an ordinary
        // `UserTurn`, which a RESUMED session's replay (`record_to_event`)
        // renders into the SAME live transcript pane a raw escape WOULD
        // actually reach -- the exact hazard this finding names, just
        // deferred to a later session instead of this one. Sanitizing
        // once, upstream of both destinations, closes it for both without
        // a second mechanism.
        let exit_line = match done.exit_code {
            Some(code) => format!("exit code: {code}"),
            None => "(killed or timed out -- see output above)".to_string(),
        };
        Some(format!(
            "I ran this myself:\n\n$ {}\n\n{}\n\n{exit_line}",
            done.command, done.output,
        ))
    }
}

/// `$SHELL -c`, when `$SHELL` is set and non-empty; else `/bin/bash -c` --
/// see this module's own doc for why the operator's own shell, not the
/// fixed `/bin/bash` `conway_tools::shell::BashTool` always uses.
fn resolve_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/bash".to_string())
}

/// Appends `line` (plus a separating `\n` when `buf` is already non-empty)
/// to `buf`, capped at [`SHELL_OUTPUT_CAP`] bytes total -- truncates at a
/// char boundary (never splits a multi-byte character) and sets
/// `*truncated = true` the moment any byte of `line` is dropped. Once
/// `buf` is already at the cap, every further call is a no-op (besides
/// marking truncated), so the cap is a TOTAL budget across every line, not
/// a per-line one.
///
/// **Review round 1 (SIGNIFICANT finding 3, "unsanitized output"):** `line`
/// is run through `conway::sanitize_control_chars` BEFORE
/// anything else -- the earliest point captured stdout/stderr bytes exist
/// as a Rust `String` at all -- so a raw ANSI/OSC escape sequence a
/// command's output carries (`curl`, `cat` of a downloaded file, ...)
/// never survives into `ExecResult::output`, and therefore never reaches
/// EITHER consumer downstream: the live transcript render
/// (`view/transcript.rs::shell_lines`) or the `!> command` form's own
/// to-model block (`App::apply_shell_done`). One call site, reused
/// everywhere -- the SAME shape `conway_runtime::tools::runner::
/// sanitize_rendered` already uses for the identical class of problem (a
/// tool's own `rendered` text), not a second, independently-applied
/// sanitize call per consumer.
fn append_capped(buf: &mut String, line: &str, truncated: &mut bool) {
    let line = conway::sanitize_control_chars(line);
    let line = line.as_str();
    if buf.len() >= SHELL_OUTPUT_CAP {
        *truncated = true;
        return;
    }
    if !buf.is_empty() {
        buf.push('\n');
    }
    let remaining = SHELL_OUTPUT_CAP.saturating_sub(buf.len());
    if line.len() > remaining {
        let mut end = remaining.min(line.len());
        while end > 0 && !line.is_char_boundary(end) {
            end -= 1;
        }
        buf.push_str(&line[..end]);
        *truncated = true;
    } else {
        buf.push_str(line);
    }
}

/// Reads whatever lines are ALREADY buffered on `lines`, right now, without
/// waiting for more to arrive -- the identical zero-wait-timeout idiom
/// `conway_tools::shell::BashTool`'s own `drain_available` uses (that
/// module's own doc explains why `tokio::time::timeout(Duration::ZERO,
/// ..)` is deterministic here, not a race), restated rather than reused
/// (that function is private to a different crate, and this module's
/// signature does not need `ctx`/`call_id`/live progress emission at all).
async fn drain_available<R>(lines: &mut Lines<R>, buf: &mut String, truncated: &mut bool)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    while let Ok(Ok(Some(text))) = tokio::time::timeout(Duration::ZERO, lines.next_line()).await {
        append_capped(buf, &text, truncated);
    }
}

/// How this run ended -- resolved by the `tokio::select!` loop in
/// [`execute`].
enum Outcome {
    Completed(std::process::ExitStatus),
    Cancelled,
    TimedOut,
    /// `child.wait()` itself returned an OS-level error (the wait syscall
    /// failing, not the child exiting) -- rare, but left unhandled it
    /// would spin the loop until the deadline; treated as a definite
    /// failure (after a defensive group kill, since the child's true
    /// state is unknown), mirroring `conway_tools::shell::BashTool`'s own
    /// identical `WaitFailed` case.
    WaitFailed,
}

/// Runs `command` via the resolved shell (see [`resolve_shell`]) in `cwd`,
/// as its own process-group leader, capturing combined stdout+stderr up to
/// [`SHELL_OUTPUT_CAP`] bytes, bounded by [`SHELL_TIMEOUT`], cancellable
/// through `cancel_rx` -- `App::handle_ctrl_c`'s own send on
/// `App::shell_cancel_tx`. Never blocks the caller's own event loop: this
/// is always run inside a `tokio::spawn`ed task (`App::
/// spawn_shell_command`), never awaited inline on it.
async fn execute(command: &str, cwd: &Path, mut cancel_rx: oneshot::Receiver<()>) -> ExecResult {
    let shell_bin = resolve_shell();
    let mut cmd = tokio::process::Command::new(&shell_bin);
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Review round 1 (SIGNIFICANT finding 1): belt-and-suspenders for
        // the leader specifically, mirroring `conway_tools::process::
        // child_session`'s own `Command` exactly (that module's own doc:
        // "even if [the group] `kill(-pgid)` is beaten to it, the leader
        // dies when the `Child` handle drops") -- even if `App::
        // kill_shell_command_for_quit`'s own bounded wait is somehow never
        // reached, dropping this `Child` (e.g. the whole `tokio::Runtime`
        // tearing down around a still-suspended task) still kills the
        // leader process. This is NOT sufficient on its own for the whole
        // GROUP (see that method's own doc for the exact gap it leaves);
        // it is additive to the explicit kill, not a replacement for it.
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return ExecResult {
                output: format!(
                    "stdout:\n(empty)\n\nstderr:\n(empty)\n\nfailed to spawn \
                                  {shell_bin}: {e}"
                ),
                exit_code: None,
                truncated: false,
            };
        }
    };
    // `process_group(0)` (unix) makes the child its own group leader, so
    // its pid doubles as the pgid every termination path signals -- `None`
    // only if the child already exited before its pid could even be read
    // (vanishingly rare; handled below by simply having nothing left to
    // kill).
    let pgid = child.id().map(|id| id as i32);

    let mut stdout_lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let mut stderr_lines = BufReader::new(child.stderr.take().expect("piped stderr")).lines();
    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut truncated = false;
    let deadline = Instant::now() + SHELL_TIMEOUT;

    let outcome = loop {
        if Instant::now() >= deadline {
            break Outcome::TimedOut;
        }
        tokio::select! {
            line = stdout_lines.next_line(), if !stdout_done => {
                match line {
                    Ok(Some(text)) => append_capped(&mut stdout_buf, &text, &mut truncated),
                    _ => stdout_done = true,
                }
            }
            line = stderr_lines.next_line(), if !stderr_done => {
                match line {
                    Ok(Some(text)) => append_capped(&mut stderr_buf, &text, &mut truncated),
                    _ => stderr_done = true,
                }
            }
            status = child.wait() => {
                match status {
                    Ok(status) => break Outcome::Completed(status),
                    Err(_) => break Outcome::WaitFailed,
                }
            }
            _ = &mut cancel_rx => {
                break Outcome::Cancelled;
            }
        }
    };

    match outcome {
        Outcome::Completed(status) => {
            drain_available(&mut stdout_lines, &mut stdout_buf, &mut truncated).await;
            drain_available(&mut stderr_lines, &mut stderr_buf, &mut truncated).await;
            let exit_code = status.code();
            let mut output = format!(
                "stdout:\n{}\n\nstderr:\n{}",
                non_empty(&stdout_buf),
                non_empty(&stderr_buf),
            );
            if exit_code.is_none() {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(sig) = status.signal() {
                        output.push_str(&format!("\n\nterminated by signal {sig}"));
                    }
                }
            }
            ExecResult {
                output,
                exit_code,
                truncated,
            }
        }
        Outcome::Cancelled => {
            kill_child(&mut child, pgid).await;
            ExecResult {
                output: format!(
                    "stdout:\n{}\n\nstderr:\n{}\n\nkilled (Ctrl-C)",
                    non_empty(&stdout_buf),
                    non_empty(&stderr_buf),
                ),
                exit_code: None,
                truncated,
            }
        }
        Outcome::TimedOut => {
            kill_child(&mut child, pgid).await;
            ExecResult {
                output: format!(
                    "stdout:\n{}\n\nstderr:\n{}\n\ntimed out after {}ms",
                    non_empty(&stdout_buf),
                    non_empty(&stderr_buf),
                    SHELL_TIMEOUT.as_millis(),
                ),
                exit_code: None,
                truncated,
            }
        }
        Outcome::WaitFailed => {
            kill_child(&mut child, pgid).await;
            ExecResult {
                output: format!(
                    "stdout:\n{}\n\nstderr:\n{}\n\nfailed to wait for the shell process",
                    non_empty(&stdout_buf),
                    non_empty(&stderr_buf),
                ),
                exit_code: None,
                truncated,
            }
        }
    }
}

fn non_empty(s: &str) -> &str {
    if s.is_empty() {
        "(empty)"
    } else {
        s
    }
}

/// Kills `child`'s whole process group through `conway_tools::process::
/// unix::kill_group` (re-exported `conway::plugin::kill_group`) -- see this
/// module's own doc for why this must never be a second implementation.
/// `pgid: None` (the child exited before its pid could be read) is a no-op:
/// there is nothing left to kill.
#[cfg(unix)]
async fn kill_child(child: &mut tokio::process::Child, pgid: Option<i32>) {
    if let Some(pgid) = pgid {
        let _ = conway::plugin::kill_group(child, pgid).await;
    }
}

/// Non-unix hosts have no `conway::plugin::kill_group` (that re-export is
/// `cfg(unix)`, mirroring `conway_tools::process::unix`'s own gate) --
/// best-effort plain kill instead. `!` commands on a non-unix host are not
/// otherwise exercised by this crate's own test suite today (every
/// production host this ships to is unix).
#[cfg(not(unix))]
async fn kill_child(child: &mut tokio::process::Child, _pgid: Option<i32>) {
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::*;
    use crate::tui::state::Entry;

    fn parse(text: &str) -> Bang {
        parse_bang(text)
    }

    #[test]
    fn bang_alone_runs_nothing() {
        assert!(matches!(parse("!"), Bang::Empty));
    }

    #[test]
    fn bang_arrow_alone_runs_nothing() {
        assert!(matches!(parse("!>"), Bang::Empty));
        assert!(matches!(parse("!> "), Bang::Empty));
    }

    #[test]
    fn double_bang_is_rerun() {
        assert!(matches!(parse("!!"), Bang::Rerun));
    }

    #[test]
    fn plain_bang_command_is_not_sent_to_model() {
        match parse("!git status") {
            Bang::Run { command, to_model } => {
                assert_eq!(command, "git status");
                assert!(!to_model);
            }
            _ => panic!("expected Bang::Run"),
        }
    }

    #[test]
    fn leading_space_after_bang_is_trimmed() {
        match parse("! git status") {
            Bang::Run { command, to_model } => {
                assert_eq!(command, "git status");
                assert!(!to_model);
            }
            _ => panic!("expected Bang::Run"),
        }
    }

    #[test]
    fn arrow_form_is_sent_to_model() {
        match parse("!> echo hi") {
            Bang::Run { command, to_model } => {
                assert_eq!(command, "echo hi");
                assert!(to_model);
            }
            _ => panic!("expected Bang::Run"),
        }
    }

    #[test]
    fn arrow_form_with_no_space_is_also_sent_to_model() {
        match parse("!>echo hi") {
            Bang::Run { command, to_model } => {
                assert_eq!(command, "echo hi");
                assert!(to_model);
            }
            _ => panic!("expected Bang::Run"),
        }
    }

    /// Acceptance 1: `!git status` shows output and exit code; `/context`
    /// does not grow. The second half is proved at the `conway-runtime`
    /// layer (`context::builder::own_segment_provenance_tests::
    /// build_never_admits_an_operator_shell_records_command_or_output`);
    /// this end-to-end test proves the first half through the REAL
    /// `App::submit` -> `spawn_shell_command` -> `shell_rx` ->
    /// `apply_shell_done` path, and that `shell_in_flight` clears.
    #[tokio::test]
    async fn plain_bang_runs_and_shows_output_and_exit_code() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let outcome = app
            .submit("!echo hello-from-bang".to_string())
            .await
            .expect("submit should not error");
        assert!(matches!(outcome, super::super::SubmitOutcome::Continue));
        assert!(app.state.shell_in_flight, "the command is now running");

        let done = tokio::time::timeout(
            StdDuration::from_secs(5),
            app.shell_rx.as_mut().expect("shell_rx is set").recv(),
        )
        .await
        .expect("the spawned shell task must reply promptly")
        .expect("shell_tx's sender half is alive for the duration");

        let to_model_block = app.apply_shell_done(done);
        assert!(
            to_model_block.is_none(),
            "the plain form sends nothing to the model"
        );
        assert!(!app.state.shell_in_flight);

        let entry = app
            .state
            .transcript
            .iter()
            .rev()
            .find_map(|e| match e {
                Entry::Shell {
                    command,
                    output,
                    exit_code,
                    to_model,
                    ..
                } => Some((command.clone(), output.clone(), *exit_code, *to_model)),
                _ => None,
            })
            .expect("an Entry::Shell must have been pushed");
        assert_eq!(entry.0, "echo hello-from-bang");
        assert!(entry.1.contains("hello-from-bang"), "{:?}", entry.1);
        assert_eq!(entry.2, Some(0));
        assert!(!entry.3);
    }

    /// Acceptance 2: `!> echo hi` sends the output to the model. Proved
    /// here as far as `apply_shell_done`'s own return value (the block
    /// `App::run`'s `shell_rx` arm would hand to `prompt_agent`) -- the
    /// "the next assembled request contains it" half is a property of
    /// `prompt_agent`/`UserTurn`/`ContextBuilder`, already covered by this
    /// crate's and `conway-runtime`'s own existing prompt/context tests
    /// (an ordinary `UserTurn` is unconditionally context-admitted; no new
    /// mechanism was introduced for this form at all -- see this module's
    /// own doc).
    #[tokio::test]
    async fn arrow_bang_runs_and_returns_a_block_for_the_model() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("!> echo hi-model".to_string())
            .await
            .expect("submit should not error");

        let done = tokio::time::timeout(
            StdDuration::from_secs(5),
            app.shell_rx.as_mut().expect("shell_rx is set").recv(),
        )
        .await
        .expect("the spawned shell task must reply promptly")
        .expect("shell_tx's sender half is alive for the duration");

        let block = app
            .apply_shell_done(done)
            .expect("the arrow form must return a block for the model");
        assert!(block.contains("echo hi-model"));
        assert!(block.contains("hi-model"));
        assert!(block.contains("exit code: 0"));
    }

    /// Acceptance 3: a `bash` deny rule blocks `!rm -rf x`, naming the
    /// rule, and never spawns anything (`shell_in_flight` never flips).
    #[tokio::test]
    async fn a_bash_deny_rule_blocks_a_bang_command_and_names_the_rule() {
        let conway = echo_conway();
        conway.grant_deny_pattern(
            conway_core::permission_pattern::PatternRule::parse("bash:rm -rf").expect("valid rule"),
            std::path::PathBuf::from("/repo/.conway/permissions.json"),
        );
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("!rm -rf x".to_string())
            .await
            .expect("submit should not error");

        assert!(
            !app.state.shell_in_flight,
            "a denied command must never spawn"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, .. } if text.contains("rm -rf") && text.contains("denied")
            )),
            "the denial must name the rule: {:?}",
            app.state.transcript
        );
    }

    /// `!!` with nothing run yet is a plain notice, not a panic or a
    /// silent no-op.
    #[tokio::test]
    async fn double_bang_with_no_prior_command_is_a_notice() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("!!".to_string())
            .await
            .expect("submit should not error");

        assert!(!app.state.shell_in_flight);
        assert!(app.state.transcript.iter().any(|e| matches!(
            e,
            Entry::Notice { text } if text.contains("no previous")
        )));
    }

    /// `!!` repeats the exact same command (and form) most recently run.
    #[tokio::test]
    async fn double_bang_reruns_the_last_command() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        app.submit("!echo first-run".to_string())
            .await
            .expect("submit should not error");
        let done = app
            .shell_rx
            .as_mut()
            .expect("shell_rx is set")
            .recv()
            .await
            .expect("the first run must reply");
        app.apply_shell_done(done);

        app.submit("!!".to_string())
            .await
            .expect("submit should not error");
        let done = tokio::time::timeout(
            StdDuration::from_secs(5),
            app.shell_rx.as_mut().expect("shell_rx is set").recv(),
        )
        .await
        .expect("the rerun must reply promptly")
        .expect("shell_tx's sender half is alive for the duration");
        assert_eq!(done.command, "echo first-run");
    }

    /// A byte cap on captured output: `SHELL_OUTPUT_CAP` is enforced and
    /// `truncated` is reported honestly once exceeded.
    #[tokio::test]
    async fn output_is_capped_and_truncation_is_reported() {
        let (_tx, rx) = oneshot::channel::<()>();
        // A command that writes well over the cap -- `yes` piped through
        // `head` keeps this fast and deterministic rather than depending on
        // real wall-clock throughput.
        let big = SHELL_OUTPUT_CAP * 2;
        let command = format!("yes x | head -c {big}");
        let result = execute(&command, &std::env::temp_dir(), rx).await;
        assert!(result.truncated, "exceeding the cap must set truncated");
        assert!(
            result.output.len() < SHELL_OUTPUT_CAP + 4096,
            "captured output must stay bounded near the cap: {} bytes",
            result.output.len()
        );
        drop(_tx);
    }

    /// Review round 1, SIGNIFICANT finding 3 ("unsanitized output"):
    /// `execute` itself -- not merely the render layer -- never returns a
    /// raw control byte in `ExecResult::output`. A real shell command
    /// (`printf`) genuinely emits the ESC byte on its stdout here; this
    /// proves `append_capped`'s own sanitize call actually runs on real
    /// captured bytes, not just on a hand-built string a unit test
    /// constructs directly.
    #[tokio::test]
    async fn execute_sanitizes_a_real_raw_escape_sequence_from_the_childs_own_stdout() {
        let (_tx, rx) = oneshot::channel::<()>();
        let command = "printf 'before\\033[2Jafter'";
        let result = execute(command, &std::env::temp_dir(), rx).await;
        assert!(
            !result.output.contains('\x1b'),
            "a raw ESC byte from the child's own stdout must never survive into \
             ExecResult::output: {:?}",
            result.output
        );
        assert!(result.output.contains("before"), "{:?}", result.output);
        assert!(result.output.contains("after"), "{:?}", result.output);
        drop(_tx);
    }

    /// Break-the-guard for acceptance 4 (Ctrl-C kills the process group):
    /// the command writes its OWN pid to a file, then sleeps far longer
    /// than this test's own timeout; `cancel_tx` fires, and this asserts
    /// the OS-level process is ACTUALLY GONE (`kill -0 <pid>` fails) --
    /// not merely that `execute` returned a `killed`-labeled result, which
    /// a mislabeled-but-leaked child would satisfy just as easily. Mirrors
    /// `conway_tools`' own `assert_group_dead` test idiom
    /// (`crates/conway-tools/tests/shell_bash.rs`) for the identical
    /// reason: a leaked, still-sleeping child would also make this test's
    /// OWN 5s timeout below fail (the `nix::sys::signal::kill(pid, 0)`
    /// polling loop would never observe `ESRCH`).
    #[tokio::test]
    async fn ctrl_c_kills_the_child_process_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("pid");
        let command = format!("echo $$ > {} && sleep 30", pid_file.display());
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let cwd = std::env::temp_dir();
        let handle = tokio::spawn(async move { execute(&command, &cwd, cancel_rx).await });

        // Wait for the pid file to actually appear before cancelling --
        // otherwise a slow spawn could race `cancel_tx` ahead of the
        // child's own `echo $$` ever running.
        let pid: i32 = {
            let mut pid = None;
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(&pid_file) {
                    if let Ok(parsed) = text.trim().parse() {
                        pid = Some(parsed);
                        break;
                    }
                }
                tokio::time::sleep(StdDuration::from_millis(20)).await;
            }
            pid.expect("the child must write its pid within 2s")
        };

        cancel_tx
            .send(())
            .expect("the task must still be waiting on cancel_rx");

        let result = tokio::time::timeout(StdDuration::from_secs(5), handle)
            .await
            .expect("cancellation must make execute return promptly, not wait out the sleep")
            .expect("the spawned task must not panic");
        assert_eq!(result.exit_code, None);
        assert!(result.output.contains("killed"));

        // The sharpest possible assertion: the process this pid names is
        // actually gone, polled (never a blind single check, which could
        // race the signal's own delivery) up to 2s.
        let mut still_alive = true;
        for _ in 0..100 {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                still_alive = false;
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
        assert!(
            !still_alive,
            "pid {pid} must no longer exist after Ctrl-C -- the process group was not actually \
             killed, only labeled as such"
        );
    }

    /// Review round 1, SIGNIFICANT finding 1 ("orphaned child on quit"):
    /// quitting (here, `App::purge_open_ask_modal` directly -- the SAME
    /// funnel EVERY quit path, `/quit`/`Ctrl-D`/double-`Ctrl-C`/every other
    /// `Effect::Quit` arm, now calls) while a `!` command is running must
    /// not orphan it.
    ///
    /// **Deliberately a BACKGROUNDED grandchild (`sleep 999999 &`, with
    /// `wait` as bash's own final statement), not a plain `&&`-chained
    /// command.** For a plain "last command in the script" shape, bash's
    /// own exec optimization can replace the leader's process image with
    /// the final command's -- so a leader-only kill (`kill_on_drop(true)`
    /// alone, with NO group signal at all) would coincidentally still
    /// catch it, proving nothing about whether the group kill itself ran.
    /// A background job never gets its own process group under
    /// non-interactive `bash -c` (`conway_tools::shell::BashTool`'s own
    /// module doc states this exactly), so `sleep`'s pid here is
    /// GENUINELY DIFFERENT from the leader bash's -- only a real
    /// `kill(-pgid, ..)` reaches it. This is the sharpest version of this
    /// test: it would catch a regression that reverted `Self::
    /// kill_shell_command_for_quit`'s explicit, awaited group-kill back to
    /// relying on `kill_on_drop(true)` alone.
    #[tokio::test]
    async fn quitting_while_a_bang_command_runs_kills_its_whole_process_group() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("pid");
        app.submit(format!(
            "!sleep 999999 & echo $! > {} && wait",
            pid_file.display()
        ))
        .await
        .expect("submit should not error");
        assert!(app.state.shell_in_flight, "the command is now running");

        let pid: i32 = {
            let mut pid = None;
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(&pid_file) {
                    if let Ok(parsed) = text.trim().parse() {
                        pid = Some(parsed);
                        break;
                    }
                }
                tokio::time::sleep(StdDuration::from_millis(20)).await;
            }
            pid.expect("the backgrounded sleep must write its pid within 2s")
        };

        // The exact funnel `/quit`, `Ctrl-D`, the double-`Ctrl-C` exit, and
        // every other `Effect::Quit` arm now call.
        tokio::time::timeout(StdDuration::from_secs(5), app.purge_open_ask_modal())
            .await
            .expect("quitting must not hang waiting on the shell command's own kill");

        let mut still_alive = true;
        for _ in 0..100 {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                still_alive = false;
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
        assert!(
            !still_alive,
            "pid {pid} (the BACKGROUNDED grandchild, not the leader) must no longer exist after \
             quitting while a `!` command was running -- it would have been orphaned by a \
             leader-only kill"
        );
    }
}
