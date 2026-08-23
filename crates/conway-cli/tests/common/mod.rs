//! Shared test harness for the one-shot integration suite: the
//! [`mock_backend`] module plus [`run_conway`]/[`spawn_conway`], which
//! template `fixtures/conway.json.tmpl` into a fresh `TempDir` pointed at a
//! live [`mock_backend::MockHandle`] and drive the real compiled `conway`
//! binary against it.

pub mod mock_backend;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use conway::config::{CliOverrides, LoadOptions};
use conway::gates::AllowListGate;
use conway::{Conway, ConwayBuilder, PermissionGate};
use mock_backend::MockHandle;

/// The template's `backends.mock.dialect` is `"openai"`, not `"ollama"`:
/// `Dialect::OpenAi`'s default `tool_calling` is `Streaming{validated:true}`,
/// so `AttemptEngine::strategy_for` always picks the streaming path -- both
/// with and without tools in the request. Every root session in this suite
/// has the full builtin toolset available (`has_tools == true`), and
/// `Dialect::Ollama`'s `NonStreamingOnly` default would force the
/// *non*-streaming `generate()` path (a single JSON object, not SSE) for
/// every one of those requests -- a wire shape `MockBackend` does not
/// speak. Using `openai` here keeps every request in this suite on the one
/// wire format the mock implements.
///
/// This fixture's own role is likewise given a valid chain, because
/// routing validation rejects an empty chain on ANY role, not just the one
/// named by `default_role` -- see `cli_surface.rs::MINIMAL_CONFIG`'s
/// identical note. Without it, `build()` fails with EmptyChain before
/// dispatch ever reaches one-shot mode. (`default_document()` bakes in an
/// empty-chain role of its own at the lowest merge layer; it is named
/// `default`, not `coder`, since the shipped default stopped naming one use
/// of the harness.)
const TEMPLATE: &str = include_str!("../fixtures/conway.json.tmpl");

/// A fresh temp directory holding a rendered `conway.json`, plus the path
/// to that file. Kept alive for as long as a test needs the config/session
/// store to exist on disk.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub config_path: PathBuf,
}

/// Renders [`TEMPLATE`] against `mock` and `max_steps`, writing the result
/// into a fresh `TempDir`'s `conway.json`.
pub fn write_fixture(mock: &MockHandle, max_steps: u32) -> Fixture {
    write_fixture_with(&mock.base_url, &mock.model, max_steps)
}

/// As [`write_fixture`], but from a bare `base_url`/`model` rather than a
/// live [`MockHandle`] -- for tests that need the config to name a backend
/// address after the mock behind it has already been dropped (e.g.
/// `exit_4_no_backend`'s "mock refuses connections" scenario).
pub fn write_fixture_with(base_url: &str, model: &str, max_steps: u32) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let rendered = TEMPLATE
        .replace("{{BASE_URL}}", base_url)
        .replace("{{MODEL}}", model)
        .replace("{{MAX_STEPS}}", &max_steps.to_string());
    let config_path = dir.path().join("conway.json");
    let mut f = std::fs::File::create(&config_path).expect("create conway.json");
    f.write_all(rendered.as_bytes()).expect("write conway.json");

    // `ConwayBuilder::build`'s `CapabilityIndex` is populated *only* from
    // `config.models.metadata_path` (default `.conway/models.json`) -- it
    // is a wholly separate store from `conway_plugin_backends`' own bundled
    // dialect-default metadata, so a model this facade-local file does not
    // name is an `unknown (backend, model) pair` the router rejects
    // (`CapabilitySkip`) before ever dialing a backend
    // (`conway-routing/src/router.rs`'s `check_candidate`). Every fixture
    // therefore declares its own mock model here, matching the `backend/model`
    // chain string `fixtures/conway.json.tmpl` renders.
    let models_dir = dir.path().join(".conway");
    std::fs::create_dir_all(&models_dir).expect("create .conway dir");
    let models_json = serde_json::json!({
        "models": {
            format!("mock/{model}"): {
                "max_context_tokens": 128_000,
                "tool_calling": "streaming_validated",
                "reasoning": false,
                "reliability_tier": "verified",
            }
        }
    });
    std::fs::write(
        models_dir.join("models.json"),
        serde_json::to_vec(&models_json).expect("serialize models.json"),
    )
    .expect("write models.json");

    Fixture { dir, config_path }
}

