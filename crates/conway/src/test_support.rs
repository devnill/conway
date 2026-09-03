//! Facade-level test scaffolding: the one place a `Conway` is assembled
//! for a test.
//!
//! # Why this is not in `conway-testkit`
//!
//! `conway-testkit` ships the doubles for every `conway-core` port, and it
//! depends on `conway-core` and nothing else in this workspace on purpose
//! (T1 in `crates/conway/tests/architecture_invariants.rs` is the guard).
//! It structurally cannot offer a helper that returns a `Conway`, because
//! it cannot see `ConwayBuilder`. So the doubles lived there and the
//! *assembly* of those doubles into a working harness lived nowhere: it
//! was hand-rolled as a local `build_conway` helper in 46 test files across
//! seven crates, which made "change how a test's `Conway` is wired" a
//! 46-file edit.
//!
//! # Why a feature and not a crate
//!
//! This module is compiled only under the non-default `test-support`
//! feature (which implies `testkit`, since it is built out of those
//! doubles). A third party enabling nothing sees no trace of it: not the
//! module, not the `conway-testkit` dependency it pulls in. That is the
//! smallest shape that keeps it out of the default public surface, and it
//! reuses the exact mechanism the `testkit` feature already established
//! rather than adding a workspace crate whose only job is to be
//! dev-depended on.
//!
//! Consumers -- including `conway`'s own test suite, via a dev-dependency
//! on itself -- opt in with
//! `conway = { path = "..", features = ["test-support"] }` under
//! `[dev-dependencies]`.
//!
//! # What is deliberately NOT here
//!
//! There is no `fake_router()` either. `conway_testkit::FakeRouter::single`
//! already is that helper; `test_builder` calls it directly, and so should
//! any test that needs a router of its own.
//!
//! `base_config`/`base_config_at` *are* here (board item
//! `01M1FSM2FS9BMNN7G8934PQVM4`, superseding the note this module used to
//! carry about the 14 genuinely different configs among the old 46 local
//! copies): every one of those copies turned out to be
//! `ConwayConfig::baseline()` plus, at most, an overridden `cwd` --
//! nothing about assembling the `Conway` (the concern this module already
//! owned). A file whose fixture needs more than that mutates the
//! returned value or defines its own small local wrapper around these two
//! -- it never goes back to a fresh `ConwayConfig { .. }` literal.

use std::path::PathBuf;
use std::sync::Arc;

use conway_core::agent::PermissionDecision;
use conway_core::ids::{BackendId, ModelId, ModelRef};
use conway_core::ports::{Backend, PermissionGate, Router, SessionStore};
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

use crate::config::schema::ConwayConfig;
use crate::{Conway, ConwayBuilder, PluginSelection};

/// The shared starting point for a test's `ConwayConfig`: one `default`
/// role with an empty chain, every other section at its schema default --
/// exactly `ConwayConfig::baseline()`.
///
/// A test that needs one field different from this mutates the returned
/// value (`let mut c = base_config(); c.plugins.default_backends =
/// vec![];`), or defines its own small local wrapper that does the same
/// -- never a fresh `ConwayConfig { .. }` struct literal, which is what
/// `crates/conway/tests/architecture_invariants.rs`'s
/// `t12_no_hand_rolled_base_config_in_facade_tests` guards against.
pub fn base_config() -> ConwayConfig {
    ConwayConfig::baseline()
}

/// [`base_config`] with `cwd` overridden -- the other shape the old local
/// copies took, for a test whose subject depends on the working
/// directory the config reports.
pub fn base_config_at(cwd: impl Into<PathBuf>) -> ConwayConfig {
    let mut config = base_config();
    config.cwd = cwd.into();
    config
}

/// The model reference every fake router in this workspace's tests
/// resolves to: backend `"fake"`, model `"echo-model"`.
///
/// Both ids are load-bearing, not decoration. `"fake"` is the
/// `BackendId` the injected `FakeBackend`/`ScriptedBackend` doubles are
/// built with (`ScriptedBackend::with_id(BackendId::new("fake"))`), so a
/// route resolved to any other backend id would find no backend at all.
pub fn echo_model() -> ModelRef {
    ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }
}

