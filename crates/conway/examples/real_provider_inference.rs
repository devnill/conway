//! Inference against a REAL provider -- the same screenful
//! `discover_getting_started.rs` runs, with its `FakeBackend` replaced by a
//! genuinely constructed [`conway_plugin_backends::openai_compat::
//! OpenAiCompatBackend`] wired to a real, running OpenAI-compatible
//! endpoint (a local Ollama/vLLM/llama.cpp server, or a hosted one).
//!
//! ```console
//! CONWAY_EXAMPLE_BASE_URL=http://localhost:11434/v1 \
//! CONWAY_EXAMPLE_MODEL=llama3 \
//! cargo run -p conway --example real_provider_inference
//! ```
//!
//! **This is the one example in this crate that is NOT safe to run
//! unattended.** Every other example here is fully offline; this one makes
//! a real network call to wherever you point it, and does so ONLY if you
//! set `CONWAY_EXAMPLE_BASE_URL` -- absent it, `main` prints instructions
//! and exits successfully without touching the network at all, so `cargo
//! build --examples`/CI compiling this file never triggers one.
//!
//! **No credential is ever hardcoded, logged, or printed here.**
//! `CONWAY_EXAMPLE_API_KEY`, when set, is read once from the environment
//! and handed straight to [`SecretString`] (whose own `Debug` impl never
//! reveals its value -- `conway_plugin_backends::config`'s own test pins
//! this), and this file never formats or otherwise surfaces it. This is the
//! one place in this item's examples where a real key COULD leak into a
//! transcript if that discipline slipped, so it is stated here explicitly
//! rather than left implicit.
//!
//! ## What's real here, and what's still a stand-in
//!
//! The [`Backend`](conway::Backend) is real: a genuine
//! `OpenAiCompatBackend`, constructed the same way
//! `ConwayBuilder::with_backend_factory`'s own `[backends.<id>]` resolution
//! path builds one internally, just called directly here rather than
//! through a `[backends.<id>]` config entry -- see
//! `docs/embedding.md`'s "Installing a backend" section for the
//! config-driven form of the same construction.
//!
//! Routing is still a stand-in: `conway_testkit::FakeRouter::single`,
//! pointed at the one real backend/model this example just built --
//! `discover_getting_started.rs`'s own doc explains why role/chain
//! resolution is bypassed the same way there. Role-based routing over a
//! real fallback chain is a solved, separate concern
//! ([`docs/routing.md`](../../../docs/routing.md)), out of scope for a
//! minimal "does a real backend actually answer" example.

use std::sync::Arc;

use conway::backend::{BackendId, ModelId};
use conway::{ConwayBuilder, ConwayError, ModelRef, PermissionDecision, SessionSpec};
use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig, SecretString};
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use conway_testkit::{FakeGate, FakeRouter, FakeStore};

/// See `discover_getting_started.rs`'s own doc/copy of this same helper.
/// Applied here too, AFTER the early-return below: this example's own
/// `[backends.<id>]` entry is injected directly via `with_backend`, but
/// `ConwayBuilder::discover()` still reads whatever `[backends.<id>]`
/// entries the machine's OWN ambient `settings.json` declares, and `build()`
/// fails naming an unrecognized `kind` for any of those this facade has no
/// registered `BackendFactory` for (this example registers none -- it
/// injects one already-built `Backend` instead) -- unrelated to, and
/// independent of, whether the real call this example makes itself
/// succeeds. Isolating ambient config keeps this example's own outcome
/// scoped to the one backend it just built, on every machine.
fn isolate_ambient_config_for_this_example() {
    let scratch = std::env::temp_dir().join(format!(
        "conway-real-provider-inference-example-{}",
        std::process::id()
    ));
    let xdg = scratch.join("xdg-config-home");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&xdg).expect("create scratch XDG_CONFIG_HOME");
    std::fs::create_dir_all(&cwd).expect("create scratch cwd");
    std::env::set_var("XDG_CONFIG_HOME", &xdg);
    std::env::set_current_dir(&cwd).expect("set scratch cwd");
}

#[tokio::main]
async fn main() -> conway::Result<()> {
    let Ok(base_url) = std::env::var("CONWAY_EXAMPLE_BASE_URL") else {
        println!(
            "This example makes a REAL network call and is opt-in only. Set:\n\
             \n\
             \x20\x20CONWAY_EXAMPLE_BASE_URL   an OpenAI-compatible endpoint, e.g. \
             http://localhost:11434/v1\n\
             \x20\x20CONWAY_EXAMPLE_MODEL      the model name that endpoint serves\n\
             \x20\x20CONWAY_EXAMPLE_API_KEY    (optional) a credential, if the endpoint needs one\n\
             \n\
             then run again: `cargo run -p conway --example real_provider_inference`.\n\
             Doing nothing further -- no network call was made."
        );
        return Ok(());
    };
    isolate_ambient_config_for_this_example();
    let model_name = std::env::var("CONWAY_EXAMPLE_MODEL").map_err(|_| ConwayError::Config {
        path: None,
        message: "CONWAY_EXAMPLE_MODEL must also be set alongside CONWAY_EXAMPLE_BASE_URL"
            .to_string(),
    })?;
    // Read once, handed straight to `SecretString` -- never formatted,
    // printed, or logged anywhere in this file (see this module's own doc).
    let api_key = std::env::var("CONWAY_EXAMPLE_API_KEY").ok();

    let backend_id = BackendId::new("real");
    let cfg = OpenAiCompatConfig {
        id: backend_id.clone(),
        base_url: base_url.parse().map_err(|e| ConwayError::Config {
            path: None,
            message: format!("CONWAY_EXAMPLE_BASE_URL is not a valid URL: {e}"),
        })?,
        api_key: api_key.map(SecretString::new),
        profile: Dialect::OpenAi.profile(),
        timeout: None,
        metadata_path: None,
        models: Default::default(),
    };
    let backend = Arc::new(
        OpenAiCompatBackend::new(cfg).map_err(|e| ConwayError::Build {
            message: format!("failed to construct the real backend: {e}"),
        })?,
    );
    let route = ModelRef {
        backend: backend_id,
        model: ModelId::new(model_name),
    };

    let conway = ConwayBuilder::discover()?
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(Arc::new(FakeStore::new()))
        .build()?;

    let session = conway.new_session(SessionSpec::default()).await?;
    let turn = session.prompt("Say hello in exactly three words.").await?;
    println!("prompt -> {}", turn.text().await?);
    let _ = turn.result().await?;

    Ok(())
}
