// This module is compiled separately into EVERY integration-test binary that
// declares `mod common;` -- a fixture constant only one of those binaries
// reaches for is "dead code" from the OTHER binary's own point of view, so
// this whole module is exempted from that lint rather than each binary needing
// its own partial re-export.
#![allow(dead_code)]

//! Test fixture helpers for `conway-plugin-mcp`: every fixture "MCP server"
//! here is a plain Python 3 script this test suite writes into a fresh temp
//! dir at run time -- **never** a pre-existing binary this repo ships, and
//! never a path built from untrusted input (the same hard rule
//! `conway-plugin-subprocess`'s own `tests/common/mod.rs` states). Python, not
//! a second Rust crate, deliberately: this is acceptance criterion 2's own
//! "a real MCP server (even a trivial reference one, hand-written for the
//! test)" -- these fixtures import nothing from `conway`, `serde`, or cargo
//! at all, so the JSON-RPC 2.0 wire contract genuinely is the whole interface
//! an MCP server author needs. They speak the REAL MCP 2024-11-05 stdio
//! subset (`initialize`/`notifications/initialized`/`tools/list`/`tools/call`),
//! NOT a mock of conway's own protocol.
//!
//! The reference server's wire shapes are the well-known MCP 2024-11-05
//! stdio subset, drawn from the MCP spec: a request is a single JSON-RPC 2.0
//! object per line (`{"jsonrpc":"2.0","id":N,"method":"...","params":{...}}`);
//! `initialize` answers with `{"protocolVersion":"2024-11-05","capabilities":
//! {"tools":{}},"serverInfo":{"name":"ref-mcp","version":"0.1"}}`;
//! `notifications/initialized` is a no-id notification the server does NOT
//! answer; `tools/list` answers `{"tools":[{"name","description",
//! "inputSchema"},...]}`; `tools/call` answers `{"content":[{"type":"text",
//! "text":"..."}],"isError":false}`.

use std::io::Write as _;
use std::path::PathBuf;

use conway_plugin_mcp::McpPluginSpec;

/// Writes `contents` to `<dir>/<name>`, marks it executable (unix), and
/// returns the argv this test hands to [`McpPluginSpec::command`]: the
/// script's own path, relying on its `#!` shebang line -- exactly how an
/// operator would name a real MCP server script in `settings.json`, no
/// interpreter prepended by this harness. The identical helper
/// `conway-plugin-subprocess::tests::common::write_script` provides.
pub fn write_script(dir: &std::path::Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create fixture script");
    f.write_all(contents.as_bytes())
        .expect("write fixture script");
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x fixture script");
    }
    path
}

/// Executes `path` once, with stdin closed, and discards everything about
/// the result -- before returning, so any caller sequencing `warm` before a
/// timed call pays this cost OUTSIDE the clock, not inside it.
///
/// Identical rationale and caveats to
/// `conway_plugin_subprocess`'s own `tests/common::warm` (board item
/// `01M09MPZ9C188AHNBKWEJ3CEQA`, measured 2026-08-21): executing a
/// FRESHLY WRITTEN script for the first time on this OS can block for
/// seconds at ~0% CPU before the script's own code ever runs (23.5s at 0%
/// CPU measured once; 44ms/35ms on the SAME file's second/third exec).
/// `write_script` above writes a fresh file into a fresh temp dir moments
/// before a test execs it under a bounded `timeout_ms`, so without warming
/// that tax lands inside the timed assertion rather than the handshake it
/// is meant to bound. The 0%-CPU blocking is MEASURED; attributing it to
/// Gatekeeper/XProtect specifically is INFERENCE (see the sibling crate's
/// doc for what was and was not confirmed).
///
/// Every current fixture server here either loops "one line at a time"
/// over stdin (and simply exits when stdin is closed/empty) or fails fast
/// on empty input -- none reaches an unconditional sleep with no input.
/// Do not reuse `warm` for a fixture that would.
pub async fn warm(path: &std::path::Path) {
    let child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let _ = child.wait().await;
    }
}

/// Builds an [`McpPluginSpec`] pointing at `dir/<script_name>` written with
/// `contents`. Uses the default [`McpPluginSpec::timeout_ms`] (5000ms). Does
/// NOT warm the fixture -- use this only for a spec that is never handed to
/// a timed call against a freshly-written script (e.g.
/// `discover_fails_closed_when_the_command_cannot_be_spawned`'s nonexistent
/// path, which writes nothing). For any spec built from a real fixture
/// script and handed to `McpPlugin::discover`, use [`spec_for_warmed`]
/// instead: this crate's own default margin turned out NOT to be enough
/// (board item `01M09MPZ9C188AHNBKWEJ3CEQA`) -- 8 of 10 tests in
/// `mcp_end_to_end.rs` that called this function directly were measured
/// failing `TimedOut` under load before they were switched to
/// [`spec_for_warmed`]; the previous claim here that "5000ms already has
/// enough margin" was an untested assumption, not a measurement.
pub fn spec_for(dir: &std::path::Path, script_name: &str, contents: &str) -> McpPluginSpec {
    let path = write_script(dir, script_name, contents);
    McpPluginSpec::new("test-fixture", vec![path.display().to_string()])
}

