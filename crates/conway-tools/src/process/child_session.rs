//! Generic child-process session lifecycle: spawn once, keep the child
//! alive across many id-correlated NDJSON round trips over its stdin/
//! stdout, and fail closed -- never a hang, never a silent retry -- on the
//! four causes every such session shares (board item
//! `01M0TV7ZDS8X4F4TEJPRZB9P6T`, extending board items
//! `01M0EKVR1BEXXS75NV2JC4HZZ9`'s [`super::unix::kill_group`] consolidation
//! and `01M0TV6E2K6QF9VXP6C7TFH06X`'s `DEFAULT_TIMEOUT_MS` re-export onto the
//! SAME facade route).
//!
//! **What this module proves.** `conway-plugin-mcp::session::McpSession`
//! (JSON-RPC 2.0 over stdio) and `conway-plugin-subprocess::session::
//! PersistentSession` (conway's own `initialize/1`/`tool/1`/... wire) each
//! hand-rolled an IDENTICAL shape: spawn a long-lived child with
//! `process_group(0)` + `kill_on_drop(true)`, a pending table keyed by a
//! numeric `id` field, a long-lived reader task that frames stdout
//! line-by-line and routes each line by `id` (or, for a no-`id` line, some
//! notification handling), a write-then-await round trip bounded by a
//! per-call deadline on BOTH the write and the read, a graceful
//! SIGTERM-then-SIGKILL kill on that deadline elapsing
//! ([`super::unix::kill_group`]), and a synchronous `Drop`-time SIGKILL
//! (`Drop` cannot `await` the graceful sequence). Four failure causes are
//! shared verbatim: the child could not be spawned at all, a round trip's
//! per-call deadline elapsed, the child died mid-session (closed stdout,
//! exited, or a stdin write failed), or the child wrote a frame this host
//! could not parse. This module IS that shared machinery, extracted once
//! rather than hand-rolled twice.
//!
//! **What stays OUTSIDE this module, deliberately (INTENT §8.10: "similar
//! is not duplicate").** Everything specific to one wire dialect: MCP's
//! `initialize`/`notifications/initialized`/`tools/list`/`tools/call`
//! request shapes and JSON-RPC-`error` handling
//! (`conway-plugin-mcp::session`), and conway's own `initialize/1`/
//! `permission.policy/1`/`observe/1`/`status.declare/1`/`tool/1` request
//! shapes, its version-negotiation table, and its per-point
//! participant-vs-observer refuse/degrade rules
//! (`conway-plugin-subprocess::session`). Those are genuinely different
//! protocols, for different external reasons -- only the process-lifecycle
//! mechanics underneath both, a SAFETY property (fail-closed on child
//! death/timeout/malformed frame), had one meaning written down twice.
//!
//! **Each crate's own public error enum is UNCHANGED.** `McpPluginError`
//! and `SubprocessPluginError` keep their own variants and their own
//! `thiserror` `Display` text -- callers see no difference. Each crate maps
//! this module's four shared causes onto its own error type by implementing
//! [`ChildSessionError`], a one-line-per-variant translation
//! (`crate-local-variant { config_id: config_id.to_string(), .. }`). A
//! divergence this module does NOT paper over: [`ChildSessionError`]'s
//! `Display`-facing `detail` text for a write/flush failure is now a
//! single, generic ("write to child stdin failed") string shared by both
//! crates, where each previously spelled a crate-specific noun ("MCP
//! server"/"plugin"). Neither crate's test suite asserts on that inner
//! `detail` text (only on the OUTER `thiserror` message, e.g. `"session
//! died"`/`"timed out"`/`"malformed frame"`, which come from each crate's
//! OWN unchanged enum and are unaffected) -- see this item's own completion
//! report for the full divergence list.
//!
//! **Reached through the facade, not this crate directly** -- the SAME
//! route [`super::unix::kill_group`] and `conway::plugin::
//! DEFAULT_TIMEOUT_MS` already travel: re-exported as
//! `conway::plugin::{ChildSession, ChildSessionError, NotificationRoute,
//! PendingGuard}`, gated `cfg(all(unix, feature = "builtin-tools"))` because
//! this module calls [`super::unix::kill_group`] directly, the exact gate
//! `kill_group`'s own re-export already carries.
//!
//! **Patience before the kill, on the READ side only (board item
//! `01M1YQ3MJQSCQTMVAZ3GCSTB8P`).** The pre-existing SAFETY property above
//! (fail-closed on timeout) is unchanged in OUTCOME; what changed is WHEN it
//! fires. [`ChildSession::await_response`] no longer kills the instant a
//! round trip's read deadline elapses -- it warns and waits again, for the
//! SAME pending response (never a resend), up to a small, fixed, documented
//! multiple of that deadline (`GRACE_CEILING_FACTOR`, this module's own
//! private constant) before giving up
//! exactly as before. The FIRST ordinary round trip after spawn additionally
//! gets a caller-chosen, larger base deadline
//! ([`ChildSession::next_round_trip_timeout_ms`]) -- a one-time warm-up cost
//! (opening a database, priming a cache) is common on a freshly-spawned
//! child's first real request and should not be punished at the same
//! trigger-happy rate as an ordinary call. Both mechanisms live ONCE, here,
//! so every consumer inherits them without restating the flag or the
//! kill-on-elapse bookkeeping -- safety-critical resolution logic has one
//! implementation, never a per-crate copy. See this item's own completion
//! report for the full argument and the incident it answers. Deliberately
//! NOT extended to the WRITE deadline (`ChildSession`'s own private
//! `write_locked`): a child that has stopped draining its own stdin is a
//! different, more unambiguous failure shape than one merely slow to
//! answer, and the acceptance criteria this item shipped against are all
//! read-side.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tokio::time::timeout;

