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

A third kind, `glob`, exists for a narrower problem those two cannot express:
"every member of a directory family is named somewhere in this doc." `absent`/
`present` evaluate one fixed pattern against one fixed path set -- there is no
way to derive the pattern itself from the filesystem, so a claim of the form
"every crates/conway-plugin-* directory is mentioned in ARCHITECTURE.md" would,
in the `present` vocabulary, have to spell out today's crate names in the
regex by hand -- which regresses the moment an existing one is deleted, but
says nothing at all about a new one arriving unmentioned, because nothing
re-derives the set. A `glob` block re-globs its directory family on every run
and checks each match's basename against the target file's text itself, so
adding crate seventeen without a matching mention is what fails it, not a
predicate someone forgot to widen.

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
  * **A `glob` claim only proves a basename appears somewhere in the target
    file's text.** It does not check that the surrounding sentence is
    accurate, non-vacuous, or even about that crate -- the same substring-only
    caveat `check-orphan-docs.py`'s module doc states for its own reachability
    search. It is a coverage floor, not a prose review.

PARTIAL-ENUMERATION HUB AUDIT. Board item 01M12X66TSV7VSNJZNM7MWFSA8: a `glob`
claim's own `paths:` is itself a hand-remembered enumeration of where a fact
lives, protecting a hand-remembered enumeration of where a fact lives -- it
guards the copies someone declared and is blind to the ones they didn't.
`ARCHITECTURE.md` carried a `glob` claim for the `crates/conway-plugin-*`
roster; `README.md` carried an undeclared, hand-written copy of the same
roster that had drifted to sixteen of seventeen names (missing
`conway-plugin-statusline`) with nothing watching it; `docs/plugins/README.md`
carried a third. The glob claim was green throughout, because green only ever
meant "the one file I was told to check still agrees with the filesystem."

So, for every `glob` claim, after its declared `paths:` are checked as above,
this script also scans a **fixed, explicitly-named hub-file set** --
`README.md`, `ARCHITECTURE.md`, `docs/plugins/README.md` (see `HUB_FILES`
below) -- for any file NOT already in that claim's `paths:` whose text names
**most, but not all,** of the glob's matched basenames. Naming most of a
family and silently dropping the rest is the specific shape of a hand
enumeration that has partially rotted; naming all of them or none of them is
not evidence of drift and must not fire (see `_hub_audit`'s own docstring for
the exact threshold and the reasoning behind it, including the two degenerate
cases -- a glob matching 0 or 1 directories -- where "most but not all" is not
a meaningful predicate at all).

**Scope, stated against the name.** This is a scan of three named files, not
of the tree. A fourth copy anywhere else -- a crate's own README, a design doc
under `docs/vision/`, a comment -- is invisible to it. Widening `HUB_FILES` is
a deliberate, reviewed edit, the same as widening a `paths:` list; it is not
meant to grow into a repo-wide grep (that shape was considered and rejected --
see board item 01M12X66TSV7VSNJZNM7MWFSA8's "Rejected, with costs" section --
because it would require naming an identifier for every future mechanism
ahead of time, recreating the hand-enumeration problem this audit exists to
solve).

**`PHILOSOPHY.md` is excluded from `HUB_FILES`, by name, deliberately.** GP-15
makes that page deliberately present-tense about things not yet built -- it
states intent, not inventory, and CONTRIBUTING.md's own §2 exemption for it is
why the rest of this checker exists at all (see "WHY THIS EXISTS" above). A
substring-membership audit has no way to tell "this page hasn't caught up to
seventeen crates yet" apart from "this page is stating design intent for a
roster that isn't built yet" -- both look like a file naming some but not all
of a family. Excluding the page outright is the only reading that survives
GP-15's exemption; making the audit "tense-aware" instead is not attempted,
because inferring tense from prose is exactly the kind of judgment call this
mechanical checker is not equipped to make, and a wrong guess there is a
false positive against the one page CONTRIBUTING.md already carved out.

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

# The fixed hub-file set the partial-enumeration audit scans -- see the
# module doc's "PARTIAL-ENUMERATION HUB AUDIT" section for what this catches
# and its explicit scope. `PHILOSOPHY.md` is excluded by name: GP-15 makes it
# deliberately present-tense about unbuilt things, and this audit has no way
# to distinguish that from a stale hand enumeration -- see the module doc.
HUB_FILES = ("README.md", "ARCHITECTURE.md", "docs/plugins/README.md")

BLOCK = re.compile(
    r"<!--\s*claim-check\s*\n(.*?)-->",
    re.DOTALL)



