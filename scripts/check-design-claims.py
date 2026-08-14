#!/usr/bin/env python3
"""Check the falsifiable claims declared in `scripts/board-claims.md`.

WHY THIS EXISTS. Something has to make `PHILOSOPHY.md`'s present-tense
exemption safe: a claim about a gap between the page and the tree that nobody
tracks is expected to be true right now (`CONTRIBUTING.md` §2). The tracking
document used to be a prose ledger, and nothing enforced *it* either. Two
documents describing the same hook work diverged once -- `docs/plugins/hooks.md`
stayed exact while the ledger went stale on its highest-traffic entry, in the
*understating* direction, telling readers a shipped capability was unbuilt.
The difference was enforcement, not care: `docs/` is gated, the ledger was
gated by nothing.

THE SHAPE, AND WHY THIS ONE. The obvious alternative -- "freshness by commit",
failing when a commit touches a mapped area without touching the ledger --
would have caught that case, and is also the one most likely to be resented and
switched off. This shape is better on both counts:

  **The claim declares the predicate that makes itself falsifiable, and the
  check evaluates it.**

A claim that says a capability is unbuilt declares an `absent` predicate: a
pattern that must match nothing. When the capability ships, the pattern starts
matching and the check fails, naming the claim. A claim that says something
exists declares a `present` predicate, which fails if that thing is deleted or
renamed.

Against the case above it fires exactly: the hooks claim "every event other
than pre_tool_use dispatches nothing" is an `absent` predicate over the other
six event names, so wiring one up turns the check red in the same change. And
unlike freshness-by-commit it is quiet -- editing a hook test, or any other
file in the "hooks area", changes no predicate's truth, so there is nothing to
resent.

WHAT IT DOES NOT CATCH, stated because a gate whose blind spots are unstated is
the same defect one level up:

  * **A claim with no predicate.** Prose can say more than its predicate
    covers. Nothing here proves a claim's predicates are sufficient for its own
    prose -- only that the declared ones hold. A claim with a weak predicate
    rots exactly as before.
  * **A claim that is wrong in a way no pattern expresses.** "This design is
    incoherent" is not checkable here.
  * **Rot in the prose around a still-true predicate** -- a rationale that has
    been overtaken while the mechanical fact it cites is unchanged.

Usage:  python3 scripts/check-design-claims.py [--list]
Exit:   0 all declared claims hold | 1 one or more failed | 2 malformed block
        or a missing path
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# The predicate manifest -- see its own header for the format.
CLAIM_SOURCE = "scripts/board-claims.md"

BLOCK = re.compile(
    r"<!--\s*claim-check\s*\n(.*?)-->",
    re.DOTALL)



class Claim:
    def __init__(self, fields: dict[str, str], lineno: int):
        self.lineno = lineno
        self.why = fields.get("why", "")
        self.claim = fields.get("claim", "")
        self.note = fields.get("note", "")
        self.kind = "absent" if "absent" in fields else "present"
        self.pattern = fields.get("absent") or fields.get("present") or ""
        self.paths = fields.get("paths", "")

    def label(self) -> str:
        """A human-readable name for this claim, for output only."""
        return self.why or self.claim

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
                f"{CLAIM_SOURCE}:{self.lineno}: BROKEN CLAIM: {self.claim}\n"
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
                f"{CLAIM_SOURCE}:{self.lineno}: grep failed for claim {self.claim!r} "
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
        for required in ("why", "claim", "paths"):
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

    claims = parse(CLAIM_SOURCE)
    print(f"{CLAIM_SOURCE}: {len(claims)} declared claim(s)")

    if args.list:
        for c in claims:
            print(f" [{c.kind}] {c.claim}\n why: {c.why}")
        return 0

    failed = 0
    for c in claims:
        holds, evidence = c.evaluate()
        if holds:
            continue
        failed += 1
        print()
        print(f"{CLAIM_SOURCE}:{c.lineno}: STALE CLAIM -- {c.why}")
        print(f'    the claim says: "{c.claim}"')
        if c.kind == "absent":
            print(f"    but this pattern now matches, so the claim is no longer true:")
            print(f"      /{c.pattern}/")
            print(evidence)
        else:
            print(f"    but this pattern no longer matches anywhere:")
            print(f"      /{c.pattern}/")
            print(evidence)
        print(" Update this predicate in the same change as the code")
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
