#!/usr/bin/env bash
# Run the CI gates that are fast, network-free, and need no toolchain beyond
# the one this repo already pins -- the subset a worker who was told NOT to
# run the full workspace suite (disk/time reasons; see
# `scripts/run-workspace-tests.sh`) can still run, so that "I ran build, test,
# clippy and fmt and they were all green" stops being a report that can ship a
# rustdoc-gate failure.
#
# WHY THIS EXISTS. Board item 01M0TJC2GY2C81Y9R1KKWT5PJJ: on 2026-08-24, two
# of three workers in one execution batch independently shipped a public doc
# comment linking to a private item -- `error: public documentation for X
# links to private item Y` under `RUSTDOCFLAGS=-D warnings`. Both had run
# build, test, clippy and fmt and reported all green; neither ran `cargo doc`,
# because nothing told them to. `.github/workflows/ci.yml` already runs this
# gate (job `cargo doc (-D warnings)`) and would have caught both before
# merge -- so the defect this script answers is a wasted round-trip and a
# manual catch, not a shipped bug. It is a fast-feedback problem, not a
# correctness hole, and the fix is proportionate to that: give a worker the
# same gate CI runs, cheaply enough to run before every anchor, not a second
# copy of the whole CI matrix.
#
# NOT EVERY CI GATE IS HERE, on purpose. Of CI's eight jobs:
#   * fmt, design-claims, board-citations, doc, clippy -- all here. None
#     needs network access or a toolchain this repo does not already pin
#     (`rust-toolchain.toml`), and none takes more than a few minutes.
#   * `cargo test (workspace, all features)` -- deliberately NOT here. It is
#     `scripts/run-workspace-tests.sh`'s job (20-45+ min, and per that
#     script's own header, `set -u` is not what makes that safe to run
#     concurrently -- disk is). Compose them:
#       scripts/check-disk-floor.sh && scripts/check-fast-gates.sh && scripts/run-workspace-tests.sh
#     Bundling the slow suite in here would silently make this script slow
#     too, defeating the point of having a fast tier at all.
#   * `cargo build (MSRV 1.88.0)` -- needs a second Rust toolchain
#     (1.88.0, distinct from the 1.94.0 this repo's `rust-toolchain.toml`
#     pins) installed on the machine. Not assumed present; CI-only.
#   * `cargo deny check` -- needs `cargo-deny` installed AND a network fetch
#     of the advisories database. Not assumed present or reachable; CI-only.
#   * `cargo test -p conway (feature-matrix)` -- re-runs the suite under six
#     feature combinations. Correctness-important but not fast; CI-only for
#     the same reason the full workspace suite is excluded above.
# If you have the MSRV toolchain and `cargo-deny` locally and want to run
# those too, their exact invocations are in `.github/workflows/ci.yml` --
# copy them from there, not from a paraphrase, for the same reason this
# script exists: two spellings of the same gate can silently disagree.
#
# SINGLE SOURCE OF TRUTH FOR THE INVOCATIONS. `.github/workflows/ci.yml`'s
# `fmt`, `design-claims`, `board-citations`, `doc`, and `clippy` jobs each
# call `scripts/check-fast-gates.sh --gate "<name>"` rather than repeating
# the command inline -- so the local run and the CI run are the same code
# path, not two hand-kept spellings of the same command that can drift apart
# the way this file's own motivating incident happened.
#
# WHAT THIS SCRIPT DELIBERATELY DOES NOT DO, for the same reason
# `check-disk-floor.sh` and `run-workspace-tests.sh` each state their own
# omissions:
#   * It does not run the disk-floor check for you -- compose it, as shown
#     above. `cargo doc` and `cargo clippy` both compile the workspace and
#     write real build output; running this below the disk floor is exactly
#     the case that check exists to catch, and bundling them would hide
#     which one refused.
#   * It never reports a bare pass/fail. Every gate is named, individually,
#     in the summary, and the exit code is non-zero if and only if at least
#     one gate failed. A bare "gates failed" is not more actionable than the
#     defect this script exists to prevent.
#
# Usage:
#   scripts/check-fast-gates.sh              # run every fast gate, print a
#                                             # per-gate summary
#   scripts/check-fast-gates.sh --list       # print the gate names and exit 0
#   scripts/check-fast-gates.sh --gate NAME  # run exactly one gate by name,
#                                             # streaming its own output
#                                             # directly (this is what CI
#                                             # calls; the exit code is the
#                                             # gate's own)
# Exit: 0 if every gate requested passed, 1 if any failed, 2 usage error.