use super::spawn_retry::spawn_with_retry;
use super::unix::kill_group;

/// The multiple of a round trip's stated deadline (`timeout_ms` for an
/// ordinary call, [`ChildSession`]'s own `first_call_timeout_ms` for the
/// first) this host waits, IN TOTAL, before concluding a late-but-still-
/// running process is genuinely unresponsive and killing it -- board item
/// `01M1YQ3MJQSCQTMVAZ3GCSTB8P`, extending this module's own timeout-then-
/// kill sequence with ONE bounded grace extension rather than killing on
/// the first millisecond of overrun.
///
/// **The incident this answers, 2026-09-07.** A dogfooding session's
/// `ideate` MCP plugin lost its session on the FIRST real tool call:
/// `cargo build` competing for CPU (host load average 6/11/12) pushed one
/// ordinarily 5-second-budgeted call a little past its deadline, and the
/// old code killed the whole session on that first millisecond of overrun.
/// An isolated repro of the IDENTICAL call against IDENTICAL data answered
/// in 0.05s once the machine was not contended -- the DATA was never slow,
/// the SCHEDULER was, for a handful of seconds. A flat cutoff has no room
/// for that; this factor is the room.
///
/// **One extension, not a retry loop, and the SAME pending answer.**
/// [`ChildSession::await_response`] waits `base`; if that elapses with no
/// answer and the session has not died for some other reason, it emits a
/// `tracing::warn!` naming the plugin and waits again -- for the SAME
/// outstanding request, via the SAME oneshot receiver -- up to a TOTAL of
/// `base * GRACE_CEILING_FACTOR`. The original request is never resent: a
/// resend of a side-effecting call (e.g. `work_claim`) could double it,
/// which this item states as a hard "never." If the answer lands inside the
/// extension, the call succeeds, late, with only the warning to show for
/// it. If the full ceiling still elapses, this host gives up exactly as
/// before -- [`ChildSession::kill_group_now`], then
/// [`ChildSessionError::timed_out`] -- so the fail-closed OUTCOME on a
/// genuinely hung or malicious server is unchanged; only WHEN it fires
/// moved, and only by this fixed, bounded amount.
///
/// **A multiple of the deadline, not a fixed number of added seconds -- and
/// never a function of observed load.** A 5s ordinary call and a 20s
/// first-call budget (`conway-plugin-mcp::DEFAULT_FIRST_CALL_TIMEOUT_MS`)
/// are asking about scheduler contention at very different scales;
/// multiplying keeps the grace proportionate to what "a little late" means
/// for THAT call. This host does not measure load and must not chase it --
/// a load-proportional grace would make the worst-case wait unbounded
/// exactly when the fail-closed guarantee matters most. The factor below is
/// fixed and applies identically whether the host is idle or thrashing.
///
/// **3, not 2 or 10.** 2x (doubling) leaves only one more base deadline of
/// headroom -- for the default 5s per-call budget, 5s more, which the live
/// incident's own load spike plausibly could have exceeded (how long that
/// particular spike lasted was never measured, only that it pushed one call
/// past 5s). 10x turns "a little late" into "close enough to unbounded" for
/// a caller already holding a generous deadline (a 20s first-call budget
/// would then tolerate over three minutes of silence). 3x -- roughly 10s of
/// added patience on the default 5s per-call deadline, roughly 40s on the
/// 20s first-call default -- absorbs a real scheduling burst without
/// approaching "no different from no deadline at all."
const GRACE_CEILING_FACTOR: u64 = 3;

