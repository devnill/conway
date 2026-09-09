"""Shared vocabulary and parsing for the `.changelog.d/` fragment mechanism.

WHY THIS EXISTS. `scripts/check-changelog-fragments.py` (the gate) and
`scripts/collect-changelog.py` (the collector) both need to answer "is this a
subsection CHANGELOG.md's `## [Unreleased]` actually has" and "what bullets
does this fragment declare, under which heading". Those are the same
question asked by two different scripts, and the failure mode of a shared
vocabulary restated twice is a silent drift: one script's copy of the
subsection list gains a case the other's does not, and the mismatch is not an
error, it is a fragment the gate accepts and the collector silently drops (or
the reverse). `CONTRIBUTING.md`'s safety-bearing-code discipline is that this
kind of resolution logic has ONE implementation; this module is that
implementation, imported by both, restated by neither.

WHAT THIS DOES NOT DO. It has no opinion on where `.changelog.d/` lives
relative to a caller's cwd -- `fragment_files` takes the directory as a
parameter -- and it never touches `CHANGELOG.md` itself; merging fragments
into that file is `collect-changelog.py`'s job alone, so a caller that only
wants to validate fragments (the gate) never has to import anything that
knows how to write the changelog.

Usage: imported, not run. `python3 scripts/changelog_fragments.py` does
nothing on its own.
"""

from __future__ import annotations

import pathlib

# The filename a fragment directory listing must skip: it is prose read by a
# contributor, not a fragment the collector folds in.
FRAGMENTS_DIRNAME = ".changelog.d"
FRAGMENTS_README = "README.md"

# THE single source of truth for which `### <Subsection>` headings a fragment
# -- and `## [Unreleased]` itself -- may use. Both `check-changelog-fragments.py`
# and `collect-changelog.py` import this tuple rather than each spelling out
# the four names: this is the same "one implementation of safety-bearing
# resolution logic" discipline `CONTRIBUTING.md` §5 states for the rest of the
# tree, applied to these two scripts instead of restated for them.
SUBSECTIONS: tuple[str, ...] = ("Added", "Changed", "Fixed", "Removed")


class FragmentError(ValueError):
    """A `.changelog.d/*.md` fragment is malformed in a way the gate must
    catch before the collector ever sees it."""


def fragment_files(fragments_dir: pathlib.Path) -> list[pathlib.Path]:
    """Every `.changelog.d/*.md` fragment file, `README.md` excluded, sorted
    by filename. Returns `[]` if the directory does not exist -- an absent
    `.changelog.d/` is "nothing pending", not an error.

    Sorting is by filename alone. `collect-changelog.py`'s module doc states
    what that order does and does not promise about true recency; this
    function only guarantees the same input directory always yields the same
    list in the same order, on any machine, regardless of file mtimes (which
    a git checkout does not preserve) or directory-listing order (which the
    OS does not guarantee).
    """
    if not fragments_dir.is_dir():
        return []
    return sorted(p for p in fragments_dir.glob("*.md") if p.name != FRAGMENTS_README)


def parse_fragment(text: str, source: str) -> dict[str, list[str]]:
    """Parse one fragment's text into `{subsection: [bullet, ...]}`, keyed in
    the order its headings first appear. Each bullet is the raw `- ...` line,
    right-stripped, exactly as it will be written into `CHANGELOG.md` --
    this mechanism does not reword or reflow a bullet, only relocates it.

    Raises `FragmentError`, naming `source`, for every malformed shape this
    mechanism is defined to reject:
      * the file is empty (or whitespace-only)
      * no `### <Subsection>` heading appears anywhere in it
      * a heading names something outside `SUBSECTIONS`
      * a bullet line appears before any heading
      * a heading has zero bullets under it

    A non-blank line that is neither a `### ` heading nor a `- ` bullet is
    rejected too: a fragment is exactly headings and bullets, nothing else,
    so a stray line does not silently vanish into a bullet it was never
    meant to be part of. This deliberately does NOT support a bullet
    wrapping onto a continuation line -- every bullet in `CHANGELOG.md`'s own
    existing convention (see its `## [Unreleased]` section) is one long
    single line, and a fragment matches that convention rather than
    introducing a second one this mechanism would then have to reconcile.
    """
    if not text.strip():
        raise FragmentError(f"{source}: fragment is empty")

    sections: dict[str, list[str]] = {}
    current: str | None = None
    saw_heading = False

    for raw_line in text.splitlines():
        line = raw_line.rstrip()
        stripped = line.strip()

        if not stripped:
            continue

        if line.startswith("### "):
            name = line[4:].strip()
            saw_heading = True
            if name not in SUBSECTIONS:
                known = ", ".join(f"### {s}" for s in SUBSECTIONS)
                raise FragmentError(
                    f"{source}: unknown subsection heading '### {name}' "
                    f"(must be one of: {known})"
                )
            current = name
            sections.setdefault(current, [])
            continue

        if stripped.startswith("- "):
            if current is None:
                raise FragmentError(
                    f"{source}: bullet {stripped!r} appears before any "
                    f"'### <Subsection>' heading"
                )
            sections[current].append(stripped)
            continue

        raise FragmentError(
            f"{source}: unrecognized line (expected a '### <Subsection>' "
            f"heading or a '- ' bullet): {stripped!r}"
        )

    if not saw_heading:
        raise FragmentError(f"{source}: no recognizable '### <Subsection>' heading found")

    for name, bullets in sections.items():
        if not bullets:
            raise FragmentError(f"{source}: '### {name}' has no bullets under it")

    return sections
