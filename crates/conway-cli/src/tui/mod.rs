//! Interactive TUI (WI-111: stub; WI-114 implements the ratatui shell;
//! WI-115 adds slash commands).

use conway::Conway;

use crate::cli::Cli;
use crate::exit::ExitCode;

/// Stub entry point (WI-111). See `commands::sessions::run`'s doc comment
/// for the "not implemented" contract every stub in this crate shares.
pub async fn run(_cli: &Cli, _conway: Conway) -> conway::Result<ExitCode> {
    crate::diag::error("interactive mode: not implemented");
    Ok(ExitCode::Usage)
}
