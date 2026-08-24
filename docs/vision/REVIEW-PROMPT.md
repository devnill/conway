# The recurring architectural review

> A re-runnable prompt. Paste §2 into a fresh agent session at the root of this
> repository whenever you want to know where conway stands against its own
> intent. It is designed to be run repeatedly — quarterly, after a big landing,
> or any time the tree has drifted far enough that you cannot answer "how is it
> going?" from memory.
>
> **§2 is short on purpose.** The agent you paste it into is an *orchestrator*:
> it measures the tree, dispatches reviewers, and synthesises. The detail each
> reviewer needs lives in [`review/`](review/) and is loaded by that reviewer
> only. Do not paste the lens files; do not summarise them into §2.

---

## 1. What this produces

| Artifact | Audience | Lifetime |
| --- | --- | --- |
| [`INTENT.md`](INTENT.md) | the operator | permanent, appended to |
| [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md) | the operator | replaced each run |
| [`PLAN.md`](PLAN.md) | the agents doing the work | replaced each run |
| This page and [`review/`](review/) | whoever runs it next | amended when the process changes |

`INTENT.md` is the only one that accumulates. The other two are snapshots and
should be overwritten wholesale, not merged — a stale finding that survives a
rewrite is worse than a missing one.

### The review files

| File | Loaded by |
| --- | --- |
| [`review/CONDUCT.md`](review/CONDUCT.md) | every reviewer, first |
| [`review/lens-adherence.md`](review/lens-adherence.md) | one reviewer — is the written record true, both directions |
| [`review/lens-surfaces.md`](review/lens-surfaces.md) | one reviewer — ports, hooks, plugins, embedding |
| [`review/lens-sustainability.md`](review/lens-sustainability.md) | **one reviewer per territory** — cost of change, DRY, consolidation |
| [`review/lens-evidence.md`](review/lens-evidence.md) | one reviewer — failing premises, one-directional proofs |
| [`review/lens-operator.md`](review/lens-operator.md) | one reviewer — the CLI and the daily-driver bar |

---

## 2. The prompt

Copy everything between the rules.

---

You are the **lead** on a recurring architectural review of **conway**, a Rust
agent harness, at the root of this repository. You will not review the tree
yourself. You will measure it, dispatch reviewers, and synthesise what they
return.

### Step 1 — measure, once, for everyone

Run these and keep the output. Every reviewer gets this table in their brief so
that none of them re-derives it.

```sh
# production vs test split, per crate
for c in crates/*/; do n=$(basename $c);
  src=$(find $c/src -name '*.rs' 2>/dev/null | xargs cat 2>/dev/null | wc -l);
  tst=$(find $c/tests -name '*.rs' 2>/dev/null | xargs cat 2>/dev/null | wc -l);
  printf "%-28s src=%-8s tests=%-8s\n" "$n" "$src" "$tst"; done

# the twenty largest files
find crates -name '*.rs' | xargs wc -l | sort -rn | sed -n '2,21p'

# the extension surfaces
ls crates/conway-core/src/ports/

# gates
git status --short && git log --oneline -10
```

The `src=` figures still contain inline `#[cfg(test)]` modules. Say so when you
pass the table on — the true production figure is lower, and a reviewer who
treats these as production lines will overstate the tree.

Then read [`docs/vision/review/CONDUCT.md`](review/CONDUCT.md) yourself. You are
bound by it too, and §6 tells you where the board is.

### Step 2 — choose the fan-out

| Round | Reviewers | Which |
| --- | --- | --- |
| **Minimum** — after a small landing | 3 | adherence, evidence, sustainability ×1 (whole tree) |
| **Normal** — the default | 6 | adherence, surfaces, operator, evidence, sustainability ×2 |
| **Full audit** — quarterly, or when the tree feels slow | 9 | the four singles, plus sustainability ×5, one per territory |

**Never more than twelve.** Past that the reviewers stop finding new things and
start finding each other's things, and you pay for the merge.

The sustainability territories, when you split them. Each reviewer gets exactly
one and is told not to read outside it:

| Territory | Owns |
| --- | --- |
| **core** | `conway-core`, `conway-session` |
| **runtime** | `conway-runtime`, `conway` |
| **cli** | `conway-cli` (41k lines of `src` — never merge this with another territory) |
| **plugins** | `crates/conway-plugin-*`, `conway-thirdparty-backend` |
| **harness** | `conway-testkit`, `conway-tools`, and all `tests/` directories workspace-wide |

