# Reviewer conduct

> **Every reviewer reads this file first, then exactly one `lens-*.md`.**
> It holds the things that are true for all of them: what to read, what counts
> as verified, how to spend the budget, and the failure modes that produced
> real shipped defects in this tree.
>
> This file exists so that no lens has to restate any of it. If you find
> yourself wanting to copy a paragraph from here into a lens, the lens is
> wrong — cite the section instead.

---

## 1. The reading list

Read these before touching the tree. They are the authority, in this order,
and a disagreement between two of them is resolved by whichever is higher.

| Document | What it is | Weight |
| --- | --- | --- |
| `docs/vision/INTENT.md` | what the operator wants and what "good" means | the authority on intent |
| `PHILOSOPHY.md` | the 1.0 specification | downstream of INTENT |
| `ARCHITECTURE.md` | how the pieces fit today | downstream of PHILOSOPHY |
| `scripts/board-claims.md` | machine-checked ledger of falsifiable claims | fastest honest read of spec-vs-code drift; CI fails when an entry stops being true **in either direction** |
| `CONTRIBUTING.md` §2 | the rule that keeps "Where the tree is today" notes honest | procedural |

`PHILOSOPHY.md` is written in the present tense. Claims that are ahead of the
tree carry a **"Where the tree is today"** note. Anything without such a note is
asserted to be true right now, and if it is not, that is a finding.

**Read only what your lens needs.** You are one of several reviewers running in
parallel. The lens tells you your territory; do not read outside it to
double-check someone else's. Redundant reading is the single largest cost in
this review and it produces duplicate findings the lead then has to merge.

### On the `docs/vision/DESIGN-*.md` documents

These are design records written **before** the work they describe. They state
what a mechanism was expected to need, and those expectations are **hypotheses,
not constraints the tree must satisfy**.

Where a design doc and the shipped code disagree, that is a FINDING to
investigate, not automatically a defect in the code — the doc may be what is
wrong. `DESIGN-context-path.md` §11.7 is a live example: it asserts memory
"needs no storage of its own, no retrieval semantics of its own, and no new
port", and the operator overrode that on 2026-08-18 after the claim was built to
and failed. See that section's own amendment note.

**Do not recommend reverting shipped work solely because a design document
predicted a different shape.** This is `INTENT.md` §8.8 stated as conduct.

---

## 2. What counts as verified

> **Verify before asserting. Cite `path:line` for every claim a reader might
> want to check.**

- **Both directions matter.** A document that *understates* what is built is the
  same defect as one that overstates it, and this repository has been bitten by
  the understating kind — five top-level pages once said memory, skills and MCP
  were unbuilt while all three shipped.
- **A read path is not a write path.** This tree shipped a memory feature that
  selected sessions by a label nothing could ever set: the filter existed, the
  query existed, both were reachable, and no code path wrote the field. When a
  design rests on a piece of data, verify **both ends** before believing it
  works.
- **Prefer one end-to-end proof through the real surface over two half-proofs.**
  A test that exercises a seam demonstrates the seam compiles, which was never
  in question. `INTENT.md` §8.5: a surface is proven when something that is not
  its author uses it to do a thing someone wanted.
- **The counts are not what `wc -l` says.** Production lines must be separated
  from test lines (count from the first `#[cfg(test)]` in each file). The raw
  figure is roughly 3× the production figure and will mislead you. The lead
  agent has already measured this and passed you the table — **use it, do not
  re-derive it.**

---

## 3. How to spend the budget

You have a **tool-call budget and a return-size limit**, both stated in your
dispatch brief. They are not advisory. This review runs many agents in parallel
and its wall-clock cost is the slowest agent, not the average one.

- **Stop when the marginal finding stops changing the plan.** You are not
  producing an inventory. Three findings that would each change what gets built
  next are worth more than fifteen that are true.
- **Sample, then confirm.** Establish a pattern from three instances and confirm
  its extent with one counting command. Do not enumerate every instance by hand.
- **One counting command beats ten reads.** `grep -c`, `grep -rl … | wc -l`, and
  `git log --format= --name-only` answer most extent questions in one call.
- **Do not fix anything.** Findings become board items. A reviewer who edits the
  tree has invalidated every other reviewer's citations.
- **Do not re-plan.** You return findings; the lead writes the plan.
- **Where the intent is genuinely ambiguous, say so rather than picking.** Every
  question you have to ask is itself a finding about `INTENT.md`, and
  `INTENT.md` §8.1 makes it the most important kind.

---

## 4. What you return

A single structured response, under the word limit in your brief. The lead
merges these mechanically — anything outside this shape costs a re-read.

```
## Verdict
One paragraph. What is the state of your territory, in the operator's terms?
Say what is GOOD as plainly as what is broken.

## Findings
For each, in descending order of how much it would change the plan:

### F<n>. <one-line claim>
- **Evidence:** path:line, path:line — what you actually checked
- **Extent:** how big, measured not guessed
- **Against:** which INTENT.md § this violates, or "none — see Intent gaps"
- **What done looks like:** the acceptance a board item would carry
- **Size:** S / M / L
- **Cost of not doing it:** what stays hard, or what breaks later

## Intent gaps
Questions your territory raised that INTENT.md does not answer. One line each.

## Not checked
What your territory contains that you did not get to, and why.
```

The **Not checked** section is mandatory and is not a confession. A review that
silently bounds its own coverage reads as "covered everything" when it did not —
which is §5's fourth failure mode wearing a different hat.

---

## 5. Four failure modes this review exists to catch

Each produced a real, shipped defect in this repository. They are listed as
things to LOOK FOR in the tree, **and** as things to avoid while reviewing.

**1. A premise defended by a series of workarounds.** When a feature has
accumulated two or more accommodations — a limitation filed as an "ergonomics
follow-up", a missing operation left as an "open question", a cap standing in for
a bound — read them TOGETHER rather than one at a time. Each accommodation is
usually locally reasonable, which is what makes the series invisible; the series
is the signal that the premise underneath is failing. Watch especially for
reassuring phrasing applied to a workaround: **"bounded by construction"** was
written in this tree to describe a cap that existed only because the unit of work
was wrong. A symptom phrased as a virtue is the hardest kind to see.

**2. A capability verified in one direction only.** See §2. A complete READ path
is not evidence of a WRITE path.

**3. A design document treated as a constraint.** See §1. When the tree and a
`DESIGN-*.md` disagree, investigate which is wrong. Do not assume the code.

**4. A limitation reported after the success.** If something works end to end but
is not usable — a feature whose enabling path is unwired, a store that forgets on
restart — **say the limitation FIRST**. Both occurred here, and in both cases the
accurate-but-late framing left a reader believing something was finished when it
was not. A review that buries the caveat has misled its reader even if every
sentence is true.

---

## 6. The board is MCP, not the YAML directory

If your lens tells you to consult the board, the board is the **ideate MCP
server** (`work_list`, `work_get`).

**Do NOT read `.ideate/work-items/*.yaml` as the board.** That directory is a
dead, all-done export from an earlier tooling generation. It has drifted far out
of date, and a reviewer who reads it concludes the project has no open work. If
you have no MCP access, say so and treat the board question as NOT DONE rather
than substituting the YAML files.
