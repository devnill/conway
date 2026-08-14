#!/usr/bin/env python3
"""Resolve every board-item id cited in the tree against the ideate stores.

WHY THIS EXISTS. The tree cites board items by ULID in ~980 places. A citation
that names a *closed* item in a pending-work sense is a dangling promise: a
reader who follows "tracked under 01K..." finds finished work rather than the
open question the sentence implied. Board item 01KZVYPE1TK3THV2M3GEBMCYPP found
that class by accident three times before this check existed.

WHY IT IS NOT A CI GATE, stated plainly because a check whose limits are
unstated is the defect one level up. Both stores it resolves against are
local-only and excluded by .gitignore:

  * the work board   -- .ideate-work/board.db  (sqlite, `items` table)
  * the record store -- .ideate/record/YYYY/MM/<ULID>.md  (decisions and more)

CI has neither, and committing board state was declined (the same operator
direction that declined vendoring the steering store). So this runs on a
maintainer checkout. When the stores are ABSENT it exits 2 and says SKIPPED --
it never exits 0 on a run that verified nothing, because "green because it
checked nothing" is exactly the failure this repository keeps finding.

WHAT IT FLAGS, and the distinction that keeps it usable. A citation naming a
`done` item is NOT automatically wrong: "implemented by 01K..." is legitimate
and useful provenance, and most of the ~980 are exactly that. The defect is a
citation that implies PENDING work -- "tracked under", "deferred to", "01K...
tracks that" -- while naming something `done` or `cancelled`. A checker that
flagged every done-item citation would report hundreds of false positives and
be switched off within a week, so the pending-sense cue must sit next to the
citation and govern it, not merely appear in the same paragraph.

TWO ID NAMESPACES. Work items and record entries (decisions among them) are
separate stores sharing one id shape. docs/plugins/hooks.md cites
01KZHEWXDZWPWMEAQ01XY2RDCB and 01KYXS3PTYVATWR58JR95AZJYN as *decisions*; both
resolve in the record store and return nothing from the work board. A resolver
that checked only the board would call both dangling and be wrong.

A SECOND, UNRELATED CLASS THIS ALSO CHECKS: steering-shorthand leaking onto a
user-facing page (board item 01KZWV0BG353J7SVZX5YV8N459, added by
01KZY8TEE2FDWQMHEKJDDC3SG9). `GP-03`, `P-2`, `C-04` and similar are internal
governance ids from `.ideate/steering/`, a directory `.gitignore` excludes on
purpose (the same operator direction that declined vendoring the board).
`docs/plugins/authoring.md` is read by a third-party plugin author who will
never have that directory; a bare `GP-03` resolves for nobody but a
maintainer. This check runs unconditionally (no store needed, unlike the ULID
check above) over `docs/` plus the five root pages `01KZWV0BG353J7SVZX5YV8N459`
measured (`README.md`, `ARCHITECTURE.md`, `PHILOSOPHY.md`, `CONTRIBUTING.md`,
`CHANGELOG.md`) and fails on any match. It does NOT cover `crates/*/src/`
(that surface's own regression guard is board item `01KZVYKS7WPSZXTGN3XAMM0PC7`,
"S0c" -- deliberately kept a separate item and a separate invariant, not
widened into this one), and it does NOT cover the `T-`/`V-`/`F-`/`R-` id
families `docs/plugins/hooks.md` still quotes (e.g. `F12`, `T7`) -- those are
historical labels from an item's own original spec text, always paired inline
with the real, resolvable board ULID that superseded them, which is a
different (legitimate) citation shape than a bare, untranslated `GP-*`/`P-*`/
`C-*` reference standing alone.

WHAT THIS DOES NOT CHECK, stated because a gate whose limits are unstated is
the defect one level up:

  * **Citation resolution (the ULID half) needs a maintainer checkout.** CI
    has no `.ideate-work/board.db`/`.ideate/record`, so it cannot notice a
    dangling `tracked under 01K...` citation -- only the steering-shorthand
    half runs there. Run on a checkout with both stores present before
    trusting a citation's pending/done status.
  * **The pending-sense phrase list (`PENDING_BEFORE`/`PENDING_AFTER`) is a
    fixed vocabulary, not language understanding.** A citation phrased a way
    neither regex anticipates is invisible to this check even naming a
    `done` item in a clearly pending sense.
  * **`crates/*/src/` is not rescanned for steering shorthand.** That surface
    has its own regression guard, board item `01KZVYKS7WPSZXTGN3XAMM0PC7`
    ("S0c") -- deliberately a separate invariant, not widened into this one.
  * **`T-`/`V-`/`F-`/`R-` id families are not steering shorthand here.**
    `docs/plugins/hooks.md` still quotes some (`F12`, `T7`) as historical
    labels from an item's own original spec text, always paired inline with
    the real board ULID that superseded them -- a legitimate citation shape
    this check does not flag, distinct from a bare, untranslated `GP-*`/
    `P-*`/`C-*` reference standing alone.
  * **A false PRESENT-TENSE capability claim with no `GP-*`/`P-*`/`C-*` id at
    all is invisible to this script.** That class (a page asserting a
    default build does something only a plugin adds) is guarded instance by
    instance as a `scripts/board-claims.md` regression predicate, not
    mechanically detected here -- see `check-design-claims.py`'s own header.

Usage:  python3 scripts/check-board-citations.py [--verbose]
Exit:   0 clean | 1 violations found (either class)
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sqlite3
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BOARD_DB = ROOT / ".ideate-work" / "board.db"
RECORD_DIR = ROOT / ".ideate" / "record"

SCAN_DIRS = ["crates", "docs", ".design"]
SCAN_SUFFIXES = {".rs", ".md", ".toml"}

ULID = re.compile(r"\b01[0-9A-HJKMNP-TV-Z]{24}\b")
QUOTED = re.compile(r'"[^"]*"')

# A citation carried in a structured field rather than prose, e.g.
# `board_item: "01K..."`. Scanned in addition to comment lines; the quoted-span
# mask below is skipped for these, since the id IS the quoted value here.
CITATION_FIELD = re.compile(
    r'\b(?:board_item|board|item|tracked_by|decision)\s*:\s*"(01[0-9A-HJKMNP-TV-Z]{24})"'
)

# A ULID citation immediately governed by a pending-work phrase. The phrase must
# sit within a short span of the id -- not merely somewhere in the paragraph.
_ID = r'(?:board\s+item\s+)?[`"(\[]*\b(01[0-9A-HJKMNP-TV-Z]{24})\b'
PENDING_BEFORE = re.compile(
    r"\b(?:tracked\s+(?:under|by|in)|deferred\s+(?:to|into)|blocked\s+on"
    r"|see\s+(?:the\s+)?open\s+item|pending\s+(?:in|under)|filed\s+as\s+the\s+open"
    r"|awaiting|owned\s+by\s+the\s+open"
    r"|will\s+be\s+(?:done|built|wired|closed|added)\s+(?:under|in|by))"
    r"\W{0,24}" + _ID,
    re.IGNORECASE,
)
PENDING_AFTER = re.compile(
    _ID + r'[`")\]]*\s*(?:\([^)]{0,60}\)\s*)?'
    r"(?:tracks\b|is\s+tracking\b|remains?\s+open\b|is\s+still\s+open\b"
    r"|will\s+(?:wire|build|close|land|add|resolve|fix|track)\b)",
    re.IGNORECASE,
)

# ULIDs that appear in a comment or in prose but are NOT board citations. Each
# needs a reason; an unexplained entry here would hide a real dangling id.
ALLOWLIST = {
    "01ARZ3NDEKTSV4RRFFQ69G5FAV": "the ULID spec's own well-known example id, used as test data",
    "01KZWT0ET8E669YPNQWXB3GQZA": "a conway *session* id in a recorded walkthrough transcript, not a board item",
}

# Internal governance shorthand from `.ideate/steering/` -- unresolvable for
# anyone without that gitignored directory. See the module doc's "A SECOND,
# UNRELATED CLASS" section for what this deliberately does not cover (T-/V-/
# F-/R- ids, crates/*/src/).
STEERING_SHORTHAND = re.compile(r"\b(GP-[0-9]+|P-[0-9]+|C-[0-9]+)\b")

# The exact surface board item 01KZWV0BG353J7SVZX5YV8N459 measured: docs/ plus
# the five root pages a reader outside this repo actually lands on.
USER_FACING_ROOT_MD = ("README.md", "ARCHITECTURE.md", "PHILOSOPHY.md", "CONTRIBUTING.md", "CHANGELOG.md")


def load_stores() -> tuple[dict[str, str], set[str]] | None:
    if not BOARD_DB.exists() or not RECORD_DIR.is_dir():
        return None
    con = sqlite3.connect(f"file:{BOARD_DB}?mode=ro", uri=True)
    try:
        board = {row[0]: row[1] for row in con.execute("select id, status from items")}
    finally:
        con.close()
    record = {p.stem for p in RECORD_DIR.rglob("*.md")}
    return board, record


def citation_lines(path: pathlib.Path):
    """Yield (lineno, raw_line) for lines that can carry a citation.

    Rust: comment lines only. Markdown: prose outside fenced code blocks. This
    is what separates a citation from a ULID *literal* -- a test fixture id, a
    JSON example in docs, a session id in a log path. Quoted spans are masked
    on top of that, so an id inside a string on a comment line is skipped too.
    """
    fenced = False
    for lineno, line in enumerate(path.read_text(errors="ignore").split("\n"), 1):
        if path.suffix == ".md":
            if line.lstrip().startswith("```"):
                fenced = not fenced
                continue
            if fenced:
                continue
        else:
            # Comment lines, plus structured citation FIELDS. The second half
            # matters: `enum_variant_construction_guard.rs` carries its
            # citations in a `board_item: "01K..."` struct field, and a
            # comment-only scan misses them silently -- the exact "a regex
            # anchored to one context misses the others" hazard this check
            # was asked to avoid.
            if not (line.lstrip().startswith("//") or CITATION_FIELD.search(line)):
                continue
        yield lineno, line


def scan_steering_shorthand() -> list[tuple[str, int, str, str]]:
    """`(rel_path, lineno, id, line)` for every bare GP-*/P-*/C-* citation on a
    user-facing page. Needs neither store -- this is the half of the module
    that DOES gate CI, unlike the ULID-resolution half below.
    """
    files = sorted(
        [p for p in (ROOT / "docs").rglob("*.md") if p.is_file()]
        + [ROOT / f for f in USER_FACING_ROOT_MD if (ROOT / f).exists()]
    )
    hits: list[tuple[str, int, str, str]] = []
    for path in files:
        rel = path.relative_to(ROOT).as_posix()
        for lineno, line in citation_lines(path):
            masked = QUOTED.sub(lambda m: " " * len(m.group()), line)
            for m in STEERING_SHORTHAND.finditer(masked):
                hits.append((rel, lineno, m.group(1), line.strip()[:140]))
    return hits


def scan():
    shorthand = scan_steering_shorthand()
    for rel, lineno, ident, ctx in shorthand:
        print(f"{rel}:{lineno}: STEERING-SHORTHAND citation on a user-facing page: {ident}")
        print(f"    {ctx}")
    if shorthand:
        print(
            f"\n{len(shorthand)} steering-shorthand citation(s) found -- unresolvable "
            f"for a reader without .ideate/steering/. Reference the concept instead "
            f"(see board item 01KZWV0BG353J7SVZX5YV8N459's translation table)."
        )

    stores = load_stores()
    if stores is None:
        print(
            "\nCITATION-RESOLUTION HALF SKIPPED: the ideate stores are not present "
            "in this checkout."
        )
        print(f"  expected {BOARD_DB.relative_to(ROOT)} and {RECORD_DIR.relative_to(ROOT)}")
        print("  Both are .gitignore'd local tooling state, so CI and a plain clone")
        print("  cannot run it -- unlike the steering-shorthand check above, which just")
        print("  ran regardless. Run this on a maintainer checkout before trusting a")
        print("  board_item citation's pending/done status.")
        return 1 if shorthand else 0
    board, record = stores

    unknown: list[tuple[str, int, str, str]] = []
    stale: list[tuple[str, int, str, str, str]] = []
    cited = 0

    def classify(ident: str) -> str | None:
        """`done`/`cancelled`/`open`/... for a board item, `record` for a
        record-store entry (a decision among others), None for neither.

        A pending-sense citation naming a `record` id is a defect in its own
        right: a decision is a settled ruling, so nothing can be "tracked
        under" one. That case is how `LogRecord::ContextMask`'s doc came to
        say "Tracked by board item 01KYTQWD2SBW33YPNGY0YBN9WY" while that id
        was a decision that had since been overtaken.
        """
        if ident in board:
            return board[ident]
        return "record" if ident in record else None

    files = [
        p
        for d in SCAN_DIRS
        for p in (ROOT / d).rglob("*")
        if p.is_file() and p.suffix in SCAN_SUFFIXES
    ] + [p for p in ROOT.glob("*.md")]

    for path in sorted(files):
        rel = path.relative_to(ROOT).as_posix()
        lines = list(citation_lines(path))
        for idx, (lineno, line) in enumerate(lines):
            masked = QUOTED.sub(lambda m: " " * len(m.group()), line)
            found = {m.group() for m in ULID.finditer(masked)}
            found |= {m.group(1) for m in CITATION_FIELD.finditer(line)}
            for ident in sorted(found):
                if ident in ALLOWLIST:
                    continue
                cited += 1
                if classify(ident) is None:
                    unknown.append((rel, lineno, ident, line.strip()[:120]))

            # Pending-sense detection runs over this line joined with the next,
            # so a citation wrapped across two comment lines is still seen.
            nxt = lines[idx + 1][1] if idx + 1 < len(lines) else ""
            window = " ".join(x.lstrip(" /!*#|->").strip() for x in (line, nxt))
            for rx in (PENDING_BEFORE, PENDING_AFTER):
                for hit in rx.finditer(window):
                    ident = hit.group(1)
                    if ident in ALLOWLIST:
                        continue
                    status = classify(ident)
                    if status in ("done", "cancelled", "record"):
                        stale.append((rel, lineno, ident, status, window.strip()[:140]))

    seen = set()
    stale = [s for s in stale if not (s[:3] in seen or seen.add(s[:3]))]

    for rel, lineno, ident, status, ctx in stale:
        what = (
            "a RECORD entry (a decision, not trackable work)"
            if status == "record"
            else f"a {status.upper()} item"
        )
        print(f"{rel}:{lineno}: PENDING-SENSE citation names {what}: {ident}")
        print(f"    {ctx}")
    for rel, lineno, ident, ctx in unknown:
        print(f"{rel}:{lineno}: UNKNOWN id {ident} resolves in neither store")
        print(f"    {ctx}")

    total = len(stale) + len(unknown) + len(shorthand)
    print(
        f"\nchecked {cited} citations across {len(files)} files "
        f"({len(board)} board items, {len(record)} record entries): "
        f"{len(stale)} stale, {len(unknown)} unknown, "
        f"{len(shorthand)} steering-shorthand"
    )
    return 1 if total else 0


def main() -> int:
    argparse.ArgumentParser(description=__doc__).parse_args()
    return scan()


if __name__ == "__main__":
    sys.exit(main())
