//! Builds the argv `conway::plugin::BashTool::with_launcher` spawns --
//! `sandbox-exec` on macOS, `bwrap` on Linux -- each wrapping the identical
//! `/bin/bash -c <command>` invocation `conway.shell`'s own `bash` tool runs
//! bare. **Mechanism, not policy (P-14): neither function below reads
//! `command` to decide anything.** The OS primitive is handed the WHOLE
//! command string verbatim, exactly as `conway.shell`'s own `default_
//! launcher` does -- the containment guarantee is the kernel/Seatbelt's,
//! never a Rust-side inspection of what the shell is about to do.
//!
//! **What is confined, and what is not -- the ruling this crate implements
//! (operator ruling 2026-09-01, decision `01M1FQG08GDQ71984T0W0RJ019`):
//! WRITES only.** Both profiles below deny filesystem writes everywhere
//! except under the confinement root; neither restricts reads or network
//! reachability at all. See `docs/plugins/confine.md`'s "What this does and
//! does not confine" section for the full, disclosed boundary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway::plugin::Launcher;
use tokio::process::Command;

/// macOS: `sandbox-exec -p <profile> /bin/bash -c <command>`.
///
/// **The profile, read literally.** `(allow default)` starts from "nothing
/// restricted" (reads and network stay open, per the ruling above);
/// `(deny file-write*)` then blanket-denies every filesystem write;
/// `(allow file-write* (subpath "<root>"))` re-opens writes ONLY under the
/// confinement root. Seatbelt evaluates rules in file order and the LAST
/// matching rule for a given operation+path wins (the same "more specific,
/// later rule overrides" idiom widely used in shipped `sandbox-exec`
/// profiles), so a write under `root` matches both the blanket deny and the
/// later, narrower allow, and the allow wins; a write anywhere else matches
/// only the deny.
///
/// `root` is embedded as a Scheme string literal -- [`scheme_string_literal`]
/// escapes it, since a root path containing `"` or `\` would otherwise
/// terminate the literal early or corrupt the profile Seatbelt parses.
///
/// `#[cfg(target_os = "macos")]`: this crate's `tool::build_launcher` is the
/// only caller, and it is itself gated identically -- keeping the gate here
/// too (rather than relying solely on the caller's) is what keeps this
/// function from becoming a silent dead-code warning on any OTHER target,
/// where it would compile but never be reachable.
#[cfg(target_os = "macos")]
pub(crate) fn sandbox_exec_launcher(binary: PathBuf, root: PathBuf) -> Launcher {
    Arc::new(move |command: &str, cwd: &Path| {
        let profile = format!(
            "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* (subpath {}))\n",
            scheme_string_literal(&root)
        );
        let mut cmd = Command::new(&binary);
        cmd.arg("-p")
            .arg(profile)
            .arg("/bin/bash")
            .arg("-c")
            .arg(command)
            .current_dir(cwd);
        cmd
    })
}

/// Renders `path` as a double-quoted Scheme string literal, escaping `\`
/// and `"` -- the two characters that would otherwise let a pathological
/// root path (containing either) break out of the literal Seatbelt parses
/// `-p`'s argument as. `#[cfg(target_os = "macos")]`: [`sandbox_exec_launcher`]
/// is its only caller.
#[cfg(target_os = "macos")]
fn scheme_string_literal(path: &Path) -> String {
    let escaped = path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Linux: `bwrap --ro-bind / / --bind <root> <root> --dev /dev --proc /proc
/// --die-with-parent -- /bin/bash -c <command>`.
///
/// **Read literally, matching the macOS profile's own shape.** `--ro-bind /
/// /` mounts the entire host filesystem back over itself, read-only --
/// every existing path is still reachable (reads unaffected, per the
/// ruling) but no longer writable through this mount; `--bind <root>
/// <root>` re-mounts the confinement root over the SAME path, read-write,
/// which — because bind mounts apply in argument order and a later one at
/// an overlapping path takes over that path — restores write access
/// exactly there and nowhere else. `--dev /dev`/`--proc /proc` give the
/// sandboxed process a working `/dev`/`/proc` (bwrap's own default when
/// none is bound is an EMPTY namespace, which breaks an ordinary shell);
/// `--die-with-parent` stops an orphaned sandboxed process from outliving
/// this tool's own cancellation/timeout handling. Network is left alone --
/// no `--unshare-net`/`--share-net` flag is passed at all, so the sandboxed
/// process shares the host's network namespace exactly as it would
/// unsandboxed (reads/network unaffected, per the ruling).
///
/// **Declaration honesty (GP-14): this profile is unverified in this
/// worktree.** No `bwrap` binary was available to exercise it here; the
/// macOS `sandbox-exec` profile above IS exercised by this crate's own
/// `#[cfg(target_os = "macos")]` test. See `docs/plugins/confine.md` for
/// exactly which OS this crate's own containment claim is proven on, and
/// this crate's `#[cfg(target_os = "linux")]` test for the exact skip
/// condition and printed reason when `bwrap` is absent.
///
/// `#[cfg(target_os = "linux")]` for the identical reason
/// [`sandbox_exec_launcher`]'s own doc gives for its matching gate.
#[cfg(target_os = "linux")]
pub(crate) fn bwrap_launcher(binary: PathBuf, root: PathBuf) -> Launcher {
    Arc::new(move |command: &str, cwd: &Path| {
        let mut cmd = Command::new(&binary);
        cmd.arg("--ro-bind")
            .arg("/")
            .arg("/")
            .arg("--bind")
            .arg(&root)
            .arg(&root)
            .arg("--dev")
            .arg("/dev")
            .arg("--proc")
            .arg("/proc")
            .arg("--die-with-parent")
            .arg("--")
            .arg("/bin/bash")
            .arg("-c")
            .arg(command)
            .current_dir(cwd);
        cmd
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn scheme_string_literal_escapes_backslash_and_quote() {
        let path = Path::new("/tmp/weird\"root\\dir");
        let literal = scheme_string_literal(path);
        assert_eq!(literal, "\"/tmp/weird\\\"root\\\\dir\"");
    }

    #[test]
    fn scheme_string_literal_passes_an_ordinary_path_through_quoted() {
        let path = Path::new("/tmp/plain-root");
        assert_eq!(scheme_string_literal(path), "\"/tmp/plain-root\"");
    }
}