/// The permission gate the overwhelming majority of these suites want:
/// allow every request, once.
///
/// A test whose subject IS the gate injects its own instead --
/// `with_permission_gate` overwrites, so chaining one onto a builder from
/// `test_builder` is enough.
pub fn allow_once_gate() -> Arc<dyn PermissionGate> {
    Arc::new(FakeGate::new(PermissionDecision::AllowOnce))
}

/// `ScriptedBackend::new(script)` under the `"fake"` backend id that
/// `echo_model` routes to.
///
/// Twenty-odd of the old local copies opened with exactly this line. The
/// id is not cosmetic: `ScriptedBackend::new` defaults to
/// `BackendId::new("scripted")`, which no route in these suites resolves
/// to, so the `with_id` is what makes the double reachable at all.
pub fn scripted_backend(script: Vec<ScriptedTurn>) -> Arc<dyn Backend> {
    Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")))
}

/// A `ConwayBuilder` over `config` with the three overwritable ports every
/// test injects already wired to doubles: an empty `FakeStore`, an
/// allow-once `FakeGate`, and `FakeRouter::single(echo_model())`.
///
/// Each of those three is stored as an `Option` inside `ConwayBuilder`, so
/// a later `with_session_store` / `with_permission_gate` / `with_router`
/// simply replaces it. That is what makes pre-wiring them safe.
///
/// **A backend is deliberately not pre-wired.** `ConwayBuilder::
/// with_backend` *pushes onto a list* rather than replacing a slot, so a
/// default backend could never be overridden, only silently added to --
/// giving the build two backends and a capability index the test never
/// asked for. Every caller supplies its own.
///
/// Use `test_builder_without_router` instead when the router or the
/// backend is meant to come from `config` through a registered factory.
pub fn test_builder(config: ConwayConfig) -> ConwayBuilder {
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(echo_model()));
    test_builder_without_router(config).with_router(router)
}

/// `test_builder` minus the router.
///
/// For the suites that resolve a router or a backend out of `config`
/// through `with_router_factory` / `with_backend_factory`: an injected
/// `Router` takes precedence over a `RouterFactory` at `build()` time, so
/// pre-wiring `FakeRouter` there would pre-empt the very thing under test.
pub fn test_builder_without_router(config: ConwayConfig) -> ConwayBuilder {
    ConwayBuilder::from_parts(config)
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(allow_once_gate())
}

/// The shape 30-odd of the old local copies had: `config` + an injected
/// backend + an injected store, every other port a double, built.
///
/// Panics with a message naming the injected ports if `build()` fails --
/// a test that expects `build()` to fail should drive `ConwayBuilder`
/// itself and inspect the `Err`.
pub fn build_conway(
    config: ConwayConfig,
    backend: Arc<dyn Backend>,
    store: Arc<dyn SessionStore>,
) -> Conway {
    test_builder(config)
        .with_backend(backend)
        .with_session_store(store)
        .build()
        .expect("build should succeed with every port injected")
}

/// `build_conway` with the echo backend six of the old local copies
/// defaulted to: `FakeBackend::echo(BackendId::new("fake"))`, whose id is
/// the one `echo_model` routes to.
///
/// A named variant rather than a default inside `build_conway`, because
/// most callers script their backend and the two groups must not drift
/// into each other.
pub fn build_conway_with_echo_backend(
    config: ConwayConfig,
    store: Arc<dyn SessionStore>,
) -> Conway {
    build_conway(
        config,
        Arc::new(FakeBackend::echo(BackendId::new("fake"))),
        store,
    )
}

/// `build_conway` plus `with_builtin_plugins(PluginSelection::All)` and an
/// explicit gate, over a fresh `FakeStore`.
///
/// The shape every suite that drives a REAL builtin tool end to end had to
/// hand-roll: `bash` ships off by default and cannot be declined, so these
/// files must opt the whole builtin set in explicitly. They also all
/// inject the gate (it is usually the thing under test) and none of them
/// keeps a handle on the store.
pub fn build_conway_with_builtins(
    config: ConwayConfig,
    backend: Arc<dyn Backend>,
    gate: Arc<dyn PermissionGate>,
) -> Conway {
    test_builder(config)
        .with_backend(backend)
        .with_permission_gate(gate)
        .with_builtin_plugins(PluginSelection::All)
        .build()
        .expect("build should succeed with the real builtin tools registered")
}