/// One host-level lifecycle failure a [`ChildSession`] can report -- the
/// four causes shared verbatim by both consumer crates today ("Spawn /
/// TimedOut / SessionDied / MalformedFrame", the taxonomy this item's own
/// spec names). NOT itself the public error type either crate exposes: a
/// consumer implements this trait on its OWN `#[non_exhaustive] thiserror`
/// enum, mapping each cause onto its own like-named variant (same fields,
/// same `Display` text), so that crate's old public error surface is
/// unchanged. `config_id` is passed to every method rather than read from
/// `self` so a [`ChildSession`] can construct one of these BEFORE it exists
/// (a spawn failure has no session yet).
pub trait ChildSessionError: Clone + Send + Sync + std::fmt::Debug + 'static {
    /// The configured command could not even be spawned: not found, not
    /// executable, an empty command, or any other OS-level spawn failure.
    fn spawn(config_id: &str, detail: String) -> Self;
    /// A round trip's per-call deadline elapsed and the process group was
    /// killed (graceful SIGTERM-then-SIGKILL, [`super::unix::kill_group`]).
    fn timed_out(config_id: &str, after_ms: u64) -> Self;
    /// The child died mid-session: it exited, closed its stdout, or a write
    /// to its stdin failed (a broken pipe -- the child is gone).
    fn session_died(config_id: &str, detail: String) -> Self;
    /// The child wrote a line this host could not parse as a framed
    /// response: not valid JSON, or an empty line.
    fn malformed_frame(config_id: &str, detail: String) -> Self;
}

/// How a [`ChildSession`]'s reader task handles an inbound stdout line that
/// carries no correlation `id` -- a one-way, server/plugin-initiated
/// notification. The two routes in use today, both preserved verbatim from
/// the two consumer crates' own pre-existing, independently-decided
/// behavior (a real divergence this extraction keeps, not collapses -- see
/// this module's own doc and this item's completion report).
pub enum NotificationRoute {
    /// Drop the line with a `tracing::warn!` -- `conway-plugin-mcp`'s rule
    /// (an MCP server-initiated notification is out of scope for this
    /// client). Observer-class: never tears down the session.
    WarnAndDrop,
    /// Forward the line, via a non-blocking `try_send`, onto the given
    /// channel -- `conway-plugin-subprocess`'s rule (a no-`id` line may be a
    /// `status/1` push, or another future notification point; the caller's
    /// own handler task drains and classifies it). `Full` drops the line
    /// with a `tracing::warn!` (lossy-with-notice); `Closed` drops it
    /// silently (the caller's handler already ended, e.g. session drop).
    /// Observer-class: never tears down the session.
    Forward(mpsc::Sender<serde_json::Value>),
}

struct Shared<E: ChildSessionError> {
    /// Outstanding-request table: `id` -> the `oneshot` sender a waiting
    /// round trip is awaiting. Inserted by [`ChildSession::send_request`]
    /// before it writes the request line; removed by the reader task (on a
    /// routed response), by a [`PendingGuard`] whose `Drop` runs on EVERY
    /// exit from `send_request`'s caller (success, timeout, write-failure,
    /// AND a future dropped mid-await -- a caller racing the read against
    /// its own cancellation), or by `Shared::kill_all` (on session death,
    /// which drops every pending sender so its waiter sees `Err` and reads
    /// the death reason from `Shared::death`). Idempotent removal -- a
    /// no-op if the reader already routed the entry or `kill_all` already
    /// cleared the table.
    pending: Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>,
    /// Set when the session is torn down (child died, malformed frame, or
    /// explicit kill on timeout). Once true, every subsequent round trip
    /// fails fast -- no re-spawn.
    dead: AtomicBool,
    /// The typed reason the session died, set by `kill_all` under the same
    /// lock that drains `pending`. Read by a round trip whose sender was
    /// dropped (it got `Err` from `rx`) so it can surface the REAL failure
    /// mode rather than a generic "session died".
    death: Mutex<Option<E>>,
}

impl<E: ChildSessionError> Shared<E> {
    fn kill_all(&self, reason: E) {
        self.dead.store(true, Ordering::Release);
        let mut death = self.death.lock().expect("death lock poisoned");
        if death.is_none() {
            *death = Some(reason);
        }
        let mut pending = self.pending.lock().expect("pending table poisoned");
        // Dropping each sender (rather than sending a placeholder) makes the
        // waiter's `rx` resolve to `Err` -- the signal that the session died
        // and its reason is in `self.death`.
        pending.clear();
    }

    fn remove_pending(&self, id: u64) {
        let mut pending = self.pending.lock().expect("pending table poisoned");
        pending.remove(&id);
    }
}