/// Same as [`spec_for`], but pays the fresh-script first-exec tax (see
/// [`warm`]'s doc; board item `01M09MPZ9C188AHNBKWEJ3CEQA`) BEFORE
/// returning the spec, so a caller that immediately hands the spec to
/// `McpPlugin::discover` under the crate's default `timeout_ms` measures
/// the handshake it means to measure, not a first-exec OS cost.
pub async fn spec_for_warmed(
    dir: &std::path::Path,
    script_name: &str,
    contents: &str,
) -> McpPluginSpec {
    let path = write_script(dir, script_name, contents);
    warm(&path).await;
    McpPluginSpec::new("test-fixture", vec![path.display().to_string()])
}

/// Like [`spec_for`] but with a caller-chosen `timeout_ms` -- used by the
/// timeout/cancel tests that exercise the per-call deadline (a short deadline
/// fails a stuck server fast, proving the deadline bounds a hang without
/// making the suite wait seconds for it).
///
/// `async` (unlike [`spec_for`]) because a SHORT `timeout_ms` is exactly the
/// shape that pays the first-execution OS tax [`warm`]'s doc describes
/// (board item `01M09MPZ9C188AHNBKWEJ3CEQA`): this fixture is exec'd for
/// the first time, moments after being written, under the same tight
/// budget the caller is asserting against. `warm` pays that tax here,
/// discarded, before `timeout_ms` starts governing anything, so the
/// caller's deadline measures the handshake/call it says it measures, not
/// an OS-dependent first-exec cost. This helper exists as a separate
/// function (rather than a `timeout_ms` parameter on [`spec_for_warmed`])
/// because a caller-chosen SHORT deadline is the shape most likely to
/// notice the tax; [`spec_for_warmed`] warms unconditionally too, for the
/// same reason (see its own doc).
pub async fn spec_with_timeout(
    dir: &std::path::Path,
    script_name: &str,
    contents: &str,
    timeout_ms: u64,
) -> McpPluginSpec {
    let path = write_script(dir, script_name, contents);
    warm(&path).await;
    let mut spec = McpPluginSpec::new("test-fixture", vec![path.display().to_string()]);
    spec.timeout_ms = timeout_ms;
    spec
}

/// The reference MCP server: a hand-written Python 3 stdio MCP server that
/// declares two tools -- `add` (taking `{a:int,b:int}`, returning `a+b` as a
/// text block) and `greet` (taking `{name:str}`, returning `hello, <name>`)
/// -- and answers `tools/call` with real MCP `content` arrays. For bad args
/// (e.g. `name == "__boom__"`), it returns `isError: true` with an error
/// text, so a single fixture covers both the success and the tool-level-error
/// path. This is a REAL MCP server, not a mock of conway's protocol -- it
/// speaks the actual JSON-RPC 2.0 MCP method set.
///
/// The server reads NDJSON lines from stdin in a loop: `initialize` -> answer
/// the handshake; `notifications/initialized` (no id) -> no answer; `tools/list`
/// -> answer the tool list; `tools/call` -> dispatch by `name`.
pub const REF_MCP_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-mcp", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [
                {
                    "name": "add",
                    "description": "Add two integers and return the sum.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "a": {"type": "integer"},
                            "b": {"type": "integer"},
                        },
                        "required": ["a", "b"],
                    },
                },
                {
                    "name": "greet",
                    "description": "Greet the caller by name.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"name": {"type": "string"}},
                        "required": ["name"],
                    },
                },
            ]
        }
    }

def tools_call(rid, params):
    name = params.get("name", "")
    args = params.get("arguments", {})
    if name == "add":
        a = args.get("a", 0)
        b = args.get("b", 0)
        result = {"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": str(a + b)}],
            "isError": False,
        }}
    elif name == "greet":
        nm = args.get("name", "")
        if nm == "__boom__":
            result = {"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": "boom: greet refused"}],
                "isError": True,
            }}
        else:
            result = {"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": f"hello, {nm}"}],
                "isError": False,
            }}
    else:
        result = {"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": f"unknown tool: {name}"}],
            "isError": True,
        }}
    return result

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        resp = initialize(rid)
    elif method == "notifications/initialized":
        # A notification: no id, NO answer. The server does not respond.
        continue
    elif method == "tools/list":
        resp = tools_list(rid)
    elif method == "tools/call":
        resp = tools_call(rid, req.get("params", {}))
    else:
        resp = {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": f"method not found: {method}"}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

/// An MCP server that answers the FIRST `tools/call` then exits nonzero -- the
/// "dies mid-session" fixture: the second call must fail with a typed
/// `SessionDied` within the timeout, never a hang. The handshake completes
/// normally so the session opens; only a subsequent `tools/call` observes the
/// death.
pub const DIE_AFTER_ONE_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-die", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [{
                "name": "die",
                "description": "answers once then the server exits nonzero",
                "inputSchema": {"type": "object"},
            }]
        }
    }

