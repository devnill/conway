//! VERIFICATION ANCHOR for board item 01KZY8QRAVVVKCRBZ6HAEGW3GG
//! ("`/checkout` and a reachable `ContextMask` -- the session-history
//! plugin's second and third commands").
//!
//! `LogRecord::ContextMask` had a reader (`conway_core::transcript::
//! TranscriptResolver::apply_context_mask`) and a guard test long before
//! this item, but -- verified again at the start of this item, not merely
//! assumed from the record's own doc comment -- **no producer anywhere in
//! the tree outside tests**. This file drives the real producer this item
//! adds, [`conway::Conway::mask_record`] (reached in production through
//! `conway_plugin_history`'s `/conway.history.mask` command via
//! `CommandOutcome::MaskRecord` -- see that crate's own tests for the
//! command-parsing half; this file proves the OTHER half: that a mask
//! actually appended through this facade method changes what a forked
//! child's assembled request carries, not merely that a record was
//! written).
//!
//! **The anchor, precisely.** [`a_masked_record_is_absent_from_the_forked_
//! childs_assembled_segments`] masks a specific, real `UserTurn` record,
//! forks the session, drives the child through a REAL turn against a
//! [`ScriptedBackend`], and asserts on `GenerateRequest::segments` --
//! what the model would actually receive -- that the masked turn's own
//! text is gone while an unmasked sibling turn's text survives.
//! [`without_a_mask_the_same_turn_is_present`] is the same scenario with
//! the `mask_record` call simply omitted: it is the same test shown to
//! FAIL the first test's own assertion when the mask is removed, exactly
//! what the item's own "VERIFICATION ANCHOR" section asks for, done as an
//! actual second test rather than asserted in prose.
//!
//! **Also proven here: the record round-trips and is reversible**
//! (the item's first acceptance criterion) -- [`the_mask_record_round_
//! trips_and_is_reversible`] appends a mask, reads it back byte-for-byte
//! via `SessionStore::read`, then appends the opposite (`excluded: false`)
//! and shows the LATEST-by-append-order record is what a resolver would
//! honor, per `LogRecord::ContextMask`'s own "later records win" doc.
//!
//! **What this file does NOT cover:** the `/conway.history.mask`/
//! `/conway.history.checkout` COMMANDS' own arg-parsing
//! (`conway-plugin-history`'s own unit tests), and the TUI's own
//! `CommandOutcome::MaskRecord`/`CommandOutcome::Checkout` host-side
//! resolution against the REAL plugin end to end
//! (`crates/conway-cli/src/tui/app/plugin_cmd.rs`'s own test module, the
//! same file `/rewind`'s own anchor lives in). This file is the one
//! layer none of those reach: the actual effect on what a model sees.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, ForkSpec, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::ContentBlock;
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, SeqRange};
use conway_core::log::LogRecord;
use conway_core::ports::Backend;
use conway_testkit::{
    text_response, FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn,
};

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Every turn scripted below replies with a fixed, marker-free
/// acknowledgement -- the interesting text lives in the PROMPTS
/// (`UserTurn` records), which is what gets masked/read back.
///
/// Returns the store AND the backend handle -- `Conway` exposes no
/// accessor for "the backend it was built with," so every test keeps its
/// own `Arc<ScriptedBackend>` to inspect `calls()` after driving turns.
fn build_conway_and_store(turns: usize) -> (Conway, Arc<FakeStore>, Arc<ScriptedBackend>) {
    let script = (0..turns)
        .map(|_| ScriptedTurn::Respond(text_response("ack")))
        .collect();
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let store = Arc::new(FakeStore::new());
    let gate: Arc<dyn conway_core::ports::PermissionGate> =
        Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let conway = ConwayBuilder::from_parts(base_config())
        .with_backend(backend.clone() as Arc<dyn Backend>)
        .with_session_store(store.clone())
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed");
    (conway, store, backend)
}

