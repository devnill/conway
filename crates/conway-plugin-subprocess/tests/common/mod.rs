// This module is compiled independently into EVERY integration-test binary
// that declares `mod common;` (`mechanism.rs`, `end_to_end.rs`) -- a
// fixture constant only one of those binaries reaches for is "dead code"
// from the OTHER binary's own point of view, not a real unused item, so
// this whole module is exempted from that lint rather than each binary
// needing its own partial re-export.
#![allow(dead_code)]

//! Test fixture helpers: every fixture "plugin" here is a plain Python 3
//! script this test suite writes into a fresh temp dir at run time --
//! **never** a pre-existing binary this repo ships, and never a path built
//! from untrusted input (this item's own hard rule: "never execute an
//! arbitrary binary a test constructs from untrusted input; test fixtures
//! must be scripts the test itself writes"). Python, not a second Rust
//! crate, deliberately: this is the ACCEPTANCE criterion's own "authored
//! outside this workspace's dependency graph" proof -- these fixtures
//! import nothing from `conway`, `serde`, or cargo at all, so the wire
//! contract genuinely is the whole interface a plugin author needs.

use std::io::Write as _;
use std::path::PathBuf;

use conway_plugin_subprocess::SubprocessPluginSpec;

/// Writes `contents` to `<dir>/<name>`, marks it executable (unix), and
/// returns the argv this test hands to [`SubprocessPluginSpec::command`]:
/// the script's own path, relying on its `#!` shebang line -- exactly how
/// an operator would name a real plugin script in `settings.json`, no
/// interpreter prepended by this harness.
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

/// The full-featured fixture: declares one tool, `greet`, taking a
/// required `name: string` argument. `tool/1` for `greet` replies with
/// `"hello, <name>"` -- unless `name` is exactly `"__boom__"`, which
/// answers `{"ok": false, "error": {"kind": "internal", ...}}` instead, so
/// a single fixture covers both the success and the declared-failure path
/// without a second script.
pub const GREET_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

req = json.loads(sys.stdin.read())
op = req.get("op")

if op == "tool.spec/1":
    print(json.dumps({
        "id": "acme.greet",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            },
            "category": "read",
            "permission": "safe",
        }],
    }))
elif op == "tool/1":
    tool = req.get("tool")
    args = req.get("arguments", {})
    if tool == "greet":
        name = args.get("name", "")
        if name == "__boom__":
            print(json.dumps({
                "ok": False,
                "error": {"kind": "internal", "detail": "boom"},
            }))
        else:
            print(json.dumps({
                "ok": True,
                "blocks": [{"type": "text", "text": f"hello, {name}"}],
                "is_error": False,
            }))
    else:
        print(json.dumps({
            "ok": False,
            "error": {"kind": "invalid_arguments", "detail": f"unknown tool {tool}"},
        }))
else:
    print(json.dumps({
        "ok": False,
        "error": {"kind": "internal", "detail": f"unknown op {op}"},
    }))
"#;

/// Sleeps past any sane test timeout before ever answering -- the timeout
/// fixture. Reads (and discards) stdin first so a write-then-await-answer
/// host never blocks on a full stdin pipe buffer for an unrelated reason.
pub const SLEEPY_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, time
sys.stdin.read()
time.sleep(10)
print("{}")
"#;

/// Exits 0 with stdout that is not valid JSON -- the garbage-output
/// fixture.
pub const GARBAGE_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys
sys.stdin.read()
print("not json at all")
"#;

/// Exits 3 regardless of input -- the nonzero-exit fixture.
pub const FAILING_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys
sys.stdin.read()
sys.exit(3)
"#;

pub fn spec_for(dir: &std::path::Path, script_name: &str, contents: &str) -> SubprocessPluginSpec {
    let path = write_script(dir, script_name, contents);
    SubprocessPluginSpec::new("test-fixture", vec![path.display().to_string()])
}

