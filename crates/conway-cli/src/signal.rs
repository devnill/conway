//! SIGINT/SIGTERM/SIGHUP handling for one-shot mode.
//!
//! [`install`] spawns a background task that watches for `Ctrl-C`. The
//! **first** delivery only records itself: `oneshot::run`'s own render loop
//! reacts to it (cancelling the session, starting a grace window) because
//! only that loop knows the live `SessionHandle`. The **second** (and every
//! later) delivery aborts the process immediately and unconditionally, from
//! this background task alone -- "second Ctrl-C forces an immediate exit"
//! has to hold even if the render loop is itself stuck (e.g. blocked on a
//! backend that never responds), so it cannot depend on that loop noticing
//! anything.
//!
//! ## Board item A5.3: SIGTERM/SIGHUP get the same treatment
//!
//! [`install_termination`] (below) is [`install`]'s sibling for the two
//! other termination-class signals a one-shot process routinely receives
//! from something other than an interactive Ctrl-C: `SIGTERM` (the ordinary
//! "please stop" a process manager, `kill`, or -- the diagnosed cause of
//! the 2026-09-07 incident this item exists to close -- `conway-tools`'
//! `bash` tool's own `kill_group` (A5.5) sends to a whole process group on
//! timeout/cancellation) and `SIGHUP` (sent when a controlling terminal
//! closes on a process that was never `nohup`'d, plausible for exactly the
//! same "backgrounded `conway -p ... &`" shape).
//!
//! **Why this matters at all, when Rust installs no signal handlers by
//! default.** An uncaught `SIGTERM`/`SIGHUP` runs no process code
//! whatsoever before the OS tears the process down -- from a durability
//! standpoint indistinguishable from `SIGKILL` for anything that was never
//! written down first. `oneshot::run`'s render loop already reacts to
//! [`SigintWatch`] by cancelling the running root agent and giving it a
//! bounded grace window to publish (and persist) a real terminal result
//! through the SAME `AgentLoop::finish_cancelled` -> `finish` path every
//! other cancellation reason uses (`agent_loop.rs`) -- [`TerminationWatch`]
//! lets that identical reaction fire for `SIGTERM`/`SIGHUP` too, closing the
//! gap for a process that would otherwise die with nothing recorded at all.
//! `SIGKILL` itself remains uncatchable, on purpose -- nothing in this
//! module claims otherwise; the parent-side synthesis for that case lives
//! at the AWAITING side, not here (see `docs/agents.md`).

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

/// Tracks how many SIGINTs this process has observed since [`install`].
pub struct SigintWatch {
    hits: Arc<AtomicU8>,
    notify: Arc<Notify>,
}

impl SigintWatch {
    /// The number of SIGINTs observed so far.
    pub fn hits(&self) -> u8 {
        self.hits.load(Ordering::SeqCst)
    }

    /// Resolves the next time a SIGINT is observed. `oneshot::run`'s render
    /// loop `select!`s on this to react to the first delivery (cancel the
    /// session, start the grace window) without polling `hits()`.
    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

/// Spawns the SIGINT watcher on the current Tokio runtime. Must be called
/// from within a running runtime (`oneshot::run` always is, per `main.rs`'s
/// `#[tokio::main]`).
pub fn install() -> SigintWatch {
    let hits = Arc::new(AtomicU8::new(0));
    let notify = Arc::new(Notify::new());

    let task_hits = hits.clone();
    let task_notify = notify.clone();
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                // The signal handler could not be installed (or the
                // underlying stream ended) -- nothing more this task can
                // do; stop rather than spin.
                break;
            }
            record(&task_hits, &task_notify, &abort);
        }
    });

    SigintWatch { hits, notify }
}

/// The abort action for the second (and every later) SIGINT delivery:
/// flush stdout (defensive -- every renderer already flushes after each
/// event it writes, so nothing should be pending, but this is the last
/// chance to catch anything that is) and exit unconditionally with the
/// `Interrupted` status.
fn abort() {
    let _ = std::io::stdout().flush();
    std::process::exit(130);
}