/// RAII removal of a [`ChildSession::send_request`] pending-table entry.
/// `Drop` runs on EVERY exit from a round trip -- the ordinary paths
/// (success, timeout, write-failure) AND a caller racing the read against
/// its OWN cancellation, which drops this guard mid-await. Idempotent via
/// `Shared::remove_pending`: a no-op on the success path, where the reader
/// already removed the entry.
pub struct PendingGuard<E: ChildSessionError> {
    shared: Arc<Shared<E>>,
    id: u64,
}

impl<E: ChildSessionError> Drop for PendingGuard<E> {
    fn drop(&mut self) {
        self.shared.remove_pending(self.id);
    }
}

/// A long-lived handle to one child process: owns the `tokio::process::
/// Child`, its stdin write half, and a long-lived reader task that frames
/// NDJSON lines off stdout and routes them by a numeric `id` field to the
/// waiting round trip. Built by [`ChildSession::spawn`]; NOT `Clone` -- a
/// consumer crate wraps this in an `Arc` the same way its own session type
/// already did before this extraction.
pub struct ChildSession<E: ChildSessionError> {
    config_id: String,
    pgid: i32,
    timeout_ms: u64,
    /// The deadline the FIRST ordinary round trip (through
    /// [`Self::framed_round_trip`]) gets instead of `timeout_ms` -- see
    /// [`Self::next_round_trip_timeout_ms`]'s own doc. A caller with no
    /// warm-up concept of its own (today, `conway-plugin-subprocess`) passes
    /// the SAME value as `timeout_ms` here at [`Self::spawn`], which makes
    /// this field a no-op elevation -- the flag still advances, but to a
    /// value indistinguishable from the ordinary deadline, so that caller's
    /// behavior is byte-for-byte unchanged while still inheriting the
    /// grace-on-elapse mechanism ([`GRACE_CEILING_FACTOR`]) uniformly.
    first_call_timeout_ms: u64,
    /// Set `true` the first time [`Self::next_round_trip_timeout_ms`] is
    /// read, so at most ONE ordinary round trip ever sees
    /// `first_call_timeout_ms` instead of `timeout_ms`.
    first_call_taken: AtomicBool,
    /// The child, held for the session's lifetime and killed on drop.
    /// Behind an async mutex so the timeout path can `kill_group` it while
    /// a write may be in flight.
    child: AsyncMutex<Option<Child>>,
    /// The child's stdin write half, shared between every round trip.
    /// Behind an `Arc<AsyncMutex>` so writes serialize -- two concurrent
    /// calls never corrupt each other's framing.
    stdin: Arc<AsyncMutex<ChildStdin>>,
    next_id: AtomicU64,
    shared: Arc<Shared<E>>,
    /// Kept (never awaited) so the tasks are not leaked: they end on
    /// stdout/stderr EOF or when the session is killed.
    _reader_handle: tokio::task::JoinHandle<()>,
    _stderr_handle: tokio::task::JoinHandle<()>,
}

