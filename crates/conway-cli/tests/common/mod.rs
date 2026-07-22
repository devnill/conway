//! Shared test harness for the one-shot integration suite (WI-113): the
//! [`mock_backend`] module plus [`run_conway`]/[`spawn_conway`], which
//! template `fixtures/conway.toml.tmpl` into a fresh `TempDir` pointed at a
//! live [`mock_backend::MockHandle`] and drive the real compiled `conway`
//! binary against it.

pub mod mock_backend;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use mock_backend::MockHandle;

const TEMPLATE: &str = include_str!("../fixtures/conway.toml.tmpl");

/// A fresh temp directory holding a rendered `conway.toml`, plus the path
/// to that file. Kept alive for as long as a test needs the config/session
/// store to exist on disk.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub config_path: PathBuf,
}

/// Renders [`TEMPLATE`] against `mock` and `max_steps`, writing the result
/// into a fresh `TempDir`'s `conway.toml`.
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
    let config_path = dir.path().join("conway.toml");
    let mut f = std::fs::File::create(&config_path).expect("create conway.toml");
    f.write_all(rendered.as_bytes()).expect("write conway.toml");

    // `ConwayBuilder::build`'s `CapabilityIndex` is populated *only* from
    // `config.models.metadata_path` (default `.conway/models.json`) -- it
    // is a wholly separate store from `conway_backends`' own bundled
    // dialect-default metadata, so a model this facade-local file does not
    // name is an `unknown (backend, model) pair` the router rejects
    // (`CapabilitySkip`) before ever dialing a backend
    // (`conway-routing/src/router.rs`'s `check_candidate`). Every fixture
    // therefore declares its own mock model here, matching the `backend/model`
    // chain string `fixtures/conway.toml.tmpl` renders.
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