/// One SIGINT delivery: increments `hits`, wakes every `notified()` waiter,
/// then invokes `abort` if this was the second (or later) delivery.
///
/// Shared by [`install`]'s real `ctrl_c`-driven task and this module's own
/// unit test below, which calls it directly instead of raising a real OS
/// signal -- doing that from a `#[test]` would be neither portable (no
/// `SIGINT` on Windows) nor deterministic (this process's own signal
/// handler and the test harness's would race). the integration tests
/// cover the real, OS-signal-driven path against the compiled binary.
fn record(hits: &AtomicU8, notify: &Notify, abort: &dyn Fn()) {
    let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
    notify.notify_waiters();
    if n >= 2 {
        abort();
    }
}

// ---------------------------------------------------------------------
// SIGTERM / SIGHUP (board item A5.3)
// ---------------------------------------------------------------------

/// Which termination-class signal this process observed. SIGINT has its
/// own richer two-strike handling ([`SigintWatch`], above) and is not
/// modeled here -- this covers the two OTHER signals a one-shot process
/// routinely receives from something other than an interactive Ctrl-C (see
/// this module's doc comment for the 2026-09-07 incident this closes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermSignal {
    Term,
    Hup,
}

impl TermSignal {
    /// The `ResultStatus::Cancelled` reason text [`crate::oneshot::run`]
    /// hands to the running root's `SessionHandle::cancel` -- routed
    /// through the SAME single writer (`AgentLoop::finish_cancelled` ->
    /// `finish`, `conway-runtime/src/agent_loop.rs`) every other
    /// cancellation reason already uses (this item's "single
    /// implementation" constraint: the signal handler never writes its own
    /// copy of a terminal result). `docs/agents.md` names this exact
    /// string.
    pub fn reason(self) -> &'static str {
        match self {
            TermSignal::Term => "signal: SIGTERM",
            TermSignal::Hup => "signal: SIGHUP",
        }
    }

    /// The documented process exit status for a run terminated by this
    /// signal -- the POSIX `128 + signum` convention this crate's own
    /// `ExitCode::Interrupted = 130` (128 + SIGINT's 2) already follows.
    /// See `docs/scripting.md`'s exit-code table and
    /// `exit::ExitCode::{TerminatedBySigterm,TerminatedBySighup}`.
    pub fn exit_code(self) -> i32 {
        match self {
            TermSignal::Term => 143, // 128 + SIGTERM(15)
            TermSignal::Hup => 129,  // 128 + SIGHUP(1)
        }
    }

    fn encode(self) -> u8 {
        match self {
            TermSignal::Term => 1,
            TermSignal::Hup => 2,
        }
    }

    fn decode(state: u8) -> Option<Self> {
        match state {
            1 => Some(TermSignal::Term),
            2 => Some(TermSignal::Hup),
            _ => None,
        }
    }
}

/// Tracks the FIRST termination-class signal (`SIGTERM`/`SIGHUP`) this
/// process has observed since [`install_termination`], and forces an
/// unconditional exit on a SECOND one -- mirroring [`SigintWatch`]'s
/// "second delivery forces immediate exit" safety valve, for the identical
/// reason: `oneshot::run`'s graceful reaction (cancel the root, wait a
/// bounded grace window for a real terminal result) depends on that render
/// loop noticing and reacting, and a second signal while that loop is
/// itself stuck must not depend on it noticing anything.
pub struct TerminationWatch {
    // 0 = none observed yet; otherwise `TermSignal::encode()`.
    state: Arc<AtomicU8>,
    notify: Arc<Notify>,
}

impl TerminationWatch {
    /// The first termination signal observed so far, if any.
    pub fn observed(&self) -> Option<TermSignal> {
        TermSignal::decode(self.state.load(Ordering::SeqCst))
    }

