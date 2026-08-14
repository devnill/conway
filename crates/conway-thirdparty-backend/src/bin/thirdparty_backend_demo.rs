//!'s compiled-binary demonstration:
//! byte-identical wiring to `tests/end_to_end.rs`'s library-embedder
//! proof, but run as a genuinely separate compiled process rather than a
//! function call inside this crate's own `cargo test` binary -- "the same
//! capability... demonstrated from the compiled binary as well as from the
//! library, so it is not trapped in one mode" (the item's own acceptance
//! criterion).
//!
//! A third-party `Backend` author never gets to run code inside `conway`'s
//! own compiled binary -- there is no dynamic-loading mechanism anywhere
//! in this tree (`grep -rln "libloading\|dlopen\|dylib"` over every
//! crate's `src/` and `Cargo.toml` returns nothing). Their compiled-binary
//! proof is necessarily their OWN binary, exactly what this one stands in
//! for: it renders a real `settings.json`, loads it through
//! `conway::config::load`, installs `ThirdPartyBackendFactory` through
//! `ConwayBuilder::with_backend_factory`, completes one real turn, and
//! prints the turn's own text to stdout, then exits `0`.
//! `tests/binary.rs` runs this compiled binary via `assert_cmd` and
//! asserts on its captured stdout.
//!
//! `[[bin]]` targets under `src/bin/` receive this crate's own
//! `[dependencies]` only (never `[dev-dependencies]` -- Cargo does not
//! link those into a plain binary target), which is why
//! `conway_thirdparty_backend::{build_conway, fixture}` (this crate's own
//! `[dependencies]`-only helpers) are what this file calls, not
//! `tempfile`/`assert_cmd` from `[dev-dependencies]`.

fn main() {
    let dir = conway_thirdparty_backend::fixture::fresh_dir("binary-demo");
    let config_path = conway_thirdparty_backend::fixture::write_settings(&dir);

    let conway = conway_thirdparty_backend::build_conway(&dir, &config_path)
        .expect("build should succeed: a settings.json-derived thirdparty-stub backend");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build a throwaway current-thread tokio runtime");

    let text = runtime.block_on(async {
        let handle = conway
            .new_session(conway::SessionSpec::default())
            .await
            .expect("new_session should succeed");
        let turn = handle.prompt("hi").await.expect("prompt should succeed");
        turn.text().await.expect("turn should succeed")
    });

    // The library test (`tests/end_to_end.rs`) asserts equality against
    // `REPLY_TEXT` directly; this binary prints the raw text instead and
    // leaves the assertion to `tests/binary.rs`'s `assert_cmd` caller --
    // exactly what a real compiled embedder's own binary would do (produce
    // output, let something else outside the process check it).
    println!("{text}");
}
