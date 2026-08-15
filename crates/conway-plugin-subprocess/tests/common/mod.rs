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
