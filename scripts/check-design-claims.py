#!/usr/bin/env python3
"""Check the falsifiable claims declared in `scripts/board-claims.md`.

WHY THIS EXISTS. Something has to make `PHILOSOPHY.md`'s present-tense
exemption safe: a claim about a gap between the page and the tree that nobody
tracks is expected to be true right now (`CONTRIBUTING.md` §2). Until
2026-08-13 the tracking document was `.design/philosophy-debt.md`, a prose
ledger, and nothing enforced *it* either. In August 2026 two documents
describing the same hook work diverged -- `docs/plugins/hooks.md` stayed exact
while the ledger went stale on its highest-traffic entry, in the
*understating* direction, telling readers a shipped capability was unbuilt.
The difference was enforcement, not care: `docs/` is gated, the ledger was
gated by nothing.

THE SHAPE, AND WHY THIS ONE. Board item 01KZVYR259XN9JC12A6X69CGGD offered
three candidate shapes and observed that only "freshness by commit" -- fail
when a commit touches a mapped area without touching the ledger -- would have
caught the August case, while also being the one most likely to be resented and
switched off. This is a fourth shape, and it is better on both counts:

  **The claim declares the predicate that makes itself falsifiable, and the
  check evaluates it.**

A claim that says a capability is unbuilt declares an `absent` predicate: a
pattern that must match nothing. When the capability ships, the pattern starts
matching and the check fails, naming the claim and the board item it belongs
to. A claim that says something exists declares a `present` predicate, which
fails if that thing is deleted or renamed.

Against the August case this fires exactly: the hooks claim "every event other
than pre_tool_use dispatches nothing" is an `absent` predicate over the other
six event names, so wiring one up turns the check red in the same change. And
unlike freshness-by-commit it is quiet -- editing a hook test, or any other
file in the "hooks area", changes no predicate's truth, so there is nothing to
resent.

WHY THE PREDICATES LIVE ON THE BOARD NOW (board items 01KZYAHSXDXFDY9FX5MXPRZ4M1,
01KZY8TEE2FDWQMHEKJDDC3SG9), NOT IN `.design/`. The prose ledger's own
completeness contract -- "a claim not in this list is expected to be true right
now" -- survives, but its home changed: every gap between `PHILOSOPHY.md` and
the tree is now an open board item, and a claim that used to be ledger prose is
now that item's own spec. `scripts/board-claims.md` carries only the
mechanical part -- the pattern and the `board_item:` id that owns it -- see
that file's own header for the full reasoning, including why the pattern still
lives in a git-tracked file rather than literally inside the board item's row
(CI cannot read `.ideate-work/board.db`; it is `.gitignore`d local state).

WHAT IT DOES NOT CATCH, stated because a gate whose blind spots are unstated is
the same defect one level up:

  * **A claim with no predicate.** Prose can say more than its predicate
    covers. Nothing here proves a board item's predicates are sufficient for
    its own prose -- only that the declared ones hold. A claim with a weak
    predicate rots exactly as before.
  * **A claim that is wrong in a way no pattern expresses.** "This design is
    incoherent" is not checkable here.
  * **Rot in the prose around a still-true predicate** -- a rationale that has
    been overtaken while the mechanical fact it cites is unchanged.
  * **A predicate whose `board_item:` is `UNFILED`.** The pattern is still
    evaluated and still gates the build, but nothing on the board owns it yet
    -- see `board-claims.md`'s header. `--list` and a normal run both name
    these loudly rather than let them hide.
  * **Board cross-validation only runs on a maintainer checkout.** CI has no
    `.ideate-work/board.db`, so it cannot notice a `board_item:` that no
    longer resolves, or that has quietly gone `done`/`cancelled` while its
    `absent` predicate still claims the work is open. Run with the board
    reachable before trusting a `board_item:` citation.

Usage:  python3 scripts/check-design-claims.py [--list]
Exit:   0 all declared claims hold | 1 one or more failed | 2 malformed block
        or an unresolved reference (missing path, or a board_item that does
        not resolve on a checkout where the board IS reachable)
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sqlite3
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BOARD_DB = ROOT / ".ideate-work" / "board.db"

# The predicate manifest. A git-tracked file, not `.design/` -- see its own
# header for why the predicates live here rather than inside a board item's
# row, and for the `board_item: UNFILED` escape hatch.
CLAIM_SOURCE = "scripts/board-claims.md"

BLOCK = re.compile(
    r"<!--\s*claim-check\s*\n(.*?)-->",
    re.DOTALL,
)

UNFILED = "UNFILED"


def load_board() -> dict[str, str] | None:
    """`{id: status}` for every board item, read-only. `None` if unreachable.

    Never opens for write -- this script only ever needs to know an id
    resolves and what its current status is. `check-board-citations.py`
    already established that reading `.ideate-work/board.db` from a script on
    a maintainer checkout is practical; this does the same, `mode=ro`.
    """
    if not BOARD_DB.exists():
        return None
    con = sqlite3.connect(f"file:{BOARD_DB}?mode=ro", uri=True)
    try:
        return {row[0]: row[1] for row in con.execute("select id, status from items")}
    finally:
        con.close()


class Claim:
    def __init__(self, fields: dict[str, str], lineno: int):
        self.lineno = lineno
        self.board_item = fields.get("board_item", "")
        self.claim = fields.get("claim", "")
        self.note = fields.get("note", "")
        self.kind = "absent" if "absent" in fields else "present"
        self.pattern = fields.get("absent") or fields.get("present") or ""
        self.paths = fields.get("paths", "")

    def label(self, board: dict[str, str] | None) -> str:
        """A human-readable name for this claim's owner, for output only."""
        if self.board_item == UNFILED:
            return "UNFILED"
        if board is not None and self.board_item in board:
            return f"{self.board_item} ({board[self.board_item]})"
        return self.board_item

    def evaluate(self) -> tuple[bool, str]:
        """Returns (holds, evidence).

        **A MISSING PATH IS A HARD ERROR, NOT AN ABSENT MATCH.** grep over a
        path that does not exist prints nothing to stdout and exits non-zero
        -- on BSD grep, with an EMPTY stderr, so there is no diagnostic to
        notice in a CI log either. Reading only stdout would make a stale
        `paths:` value indistinguishable from a genuinely-absent pattern, and
        report "holds" for a claim nothing scanned. That is the exact
        silent-staleness failure this whole script exists to prevent, and it
        would fire precisely on the rename scenario the module doc cites as
        its motivating incident. So paths are checked before grep runs, and
        a grep exit status outside {0 = matched, 1 = no match} is fatal.
        """
        missing = [p for p in self.paths.split() if not (ROOT / p).exists()]
        if missing:
            print(
                f"{CLAIM_SOURCE}:{self.lineno}: BROKEN CLAIM for {self.board_item}\n"
                f"    its `paths:` names something that does not exist: {' '.join(missing)}\n"
                f"    Nothing was scanned, so this claim is unverified rather than true.\n"
                f"    Repoint the path, or delete the claim if its subject is gone."
            )
            sys.exit(2)

        cmd = ["grep", "-rEn", "--include=*.rs", "--include=*.toml", "--include=*.md"]
        cmd += [self.pattern]
        cmd += [str(ROOT / p) for p in self.paths.split()]
        proc = subprocess.run(cmd, capture_output=True, text=True)
        if proc.returncode not in (0, 1):
            print(
                f"{CLAIM_SOURCE}:{self.lineno}: grep failed for {self.board_item} "
                f"(exit {proc.returncode}): {proc.stderr.strip()}"
            )
            sys.exit(2)
        hits = [l for l in proc.stdout.strip().split("\n") if l]
        if self.kind == "absent":
            return (not hits, "\n".join(f"      {h[len(str(ROOT)) + 1:]}" for h in hits[:5]))
        return (bool(hits), "      (no match anywhere in: " + self.paths + ")")


