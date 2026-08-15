//! The shortest path this crate has from `cargo add conway` to a model's
//! answer: discover ambient configuration, inject the two things discovery
//! cannot know on a caller's behalf, and prompt.
//!
//! ```console
//! cargo run -p conway --example discover_getting_started
//! ```
//!
//! ## Why this reads so differently from `bare_inference.rs`
//!
//! That example hand-builds a `ConwayConfig` struct literal -- naming all
//! fourteen fields -- via `ConwayBuilder::from_parts`, and its own doc
//! explains exactly why nothing shorter was reachable from THAT entry point:
//! `ConwayConfig` has no `Default`. What that example does not do is call
//! [`conway::ConwayBuilder::discover`], which already exists, already
//! layers a documented, inspectable, built-in default over every one of
//! those fourteen fields (`config::merge::default_document`, reachable
//! through the same five-source precedence chain -- default < XDG < project
//! < env < CLI -- a real host's settings file/environment/CLI flags flow
//! through), and gets a host to a first answer in well under half the
//! lines, with zero struct-literal ceremony -- measured directly, not
//! estimated: an equivalent `from_parts`-based "before" and this file's own
//! "after," each compiled and run as their own crate outside this
//! workspace (depending on nothing but a published `conway`), come to 66
//! and 33 lines respectively (55 and 23 excluding blank lines and `use`
//! statements) for the identical bare-inference outcome. This example is
//! that "after" path, actually exercised in this workspace too, not just
//! described. `docs/embedding.md` opens with exactly this screenful.
//!
//! ## What's still explicit, and why
//!
//! Two things `discover()` deliberately does NOT supply on your behalf,
//! because guessing either for you would be exactly the "guesses silently"
//! failure mode this crate's builder is not allowed to have:
//!
//! - **A [`conway::Backend`].** Nobody may be silently billed for a
//!   provider they never named -- so there is no compiled-in fallback
//!   backend, ever. [`conway::ConwayBuilder::with_backend`] injects one;
//!   here it's `conway_testkit::FakeBackend` so this example stays fully
//!   offline, like this crate's other examples (see
//!   `real_provider_inference.rs` for the real-backend shape of this same
//!   screenful).
//! - **Where to route.** The default document's baked-in role
//!   (`default_role = "coder"`, an empty chain) deliberately names no
//!   destination -- see `conway::ConwayBuilder`'s own module doc for why
//!   `default_role` has no opinion worth inventing at the core. A caller who
//!   already knows exactly which backend/model to use (the common embedding
//!   case) bypasses role/chain resolution entirely with
//!   [`conway::ConwayBuilder::with_router`], exactly as below.
//!
//! Everything else below `discover()` -- session storage, limits, tool
//! registration, headroom -- comes from its own layered defaults, or from
//! the two lightweight PER-FIELD overrides used here
//! ([`conway::config::CliOverrides`], [`conway::PluginSelection`]) rather
//! than a second, competing construction path: both are existing
//! `ConwayBuilder`/`config` mechanisms, not new API this item added.
//!
//! ## A note on what "discover" reads here
//!
//! This example genuinely calls [`conway::ConwayBuilder::discover`] -- the
//! real entry point a host application uses, which reads THIS process's own
//! `$XDG_CONFIG_HOME`/`~/.conway/settings.json` and walks up from its
//! current directory looking for a project `.conway/settings.json`. Left
//! alone, that means this example's output would depend on whatever happens
//! to be configured on the machine running it -- fine for a real host (it
//! WANTS its own ambient configuration), wrong for an example every reader
//! should be able to run and get the identical result from. `main` below
//! points `XDG_CONFIG_HOME` and the process `cwd` at fresh, empty scratch
//! directories before calling `discover()`, purely so this stays
//! reproducible -- `discover()` itself is not special-cased or mocked, it
//! is simply given nothing to find, the same "no ambient config" case
//! `tests/discover_getting_started_example_smoke.rs` also covers (that
//! test cannot call `discover()` directly at all -- see its own doc for why
//! `crates/conway/tests/config_isolation_guard.rs` forbids it in-process --
//! so it drives `config::load` with an isolated `LoadOptions` instead, the
//! same outcome this example's own isolation reaches by a different route).
//! A real embedder does none of this -- it just calls `discover()`.

use std::sync::Arc;

use conway::backend::{BackendId, ModelId};
use conway::config::CliOverrides;
use conway::{ConwayBuilder, ModelRef, PluginSelection, SessionSpec};
// `conway_testkit` is a normal, unconditional dev-dependency of this crate
// (`crates/conway/Cargo.toml`'s `[dev-dependencies]`) -- unlike
// `conway::testkit` (this crate's OWN re-export of the same crate, gated
// behind its `testkit` cargo feature for a THIRD PARTY to reach), so this
// example runs with `cargo run -p conway --example discover_getting_started`
// alone, no extra `--features` needed, matching every other example in this
// crate.
use conway_testkit::{FakeBackend, FakeRouter, FakeStore};

/// Points this process's own `$XDG_CONFIG_HOME` and current directory at
/// fresh, empty scratch directories, so the `ConwayBuilder::discover()`
/// call below deterministically finds nothing regardless of the machine
/// running this example -- see this module's own doc for why. A real host
/// application does not do this.
fn isolate_ambient_config_for_this_example() {
    let scratch = std::env::temp_dir().join(format!(
        "conway-discover-getting-started-example-{}",
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
    isolate_ambient_config_for_this_example();

    let backend = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let route = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    };

    let conway = ConwayBuilder::discover()?
        // Per-field overrides over discover()'s own defaults -- not a
        // second config. `permission_mode = "deny"` lets this build without
        // a permission gate at all (discover()'s default, "prompt", needs
        // one -- see `ConwayBuilder::with_prompt_handler`'s own doc for the
        // real-embedding alternative to this offline example's shortcut).
        // `PluginSelection::None` mirrors it: no tools registered, nothing
        // for "deny" to ever have to refuse.
        .with_cli_overrides(CliOverrides {
            permission_mode: Some("deny".to_string()),
            ..CliOverrides::default()
        })
        .with_builtin_plugins(PluginSelection::None)
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_session_store(Arc::new(FakeStore::new()))
        .build()?;

    let session = conway.new_session(SessionSpec::default()).await?;
    let turn = session.prompt("Hello, conway!").await?;
    println!("prompt -> {}", turn.text().await?);
    let _ = turn.result().await?;

    Ok(())
}
