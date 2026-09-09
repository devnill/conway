#!/usr/bin/env python3
"""Tests for `scripts/collect-changelog.py`'s merge, the only code in this
repository that writes to `CHANGELOG.md`.

WHY THIS EXISTS, and it is not "because tests are good". The first version of
`merge_bullets` rebuilt `## [Unreleased]` from only what a bullet-shaped regex
matched, and discarded every byte the regex missed. Two ordinary shapes fell
through it and DELETED PUBLISHED CHANGELOG ENTRIES while exiting 0:

  * `## [Unreleased]` as the last section of the file, whose final bullet is
    followed by a single newline rather than a blank line -- the shape any
    changelog has before its first release is cut.
  * the same `### <Subsection>` heading appearing twice in the block.

Neither was caught by the item's own acceptance criteria, which exercised the
happy path (fragments merge) and the empty path (nothing to merge) -- the two
cases that cannot distinguish a correct parser from that one. P-15's rule is
that a check is not established until it has been shown to fail, and a merge
is a check on its own output: `test_regression_*` below are the two shapes,
kept as tests rather than as a fixed bug, because the failure was silent and
the next person to touch the parser deserves to find out from a red test
rather than from a reader noticing an entry went missing.

WHAT THIS DOES NOT COVER. The fragment PARSER (`changelog_fragments.py`) is
exercised by `check-changelog-fragments.py` against real fragments and is not
re-tested here; this file is about the merge only. Nothing here touches the
real `CHANGELOG.md` or `.changelog.d/` -- every case builds its own text in
memory.

Usage:  python3 scripts/test_collect_changelog.py
Exit:   0 all passed | 1 one or more failed
"""

from __future__ import annotations

import importlib.util
import pathlib
import sys

_HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE))

_spec = importlib.util.spec_from_file_location(
    "collect_changelog", _HERE / "collect-changelog.py"
)
cc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cc)


FAILURES: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"  PASS  {name}")
    else:
        print(f"  FAIL  {name}")
        if detail:
            for line in detail.splitlines():
                print(f"        {line}")
        FAILURES.append(name)


def test_regression_unreleased_is_last_section() -> None:
    """The bug: `## [Unreleased]` last in the file, no blank line after its
    final bullet, so the old bullet-shaped regex did not match it and the
    whole subsection was dropped on write."""
    before = (
        "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- an already-published fix\n"
    )
    after = cc.merge_bullets(before, {"Added": ["- a brand new entry"]})
    check(
        "regression: Unreleased-is-last keeps its existing bullet",
        "- an already-published fix" in after,
        f"got:\n{after}",
    )
    check(
        "regression: Unreleased-is-last adds the new bullet",
        "- a brand new entry" in after,
        f"got:\n{after}",
    )


def test_regression_duplicate_heading() -> None:
    """The bug: a name-keyed dict kept only the LAST `### Added`, silently
    dropping the first one's bullets."""
    before = (
        "# Changelog\n\n## [Unreleased]\n\n"
        "### Added\n\n- first block bullet\n\n"
        "### Added\n\n- second block bullet\n\n"
        "## [0.1.0]\n\n- old\n"
    )
    after = cc.merge_bullets(before, {"Added": ["- new bullet"]})
    check(
        "regression: duplicate heading keeps BOTH existing bullets",
        "- first block bullet" in after and "- second block bullet" in after,
        f"got:\n{after}",
    )


def test_conservation_guard_fires() -> None:
    """`_assert_no_bullet_lost` must actually refuse a lossy result -- a guard
    that cannot fire is not a guard (P-15)."""
    try:
        cc._assert_no_bullet_lost("- kept\n- dropped\n", "- kept\n")
    except RuntimeError as exc:
        check(
            "conservation guard fires on a dropped bullet",
            "dropped" in str(exc),
            f"message was: {exc}",
        )
        return
    check("conservation guard fires on a dropped bullet", False, "it did not raise")


def test_conservation_guard_does_not_overfire() -> None:
    """...and must not fire on a correct merge (the second demonstration
    P-15 requires: shown not to fire on the shapes it must tolerate)."""
    try:
        cc._assert_no_bullet_lost("- kept\n", "- added\n- kept\n")
    except RuntimeError as exc:
        check("conservation guard does not overfire", False, f"raised: {exc}")
        return
    check("conservation guard does not overfire", True)


