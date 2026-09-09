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
/// A server that is SLOW TO START and then behaves normally: it sleeps
/// before answering `initialize`, then serves like [`REF_MCP_SERVER`].
///
/// Stands in for a Claude Code plugin that builds itself on first launch
/// (ideate's `bin/ideate-mcp` runs `npm install && npm run build` before
/// exec'ing Node). The delay is deliberately placed BEFORE the initialize
/// answer and nowhere else, so a test can hold the per-call deadline far
/// below it and still open the session -- which is exactly the separation
/// `DEFAULT_STARTUP_TIMEOUT_MS` exists to express.
pub const SLOW_START_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, time, os

time.sleep(float(os.environ.get("SLOW_START_SECONDS", "1.5")))

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "slow-start", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [{
                "name": "ping",
                "description": "Answer pong.",
                "inputSchema": {"type": "object", "properties": {}},
            }]
        }
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    rid = msg.get("id")
    if rid is None:
        continue
    if method == "initialize":
        out = initialize(rid)
    elif method == "tools/list":
        out = tools_list(rid)
    elif method == "tools/call":
        out = {"jsonrpc": "2.0", "id": rid,
               "result": {"content": [{"type": "text", "text": "pong"}]}}
    else:
        out = {"jsonrpc": "2.0", "id": rid,
               "error": {"code": -32601, "message": "no"}}
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()
"#;

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

/// An MCP server whose GENERATION 1 dies mid-call -- WITHOUT ever answering
/// the first `tools/call` it receives -- and whose every later generation
/// answers normally, with its own pid as the result text. The
/// "session dies mid-flight, never re-sent" fixture (board item
/// `01M1ZR1DGZB8FB399SMG51RHP5`'s correction): unlike an older fixture this
/// one replaced, generation 1 does NOT answer-then-exit (that shape can
/// never distinguish "the call succeeded, the death is incidental" from "the
/// call itself died in flight" from OUTSIDE the process) -- it exits
/// nonzero with NO response line written at all, so the client's read
/// genuinely observes the death mid-call, the exact shape a resent request
/// would be needed to paper over.
///
/// Two env vars, both optional/required as noted:
/// - `SPAWN_COUNTER_FILE` (required): each spawn's generation number is
///   `1 + the number of lines already in this file` (read BEFORE this
///   spawn appends its own line), and this spawn then appends its OWN
///   `os.getpid()` as one line -- so a test reading this file back gets
///   BOTH the total spawn count (line count) AND each generation's actual
///   pid (the line's own content), letting it assert generation 2's pid
///   differs from generation 1's without generation 1 ever having to
///   report its own pid over the wire (it never gets to -- it dies first).
/// - `REQUEST_LOG_FILE` (optional): every `tools/call` THIS generation ever
///   receives is appended to this file (its `params`, as one JSON line)
///   BEFORE this generation decides whether to answer or die -- so a
///   resent request would show up here as a SECOND line even though only
///   one user-level call happened. This is the load-bearing evidence for
///   the no-resend test; see that test's own doc.
pub const DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os

counter_path = os.environ["SPAWN_COUNTER_FILE"]
generation = 0
if os.path.exists(counter_path):
    with open(counter_path) as f:
        generation = len(f.readlines())
generation += 1
with open(counter_path, "a") as f:
    f.write(str(os.getpid()) + "\n")

request_log_path = os.environ.get("REQUEST_LOG_FILE")

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-die-once", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [{
                "name": "die",
                "description": "generation 1 dies mid-call without answering; every later generation answers with its own pid",
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
        sys.stdout.flush()
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
        sys.stdout.flush()
    elif method == "tools/call":
        if request_log_path:
            with open(request_log_path, "a") as f:
                f.write(json.dumps(req.get("params", {})) + "\n")
        if generation == 1:
            # Dies mid-call, WITHOUT writing a response line at all -- the
            # client's read observes a genuine mid-flight death, never a
            # completed-then-exited call.
            sys.exit(1)
        else:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": str(os.getpid())}],
                "isError": False,
            }}) + "\n")
            sys.stdout.flush()
"#;

/// An MCP server that answers its handshake normally but, on EVERY
/// `tools/call` it ever receives (in ANY generation -- there is no "answer
/// once" here, unlike [`DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER`]), exits
/// nonzero WITHOUT
/// answering at all: a genuinely crash-looping server, never transiently
/// unlucky. Writes its own spawn generation to the file named by the
/// `SPAWN_COUNTER_FILE` env var on startup (appending one line per spawn) --
/// a test reads that file's line count to prove how many times this host
/// actually spawned a fresh child, independent of and more reliable than
/// counting `invoke` calls (which also include calls that never trigger a
/// respawn).
pub const ALWAYS_DIES_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os

counter_path = os.environ.get("SPAWN_COUNTER_FILE")
if counter_path:
    with open(counter_path, "a") as f:
        f.write("x\n")

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-crashloop", "version": "0.1"},
        }
    }