count = 0
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps(initialize(rid)) + "\n")
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
    elif method == "tools/call":
        count += 1
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": "first"}],
            "isError": False,
        }}) + "\n")
        sys.stdout.flush()
        sys.exit(1)
    sys.stdout.flush()
"#;

/// An MCP server that answers the handshake but sleeps past any test timeout
/// on the first `tools/call` -- the per-call timeout fixture: the call must be
/// killed and reported `TimedOut` within `timeout_ms`, never a hang.
pub const SLEEPY_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, time

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-sleepy", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [{
                "name": "sleep",
                "description": "reads a call then sleeps forever",
                "inputSchema": {"type": "object"},
            }]
        }
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps(initialize(rid)) + "\n")
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
    elif method == "tools/call":
        time.sleep(10)
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": "slept"}],
            "isError": False,
        }}) + "\n")
    sys.stdout.flush()
"#;

/// An MCP server that answers the handshake but sleeps a SHORT, bounded 300ms
/// on EVERY `tools/call` (then answers `slept`) -- the cancel-survives fixture:
/// a first call is cancelled mid-read-sleep (returns `Cancelled` promptly), and
/// a SECOND call afterwards must still SUCCEED, proving the shared session
/// survived the cancellation and the NDJSON framing was not corrupted (a
/// cancel-during-write would have left a partial request line in the pipe; the
/// second call's full request would concatenate onto it and the server's
/// `json.loads` would choke). 300ms is long enough that a 50ms cancel lands
/// squarely in the read sleep, yet short enough that the second call -- which
/// waits for the first sleep to finish, then its own -- completes well inside
/// the 5000ms per-call timeout.
pub const SHORT_SLEEPY_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, time

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-short-sleepy", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [{
                "name": "sleep",
                "description": "reads a call then sleeps 300ms before answering",
                "inputSchema": {"type": "object"},
            }]
        }
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps(initialize(rid)) + "\n")
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
    elif method == "tools/call":
        time.sleep(0.3)
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": "slept"}],
            "isError": False,
        }}) + "\n")
    sys.stdout.flush()
"#;

/// An MCP server whose `tools/call` answer mixes a KNOWN `text` block with an
/// UNKNOWN content block type (`{"type":"quantum","data":"..."}`) -- the
/// drop+count+surface degradation proof: the call must SUCCEED with the known
/// block, and a `ContentBlock::Text` note naming the dropped type must be
/// appended (unless `isError` is already true). The manifest declares only
/// known tags so discovery is clean; the unknown type appears only in the
/// `tools/call` answer body.
pub const UNKNOWN_BLOCK_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-mix", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [{
                "name": "mix",
                "description": "returns a known and an unknown block type",
                "inputSchema": {"type": "object"},
            }]
        }
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps(initialize(rid)) + "\n")
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
    elif method == "tools/call":
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [
                {"type": "text", "text": "kept"},
                {"type": "quantum", "data": "future block this host does not know"},
            ],
            "isError": False,
        }}) + "\n")
    sys.stdout.flush()
"#;

/// An MCP server that does NOT offer the `tools` capability in its
/// `initialize` result -- the handshake-refusal fixture: `discover` must fail
/// with `HandshakeFailed` naming the missing `tools` capability, at discover
/// time, before any `tools/call` runs.
pub const NO_TOOLS_CAP_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        # Deliberately OMIT the `tools` capability -- the host must refuse.
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": {"name": "ref-no-tools", "version": "0.1"},
        }}) + "\n")
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {"tools": []}}) + "\n")
    sys.stdout.flush()
"#;

/// An MCP server that reports its OWN `os.getpid()` as the `tools/call` result
/// -- the load-bearing fixture for "every tool shares ONE child process": two
/// sequential calls must return the SAME pid (the child was reused), not a
/// fresh pid per call.
pub const PID_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-pid", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [{
                "name": "pid",
                "description": "reports this process's own pid",
                "inputSchema": {"type": "object"},
            }]
        }
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps(initialize(rid)) + "\n")
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
    elif method == "tools/call":
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": str(os.getpid())}],
            "isError": False,
        }}) + "\n")
    sys.stdout.flush()
"#;
