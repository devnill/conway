//! Board item `01M1WVQM440XZDSP0KC664HJ7S` (architecture review finding F11):
//! `ConwayBuilder::with_session_store` already existed before this item --
//! what was missing was a re-export gap (`SessionStore::live_owner`/
//! `touch_live_owner` need `LiveOwner`, not previously reachable from a
//! facade-only crate) and, separately, proof that an embedder-supplied,
//! non-default `SessionStore` is genuinely consulted by a real run rather
//! than merely accepted by the builder. This test is that proof, mirroring
//! `example_smoke.rs`'s own minimal-session shape.

use std::sync::Arc;
use std::time::Duration;

use conway::test_support::base_config;
use conway::{ConwayBuilder, SessionFilter, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::ids::{BackendId, ModelId, ModelRef};
use conway_core::ports::SessionStore;
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

const T: Duration = Duration::from_secs(5);

fn route() -> ModelRef {
    ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }
}

#[tokio::test]
async fn with_session_store_is_genuinely_consulted_for_create_and_append() {
    // `FakeStore` is a facade-external, non-default `SessionStore`
    // implementation -- the default `build()` would otherwise construct is
    // `JsonlSessionStore`, backed by real files on disk.
    let store = Arc::new(FakeStore::new());

    let conway = ConwayBuilder::from_parts(base_config())
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(store.clone())
        .with_router(Arc::new(FakeRouter::single(route())))
        .build()
        .expect("build with a custom, non-default SessionStore should succeed");

    assert_eq!(
        store.total_record_count(),
        0,
        "sanity: nothing appended before any session has run"
    );

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");
    let turn = tokio::time::timeout(T, session.prompt("Hello, custom session store!"))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");
    let _ = tokio::time::timeout(T, turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    // The proof: conway must have actually called `create`/`append` on the
    // INJECTED store for this real turn, not merely accepted it at build
    // time and silently fallen back to something else.
    assert!(
        store.total_record_count() > 0,
        "conway must actually append records through the injected SessionStore"
    );

    let listed = store
        .list(SessionFilter::default())
        .await
        .expect("list should succeed against the injected store");
    assert_eq!(
        listed.len(),
        1,
        "the session conway created must be visible through the SAME injected store, \
         not a second, hidden one"
    );
    assert_eq!(listed[0].id, session.id());
}
