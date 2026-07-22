//! `conway sessions {list,show,tree,export}` (WI-111: flag surface + stub;
//! WI-116 fills in the real formatters).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use conway::Conway;

use crate::exit::ExitCode;

#[derive(Args, Debug)]
pub struct SessionsArgs {
    #[command(subcommand)]
    pub action: SessionsAction,
}

#[derive(Subcommand, Debug)]
pub enum SessionsAction {
    /// List known sessions.
    List {
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one session's resolved transcript.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Print a session's fork tree.
    Tree { id: String },
    /// Export a session's ancestry-resolved transcript as JSONL.
    Export {
        id: String,
        #[arg(long = "out")]
        out: Option<PathBuf>,
    },
}

/// Stub entry point (WI-111). Every real formatter is WI-116's scope; this
/// only establishes that the subcommand dispatches, writes nothing to
/// stdout, and reports the same "not implemented" contract every other stub
/// in this crate does.
pub async fn run(_args: &SessionsArgs, _conway: &Conway) -> conway::Result<ExitCode> {
    crate::diag::error("sessions: not implemented");
    Ok(ExitCode::Usage)
}
