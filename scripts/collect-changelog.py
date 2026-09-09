#!/usr/bin/env python3
"""Fold every `.changelog.d/*.md` fragment into `CHANGELOG.md`'s
`## [Unreleased]` section, then delete the fragments it consumed.

WHY THIS EXISTS. See `scripts/check-changelog-fragments.py`'s module doc for
the conflict this mechanism replaces. That script is the gate a fragment
must pass; this script is the fold -- the thing a build lane runs at a
wave's gate, once every writer in the batch has landed its own
`.changelog.d/<slug>.md`, to produce the single `CHANGELOG.md` edit that
used to be eight hand-merged ones.

WHAT THIS DOES:
  1. Reads every `.changelog.d/*.md` fragment (`README.md` excluded),
     parsed by the one shared `scripts/changelog_fragments.py` vocabulary
     the gate also uses.
  2. Groups bullets by target subsection (`Added`/`Changed`/`Fixed`/
     `Removed`) across ALL fragments in the batch.
  3. Merges them into `## [Unreleased]`'s matching subsections, creating a
     subsection that does not yet exist, and creating `## [Unreleased]`
     itself if `CHANGELOG.md` somehow has none.
  4. Deletes the fragments it just folded in.

Never touches anything outside `## [Unreleased]`: no other version section
is read or written, and a subsection this run does not add to is
reconstructed byte-for-byte from its own existing text, not reformatted.

MERGE ORDER, AND WHAT IT DOES NOT PROMISE. Fragments are processed in
DESCENDING filename order, so a fragment whose bullets target the same
subsection as another's ends up ABOVE it -- consistent with this project's
"newest at the top" convention for `## [Unreleased]`, and the whole reason a
batch of concurrent writers needs a defined order at all: the merge must be
deterministic and reproducible (the same fragment set always produces the
same `CHANGELOG.md` text, regardless of which worktree happened to write
its file first, or in what order a filesystem lists a directory). Work-item
ids are ULIDs, which sort lexicographically in creation order, so for the
expected case (a fragment named after its work item) descending filename
order also happens to be newest-item-first. This script does not rely on
that being true, and does not otherwise attempt to determine genuine
recency -- file mtimes are not preserved by a git checkout and are not
trusted here.

WHAT THIS DELIBERATELY DOES NOT DO:
  * It does not run the gate. A malformed fragment makes this script fail
    loudly (see below) rather than silently drop or mis-file a bullet, but
    the descriptive, multi-fragment reporting `check-changelog-fragments.py`
    gives a human belongs to that script, not this one -- run the gate
    first, as `scripts/check-fast-gates.sh` and the invocation this script's
    own `--dry-run` output recommends.
  * It does not reword, reflow, deduplicate, or reorder any EXISTING bullet.
    A subsection this run has nothing new for is byte-identical before and
    after.
  * It is not release automation. It never bumps a version, renames
    `## [Unreleased]` to a dated section, or touches git in any way.

Usage:  python3 scripts/collect-changelog.py [--dry-run] [--root PATH]
        --dry-run   report what would change; write nothing; exit 0
        --root PATH repo root containing CHANGELOG.md and .changelog.d/
                    (default: this repo -- override to run against a
                    scratch copy for testing)
Exit:   0 on success (including "nothing to collect") | 1 on a malformed
        fragment or a missing CHANGELOG.md
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from changelog_fragments import (  # noqa: E402
    FRAGMENTS_DIRNAME,
    FragmentError,
    SUBSECTIONS,
    fragment_files,
    parse_fragment,
)

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

UNRELEASED_RE = re.compile(r"^## \[Unreleased\]\n", re.MULTILINE)
NEXT_HEADING_RE = re.compile(r"^## \[", re.MULTILINE)
# Every `### <heading>` line inside the `## [Unreleased]` body, whatever it
# names. This deliberately does NOT restrict itself to `SUBSECTIONS`, and
# deliberately does not try to match a subsection's bullets: the parser below
# must account for EVERY BYTE of the block, so it has to see a heading it does
# not recognize rather than skip silently past it.
#
# WHY THIS IS NOT A BULLET-SHAPED REGEX ANY MORE, and it is the whole reason
# this file has a test suite. The first version of this parser matched
# `^### (Added|Changed|Fixed|Removed)\n\n((?:-.*\n)+)\n` and rebuilt the block
# from ONLY what it matched -- so any text the pattern failed to match was
# silently dropped on write. Two shapes fell straight through it: a
# subsection whose last bullet is not followed by a blank line (i.e.
# `## [Unreleased]` is the final section of the file, with the ordinary single
# trailing newline), and a subsection heading appearing twice. Both deleted
# real, already-published changelog text and exited 0.
#
# `INTENT.md` §8.3's rule is that silent loss is the offence. A parser that
# infers "this subsection is absent" from its own non-match is structurally
# incapable of honouring that, however many shapes it is taught -- so the
# shape below reads headings only, every span between two headings is kept
# verbatim, and `_assert_no_bullet_lost` refuses to return a result that
# dropped a line.
BLOCK_HEADING_RE = re.compile(r"^### (.+?)[ \t]*\n", re.MULTILINE)

# Any `- ` bullet line, used only by the post-merge conservation check.
BULLET_RE = re.compile(r"^-.*$", re.MULTILINE)


def collect_bullets(
    frag_paths: list[pathlib.Path],
) -> tuple[dict[str, list[str]], list[pathlib.Path]]:
    """Read and parse every fragment in `frag_paths` (descending filename
    order -- see module doc). Returns `(bullets_by_subsection, consumed)`;
    raises `FragmentError` on the first malformed fragment, naming it, and
    consumes/deletes nothing in that case."""
    result: dict[str, list[str]] = {name: [] for name in SUBSECTIONS}
    consumed: list[pathlib.Path] = []
    for path in sorted(frag_paths, key=lambda p: p.name, reverse=True):
        parsed = parse_fragment(path.read_text(), path.name)
        for name, bullets in parsed.items():
            result[name].extend(bullets)
        consumed.append(path)
    result = {name: bullets for name, bullets in result.items() if bullets}
    return result, consumed


def _split_block(block: str) -> tuple[str, list[list[str]]]:
    """Split an `## [Unreleased]` body into `(leading_text, sections)`, where
    `sections` is an ordered list of `[heading_name, body_text]` pairs.

    EVERY BYTE OF `block` IS ACCOUNTED FOR: `leading_text` is whatever
    precedes the first `### ` heading, and each section's body is the exact
    span from just after its heading line to just before the next heading (or
    the end of the block). Concatenating `leading_text` with each
    `f"### {name}\\n" + body` reproduces `block` byte for byte. That property
    is what makes silent loss structurally impossible here, and
    `_assert_no_bullet_lost` checks it held.

    A heading this mechanism does not recognize (anything outside
    `SUBSECTIONS`) is returned like any other: it is not this script's to
    delete, only to carry through untouched.
    """
    matches = list(BLOCK_HEADING_RE.finditer(block))
    if not matches:
        return block, []
    leading = block[: matches[0].start()]
    sections: list[list[str]] = []
    for i, sm in enumerate(matches):
        end = matches[i + 1].start() if i + 1 < len(matches) else len(block)
        sections.append([sm.group(1).strip(), block[sm.end() : end]])
    return leading, sections


def _prepend_bullets(body: str, bullets: list[str]) -> str:
    """Insert `bullets` at the top of an existing subsection `body`, below
    whatever blank line already separates the heading from its content, and
    above every bullet already there (this project's `## [Unreleased]`
    convention is newest-first). The rest of `body` is untouched."""
    if not bullets:
        return body
    i = 0
    while i < len(body) and body[i] == "\n":
        i += 1
    lead, rest = body[:i], body[i:]
    if not lead:
        lead = "\n"
    return lead + "".join(f"{b}\n" for b in bullets) + rest


def _assert_no_bullet_lost(before: str, after: str) -> None:
    """Refuse to return a merge result that dropped a bullet line that was
    in the original text.

    This is a belt-and-braces guard over `_split_block`'s byte-conservation
    property, and it exists because the defect it catches has already
    happened once here: the previous parser silently deleted published
    changelog entries and exited 0. A conservation check that can only fire
    on a real regression is cheap; the alternative is trusting that every
    future edit to this file preserves a property nothing enforces.
    """
    lost = [
        line
        for line in BULLET_RE.findall(before)
        if line.strip() and line not in BULLET_RE.findall(after)
    ]
    if lost:
        raise RuntimeError(
            "collect-changelog: refusing to write -- the merge would have "
            f"dropped {len(lost)} existing bullet line(s) from CHANGELOG.md, "
            "which is a bug in this script, not in your fragment. First "
            f"dropped line: {lost[0]!r}"
        )


def _render_block(
    leading: str,
    sections: list[list[str]],
) -> str:
    """Render an `## [Unreleased]` body back out from `_split_block`'s
    representation, after `merge_bullets` has folded new bullets into it."""
    return leading + "".join(f"### {name}\n{body}" for name, body in sections)


def merge_bullets(changelog_text: str, new_bullets: dict[str, list[str]]) -> str:
    """Return `changelog_text` with `new_bullets` folded into
    `## [Unreleased]`, creating that section (and/or a missing subsection
    within it) if needed. `new_bullets` maps subsection name to its bullets
    in final merge order (see `collect_bullets`)."""
    m = UNRELEASED_RE.search(changelog_text)

    if m is None:
        fresh = [
            [s, "\n" + "".join(f"{b}\n" for b in new_bullets[s])]
            for s in SUBSECTIONS
            if s in new_bullets
        ]
        block = _render_block("\n", fresh)
        new_section = "## [Unreleased]\n" + block
        next_m = NEXT_HEADING_RE.search(changelog_text)
        if next_m:
            at = next_m.start()
            return changelog_text[:at] + new_section + changelog_text[at:]
        if changelog_text and not changelog_text.endswith("\n"):
            changelog_text += "\n"
        sep = "\n" if changelog_text and not changelog_text.endswith("\n\n") else ""
        return changelog_text + sep + new_section

    header_end = m.end()
    next_m = NEXT_HEADING_RE.search(changelog_text, header_end)
    block_end = next_m.start() if next_m else len(changelog_text)
    block = changelog_text[header_end:block_end]

    leading, sections = _split_block(block)

    # Fold each subsection's new bullets into the FIRST existing section of
    # that name. A duplicate heading (which this tool never produces, but a
    # hand-edited file can carry) keeps both occurrences and all their
    # bullets -- the previous parser's name-keyed dict silently kept only the
    # last one.
    placed: set[str] = set()
    for section in sections:
        name = section[0]
        if name in new_bullets and name not in placed:
            section[1] = _prepend_bullets(section[1], new_bullets[name])
            placed.add(name)

    # A subsection with new bullets that the block does not have yet is
    # inserted in `SUBSECTIONS` order relative to the known subsections
    # already present, so `## [Unreleased]` keeps its documented section
    # order. An unrecognized heading is never used as an anchor and never
    # moved -- it stays exactly where its author put it.
    for name in SUBSECTIONS:
        if name not in new_bullets or name in placed:
            continue
        body = "\n" + "".join(f"{b}\n" for b in new_bullets[name]) + "\n"
        rank = SUBSECTIONS.index(name)
        at = len(sections)
        for i, (existing_name, _) in enumerate(sections):
            if existing_name in SUBSECTIONS and SUBSECTIONS.index(existing_name) > rank:
                at = i
                break
        sections.insert(at, [name, body])
        placed.add(name)

    new_block = _render_block(leading, sections)
    merged = changelog_text[:header_end] + new_block + changelog_text[block_end:]
    _assert_no_bullet_lost(changelog_text, merged)
    return merged


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="report what would change; write nothing"
    )
    parser.add_argument(
        "--root",
        type=pathlib.Path,
        default=REPO_ROOT,
        help="repo root containing CHANGELOG.md and .changelog.d/ (default: this repo)",
    )
    args = parser.parse_args()

    root = args.root.resolve()
    changelog_path = root / "CHANGELOG.md"
    fragments_dir = root / FRAGMENTS_DIRNAME

    frag_paths = fragment_files(fragments_dir)
    if not frag_paths:
        print(f"no fragments in {FRAGMENTS_DIRNAME}/ -- CHANGELOG.md left untouched")
        return 0

    try:
        new_bullets, consumed = collect_bullets(frag_paths)
    except FragmentError as exc:
        print(f"collect-changelog: {exc}", file=sys.stderr)
        print(
            "run scripts/check-changelog-fragments.py first -- a fragment "
            "failed to parse",
            file=sys.stderr,
        )
        return 1

    if not changelog_path.is_file():
        print(f"collect-changelog: {changelog_path} not found", file=sys.stderr)
        return 1

    original = changelog_path.read_text()
    merged = merge_bullets(original, new_bullets)

    total = sum(len(v) for v in new_bullets.values())
    print(f"merging {total} bullet(s) from {len(consumed)} fragment(s):")
    for name in SUBSECTIONS:
        for bullet in new_bullets.get(name, []):
            preview = bullet if len(bullet) <= 88 else bullet[:85] + "..."
            print(f"  -> ### {name}: {preview}")

    if args.dry_run:
        print("[dry-run] not writing CHANGELOG.md, not deleting fragments")
        return 0

    changelog_path.write_text(merged)
    for path in consumed:
        path.unlink()
    print(f"wrote {changelog_path}, removed {len(consumed)} fragment(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
