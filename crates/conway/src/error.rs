//! The crate-level error type: every fallible `conway` public API returns
//! [`Result<T>`], an alias for `Result<T, FacadeError>`.

use std::path::PathBuf;

use conway_core::error::{BackendError, PathStoreError, RoutingError, RuntimeError, StoreError};

/// The `conway` crate's umbrella error type.
///
/// Named `FacadeError`, not `ConwayError` (board item CON-3): `conway-core`
/// already has its own `ConwayError` (`conway_core::error::ConwayError`,
/// re-exported here as [`crate::CoreConwayError`]), a distinct, wider type
/// this crate's own `#[from]` impls draw from selectively rather than wrap.
/// The two used to share the bare name at different crate depths, which
/// meant every "ConwayError" reference — in code, in a bug report — had to
/// specify which one. `FacadeError` names this type by what it actually is:
/// the umbrella error of the embeddable *facade* (`conway`, the crate
/// `docs/embedding.md` calls "the facade"), not of the harness core.
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
pub enum FacadeError {
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

    /// The path store reported an error (`FsPathStore::open`, at
    /// `ConwayBuilder::build`'s path-store resolution step -- D1-3d-wire).
    #[error("{0}")]
    PathStore(#[from] PathStoreError),

    /// Routing could not resolve a candidate.
    #[error("{0}")]
    Routing(#[from] RoutingError),

    /// The runtime reported an error.
    #[error("{0}")]
    Runtime(#[from] RuntimeError),

    /// An agent definition file failed to load or parse.
    #[error("{message}")]
    AgentDef { path: PathBuf, message: String },

    /// A skill definition file (`.conway/skills/*/SKILL.md`) failed to load
    /// or parse. Mirrors [`FacadeError::AgentDef`]'s shape exactly —
    /// `crate::skills::load_skill_defs` is the same discovery/parse shape as
    /// `crate::agents::load_agent_defs`, just for `conway_core::config::SkillDef`.
    #[error("{message}")]
    SkillDef { path: PathBuf, message: String },

    /// `ConwayBuilder::build` could not assemble a `Conway`.
    #[error("{message}")]
    Build { message: String },

    /// A caller reached a code path whose cargo feature was not enabled at
    /// build time.
    ///
    /// Backend selection (Anthropic vs. OpenAI-compatible) never has a
    /// producer here: it is not a cargo-feature axis at all, and this crate does not even depend on either
    /// dialect's implementation crate any more -- a `[backends.<id>].kind`
    /// this build cannot resolve is `FacadeError::Config`, naming the
    /// offending value, not this variant. The sole remaining producer is
    /// `config::model_metadata::refresh`, gated on the still-genuinely-
    /// optional `metadata-refresh` feature (no HTTP client
    /// implementation exists yet).
    #[error("{message}")]
    UnsupportedFeature {
        feature: &'static str,
        message: String,
    },

    /// The ephemeral intent-classification turn (`Conway::
    /// classify_agent_intent`, C1) ended without a usable reply: the child
    /// agent reached a non-`Completed` terminal status (a backend/routing
    /// failure folded into `ResultStatus::Failed` by the runtime's agent
    /// loop, budget exhaustion, or cancellation). Distinct from an
    /// UNPARSEABLE reply, which is not an error at all — it degrades to a
    /// verbatim passthrough `AgentIntent` (see `crate::intent`'s module
    /// doc for the full policy).
    #[error("intent classification failed: {message}")]
    IntentClassification { message: String },
}

/// Alias for `std::result::Result<T, FacadeError>`, exported from the crate
/// root as `conway::Result`.
pub type Result<T> = std::result::Result<T, FacadeError>;
