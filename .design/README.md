# .design

This directory is a record of how and why conway was built — including
alternatives that were considered and rejected, and status banners marking
what has since been superseded or landed.

## Claim-bearing documents, and the check over them

Most of what is here **cannot go stale**, and is deliberately ungated: a
rejected alternative stays rejected, and `extension-architecture.md`'s
thousands of unbuilt lines are not a defect because this directory is declared
non-authoritative and that document is labelled design-not-implementation.

A small subset is different. A document is **claim-bearing** when it asserts
*what is true of the tree right now* — not how a decision was reached, but what
the code does or does not do today. Those claims rot, and a reader has no way
to tell a current one from a stale one by looking.

The subset, today:

| Document | Why it is claim-bearing |
| --- | --- |
| [`philosophy-debt.md`](philosophy-debt.md) | Its own contract is that a claim *absent* from it is expected to be true right now. That makes `PHILOSOPHY.md`'s present-tense exemption safe, and makes an error in it unsafe in whichever direction the error runs. |

Nothing else in this directory is covered, and adding a document to the subset
is a deliberate act: edit `CLAIM_BEARING` in
[`scripts/check-design-claims.py`](../scripts/check-design-claims.py).

**How the check works.** A claim-bearing document declares, beside each claim,
the predicate that would falsify it — an `absent` pattern for "this is not
built yet" or a `present` pattern for "this exists". `check-design-claims.py`
evaluates them and fails naming the document, the entry, and the claim's own
words. So when a capability the ledger calls unbuilt actually ships, the
predicate starts matching and the check goes red in the same change.

That shape was chosen over the obvious alternative — failing when a commit
touches a mapped area without touching the ledger — because the obvious one is
noisy enough to get switched off: it would demand a ledger edit for every test
tweak near the hooks code. This one is quiet until a claim is actually false.

**What it does not catch**, stated because a gate with unstated blind spots is
the same defect one level up: a claim with no predicate, or with a predicate
narrower than its prose; a claim wrong in a way no pattern expresses; and rot
in the reasoning around a still-true mechanical fact. The script's own module
doc carries this list too.

It is **not** user documentation and is not maintained as such. If you are
looking for docs on using conway, go to [`docs/`](../docs/) instead.

For the plugin and hook system specifically,
[`docs/plugins/`](../docs/plugins/README.md) is the **authoritative**
statement of what an author may rely on. The spike material here records how
those decisions were reached, including alternatives that were rejected —
open it to learn *why* something is the way it is, never to learn what to
build against. Where a page here and a page there disagree, `docs/plugins/`
wins.

`extension-architecture.md` is the synthesis of `d1-transport.md` through
`d5-template-instrumentation.md` and supersedes those five specs where they
disagree with it.

`d7-repetition-resistant-tool-calls.md` is a captured design direction
(out of scope for implementation as of 2026-08-03): solving the
repeated-tool-call loop class by idiomatic tool-result design rather than
detection. It is the framing for any future revisit of the
[`docs/whitepaper.md`](../docs/whitepaper.md) §4.5 vs WI-086 (in-core
`StepDigest`) question.
