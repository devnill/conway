//! Interactive TUI (stub; the ratatui shell is implemented here;
//! adds slash commands on top of `app.rs`).
//!
//! Terminal lifecycle: [`run`] enters raw mode + the alternate screen,
//! installs a panic hook that restores the terminal before re-raising (so a
//! panic mid-session never leaves the user's shell in raw mode), and
//! restores the terminal on every other exit path too (`Ok`, `Err`, or a
//! failure constructing the session).
//!
//! T8 also enables crossterm's bracketed-paste mode here: without it the
//! terminal never emits `Event::Paste` at all, and a pasted block arrives as
//! a flood of ordinary key events instead of one atomic paste (`app.rs`'s
//! `CEvent::Paste` arm handles the event; THIS is what makes the terminal
//! send it in the first place). Disabled in `restore_terminal` alongside
//! raw mode and the alternate screen, on every exit path.
//!
//! Terminal attention notifications also enable crossterm's focus-change
//! reporting here, for the identical reason: without `EnableFocusChange`
//! the terminal never emits `Event::FocusGained`/`FocusLost`, and
//! `AppState::terminal_focused` stays permanently `None` -- treated as
//! unfocused by `crate::tui::attention::should_notify`, so the feature
//! still works, just always as if unfocused. `app/run.rs`'s own
//! `CEvent::FocusGained`/`FocusLost` arms are what makes the terminal's
//! reports actually update that field. Disabled in `restore_terminal`
//! alongside everything else, on every exit path including the panic hook.
//!
//! ## SIGTERM/SIGHUP quit cleanly too (board item `01M3WJ7906NP3P2ZNVDK549T1Q`)
//!
//! An uncaught `SIGTERM`/`SIGHUP` (closing the terminal sends the latter; a
//! process manager stopping conway sends the former) kills a process before
//! any destructor runs -- no `kill_on_drop`, no process-group kill for an
//! in-flight `!` command, nothing. [`run`] installs the SAME
//! `signal::install_termination()` watcher `oneshot::run` already uses
//! (`crate::signal`'s own module doc, board item A5.3) and hands it to
//! [`App::run`], whose own `select!` loop reacts to the first delivery
//! of either signal through the EXACT SAME funnel every other quit path
//! already uses -- `App::purge_open_ask_modal`
//! (`app/shutdown.rs`), which kills an in-flight `!` command's whole
//! process group (bound-awaited, not fire-and-forget) and discards every
//! other kind of in-flight residue -- then returns the documented
//! `ExitCode::TerminatedBySigterm`/`TerminatedBySighup` (143/129, the same
//! codes `docs/scripting.md`'s exit-code table already documents for
//! one-shot mode; see `docs/interactive.md` for the TUI's own note). This
//! function's own unconditional `restore_terminal()` call right after
//! `app.run()` returns, unchanged, is what restores the terminal on this
//! path too -- no new restore call needed. A SECOND signal still forces an
//! immediate, unconditional `std::process::exit` from INSIDE
//! `signal::install_termination`'s own background task (mirroring
//! `oneshot::run`'s identical safety valve exactly) -- deliberately WITHOUT
//! restoring the terminal first, since that forced exit exists precisely so
//! a stuck app loop (which could easily be the very thing preventing an
//! orderly `restore_terminal()` call) cannot block getting out at all; see
//! [`install_panic_hook`]'s own doc for the identical reasoning applied to a
//! panic instead of a second signal.

mod attention;
pub mod app;
pub mod commands;
pub mod config;
pub mod form;
pub mod gate;
pub mod history;
pub mod input;
pub mod keybindings;
pub mod mentions;
pub mod model_picker;
pub mod session_picker;
pub mod state;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod usage_format;
pub mod view;

use conway::{Conway, FacadeError};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

use crate::cli::Cli;
use crate::exit::ExitCode;
use crate::signal;

use app::App;
use gate::GateReceiver;

