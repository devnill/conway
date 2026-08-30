//! Fetches a Claude Code marketplace's manifest and installs a plugin from
//! it into conway's own plugin store (board item
//! `01M0VR96Y87FF2BVNTBSC6GEYR`) -- the network-reaching half of the
//! plugin feature; the trust question is settled and out of scope for this
//! crate (`docs/plugins/trust-and-security.md`'s marketplace-ruling
//! section: a fetched artifact is checked against nothing, the same
//! footing as a hook command -- this crate builds no digest check, no
//! allow-list, and no trust prompt, and never will).
//!
//! # The three problems this crate exists to solve
//!
//! 1. **A network dependency in a crate that had none.** See `Cargo.toml`'s
//!    own doc for the dependency-minimalism argument (`reqwest` is reused
//!    from the workspace lock, not newly added; `conway-cli` itself never
//!    gains the dependency, because this crate's own public functions build
//!    their own client internally).
//! 2. **No plugin store.** [`install::install_plugin`]/[`install::
//!    install_entry`]/[`install::uninstall_plugin`] -- see `install.rs`'s
//!    own doc for where a fetched artifact lives, who owns it, and why
//!    nothing is written until every one of a plugin's declared files has
//!    been fetched successfully (P-13).
//! 3. **No config writer for an array of OBJECTS.** NOT solved by this
//!    crate -- see spec update 1, confirmed here: an installed plugin is
//!    declared to conway as an ordinary `[plugins].claude_compat[]` entry
//!    (`{ id, dir }`, `conway::config::schema::ClaudeCompatPluginEntry`),
//!    the exact shape [`install::InstalledPlugin`] already carries. The
//!    writer itself (`conway::config::writer::set_claude_compat_entry`)
//!    lives in the `conway` facade crate, alongside its sibling
//!    `set_plugin_installed` -- this crate has no `conway` dependency in
//!    `[dependencies]` (only in `[dev-dependencies]`, for this crate's own
//!    end-to-end test) and never writes to `settings.json` itself. The
//!    caller (`crates/conway-cli/src/tui/app/marketplace.rs`) is what wires
//!    [`install::InstalledPlugin`]'s `id`/`dir` into that writer, mirroring
//!    `tui/app/plugin_toggle.rs`'s own division of labor between "write
//!    the artifact" and "write the config that names it".
//!
//! # `01M0Y6RYZA94BK6YXJ7X8TNEGR`: a real, published Claude Code marketplace
//! now installs
//!
//! This crate originally understood only its OWN manifest format -- a
//! files-map `plugins[]` entry identified by `id`. No published Claude Code
//! marketplace has ever used that shape, so `01M0VR96Y87FF2BVNTBSC6GEYR`'s
//! own "install a plugin from a Claude Code marketplace" claim was false
//! the moment an operator pointed it at a real one. `manifest.rs` now
//! parses the real schema too (`name`+`source`, `owner`/`metadata` tolerated
//! permissively -- that module's own doc has the full shape), and this
//! crate's own (crate-private) `git_source` module fetches a `git-subdir`/
//! `github` source by invoking the SYSTEM `git` binary. The files-map
//! format is KEPT, not replaced: it is what lets a conway-native
//! marketplace exist with no git remote of its own.
//!
//! # What this crate does NOT do
//!
//! No archive extraction (`.tar.gz`/`.zip`) of any kind, ever
//! (`Cargo.toml`'s own doc has the full argument) -- a source kind that
//! would need one refuses BY NAME (`MarketplaceError::UnsupportedSourceKind`)
//! rather than being fetched. No digest check, no allow-list, no trust
//! prompt (`docs/plugins/trust-and-security.md`'s ruling, restated at the
//! top of this doc) -- unchanged by adding a git fetcher: a git-cloned
//! artifact sits on the identical trust footing an HTTP-fetched one always
//! has. Everything a fetched plugin's directory declares runs with the
//! operator's own privileges once conway loads it, on the identical
//! footing `[plugins].claude_compat[]`'s own doc already states for a
//! directory the operator placed there by hand
//! (`conway::config::schema::PluginsConfig::claude_compat`) -- fetching the
//! bytes over the network rather than the operator copying them by hand
//! changes nothing about that footing (the ruling's own words: "a
//! marketplace-sourced artifact is not safer than a command path the
//! operator typed by hand").

pub mod error;
/// Fetches a `git-subdir`/`github` `manifest::PluginSource` by invoking the
/// system `git` binary -- board item `01M0Y6RYZA94BK6YXJ7X8TNEGR`, layer 4.
/// Not part of this crate's public API: `install::install_entry` is the one
/// place that needs it, exactly the way `manifest::fetch_bytes` stays
/// crate-private behind `install::install_entry`/`manifest::
/// fetch_marketplace` today.
mod git_source;
pub mod install;
pub mod manifest;

pub use error::MarketplaceError;
pub use install::{
    install_entry, install_plugin, plugin_dir, uninstall_plugin, validate_plugin_id,
    InstalledPlugin, MAX_FILES_PER_PLUGIN, MAX_FILE_BYTES,
};
pub use manifest::{
    fetch_marketplace, MarketplaceManifest, MarketplaceMetadata, MarketplaceOwner,
    MarketplacePluginEntry, PluginSource, MAX_MANIFEST_BYTES,
};
