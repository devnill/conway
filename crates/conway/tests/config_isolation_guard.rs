//!'s guard: fails if a future test
//! starts reading ambient config again.
//!
//! ## What "ambient" means here, precisely
//!
//! `ConwayBuilder::from_config`, `ConwayBuilder::discover`, and
//! `config::load`/`LoadOptions::default()` all read
//! `$XDG_CONFIG_HOME/conway/settings.json` (or `~/.conway/settings.json`,
//! the invoking user's OWN file, unrelated to any test fixture) and deep-
//! merge it into whatever config the test believes it built. That is
//! precisely the defect this item closed: `crates/conway-cli/tests/
//! continuity.rs` and `oneshot_ask.rs` failed on any operator machine whose
//! real settings named a backend kind this facade does not link (board
//! item's own reproduction). `from_config_only`/`load_ignoring_xdg` are the
//! honest seam; this guard is what keeps a future in-process test call site
//! from silently reintroducing the ambient read the seam exists to avoid.
//!
//! ## Scope: in-process calls only
//!
//! A compiled-binary subprocess test (`crates/conway-cli/tests/common/
//! mod.rs`'s `command`/`run_conway`) is unaffected by this guard and by the
//! defect it guards against: that harness already redirects the SUBPROCESS's
//! own `XDG_CONFIG_HOME` explicitly, every invocation. The gap was always
//! in-process calls into the `conway` library, made by the TEST binary
//! itself, which that subprocess-level redirection never reaches.
//!
//! ## Why regex over source text, not a `#[deny]` lint
//!
//! There is no lint (`clippy` or otherwise) that can distinguish "calls the
//! ambient-reading constructor" from "calls the isolated one" -- both are
//! ordinary, well-typed function calls. A structural guard over the test
//! suite's own source text is the same mechanism `crates/conway/tests/
//! architecture_invariants.rs` and this crate's own
//! `crates/conway/src/config/mod.rs` (`config_module_never_names_a_network_
//! client_identifier`) already establish for this exact class of invariant
//! -- reused here, not invented.
//!
//! Comments and string literals are stripped before scanning (see
//! `strip_comments_and_strings`) so this module doc's own mentions of the
//! banned identifiers, above, do not trip the guard on itself.

use std::path::{Path, PathBuf};

/// The repository root, derived from this crate's manifest directory
/// (`crates/conway` -> up two) -- mirrors `architecture_invariants.rs`'s own
/// `repo_root` helper exactly.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> is always two levels below the repo root")
        .to_path_buf()
}

/// Every `.rs` file under `dir` (recursing into subdirectories, e.g.
/// `common/`, `support/`, `fixtures/` — a fixture `.rs` file, if one ever
/// existed, should be scanned too; only non-`.rs` fixture assets are
/// irrelevant and are naturally skipped by the extension filter).
fn rs_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            out.extend(rs_files_under(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    out
}

/// A minimal Rust comment/string stripper: replaces the contents of `//...`
/// line comments (which also covers `///`/`//!` doc comments -- both start
/// with `//`), `/* ... */` block comments (non-nested; sufficient for this
/// workspace's own style, which does not nest block comments), and string
/// literal bodies with spaces, preserving line structure (byte-for-byte
/// length per line) so a failure message can still cite an accurate line
/// number. This is what lets this guard's own module doc, and any test's
/// doc comment quoting the banned APIs by name, exist without self-tripping
/// or false-positiving.
fn strip_comments_and_strings(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_block_comment = false;
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
                result.push(' ');
                result.push(' ');
            } else if c == '\n' {
                result.push('\n');
            } else {
                result.push(' ');
            }
            continue;
        }
        if in_string {
            if c == '\\' {
                // Consume the escaped character too, so an escaped `"`
                // never prematurely ends the string.
                if let Some(next) = chars.next() {
                    result.push(if next == '\n' { '\n' } else { ' ' });
                }
                result.push(' ');
                continue;
            }
            if c == '"' {
                in_string = false;
                result.push(' ');
            } else if c == '\n' {
                result.push('\n');
            } else {
                result.push(' ');
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            result.push(' ');
            result.push(' ');
            for nc in chars.by_ref() {
                if nc == '\n' {
                    result.push('\n');
                    break;
                }
                result.push(' ');
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            result.push(' ');
            result.push(' ');
            continue;
        }
        if c == '"' {
            in_string = true;
            result.push(' ');
            continue;
        }
        result.push(c);
    }
    result
}

/// The banned in-process, ambient-reading call patterns, and why each one
/// is banned (all read `$XDG_CONFIG_HOME`/`~/.conway/settings.json`
/// unconditionally unless the caller explicitly isolates `env`, which none
/// of these three do on their own).
const BANNED_PATTERNS: &[(&str, &str)] = &[
    (
        "ConwayBuilder::from_config(",
        "reads the ambient XDG/user layer unconditionally -- use \
         ConwayBuilder::from_config_only(..) for an in-process test that \
         wants isolation",
    ),
    (
        "ConwayBuilder::discover(",
        "runs the full five-source discovery chain, including the ambient \
         XDG/user layer, against this TEST process's own real environment",
    ),
    (
        "LoadOptions::default()",
        "seeds `env` from this test process's own std::env::vars() and \
         resolves the XDG layer against its real $HOME -- construct \
         LoadOptions explicitly with an isolated `env` map (see \
         crates/conway/tests/support/mod.rs::isolated_env) or call \
         config::load_ignoring_xdg instead",
    ),
];

/// Files this guard does not scan: itself (its own module doc and
/// `BANNED_PATTERNS` array legitimately name every banned identifier as
/// plain source text, which `strip_comments_and_strings` only protects
/// against inside comments/strings, not inside a `&str` array literal like
/// `BANNED_PATTERNS` itself).
const SELF_EXCLUDED_FILE: &str = "config_isolation_guard.rs";

#[test]
fn no_in_process_test_reads_ambient_config() {
    let dirs = [
        repo_root().join("crates/conway/tests"),
        repo_root().join("crates/conway-cli/tests"),
    ];

    let mut violations: Vec<String> = Vec::new();
    for dir in &dirs {
        for path in rs_files_under(dir) {
            if path.file_name().and_then(|n| n.to_str()) == Some(SELF_EXCLUDED_FILE) {
                continue;
            }
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let scrubbed = strip_comments_and_strings(&content);
            for (needle, reason) in BANNED_PATTERNS {
                if scrubbed.contains(needle) {
                    let line = scrubbed
                        .lines()
                        .position(|l| l.contains(needle))
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    violations.push(format!(
                        "{}:{line}: calls `{needle}` -- {reason}",
                        path.display()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "in-process test(s) read ambient config; every violation must switch to the \
         isolated seam instead:\n{}",
        violations.join("\n")
    );
}
