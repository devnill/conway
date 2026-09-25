//! One function: [`script_command`], the single implementation of a
//! decision a handful of test files in this workspace each need to make
//! correctly -- one copy rather than several that could silently drift --
//! namely `conway-tools/tests/kill_group.rs` (both its own spawn
//! sites), `conway-tools/tests/hook_runner.rs` (its `warm` helper only --
//! see below), `conway-cli/tests/subprocess_plugins.rs`, and
//! `conway-plugin-claude/tests/end_to_end.rs`. Each of those writes a small
//! fixture script to a fresh `TempDir` at test time and then runs it; this
//! crate exists so every one of them makes the same exec-safety choice about
//! HOW to run it, rather than independent restatements that could silently
//! drift.
//!
//! Dev-dependency only. Nothing under any crate's `src/` may depend on this
//! -- it exists to keep a test's own fixture-launch decision out of
//! production code, not to become part of it.
//!
//! # Two call sites that deliberately do NOT use this, and why
//!
//! Board item `01M3CMQZZCSRF7YB9GEMA92M0C` set out to convert every
//! `Command::new(path)` site sharing this shape, including
//! `conway-tools/tests/hook_runner.rs::warm_hanging_fixture` and
//! `conway-tools/tests/child_session_grace.rs::warm`. Both were converted,
//! then reverted, on direct measurement: each exists SPECIFICALLY to
//! `execve` the exact freshly-written fixture file once, ahead of an
//! immediately-following, unmodifiable production spawn of that SAME file
//! (`ProcessHookRunner::run` / `ChildSession::spawn`), so that a macOS-only
//! first-exec tax on a brand-new file (board item
//! `01M09MPZ9C188AHNBKWEJ3CEQA`; empirically real, not theoretical -- see
//! those two functions' own doc comments) lands during the warm-up rather
//! than inside the production call's own tight timing budget (2000ms and
//! 1200ms respectively). Routing either warm-up through `script_command`
//! avoids the `execve` of the fixture file entirely (the interpreter merely
//! `open()`s it), so the fixture is never actually warmed by it, and that
//! tax is deferred into the timed call instead. Measured, not assumed: with
//! `script_command` in either helper, the downstream timed tests failed
//! reliably (`hang_trapping_sigterm_is_killed_and_reported_as_timed_out`,
//! `backgrounded_grandchild_does_not_survive_the_timeout_path`, and
//! `the_first_ordinary_round_trip_gets_the_warm_up_budget` all failed under
//! `cargo test`'s default parallelism); reverted to `Command::new(path)`,
//! all three passed repeatedly (20+ consecutive runs each). Both of those
//! two helpers' own spawn failures are unconditionally discarded (`if let
//! Ok(..) = ..`), so neither ever turns an OS-level race into a panic the
//! way `kill_group.rs`'s ORIGINAL, unfixed spawns did -- converting them
//! traded a real, working timing guarantee for protection against a failure
//! mode that was already harmless. `conway-tools/tests/hook_runner.rs`'s
//! OTHER helper, `warm`, has no such tight downstream budget (its callers'
//! `timeout_ms` is 5000, and none assert on elapsed wall time), and
//! converting it is safe -- verified by dozens of repeated runs with no
//! regression.
//!
//! # The race this closes, and why it is a race and not a bug in the fixture
//!
//! A test that writes a small script and then runs it can fail on Linux for
//! a reason that has nothing to do with what it is testing. When a program
//! launches a script, the kernel reads its `#!` line and actually launches
//! the interpreter named there, handing it the script; to do that it
//! re-opens the script file itself. If that file was written moments ago, on
//! a loaded machine that re-open can find the file still considered busy,
//! and the launch fails with `ETXTBSY` -- "Text file busy". The test then
//! panics before it has tested anything. It is a timing race, so it appears
//! and disappears with machine load, which is what makes it corrosive: a
//! suite that fails once in fifty runs teaches people to re-run rather than
//! read.
//!
//! **The mechanism was established by direct reproduction, not guessed at**
//! (board item `01M3CCB39A1SKWFQEKC2CZK7S9`, the fix originally landed only
//! for `kill_group.rs`; this crate is that fix generalised to the other
//! exposures per board item `01M3CMQZZCSRF7YB9GEMA92M0C` that turned out to
//! be safe to generalise -- see the section above for the two that did not).
//! A minimal reproduction outside any test suite (write, chmod, `spawn()`,
//! in a tight loop across 48 threads on a 10-core Linux box under synthetic
//! CPU load, no tokio) measured a genuine, repeatable `ETXTBSY` rate against
//! each candidate fix, 96,000 launch attempts per configuration:
//!
//! - launching the freshly-written file directly (`Command::new(path)`,
//!   the shape every one of these call sites used before this crate
//!   existed): 1,939-2,688 failures per 96,000 across six runs (~2-3%) --
//!   confirms the race is real and load-dependent, not a defect in any
//!   fixture-writing helper.
//! - flushing the file to disk before launching (`f.sync_all()` before
//!   `drop(f)`): 65,518-66,860 per 96,000 (~68%) -- dramatically WORSE.
//!   Ruled out.
//! - write-then-rename (write to a temporary name, `fs::rename` into
//!   place, then exec the final name): no better than baseline, because the
//!   kernel's busy check is per-INODE and a rename never changes which
//!   inode gets launched. Ruled out.
//! - naming the interpreter explicitly and passing the script as its
//!   argument (`Command::new(interpreter).arg(path)`, what
//!   [`script_command`] does): **0 failures across 288,000 attempts.**
//!
//! The last one works because it never asks the kernel to execve the
//! freshly-written file at all. The interpreter is a binary that has been on
//! disk for months; it merely `open()`s the script to READ it, and reading is
//! not subject to the write-busy check that `execve()` is. It is the same
//! substitution the kernel's own binfmt_script handler performs anyway, done
//! one level earlier.
//!
//! # Why this takes the interpreter as a parameter, not a constant
//!
//! The call sites are NOT uniform: `kill_group.rs`'s and `hook_runner.rs`'s
//! own fixtures are POSIX `sh` scripts (`#!/bin/sh`), while
//! `subprocess_plugins.rs`'s and `end_to_end.rs`'s are Python
//! (`#!/usr/bin/env python3`). Hard-coding `/bin/sh` here would hand a
//! Python script to a shell and turn a flake into a broken test.
//! `Command::new("python3")` resolves through
//! `PATH` exactly as `#!/usr/bin/env python3` does, so callers pass
//! `"python3"` (never a hard-coded absolute path such as
//! `/usr/bin/python3`, which would silently change which interpreter runs)
//! to preserve that resolution behaviour unchanged.
use std::path::Path;

/// Builds a `Command` that runs the fixture script at `path` by invoking
/// `interpreter path` explicitly (e.g. `"/bin/sh"` or `"python3"`), rather
/// than `Command::new(path)`'s direct `execve` of a file the caller's own
/// fixture-writer may have closed only moments earlier. See the module doc
/// for the measured mechanism and the numbers behind it.
///
/// Structural, not a retry: nothing here retries, nothing sleeps, and the
/// number of times the caller ends up spawning is unchanged -- only the exec
/// target itself changes, from "the file this process just wrote" to "a
/// stable binary already on disk that merely reads the file."
///
/// `interpreter` is resolved through `PATH` exactly as a `#!` line naming
/// the same interpreter would be (`Command::new` never treats its argument
/// as a path unless it contains a separator) -- passing `"python3"` here
/// preserves an existing `#!/usr/bin/env python3` fixture's own
/// interpreter-resolution behaviour rather than pinning it to one location.
pub fn script_command(interpreter: &str, path: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(interpreter);
    command.arg(path);
    command
}