At 6 reviewers, use **core+runtime** and **cli+plugins+harness**.

### Step 3 — dispatch

Send them **all at once, in parallel**. Each brief contains, and contains only:

1. **The objective**, in one sentence.
2. **The territory**, named explicitly, with the instruction not to read outside it.
3. **The two files to read**: `docs/vision/review/CONDUCT.md`, then their one lens.
4. **The measurement table** from Step 1.
5. **The budget**: the tool-call range and word limit their lens states.
6. **The output format**: `CONDUCT.md` §4.

Do not add guidance of your own. If a reviewer needs something the lens does not
say, that is a defect in the lens — note it for §4 rather than patching it in the
brief, where it will be lost.

### Step 4 — the board, while they run

The live board is the **ideate MCP server** (`work_list`, `work_get`). Establish
what is open, what is done, and whether the open set matches what is coming
back. Do this yourself while the reviewers work; it needs MCP and it needs the
whole picture.

**Do NOT read `.ideate/work-items/*.yaml` as the board** — see `CONDUCT.md` §6.

### Step 5 — synthesise

Merge the returns. Three rules:

- **A finding two reviewers found is one finding**, and the fact that two lenses
  hit it is evidence of weight, not two items.
- **A finding you cannot cite is not a finding.** If a reviewer asserted
  something without `path:line`, verify it or drop it. Do not pass it through.
- **Collect every reviewer's `Not checked` section.** Its union is this review's
  coverage gap, and it goes in `STATE-OF-THE-UNION.md` explicitly. A review that
  silently bounds its own coverage reads as complete when it is not.

Then produce three things.

**A. `docs/vision/STATE-OF-THE-UNION.md`** — an architectural review for the
operator, who is involved in high-level design and *not* familiar with individual
decisions. In priority order:

- Legible to a layman. Decisions have to be makeable from **blocks in a block
  diagram**, not from interface signatures. If a reader needs to understand a
  trait to follow a paragraph, rewrite the paragraph.
- Every claim verified in this run, with `path:line` for anything checkable.
- Say what is **good** as plainly as what is broken. A review that only lists
  problems is not a state of the union.
- Score against `INTENT.md`, not against general software-quality intuitions —
  with the one admitted exception `lens-sustainability.md` §4 names, and it
  carries its own reconciliation rule.
- Carry a **sustainability section**. Where is this tree getting more expensive
  to change, and what would make it cheaper? This is the operator's standing
  question and it gets its own heading whether or not the round found much.
- No weeds. If something needs the weeds, it belongs in a board item.

**B. `docs/vision/PLAN.md`** — the plan of attack:

- Organised into **domains that can be worked in parallel** without conflicting.
  Each domain names the files and crates it owns.
- Shared files (`PHILOSOPHY.md`, `Cargo.toml`, `ARCHITECTURE.md`,
  `crates/conway-core/src/ports/*`, `crates/conway-runtime/src/agent_loop.rs`)
  are the collision risk. Name every one, name its single owner for this round,
  and give the serialisation order for anything that must touch it second.
- Cover **adherence** (the tree does not match the spec), **quality** (it
  matches and is not good enough), and **sustainability** (it is right and
  costly to change).
- Each item states: what done looks like, which domain owns it, what it depends
  on, and roughly how big.
- Fan-out is variable by design. Express dependencies so it can be chosen at
  dispatch time rather than baked in.

**C. Amendments to `docs/vision/INTENT.md`.** Every reviewer's **Intent gaps**
section lands here. A question the intent document does not answer is a
**failure of the spec, not a gap in the code** — `INTENT.md` §8.1, the operator's
first rule. Draft the missing sentiment as a proposed addition and **flag it for
the operator rather than deciding it yourself.**

### How to behave

- **Verify before asserting**, in both directions. A document that understates
  what is built is the same defect as one that overstates it.
- **Do not fix anything.** Findings become board items.
- **Do not review in parallel with your reviewers.** Your job is the merge, and
  a lead who also reviews produces the duplicate the fan-out was meant to avoid.
- Where the intent is genuinely ambiguous, ask the operator rather than picking.

---

## 3. After the run

1. Read `STATE-OF-THE-UNION.md`. It is written for you, and if it is not legible
   that is a defect in this prompt — amend §2 or the lens before amending the
   output.
