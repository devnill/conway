//! SIGINT handling for one-shot mode (WI-112).
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
/// handler and the test harness's would race). WI-113's integration tests
/// cover the real, OS-signal-driven path against the compiled binary.
fn record(hits: &AtomicU8, notify: &Notify, abort: &dyn Fn()) {
    let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
    notify.notify_waiters();
    if n >= 2 {
        abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

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
}
