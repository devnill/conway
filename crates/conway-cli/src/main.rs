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
mod first_party_plugins;
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
    install_tracing(cli.verbose);

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

    let conway = match build_conway(&cli, gate_override, is_tui) {
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
///
/// `--root` (board item 01KYTMH9JX21CGSE2Y6E2KP8SJ) is applied here, via
/// `ConwayBuilder::with_root`, whenever `cli.root` is `Some` -- absent, this
/// `Conway`'s root agent (and every session it starts) stays `Unconfined`,
/// exactly as before this flag existed. Resolved relative to the process's
/// OWN working directory, same as `--cwd` immediately above in `main`
/// (`std::env::set_current_dir` has already run by the time this is
/// called), NOT re-resolved against `--cwd`'s value again here -- clap
/// itself never joins two path flags together, and neither does this
/// function.
///
/// `is_tui` (bash opt-in board item: bash ships on by default and cannot be
/// declined) selects which built-in plugins `build()` registers.
/// Every non-interactive CLI target (`sessions`, `routes`, one-shot `-p`)
/// keeps this crate's pre-item behavior UNCHANGED -- every built-in,
/// `conway.shell`/bash included, is always registered
/// (`PluginSelection::All`) exactly as `ConwayBuilder::build`'s own
/// pre-item default did; one-shot's `--allowed-tools` allow-list (default:
/// empty -- see `oneshot::build_gate`) is, and always was, the thing that
/// actually keeps bash from running unattended, not registration. The
/// interactive TUI is the one CLI target that now defers to the
/// config-derived selection instead (`ConwayConfig::tools.builtin_plugins`,
/// default: every built-in except bash) -- an operator turns bash on for
/// the TUI by adding `"conway.shell"` to that `settings.json` array (see
/// `docs/interactive.md`).
fn build_conway(cli: &Cli, gate: Option<Arc<dyn PermissionGate>>, is_tui: bool) -> conway::Result<Conway> {
    let builder = match &cli.config {
        Some(path) => ConwayBuilder::from_config(path)?,
        None => ConwayBuilder::discover()?,
    };
    let builder = match gate {
        Some(gate) => builder.with_permission_gate(gate),
        None => builder,
    };
    let builder = match &cli.root {
        Some(root) => builder.with_root(root.clone()),
        None => builder,
    };
    let builder = if is_tui {
        builder
    } else {
        builder.with_builtin_plugins(conway::PluginSelection::All)
    };
    // First-party plugin tier (board item 01KZDC3JQ7W4DY1MG6MBCVB2DV):
    // `[plugins].install` names ids against the small bundle this binary
    // links (`first_party_plugins::bundle`) -- every dispatch target
    // (TUI, one-shot `-p`, `sessions`, `routes`) shares this single
    // `build_conway` choke point, so all four see the same installed set
    // from the same config, with no target-specific carve-out the way
    // `is_tui`'s built-in selection above has one.
    let wanted = builder.config().plugins.install.clone();
    let builder = first_party_plugins::install(builder, &wanted)?;
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

/// Installs a `tracing_subscriber::fmt` subscriber to stderr when `-v`/`-vv`
/// is passed (WI-121). Without this, `diag::set_verbosity` had no effect on
/// the `tracing::{trace,info,warn,error}!` calls already scattered through
/// conway-runtime/-backends/-session/conway (agent-loop failures, backend
/// health-breaker trips, probe warnings): nothing installs a subscriber, so
/// those events went nowhere and `-vv` produced no output at all.
///
/// `-v` surfaces `info` and above (routing/health notices); `-vv` (or more)
/// also surfaces `trace`. Scoped to this workspace's own crates so `-v`
/// doesn't also turn on dependency-level tracing noise (reqwest/hyper/tokio
/// emit nothing here today, but nothing stops a future dependency from
/// wiring in `tracing`). `RUST_LOG`, when set, overrides this entirely.
///
/// Writes to stderr only, via `with_writer` -- `-p` one-shot mode's stdout
/// purity contract (stdout carries only model output) must hold regardless
/// of verbosity.
fn install_tracing(verbose: u8) {
    if verbose == 0 {
        return;
    }
    let level = if verbose >= 2 { "trace" } else { "info" };
    let directive = format!(
        "warn,conway={level},conway_core={level},conway_backends={level},\
         conway_routing={level},conway_tools={level},conway_session={level},\
         conway_runtime={level},conway_cli={level}"
    );
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(directive));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn to_process_code(code: ExitCode) -> std::process::ExitCode {
    std::process::ExitCode::from(code.code() as u8)
}