def tools_list(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "tools": [{
                "name": "boom",
                "description": "always exits nonzero on tools/call, without answering",
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
        sys.stdout.flush()
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        sys.stdout.write(json.dumps(tools_list(rid)) + "\n")
        sys.stdout.flush()
    elif method == "tools/call":
        # Exits WITHOUT writing a response line at all -- the client's read
        # observes EOF mid-call, the same "died mid-call" shape
        # `DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER`'s generation 1 produces,
        # but on every generation.
        sys.exit(1)
"#;

/// An MCP server whose `tools/list` answer depends on its OWN spawn
/// generation, read from the `SPAWN_COUNTER_FILE` env var (same mechanism as
/// [`ALWAYS_DIES_SERVER`]): generation 1 declares two tools (`stable`,
/// `only_on_first`) and exits nonzero without answering the first
/// `tools/call` it receives (forcing a respawn); generation 2+ declares only
/// `stable` -- `only_on_first` is GONE. Proves acceptance criterion 5: a
/// respawn whose fresh `tools/list` no longer matches the tool set this
/// plugin registered at `discover` time must fail the call with a typed
/// error naming the difference, never silently drop the tool from the
/// runtime's view.
pub const TOOL_SET_CHANGES_ON_RESPAWN_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os

counter_path = os.environ["SPAWN_COUNTER_FILE"]
generation = 0
if os.path.exists(counter_path):
    with open(counter_path) as f:
        generation = len(f.readlines())
generation += 1
with open(counter_path, "a") as f:
    f.write("x\n")

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-shifting", "version": "0.1"},
        }
    }

def tools_list(rid):
    tools = [{
        "name": "stable",
        "description": "present in every generation",
        "inputSchema": {"type": "object"},
    }]
    if generation == 1:
        tools.append({
            "name": "only_on_first",
            "description": "present only in generation 1",
            "inputSchema": {"type": "object"},
        })
    return {"jsonrpc": "2.0", "id": rid, "result": {"tools": tools}}

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
        if generation == 1:
            # Generation 1 dies mid-call, without answering, forcing a
            # respawn -- the respawn is what should discover the shrunk
            # generation-2 tool set and refuse it.
            sys.exit(1)
        else:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": "ok"}],
                "isError": False,
            }}) + "\n")
            sys.stdout.flush()
"#;

/// The IDENTICAL "dies mid-first-call, every later generation recovers"
/// fixture as [`DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER`], with one
/// addition: its `die` tool's `tools/list` entry carries an
/// `annotations.idempotentHint` controlled ENTIRELY by the
/// `IDEMPOTENT_HINT` env var, read fresh on every spawn (so a respawned
/// generation reports the SAME value the original generation did, matching
/// the real-world shape of one server declaring one fixed annotation for
/// its whole life):
/// - unset -> no `annotations` key at all (the "absent" case -- MCP's own
///   documented default applies: unsafe to retry).
/// - `"true"` -> `{"idempotentHint": true}`.
/// - `"false"` -> `{"idempotentHint": false}` (the EXPLICIT-false case,
///   distinct from absent -- both must fail closed the same way).
///
/// One script, one behavior, three env-selected declarations -- so the
/// "retried" and "not retried" tests exercise a genuinely IDENTICAL fixture
/// (same file bytes, same generation-1-dies/generation-2-recovers shape),
/// differing only in the ONE field this item's retry decision reads. Reuses
/// the identical `SPAWN_COUNTER_FILE`/`REQUEST_LOG_FILE` evidence
/// [`DIES_MID_FIRST_CALL_THEN_RECOVERS_SERVER`]'s own doc describes: the
/// counter file proves how many children were actually spawned, and the
/// request log proves how many `tools/call`s actually reached a server
/// process -- 1 line for a call that was correctly NOT retried, 2 lines for
/// one that WAS.
pub const DIES_MID_FIRST_CALL_ANNOTATED_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, os

counter_path = os.environ["SPAWN_COUNTER_FILE"]
generation = 0
if os.path.exists(counter_path):
    with open(counter_path) as f:
        generation = len(f.readlines())
generation += 1
with open(counter_path, "a") as f:
    f.write(str(os.getpid()) + "\n")

request_log_path = os.environ.get("REQUEST_LOG_FILE")
idempotent_hint_mode = os.environ.get("IDEMPOTENT_HINT")  # "true", "false", or unset

def initialize(rid):
    return {
        "jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "ref-die-annotated", "version": "0.1"},
        }
    }

def tools_list(rid):
    tool = {
        "name": "die",
        "description": "generation 1 dies mid-call without answering; every later generation answers with its own pid",
        "inputSchema": {"type": "object"},
    }
    if idempotent_hint_mode == "true":
        tool["annotations"] = {"idempotentHint": True}
    elif idempotent_hint_mode == "false":
        tool["annotations"] = {"idempotentHint": False}
    # else: no "annotations" key at all -- the absent case.
    return {"jsonrpc": "2.0", "id": rid, "result": {"tools": [tool]}}

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
        if request_log_path:
            with open(request_log_path, "a") as f:
                f.write(json.dumps(req.get("params", {})) + "\n")
        if generation == 1:
            # Dies mid-call, WITHOUT writing a response line at all -- the
            # client's read genuinely observes a mid-flight death, never a
            # completed-then-exited call.
            sys.exit(1)
        else:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": str(os.getpid())}],
                "isError": False,
            }}) + "\n")
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
