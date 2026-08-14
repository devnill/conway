#!/usr/bin/env bash
# Refuse to start a build/test run below a disk-free floor.
#
# WHY THIS EXISTS. `target/` reached 175 GB once and filled the disk, silently
# -- board item 01KZY8VNXQ3TE7D1WE7W2ZQK53's own measurement. Nothing on this
# machine ever refused to start a build because disk was low; the incident was
# discovered only once the disk WAS full. This is the cheap half of that
# item's answer: a floor check an agent runs before a build, not a monitor
# that watches one in progress.
#
# WHAT THIS DELIBERATELY DOES NOT DO, stated because a check that silently
# does less than its name suggests is the same defect this whole day's work
# keeps finding elsewhere:
#   * It does not clean anything up. It refuses and tells you to, it never
#     runs `cargo clean` for you -- a script with permission to delete build
#     output unattended is a worse hazard than the disk-full incident it
#     replaces.
#   * It checks free space ONCE, at invocation. A build that consumes the
#     remaining headroom AFTER this check passes is not caught -- this is a
#     floor at the start line, not a guard rail during the run.
#   * The 40 GB figure is a starting estimate, not a measured worst case.
#     `target/` alone reached 175 GB in the incident this item responds to;
#     this floor is meant to catch the "already low" case early, not to
#     guarantee 40 GB is always enough headroom for every possible build.
#
# Usage:  scripts/check-disk-floor.sh [--min-free-gb N] [PATH]
#         PATH defaults to the repository root (this script's parent's parent).
# Exit:   0 sufficient free space | 1 below the floor | 2 usage error
set -u

MIN_FREE_GB=40
CHECK_PATH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --min-free-gb)
      MIN_FREE_GB="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      CHECK_PATH="$1"
      shift
      ;;
  esac
done

if [[ -z "$CHECK_PATH" ]]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
  CHECK_PATH="$(cd "$SCRIPT_DIR/.." && pwd)"
fi

if [[ ! -e "$CHECK_PATH" ]]; then
  echo "check-disk-floor: path does not exist: $CHECK_PATH" >&2
  exit 2
fi

# `df -Pk` -- POSIX output format (stable columns) in 1024-byte blocks, so
# this parses the same on macOS and Linux. The available-blocks column is
# always the 4th field of the second (data) line.
AVAIL_KB=$(df -Pk "$CHECK_PATH" | awk 'NR==2 {print $4}')
if [[ -z "$AVAIL_KB" || ! "$AVAIL_KB" =~ ^[0-9]+$ ]]; then
  echo "check-disk-floor: could not parse \`df -Pk $CHECK_PATH\` output" >&2
  exit 2
fi

AVAIL_GB=$((AVAIL_KB / 1024 / 1024))

if [[ "$AVAIL_GB" -lt "$MIN_FREE_GB" ]]; then
  echo "REFUSED: ${AVAIL_GB} GB free on the filesystem holding $CHECK_PATH," >&2
  echo "  below the ${MIN_FREE_GB} GB floor. Run \`cargo clean\` (in this" >&2
  echo "  worktree and any stale sibling worktrees) or free space some other" >&2
  echo "  way before building -- target/ reached 175 GB and filled the disk" >&2
  echo "  once already (board item 01KZY8VNXQ3TE7D1WE7W2ZQK53)." >&2
  exit 1
fi

echo "OK: ${AVAIL_GB} GB free on the filesystem holding $CHECK_PATH (floor: ${MIN_FREE_GB} GB)"
exit 0