class Claim:
    def __init__(self, fields: dict[str, str], lineno: int):
        self.lineno = lineno
        self.why = fields.get("why", "")
        self.claim = fields.get("claim", "")
        self.note = fields.get("note", "")
        self.glob = fields.get("glob", "")
        if self.glob:
            self.kind = "glob"
        else:
            self.kind = "absent" if "absent" in fields else "present"
        self.pattern = fields.get("absent") or fields.get("present") or ""
        self.paths = fields.get("paths", "")

    def label(self) -> str:
        """A human-readable name for this claim, for output only."""
        return self.why or self.claim

    def evaluate(self) -> tuple[bool, str]:
        """Returns (holds, evidence). See `_evaluate_glob` for the `glob`
        kind's own contract; the rest of this docstring is the `absent`/
        `present` kinds this method also serves.

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

        if self.kind == "glob":
            return self._evaluate_glob()

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

    def _evaluate_glob(self) -> tuple[bool, str]:
        """`glob` re-derives its own subject set from the filesystem on every
        run -- see the module doc's "A third kind, `glob`" section for why
        `absent`/`present` cannot express this. Every directory `self.glob`
        matches (relative to ROOT) must have its basename appear as a
        substring somewhere in the concatenated text of `self.paths`.

        A glob matching nothing is treated the same as a missing `paths`
        entry above: silent success over an empty set is exactly the
        false-green this script exists to prevent, most likely caused by a
        typo'd pattern or a directory family that no longer exists.
        """
        matches = sorted(p for p in ROOT.glob(self.glob) if p.is_dir())
        if not matches:
            print(
                f"{CLAIM_SOURCE}:{self.lineno}: BROKEN CLAIM: {self.claim}\n"
                f"    its `glob:` pattern {self.glob!r} matched no directory.\n"
                f"    Nothing was scanned, so this claim is unverified rather than true.\n"
                f"    Repoint the glob, or delete the claim if its subject is gone."
            )
            sys.exit(2)
        target_text = "\n".join((ROOT / p).read_text() for p in self.paths.split())
        missing_names = [m.name for m in matches if m.name not in target_text]
        evidence_lines = [f"      not named in {self.paths}: {n}" for n in missing_names[:10]]

        for hub, missing in self._hub_audit(matches):
            evidence_lines.append(
                f"      {hub} names {len(matches) - len(missing)} of "
                f"{len(matches)} crates/conway-plugin-* members but is not "
                f"in this claim's paths: -- missing {', '.join(sorted(missing))}"
            )

        holds = not evidence_lines
        evidence = "\n".join(evidence_lines[:10])
        return (holds, evidence)

    def _hub_audit(
        self, matches: list[pathlib.Path]
    ) -> list[tuple[str, list[str]]]:
        """Scan `HUB_FILES` (module-level, and see the module doc's
        "PARTIAL-ENUMERATION HUB AUDIT" section) for a file that is NOT
        already in this claim's `paths:` but names most, not all, of
        `matches`' basenames -- the shape of a hand-written enumeration that
        has partially rotted, as opposed to one that names every member (not
        drift) or none of them (not this claim's subject at all).

        THE THRESHOLD: a strict majority (more than half) of `matches`
        named, with at least one missing. Chosen as a *proportion* rather
        than an absolute count so it scales across globs of very different
        sizes -- there is one `glob` claim in this repo today, but this
        method runs against any future one. Against the defect that
        motivated this audit (README.md naming sixteen of seventeen
        `crates/conway-plugin-*` members, 94%), a majority threshold catches
        it many times over; it would still catch a hub file that drifted by
        two or three members out of seventeen (naming fourteen is 82%,
        comfortably above half). It declines to fire on a file that "happens
        to mention a couple of crates in passing" -- two or three names out
        of seventeen is 12-18%, nowhere near a majority -- which is the
        false-positive shape that gets a check distrusted and switched off.
        A bare count threshold (e.g. "all but one") was rejected for the
        same reason a bare count claim was rejected elsewhere in this
        design: someone has to pick and maintain the number, which is the
        hand-enumeration problem this audit exists to solve, one level up.

        THE DEGENERATE CASES. For `len(matches)` in {0, 1}, "most but not
        all" is not a meaningful predicate -- there is no integer count that
        is both a strict majority and strictly less than the total. The
        `count > total / 2 and count < total` test below already returns
        false for every possible `count` in both cases (for `total == 1`:
        `count > 0.5` forces `count == 1`, but `count < 1` then forces
        `count == 0` -- contradiction), so no special case is needed to
        avoid a spurious failure. `total == 0` cannot reach this method at
        all: the empty-glob check above it exits fatally first, but the
        `total > 0` guard is kept anyway so this method stays correct if
        ever called on its own (e.g. from a test) and never divides by zero.
        """
        declared = set(self.paths.split())
        total = len(matches)
        violations: list[tuple[str, list[str]]] = []
        for hub in HUB_FILES:
            if hub in declared:
                continue
            hub_path = ROOT / hub
            if not hub_path.exists():
                continue
            text = hub_path.read_text()
            missing = [m.name for m in matches if m.name not in text]
            count = total - len(missing)
            if total > 0 and count > total / 2 and count < total:
                violations.append((hub, missing))
        return violations


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
        if "absent" not in fields and "present" not in fields and "glob" not in fields:
            print(f"{doc}:{lineno}: claim-check block needs `absent`, `present`, or `glob`")
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
        elif c.kind == "present":
            print(f"    but this pattern no longer matches anywhere:")
            print(f"      /{c.pattern}/")
            print(evidence)
        else:
            print(f"    but glob {c.glob!r} does not hold:")
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
