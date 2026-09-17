//! CLI-level acceptance test for board item `01M2PJM777FFSZW8GZWD46X49B`:
//! two configured MCP servers that independently succeed at their own
//! handshake but happen to declare the SAME tool name used to abort the
//! whole `conway` process with a raw Rust panic
//! (`crates/conway-runtime/src/runtime.rs:410`'s own `.expect()`, before
//! this item), even though `PluginRegistry::from_plugins` already built a
//! correctly-named error. `crates/conway-runtime/tests/
//! duplicate_tool_name_propagation.rs` proves the fix in-process, against
//! `Runtime::try_new` directly; this file is the end-to-end proof that the
//! real compiled binary, driven exactly the way an operator would hit this
//! (two entries in `[plugins].mcp[]`), now exits cleanly with a named error
//! instead of aborting.
//!
//! P-15's own bar: "two servers, same tool name, clean named error and
//! non-panicking exit. Fails against HEAD by panicking." -- this file's
//! one test fails against HEAD exactly that way: the subprocess aborts
//! with a Rust panic backtrace on stderr and exit code 101, rather than the
//! ordinary `conway: error: ...` line and exit code 2 asserted below.
//!
//! Uses `routes explain` (never `-p`/a live backend) deliberately -- the
//! duplicate-tool collision is detected inside `ConwayBuilder::build`,
//! downstream of BOTH servers' own successful discovery, so it reaches
//! every dispatch target identically, introspection-only ones included
//! (contrast board item `01M2PJCT90G2010KGCJ4YSFREM`'s own fix, which only
//! ever tolerates a server that fails to START -- a categorically different
//! condition from two servers that both started fine and collided).

#[allow(dead_code)]
mod common;

// A sibling top-level module, not nested inside `common` -- see
// `dogfood_routes_and_status.rs::fixture_with_unverified_floor`'s own doc
// for the identical `mcp_fixtures` inclusion and per-instance script
// patching idiom this file follows.
#[path = "common/mcp_fixtures.rs"]
mod mcp_fixtures;

/// Two entries, each a patched copy of `mcp_fixtures::SLEEP_SERVER` with
/// its own DISTINCT advertised `serverInfo.name` (so the two attach as
/// distinct plugins, `mcp.dogfood-mcp-0`/`mcp.dogfood-mcp-1`, rather
/// than colliding on `duplicate plugin id` first) but the SAME,
/// UNPATCHED, declared tool name `"sleep"` -- the one difference from
/// `fixture_with_unverified_floor`'s own per-index patching, which
/// deliberately patches the tool name too so its servers do NOT collide.
/// This is the exact "distinct server names, same tool name" shape the
/// item's own file-fence note describes.
fn fixture_with_colliding_tool_name() -> common::Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime for MCP script warmup");
    let mcp: Vec<serde_json::Value> = (0..2)
        .map(|i| {
            let id = format!("dogfood-mcp-{i}");
            let script_src = mcp_fixtures::SLEEP_SERVER.replace(
                "\"name\": \"dogfood-sleep\"",
                &format!("\"name\": \"{id}\""),
            );
            let script_path =
                mcp_fixtures::write_script(dir.path(), &format!("{id}.py"), &script_src);
            // Board item 01M09MPZ9C188AHNBKWEJ3CEQA: warm the freshly-
            // written script once before the real, timed install-time
            // handshake -- every other `mcp_fixtures` consumer in this
            // crate follows the same precedent.
            rt.block_on(mcp_fixtures::warm(&script_path));
            serde_json::json!({
                "id": id,
                "command": [script_path.display().to_string()],
            })
        })
        .collect();
    let config = serde_json::json!({
        "default_role": "default",
        "limits": { "max_steps": 5 },
        "backends": {
            "unreachable": {
                "kind": "openai-compat",
                "base_url": "http://127.0.0.1:1/v1",
                "dialect": "openai"
            }
        },
        "roles": {
            "default": { "chain": ["unreachable/does-not-matter"] }
        },
        "plugins": { "mcp": mcp }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");
    common::Fixture { dir, config_path }
}

#[test]
fn two_mcp_servers_with_the_same_tool_name_exit_cleanly_instead_of_panicking() {
    let fixture = fixture_with_colliding_tool_name();

    let out = common::run_conway(&["routes", "explain", "default"], &fixture);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "a duplicate tool name across two MCP plugins must still fail the build"
    );
    // The headline claim: NOT a Rust panic. A panicking process exits 101
    // under this workspace's default (unwinding, non-`panic=abort`) panic
    // runtime and prints "thread 'main' panicked at" to stderr; this must
    // be neither.
    assert_ne!(
        out.status.code(),
        Some(101),
        "must not exit with the raw Rust panic exit code; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "must not print a Rust panic backtrace; stderr: {stderr}"
    );
    // `FacadeError::Build` -> `ExitCode::Usage` (2) -- the SAME exit code
    // the neighbouring "duplicate plugin id" collision already produces
    // (both are `FacadeError::Build`, raised a few dozen lines apart in
    // `ConwayBuilder::build`).
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected the ordinary Usage exit code for a build-time refusal, got: {out:?}"
    );

    // Structural where possible (P-15/GP-14's own instruction): the exit
    // code and the "not a panic" assertions above are already structural.
    // The message CONTENT has no structured surface at the CLI layer (a
    // `FacadeError::Build`'s `message` is free text, not JSON), so the
    // remaining checks are necessarily `contains` -- matching this item's
    // own instruction to route the error through "the same path and shape
    // as the existing duplicate plugin id error": the same wording that
    // error already uses (`crates/conway/src/builder.rs`'s own `format!
    // ("duplicate plugin id: '{id}'")`), one layer down naming the tool.
    assert!(
        stderr.contains("conway: error:"),
        "expected the standard diag::error prefix, got: {stderr:?}"
    );
    assert!(
        stderr.contains("duplicate tool"),
        "expected the collision to be named as a duplicate TOOL (not just \
         re-using the duplicate PLUGIN id wording), got: {stderr:?}"
    );
    assert!(stderr.contains("sleep"), "expected the colliding tool's own name: {stderr:?}");
    assert!(
        stderr.contains("mcp.dogfood-mcp-0") && stderr.contains("mcp.dogfood-mcp-1"),
        "expected BOTH colliding plugins' own derived ids (`mcp.<serverInfo.name>`) named, got: \
         {stderr:?}"
    );
}