set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
cd "$REPO_ROOT" || exit 2

# --- Gate table -------------------------------------------------------------
# Each gate's NAME matches its CI job's `name:` in `.github/workflows/ci.yml`
# exactly, so `--gate "<name>"` composes with a `run:` line there byte for
# byte and a report naming a gate is unambiguous against the CI log naming
# the same gate.

gate_fmt() {
  cargo fmt --all --check
}

gate_design_claims() {
  python3 scripts/check-design-claims.py
}

gate_board_citations() {
  python3 scripts/check-board-citations.py
}

gate_doc() {
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
}

gate_clippy() {
  cargo clippy --workspace --all-targets --all-features -- -D warnings
}

GATE_NAMES=(
  "cargo fmt (--check)"
  "design claims (board-sourced predicates)"
  "board citations (steering-shorthand half)"
  "cargo doc (-D warnings)"
  "cargo clippy (-D warnings)"
)
GATE_FUNCS=(
  gate_fmt
  gate_design_claims
  gate_board_citations
  gate_doc
  gate_clippy
)

usage() {
  # Anchored on the content, not a line range: an earlier version printed
  # lines 2-30, and the header grew past that, so `--help` showed only the
  # rationale and never the invocation syntax it exists to show.
  sed -n '/^# Usage:/,/^# Exit:/p' "$0" | sed 's/^# \{0,1\}//'
}

# --- Argument handling -------------------------------------------------------

MODE="all"
WANT_GATE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --list)
      MODE="list"
      shift
      ;;
    --gate)
      # `shift 2` with only one positional left does NOT shift and returns
      # non-zero; with `set -u` but no `set -e` that failure is silent, `$1`
      # stays `--gate`, and this case arm re-enters forever. Guard the
      # precondition explicitly rather than relying on the shift.
      if [[ $# -lt 2 ]]; then
        echo "check-fast-gates: --gate requires a name (see --list)" >&2
        exit 2
      fi
      MODE="single"
      WANT_GATE="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "check-fast-gates: unrecognized argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ "$MODE" == "list" ]]; then
  for name in "${GATE_NAMES[@]}"; do
    echo "$name"
  done
  exit 0
fi

if [[ "$MODE" == "single" ]]; then
  if [[ -z "$WANT_GATE" ]]; then
    echo "check-fast-gates: --gate requires a name (see --list)" >&2
    exit 2
  fi
  for i in "${!GATE_NAMES[@]}"; do
    if [[ "${GATE_NAMES[$i]}" == "$WANT_GATE" ]]; then
      "${GATE_FUNCS[$i]}"
      exit $?
    fi
  done
  echo "check-fast-gates: no such gate: $WANT_GATE" >&2
  echo "  known gates:" >&2
  for name in "${GATE_NAMES[@]}"; do
    echo "    $name" >&2
  done
  exit 2
fi

# --- Run every gate, hiding none of the results ------------------------------

LOG_DIR="${TMPDIR:-/tmp}/conway-fast-gates-$$"
mkdir -p "$LOG_DIR"
echo "Logs: $LOG_DIR"

FAILED_NAMES=()

for i in "${!GATE_NAMES[@]}"; do
  name="${GATE_NAMES[$i]}"
  func="${GATE_FUNCS[$i]}"
  slug="$(echo "$name" | tr -c 'A-Za-z0-9' '_')"
  log="$LOG_DIR/${slug}.log"

  echo "==> ${name}"
  if "$func" >"$log" 2>&1; then
    echo "    PASS"
  else
    echo "    FAIL -- see $log"
    tail -n 20 "$log" | sed 's/^/    | /'
    FAILED_NAMES+=("$name")
  fi
done

echo
echo "--- summary ---"
for name in "${GATE_NAMES[@]}"; do
  is_failed=0
  for f in "${FAILED_NAMES[@]:-}"; do
    [[ "$f" == "$name" ]] && is_failed=1
  done
  if [[ "$is_failed" -eq 1 ]]; then
    echo "FAIL: $name"
  else
    echo "PASS: $name"
  fi
done

if [[ "${#FAILED_NAMES[@]}" -gt 0 ]]; then
  echo
  echo "FAST_GATES_EXIT=1 (${#FAILED_NAMES[@]} gate(s) failed: ${FAILED_NAMES[*]})"
  exit 1
fi

echo
echo "FAST_GATES_EXIT=0 (all ${#GATE_NAMES[@]} fast gates passed)"
exit 0
