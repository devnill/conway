//! Interactive TUI (WI-111: stub; WI-114 implements the ratatui shell here;
//! WI-115 adds slash commands on top of `app.rs`).
//!
//! Terminal lifecycle: [`run`] enters raw mode + the alternate screen,
//! installs a panic hook that restores the terminal before re-raising (so a
//! panic mid-session never leaves the user's shell in raw mode), and
//! restores the terminal on every other exit path too (`Ok`, `Err`, or a
//! failure constructing the session).

pub mod app;
pub mod gate;
pub mod input;
pub mod state;
pub mod view;

use conway::{Conway, ConwayError};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

use crate::cli::Cli;
use crate::exit::ExitCode;

use app::App;
use gate::GateReceiver;

/// Entry point (WI-114). Deviates from the module notes' literal two-argument
/// `tui::run(cli, conway)` form by one argument -- see this crate's
/// `main.rs`, `build_conway`'s doc comment, and this item's own Self-Check
/// for why: `conway_runtime::runtime::Runtime` bakes its `PermissionGate` in
/// at construction, with no later swap point, so the interactive permission
/// gate this item is responsible for wiring (`gate::TuiGate`, CARRIED
/// F-100-1) must be constructed and handed to `ConwayBuilder::with_permission_gate`
/// *before* `conway: Conway` (this function's second argument) is built --
/// i.e. in `main.rs`, ahead of this call. This function receives the
/// matching [`GateReceiver`] half of that same channel as its third
/// argument so the app loop can service the requests the already-built
/// `Conway`'s live `Runtime` sends into it.
pub async fn run(cli: &Cli, conway: Conway, gate_rx: GateReceiver) -> conway::Result<ExitCode> {
    install_panic_hook(restore_terminal);

    enable_raw_mode().map_err(ConwayError::Io)?;
    if let Err(e) = execute!(std::io::stdout(), EnterAlternateScreen) {
        restore_terminal();
        return Err(ConwayError::Io(e));
    }

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(e) => {
            restore_terminal();
            return Err(ConwayError::Io(e));
        }
    };

    let app = match App::new(cli, &conway).await {
        Ok(app) => app,
        Err(e) => {
            restore_terminal();
            return Err(e);
        }
    };

    let result = app.run(&mut terminal, gate_rx).await;
    restore_terminal();
    result
}

/// Disables raw mode and leaves the alternate screen. Idempotent-ish (each
/// step is independently best-effort: a failure disabling raw mode must not
/// prevent trying to leave the alternate screen too) and infallible from the
/// caller's perspective -- called from both normal exit paths and the panic
/// hook, neither of which can usefully propagate a further error.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
}

/// Installs a panic hook that calls `restore` before delegating to whatever
/// hook was previously installed (so panic output/backtraces still print
/// normally, just after the terminal is sane again). Exposed as a free
/// function (rather than inlined into [`run`]) so it can be unit-tested
/// without a real terminal: a test-only `restore` closure records that it
/// ran instead of touching the terminal.
pub fn install_panic_hook<F>(restore: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    // Serializes tests that touch the process-global panic hook -- only
    // this module's own test does today, but the lock keeps that true if a
    // second one is ever added.
    static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn install_panic_hook_runs_restore_before_re_raising() {
        let _guard = PANIC_HOOK_LOCK.lock().unwrap();
        let original = std::panic::take_hook();

        // A silent hook first, so `install_panic_hook`'s delegation to
        // "whatever was previously installed" doesn't print a noisy
        // backtrace to stderr for this deliberately-triggered panic.
        std::panic::set_hook(Box::new(|_info| {}));

        let restored = Arc::new(AtomicBool::new(false));
        let restored_writer = restored.clone();
        install_panic_hook(move || restored_writer.store(true, Ordering::SeqCst));

        let result = std::panic::catch_unwind(|| panic!("tui panic-hook test"));
        assert!(
            result.is_err(),
            "the panic must still propagate to catch_unwind"
        );
        assert!(
            restored.load(Ordering::SeqCst),
            "the restore closure must run when a panic occurs"
        );

        std::panic::set_hook(original);
    }
}