def test_no_unreleased_section_is_created() -> None:
    before = "# Changelog\n\n## [0.1.0]\n\n### Added\n\n- old thing\n"
    after = cc.merge_bullets(before, {"Added": ["- new thing"]})
    check(
        "missing [Unreleased] is created above the next version",
        "## [Unreleased]" in after
        and after.index("## [Unreleased]") < after.index("## [0.1.0]")
        and "- old thing" in after,
        f"got:\n{after}",
    )


def test_empty_changelog() -> None:
    after = cc.merge_bullets("", {"Fixed": ["- first ever entry"]})
    check(
        "empty CHANGELOG.md gains a section",
        "## [Unreleased]" in after and "- first ever entry" in after,
        f"got:\n{after}",
    )


def test_unknown_heading_preserved() -> None:
    """A heading outside SUBSECTIONS is not this script's to delete."""
    before = (
        "# Changelog\n\n## [Unreleased]\n\n"
        "### Security\n\n- a hand-written entry under a heading we do not manage\n\n"
        "## [0.1.0]\n\n- old\n"
    )
    after = cc.merge_bullets(before, {"Added": ["- new"]})
    check(
        "unrecognized heading and its bullets survive",
        "### Security" in after
        and "- a hand-written entry under a heading we do not manage" in after,
        f"got:\n{after}",
    )


def test_nothing_outside_unreleased_changes() -> None:
    before = (
        "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- existing\n\n"
        "## [0.1.0]\n\n### Added\n\n- shipped long ago\n\n"
        "[0.1.0]: https://example.invalid/compare\n"
    )
    after = cc.merge_bullets(before, {"Added": ["- new"]})
    tail_before = before[before.index("## [0.1.0]") :]
    tail_after = after[after.index("## [0.1.0]") :]
    check(
        "everything below [Unreleased] is byte-identical",
        tail_before == tail_after,
        f"before tail:\n{tail_before}\nafter tail:\n{tail_after}",
    )


def test_untouched_subsection_is_byte_identical() -> None:
    before = (
        "# Changelog\n\n## [Unreleased]\n\n"
        "### Added\n\n- existing added\n\n"
        "### Fixed\n\n- existing fixed\n\n"
        "## [0.1.0]\n\n- old\n"
    )
    after = cc.merge_bullets(before, {"Added": ["- new added"]})
    check(
        "a subsection with nothing new is unchanged",
        "### Fixed\n\n- existing fixed\n" in after,
        f"got:\n{after}",
    )
    check(
        "new bullet lands above the existing one in its own subsection",
        after.index("- new added") < after.index("- existing added"),
        f"got:\n{after}",
    )


def test_subsection_order_is_canonical() -> None:
    """A newly created subsection is inserted in SUBSECTIONS order, not
    appended blindly at the end."""
    before = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- a fix\n\n## [0.1.0]\n\n- old\n"
    after = cc.merge_bullets(before, {"Added": ["- an addition"]})
    check(
        "new 'Added' is inserted above existing 'Fixed'",
        after.index("### Added") < after.index("### Fixed"),
        f"got:\n{after}",
    )


def test_bullet_containing_heading_like_text() -> None:
    before = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- mentions `### Fixed` inline\n\n## [0.1.0]\n\n- old\n"
    after = cc.merge_bullets(before, {"Added": ["- new"]})
    # The discriminating observable is whether a HEADING was created, not
    # whether the substring appears -- it appears inside the bullet's own
    # text either way. Ask the parser's own line-anchored regex, which is the
    # thing that would misfire if this were broken.
    headings = [m.group(1) for m in cc.BLOCK_HEADING_RE.finditer(after)]
    check(
        "a bullet mentioning a heading inline is not parsed as a heading",
        "- mentions `### Fixed` inline" in after and headings == ["Added"],
        f"headings parsed: {headings}\ngot:\n{after}",
    )


def main() -> int:
    print("collect-changelog merge tests")
    for fn in [
        test_regression_unreleased_is_last_section,
        test_regression_duplicate_heading,
        test_conservation_guard_fires,
        test_conservation_guard_does_not_overfire,
        test_no_unreleased_section_is_created,
        test_empty_changelog,
        test_unknown_heading_preserved,
        test_nothing_outside_unreleased_changes,
        test_untouched_subsection_is_byte_identical,
        test_subsection_order_is_canonical,
        test_bullet_containing_heading_like_text,
    ]:
        fn()
    print()
    if FAILURES:
        print(f"{len(FAILURES)} failure(s): {', '.join(FAILURES)}")
        return 1
    print("all merge tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
