# .design

This directory is a record of how and why conway was built — including
alternatives that were considered and rejected, and status banners marking
what has since been superseded or landed.

## Claim-bearing predicates, and the check over them

Most of what is here **cannot go stale**, and is deliberately ungated: a
rejected alternative stays rejected, and `extension-architecture.md`'s
thousands of unbuilt lines are not a defect because this directory is declared
non-authoritative and that document is labelled design-not-implementation.

Until 2026-08-13 one document was different: `philosophy-debt.md` asserted
*what is true of the tree right now* rather than how a decision was reached,
with a stated contract that a claim *absent* from it was expected to be true
right now. That is what made `PHILOSOPHY.md`'s present-tense exemption safe. It
was retired that day (board items `01KZYAHSXDXFDY9FX5MXPRZ4M1`,
`01KZY8TEE2FDWQMHEKJDDC3SG9`): the *predicates* — the falsifiable `absent`/
`present` patterns that made the contract checkable — moved to
[`../scripts/board-claims.md`](../scripts/board-claims.md), and the *narrative*
each one used to carry as ledger prose moved to the open board item that now
owns it (see that file's header for the reasoning, including why the
predicates live in a git-tracked file rather than inside a board item's row).
The completeness contract survives in the new form: every gap between
`PHILOSOPHY.md` and the tree is now an open board item carrying a falsifiable
predicate, stated in `CONTRIBUTING.md` §2.

**How the check works.** `scripts/board-claims.md` declares, beside each
claim, the predicate that would falsify it — an `absent` pattern for "this is
not built yet" or a `present` pattern for "this exists" — and the `board_item:`
that owns it. `check-design-claims.py` evaluates the predicates (unconditionally,
including in CI) and, on a maintainer checkout where `.ideate-work/board.db` is
reachable, additionally resolves each `board_item:` read-only and reports its
live status. It fails naming the board item, the claim, and the claim's own
words. So when a capability a predicate calls unbuilt actually ships, the
predicate starts matching and the check goes red in the same change.

That shape was chosen over the obvious alternative — failing when a commit
touches a mapped area without touching the ledger — because the obvious one is
noisy enough to get switched off: it would demand a ledger edit for every test
tweak near the hooks code. This one is quiet until a claim is actually false.

**What it does not catch**, stated because a gate with unstated blind spots is
the same defect one level up: a claim with no predicate, or with a predicate
narrower than its prose; a claim wrong in a way no pattern expresses; rot in
the reasoning around a still-true mechanical fact; and a `board_item:` citation
that has gone stale, which only a maintainer checkout with the board reachable
can catch. The script's own module doc carries this list too.

It is **not** user documentation and is not maintained as such. If you are
looking for docs on using conway, go to [`docs/`](../docs/) instead.

For the plugin and hook system specifically,
[`docs/plugins/`](../docs/plugins/README.md) is the **authoritative**
statement of what an author may rely on. The spike material here records how
those decisions were reached, including alternatives that were rejected —
open it to learn *why* something is the way it is, never to learn what to
build against. Where a page here and a page there disagree, `docs/plugins/`
wins.

`extension-architecture.md` is the synthesis of the original `d1`–`d5` spike
specs and supersedes them where they disagree with it.

Three of those spikes were **removed on 2026-08-13**: `d2-extension-points.md`
and `d5-template-instrumentation.md`, both fully superseded by the synthesis,
and `d7-repetition-resistant-tool-calls.md`, a design direction recorded as out
of scope and never picked up. Together they were 11,680 words that nothing in
`crates/`, `docs/`, or the root pages cited. They are in git history if the
reasoning is ever wanted again; recovering one is `git show`, and reconstructing
it from the synthesis is usually easier than reading it.

That deletion is the standing policy for this directory, not a one-off:
**a spike doc survives here only while something still reads it.** Design
material that is still load-bearing belongs in `docs/`, where
`CONTRIBUTING.md` §1 gates it against going stale. Work that still needs doing
belongs on the board, which has a lifecycle. This directory is for the record of
how a decision was reached — and a record nobody consults is not a record.
