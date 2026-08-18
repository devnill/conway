//! Facade-reachable construction for the durable [`crate::plugin::
//! MemoryStore`] implementation (board item `01M09P2T8E5M292WMSMS64CVC4`).
//!
//! **Why this module exists, when `conway_session::JsonlSessionStore` gets
//! no equivalent re-export.** `ConwayBuilder::build` constructs the default
//! `SessionStore` itself, INTERNALLY, from `ConwayConfig::session.root` --
//! an embedder never needs to name `JsonlSessionStore` to get one (only to
//! override it via `with_session_store`, which needs the PORT, not the
//! concrete type). A `MemoryStore`-backed plugin has no equivalent
//! builder-owned construction point: `Plugin`s are constructed and handed
//! to `ConwayBuilder::with_plugin`/`install_selected` BEFORE `build()` runs,
//! so whoever constructs `conway_plugin_memory::MemoryPlugin` needs an
//! already-built `Arc<dyn MemoryStore>` in hand at THAT point, not after.
//! Without this re-export, a caller wanting the durable, filesystem-backed
//! implementation would have to depend on `conway-session` directly --
//! exactly the shortcut the plugin tier's facade-only discipline (and, for
//! `conway-cli` specifically, its own `no_forbidden_deps` test) exists to
//! close off.
//!
//! Gated behind the SAME `jsonl-store` feature `conway-session` itself is
//! optional behind (T5, `crates/conway/tests/architecture_invariants.rs`):
//! a build with that feature disabled compiles this module as empty, never
//! silently links `conway-session` regardless.
#[cfg(feature = "jsonl-store")]
pub use conway_session::FsMemoryStore;
