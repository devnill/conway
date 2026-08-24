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

/// Executes `path` once, with stdin closed, and discards everything about
/// the result -- before returning, so any caller sequencing `warm` before a
/// timed call pays this cost OUTSIDE the clock, not inside it.
///
/// # Why this exists (board item `01M09MPZ9C188AHNBKWEJ3CEQA`, measured
/// 2026-08-21)
///
/// Executing a FRESHLY WRITTEN script for the first time on this OS can
/// block for seconds at ~0% CPU before the script's own code ever runs --
/// one measurement on this machine caught 23.5s wall clock at 0% CPU for a
/// brand-new two-line `#!/usr/bin/env python3` file; the SAME file's
/// second and third executions cost 44ms and 35ms. 0% CPU while blocked
/// means the delay is a kernel/OS-service-side check on the file, not
/// computation -- consistent with (but not proven to be; see below) macOS
/// scanning a newly created executable the first time it is exec'd, the
/// way it scans a downloaded one.
///
/// `write_script` writes a fresh file into a fresh temp dir and the tests
/// in this suite exec it moments later under a bounded `timeout_ms` --
/// exactly the shape that pays this tax. Left unwarmed, the tax lands
/// INSIDE the timed assertion, so a budget sized to bound the mechanism
/// under test (a handshake, a hang) instead measures a first-execution OS
/// cost that has nothing to do with this crate. That is why the failure
/// always showed up on the promptly-answering DISCOVERY step rather than
/// the hang the test actually exercises, and why it tracked parallelism
/// (more test binaries => more fresh fixtures competing for the same
/// scan) rather than CPU load as such.
///
/// Calling `warm(&path)` before constructing the timed spec pays this cost
/// once, discarded, so the timed call downstream measures the mechanism it
/// says it measures, not an OS-dependent first-exec tax. Do NOT "fix" a
/// flaky timeout test by raising its budget again without first checking
/// whether it execs a fixture `write_script` just wrote -- if so, warm it
/// instead of guessing a bigger number.
///
/// Every current fixture in this module either reads stdin then fails
/// fast on empty/invalid input, or loops "one line at a time" and simply
/// ends when stdin is empty -- none of them reach an unconditional sleep
/// on closed/empty stdin. Do not reuse `warm` for a fixture that would
/// (it would turn the warm-up into the sleep itself).
///
/// One caveat this item is explicit about keeping honest: the 0%-CPU
/// blocking above is MEASURED. Attributing it specifically to Gatekeeper
/// or XProtect is INFERENCE, not measurement -- this repro re-run (on this
/// machine, this session) found a much smaller tax (~150-250ms, not
/// 23.5s), and confirmed the created file DOES carry a
/// `com.apple.provenance` extended attribute (checked with `xattr -l`),
/// which is the kind of tag first-launch trust checks key off on current
/// macOS -- suggestive, not conclusive. `log show` for `syspolicyd`/
/// `amfid` around an exec produced no matching entries on this machine,
/// which is inconclusive (could mean no such check ran, or could mean
/// this user's log visibility does not cover it) rather than a
/// disconfirmation. The fix does not depend on knowing which OS service is
/// responsible: it depends only on the measured fact that the SAME file's
/// first exec is slow and its later execs are fast.
pub async fn warm(path: &std::path::Path) {
    let child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        // Outcome deliberately discarded: warming only cares that the exec
        // happened (and whatever OS-side check gates it completed), not
        // what the script did with no input.
        let _ = child.wait().await;
    }
    // A spawn failure here is not this helper's problem to report -- the
    // real, timed call immediately after will hit (and report) the same
    // spawn failure if the path is genuinely bad.
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

