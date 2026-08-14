//!'s library-embedder demonstration:
//! a `kind = "thirdparty-stub"` `[backends.thirdparty]` entry in a real,
//! on-disk `settings.json`, loaded through `conway::config::load`,
//! resolved against `ThirdPartyBackendFactory` installed through
//! `ConwayBuilder::with_backend_factory` -- no injected `Backend` anywhere
//! -- completing a real turn whose text is asserted through the facade.
//!
//! Credential-free and network-free: `ThirdPartyBackend` performs no I/O
//! (`crates/conway-thirdparty-backend/src/lib.rs`'s own module doc), the
//! session store is the real `conway_session::JsonlSessionStore` writing to
//! an isolated temp directory (local disk only, never a socket), and
//! `build_conway`'s `XDG_CONFIG_HOME` isolation keeps a real
//! `~/.conway/settings.json` on the machine running this out of the merge
//! entirely (same module's doc comment).
//!
//! `src/bin/thirdparty_backend_demo.rs` is this same proof compiled to a
//! genuinely separate binary and driven via `assert_cmd` from
//! `binary.rs` -- "not trapped in one mode" (the item's own acceptance
//! criterion) means both files must independently pass, not that either
//! alone suffices.

use conway_thirdparty_backend::{build_conway, fixture, REPLY_TEXT};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn factory_installed_backend_serves_a_real_turn_through_the_facade() {
    let dir = fixture::fresh_dir("end-to-end");
    let config_path = fixture::write_settings(&dir);

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

    assert_eq!(
        text, REPLY_TEXT,
        "the turn's own text must be the third-party backend's real reply, proving the \
         registered BackendFactory constructed a working backend whose output genuinely \
         reached the caller through conway::SessionHandle::prompt -- not merely that the \
         factory was invoked (an invoked-and-discarded factory produces the same call count)"
    );
}
