#!/usr/bin/env python3
"""Every `.changelog.d/*.md` fragment must be well-formed before it lands.

WHY THIS EXISTS. Almost every item on the 2026-09-07 board has to add a
CHANGELOG.md entry (`CONTRIBUTING.md` §1, "Docs and CHANGELOG in the same
change"), and six to eight of them land concurrently, in isolated git
worktrees, several waves deep. Every entry used to go at the top of the same
short `## [Unreleased]` subsection, so a batch landing meant eight edits
stacked on the same three lines, hand-merged -- pure conflict overhead with
no useful signal when it went wrong. `.changelog.d/<slug>.md` fragments
replace that: each writer names a file nobody else names, so two concurrent
writers can never conflict, and `scripts/collect-changelog.py` folds every
fragment into `CHANGELOG.md` at a wave's gate. This script is the gate that
keeps a malformed fragment from ever reaching that fold.

WHAT THIS CHECKS, for every `.changelog.d/*.md` file (`README.md` excluded --
it is prose read by a contributor, not a fragment):

  1. **The fragment parses at all** -- `scripts/changelog_fragments.py`'s
     `parse_fragment` is the one place this vocabulary lives (see that
     module's doc for why it is not restated here); every parse failure it
     raises surfaces here verbatim, naming the file: an empty fragment, no
     recognizable `### <Subsection>` heading, a heading naming something
     outside `Added`/`Changed`/`Fixed`/`Removed`, a bullet before any
     heading, or a heading with zero bullets under it.
  2. **The filename does not collide with an already-collected entry.** A
     fragment's slug (its filename without `.md`) is checked as a plain
     substring of `CHANGELOG.md`'s current text. `collect-changelog.py`
     deletes a fragment the moment it folds it in, so under normal operation
     a live fragment's slug never appears in `CHANGELOG.md` yet -- if it
     does, the far more likely explanation is that this fragment was already
     merged once (a stale worktree resurrecting a deleted file, a rebase
     replaying an already-landed commit) and folding it again would land the
     same bullet twice.

WHAT THIS DOES NOT CATCH, stated because a gate whose blind spots are
unstated is the same defect `scripts/check-orphan-docs.py`'s own doc names
for itself:

  * **A slug collision is a substring match, not a provenance check.** It
    proves the slug's literal text already appears somewhere in
    `CHANGELOG.md`, not that this exact fragment produced it -- a short,
    generic slug could coincidentally match unrelated prose. Fragments named
    after a work-item id (the convention this mechanism expects; see
    `.changelog.d/README.md`) are long, unique ULIDs, so this is a real
    signal for the case it exists to catch and a false positive only for a
    slug someone chose to also be an ordinary word already used in the
    changelog's prose.
  * **A bullet's CONTENT is never checked.** Wording, accuracy, whether it
    actually belongs in the subsection it names -- none of that is
    mechanical; a human reviewing the fragment's diff is still what catches
    a wrong or misleading bullet, the same as it always was for a direct
    `CHANGELOG.md` edit.
  * **This gate has no job in `.github/workflows/ci.yml`.** Like `orphan
    docs (docs/vision index + reachability)` and `ideate record layout` in
    `scripts/check-fast-gates.sh`, it runs locally and in that script's
    default sweep, but is NOT enforced on a PR the way `fmt`/`design
    claims`/`board citations`/`doc`/`clippy` are. Wiring a new CI job is that
    workflow's owner's call, out of this item's file ownership -- stated
    here so a reader does not believe more coverage exists than does.

Usage:  python3 scripts/check-changelog-fragments.py [--verbose] [--root DIR]
Exit:   0 clean | 1 violations found

`--root` exists so this gate can be pointed at a fixture directory rather
than only at this repo, and it mirrors `scripts/collect-changelog.py`'s flag
of the same name. That is not a convenience: a check is not established
until it has been shown to FAIL, and a gate that can only ever run against
the real `.changelog.d/` can only be shown to fail by planting a malformed
fragment in the working tree and hoping to remember to remove it. Both
scripts take the same flag so the pair can be exercised the same way.
"""

from __future__ import annotations

import argparse
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from changelog_fragments import (  # noqa: E402
    FRAGMENTS_DIRNAME,
    FragmentError,
    fragment_files,
    parse_fragment,
)

ROOT = pathlib.Path(__file__).resolve().parent.parent


def scan(verbose: bool, root: pathlib.Path = ROOT) -> int:
    fragments_dir = root / FRAGMENTS_DIRNAME
    changelog_path = root / "CHANGELOG.md"
    frag_paths = fragment_files(fragments_dir)
    changelog_text = changelog_path.read_text() if changelog_path.is_file() else ""

    problems: list[str] = []
    for path in frag_paths:
        rel = path.relative_to(root)
        text = path.read_text()
        try:
            parse_fragment(text, str(rel))
        except FragmentError as exc:
            problems.append(str(exc))
            continue

        # A HEURISTIC, AND ITS FALSE-POSITIVE MODE IS STATED RATHER THAN
        # DISCOVERED. This asks "does this fragment's slug already appear
        # anywhere in CHANGELOG.md", which answers the real question ("was
        # this fragment already merged once?") only because `.changelog.d/`'s
        # own README tells a writer to put the board item id in the bullet
        # text -- so a merged fragment leaves its slug behind in the prose.
        #
        # It is a plain substring match, so a SHORT or common slug collides
        # falsely: a fragment named `f.md` or `web.md` matches somewhere in a
        # 6700-line changelog every time. Measured against the slugs this
        # board's own items produce -- ULIDs, and kebab forms like
        # `b1-web`/`a3-checkpoint` -- none collide, which is why the check is
        # left as it is rather than made cleverer. The failure is loud, names
        # the fragment, and the message already tells the writer to rename;
        # a false positive costs one rename, where a missed double-merge
        # costs a duplicated changelog entry nobody notices.
        slug = path.stem
        if slug and slug in changelog_text:
            problems.append(
                f"{rel}: filename collides with an already-collected entry -- "
                f"'{slug}' already appears in CHANGELOG.md, so this fragment "
                f"was very likely already merged once; delete it if so, or "
                f"rename it if the collision is coincidental"
            )

    if verbose:
        print(f"scanned {len(frag_paths)} fragment(s) in {FRAGMENTS_DIRNAME}/")

    for p in sorted(problems):
        print(p)

    print(
        f"\n{len(problems)} changelog-fragment problem(s) found across "
        f"{len(frag_paths)} fragment(s)"
    )
    return 1 if problems else 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument(
        "--root",
        type=pathlib.Path,
        default=ROOT,
        help="repo root containing CHANGELOG.md and .changelog.d/ (default: this repo)",
    )
    args = parser.parse_args()
    return scan(args.verbose, args.root.resolve())


if __name__ == "__main__":
    sys.exit(main())