/// Reads a session's own persisted records and returns the `LogSeq` of the
/// `UserTurn` whose text is exactly `needle` -- the real seq
/// `Conway::mask_record` targets, not a guessed one.
async fn find_user_turn_seq(
    store: &FakeStore,
    sid: conway::SessionId,
    needle: &str,
) -> conway::LogSeq {
    let records = conway::SessionStore::read(store, &sid, SeqRange::full())
        .await
        .expect("read should succeed");
    records
        .into_iter()
        .find_map(|rec| match rec {
            LogRecord::UserTurn { seq, text, .. } if text == needle => Some(seq),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no UserTurn record with text {needle:?} in {sid}"))
}

fn request_texts(req: &conway_core::ports::GenerateRequest) -> Vec<String> {
    req.segments
        .iter()
        .flat_map(|seg| seg.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn contains_marker(texts: &[String], marker: &str) -> bool {
    texts.iter().any(|t| t.contains(marker))
}

/// **The positive half of the verification anchor.** Masks the `UserTurn`
/// carrying `ALPHA_MARKER`, forks the session, and drives the child
/// through a real turn: the assembled request the (fake) model actually
/// receives must not contain `ALPHA_MARKER` anywhere, while `BETA_MARKER`
/// (an unmasked sibling turn) must still be present.
#[tokio::test]
async fn a_masked_record_is_absent_from_the_forked_childs_assembled_segments() {
    let (conway, store, backend) = build_conway_and_store(3);
    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..Default::default()
        })
        .await
        .expect("new_session should succeed");
    let sid = handle.id();

    handle
        .prompt("ALPHA_MARKER")
        .await
        .expect("turn 1 should not error")
        .text()
        .await
        .expect("turn 1 should complete");
    handle
        .prompt("BETA_MARKER")
        .await
        .expect("turn 2 should not error")
        .text()
        .await
        .expect("turn 2 should complete");

    let alpha_seq = find_user_turn_seq(&store, sid, "ALPHA_MARKER").await;

    conway
        .mask_record(sid, alpha_seq, true)
        .await
        .expect("mask_record should succeed");

    let head = conway
        .session_head(sid)
        .await
        .expect("session_head should succeed");
    let child = conway
        .fork_from(sid, head, ForkSpec::new(""))
        .await
        .expect("fork_from should succeed");

    child
        .prompt("GAMMA_MARKER")
        .await
        .expect("child turn should not error")
        .text()
        .await
        .expect("child turn should complete");

    // Inspect the LAST request the backend actually received (the child's
    // own turn) -- the assembled segments a real model would see.
    let last_request = backend
        .calls()
        .into_iter()
        .last()
        .expect("at least one request must have been sent");
    let texts = request_texts(&last_request);

    assert!(
        !contains_marker(&texts, "ALPHA_MARKER"),
        "the masked turn's text must not reach the forked child's assembled request: {texts:?}"
    );
    assert!(
        contains_marker(&texts, "BETA_MARKER"),
        "an UNMASKED sibling turn's text must still reach the forked child's assembled \
         request: {texts:?}"
    );

    // The masked record itself was never mutated or deleted -- still
    // readable, byte-for-byte, from the parent's own log.
    let parent_records = conway::SessionStore::read(store.as_ref(), &sid, SeqRange::full())
        .await
        .expect("read should succeed");
    assert!(
        parent_records
            .iter()
            .any(|r| matches!(r, LogRecord::UserTurn { text, .. } if text == "ALPHA_MARKER")),
        "masking must never delete or mutate the target record in the parent's own log"
    );
}

/// **The negative half of the verification anchor**: the SAME scenario as
/// above with the `mask_record` call simply omitted -- `ALPHA_MARKER` IS
/// present in the forked child's assembled request. This is what makes the
/// positive test's own absence assertion meaningful rather than vacuous:
/// remove the mask, and the exact assertion the first test makes would
/// fail.
#[tokio::test]
async fn without_a_mask_the_same_turn_is_present() {
    let (conway, _store, backend) = build_conway_and_store(3);
    let handle = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..Default::default()
        })
        .await
        .expect("new_session should succeed");
    let sid = handle.id();

    handle
        .prompt("ALPHA_MARKER")
        .await
        .expect("turn 1 should not error")
        .text()
        .await
        .expect("turn 1 should complete");
    handle
        .prompt("BETA_MARKER")
        .await
        .expect("turn 2 should not error")
        .text()
        .await
        .expect("turn 2 should complete");

    // No `mask_record` call here -- the only difference from the positive
    // test above.
    let head = conway
        .session_head(sid)
        .await
        .expect("session_head should succeed");
    let child = conway
        .fork_from(sid, head, ForkSpec::new(""))
        .await
        .expect("fork_from should succeed");

    child
        .prompt("GAMMA_MARKER")
        .await
        .expect("child turn should not error")
        .text()
        .await
        .expect("child turn should complete");

    let last_request = backend
        .calls()
        .into_iter()
        .last()
        .expect("at least one request must have been sent");
    let texts = request_texts(&last_request);

    assert!(
        contains_marker(&texts, "ALPHA_MARKER"),
        "sanity: without a mask, the turn's text must reach the forked child's assembled \
         request, or this test (and the positive test's own absence assertion) proves \
         nothing: {texts:?}"
    );
}