/// Same as [`spec_for`], but pays the fresh-script first-exec tax (see
/// [`warm`]'s doc; board item `01M09MPZ9C188AHNBKWEJ3CEQA`) BEFORE
/// returning the spec, so a caller that immediately hands the spec to
/// `SubprocessPlugin::discover` under the crate's default `timeout_ms`
/// measures the mechanism it means to measure, not a first-exec OS cost.
/// Do not reach for this on a fixture that would hang forever on
/// closed/empty stdin (see `warm`'s own caveat) -- none of the ONE-SHOT
/// fixtures in this suite do.
pub async fn spec_for_warmed(
    dir: &std::path::Path,
    script_name: &str,
    contents: &str,
) -> SubprocessPluginSpec {
    let path = write_script(dir, script_name, contents);
    warm(&path).await;
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

/// Same as [`persistent_spec_for`], but pays the fresh-script first-exec tax
/// (see [`warm`]'s doc; board item `01M09MPZ9C188AHNBKWEJ3CEQA`) BEFORE
/// returning the spec, so a caller that immediately hands the spec to
/// `SubprocessPlugin::discover` under the crate's default `timeout_ms`
/// measures the handshake it means to measure, not a first-exec OS cost.
/// Use this over [`persistent_spec_for`] whenever the test does not already
/// warm the fixture itself (e.g. because it needs the raw path for its own
/// purposes, as `tests/mechanism.rs` and `tests/persistent.rs` do).
pub async fn persistent_spec_for_warmed(
    dir: &std::path::Path,
    script_name: &str,
    contents: &str,
) -> SubprocessPluginSpec {
    let path = write_script(dir, script_name, contents);
    warm(&path).await;
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
        sys.stdout.flush()
    elif op == "tool.spec/1":
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
        sys.stdout.flush()
    elif op == "tool.spec/1":
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
        sys.stdout.flush()
    elif op == "tool.spec/1":
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
        sys.stdout.flush()
    elif op == "tool.spec/1":
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
    if op == "initialize/1":
        rid = req.get("id")
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
        sys.stdout.flush()
    elif op == "tool.spec/1":
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
    if op == "initialize/1":
        rid = req.get("id")
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
        sys.stdout.flush()
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
        sys.stdout.flush()
    elif op == "tool/1":
        sys.stdout.write("this is not valid json\n")
        sys.stdout.flush()
        sys.exit(0)
"#;

/// A one-shot fixture that declares a tool with UNKNOWN `category` and
/// `permission` string tags (`"deploy"` and `"yolo"` -- neither is a variant
/// this host knows). Board item `01M03VJPRT8629CYR8JK4A8JPF`: discovery must
/// LOAD the tool, degrading `category` to `ToolCategory::Execute` and
/// `permission` to `PermissionClass::Dangerous` (the most restrictive
/// values), NOT fail closed on the unknown tags. `tool/1` answers normally so
/// the same fixture also proves the loaded tool still invokes.
pub const UNKNOWN_TAG_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

req = json.loads(sys.stdin.read())
op = req.get("op")

if op == "tool.spec/1":
    print(json.dumps({
        "id": "acme.unknown-tags",
        "version": "0.1.0",
        "tools": [{
            "name": "frob",
            "description": "declares unknown category/permission tags",
            "schema": {"type": "object"},
            "category": "deploy",
            "permission": "yolo",
        }],
    }))
elif op == "tool/1":
    print(json.dumps({
        "ok": True,
        "blocks": [{"type": "text", "text": "frobbed"}],
        "is_error": False,
    }))
"#;

/// A one-shot fixture whose `tool/1` answer mixes a KNOWN content block
/// (`{"type":"text","text":"kept"}`) with an UNKNOWN block type
/// (`{"type":"quantum","data":...}`). Board item `01M03VJPRT8629CYR8JK4A8JPF`:
/// the call must SUCCEED with the known block, and the dropped unknown block
/// must be surfaced -- the host appends a summary `ContentBlock::Text`
/// naming the dropped count and the unknown type tag, and sets `is_error`.
pub const UNKNOWN_BLOCK_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

req = json.loads(sys.stdin.read())
op = req.get("op")

if op == "tool.spec/1":
    print(json.dumps({
        "id": "acme.unknown-block",
        "version": "0.1.0",
        "tools": [{
            "name": "mix",
            "description": "returns a known and an unknown block type",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }))
elif op == "tool/1":
    print(json.dumps({
        "ok": True,
        "blocks": [
            {"type": "text", "text": "kept"},
            {"type": "quantum", "data": "future block this host does not know"},
        ],
        "is_error": False,
    }))
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

# The persistent transport now sends `initialize/1` as the FIRST line of the
# session (board item 01M03VK7MRPSAVWMW7YNYPRPGT). Read that one line in full
# (it is small -- well under a pipe buffer) and answer it so the handshake
# completes and the host proceeds to the `tool/1` call this fixture wedges on.
first = sys.stdin.readline()
req = json.loads(first)
op = req.get("op")
if op == "tool.spec/1":
    # One-shot discovery path: the host closes stdin after writing the
    # discovery request, so this is the only line. Answer the manifest.
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
    sys.exit(0)
elif op == "initialize/1":
    sys.stdout.write(json.dumps({"id": req.get("id"), "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
    sys.stdout.flush()
else:
    time.sleep(3600)
    sys.exit(0)

# Now read a SMALL prefix only for the next request: a large `tool/1` does not
# fit, `json.loads` raises, and we WEDGE -- stop draining stdin, stay alive,
# keep stdout open. The host's `write_all` fills the pipe buffer and blocks --
# the hang the per-call write deadline exists to bound.
chunk = sys.stdin.read(100)
try:
    req = json.loads(chunk)
except Exception:
    # Incomplete JSON -> a large tool/1 request the host is still writing.
    time.sleep(3600)
    sys.exit(0)
# A small tool/1 (if any) -- still wedge rather than answer.
time.sleep(3600)
"#;

/// A persistent-transport fixture that returns a `tool/1` answer mixing a
/// KNOWN `text` block with an UNKNOWN `quantum` block type -- the
/// persistent-channel proof that the `ContentBlock` drop+count+surface
/// degradation (which lives in the shared `RawToolResult::classify`) applies
/// over the NDJSON transport too, not only the one-shot path. The manifest
/// uses only known tags so discovery is clean; the unknown tag appears only
/// in the `tool/1` answer body.
pub const PERSISTENT_UNKNOWN_BLOCK_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.mix",
        "version": "0.1.0",
        "tools": [{
            "name": "mix",
            "description": "returns a known block and an unknown block type",
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
        sys.stdout.flush()
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        resp = {
            "id": rid,
            "ok": True,
            "blocks": [
                {"type": "text", "text": "kept"},
                {"type": "quantum", "q": "?"},
            ],
            "is_error": False,
        }
        sys.stdout.write(json.dumps(resp) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// A one-shot fixture that declares a KNOWN `required_host_caps` value
/// (`["subagent"]`) in its `tool.spec/1` manifest -- board item
/// `01M03VJXARFHSDAGHFXGCWKJTY`: discovery must LOAD the plugin and map the
/// declared cap into `PluginManifest::required_host_caps` verbatim. (Whether
/// the host then OFFERS the cap is the `conway` builder's gate, proven in
/// `crates/conway/tests/builder.rs`; this fixture proves only that the wire
/// carries the field and `discover` maps it.)
pub const CAP_REQUIRED_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

req = json.loads(sys.stdin.read())
op = req.get("op")

if op == "tool.spec/1":
    print(json.dumps({
        "id": "acme.needs-subagent",
        "version": "0.1.0",
        "required_host_caps": ["subagent"],
        "tools": [{
            "name": "frob",
            "description": "declares a required host cap",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }))
elif op == "tool/1":
    print(json.dumps({
        "ok": True,
        "blocks": [{"type": "text", "text": "frobbed"}],
        "is_error": False,
    }))
"#;

/// A one-shot fixture that declares an UNKNOWN `required_host_caps` tag
/// (`"quantum-cap"` -- not a variant this host's `HostCapability` enum
/// recognizes) -- board item `01M03VJXARFHSDAGHFXGCWKJTY`: a capability
/// requirement is a GATE, not a value that degrades, so an unknown cap tag
/// must FAIL CLOSED at parse (the plugin is refused), unlike the
/// `ToolCategory`/`PermissionClass`/`ContentBlock` degradation table. The
/// `required_host_caps` field has `#[serde(default)]`, so OMITTING it parses
/// as empty; naming an unknown value is the fail-closed case this fixture
/// proves.
pub const UNKNOWN_CAP_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

req = json.loads(sys.stdin.read())
op = req.get("op")

if op == "tool.spec/1":
    print(json.dumps({
        "id": "acme.needs-quantum",
        "version": "0.1.0",
        "required_host_caps": ["quantum-cap"],
        "tools": [{
            "name": "frob",
            "description": "declares an unknown required host cap",
            "schema": {"type": "object"},
            "category": "read",
            "permission": "safe",
        }],
    }))
elif op == "tool/1":
    print(json.dumps({
        "ok": True,
        "blocks": [{"type": "text", "text": "frobbed"}],
        "is_error": False,
    }))
"#;

// ----- initialize/1 handshake fixtures (board item 01M03VK7MRPSAVWMW7YNYPRPGT)
//       Each fixture is a persistent-transport line loop that handles
//       `tool.spec/1` (the one-shot discovery path -- the host closes stdin
//       after writing the discovery request, so the loop runs once and exits
//       on EOF), `initialize/1` (the one-time persistent-session handshake),
//       and `tool/1` (the persistent call). The `initialize/1` answer is what
//       each fixture varies to cover the version-negotiation table's rows.

/// The accept-branch fixture: answers `initialize/1` with matching `major=1`,
/// `minor_min=1`, the `tool/1` point version, AND an UNKNOWN extra field
/// (`"future_field": "bonus"`) -- proving the compatibility table's accept
/// branch / forward-compat rule: a newer plugin's extra field is ignored-and-
/// counted, NOT rejected (acceptance criterion 1 AND criterion 4 share this
/// fixture -- criterion 1 proves the session opens and `tool/1` proceeds;
/// criterion 4's "counted/surfaced" assertion is pinned by the unit test
/// `wire::tests::initialize_answer_with_unknown_field_is_accepted_and_counted`).
/// `tool/1` for `greet` replies `"hello, <name>"` so criterion 1's "proceeds
/// to serve tool/1 calls" is asserted end-to-end.
pub const PERSISTENT_HANDSHAKE_OK_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.handshake-ok",
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
    if op == "initialize/1":
        # Accept branch: matching major, satisfied minor_min, AND an unknown
        # extra field (`future_field`) -- ignored-and-counted by the host.
        resp = {
            "id": rid, "ok": True, "major": 1, "minor_min": 1,
            "points": [{"name": "tool/1", "version": 1}],
            "future_field": "bonus",
        }
        sys.stdout.write(json.dumps(resp) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        tool = req.get("tool")
        args = req.get("arguments", {})
        if tool == "greet":
            name = args.get("name", "")
            resp = {"id": rid, "ok": True, "blocks": [{"type": "text", "text": f"hello, {name}"}], "is_error": False}
        else:
            resp = {"id": rid, "ok": False, "error": {"kind": "invalid_arguments", "detail": f"unknown tool {tool}"}}
        sys.stdout.write(json.dumps(resp) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// The major-mismatch fixture: answers `initialize/1` with `major=2` (the
/// host's `HOST_WIRE_MAJOR` is `1`) -- acceptance criterion 2: the host must
/// REFUSE to load with a typed `HandshakeRefused` naming both majors and
/// "major mismatch". One-shot discovery (`tool.spec/1`) still succeeds so the
/// refusal is specifically the handshake's, not a discovery failure.
pub const PERSISTENT_HANDSHAKE_MAJOR_MISMATCH_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.handshake-major",
        "version": "0.1.0",
        "tools": [{
            "name": "frob",
            "description": "a tool the host will never call (major mismatch refuses load)",
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 2, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// The unsatisfied-minor_min fixture: answers `initialize/1` with `major=1`
/// but `minor_min=2` (the host's `HOST_WIRE_MINOR` is `1`) -- acceptance
/// criterion 3: the host must REFUSE to load with a typed `HandshakeRefused`
/// naming the required minor and the host's minor.
pub const PERSISTENT_HANDSHAKE_MINOR_MIN_TOO_HIGH_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.handshake-minor",
        "version": "0.1.0",
        "tools": [{
            "name": "frob",
            "description": "a tool the host will never call (minor_min unsatisfied refuses load)",
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 2, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// The close-without-answering fixture: reads the `initialize/1` request then
/// exits (closing stdout) WITHOUT answering -- acceptance criterion 5: the
/// host must fail closed with a typed error within `timeout_ms`, never hang.
/// The reader task's EOF path `kill_all`s the session as `SessionDied`, the
/// initialize sender is dropped, and `framed_round_trip` surfaces the typed
/// death reason. One-shot discovery still answers `tool.spec/1` so the
/// failure is specifically the handshake's.
pub const PERSISTENT_HANDSHAKE_NO_ANSWER_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.handshake-no-answer",
        "version": "0.1.0",
        "tools": [{
            "name": "frob",
            "description": "a tool the host will never call (no initialize answer)",
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
    elif op == "initialize/1":
        # Close stdout without answering -- the host must fail closed within
        # timeout_ms, never hang.
        sys.exit(0)
    else:
        sys.exit(0)
"#;

/// The host-version-is-informational fixture: answers `initialize/1`
/// correctly (major=1, minor_min=1) AND reads the `host.version` the host
/// sent, reflecting it back as the `tool/1` text result -- the hazard test:
/// `host.version` is on the wire (the plugin receives it) but is NEVER
/// branched on by the host's negotiation (which compares ONLY `major` and
/// `minor_min`). The negotiation outcome ("load") is the same regardless of
/// what version string the host sent; this fixture proves the version reaches
/// the plugin, and the load succeeds. The structural guarantee -- that
/// `initialize` never compares `host.version` -- is in the code:
/// `PersistentSession::initialize` references `HOST_WIRE_MAJOR`/
/// `HOST_WIRE_MINOR` only; `host.version` is serialized into the request by
/// `PersistentInitializeRequest::new` and never read back.
pub const PERSISTENT_HANDSHAKE_REFLECTS_HOST_VERSION_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

host_version_seen = None

def manifest():
    return {
        "id": "acme.handshake-reflect",
        "version": "0.1.0",
        "tools": [{
            "name": "echo",
            "description": "reflects the host.version seen at initialize",
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
    if op == "initialize/1":
        host_version_seen = req.get("host", {}).get("version")
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}]}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        resp = {"id": rid, "ok": True, "blocks": [{"type": "text", "text": str(host_version_seen)}], "is_error": False}
        sys.stdout.write(json.dumps(resp) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

// ---------------------------------------------------------------------
// permission.policy/1 fixtures (board item `01M03VKJG7JJ0JEKY265WA7MJ7`).
// Every fixture below answers `initialize/1` declaring BOTH `tool/1` and
// `permission.policy/1` (so the host exchanges the policy after the
// handshake), then answers `permission.policy/1` with the shape the named
// case exercises. A fixture that declares the point at an unsupported
// version never gets a `permission.policy/1` request -- the host refuses at
// discover before sending one.
// ---------------------------------------------------------------------

/// A persistent-transport fixture that declares `permission.policy/1` at
/// version 1 and answers with a NARROWING policy: `greet` -> `prompt` (force
/// the operator's gate), `bash` -> `deny` (refuse outright), `read` ->
/// `abstain`. The host stores the rules and `SubprocessPlugin::
/// permission_rules` surfaces them as `PluginPermissionRule`s the `conway`
/// facade installs as `PatternOrigin::Plugin` deny/prompt rules.
pub const PERSISTENT_POLICY_OK_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.policy-ok",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
            "category": "read",
            "permission": "safe",
        }],
    }

POLICY = [
    {"tool": "greet", "verdict": "prompt", "reason": "greet should be approved"},
    {"tool": "bash", "verdict": "deny", "reason": "bash is refused by this plugin"},
    {"tool": "read", "verdict": "abstain"},
]

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    rid = req.get("id")
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "permission.policy/1", "version": 1}]}) + "\n")
    elif op == "permission.policy/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "rules": POLICY}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        args = req.get("arguments", {})
        name = args.get("name", "")
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": f"hello, {name}"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// Declares `permission.policy/1` at version 2 (the host speaks version 1)
/// -- the host must REFUSE to load with a typed `HandshakeRefused` naming the
/// version mismatch (the participant rule). The fixture never receives a
/// `permission.policy/1` request because the host refuses before sending
/// one.
pub const PERSISTENT_POLICY_VERSION_MISMATCH_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.policy-ver",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "permission.policy/1", "version": 2}]}) + "\n")
    elif op == "permission.policy/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "rules": []}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": "hi"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// Declares `permission.policy/1` at version 1 but answers with a structurally
/// malformed body (`rules` is not an array) -- the host must fail CLOSED with
/// a typed `HandshakeMalformed`, never silently no-op (acceptance criterion 3).
pub const PERSISTENT_POLICY_MALFORMED_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.policy-bad",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "permission.policy/1", "version": 1}]}) + "\n")
    elif op == "permission.policy/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "rules": "not-an-array"}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": "hi"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// Declares `permission.policy/1` at version 1 but answers `ok:false` (the
/// plugin deliberately declines to declare a policy) -- surfaces as
/// `HandshakeRefused`, the categorical twin of `initialize/1`'s own
/// `ok:false`-with-error refusal.
pub const PERSISTENT_POLICY_REFUSED_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.policy-no",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "permission.policy/1", "version": 1}]}) + "\n")
    elif op == "permission.policy/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": "I decline to declare a policy"}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": "hi"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

// ---------------------------------------------------------------------
// observe/1 + status.declare/1 / status/1 fixtures (board item
// `01M03VKQ738DTGHHK2C4RWXC0E`). Every fixture below answers
// `initialize/1` declaring `tool/1` plus the observer point(s) the named
// case exercises, then answers the engagement request(s). An observe
// fixture records every `observe/1` notification it receives on stdin to a
// file whose path the test sets in the `OBSERVE_LOG` env var (the child
// inherits the parent's env) -- the load-bearing proof the plugin actually
// received the host's one-way notifications. A status fixture PUSHES
// `status/1` no-`id` lines on stdout right after answering
// `status.declare/1`, including an UNKNOWN `ResultStatus` tag so the
// degrade-to-`Failed` rule is exercised end-to-end.
// ---------------------------------------------------------------------

/// A persistent-transport fixture that declares `observe/1` at version 1 and
/// subscribes with selector `["*"]` (every `Event`). Every `observe/1`
/// notification the plugin receives on stdin (a no-`id` line whose `op` is
/// `"observe/1"`) is appended to the file named by the `OBSERVE_LOG` env var,
/// one JSON object per line -- the load-bearing proof the host's one-way
/// notifications actually reached the plugin. The fixture keeps serving
/// `tool/1` for `greet` so the test can prove the session is still alive
/// AFTER receiving notifications (an observer must not error the session).
/// `tool.spec/1` discovery still answers so the one-shot discovery path
/// succeeds.
pub const PERSISTENT_OBSERVE_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json, os

def manifest():
    return {
        "id": "acme.observe",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
            "category": "read",
            "permission": "safe",
        }],
    }

log_path = os.environ.get("OBSERVE_LOG")

def record(obj):
    if log_path:
        with open(log_path, "a") as f:
            f.write(json.dumps(obj) + "\n")

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    rid = req.get("id")
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "observe/1", "version": 1}]}) + "\n")
    elif op == "observe/1":
        # Distinguish the ENGAGEMENT (carries an `id`, answer with the
        # selector) from a one-way NOTIFICATION (no `id`, record it WITHOUT
        # answering -- an unknown `event` tag is recorded too, the plugin
        # ignores it, the session stays alive).
        if "id" in req:
            sys.stdout.write(json.dumps({"id": rid, "ok": True, "events": ["*"]}) + "\n")
        else:
            record(req)
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        args = req.get("arguments", {})
        name = args.get("name", "")
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": f"hello, {name}"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// Declares `observe/1` at version 2 (the host speaks version 1) -- the host
/// must DEGRADE (load WITHOUT the point, warn), NOT refuse. The fixture never
/// receives an `observe/1` engagement request because the host does not send
/// one for an unsupported version. It still serves `tool/1` so the test can
/// assert the plugin LOADED and serves calls despite the degrade.
pub const PERSISTENT_OBSERVE_VERSION_MISMATCH_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.observe-ver",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "observe/1", "version": 2}]}) + "\n")
    elif op == "observe/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "events": ["*"]}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": "hi"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// A persistent-transport fixture that declares `observe/1` at version 1 and
/// subscribes with a `Tags` selector `["turn_started"]` -- ONLY the
/// `turn_started` tag (plus the always-forwarded `lagged`). Every `observe/1`
/// notification the plugin receives on stdin is appended to the file named by
/// the `OBSERVE_TAGS_LOG` env var (a DISTINCT env var from the `["*"]`
/// fixture's `OBSERVE_LOG`, so the two tests cannot clobber each other's log
/// path when they run in parallel). The host filters by the declared selector
/// BEFORE forwarding -- a non-matching `Event` is dropped at the host and never
/// reaches this stdin -- so the number of lines the plugin records is the
/// proof the filter ran end-to-end. Keeps serving `tool/1` for `greet` so the
/// test can prove the session is still alive AFTER a non-matching event was
/// filtered.
pub const PERSISTENT_OBSERVE_TAGS_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json, os

def manifest():
    return {
        "id": "acme.observe-tags",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
            "category": "read",
            "permission": "safe",
        }],
    }

log_path = os.environ.get("OBSERVE_TAGS_LOG")

def record(obj):
    if log_path:
        with open(log_path, "a") as f:
            f.write(json.dumps(obj) + "\n")

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    rid = req.get("id")
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "observe/1", "version": 1}]}) + "\n")
    elif op == "observe/1":
        # Distinguish the ENGAGEMENT (carries an `id`, answer with the
        # selector) from a one-way NOTIFICATION (no `id`, record it). A
        # Tags selector `["turn_started"]`: the host forwards ONLY
        # `turn_started` (and the always-forwarded `lagged`); every other
        # `Event` tag is filtered out at the host and never reaches here.
        if "id" in req:
            sys.stdout.write(json.dumps({"id": rid, "ok": True, "events": ["turn_started"]}) + "\n")
        else:
            record(req)
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        args = req.get("arguments", {})
        name = args.get("name", "")
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": f"hello, {name}"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// A persistent-transport fixture that declares `status.declare/1` at version
/// 1 and, immediately AFTER answering the engagement, PUSHES two `status/1`
/// no-`id` notifications on stdout: `{"key":"build","status":"completed",
/// "value":"green"}` (a KNOWN tag) and `{"key":"lint","status":"quantum",
/// "value":"?"}` (an UNKNOWN `ResultStatus` tag the host must degrade to
/// `Failed`). The host's reader routes both to the notification channel and
/// the handler stores them; the test polls `status_contributions()` and
/// asserts `build` -> `Completed` and `lint` -> `Failed` (degraded, never
/// `Completed`). The fixture keeps serving `tool/1` so the test can prove the
/// session is still alive after pushing notifications.
pub const PERSISTENT_STATUS_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json, time