    /// Resolves the next time a termination signal is FIRST observed.
    /// Fires exactly once per process (the underlying `Notify` is woken on
    /// every delivery, but every delivery after the first is also the
    /// trigger for `record_termination`'s own unconditional exit, so no
    /// second `notified()` waiter ever actually gets to observe a change --
    /// there isn't a second live process left to observe it in).
    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

/// Spawns the SIGTERM/SIGHUP watchers on the current Tokio runtime. Must be
/// called from within a running runtime, exactly like [`install`].
///
/// Two independent `tokio::signal::unix::signal` streams, not one -- each
/// signal kind needs its own registration; there is no combined "any
/// termination signal" stream in `tokio::signal`. Both funnel into the same
/// shared `state`/`notify` pair via `record_termination`, so a caller
/// sees a single, unified [`TerminationWatch`] regardless of which of the
/// two actually fired.
#[cfg(unix)]
pub fn install_termination() -> TerminationWatch {
    use tokio::signal::unix::SignalKind;

    let state = Arc::new(AtomicU8::new(0));
    let notify = Arc::new(Notify::new());

    spawn_termination_watcher(SignalKind::terminate(), TermSignal::Term, &state, &notify);
    spawn_termination_watcher(SignalKind::hangup(), TermSignal::Hup, &state, &notify);

    TerminationWatch { state, notify }
}

/// No `SIGTERM`/`SIGHUP` concept on a non-unix target (this crate's own
/// `bash.rs`-adjacent tools already carry an identical `#[cfg(not(unix))]`
/// fallback for the same reason) -- returns a [`TerminationWatch`] whose
/// `notified()` future simply never resolves, matching "this platform never
/// delivers this signal" rather than a compile error for callers that build
/// the watch unconditionally.
#[cfg(not(unix))]
pub fn install_termination() -> TerminationWatch {
    TerminationWatch {
        state: Arc::new(AtomicU8::new(0)),
        notify: Arc::new(Notify::new()),
    }
}

#[cfg(unix)]
fn spawn_termination_watcher(
    kind: tokio::signal::unix::SignalKind,
    sig: TermSignal,
    state: &Arc<AtomicU8>,
    notify: &Arc<Notify>,
) {
    let task_state = state.clone();
    let task_notify = notify.clone();
    tokio::spawn(async move {
        let mut stream = match tokio::signal::unix::signal(kind) {
            Ok(s) => s,
            // The signal handler could not be installed -- nothing more
            // this task can do; stop rather than spin (mirrors `install`'s
            // identical give-up on a `ctrl_c()` error).
            Err(_) => return,
        };
        loop {
            if stream.recv().await.is_none() {
                break;
            }
            record_termination(&task_state, &task_notify, sig, &abort_with);
        }
    });
}

/// One termination-signal delivery. The first delivery of EITHER
/// `SIGTERM`/`SIGHUP` wins the `state` compare-exchange, records itself, and
/// wakes every `notified()` waiter -- `oneshot::run`'s render loop reacts
/// from there (cancel the root, start the grace window). Every delivery
/// AFTER that first one -- whether the same signal repeated or the other
/// termination signal arrived -- invokes `abort` unconditionally, using the
/// FIRST-observed signal's own documented exit code (not the second
/// delivery's: the code reported is "why this run is ending", decided once,
/// at the first signal).
fn record_termination(state: &AtomicU8, notify: &Notify, sig: TermSignal, abort: &dyn Fn(i32)) {
    match state.compare_exchange(0, sig.encode(), Ordering::SeqCst, Ordering::SeqCst) {
        Ok(_) => notify.notify_waiters(),
        Err(already) => {
            let first = TermSignal::decode(already).unwrap_or(sig);
            abort(first.exit_code());
        }
    }
}

/// The abort action for the second (and every later) termination-signal
/// delivery -- same shape as SIGINT's [`abort`]: flush stdout defensively,
/// then exit unconditionally with `code`.
fn abort_with(code: i32) {
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn second_delivery_invokes_the_abort_callback() {
        let hits = Arc::new(AtomicU8::new(0));
        let notify = Arc::new(Notify::new());
        let aborted = Arc::new(AtomicBool::new(false));
        let aborted_flag = aborted.clone();
        let abort_cb = move || aborted_flag.store(true, Ordering::SeqCst);

        record(&hits, &notify, &abort_cb);
        assert!(
            !aborted.load(Ordering::SeqCst),
            "must not abort on the first delivery"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        record(&hits, &notify, &abort_cb);
        assert!(
            aborted.load(Ordering::SeqCst),
            "must abort on the second delivery"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // A third delivery must abort again too -- "second and every later".
        let aborted_again = Arc::new(AtomicBool::new(false));
        let aborted_again_flag = aborted_again.clone();
        let abort_again_cb = move || aborted_again_flag.store(true, Ordering::SeqCst);
        record(&hits, &notify, &abort_again_cb);
        assert!(aborted_again.load(Ordering::SeqCst));
    }

    #[test]
    fn hits_starts_at_zero_and_notify_does_not_panic_with_no_waiters() {
        let hits = Arc::new(AtomicU8::new(0));
        let notify = Arc::new(Notify::new());
        let watch = SigintWatch { hits, notify };
        assert_eq!(watch.hits(), 0);
    }

    #[test]
    fn term_signal_exit_codes_follow_128_plus_signum() {
        assert_eq!(TermSignal::Term.exit_code(), 143); // 128 + SIGTERM(15)
        assert_eq!(TermSignal::Hup.exit_code(), 129); // 128 + SIGHUP(1)
    }

    #[test]
    fn term_signal_reasons_are_distinct_and_named() {
        assert_eq!(TermSignal::Term.reason(), "signal: SIGTERM");
        assert_eq!(TermSignal::Hup.reason(), "signal: SIGHUP");
    }

    #[test]
    fn first_termination_delivery_records_and_does_not_abort() {
        let state = Arc::new(AtomicU8::new(0));
        let notify = Arc::new(Notify::new());
        let aborted = Arc::new(Mutex::new(None));
        let aborted_cb = aborted.clone();
        let abort = move |code: i32| *aborted_cb.lock().unwrap() = Some(code);

        record_termination(&state, &notify, TermSignal::Term, &abort);

        assert!(
            aborted.lock().unwrap().is_none(),
            "must not abort on the first delivery"
        );
        assert_eq!(
            TermSignal::decode(state.load(Ordering::SeqCst)),
            Some(TermSignal::Term)
        );
    }

    /// The second delivery aborts using the FIRST-observed signal's exit
    /// code, even when the second delivery is a DIFFERENT termination
    /// signal (SIGHUP arriving after SIGTERM already started the shutdown)
    /// -- "why this run is ending" is decided once, at the first signal.
    #[test]
    fn second_termination_delivery_aborts_with_the_first_signals_code_even_if_different() {
        let state = Arc::new(AtomicU8::new(0));
        let notify = Arc::new(Notify::new());
        let aborted = Arc::new(Mutex::new(None));
        let aborted_cb = aborted.clone();
        let abort = move |code: i32| *aborted_cb.lock().unwrap() = Some(code);

        record_termination(&state, &notify, TermSignal::Term, &abort);
        assert!(aborted.lock().unwrap().is_none());

        record_termination(&state, &notify, TermSignal::Hup, &abort);
        assert_eq!(
            *aborted.lock().unwrap(),
            Some(TermSignal::Term.exit_code()),
            "the second delivery must abort with the FIRST signal's own code"
        );
    }

    #[test]
    fn termination_watch_observed_is_none_until_a_signal_is_recorded() {
        let state = Arc::new(AtomicU8::new(0));
        let notify = Arc::new(Notify::new());
        let watch = TerminationWatch { state, notify };
        assert_eq!(watch.observed(), None);
    }
}
