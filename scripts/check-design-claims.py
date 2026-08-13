#!/usr/bin/env python3
"""Check the falsifiable claims declared in `.design/`'s claim-bearing documents.

WHY THIS EXISTS. `.design/philosophy-debt.md` is the register that makes
`PHILOSOPHY.md`'s present-tense exemption safe: a claim absent from the ledger
is expected to be true right now. That contract only holds while the ledger is
accurate, and nothing enforced it. In August 2026 two documents describing the
same hook work diverged -- `docs/plugins/hooks.md` stayed exact while the
ledger went stale on its highest-traffic entry, in the *understating*
direction, telling readers a shipped capability was unbuilt. The difference was
enforcement, not care: `docs/` is gated, `.design/` was gated by nothing.

THE SHAPE, AND WHY THIS ONE. Board item 01KZVYR259XN9JC12A6X69CGGD offered
three candidate shapes and observed that only "freshness by commit" -- fail
when a commit touches a mapped area without touching the ledger -- would have
caught the August case, while also being the one most likely to be resented and
switched off. This is a fourth shape, and it is better on both counts:

  **The ledger entry declares the predicate that makes its own claim
  falsifiable, and the check evaluates it.**

An entry that says a capability is unbuilt declares an `absent` predicate: a
pattern that must match nothing. When the capability ships, the pattern starts
matching and the check fails, naming the document and the claim. An entry that
says something exists declares a `present` predicate, which fails if that thing
is deleted or renamed.

Against the August case this fires exactly: the hooks entry's "every event
other than pre_tool_use dispatches nothing" is an `absent` predicate over the
other six event names, so wiring one up turns the check red in the same change.
And unlike freshness-by-commit it is quiet -- editing a hook test, or any other
file in the "hooks area", changes no predicate's truth, so there is nothing to
resent.

WHAT IT DOES NOT CATCH, stated because a gate whose blind spots are unstated is
the same defect one level up:

  * **A claim with no predicate.** Prose can say more than its predicate
    covers. Nothing here proves an entry's predicates are sufficient for its
    prose -- only that the declared ones hold. An entry with a weak predicate
    rots exactly as before.
  * **A claim that is wrong in a way no pattern expresses.** "This design is
    incoherent" is not checkable here.
  * **Rot in the prose around a still-true predicate** -- a rationale that has
    been overtaken while the mechanical fact it cites is unchanged.
  * **Documents outside the claim-bearing subset.** That subset, and the
    criterion that decides membership, is enumerated in `.design/README.md`.

Usage:  python3 scripts/check-design-claims.py [--list]
Exit:   0 all declared claims hold | 1 one or more failed | 2 malformed block
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# The claim-bearing subset. A document belongs here when it asserts what is TRUE
# OF THE TREE RIGHT NOW, as opposed to recording how a decision was reached.
# See `.design/README.md` for the criterion in full. Adding a file here without
# also adding claim blocks to it is harmless but pointless; the check reports
# how many blocks it found per file so an empty one is visible.
CLAIM_BEARING = [
    ".design/philosophy-debt.md",
]

BLOCK = re.compile(
    r"<!--\s*claim-check\s*\n(.*?)-->",
    re.DOTALL,
)


class Claim:
    def __init__(self, doc: str, fields: dict[str, str], lineno: int):
        self.doc = doc
        self.lineno = lineno
        self.entry = fields.get("entry", "")
        self.claim = fields.get("claim", "")
        self.kind = "absent" if "absent" in fields else "present"
        self.pattern = fields.get("absent") or fields.get("present") or ""
        self.paths = fields.get("paths", "")

    def evaluate(self) -> tuple[bool, str]:
        """Returns (holds, evidence)."""
        cmd = ["grep", "-rEn", "--include=*.rs", "--include=*.toml", self.pattern]
        cmd += [str(ROOT / p) for p in self.paths.split()]
        out = subprocess.run(cmd, capture_output=True, text=True).stdout.strip()
        hits = [l for l in out.split("\n") if l]
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
        for required in ("entry", "claim", "paths"):
            if required not in fields:
                print(f"{doc}:{lineno}: claim-check block is missing `{required}`")
                sys.exit(2)
        if "absent" not in fields and "present" not in fields:
            print(f"{doc}:{lineno}: claim-check block needs `absent` or `present`")
            sys.exit(2)
        claims.append(Claim(doc, fields, lineno))
    return claims


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--list", action="store_true", help="print every claim and exit")
    args = ap.parse_args()

    failed = 0
    total = 0
    for doc in CLAIM_BEARING:
        claims = parse(doc)
        print(f"{doc}: {len(claims)} declared claim(s)")
        for c in claims:
            total += 1
            if args.list:
                print(f"  [{c.kind}] {c.entry}: {c.claim}")
                continue
            holds, evidence = c.evaluate()
            if holds:
                continue
            failed += 1
            print()
            print(f"{c.doc}:{c.lineno}: STALE CLAIM in entry \"{c.entry}\"")
            print(f'    the ledger says: "{c.claim}"')
            if c.kind == "absent":
                print(f"    but this pattern now matches, so the claim is no longer true:")
                print(f"      /{c.pattern}/")
                print(evidence)
            else:
                print(f"    but this pattern no longer matches anywhere:")
                print(f"      /{c.pattern}/")
                print(evidence)
            print("    Update the entry in the same change that made this true,")
            print("    or correct the predicate if the claim was always narrower.")

    if args.list:
        return 0
    print()
    if failed:
        print(f"{failed} of {total} declared claims are STALE")
        return 1
    print(f"all {total} declared claims hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