2. Approve or amend the proposed `INTENT.md` additions.
3. File `PLAN.md`'s work items onto the board and dispatch.
4. If a reviewer needed guidance its lens did not give, add it to that lens now.
   The lenses are the part of this process that is supposed to accumulate.

---

## 4. How this review spends its time

The review is deliberately fast, and the discipline that makes it fast is not
specific to reviewing. It is written down here because the same four rules apply
to any large analysis this project runs.

**One measurement, distributed — not N re-derivations.** The lead measures the
tree once and passes the table down. Before this, every reviewer counted lines
itself, and most of the review's wall-clock was the same `find | wc -l` run six
times. Whatever the shared substrate of an analysis is, establish it once and
hand it over.

**Non-overlapping territories, stated explicitly.** Vague scoping is the
documented cause of parallel agents duplicating each other's work. Every brief
names its territory and forbids reading outside it. The cost of a gap between two
territories is one missed finding; the cost of an overlap is two agents' full
budget plus a merge.

**A budget and a distillate, not a dump.** Each reviewer has a tool-call range
and a word limit and returns a fixed structure. An agent that explores widely and
returns 1,000 words has done its job; one that returns its transcript has moved
the cost to the lead instead of paying it.

**Stop at the point where findings stop changing the plan.** The instruction is
not "find everything" — it is to stop when the marginal finding no longer changes
what gets built next, and to say what was left. Analysis expands to fill the
budget it is given, so the budget has to be given.

**The corollary for the work itself.** Three of these four are properties of the
*code*, not just the review. A shared substrate established once, boundaries that
let work proceed without coordination, and an interface that returns a distillate
rather than its internals are the same three things
[`review/lens-sustainability.md`](review/lens-sustainability.md) hunts for. If
this project's analysis is slow for these reasons, it is worth asking whether the
tree is slow to change for them too.

---

## 5. Change log for this process

| Date | Change |
| --- | --- |
| 2026-08-14 | First version. Established the four-artifact shape and the rule that `INTENT.md` accumulates while the review and plan are replaced. |
| 2026-08-18 | Added "Four failure modes this review exists to catch" — premise-defended-by-workarounds, one-directional verification, design-doc-as-constraint, limitation-reported-late. All four produced shipped defects during the 2026-08-17/18 memory program. |
| 2026-08-18 | Two re-run hazards fixed. (1) The board survey pointed at `.ideate/work-items/*.yaml`, a dead all-done export — a reviewer following it would have concluded there was no open work while the live MCP board carried 12 open items. (2) Added the note on `DESIGN-*.md` documents being hypotheses rather than constraints, after a design claim was built to as a requirement, failed, and was overridden. |
| 2026-08-24 | **Restructured from a single 182-line prompt into a lead brief plus five lenses in [`review/`](review/).** Three changes, one motivation each. **(1) Progressive disclosure.** §2 is now an orchestrator brief; the detail lives in lens files loaded by the reviewer that needs them, so no agent carries the other four lenses' instructions. **(2) Parallel reviewers with named territories.** The review was one agent reading 224k lines in sequence; it is now 3–9 with non-overlapping scopes, a shared measurement done once by the lead, and a fixed return format. The single-agent version's cost was mostly re-derivation. **(3) A sustainability lens.** The operator's standing concern — code that works, is as designed, and is becoming expensive to change — had no reviewer. `lens-sustainability.md` adds DRY-in-the-knowledge-sense, orthogonality measured from commit history, ETC, and consolidation, with the guardrails that keep a duplication hunt from producing net-negative refactors. |
| 2026-08-24 | **`INTENT.md` gains §8.10 — the cost of changing something is part of whether it is good.** §8 had nine points on what "good" means and none was about duplication, consolidation, or cost of change; §8.6 came closest and is scoped to invariants at seams. The sustainability lens had no section to score against, which by §8.1 made it a defect in the spec rather than a gap in the code. §8.10 states the test (*when this decision changes, how many places must change with it, and would forgetting one be a bug?*) and carries the three guardrails that keep a duplication hunt from producing net-negative refactors — repetition that protects §8.2's agnostic core is correct, an abstraction with a hypothetical second consumer is indirection under §8.5, and a consolidation must name a change that becomes easy. The citation range in §8's header moved to §8.1–§8.10. |
