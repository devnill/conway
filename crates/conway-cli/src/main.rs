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
mod tui;

use clap::Parser;
use conway::{Conway, ConwayBuilder};

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

    let conway = match build_conway(&cli) {
        Ok(conway) => conway,
        Err(e) => {
            diag::error(e.to_string());
            return to_process_code(ExitCode::from_error(&e));
        }
    };

    let result = dispatch(&cli, conway).await;

    match result {
        Ok(code) => to_process_code(code),
        Err(e) => {
            diag::error(e.to_string());
            to_process_code(ExitCode::from_error(&e))
        }
    }
}

/// Config is loaded through `ConwayBuilder::discover()` when `--config` is
/// absent, `from_config(path)` when present (module notes).
fn build_conway(cli: &Cli) -> conway::Result<Conway> {
    let builder = match &cli.config {
        Some(path) => ConwayBuilder::from_config(path)?,
        None => ConwayBuilder::discover()?,
    };
    builder.build()
}

/// If `command.is_some()` -> `commands::{sessions,routes}::run`; else if
/// `print.is_some()` -> `oneshot::run`; else -> `tui::run` (module notes).
async fn dispatch(cli: &Cli, conway: Conway) -> conway::Result<ExitCode> {
    match &cli.command {
        Some(Command::Sessions(args)) => commands::sessions::run(args, &conway).await,
        Some(Command::Routes(args)) => commands::routes::run(args, &conway).await,
        None if cli.print.is_some() => oneshot::run(cli, conway).await,
        None => tui::run(cli, conway).await,
    }
}

fn to_process_code(code: ExitCode) -> std::process::ExitCode {
    std::process::ExitCode::from(code.code() as u8)
}
