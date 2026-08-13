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

Usage:  python3 scripts/check-board-citations.py [--verbose]
Exit:   0 clean | 1 violations found | 2 stores unavailable (SKIPPED)
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


def scan():
    stores = load_stores()
    if stores is None:
        print("SKIPPED: the ideate stores are not present in this checkout.")
        print(f"  expected {BOARD_DB.relative_to(ROOT)} and {RECORD_DIR.relative_to(ROOT)}")
        print("  Both are .gitignore'd local tooling state, so CI and a plain")
        print("  clone cannot run this. Exiting 2 rather than 0: a run that")
        print("  verified nothing must not report success.")
        return 2
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

    total = len(stale) + len(unknown)
    print(
        f"\nchecked {cited} citations across {len(files)} files "
        f"({len(board)} board items, {len(record)} record entries): "
        f"{len(stale)} stale, {len(unknown)} unknown"
    )
    return 1 if total else 0


def main() -> int:
    argparse.ArgumentParser(description=__doc__).parse_args()
    return scan()


if __name__ == "__main__":
    sys.exit(main())