/// Builds (but does not run) the real `conway` binary's `Command`, with
/// `--config` pointing at `fixture` and the process cwd set to `fixture`'s
/// temp dir. `CONWAY_CONFIG_DIR` is ALSO pointed at that same temp dir
/// (immediately below), which -- since neither `TEMPLATE` nor any fixture
/// mutation in this suite names `[session].root` -- puts the central,
/// project-keyed default entirely inside the fixture too (`config::
/// discovery::session_root`'s own doc); see [`session_dir`] for the exact
/// path a test asserting against it should compute.
pub fn command(args: &[&str], fixture: &Fixture) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("conway"));
    cmd.current_dir(fixture.dir.path())
        // Test isolation: point the user-scoped config discovery
        // (`$CONWAY_CONFIG_DIR/settings.json`) at the fixture's own temp
        // dir, which has no such file, so a real `~/.conway/settings.json` on
        // the developer's machine can never merge into and corrupt the
        // fixture config these tests build. Since board item
        // `01M0QK9GRM8HSNWRAR414TCX42`, this ALSO keeps `[session].root`'s
        // now-central default inside the fixture -- see [`session_dir`].
        .env("CONWAY_CONFIG_DIR", fixture.dir.path())
        .arg("--config")
        .arg(&fixture.config_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    cmd
}

/// Runs the real `conway` binary to completion and returns its captured
/// stdout/stderr/status. For tests that need to interact with the process
/// while it is still running (read a line before it exits, send a signal),
/// build a `Command` via [`command`] and `.spawn()` it directly instead.
pub fn run_conway(args: &[&str], fixture: &Fixture) -> Output {
    command(args, fixture).output().expect("run conway binary")
}

/// The directory the real binary actually writes `fixture`'s sessions
/// into, now that `[session].root`'s unconfigured default is the central,
/// project-keyed root (board item `01M0QK9GRM8HSNWRAR414TCX42`) rather than
/// a bare `<cwd>/.conway/sessions`. Reuses `conway::config::discovery::
/// session_root` -- the exact function `config::load` calls in the real
/// binary -- rather than re-deriving the project-key encoding by hand here,
/// so this helper can never silently drift from what the binary actually
/// does.
///
/// Mirrors [`command`]'s own env/cwd choice: `CONWAY_CONFIG_DIR` and the
/// subprocess's cwd are BOTH `fixture.dir.path()`, so the key is computed
/// against that same path.
///
/// `#[allow(dead_code)]` for the same reason `mock_backend::Chunk` carries
/// one: each `tests/*.rs` integration file compiles this module fresh as
/// its own independent crate, so a file that only needs [`open_conway`]
/// (or neither) makes this look unused *for that one binary*, even though
/// other suites in this same directory do call it.
#[allow(dead_code)]
pub fn session_dir(fixture: &Fixture) -> PathBuf {
    let mut env = std::collections::HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        fixture.dir.path().to_string_lossy().into_owned(),
    );
    // The project KEY has to be computed from the SAME path the subprocess
    // itself used: `LoadOptions.cwd` there is `std::env::current_dir()`
    // (`getcwd(3)`), which -- unlike a bare path string -- resolves every
    // symlink in the path. `fixture.dir.path()` alone is `tempfile`'s
    // UNRESOLVED return value, and on macOS `$TMPDIR` sits under
    // `/var/folders/...`, itself a symlink to `/private/var/folders/...`:
    // the two spellings name the same directory but encode to DIFFERENT
    // project keys ([`conway::config::discovery::encode_project_key`]
    // never resolves symlinks -- production doesn't need to, since
    // `std::env::current_dir()` already has by the time it gets there).
    // Canonicalizing here is what makes this helper predict the identical
    // path the subprocess actually wrote to.
    let project_dir = fixture
        .dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| fixture.dir.path().to_path_buf());
    conway::config::discovery::session_root(&project_dir, None, &env)
}

