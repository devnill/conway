//! `conway`: the CLI binary (WI-111).
//!
//! `main` never uses `?`: every fallible step is matched explicitly and
//! converted to an [`exit::ExitCode`] via [`exit::ExitCode::from_error`], so
//! there is exactly one place (the bottom of this file) that turns an
//! `ExitCode` into a process exit status.

mod cli;
mod commands;
mod diag;
mod exit;
mod oneshot;
mod render;
mod session_ref;
mod signal;
mod tui;

use std::sync::Arc;

use clap::Parser;
use conway::{Conway, ConwayBuilder, PermissionGate};

use cli::{Cli, Command};
use exit::ExitCode;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // `Cli::parse()` (not `try_parse`) already implements the exact
    // contract the WI-111 criteria ask for: `--help`/`--version` print to
    // stdout and exit 0, every other parse error prints to stderr and
    // exits 2 -- clap's own `Error::exit()`, which this calls internally.
    let cli = Cli::parse();
    diag::set_verbosity(cli.verbose);

    if let Some(dir) = &cli.cwd {
        if let Err(e) = std::env::set_current_dir(dir) {
            diag::error(format!("cannot set cwd to {}: {e}", dir.display()));
            return to_process_code(ExitCode::Usage);
        }
    }

    // **Disclosed, WI-112/WI-114 reconciliation -- widens this function and
    // `build_conway` beyond `dispatch`'s own match arms, which is the one
    // piece of this shared file each of those items' briefs asked to leave
    // alone.** `conway_runtime::runtime::Runtime` bakes its `PermissionGate`
    // in at construction and exposes no later swap point (confirmed by
    // reading `RuntimeDeps`/`Runtime::new`), so a gate built *after*
    // `Conway` already exists -- which is what every dispatch target other
    // than `tui`/`print` still does -- can never see a live tool call.
    // `tui` and one-shot (`print`) are the two targets whose own binding
    // notes require them to supply their own gate (CARRIED F-100-1: "the
    // TUI is the layer that wires the INTERACTIVE permission prompt
    // handler"; one-shot's own notes: "-p one-shot uses an ALLOW-LIST
    // gate ... this is the layer that wires the gate for non-interactive
    // use"), so each one's gate has to be built here, before the single
    // `build_conway` call below. This is also precisely the gap
    // `ConwayBuilder::build`'s own module doc flags as missing ("No
    // `with_prompt_handler` method exists ... flagged as a gap in this
    // item's own public surface -- the CLI or a future item should likely
    // add a way to supply a prompt handler") and that
    // `tests/cli_surface.rs`'s `MINIMAL_CONFIG` comment attributes to
    // "WI-112/114" by name. Every other dispatch target's behavior here is
    // byte-for-byte unchanged (`gate_override`/`tui_gate_rx` are both
    // `None`).
    let is_tui = cli.command.is_none() && cli.print.is_none();
    let (gate_override, tui_gate_rx): (
        Option<Arc<dyn PermissionGate>>,
        Option<tui::gate::GateReceiver>,
    ) = if is_tui {
        let (gate, rx) = tui::gate::TuiGate::channel();
        (Some(Arc::new(gate)), Some(rx))
    } else if cli.command.is_none() && cli.print.is_some() {
        let gate: Arc<dyn PermissionGate> = Arc::new(oneshot::build_gate(&cli));
        (Some(gate), None)
    } else {
        (None, None)
    };

    let conway = match build_conway(&cli, gate_override) {
        Ok(conway) => conway,
        Err(e) => {
            diag::error(e.to_string());
            return to_process_code(ExitCode::from_error(&e));
        }
    };

    let result = dispatch(&cli, conway, tui_gate_rx).await;

    match result {
        Ok(code) => to_process_code(code),
        Err(e) => {
            diag::error(e.to_string());
            to_process_code(ExitCode::from_error(&e))
        }
    }
}

/// Config is loaded through `ConwayBuilder::discover()` when `--config` is
/// absent, `from_config(path)` when present (module notes). `gate`, when
/// `Some`, overrides `permissions`-derived gate selection via
/// `ConwayBuilder::with_permission_gate` -- see this file's `main` for why
/// (WI-114 reconciliation) and which dispatch targets ever pass one.
fn build_conway(cli: &Cli, gate: Option<Arc<dyn PermissionGate>>) -> conway::Result<Conway> {
    let builder = match &cli.config {
        Some(path) => ConwayBuilder::from_config(path)?,
        None => ConwayBuilder::discover()?,
    };
    let builder = match gate {
        Some(gate) => builder.with_permission_gate(gate),
        None => builder,
    };
    builder.build()
}

/// If `command.is_some()` -> `commands::{sessions,routes}::run`; else if
/// `print.is_some()` -> `oneshot::run`; else -> `tui::run` (module notes).
/// `tui_gate_rx` is `Some` exactly when the `None` (tui) arm below is the
/// one taken -- see `main`'s comment.
async fn dispatch(
    cli: &Cli,
    conway: Conway,
    tui_gate_rx: Option<tui::gate::GateReceiver>,
) -> conway::Result<ExitCode> {
    match &cli.command {
        Some(Command::Sessions(args)) => commands::sessions::run(args, &conway).await,
        Some(Command::Routes(args)) => commands::routes::run(args, &conway).await,
        None if cli.print.is_some() => oneshot::run(cli, conway).await,
        None => {
            let gate_rx = tui_gate_rx.expect("tui_gate_rx is constructed whenever is_tui is true");
            tui::run(cli, conway, gate_rx).await
        }
    }
}

fn to_process_code(code: ExitCode) -> std::process::ExitCode {
    std::process::ExitCode::from(code.code() as u8)
}