def parse(doc: str) -> list[Claim]:
    text = (ROOT / doc).read_text()
    claims = []
    for m in BLOCK.finditer(text):
        lineno = text[: m.start()].count("\n") + 1
        fields: dict[str, str] = {}
        for line in m.group(1).split("\n"):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if ":" not in line:
                print(f"{doc}:{lineno}: malformed claim-check line: {line!r}")
                sys.exit(2)
            k, v = line.split(":", 1)
            fields[k.strip()] = v.strip()
        for required in ("board_item", "claim", "paths"):
            if required not in fields:
                print(f"{doc}:{lineno}: claim-check block is missing `{required}`")
                sys.exit(2)
        if "absent" not in fields and "present" not in fields:
            print(f"{doc}:{lineno}: claim-check block needs `absent` or `present`")
            sys.exit(2)
        claims.append(Claim(fields, lineno))
    return claims


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--list", action="store_true", help="print every claim and exit")
    args = ap.parse_args()

    board = load_board()
    claims = parse(CLAIM_SOURCE)
    print(f"{CLAIM_SOURCE}: {len(claims)} declared claim(s)")
    if board is None:
        print(
            "  (.ideate-work/board.db not reachable here -- board_item citations "
            "are not cross-checked; predicates still evaluate in full)"
        )

    unfiled = [c for c in claims if c.board_item == UNFILED]
    if unfiled:
        print(f"  {len(unfiled)} claim(s) carry board_item: UNFILED -- no item owns them yet:")
        for c in unfiled:
            print(f"    line {c.lineno}: {c.claim}")
            if c.note:
                print(f"      {c.note}")

    if board is not None:
        for c in claims:
            if c.board_item == UNFILED:
                continue
            if c.board_item not in board:
                print(
                    f"{CLAIM_SOURCE}:{c.lineno}: BROKEN CLAIM -- board_item "
                    f"{c.board_item} resolves to nothing in the board.\n"
                    f"    Nothing was verified for this claim's provenance. "
                    f"Repoint it, or mark it UNFILED with a note."
                )
                sys.exit(2)

    if args.list:
        for c in claims:
            print(f"  [{c.kind}] {c.label(board)}: {c.claim}")
        return 0

    failed = 0
    for c in claims:
        holds, evidence = c.evaluate()
        if holds:
            continue
        failed += 1
        print()
        print(f"{CLAIM_SOURCE}:{c.lineno}: STALE CLAIM for {c.label(board)}")
        print(f'    the claim says: "{c.claim}"')
        if c.kind == "absent":
            print(f"    but this pattern now matches, so the claim is no longer true:")
            print(f"      /{c.pattern}/")
            print(evidence)
        else:
            print(f"    but this pattern no longer matches anywhere:")
            print(f"      /{c.pattern}/")
            print(evidence)
        print("    Update the board item (and this predicate) in the same change")
        print("    that made this true, or correct the predicate if the claim was")
        print("    always narrower.")

    print()
    if failed:
        print(f"{failed} of {len(claims)} declared claims are STALE")
        return 1
    print(f"all {len(claims)} declared claims hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