/// Opens a fresh, read-only [`Conway`] against `fixture`'s on-disk session
/// store -- the same store the compiled binary's subprocess (`command`/
/// `run_conway`) wrote to. Shared by every test file that used to carry its
/// own byte-identical copy of this helper (`continuity.rs`,
/// `plugin_subcommand.rs`, `oneshot_ask.rs`, `oneshot_persona_and_budget.rs`
/// all cross-referenced `continuity.rs::open_conway` in their own docs
/// before this item factored the one implementation out here).
///
/// **Uses `ConwayBuilder::from_options_ignoring_user_config`, not
/// `ConwayBuilder::from_config_only`** -- the one load-bearing difference
/// from the pre-this-item version, and the reason this helper moved at
/// all. `from_config_only` always builds its own `LoadOptions` via
/// `LoadOptions::default()` (`cwd: std::env::current_dir()`, `env:
/// std::env::vars()` -- both this TEST PROCESS's real, ambient values, not
/// the fixture's), then relies on a LATER `CliOverrides.cwd` (applied at
/// `build()` time, via `apply_cli`) to fix up every path that resolves
/// against `config.cwd`. That still works for `agents.dir`/
/// `models.metadata_path` (both resolved at `build()` time, after
/// `apply_cli`), which is why the old code appeared to work at all. It does
/// NOT work for `[session].root`'s central-default resolution (board item
/// `01M0QK9GRM8HSNWRAR414TCX42`): that happens INSIDE `config::load`/
/// `load_ignoring_user_config` itself, using `LoadOptions.cwd`/`.env`
/// directly -- a later `CliOverrides.cwd` is too late to change it, and
/// worse, this test process's real ambient `env` has no `CONWAY_CONFIG_DIR`
/// set, so the OLD code path would have resolved the central default
/// against this developer's REAL `~/.conway`, not the fixture -- exactly
/// the hazard `CONWAY_CONFIG_DIR` isolation exists to prevent elsewhere in
/// this same file. `from_options_ignoring_user_config` (added by this same
/// item) is the seam that lets this helper hand `config::load_ignoring_
/// user_config` its OWN `cwd`/`env`, matching [`command`]'s own subprocess
/// env exactly, so this in-process helper opens the IDENTICAL store the
/// subprocess wrote to, with no real ambient state involved anywhere --
/// unlike hand-inlining the same `config::load_ignoring_user_config` +
/// `ConwayBuilder::from_parts` calls here directly would, `Conway::
/// warnings()` still comes back populated, since `from_options_ignoring_
/// user_config` attaches them the same way every other `ConwayBuilder`
/// loader constructor does.
///
/// `#[allow(dead_code)]` -- see [`session_dir`]'s own doc for why: not
/// every consuming test binary in this directory calls this one.
#[allow(dead_code)]
pub async fn open_conway(fixture: &Fixture) -> Conway {
    let gate: Arc<dyn PermissionGate> = Arc::new(AllowListGate::new(Vec::new(), Vec::new()));
    let mut env = std::collections::HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        fixture.dir.path().to_string_lossy().into_owned(),
    );
    // Canonicalized for the identical reason [`session_dir`] canonicalizes:
    // the subprocess's own `LoadOptions.cwd` came from `std::env::
    // current_dir()` (`getcwd(3)`, symlink-resolving), while `fixture.dir.
    // path()` alone is `tempfile`'s unresolved return value -- on macOS,
    // `$TMPDIR` sits under `/var/folders/...`, itself a symlink to
    // `/private/var/folders/...`. Passing the unresolved spelling here
    // would compute a DIFFERENT project key than the subprocess used,
    // opening an empty store that just happens not to error (`open`
    // creates whatever directory it's pointed at).
    let cwd = fixture
        .dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| fixture.dir.path().to_path_buf());
    ConwayBuilder::from_options_ignoring_user_config(LoadOptions {
        cwd: cwd.clone(),
        explicit_path: Some(fixture.config_path.clone()),
        env,
        // `config.cwd` (the FIELD, default `"."`) is what `agents.dir`/
        // `models.metadata_path` resolve relative paths against at
        // `build()` time -- unrelated to `LoadOptions.cwd` immediately
        // above, which only steers discovery and `[session].root`'s
        // resolution. Set here, at LOAD time, rather than via a separate
        // `ConwayBuilder::with_cli_overrides` call after: `apply_cli` runs
        // again at `build()` time regardless, so setting it once here and
        // leaving the builder's own `cli_overrides` at its default (a
        // no-op re-merge) is equivalent and one fewer step.
        cli_overrides: CliOverrides {
            cwd: Some(cwd),
            ..Default::default()
        },
        model_metadata_refresh: false,
    })
    .expect("load fixture config")
    .with_permission_gate(gate)
    // `conway` no longer compiles the fixture template's
    // `kind = "openai-compat"` entry in -- the same factory `main.rs`'s
    // own `build_conway` attaches by default, registered explicitly
    // here since this helper builds a `Conway` directly rather than
    // going through that choke point.
    .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
    .build()
    .expect("build conway against the fixture's own store")
}
