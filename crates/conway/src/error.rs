//! The crate-level error type: every fallible `conway` public API returns
//! [`Result<T>`], an alias for `Result<T, ConwayError>`.

use std::path::PathBuf;

use conway_core::error::{BackendError, RoutingError, RuntimeError, StoreError};

/// The `conway` crate's umbrella error type.
///
/// `#[non_exhaustive]`: new failure modes are expected as sibling work items
/// land (config validation, agent-def loading, builder assembly), so
/// matching exhaustively on this type outside this crate is not supported.
///
/// Several variants carry both a preformatted `message` (the `Display`
/// output) and structured fields (`path`, `feature`) for callers that want
/// to inspect the failure programmatically rather than parse the message.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ConwayError {
    /// Configuration failed to load, deserialize, or validate.
    #[error("{message}")]
    Config {
        path: Option<PathBuf>,
        message: String,
    },

    /// An I/O failure not otherwise classified (config/session file access).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A backend adapter reported an error.
    #[error("{0}")]
    Backend(#[from] BackendError),

    /// The session store reported an error.
    #[error("{0}")]
    Store(#[from] StoreError),

    /// Routing could not resolve a candidate.
    #[error("{0}")]
    Routing(#[from] RoutingError),

    /// The runtime reported an error.
    #[error("{0}")]
    Runtime(#[from] RuntimeError),

    /// An agent definition file failed to load or parse.
    #[error("{message}")]
    AgentDef { path: PathBuf, message: String },

    /// `ConwayBuilder::build` could not assemble a `Conway`.
    #[error("{message}")]
    Build { message: String },

    /// Config named a backend kind whose cargo feature was not enabled at
    /// build time.
    ///
    /// `message` follows the template: `"backend kind '{kind}' requires the
    /// '{feature}' cargo feature, which was not enabled at build time"`.
    #[error("{message}")]
    UnsupportedFeature {
        feature: &'static str,
        message: String,
    },
}

/// Alias for `std::result::Result<T, ConwayError>`, exported from the crate
/// root as `conway::Result`.
pub type Result<T> = std::result::Result<T, ConwayError>;
