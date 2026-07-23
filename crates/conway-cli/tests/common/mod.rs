//! Shared test harness for the one-shot integration suite (WI-113): the
//! [`mock_backend`] module plus [`run_conway`]/[`spawn_conway`], which
//! template `fixtures/conway.json.tmpl` into a fresh `TempDir` pointed at a
//! live [`mock_backend::MockHandle`] and drive the real compiled `conway`
//! binary against it.

pub mod mock_backend;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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
/// `roles.coder` is likewise given a valid chain: `default_document()`
/// bakes in a `roles.coder = { chain = [] }` at the lowest merge layer, and
/// routing validation rejects an empty chain on ANY role, not just
/// `default_role` -- see `cli_surface.rs::MINIMAL_CONFIG`'s identical note
/// (F-111-1). Without it, `build()` fails with EmptyChain before dispatch
/// ever reaches one-shot mode.
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
    // is a wholly separate store from `conway_backends`' own bundled
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
/// temp dir (so `session.root`'s relative default resolves inside it).
pub fn command(args: &[&str], fixture: &Fixture) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("conway"));
    cmd.current_dir(fixture.dir.path())
        // Test isolation: point the user-scoped config discovery
        // (`$XDG_CONFIG_HOME/conway/settings.json`) at the fixture's own temp
        // dir, which has no such file, so a real `~/.conway/settings.json` on
        // the developer's machine can never merge into and corrupt the
        // fixture config these tests build.
        .env("XDG_CONFIG_HOME", fixture.dir.path())
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