/// This TUI's own liveness-heartbeat cadence: how often [`run`] refreshes
/// the store's cross-process liveness marker while it holds one.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// The freshness threshold [`Conway::sweep_stale_modal_asks`] is called
/// with -- 4× [`HEARTBEAT_INTERVAL`], so a few missed beats under load do
/// not flip a live owner to "stale". **Computed here, not read back off
/// `conway` (Stage 2a):** `Conway::sweep_stale_modal_asks` used to hardcode
/// this exact ratio ("4× the TUI's 15s heartbeat") as a facade constant --
/// a presentation detail (this module's own refresh cadence) baked into
/// engine configuration, the same category of defect the rest of Stage 2a
/// moves `TuiSection` and its siblings out of `conway` to fix. Only this
/// module knows its own heartbeat interval, so only this module computes
/// the threshold derived from it; `sweep_stale_modal_asks` now takes it as
/// a plain argument with no facade-side default at all.
fn sweep_live_threshold() -> chrono::Duration {
    chrono::Duration::from_std(HEARTBEAT_INTERVAL * 4)
        .expect("4x a 15s interval fits in a chrono::Duration")
}

/// Entry point. Deviates from the module notes' literal two-argument
/// `tui::run(cli, conway)` form by one argument -- see this crate's
/// `main.rs` and `build_conway`'s doc comment
/// for why: `conway_runtime::runtime::Runtime` bakes its `PermissionGate` in
/// at construction, with no later swap point, so the interactive permission
/// gate this function is responsible for wiring (`gate::TuiGate`, CARRIED
///) must be constructed and handed to `ConwayBuilder::with_permission_gate`
/// *before* `conway: Conway` (this function's second argument) is built --
/// i.e. in `main.rs`, ahead of this call. This function receives the
/// matching [`GateReceiver`] half of that same channel as its third
/// argument so the app loop can service the requests the already-built
/// `Conway`'s live `Runtime` sends into it.
///
/// `form_rx`: board item `01M19NH39AE2D5AMJK0RZRQY86`'s exact sibling of
/// `gate_rx` -- the [`form::FormReceiver`] half of a `form::TuiFormSurface`
/// channel `main.rs` builds and hands to `ConwayBuilder` (via
/// `ConwayUiPlugin::new(Some(surface))`) at the identical point `gate_rx`'s
/// own `TuiGate` is, for the identical reason (see that field's own doc
/// just above). Threaded straight into [`App::run`], never stored on `App`
/// itself, mirroring `gate_rx`'s own shape exactly.
///
/// `plugins`: the installed plugin
/// list, forwarded verbatim to [`App::new`] so it can build the plugin
/// command registry -- see that method's own doc for why this is threaded
/// as a parameter here rather than read back off the already-built `conway`
/// (no such accessor exists on `Conway`/`Runtime` today). `main.rs`'s
/// `dispatch` is this parameter's one caller, resolving it via
/// `first_party_plugins::installed_plugins`.
///
/// `agent_names`: the SAME `conway_plugin_names::AgentNames` store that
/// same caller handed the `conway.names` plugin inside `plugins` (board
/// item `01M0TV5BSE98S16SFYECG9G9WP`), parked on `AppState::agent_names`
/// immediately after [`App::new`] so `commands::resolve_agent` can accept
/// a name and `view::agents` can draw one. Set here, on the ONE production
/// call site, rather than threaded through `App::new`'s signature:
/// `App::new` has ~40 call sites in this crate's own tests, every one of
/// which would otherwise have to name a store it does not use, and none of
/// which exercises naming. When `conway.names` is not installed this is
/// the non-durable `InMemoryAgentNames` fallback -- always empty, so every
/// surface behaves exactly as it did before this parameter existed.
pub async fn run(
    cli: &Cli,
    conway: Conway,
    gate_rx: GateReceiver,
    form_rx: form::FormReceiver,
    plugins: &[std::sync::Arc<dyn conway::plugin::Plugin>],
    agent_names: std::sync::Arc<dyn conway_plugin_names::AgentNames>,
) -> conway::Result<ExitCode> {
    install_panic_hook(restore_terminal);

    // B5 crash-residue sweep, once per startup, BEFORE the session below is
    // even created (so the tree is still empty and every modal-ask leftover
    // is eligible -- see `Conway::sweep_stale_modal_asks`'s own not-live
    // caution). Purges ONLY `AskOrigin::ModalAsk`-tagged ephemeral sessions:
    // a modal ask child left behind by a crashed/killed TUI has no modal
    // that will ever show its answer, so no user will ever choose a fate
    // for it. `conway_ask` TOOL children are never touched (their
    // `EphemeralSessionRef` artifacts would dangle). Best-effort: a sweep
    // failure only leaves residue for the NEXT startup's sweep -- it must
    // never block the TUI from starting.
    //
    // S1 follow-up: the sweep consults the store's cross-process liveness
    // marker and defers entirely if ANOTHER process is actively using this
    // store (fresh heartbeat). We publish OUR OWN marker only AFTER the
    // sweep below, so a sweep never sees its own marker. The heartbeat task
    // keeps it fresh while we run; `clear_live_owner` on exit lets a
    // subsequent cold start reap immediately instead of waiting for the
    // marker to go stale.
    let _ = conway.sweep_stale_modal_asks(sweep_live_threshold()).await;

    // Board item `01M3WJ7906NP3P2ZNVDK549T1Q`: installed before raw mode is
    // even entered, so coverage starts as early as this function can give
    // it -- `oneshot::run`'s own `signal::install_termination` call site
    // carries the identical "a signal landing before anything is actually
    // `select!`-ing on `notified()` yet is a narrow, accepted gap" caveat
    // (see that function's own comment); handed to `app.run` below, whose
    // own `select!` loop is where the FIRST delivery is actually reacted to
    // (this module's own top doc).
    let termination = signal::install_termination();

    enable_raw_mode().map_err(FacadeError::Io)?;
    // T8: bracketed paste alongside the alternate screen -- one `execute!`
    // call so a mid-sequence failure still leaves `restore_terminal` (which
    // undoes both, best-effort) as the single cleanup path below. Terminal
    // attention notifications: `EnableFocusChange` rides the same call -- without it
    // the terminal never emits `Event::FocusGained`/`FocusLost` at all, which
    // is what makes `AppState::terminal_focused` track real focus instead of
    // staying permanently unknown (treated as unfocused, `crate::tui::
    // attention::should_notify`'s own doc). Disabled in `restore_terminal`
    // alongside bracketed paste/raw mode/the alternate screen, on every exit
    // path including the panic hook.
    if let Err(e) = execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableFocusChange
    ) {
        restore_terminal();
        return Err(FacadeError::Io(e));
    }

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(e) => {
            restore_terminal();
            return Err(FacadeError::Io(e));
        }
    };

    // `with_agent_names` is the one place this process's `AgentNames` store
    // reaches the TUI (this function's own doc for why it is a setter
    // rather than an `App::new` parameter). Chained onto the `Ok` arm, so
    // no `App` ever exists in a constructed-but-unwired state and no frame
    // is drawn without it.
    let app = match App::new(cli, &conway, plugins).await {
        Ok(app) => app.with_agent_names(agent_names),
        Err(e) => {
            restore_terminal();
            return Err(e);
        }
    };

    // Publish our liveness marker (after the sweep, so the sweep never saw
    // it) and start a heartbeat task so a second process starting against
    // this store sees a fresh marker and defers its sweep. `HEARTBEAT_INTERVAL`
    // (15s) << `sweep_live_threshold()` (60s, 4x it), so a few missed beats
    // under load do not flip us to "stale". No early returns after this
    // point: the cleanup below (clear marker + abort heartbeat) runs on
    // every exit from `app.run`.
    let pid = std::process::id();
    let _ = conway.heartbeat_live_owner(pid).await;
    let heartbeat_conway = conway.clone();
    let heartbeat = tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
        loop {
            interval.tick().await;
            let _ = heartbeat_conway.heartbeat_live_owner(pid).await;
        }
    });

    let result = app.run(&mut terminal, gate_rx, form_rx, &termination).await;
    restore_terminal();
    // Clean shutdown. Stop the heartbeat FIRST so an in-flight `touch` can
    // no longer rename the marker back over a just-cleared file, then drop
    // our marker so a next cold start reaps residue immediately. A
    // blocking-pool `rename` already dispatched before `abort` could in
    // theory still land — if it does, the orphaned marker goes stale (this
    // process is exiting) and the next startup's sweep reaps it within
    // `sweep_live_threshold()`; that race is the acceptable residual under
    // the S1 scope. Best-effort both: a failure here must never mask the
    // app's own result.
    heartbeat.abort();
    let _ = heartbeat.await;
    let _ = conway.clear_live_owner().await;
    result
}

