#!/usr/bin/env python3
"""Report board specs that still quote a retired hazard or a dead baseline.

WHY THIS EXISTS. Board item measured that roughly
twenty board specs quote "never run a package-wide `cargo fmt`" -- a warning
that was true when written (the tree had ~150 pre-existing violations) and has
been false since the workspace was reformatted and gated in CI on 2026-08-13.
Several more cite a suite baseline (129/2276, 129/2292, 130/2301, 130/2314,
131/2315, or even 153/2346, all superseded) or tell a reader to redirect
`HOME` without the `CARGO_HOME`/`RUSTUP_HOME` caveat that makes that safe now
that config isolation has landed -- noise every future worker has to reason
past before doing the item's actual work.

WHAT THIS SCRIPT DOES NOT DO, stated plainly because a tool that silently
mutates the board is worse than the noise it removes. **It never writes.**
The worker that authored it has read-only board access by design -- the
claim/complete/release lifecycle belongs to the orchestrating process, not an
implementer -- and this script inherits that boundary rather than working
around it with a raw `UPDATE`, which would also bypass the board's own
optimistic-concurrency `version` column. It prints a precise, reviewable
report; applying each fix (via the ideate tool, which owns versioning and the
claim/release lifecycle) is a separate, deliberate act.

THREE CLASSES, each reported separately because they need different fixes:

  * FMT HAZARD -- a spec still tells a reader never to run a package-wide
    `cargo fmt`. Suggested replacement: state plainly that the repo is
    fmt-clean and CI-gated since 2026-08-13, so `cargo fmt` is safe, mirroring
    the correction already carries.
  * STALE BASELINE -- a spec cites a suite/test count from a specific
    canonical set of dead figures (129/2276, 129/2292, 130/2301, 130/2314,
    131/2315, 153/2346). The current baseline as of 2026-08-13 is
    **156 suites / 2454 tests**. This will go stale again; the durable fix a
    spec can make is to say "re-measure and trust the live number, not this
    one" rather than hardcode a fresh figure that just becomes the next dead
    one -- already says this about
    itself and is the pattern worth copying.
  * HOME-REDIRECT -- a spec instructs redirecting `HOME` for test isolation
    without mentioning `CARGO_HOME`/`RUSTUP_HOME`. Since config isolation
    landed 2026-08-13, redirecting `HOME` wholesale is no longer needed for
    that purpose and forces a full toolchain resync if `CARGO_HOME`/
    `RUSTUP_HOME` aren't preserved alongside it. A spec that already says NOT
    to redirect `HOME` (config isolation covers it) is correctly excluded --
    matched heuristically by a nearby negation ("without redirecting",
    "run without ... redirect"); a spec advising unconditional HOME
    redirection is flagged.

WHAT THIS SCRIPT DOES NOT CHECK, stated because a checker's blind spots must
be as visible as its coverage. It does not check steering-shorthand
(`GP-*`/`P-*`/`C-*`) inside board specs at all -- board specs are internal
tooling state, not the user-facing surface `check-board-citations.py` covers,
and a board spec citing its own internal governance vocabulary is not the
defect that item guards against. It does not verify a DONE item's historical
accuracy is worth preserving vs. correcting -- CHANGELOG.md's own precedent
 treated an append-only historical log
differently from a live reference page; a board item is neither, and this
script takes no position on whether to rewrite closed work, only reports it
so a human can decide. The HOME-redirect negation heuristic is a fixed phrase
list, not language understanding, and can both under- and over-match.

Usage:  python3 scripts/sweep-stale-board-hazards.py [--open-only]
Exit:   0 always (report tool, not a gate -- board.db is gitignored local
        state a maintainer checkout may or may not have; see SKIPPED below)
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sqlite3
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BOARD_DB = ROOT / ".ideate-work" / "board.db"

FMT_HAZARD = re.compile(
    r"never run (?:a )?(?:package|workspace|whole)[- ]wide[^.\n]{0,60}cargo fmt",
    re.IGNORECASE)

CURRENT_BASELINE = "156 suites / 2454 tests"
STALE_BASELINE_PAIRS = {"129/2276", "129/2292", "130/2301", "130/2314", "131/2315", "153/2346"}
BASELINE = re.compile(
    r"\b(\d{3})\s*suites?\s*/?\s*[\-–—]?\s*([\d,]{3,6})\s*(?:passed|tests)\b",
    re.IGNORECASE)

HOME_REDIRECT = re.compile(r"\bHOME\b[^.\n]{0,80}redirect|redirect[^.\n]{0,80}\bHOME\b")
HOME_NEGATION = re.compile(
    r"without\s+redirect|run\s+without[^.\n]{0,40}redirect|not\s+redirect|"
    r"never\s+redirect|no\s+longer\s+necessary|left\s+at\s+their\s+real\s+values|"
    r"is\s+no\s+longer\s+necessary",
    re.IGNORECASE)


def load_items() -> list[tuple[str, str, str, str]] | None:
    if not BOARD_DB.exists():
        return None
    con = sqlite3.connect(f"file:{BOARD_DB}?mode=ro", uri=True)
    try:
        return con.execute("select id, title, status, spec from items").fetchall()
    finally:
        con.close()


def context(spec: str, start: int, end: int, width: int = 60) -> str:
    lo = max(0, start - width)
    hi = min(len(spec), end + width)
    return spec[lo:hi].replace("\n", " ").strip()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--open-only", action="store_true", help="only report status='open' items"
    )
    args = ap.parse_args()

    items = load_items()
    if items is None:
        print("SKIPPED: .ideate-work/board.db is not present in this checkout.")
        print("  This is a maintainer-only report tool over gitignored local state.")
        return 0

    fmt_hits: list[tuple[str, str, str, str]] = []
    baseline_hits: list[tuple[str, str, str, str, str]] = []
    home_hits: list[tuple[str, str, str, str]] = []

    for id_, title, status, raw_spec in items:
        if args.open_only and status != "open":
            continue

        # Collapse whitespace, INCLUDING newlines, before matching. Board
        # specs are hand-wrapped prose -- "so redirecting\n`HOME`" is a real
        # example found while building this script -- and a regex anchored
        # to "no newline in between" misses exactly the wrapped case, the
        # same "a regex anchored to one context misses the others" hazard
        # check-board-citations.py's own module doc names.
        spec = re.sub(r"\s+", " ", raw_spec)

        for m in FMT_HAZARD.finditer(spec):
            fmt_hits.append((id_, status, title, context(spec, m.start(), m.end())))
            break  # one report per item is enough to act on

        seen_pairs: set[str] = set()
        for m in BASELINE.finditer(spec):
            pair = f"{m.group(1)}/{m.group(2).replace(',', '')}"
            if pair in STALE_BASELINE_PAIRS and pair not in seen_pairs:
                seen_pairs.add(pair)
                baseline_hits.append(
                    (id_, status, title, pair, context(spec, m.start(), m.end()))
                )

        for m in HOME_REDIRECT.finditer(spec):
            window = context(spec, m.start(), m.end(), width=100)
            if HOME_NEGATION.search(window):
                continue
            home_hits.append((id_, status, title, window))
            break

    print(f"FMT HAZARD -- {len(fmt_hits)} item(s) still warn against a package-wide `cargo fmt`")
    print(f"  suggested fix: state plainly the repo is fmt-clean and CI-gated since")
    print(f"  2026-08-13 (mirror's own correction)")
    for id_, status, title, ctx in fmt_hits:
        print(f"  [{status:9}] {id_}  {title[:70]}")
        print(f"      ...{ctx}...")
    print()

    print(f"STALE BASELINE -- {len(baseline_hits)} citation(s) of a dead suite/test count")
    print(f"  current baseline: {CURRENT_BASELINE} (2026-08-13, --workspace --all-features)")
    print(f"  durable fix: say \"re-measure and trust the live number\" rather than")
    print(f"  hardcode a fresh figure that just becomes the next dead one")
    for id_, status, title, pair, ctx in baseline_hits:
        print(f"  [{status:9}] {id_}  {title[:60]}  ({pair})")
        print(f"      ...{ctx}...")
    print()

    print(f"HOME-REDIRECT -- {len(home_hits)} item(s) instruct redirecting HOME unconditionally")
    print(f"  suggested fix: config isolation landed 2026-08-13 -- prefer XDG_CONFIG_HOME")
    print(f"  alone, or if HOME must be redirected, preserve CARGO_HOME/RUSTUP_HOME too")
    for id_, status, title, ctx in home_hits:
        print(f"  [{status:9}] {id_}  {title[:70]}")
        print(f"      ...{ctx}...")
    print()

    total = len(fmt_hits) + len(baseline_hits) + len(home_hits)
    print(f"{total} total hit(s) across {len({h[0] for h in fmt_hits} | {h[0] for h in baseline_hits} | {h[0] for h in home_hits})} unique item(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
