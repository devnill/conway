//! `-p`/`--print` one-shot mode (WI-111: stub; WI-112 implements the real
//! streaming renderer/gate/SIGINT path; WI-117 adds session continuity).

use conway::Conway;

use crate::cli::Cli;
use crate::exit::ExitCode;

/// Stub entry point (WI-111). See `commands::sessions::run`'s doc comment
/// for the "not implemented" contract every stub in this crate shares.
pub async fn run(_cli: &Cli, _conway: Conway) -> conway::Result<ExitCode> {
    crate::diag::error("one-shot mode: not implemented");
    Ok(ExitCode::Usage)
}
