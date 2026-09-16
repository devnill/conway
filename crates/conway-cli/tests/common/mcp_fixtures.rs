// Compiled fresh into every `tests/*.rs` binary that reaches for it --
// looks like dead code from every OTHER binary's own point of view. See
// `common::mock_backend`'s own module doc for the identical per-binary
// blindness this crate's shared harness already lives with.
#![allow(dead_code)]

//! Gate 8 (`01M1ZJRTKB2JRTYC03SFY8G2BH`) fixture helpers: hand-written
//! Python 3 stdio MCP servers, written into a fresh temp dir at run time --
//! never a pre-existing binary this repo ships, and never a path built from
//! untrusted input. Modeled DIRECTLY on `crates/conway-plugin-mcp/tests/
//! common/mod.rs`'s own fixtures (read-only reference per this item's own
//! instructions -- that crate's `tests/` directory is not importable from
//! here, each integration-test binary is its own independent crate), with
//! the same wire shapes: NDJSON stdio, `initialize`/
//! `notifications/initialized`/`tools/list`/`tools/call`, MCP 2024-11-05.
//!
//! Also builds the `conway.json` a CLI-level test needs to actually reach
//! one of these servers: an ordinary `[backends.mock]` (a real,
//! script-driven LLM, via `common::mock_backend`) PLUS one `[plugins].
//! mcp[]` entry naming the Python script -- `crates/conway/src/config/
//! schema.rs::McpPluginEntry`'s own shape (`id`, `command`, `timeout_ms`,
//! `first_call_timeout_ms`, `env`).

use std::io::Write as _;
use std::path::{Path, PathBuf};

// This file is included as a sibling top-level module via `#[path = "..."]`
// (see the consuming test's own `mod mcp_fixtures;` declaration) rather
// than nested inside `common` (`common/mod.rs` is out of this writer's
// fence) -- `crate::common::Fixture` reaches the crate-root `common` module
// from here for exactly that reason; a bare `common::Fixture` would not
// resolve (this module is common's SIBLING, not a child of it).
use crate::common::Fixture;

/// Writes `contents` to `<dir>/<name>`, marks it executable (unix). Returns
/// the path a `McpPluginEntry.command` should name.
pub fn write_script(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create fixture MCP script");
    f.write_all(contents.as_bytes())
        .expect("write fixture MCP script");
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x fixture MCP script");
    }
    path
}

/// One `[plugins].mcp[]` entry this helper's fixture builder writes.
pub struct McpEntryCfg {
    pub id: &'static str,
    pub command: Vec<String>,
    pub timeout_ms: u64,
    pub first_call_timeout_ms: u64,
    pub env: Vec<(String, String)>,
}

/// Builds a `conway.json` naming ONE mock LLM backend (`base_url`/`model`,
/// the same shape `common::write_fixture_with` renders) and ONE
/// `[plugins].mcp[]` entry (`mcp`) on top of it -- the CLI-level seam
/// `crates/conway-cli/src/mcp_plugins.rs::install` discovers and attaches
/// before the root agent's very first turn, so every tool the scripted MCP
/// server declares is already live by the time the LLM mock's own script
/// gets to call it.
pub fn write_fixture_with_mcp(
    base_url: &str,
    model: &str,
    max_steps: u32,
    mcp: &McpEntryCfg,
) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let env_pairs: Vec<serde_json::Value> = mcp
        .env
        .iter()
        .map(|(k, v)| serde_json::json!([k, v]))
        .collect();
    let config = serde_json::json!({
        "default_role": "default",
        "limits": { "max_steps": max_steps },
        "backends": {
            "mock": { "kind": "openai-compat", "base_url": base_url, "dialect": "openai" }
        },
        "roles": {
            "default": { "chain": [format!("mock/{model}")] }
        },
        "plugins": {
            "mcp": [{
                "id": mcp.id,
                "command": mcp.command,
                "timeout_ms": mcp.timeout_ms,
                "first_call_timeout_ms": mcp.first_call_timeout_ms,
                "env": env_pairs,
            }]
        }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");

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

/// A server declaring one tool, `sleep`, that sleeps `SLEEP_MS`
/// milliseconds (env var, default 0) before answering `"slept"` -- EVERY
/// `tools/call`, not just the first. Used for the grace/warm-up-budget
/// tests (a moderate, bounded overshoot) and, with a huge `SLEEP_MS`, the
/// genuinely-wedged-server test (never answers within any of this item's
/// short test ceilings).
pub const SLEEP_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, time, os

SLEEP_MS = float(os.environ.get("SLEEP_MS", "0"))

def initialize(rid):
    return {"jsonrpc": "2.0", "id": rid, "result": {
        "protocolVersion": "2024-11-05",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "dogfood-sleep", "version": "0.1"},
    }}

def tools_list(rid):
    return {"jsonrpc": "2.0", "id": rid, "result": {
        "tools": [{
            "name": "sleep",
            "description": "sleeps SLEEP_MS ms then answers",
            "inputSchema": {"type": "object"},
        }]
    }}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps(initialize(rid)) + "\n")
        sys.stdout.flush()
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
        sys.stdout.flush()
    elif method == "tools/call":
        time.sleep(SLEEP_MS / 1000.0)
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": "slept"}],
            "isError": False,
        }}) + "\n")
        sys.stdout.flush()
