//! Shared `--model` parsing (WI-128) and the generic "usage error" wrapper.
//!
//! Board item 01KZGRXFSY4ZB7NCA9NS2AGFS5: `--model` used to be wired only in
//! one-shot mode (`oneshot::resolve_session`), while the interactive TUI
//! accepted the flag and silently never read it. Factored out here, rather
//! than left as a private `fn` inside `oneshot.rs`, so both `oneshot::
//! resolve_session` and `tui::app::App::session_spec` call the exact same
//! parser: a malformed `--model` value must fail the SAME way in both
//! modes, not two independently-maintained ways that could quietly drift
//! apart.

use std::str::FromStr;

use conway::{ConwayError, ModelRef};

use crate::cli::Cli;

/// Wraps `message` as a `ConwayError::Config` with no path -- every "the
/// user typed something this CLI cannot act on" failure in this crate uses
/// this, so `exit::ExitCode::from_error` classifies it as `ExitCode::Usage`
/// (2) regardless of which flag or mode produced it.
pub(crate) fn usage_error(message: impl Into<String>) -> ConwayError {
    ConwayError::Config {
        path: None,
        message: message.into(),
    }
}

/// Parses `--model <ref>` (WI-128) into a [`ModelRef`] pin, or `None` when
/// the flag was not passed. A malformed ref is a usage error (`ExitCode::
/// Usage`, 2), consistent with every other flag this crate parses.
pub(crate) fn parse_model_pin(cli: &Cli) -> conway::Result<Option<ModelRef>> {
    cli.model
        .as_deref()
        .map(|r| ModelRef::from_str(r).map_err(|e| usage_error(format!("--model {r}: {e}"))))
        .transpose()
}
