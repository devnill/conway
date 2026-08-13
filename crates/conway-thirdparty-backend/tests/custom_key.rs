//! Board item 01KZMM8ABQJQGHTDTP5S29P88C: proves `BackendBuildContext::
//! extra` genuinely reaches a third-party `BackendFactory::build`, and that
//! the value it carries changes the built backend's own observable
//! behaviour -- not merely that the field is populated.
//!
//! `fixture::write_settings_with_greeting` renders a real `settings.json`
//! whose `[backends.thirdparty]` entry carries a `greeting` key beyond
//! `kind` (one `BackendEntry`'s five typed fields do not recognize, and
//! that struct's own `#[serde(flatten)] extra` map is the only reason it
//! parses at all rather than being rejected). Before this item,
//! `BackendBuildContext` had no field to carry that key onward, so
//! `ThirdPartyBackendFactory::build` could never have read it no matter how
//! it was written -- this test's own assertion is therefore the proof that
//! gap is closed, not merely that this crate compiles against a new field.

use conway_thirdparty_backend::{build_conway, fixture, REPLY_TEXT};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_custom_extra_key_changes_the_backends_own_reply() {
    let dir = fixture::fresh_dir("custom-key");
    let config_path = fixture::write_settings_with_greeting(&dir, "stranger");

    let conway = build_conway(&dir, &config_path).expect(
        "build should succeed: a real settings.json-derived thirdparty-stub backend, resolved \
         through the registered ThirdPartyBackendFactory alone",
    );

    let handle = conway
        .new_session(conway::SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let text = tokio::time::timeout(std::time::Duration::from_secs(10), turn.text())
        .await
        .expect("turn must not hang")
        .expect("turn should succeed");

    assert!(
        text.contains("stranger"),
        "the turn's own text must name the `greeting` value the settings.json entry carried \
         beyond `kind` -- proving BackendBuildContext::extra reached \
         ThirdPartyBackendFactory::build and that the built backend's observable behaviour \
         genuinely depends on it, not merely that the field arrived populated. Got: {text:?}"
    );
    assert_ne!(
        text, REPLY_TEXT,
        "a backend built from an entry that DID carry a custom key must not fall back to the \
         same reply text a plain, key-free entry produces -- that would mean `extra` reached \
         the factory but changed nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_extra_value_produces_a_different_reply() {
    let dir_a = fixture::fresh_dir("custom-key-a");
    let dir_b = fixture::fresh_dir("custom-key-b");
    let config_a = fixture::write_settings_with_greeting(&dir_a, "alice");
    let config_b = fixture::write_settings_with_greeting(&dir_b, "bob");

    async fn turn_text(dir: &std::path::Path, config_path: &std::path::Path) -> String {
        let conway = build_conway(dir, config_path)
            .expect("build should succeed: a real settings.json-derived thirdparty-stub backend");
        let handle = conway
            .new_session(conway::SessionSpec::default())
            .await
            .expect("new_session should succeed");
        let turn = handle.prompt("hi").await.expect("prompt should succeed");
        tokio::time::timeout(std::time::Duration::from_secs(10), turn.text())
            .await
            .expect("turn must not hang")
            .expect("turn should succeed")
    }

    let text_a = turn_text(&dir_a, &config_a).await;
    let text_b = turn_text(&dir_b, &config_b).await;

    assert!(text_a.contains("alice"), "got: {text_a:?}");
    assert!(text_b.contains("bob"), "got: {text_b:?}");
    assert_ne!(
        text_a, text_b,
        "two entries differing only in their `greeting` value must produce two different \
         replies -- the observable this item's acceptance criteria require, not merely that \
         `extra` is non-empty"
    );
}