"#;

/// A server declaring one tool, `die`: generation 1 (its own first spawn)
/// dies mid-call, WITHOUT ever writing a response line, on the FIRST
/// `tools/call` it receives; every later generation (a respawn) answers
/// normally with its own pid. Adapted verbatim (same env-var contract) from
/// `conway-plugin-mcp`'s own
/// `DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER` -- see that constant's own
/// doc for the full reasoning; this crate's tests cannot import it
/// directly (a different crate's `tests/` directory), so it is
/// reproduced here rather than referenced.
///
/// - `SPAWN_COUNTER_FILE` (required): each spawn appends its own pid as one
///   line -- a test reads the line COUNT for how many times this host
///   actually spawned a child.
/// - `REQUEST_LOG_FILE` (required): every `tools/call` THIS generation ever
///   receives is appended (its `params`, one JSON line) BEFORE deciding
///   whether to answer or die -- the load-bearing evidence for "no call is
///   ever executed twice": a resent call would show up as an EXTRA line
///   here even though only one user-level call happened.
pub const DIE_ONCE_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os

counter_path = os.environ["SPAWN_COUNTER_FILE"]
generation = 0
if os.path.exists(counter_path):
    with open(counter_path) as f:
        generation = len(f.readlines())
generation += 1
with open(counter_path, "a") as f:
    f.write(str(os.getpid()) + "\n")

request_log_path = os.environ["REQUEST_LOG_FILE"]

def initialize(rid):
    return {"jsonrpc": "2.0", "id": rid, "result": {
        "protocolVersion": "2024-11-05",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "dogfood-die-once", "version": "0.1"},
    }}

def tools_list(rid):
    return {"jsonrpc": "2.0", "id": rid, "result": {
        "tools": [{
            "name": "die",
            "description": "generation 1 dies mid-call; later generations answer with their pid",
            "inputSchema": {"type": "object"},
        }]
    }}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps(initialize(rid)) + "\n")
        sys.stdout.flush()
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
        sys.stdout.flush()
    elif method == "tools/call":
        with open(request_log_path, "a") as f:
            f.write(json.dumps(req.get("params", {})) + "\n")
        if generation == 1:
            sys.exit(1)
        else:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": "pid=" + str(os.getpid())}],
                "isError": False,
            }}) + "\n")
            sys.stdout.flush()
"#;

/// A server declaring one tool, `boom`, that answers the handshake
/// normally but exits nonzero WITHOUT answering on EVERY `tools/call`, in
/// every generation -- genuinely crash-looping, never transiently unlucky.
/// Adapted verbatim from `conway-plugin-mcp`'s own `ALWAYS_DIES_SERVER`
/// (see this module's own top doc on why it is reproduced, not imported).
///
/// `SPAWN_COUNTER_FILE` (required): one line appended per spawn -- a test
/// reads the line count to prove how many children this host actually
/// spawned, bounded by `conway_plugin_mcp::MAX_AUTO_RESPAWNS`.
pub const ALWAYS_DIE_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os

counter_path = os.environ["SPAWN_COUNTER_FILE"]
with open(counter_path, "a") as f:
    f.write("x\n")

def initialize(rid):
    return {"jsonrpc": "2.0", "id": rid, "result": {
        "protocolVersion": "2024-11-05",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "dogfood-crashloop", "version": "0.1"},
    }}

def tools_list(rid):
    return {"jsonrpc": "2.0", "id": rid, "result": {
        "tools": [{
            "name": "boom",
            "description": "always exits nonzero on tools/call, without answering",
            "inputSchema": {"type": "object"},
        }]
    }}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps(initialize(rid)) + "\n")
        sys.stdout.flush()
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
        sys.stdout.flush()
    elif method == "tools/call":
        sys.exit(1)
"#;

/// Executes `path` once, stdin closed, discarding the result, BEFORE a
/// timed call against it -- the identical first-exec-tax warmup
/// `conway-plugin-mcp`'s own `tests/common::warm` performs and documents
/// (board item `01M09MPZ9C188AHNBKWEJ3CEQA`: a freshly-written script's
/// first exec on this OS can block for seconds at ~0% CPU before its own
/// code ever runs). Every fixture in this module loops one line at a time
/// over stdin and exits cleanly on EOF, so warming with a closed stdin is
/// safe for all of them.
pub async fn warm(path: &Path) {
    let child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let _ = child.wait().await;
    }
}