/// **Acceptance: "the record round-trips and is reversible."** Appends a
/// mask, reads it back byte-for-byte, then appends the opposite
/// (`excluded: false`) and shows the LATEST record by append order is what
/// decides -- `LogRecord::ContextMask`'s own documented "later records win"
/// rule, exercised through the real producer rather than a hand-built
/// fixture.
#[tokio::test]
async fn the_mask_record_round_trips_and_is_reversible() {
    let (conway, store, backend) = build_conway_and_store(2);
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let sid = handle.id();
    handle
        .prompt("only turn")
        .await
        .expect("turn should not error")
        .text()
        .await
        .expect("turn should complete");
    let target_seq = find_user_turn_seq(&store, sid, "only turn").await;

    let mask_seq = conway
        .mask_record(sid, target_seq, true)
        .await
        .expect("mask_record should succeed");

    let records = conway::SessionStore::read(store.as_ref(), &sid, SeqRange::full())
        .await
        .expect("read should succeed");
    let masked = records
        .iter()
        .find(|r| r.seq() == Some(mask_seq))
        .expect("the appended mask record must be readable back");
    match masked {
        LogRecord::ContextMask {
            target_seq: ts,
            excluded,
            ..
        } => {
            assert_eq!(*ts, target_seq);
            assert!(*excluded);
        }
        other => panic!("expected LogRecord::ContextMask, got {other:?}"),
    }

    // Reverse it -- a second, later mask for the same target_seq.
    let unmask_seq = conway
        .mask_record(sid, target_seq, false)
        .await
        .expect("un-mask should succeed");
    assert!(
        unmask_seq.0 > mask_seq.0,
        "the un-mask must be a LATER record, not a rewrite"
    );

    let records_after = conway::SessionStore::read(store.as_ref(), &sid, SeqRange::full())
        .await
        .expect("read should succeed");
    // BOTH records are still present -- append-only, neither was deleted or
    // overwritten.
    assert!(records_after.iter().any(|r| r.seq() == Some(mask_seq)));
    assert!(records_after.iter().any(|r| r.seq() == Some(unmask_seq)));

    // And a fork taken NOW inherits the record: since the latest mask for
    // `target_seq` has `excluded: false`, the child sees it.
    let head = conway.session_head(sid).await.expect("session_head");
    let child = conway
        .fork_from(sid, head, ForkSpec::new(""))
        .await
        .expect("fork_from should succeed");
    child
        .prompt("child turn")
        .await
        .expect("child turn should not error")
        .text()
        .await
        .expect("child turn should complete");
    let last_request = backend
        .calls()
        .into_iter()
        .last()
        .expect("at least one request must have been sent");
    let texts = request_texts(&last_request);
    assert!(
        contains_marker(&texts, "only turn"),
        "un-masking (a later ContextMask with excluded: false) must restore the record for a \
         fork taken afterward: {texts:?}"
    );
}