/// A spec configured for the PERSISTENT NDJSON transport (board item
/// `01M03VJHG1WFECFJB4ZH3CKWDX`): same as [`spec_for`] but with
/// `transport` set to `Persistent`, so `tool/1` calls dispatch over one
/// long-lived child instead of re-spawning fresh per call.
pub fn persistent_spec_for(
    dir: &std::path::Path,
    script_name: &str,
    contents: &str,
) -> SubprocessPluginSpec {
    let path = write_script(dir, script_name, contents);
    let mut spec = SubprocessPluginSpec::new("test-fixture", vec![path.display().to_string()]);
    spec.transport = conway_plugin_subprocess::SubprocessTransport::Persistent;
    spec
}

/// A persistent-transport fixture: declares one tool, `greet`, and answers
/// `tool/1` calls over an NDJSON line loop (one JSON request per line on
/// stdin, one JSON response per line on stdout). The SAME script also
/// serves the one-shot `tool.spec/1` discovery path: the host writes one
/// request then closes stdin, so the line loop runs once and exits on EOF.
/// `tool/1` for `greet` replies `"hello, <name>"`; `name == "__boom__"`
/// answers a declared `internal` error -- the same dual success/failure
/// coverage the one-shot [`GREET_PLUGIN`] provides, for the persistent path.
pub const PERSISTENT_GREET_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.greet",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            },
            "category": "read",
            "permission": "safe",
        }],
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    rid = req.get("id")
    if op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        tool = req.get("tool")
        args = req.get("arguments", {})
        if tool == "greet":
            name = args.get("name", "")
            if name == "__boom__":
                resp = {"id": rid, "ok": False, "error": {"kind": "internal", "detail": "boom"}}
            else:
                resp = {"id": rid, "ok": True, "blocks": [{"type": "text", "text": f"hello, {name}"}], "is_error": False}
        else:
            resp = {"id": rid, "ok": False, "error": {"kind": "invalid_arguments", "detail": f"unknown tool {tool}"}}
        sys.stdout.write(json.dumps(resp) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// A persistent-transport fixture that reports its OWN `os.getpid()` as the
/// `tool/1` result -- the load-bearing fixture for acceptance criterion 1:
/// two sequential calls must return the SAME pid (the child was reused),
/// not a fresh pid per call. Used under BOTH transports to make the
/// assertion non-tautological: persistent -> identical pids; one-shot ->
/// differing pids (a fresh process each call).
pub const PERSISTENT_PID_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json, os

def manifest():
    return {
        "id": "acme.pid",
        "version": "0.1.0",
        "tools": [{
            "name": "pid",
            "description": "reports this process's own pid",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    rid = req.get("id")
    if op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        resp = {"id": rid, "ok": True, "blocks": [{"type": "text", "text": str(os.getpid())}], "is_error": False}
        sys.stdout.write(json.dumps(resp) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// A persistent-transport fixture that answers the FIRST `tool/1` call then
/// exits nonzero -- the "dies mid-session" fixture for acceptance criterion
/// 2: the second call must fail with a typed `SessionDied` error within the
/// timeout, never a hang.
pub const PERSISTENT_DIE_AFTER_ONE_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.die",
        "version": "0.1.0",
        "tools": [{
            "name": "die",
            "description": "answers once then exits nonzero",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }

count = 0
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    rid = req.get("id")
    if op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
        sys.stdout.flush()
    elif op == "tool/1":
        count += 1
        if count == 1:
            sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": "first"}], "is_error": False}) + "\n")
            sys.stdout.flush()
            sys.exit(1)
        else:
            # unreachable: we exited above
            sys.exit(1)
"#;

/// A persistent-transport fixture that reads a `tool/1` request and sleeps
/// past any sane timeout before answering -- the per-call timeout fixture
/// for acceptance criterion 3: the call must be killed and reported
/// `TimedOut` within `timeout_ms`, never a hang.
pub const PERSISTENT_SLEEPY_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json, time

def manifest():
    return {
        "id": "acme.sleepy",
        "version": "0.1.0",
        "tools": [{
            "name": "sleep",
            "description": "reads a request then sleeps forever",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    rid = req.get("id")
    if op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
        sys.stdout.flush()
    elif op == "tool/1":
        time.sleep(10)
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [], "is_error": False}) + "\n")
        sys.stdout.flush()
"#;

/// A persistent-transport fixture that writes a HALF line (no trailing
/// newline) then exits -- the malformed-frame fixture for acceptance
/// criterion 4: an unterminated frame is a typed parse error, not a
/// deadlock.
pub const PERSISTENT_HALF_LINE_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.half",
        "version": "0.1.0",
        "tools": [{
            "name": "half",
            "description": "writes a half line then exits",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    if op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
        sys.stdout.flush()
    elif op == "tool/1":
        # A partial JSON object with no trailing newline, then exit -- an
        # unterminated frame.
        sys.stdout.write('{"id": 1, "ok": true, ')
        sys.stdout.flush()
        sys.exit(0)
"#;

/// A persistent-transport fixture that writes a COMPLETE line of invalid
/// JSON then exits -- the second malformed-frame shape for acceptance
/// criterion 4 ("invalid JSON" alongside "no newline / partial line then
/// EOF").
pub const PERSISTENT_BAD_JSON_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.badjson",
        "version": "0.1.0",
        "tools": [{
            "name": "badjson",
            "description": "writes invalid json then exits",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    if op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
        sys.stdout.flush()
    elif op == "tool/1":
        sys.stdout.write("this is not valid json\n")
        sys.stdout.flush()
        sys.exit(0)
"#;

/// A persistent-transport fixture that WEDGES on a large `tool/1` request --
/// the regression fixture for the unbounded-write hang an adversarial review
/// surfaced: a child that stops draining stdin while staying alive (stdout
/// open) makes the host's `write_all` block forever once the OS pipe buffer
/// fills, violating the "never a hang" guarantee. This fixture reads only a
/// SMALL prefix of stdin (a `read(100)`, not a full line): enough to parse a
/// tiny `tool.spec/1` discovery request in full (it is ~22 bytes and the
/// one-shot discovery path closes stdin, so `read(100)` returns the whole
/// request), but NOT enough to drain a large `tool/1` request past the OS
/// pipe buffer. A `tool/1` request whose serialized form exceeds the read
/// size arrives here as an incomplete JSON object; `json.loads` raises, and
/// the fixture sleeps -- alive, stdout open, stdin NOT drained further -- so
/// the host's `write_all` fills the pipe and blocks. The per-call write
/// deadline must bound that block (fail `TimedOut` within `timeout_ms`); with
/// the write left unbounded, `write_all` hangs forever and this fixture hangs
/// the test to the harness's own ceiling.
pub const PERSISTENT_WEDGE_ON_WRITE_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json, time

# Read a small prefix only: a `tool.spec/1` discovery request (~22 bytes, and
# the one-shot discovery path closes stdin so this read returns it whole) fits
# and parses; a large `tool/1` request does not, and `json.loads` raises below.
chunk = sys.stdin.read(100)
try:
    req = json.loads(chunk)
except Exception:
    # Incomplete JSON -> a large tool/1 request the host is still writing.
    # WEDGE: stop draining stdin, stay alive, keep stdout open. The host's
    # write_all fills the pipe buffer and blocks -- the hang the per-call
    # write deadline exists to bound.
    time.sleep(3600)
    sys.exit(0)

op = req.get("op")
if op == "tool.spec/1":
    sys.stdout.write(json.dumps({
        "id": "acme.wedge",
        "version": "0.1.0",
        "tools": [{
            "name": "wedge",
            "description": "reads a prefix then wedges on a large request",
            "schema": {"type": "object", "properties": {"x": {"type": "string"}}, "required": ["x"]},
            "category": "read",
            "permission": "safe",
        }],
    }) + "\n")
    sys.stdout.flush()
else:
    time.sleep(3600)
"#;
