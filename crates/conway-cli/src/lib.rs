//! `conway-cli`'s library target.
//!
//! This crate ships exactly one published artifact -- the `conway` binary
//! (`src/main.rs`) -- and the workspace's `publish = false` setting means
//! this `lib` target is never itself published or consumed as a dependency
//! by anything outside this package. It exists purely so `src/main.rs` and
//! this crate's own `tests/*.rs` integration suite can share one module
//! tree, which needed: proving
//! `commands::routes::run` answers correctly for a `Conway` built with an
//! injected `Router` (`conway::ConwayBuilder::with_router`) requires
//! constructing that `Conway` directly, in-process -- there is no CLI flag
//! that injects a router into the compiled binary, and a subprocess spawn
//! of `conway` (the pattern every other integration test in this crate
//! uses, `tests/common::run_conway`) can therefore never reach that case.
//!
//! Every module below is otherwise unchanged: internal `crate::` paths
//! resolve exactly as they did when `main.rs` was this crate's only root
//! (`mod cli; mod commands; ...`) -- only the root file moved.

pub mod claude_compat_plugins;
pub mod cli;
pub mod commands;
pub mod diag;
pub mod exit;
pub mod first_party_plugins;
pub mod mcp_plugins;
mod model_pin;
pub mod oneshot;
pub mod render;
pub mod session_names;
pub mod session_ref;
pub mod signal;
pub mod subprocess_plugins;
pub mod tui;