impl<E: ChildSessionError> ChildSession<E> {
    /// Spawns `command` (argv-shaped: program, then its arguments) once,
    /// with `env` applied IN ADDITION to the parent process's own env, wires
    /// stdin/stdout/stderr, and starts the long-lived reader + stderr-drain
    /// tasks. `notify` selects how the reader routes an inbound no-`id`
    /// line (see [`NotificationRoute`]). Returns a handle whose child lives
    /// until it is dropped or a fatal error kills it.
    ///
    /// `first_call_timeout_ms` is the deadline [`Self::next_round_trip_timeout_ms`]
    /// hands the FIRST ordinary round trip after this call returns, instead
    /// of `timeout_ms` -- pass `timeout_ms` itself here for a caller with no
    /// warm-up concept of its own (a no-op elevation; see
    /// [`ChildSession`]'s own field doc).
    pub async fn spawn(
        config_id: &str,
        command: &[String],
        env: &[(String, String)],
        timeout_ms: u64,
        first_call_timeout_ms: u64,
        notify: NotificationRoute,
    ) -> Result<Self, E> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| E::spawn(config_id, "plugin command is empty".into()))?;

        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Belt-and-suspenders for the `Drop`-time group kill: even if
            // that `kill(-pgid)` is beaten to it, the leader dies when the
            // `Child` handle drops.
            .kill_on_drop(true)
            .process_group(0);
        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = spawn_with_retry(|| cmd.spawn())
            .await
            .map_err(|err| E::spawn(config_id, format!("failed to spawn '{program}': {err}")))?;

        let pgid = child.id().ok_or_else(|| {
            E::spawn(
                config_id,
                "spawned process exited before its pid could be read".into(),
            )
        })? as i32;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            dead: AtomicBool::new(false),
            death: Mutex::new(None),
        });

        // The long-lived NDJSON reader: reads stdout line-by-line and
        // routes each line to the waiting round trip by its `id`. A
        // SEPARATE task from stderr and from `child.wait()` -- no
        // `tokio::join!` here joins `child.wait()` against piped stdio (the
        // four-way-join starvation hazard both consumer crates' own module
        // docs disclose). The reader ends on stdout EOF or a malformed
        // frame.
        let reader_shared = shared.clone();
        let reader_config_id = config_id.to_string();
        let reader_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        // EOF with nothing read: a clean session end. A
                        // trailing partial line with no terminating `\n` is
                        // returned as `Ok(n > 0)`, NOT `Ok(0)`, so it is
                        // handled in the `Ok(_)` arm below (JSON parse
                        // failure -> `MalformedFrame`).
                        reader_shared.kill_all(E::session_died(
                            &reader_config_id,
                            "closed stdout (EOF) mid-session".into(),
                        ));
                        return;
                    }
                    Ok(_) => {
                        let bytes = line.trim_end().as_bytes();
                        if bytes.is_empty() {
                            reader_shared.kill_all(E::malformed_frame(
                                &reader_config_id,
                                "wrote an empty line to stdout".into(),
                            ));
                            return;
                        }
                        let value: serde_json::Value = match serde_json::from_slice(bytes) {
                            Ok(v) => v,
                            Err(err) => {
                                reader_shared.kill_all(E::malformed_frame(
                                    &reader_config_id,
                                    format!("wrote a line that is not valid JSON: {err}"),
                                ));
                                return;
                            }
                        };
                        let id = value.get("id").and_then(|v| v.as_u64());
                        let id = match id {
                            Some(id) => id,
                            None => {
                                // No correlation `id`: an inbound one-way
                                // notification. NEVER `kill_all` -- a
                                // notification is observer-class, not a
                                // malformed frame.
                                match &notify {
                                    NotificationRoute::WarnAndDrop => {
                                        tracing::warn!(
                                            config_id = %reader_config_id,
                                            "dropping an inbound line with no id (observer-class: \
                                             a server/plugin-initiated notification does not tear \
                                             down the session)"
                                        );
                                    }
                                    NotificationRoute::Forward(tx) => match tx.try_send(value) {
                                        Ok(()) => {}
                                        Err(mpsc::error::TrySendError::Full(_)) => {
                                            tracing::warn!(
                                                config_id = %reader_config_id,
                                                "inbound notification channel full; dropping a \
                                                 no-id line (lossy-with-notice: a flooding child \
                                                 must not stall the host turn)"
                                            );
                                        }
                                        Err(mpsc::error::TrySendError::Closed(_)) => {
                                            // The caller's own handler task
                                            // is gone (session dropping) --
                                            // drop silently and keep reading
                                            // until EOF.
                                        }
                                    },
                                }
                                continue;
                            }
                        };
                        let tx = {
                            let mut pending =
                                reader_shared.pending.lock().expect("pending poisoned");
                            pending.remove(&id)
                        };
                        if let Some(tx) = tx {
                            let _ = tx.send(value);
                        }
                        // `None`: no outstanding request for this id
                        // (duplicate, or a late response after timeout) --
                        // drop it, the call already failed closed via its
                        // own timeout path.
                    }
                    Err(err) => {
                        reader_shared.kill_all(E::session_died(
                            &reader_config_id,
                            format!("stdout read failed: {err}"),
                        ));
                        return;
                    }
                }
            }
        });

        // The concurrent stderr drain: discards stderr to EOF so a child
        // that writes to stderr with nobody reading it cannot block. Drained
        // bytes are DISCARDED -- neither consumer crate wires a log/event
        // sink for a child's own diagnostic output.
        let stderr_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut reader, &mut sink).await;
        });

        Ok(Self {
            config_id: config_id.to_string(),
            pgid,
            timeout_ms,
            first_call_timeout_ms,
            first_call_taken: AtomicBool::new(false),
            child: AsyncMutex::new(Some(child)),
            stdin: Arc::new(AsyncMutex::new(stdin)),
            next_id: AtomicU64::new(1),
            shared,
            _reader_handle: reader_handle,
            _stderr_handle: stderr_handle,
        })
    }

    /// This session's configured plugin/server id, for the caller's own
    /// error messages.
    pub fn config_id(&self) -> &str {
        &self.config_id
    }

    /// The spawned child's OS pgid (the process-group leader's own pid,
    /// since every session spawns with `process_group(0)`).
    pub fn pgid(&self) -> i32 {
        self.pgid
    }

    /// This session's configured per-call deadline, in milliseconds.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// The deadline the NEXT ordinary round trip should use, and advances
    /// the once-per-session "first call" flag atomically so AT MOST ONE
    /// round trip ever sees the warm-up budget (`first_call_timeout_ms`).
    /// [`Self::framed_round_trip`] calls this itself; a caller that builds
    /// its OWN round trip on [`Self::send_request`]/[`Self::await_response`]
    /// directly -- `conway-plugin-mcp::McpSession::tools_call`, which races
    /// the read against a cancellation token and so cannot use
    /// `framed_round_trip` unmodified -- calls this FIRST, to inherit the
    /// SAME warm-up/grace treatment without restating the flag or the
    /// bookkeeping around it -- one implementation, many callers.
    ///
    /// A caller with an EXPLICIT deadline -- the opening `initialize`
    /// handshake, via [`Self::framed_round_trip_within`] -- bypasses this
    /// entirely: it already states its own budget and does not consume the
    /// warm-up slot, so the first ORDINARY call after the handshake still
    /// gets it.
    pub fn next_round_trip_timeout_ms(&self) -> u64 {
        if !self.first_call_taken.swap(true, Ordering::AcqRel) {
            self.first_call_timeout_ms
        } else {
            self.timeout_ms
        }
    }

    /// The raw stdin handle, shared with every round trip via the SAME
    /// lock -- writes never interleave. Exposed for a caller that needs to
    /// write a one-way frame WITHOUT this session's own failure-propagation
    /// semantics ([`Self::write_frame`] kills the WHOLE session on any
    /// write failure/timeout). A caller that wants an independent, isolated
    /// failure path -- e.g. `conway-plugin-subprocess`'s own `observe/1`
    /// writer task, which degrades ITS OWN forwarding on a write failure
    /// without tearing down `tool/1` calls sharing this child -- takes the
    /// lock directly via this accessor instead, and manages its own bounded
    /// deadline and its own failure state.
    pub fn stdin_handle(&self) -> Arc<AsyncMutex<ChildStdin>> {
        self.stdin.clone()
    }

    /// The next monotonic correlation id, starting at 1.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// True once the session has been torn down. A subsequent round trip
    /// fails fast.
    pub fn is_dead(&self) -> bool {
        self.shared.dead.load(Ordering::Acquire)
    }

    /// The typed death reason, if the session is dead.
    pub fn death_error(&self) -> Option<E> {
        self.shared
            .death
            .lock()
            .expect("death lock poisoned")
            .clone()
    }

    /// Marks the session dead with `reason` and drops every pending sender,
    /// so each waiter wakes with `Err` and reads `reason` from
    /// [`Self::death_error`]. `pub` so a caller can fail closed on its OWN
    /// higher-level parse failure (e.g. a wire-specific structural check a
    /// generic [`ChildSession::framed_round_trip`] response still has to
    /// pass) without reaching into private state.
    pub fn kill_all(&self, reason: E) {
        self.shared.kill_all(reason);
    }

    /// Registers a oneshot sender for `id` in the pending table
    /// (double-checking dead under the lock so a death between the caller's
    /// own `is_dead` check and this insert still fails closed), then writes
    /// the already-serialized `\n`-terminated `json` request line under the
    /// per-call write deadline. Returns the response receiver and a
    /// [`PendingGuard`] whose `Drop` removes the pending entry on any exit.
    /// **The write runs uncancellable** -- bounded only by `timeout_ms` --
    /// so a caller racing the READ against its own cancellation can never
    /// drop this future mid-write and leave a partial (newline-less)
    /// request line in the pipe, which would corrupt the framing for every
    /// call sharing this session.
    pub async fn send_request(
        &self,
        id: u64,
        json: Vec<u8>,
    ) -> Result<(oneshot::Receiver<serde_json::Value>, PendingGuard<E>), E> {
        let (tx, rx) = oneshot::channel::<serde_json::Value>();
        {
            let mut pending = self.shared.pending.lock().expect("pending poisoned");
            if self.is_dead() {
                drop(pending);
                return Err(self.death_error().unwrap_or_else(|| {
                    E::session_died(
                        &self.config_id,
                        "session died while this call was being registered".into(),
                    )
                }));
            }
            pending.insert(id, tx);
        }

        // RAII cleanup: removes the pending entry on EVERY exit path from
        // here on -- success, per-call read timeout, write-failure, AND a
        // future dropped mid-read-await (a caller's own `select!` taking a
        // cancellation arm). Idempotent: a no-op if the reader already
        // routed the entry.
        let guard = PendingGuard {
            shared: self.shared.clone(),
            id,
        };

        self.write_locked(&json).await?;
        Ok((rx, guard))
    }

    /// The shared id-correlated round trip: [`Self::send_request`] writes
    /// the framed request (uncancellable), then this awaits the correlated
    /// response under [`Self::next_round_trip_timeout_ms`]'s deadline (the
    /// ordinary per-call budget, or the first-call warm-up budget for
    /// whichever round trip is genuinely first) with a bounded grace
    /// extension on elapse ([`Self::await_response`]) -- NO cancellation
    /// racing; a caller that needs to race the read against its own token
    /// uses [`Self::send_request`]/[`Self::next_round_trip_timeout_ms`]/
    /// [`Self::await_response`] directly (see `conway-plugin-mcp::session::
    /// McpSession::tools_call` for that shape). Fail-closed on every failure
    /// mode: dead session, write failure, per-call timeout (after grace), or
    /// the reader dropping the sender (session died mid-call) -- never a
    /// hang, never a silent retry, never a resend.
    pub async fn framed_round_trip(&self, id: u64, json: Vec<u8>) -> Result<serde_json::Value, E> {
        let timeout_ms = self.next_round_trip_timeout_ms();
        self.framed_round_trip_within(id, json, timeout_ms).await
    }

    /// [`Self::framed_round_trip`] under an EXPLICIT read deadline instead of
    /// [`Self::next_round_trip_timeout_ms`]'s own choice.
    ///
    /// Exists because neither the per-call nor the first-call deadline is the
    /// right bound for the OPENING handshake. A per-call timeout answers "how
    /// long may an already-running server take to answer one request"; the
    /// opening handshake also covers the process getting to the point where
    /// it can answer anything at all.
    ///
    /// **Found by the operator, 2026-08-30.** A Claude Code plugin is
    /// installed by cloning it with no build step and no bundled runtime, so
    /// a plugin whose server needs compiling builds itself on first launch --
    /// ideate's `bin/ideate-mcp` runs `npm install && npm run build` before
    /// exec'ing Node. Against a 5s per-call deadline that first start can
    /// never finish, and the operator sees the session die at startup with no
    /// hint that a build was underway.
    ///
    /// **Does NOT consume the first-call warm-up slot** -- an explicit
    /// deadline states its own budget, so [`Self::next_round_trip_timeout_ms`]
    /// is never called here and the first ORDINARY round trip after this one
    /// (e.g. an MCP session's first `tools/call`) still gets its own warm-up
    /// treatment.
    ///
    /// Everything else is identical and delegates to ONE implementation --
    /// safety-critical logic is written once and parameterized, never copied
    /// into a near-duplicate that can drift: same fail-closed handling of a
    /// dead session, write failure, timeout-with-grace, or a dropped sender.
    pub async fn framed_round_trip_within(
        &self,
        id: u64,
        json: Vec<u8>,
        timeout_ms: u64,
    ) -> Result<serde_json::Value, E> {
        let (rx, _guard) = self.send_request(id, json).await?;
        self.await_response(rx, timeout_ms).await
    }

    /// Awaits the correlated response for an outstanding [`Self::
    /// send_request`], patiently: waits `base_timeout_ms`, and if that
    /// elapses with no answer and the session has not died for some other
    /// reason, warns and waits again -- for the SAME pending response, via
    /// the SAME oneshot receiver, never a resend -- up to a TOTAL of
    /// `base_timeout_ms * GRACE_CEILING_FACTOR` before giving up exactly as
    /// the pre-grace code did: [`Self::kill_group_now`], then
    /// [`ChildSessionError::timed_out`]. See `GRACE_CEILING_FACTOR`'s own
    /// doc (this module's private constant) for the full argument behind
    /// the number and the incident it answers.
    ///
    /// `pub` (not the private detail [`Self::framed_round_trip_within`]
    /// alone needs) so a caller building its OWN round trip on
    /// [`Self::send_request`] can inherit the SAME grace treatment without
    /// restating it -- `conway-plugin-mcp::McpSession::tools_call` races
    /// THIS future (not a hand-rolled `timeout(..)`) against its own
    /// cancellation token in a `tokio::select!`; dropping this future
    /// mid-wait on a cancel is exactly as safe as dropping the pre-grace,
    /// ungraced wait always was -- the caller's own [`PendingGuard`] removes
    /// the pending entry either way, session state here is local to this
    /// call's own stack, and no request is ever sent twice.
    pub async fn await_response(
        &self,
        mut rx: oneshot::Receiver<serde_json::Value>,
        base_timeout_ms: u64,
    ) -> Result<serde_json::Value, E> {
        if let Some(result) = self.wait_for(&mut rx, base_timeout_ms).await {
            return result;
        }

        // The base deadline elapsed with no answer and the session did not
        // die for some other reason -- extend ONCE rather than killing on
        // the first millisecond of overrun. `grace_ms` is the ADDITIONAL
        // wait, so `base_timeout_ms + grace_ms == base_timeout_ms *
        // GRACE_CEILING_FACTOR`, the full ceiling this module's own doc (and
        // `GRACE_CEILING_FACTOR`'s) argues for.
        let grace_ms = base_timeout_ms.saturating_mul(GRACE_CEILING_FACTOR - 1);
        tracing::warn!(
            config_id = %self.config_id,
            base_timeout_ms,
            grace_ms,
            "a round trip exceeded its per-call deadline; waiting up to {grace_ms}ms longer for \
             the SAME pending response before concluding the process is unresponsive (never \
             resent -- a resend could double a side-effecting call)"
        );

        if let Some(result) = self.wait_for(&mut rx, grace_ms).await {
            return result;
        }

        // The full ceiling elapsed: give up exactly as the pre-grace code
        // did -- kill the group, mark the session dead, report TimedOut
        // against the FULL ceiling actually waited (not just the base),
        // since that is how long this host really tolerated the call.
        self.kill_group_now().await;
        Err(E::timed_out(
            &self.config_id,
            base_timeout_ms.saturating_add(grace_ms),
        ))
    }

    /// Waits up to `ms` for `rx` to resolve. `None` means the deadline
    /// elapsed with NEITHER a response NOR the session dying -- the caller's
    /// (i.e. [`Self::await_response`]'s) cue to extend or give up. `Some` is
    /// a TERMINAL outcome: either the correlated response, or the typed
    /// death reason if the reader dropped the sender while this waited (the
    /// session died for an UNRELATED reason -- a malformed frame from
    /// another in-flight call, say -- while this call was still patiently
    /// waiting for its own answer).
    async fn wait_for(
        &self,
        rx: &mut oneshot::Receiver<serde_json::Value>,
        ms: u64,
    ) -> Option<Result<serde_json::Value, E>> {
        match timeout(Duration::from_millis(ms), rx).await {
            Ok(Ok(value)) => Some(Ok(value)),
            Ok(Err(_canceled)) => Some(Err(self.death_error().unwrap_or_else(|| {
                E::session_died(
                    &self.config_id,
                    "the session died before it answered this call".into(),
                )
            }))),
            Err(_elapsed) => None,
        }
    }

    /// Writes a one-way frame (no `id`, no reply awaited) under the shared
    /// stdin lock, bounded by the per-call write deadline. Fail-closed on a
    /// write failure/timeout (the session dies -- a child that cannot
    /// accept a notification line is not going to answer a request
    /// either).
    pub async fn write_frame(&self, json: Vec<u8>) -> Result<(), E> {
        self.write_locked(&json).await
    }

    async fn write_locked(&self, json: &[u8]) -> Result<(), E> {
        match timeout(Duration::from_millis(self.timeout_ms), async {
            let mut stdin = self.stdin.lock().await;
            if let Err(err) = stdin.write_all(json).await {
                return Err(format!("write to child stdin failed: {err}"));
            }
            if let Err(err) = stdin.flush().await {
                return Err(format!("flush of child stdin failed: {err}"));
            }
            Ok::<(), String>(())
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(detail)) => {
                let err = E::session_died(&self.config_id, detail);
                self.shared.kill_all(err.clone());
                Err(err)
            }
            Err(_elapsed) => {
                // The write did not complete within the deadline: the child
                // is not draining stdin. Kill the group and report
                // TimedOut.
                self.kill_group_now().await;
                Err(E::timed_out(&self.config_id, self.timeout_ms))
            }
        }
    }

    /// Kills the process group with the graceful SIGTERM-then-SIGKILL
    /// sequence ([`super::unix::kill_group`], the one shared implementation
    /// -- board item `01M0EKVR1BEXXS75NV2JC4HZZ9`) and marks the session
    /// dead. `kill_group` reaps the child itself, so no separate reap is
    /// needed here.
    pub async fn kill_group_now(&self) {
        self.shared.dead.store(true, Ordering::Release);
        let mut child_guard = self.child.lock().await;
        if let Some(child) = child_guard.as_mut() {
            kill_group(child, self.pgid).await;
        }
    }
}

impl<E: ChildSessionError> Drop for ChildSession<E> {
    fn drop(&mut self) {
        // `Drop` cannot `await` the graceful `kill_group`, so the
        // process-group SIGKILL is sent synchronously here (best-effort --
        // `kill_on_drop(true)` on the `Command` is the belt-and-suspenders
        // that kills the leader even if this `kill` is beaten to it). A
        // long-lived child is never orphaned: either this SIGKILL reaches
        // the group, or `kill_on_drop` reaches the leader.
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        let _ = kill(Pid::from_raw(-self.pgid), Signal::SIGKILL);
        // `child` (still present unless a timeout already reaped it) is
        // dropped here; `kill_on_drop(true)` ensures the leader is killed.
        // The reader/stderr tasks end on the resulting stdout/stderr EOF.
    }
}
