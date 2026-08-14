#!/usr/bin/env zsh
# The documented way to run the full suite -- captures the exit code
# correctly, unlike a hand-typed pipeline.
#
# WHY THIS EXISTS. `cargo test | tee log | grep ...` throws away the exit code
# of `cargo test` itself: `$?` after a pipeline is the LAST command's status
# (`grep`'s, or `awk`'s), and a `cargo test` that was killed mid-run prints
# `failed=0` to its partial output identically to a real pass -- so the
# truncated run and the clean run are indistinguishable by exit code alone if
# you read the wrong one. This project's shell is zsh, which exposes
# `$pipestatus` (lowercase, an array, one entry per pipeline stage) -- NOT
# bash's `$PIPESTATUS`, a different variable in a different shell. Getting
# this wrong is not hypothetical: a `git merge | tail; echo $?` shipped a
# conflicted merge the same day this script was written, because `$?` named
# `tail`'s exit code, not `git merge`'s. Treat any pipe as a hazard generally,
# not just this one.
#
# WHAT THIS SCRIPT DELIBERATELY DOES NOT DO:
#   * It does not run the disk-floor check for you. Compose them:
#       scripts/check-disk-floor.sh && scripts/run-workspace-tests.sh
#     Bundling them silently would hide which one refused.
#   * It does not run `cargo clean`, retry, or otherwise recover from a
#     failure -- it reports CARGO_EXIT and the suite/pass/fail counts and
#     stops. Recovery is a judgment call this script does not make for you.
#   * It runs `cargo test --workspace --all-features` exactly once. It is not
#     a parallel-build harness; nothing here shares a `CARGO_TARGET_DIR`
#     across invocations. Board item 01KZY8VNXQ3TE7D1WE7W2ZQK53 measured that
#     a shared target dir would make concurrent builds SLOWER, not faster --
#     cargo takes a lock per target dir (confirmed on this machine: a second
#     `cargo check` against a target dir already in use by a first blocks on
#     "Blocking waiting for file lock on build directory" until the first
#     releases it) -- so per-worktree target dirs, the status quo, stay
#     recommended. Nothing to compose here.
#
# Usage:  scripts/run-workspace-tests.sh
# Output: prints suite/pass/fail counts and CARGO_EXIT=<n> on the last line.
# Exit:   the real `cargo test` exit code (0 pass, non-zero fail or crash),
#         NOT grep's or awk's.
#
# DELIBERATELY NO ARGUMENTS. This runs exactly one command, the workspace
# baseline (`cargo test --workspace --all-features`) -- not a general-purpose
# wrapper. An earlier draft accepted extra cargo args appended after the
# fixed ones; testing it (`-- -p conway-core` against a script that already
# hardcodes `--workspace`) produced a real, confusing failure --
# `error: Unrecognized option: 'p'` -- because a trailing `--` hands
# everything after it to the TEST HARNESS, not to cargo, so `-p` landed in
# the wrong place. Rather than document that footgun, this script does not
# expose one: if you need a different invocation, copy the
# `tee | grep | awk` + `$pipestatus[1]` pattern below rather than
# parameterizing this file.

emulate -L zsh
set -u

LOG="${TMPDIR:-/tmp}/conway-workspace-tests-$$.log"

echo "Log: $LOG"
cargo test --workspace --all-features 2>&1 \
  | tee "$LOG" \
  | grep -E '^test result' \
  | awk -F'[ ;]' '{p+=$4; f+=$7; n+=1} END{print "suites:", n, " passed:", p, " failed:", f}'

# $pipestatus[1] is `cargo test`'s own exit code -- the first stage of the
# pipeline above -- captured immediately, before any other command can
# overwrite $?. This is the one line that makes the whole script worth
# having; everything above it is presentation.
CARGO_EXIT=${pipestatus[1]}
echo "CARGO_EXIT=${CARGO_EXIT}"
echo "Full log: $LOG"
exit "${CARGO_EXIT}"