/// Disables raw mode and leaves the alternate screen. Idempotent-ish (each
/// step is independently best-effort: a failure disabling raw mode must not
/// prevent trying to leave the alternate screen too) and infallible from the
/// caller's perspective -- called from both normal exit paths and the panic
/// hook, neither of which can usefully propagate a further error.
fn restore_terminal() {
    // Terminal attention notifications: disable focus-change reporting
    // before disabling bracketed paste -- reverse order of `run`'s enable,
    // and independently best-effort like every other step here (a terminal
    // left reporting focus changes after conway exits is a smaller problem
    // than one left in raw mode, but there is no reason to leave either).
    let _ = execute!(std::io::stdout(), DisableFocusChange);
    // T8: disable bracketed paste before leaving the alternate screen --
    // reverse order of `run`'s enable, and independently best-effort like
    // every other step here (a failure disabling paste mode must not skip
    // leaving the alternate screen).
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
}

/// Installs a panic hook that calls `restore` before delegating to whatever
/// hook was previously installed (so panic output/backtraces still print
/// normally, just after the terminal is sane again). Exposed as a free
/// function (rather than inlined into [`run`]) so it can be unit-tested
/// without a real terminal: a test-only `restore` closure records that it
/// ran instead of touching the terminal.
///
/// **Board item `01M3WJ7906NP3P2ZNVDK549T1Q`: this hook does NOT attempt to
/// kill an in-flight `!` command's process group, by deliberate choice.**
/// Three reasons, together:
///
/// 1. **It cannot `.await`.** `std::panic::set_hook`'s closure is plain
///    sync code, run synchronously on whichever thread panicked, possibly
///    while holding locks -- the same reason `restore` itself is a bare
///    synchronous function, never an async one. The real group-kill
///    (`conway::plugin::kill_group`, `app/shell_cmd.rs::kill_child`) sends a
///    signal AND THEN awaits the child's exit to confirm it actually died;
///    only the signal-sending half could run here at all, and even that
///    needs a live pgid this free function has no way to reach (see next).
/// 2. **A pgid to kill lives on `App`** (`shell_task`/`shell_cancel_tx`,
///    `app/shell_cmd.rs`), an ordinary value on the app loop's own stack --
///    not process-wide state this hook, which has no handle to that value,
///    could read. The only way to change that would be a SECOND, parallel
///    tracking mechanism (a global registry of live child pgids this hook
///    could poll) duplicating what `App` already owns -- a SECOND killer,
///    never a good idea when one already exists and is already correct.
/// 3. **A panic on the task driving `app.run` already triggers a real,
///    if coarser, cleanup.** `#[tokio::main]` has no `catch_unwind` around
///    the top-level future, so an uncaught panic there unwinds the whole
///    process, dropping the `tokio::Runtime` and every task it was running
///    -- including a spawned `!` command's own task, wherever it was
///    suspended. That drops its `tokio::process::Child`, whose
///    `kill_on_drop(true)` (already set, `app/shell_cmd.rs::execute`) kills
///    at least the leader process. This is the SAME disclosed gap
///    `kill_shell_command_for_quit`'s own doc already names for
///    `kill_on_drop` alone (a backgrounded grandchild outside the leader's
///    own process can still survive it) -- accepted there for the identical
///    reason: the durable backstop for a child surviving its own parent's
///    death is process-group reparenting/init reaping, not a last-ditch
///    signal handler. Closing it fully (tracking every spawned child's pgid
///    somewhere this hook could reach) is real, additive scope, flagged as
///    a follow-up, not done here.
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
