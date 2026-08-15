#!/usr/bin/env bash
# scripts/dogfood-note.sh -- the one-step path from "using conway just now was
# awkward" to a board item or a comment on an existing one.
#
# WHY THIS EXISTS. Board item 01KZY8V4MYNZJABZR0X0SJ2G5Y found that conway had
# never been used for real work by anyone, and that absence shaped the whole
# board: every review audited the harness against its own documents, because
# no session ever produced the kind of friction only real use generates. The
# fix is not a doc telling someone to dogfood conway -- a page nobody reads
# changes nothing. The fix is making recording a friction point CHEAPER than
# working around it and forgetting, in the moment it happens. That is the one
# job this script has: one command, run the instant something is awkward,
# produces a board item or a comment before the friction is rationalized away.
#
# WHAT THIS DELIBERATELY DOES NOT DO:
#   * It does not decide whether your friction is worth filing. That judgment
#     stays yours -- this script only removes the CEREMONY of filing it.
#   * It does not touch conway's own config, source, or the operator's
#     ~/.conway/settings.json. It only calls `ideate-work` / `ideate-record`,
#     the same board tooling every other agent in this repo uses.
#   * It does not itself decide project scope -- it resolves ITS OWN location
#     on disk (this file's parent's parent) as the repo root and refuses to
#     run anywhere else, because `ideate-work`/`ideate-record` key their
#     project off the current working directory: run from the wrong place and
#     they silently onboard a NEW, empty board instead of failing loudly.
#   * It never files a demonstration item. Use --dry-run to see exactly what
#     command would run, without running it.
#
# THE FALSIFIABLE MARKER. Every board item this script creates has a title
# prefixed `[dogfooding] `. Every record entry it appends (comment or session
# note) carries `--scope dogfood`. Both are greppable, which is the point --
# "did the path get used" is a question with a checkable answer:
#   ideate-work list --json | python3 -c \
#     'import json,sys; items=json.load(sys.stdin)["items"]; \
#      print(sum(1 for i in items if i["title"].startswith("[dogfooding] ")))'
#   ideate-record read --scope dogfood --json
#
# Usage:
#   scripts/dogfood-note.sh friction --title "<short title>" --body "<what happened, in the moment>" [--human "<name>"] [--dry-run]
#   scripts/dogfood-note.sh comment  --id <existing-item-id> --note "<what happened>" [--human "<name>"] [--dry-run]
#   scripts/dogfood-note.sh session  --note "<what was attempted, what worked, what stopped you>" [--task <item-id>] [--human "<name>"] [--dry-run]
#
# `friction` files a new board item -- use it when the friction is not
#   obviously about something already tracked.
# `comment` appends a record entry tied to an existing item via --task --
#   use it when the friction belongs on something already on the board.
# `session` appends the standing item's required session note (see
#   docs/dogfooding.md and board item 01KZY8V4MYNZJABZR0X0SJ2G5Y). Defaults
#   --task to that standing item; pass --task to point elsewhere.
#
# Exit: 0 on success (or a completed --dry-run) | 1 on a usage or execution
#   error | 2 if run from outside this repository, or if the ideate CLIs are
#   not on PATH.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DOGFOOD_STANDING_ITEM="01KZY8V4MYNZJABZR0X0SJ2G5Y"

usage() {
  sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'
}

fail() {
  echo "dogfood-note: $1" >&2
  exit "${2:-1}"
}

# Refuse to run against the wrong project. This repo's identity is checked by
# two markers rather than one, so a stray PHILOSOPHY.md-like file elsewhere
# does not fool it -- both are load-bearing, ordinary files in every checkout.
if [[ ! -f "$REPO_ROOT/PHILOSOPHY.md" || ! -d "$REPO_ROOT/crates/conway-cli" ]]; then
  fail "resolved repo root ($REPO_ROOT) does not look like the conway checkout -- refusing to file against an unknown board" 2
fi
cd "$REPO_ROOT" || fail "could not cd to resolved repo root $REPO_ROOT" 2

for bin in ideate-work ideate-record; do
  command -v "$bin" >/dev/null 2>&1 || fail "\`$bin\` is not on PATH -- it ships with the ideate plugin; this script cannot file anything without it" 2
done

MODE="${1:-}"
[[ -n "$MODE" ]] || { usage; exit 1; }
shift || true

TITLE=""
BODY=""
NOTE=""
ITEM_ID=""
TASK_ID=""
HUMAN="${DOGFOOD_HUMAN:-${USER:-$(whoami 2>/dev/null || echo dogfooding)}}"
DRY_RUN=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --title) TITLE="$2"; shift 2 ;;
    --body) BODY="$2"; shift 2 ;;
    --note) NOTE="$2"; shift 2 ;;
    --id) ITEM_ID="$2"; shift 2 ;;
    --task) TASK_ID="$2"; shift 2 ;;
    --human) HUMAN="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unrecognized argument: $1" ;;
  esac
done

# A one-line claim for the record contract's required Claim field -- first
# line only, capped so a pasted paragraph does not become an unreadable title.
first_line_claim() {
  local text="$1"
  local line
  line="$(printf '%s' "$text" | head -n1)"
  if [[ ${#line} -gt 140 ]]; then
    line="${line:0:137}..."
  fi
  printf '%s' "$line"
}

run() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf '[dry-run] would run:'
    printf ' %q' "$@"
    printf '\n'
    return 0
  fi
  "$@"
}

case "$MODE" in
  friction)
    [[ -n "$TITLE" && -n "$BODY" ]] || fail "friction needs --title and --body"
    SPEC="$BODY

---
Origin: recorded via \`scripts/dogfood-note.sh\` during a conway dogfooding
session -- board item $DOGFOOD_STANDING_ITEM, docs/vision/INTENT.md §7b."
    run ideate-work create \
      --title "[dogfooding] $TITLE" \
      --spec "$SPEC" \
      --spec-format markdown \
      --human "$HUMAN"
    ;;
  comment)
    [[ -n "$ITEM_ID" && -n "$NOTE" ]] || fail "comment needs --id and --note"
    run ideate-record append \
      --kind finding \
      --claim "$(first_line_claim "$NOTE")" \
      --scope dogfood \
      --content "$NOTE" \
      --task "$ITEM_ID"
    ;;
  session)
    [[ -n "$NOTE" ]] || fail "session needs --note"
    TASK_ID="${TASK_ID:-$DOGFOOD_STANDING_ITEM}"
    run ideate-record append \
      --kind session-outcome \
      --claim "$(first_line_claim "$NOTE")" \
      --scope dogfood-session \
      --content "$NOTE" \
      --task "$TASK_ID"
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    usage
    fail "unknown mode: $MODE"
    ;;
esac
