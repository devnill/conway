# Direction

Where conway is going, why, and how far along it is. These pages are for
the operator and for agents working on the project — not for someone learning to
use conway, who wants [`docs/README.md`](../README.md) instead.

| Page | What it is | Lifetime |
| --- | --- | --- |
| [`INTENT.md`](INTENT.md) | What conway is *for* and what "good" means, in the operator's terms. The authority on intent — `PHILOSOPHY.md` is downstream of it. | Permanent. Added to, never replaced. |
| [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md) | Where the tree actually stands against that intent, at block-diagram altitude. | Snapshot. Replaced each review. |
| [`PLAN.md`](PLAN.md) | The work that follows, split into domains that can run in parallel without colliding. | Snapshot. Replaced each review. |
| [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md) | The re-runnable prompt that regenerates the two snapshots. Paste §2 into a fresh session; it dispatches the reviewers. | Amended when the process changes. |
| [`review/`](review/) | What each reviewer reads: shared conduct plus one lens per perspective. Loaded by the reviewer that needs it, not pasted. | Accumulates. |

## Design records and catalogues

The table above is the review cycle's own instruments — five pages, always
exactly that shape. Everything else in this directory is a *finding* the
review cycle produced: a design record settling a specific question, or a
research pass answering one. This table exists precisely because the first
one does not cover them, and an unindexed page in this directory has already
cost a full research cycle once (board item `01M0TV5PN8RR9NN97AWP09E6K7`,
EMB-1 — a review reported no discussion of non-Rust bindings existed
anywhere in the tree, because the 397-line survey that already existed was
linked from nothing). **Every `.md` file added directly to this directory
gets a row in one of these two tables in the same change that adds it** —
`scripts/check-orphan-docs.py` enforces this one mechanically; the rest is
this convention.

| Page | What it is | Lifetime |
| --- | --- | --- |
| [`CATALOGUE.md`](CATALOGUE.md) | What a default install should have one toggle away — a candidate survey against `INTENT.md` §7a/§7b, ranked and costed. Board item `01M00QEQ0PVAM2S7Y9EQNZV32F` (R1). | Living. Swept for accuracy each review, not replaced. |
| [`DESIGN-context-path.md`](DESIGN-context-path.md) | The context-path and curation design: what a `PathLog` is, what a curator may do, what stayed genuinely open and what settled. Corrections are appended dated, never absorbed upward. | Living. Amended in place, append-only. |
| [`DESIGN-bindings.md`](DESIGN-bindings.md) | The non-Rust bindings survey — Diplomat vs. UniFFI vs. `cbindgen` against conway's async streaming public API — in the same falsifiable-hypothesis register as the page above. Supersedes `BINDINGS.md`. | Living. Amended in place, append-only. |
| [`BINDINGS.md`](BINDINGS.md) | Retired. A 26-line pointer to `DESIGN-bindings.md`, kept instead of deleted because two board items and git history cite it by path. | Historical. Read `DESIGN-bindings.md` instead. |

## How these relate to the rest of the documentation

```
INTENT.md          what the operator wants           sentiment
    ↓
PHILOSOPHY.md      what conway promises at 1.0       specification
    ↓
ARCHITECTURE.md    how the pieces fit today          mechanism
    ↓
docs/*.md          how to drive it                   practice
```

The arrows are also the order in which a disagreement is resolved. If the code
disagrees with `ARCHITECTURE.md`, fix the code or the page. If `PHILOSOPHY.md`
disagrees with `INTENT.md`, `PHILOSOPHY.md` is wrong. And if a question reaches
`INTENT.md` and finds no answer, that is a failure of the specification rather
than a gap in the code — write the missing sentiment down before deciding
anything.