def manifest():
    return {
        "id": "acme.status",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "status.declare/1", "version": 1}]}) + "\n")
    elif op == "status.declare/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "keys": [{"key": "build", "max_len": 80, "ttl_ms": 5000}, {"key": "lint"}]}) + "\n")
        sys.stdout.flush()
        # PUSH the status/1 notifications NOW (no `id`, one-way). A tiny
        # sleep lets the host's reader + handler drain them before the test
        # polls.
        sys.stdout.write(json.dumps({"op": "status/1", "key": "build", "status": "completed", "value": "green"}) + "\n")
        sys.stdout.write(json.dumps({"op": "status/1", "key": "lint", "status": "quantum", "value": "?"}) + "\n")
        sys.stdout.flush()
        time.sleep(0.05)
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": "hi"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;

/// Declares `status.declare/1` at version 2 (the host speaks version 1) -- the
/// host must DEGRADE (load WITHOUT the point, warn), NOT refuse. The fixture
/// never receives a `status.declare/1` engagement request and pushes no
/// `status/1` notifications. It still serves `tool/1` so the test can assert
/// the plugin LOADED despite the degrade.
pub const PERSISTENT_STATUS_VERSION_MISMATCH_PLUGIN: &str = r#"#!/usr/bin/env python3
import sys, json

def manifest():
    return {
        "id": "acme.status-ver",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
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
    if op == "initialize/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "major": 1, "minor_min": 1, "points": [{"name": "tool/1", "version": 1}, {"name": "status.declare/1", "version": 2}]}) + "\n")
    elif op == "status.declare/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "keys": []}) + "\n")
    elif op == "tool.spec/1":
        sys.stdout.write(json.dumps(manifest()) + "\n")
    elif op == "tool/1":
        sys.stdout.write(json.dumps({"id": rid, "ok": True, "blocks": [{"type": "text", "text": "hi"}], "is_error": False}) + "\n")
    else:
        sys.stdout.write(json.dumps({"id": rid, "ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}) + "\n")
    sys.stdout.flush()
"#;
