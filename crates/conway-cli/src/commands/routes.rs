//! `conway routes explain <role>` (WI-111: flag surface + stub; WI-116
//! fills in the real formatter).

use clap::{Args, Subcommand};
use conway::Conway;

use crate::exit::ExitCode;

#[derive(Args, Debug)]
pub struct RoutesArgs {
    #[command(subcommand)]
    pub action: RoutesAction,
}

#[derive(Subcommand, Debug)]
pub enum RoutesAction {
    /// Explain how `role` would be routed right now.
    Explain {
        role: String,
        #[arg(long)]
        json: bool,
    },
}

/// Stub entry point (WI-111). See `commands::sessions::run`'s doc comment
/// for the contract every stub in this crate shares.
pub async fn run(_args: &RoutesArgs, _conway: &Conway) -> conway::Result<ExitCode> {
    crate::diag::error("routes: not implemented");
    Ok(ExitCode::Usage)
}
