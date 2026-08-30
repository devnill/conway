//! Configuration: schema, discovery, precedence merge, headroom, and
//! mandatory Anthropic subscription OAuth-token rejection.
//!
//! `load` is a pure, network-free, deterministic function of five ordered
//! sources (default < user < project < env < CLI). See `merge.rs` for the
//! precedence/env-mapping/validation logic and `schema.rs` for the wire
//! shape, including the reconciliations against already-committed
//! `conway_core` types disclosed there.

pub mod discovery;
pub mod locality;
pub mod merge;
pub mod model_metadata;
pub mod schema;
pub mod trust;
pub mod writer;

pub use discovery::discover;
pub use locality::role_is_local;
pub use merge::{
    apply_cli, load, load_ignoring_user_config, merged_document, validate, CliOverrides,
    LoadOptions,
};
pub use model_metadata::ModelMetadata;
pub use schema::ConwayConfig;
pub use writer::{
    set_backend_provider, set_claude_compat_entry, set_default_role, set_plugin_installed,
};

/// The result of [`load`]: the validated config plus any non-fatal
/// warnings (headroom-vs-context-window, and -- Stage 2a -- a `[tui]`
/// section present but no longer understood by this schema).
#[derive(Debug, Clone, PartialEq)]
pub struct LoadOutcome {
    pub config: ConwayConfig,
    pub warnings: Vec<ConfigWarning>,
}

/// One non-fatal problem `load` detected but did not fail on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    pub code: WarningCode,
    pub message: String,
}

/// The category of a [`ConfigWarning`]. `#[non_exhaustive]`: future warning
/// kinds are expected without that being a breaking change for callers that
/// match on `code` only to log `message`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningCode {
    /// A role's (or the global default's) effective headroom is `>=` the
    /// smallest `max_context_tokens` reachable through its chain, per
    /// loaded model metadata.
    HeadroomExceedsContext,
    /// A top-level `[tui]` section (or a `CONWAY_TUI__*` environment
    /// variable) is present in the merged document, but `ConwayConfig` no
    /// longer defines that key (Stage 2a moved `TuiSection` and its
    /// presentation-shaped siblings to `conway-cli`, the one reader that
    /// renders them). `load`/`load_ignoring_user_config` accept the rest of the
    /// document and discard `[tui]` rather than hard-failing the whole
    /// load on it -- the accepted-and-ignored-with-a-warning choice
    /// recorded for this migration; see `merge::merged_document`'s own doc
    /// for the escape hatch `conway-cli` uses to still read and act on
    /// `[tui]` itself.
    PresentationConfigIgnored,
    /// A plugin's `PluginManifest::optional` names a plugin id that is not
    /// among the final installed set -- `message` names both the dependent
    /// and the missing dependency. Unlike every other `WarningCode` above,
    /// this one is NOT produced by `config::load`: it is raised by
    /// `ConwayBuilder::build`, once the final installed plugin set is known
    /// (`docs/vision/DESIGN-plugin-dependencies.md` §4b: "an optional edge
    /// carries what the dependent falls back to... the host announces the
    /// degradation on whatever channel that host has"). Carried on the same
    /// `ConfigWarning`/`Conway::warnings()` surface as load-time warnings
    /// rather than a second parallel channel, since both are the identical
    /// shape -- a non-fatal, named, operator-facing notice -- and a caller
    /// already reading `Conway::warnings()` for one should not need a
    /// second accessor for the other. `ConwayBuilder::build` also emits a
    /// `tracing::warn!` naming the same two ids, for a host with no reason
    /// to read `Conway::warnings()` at all (e.g. a one-shot `-p` run).
    OptionalPluginDependencyMissing,
    /// A plugin's `PluginManifest::optional_host_caps` names a host
    /// capability this host does not offer -- `message` names both the
    /// plugin and the missing capability. The host-capability analogue of
    /// [`Self::OptionalPluginDependencyMissing`] one edge over (Edge A,
    /// plugin -> host, rather than Edge B, plugin -> plugin --
    /// `docs/vision/DESIGN-plugin-dependencies.md` §2/§4a): the SAME
    /// "announced, never silent" posture, raised by the SAME
    /// `ConwayBuilder::build` pass (right where the mandatory
    /// `PluginManifest::required_host_caps` gate already runs, via
    /// `conway::host_caps::HostCaps::missing_optional`), on the SAME
    /// `ConfigWarning`/`Conway::warnings()` surface, plus the identical
    /// `tracing::warn!` companion for a host with no reason to read
    /// `Conway::warnings()` at all.
    OptionalHostCapabilityMissing,
}

#[cfg(test)]
mod tests {
    /// Structural guard for the "`config::load` performs no network I/O"
    /// criterion, applied to every *production* source file under
    /// `crates/conway/src/config/`. `mod.rs` (this file) is excluded from
    /// the scan since its own assertion text must name the very identifiers
    /// it is checking for; excluding it does not weaken the guarantee —
    /// `mod.rs` only re-exports and defines `LoadOutcome`/`ConfigWarning`,
    /// it never performs I/O itself.
    #[test]
    fn config_module_never_names_a_network_client_identifier() {
        let network_client_needles = forbidden_network_identifiers();
        for path in production_config_files() {
            let contents = std::fs::read_to_string(&path).expect("read rs file");
            for needle in &network_client_needles {
                assert!(
                    !contents.contains(needle.as_str()),
                    "{} must not name a network-client identifier (no network I/O in config::load)",
                    path.display()
                );
            }
        }
    }

    /// Structural guard: the local metadata refresh entry point must never
    /// be called from `config::load`'s call graph. It only appears in
    /// `model_metadata.rs` (behind `#[cfg(feature = "metadata-refresh")]`)
    /// and nothing in `merge.rs`/`discovery.rs`/`schema.rs` references it.
    /// `mod.rs` is excluded from the scan for the same self-referential
    /// reason as above.
    #[test]
    fn refresh_is_never_called_from_load() {
        let call_needle = refresh_call_needle();
        for path in production_config_files() {
            if path.file_name().and_then(|n| n.to_str()) == Some("model_metadata.rs") {
                continue;
            }
            let contents = std::fs::read_to_string(&path).expect("read rs file");
            assert!(
                !contents.contains(call_needle.as_str()),
                "{} must not call the metadata refresh function from config::load's call graph",
                path.display()
            );
        }
    }

    fn production_config_files() -> Vec<std::path::PathBuf> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config");
        std::fs::read_dir(&dir)
            .expect("read config/ dir")
            .map(|entry| entry.expect("dir entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .filter(|path| path.file_name().and_then(|n| n.to_str()) != Some("mod.rs"))
            .collect()
    }

    fn forbidden_network_identifiers() -> Vec<String> {
        vec![
            "reqwest".to_string(),
            "hyper".to_string(),
            "TcpStream".to_string(),
        ]
    }

    fn refresh_call_needle() -> String {
        "refresh(".to_string()
    }
}
