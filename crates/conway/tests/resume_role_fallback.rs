//! Board item `01M3SJBY8DXDB1PWEAXARZY9FH`, criterion 3's own fallback
//! half: `Conway::resume_with`'s `role: None` arm now validates the
//! session's own persisted `SessionMeta::role` against THIS config's
//! `[routing].roles` before handing it to `resume_root` -- see
//! `Conway::resolve_resumed_role`'s own doc for the full mechanism and why
//! it lives at the facade layer, not `conway-runtime`.
//!
//! Mirrors `resume.rs`'s own "two `Conway`s sharing one `FakeStore`,
//! simulated restart" shape: a session is created under a role, then a
//! SECOND `Conway` -- standing in for a later process start whose config
//! no longer has that role -- resumes it. Before this item, `resume_root`
//! would have handed the stale alias straight to `AgentSpec`, and the
//! resumed turn below would have failed with `RoutingError::UnknownRole`
//! instead of completing.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::test_support::{allow_once_gate, base_config};
use conway::{ConwayBuilder, RoleAlias, SessionSpec};
use conway_core::ids::{BackendId, ModelId, ModelRef};
use conway_core::log::LogRecord;
use conway_core::ports::SessionStore;
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

/// The "default" role's own model under [`real_router`] -- the fallback
/// target a stale, unconfigured role must land on.
fn default_model_ref() -> ModelRef {
    ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("default-model"),
    }
}

/// A REAL `MinimalRouter` (not `FakeRouter::single`, which would resolve
/// ANY role string, including the stale one, and so could never prove the
/// fallback actually ran) -- only the "default" role is configured. A
/// resume that handed the stale `"custom"` alias straight through would
/// fail routing here with `RoutingError::UnknownRole`.
fn real_router() -> Arc<dyn conway_core::ports::Router> {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        conway_core::routing::RoleConfig {
            chain: vec![default_model_ref()],
            ..Default::default()
        },
    );
    Arc::new(conway_core::routing::MinimalRouter::new(
        conway_core::routing::RoutingConfig {
            roles,
            default_headroom_tokens: 4096,
            ..Default::default()
        },
    ))
}

/// A session created under role `"custom"` -- a role `base_config()`'s own
/// `[routing].roles` never configured at all, standing in equally well for
/// "renamed" or "removed since this session was created" (criterion 3
/// only cares that the recorded alias is NOT currently configured, not
/// which history led there).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_with_an_unconfigured_recorded_role_falls_back_and_still_completes_the_turn() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let sid = {
        let backend = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("first"))])
                .with_id(BackendId::new("fake")),
        );
        let conway = ConwayBuilder::from_parts(base_config())
            .with_backend(backend)
            .with_session_store(store.clone())
            .with_permission_gate(allow_once_gate())
            .with_router(real_router())
            .build()
            .expect("build should succeed");
        let handle = conway
            .new_session(SessionSpec {
                role: Some(RoleAlias::new("custom")),
                ..Default::default()
            })
            .await
            .expect("new_session should succeed even under an unconfigured role (unvalidated \
                      at creation time)");
        handle.id()
    };

    // A second `Conway` -- the simulated restart -- whose config never had
    // `"custom"` at all.
    let backend2 = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("second"))])
            .with_id(BackendId::new("fake")),
    );
    let conway2 = ConwayBuilder::from_parts(base_config())
        .with_backend(backend2.clone())
        .with_session_store(store.clone())
        .with_permission_gate(allow_once_gate())
        .with_router(real_router())
        .build()
        .expect("build should succeed");

    let resumed = conway2
        .resume(sid)
        .await
        .expect("resume must succeed despite the recorded role no longer being configured");

    tokio::time::sleep(Duration::from_millis(50)).await;
    let turn = resumed
        .prompt("second turn text")
        .await
        .expect("prompt on the fallback-resumed handle must succeed");
    tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result() must not hang")
        .expect(
            "the resumed turn must actually complete -- a stale role handed straight to \
             AgentSpec would instead fail routing with RoutingError::UnknownRole",
        );

    let calls = backend2.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].model,
        default_model_ref().model,
        "the fallback must route to the configured default role's own model"
    );

    // Criterion 3: "say so on screen at resume" -- the durable half, a
    // `SystemNote` naming both the stale alias and the fallback it was
    // replaced with, persisted into the session's own log before
    // `resume_root` ever ran (so the next backfill/replay carries it too).
    let records = resumed
        .transcript(resumed.root())
        .await
        .expect("transcript should be readable");
    let fallback_note = records.iter().find_map(|record| match record {
        LogRecord::SystemNote { reason, text, .. } if reason == "role_fallback_at_resume" => {
            Some(text.clone())
        }
        _ => None,
    });
    let text = fallback_note.expect(
        "expected a role_fallback_at_resume SystemNote in the resumed session's own log",
    );
    assert!(text.contains("custom"), "must name the stale role: {text:?}");
    assert!(text.contains("default"), "must name the fallback role: {text:?}");
}

/// The negative case: a role that IS still configured is read back
/// unchanged, with no fallback `SystemNote` at all -- the overwhelmingly
/// common path, proven unaffected by this item.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_with_a_still_configured_role_is_unaffected() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let sid = {
        let backend = Arc::new(
            ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("first"))])
                .with_id(BackendId::new("fake")),
        );
        let conway = ConwayBuilder::from_parts(base_config())
            .with_backend(backend)
            .with_session_store(store.clone())
            .with_permission_gate(allow_once_gate())
            .with_router(real_router())
            .build()
            .expect("build should succeed");
        let handle = conway
            .new_session(SessionSpec::default())
            .await
            .expect("new_session should succeed");
        handle.id()
    };

    let backend2 = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("second"))])
            .with_id(BackendId::new("fake")),
    );
    let conway2 = ConwayBuilder::from_parts(base_config())
        .with_backend(backend2)
        .with_session_store(store.clone())
        .with_permission_gate(allow_once_gate())
        .with_router(real_router())
        .build()
        .expect("build should succeed");

    let resumed = conway2
        .resume(sid)
        .await
        .expect("resume of a still-configured role must succeed exactly as before");

    let records = resumed
        .transcript(resumed.root())
        .await
        .expect("transcript should be readable");
    assert!(
        !records
            .iter()
            .any(|record| matches!(record, LogRecord::SystemNote { reason, .. } if reason == "role_fallback_at_resume")),
        "a still-configured role must never get a fallback notice: {records:#?}"
    );
}
