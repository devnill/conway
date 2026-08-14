# Direction

Where conway is going, why, and how far along it is. These four pages are for
the operator and for agents working on the project — not for someone learning to
use conway, who wants [`docs/README.md`](../README.md) instead.

| Page | What it is | Lifetime |
| --- | --- | --- |
| [`INTENT.md`](INTENT.md) | What conway is *for* and what "good" means, in the operator's terms. The authority on intent — `PHILOSOPHY.md` is downstream of it. | Permanent. Added to, never replaced. |
| [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md) | Where the tree actually stands against that intent, at block-diagram altitude. | Snapshot. Replaced each review. |
| [`PLAN.md`](PLAN.md) | The work that follows, split into domains that can run in parallel without colliding. | Snapshot. Replaced each review. |
| [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md) | The re-runnable prompt that regenerates the two snapshots. | Amended when the process changes. |

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
