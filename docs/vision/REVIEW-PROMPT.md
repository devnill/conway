# The recurring architectural review

> A re-runnable prompt. Paste §2 into a fresh agent session at the root of this
> repository whenever you want to know where conway stands against its own
> intent. It is designed to be run repeatedly — quarterly, after a big
> landing, or any time the tree has drifted far enough that you cannot answer
> "how is it going?" from memory.

---

## 1. What this produces

Four artifacts, in this order:

| Artifact | Audience | Lifetime |
| --- | --- | --- |
| [`INTENT.md`](INTENT.md) | the operator | permanent, appended to |
| [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md) | the operator | replaced each run |
| [`PLAN.md`](PLAN.md) | the agents doing the work | replaced each run |
| This page | whoever runs it next | amended when the process changes |

`INTENT.md` is the only one that accumulates. The other two are snapshots and
should be overwritten wholesale, not merged — a stale finding that survives a
rewrite is worse than a missing one.

## 2. The prompt

Copy everything between the rules.

---

You are doing a recurring architectural review of **conway**, a Rust agent
harness. Read these first, in order, before doing anything else:

- `docs/vision/INTENT.md` — what the operator wants and what "good" means. This
  is the authority on intent. Everything else is downstream of it.
- `PHILOSOPHY.md` — the 1.0 specification. Written in the present tense; claims
  that are ahead of the tree carry a **"Where the tree is today"** note. Anything
  without such a note is asserted to be true right now, and if it is not, that is
  a finding.
- `ARCHITECTURE.md` — how the pieces actually fit together today.
- `scripts/board-claims.md` — the machine-checked ledger of falsifiable claims
  about the tree. This is the fastest honest read of where spec and code diverge,
  because CI fails when an entry stops being true in either direction.
- `CONTRIBUTING.md` §2 — the rule that keeps the "Where the tree is today" notes
  honest.

**On the `docs/vision/DESIGN-*.md` documents.** These are design records written
BEFORE the work they describe. They state what a mechanism was expected to need,
and those expectations are hypotheses, not constraints the tree must satisfy.
Where a design doc and the shipped code disagree, that is a FINDING to
investigate, not automatically a defect in the code — the doc may be what is
wrong. `DESIGN-context-path.md` §11.7 is a live example: it asserts memory
"needs no storage of its own, no retrieval semantics of its own, and no new
port", and the operator overrode that on 2026-08-18 after the claim was built to
and failed. See that section's own amendment note. Do not recommend reverting
shipped work solely because a design document predicted a different shape.

Then survey the tree yourself. Do not take the documents' word for what is built.
At minimum establish, from the code:

1. **Size and shape.** Production lines per crate, separated from test lines
   (count from the first `#[cfg(test)]` in each file — the raw `wc -l` figure is
   roughly 3× the production figure and will mislead you).
2. **The extension surfaces and whether each is real.** For every port in
   `crates/conway-core/src/ports/`: is there a non-test implementation outside
   the core, and can a third party reach it? A port with no external implementor
   is a surface that has not been proven.
3. **The hook event vocabulary.** Which events have production dispatch sites, not
   just constants.
4. **The plugin tier.** What is in `crates/conway-plugin-*`, what
   `PHILOSOPHY.md` §5 promises, and the difference.
5. **The CLI surface.** Every flag in `crates/conway-cli/src/cli.rs`, every slash
   command, every subcommand — and what a general-purpose (non-coding) user of
   the harness would find missing.
6. **The board.** The live board is the **ideate MCP server** (`work_list`,
   `work_get`) — what is open, what is done, and whether the open set matches
   the findings you are about to write.

   **Do NOT read `.ideate/work-items/*.yaml` as the board.** That directory is a
   dead, all-done export from an earlier tooling generation. It has drifted far
   out of date, and a reviewer who reads it concludes the project has no open
   work. If you have no MCP access in this session, say so and treat the board
   survey as NOT DONE rather than substituting the YAML files.

### What to produce

**A. `docs/vision/STATE-OF-THE-UNION.md`** — an architectural review written for
the operator, who is involved in high-level design and *not* familiar with
individual design decisions. Constraints on this document, in priority order:

- It must be legible to a layman. Decisions have to be makeable from **blocks in a
  block diagram**, not from interface signatures. If a reader needs to understand
  a trait to follow a paragraph, rewrite the paragraph.
- Every claim about the tree must be one you verified from the code in this run.
  Cite `path:line` for anything a reader might want to check.
- Say what is *good* as plainly as what is broken. A review that only lists
  problems is not a state of the union.
- Score each area against `INTENT.md`, not against general software-quality
  intuitions.
- No weeds. If something needs the weeds, it belongs in a board item.

**B. `docs/vision/PLAN.md`** — the plan of attack. Constraints:

- Organized into **domains** that can be worked in parallel by different agents
  without conflicting. Each domain names the files and crates it owns.
- Shared files (`PHILOSOPHY.md`, `Cargo.toml`, `ARCHITECTURE.md`,
  `crates/conway-core/src/ports/*`) are the collision risk. Name every one, name
  its single owner for this round, and give the serialization order for anything
  that must touch it second.
- Cover both kinds of work: **adherence** (the tree does not match the spec) and
  **quality** (the tree matches the spec and the spec or the code is not good
  enough).
- Each work item states: what done looks like, which domain owns it, what it
  depends on, and roughly how big it is.
- Fan-out is variable by design — some rounds want three agents and some want
  twenty. Express dependencies so the fan-out can be chosen at dispatch time
  rather than baked into the plan.

**C. Amendments to `docs/vision/INTENT.md`.** If the review surfaced a question
the intent document does not answer, that is a **failure of the spec, not a gap in
the code** — the operator's first rule. Draft the missing sentiment as a proposed
addition and flag it for the operator rather than deciding it yourself.

### How to behave

- Verify before asserting. Both directions matter: a document that *understates*
  what is built is the same defect as one that overstates it, and this repository
  has been bitten by the understating kind.
- Do not fix anything during the review. Findings become board items.
- Where the intent is genuinely ambiguous, ask the operator rather than picking.
  Every question you have to ask is itself a finding about `INTENT.md`.

### Four failure modes this review exists to catch

Each of these produced real, shipped defects in this repository. They are listed
as things to LOOK FOR in the tree, and as things to avoid while reviewing.

**1. A premise defended by a series of workarounds.** When a feature has
accumulated two or more accommodations — a limitation filed as an "ergonomics
follow-up", a missing operation left as an "open question", a cap standing in for
a bound — read them TOGETHER rather than one at a time. Each accommodation is
usually locally reasonable, which is what makes the series invisible; the series
is the signal that the premise underneath is failing. Watch especially for
reassuring phrasing applied to a workaround: "bounded by construction" was
written in this tree to describe a cap that existed only because the unit of work
was wrong. A symptom phrased as a virtue is the hardest kind to see.

**2. A capability verified in one direction only.** A complete READ path is not
evidence of a WRITE path. This tree shipped a memory feature selecting sessions by
a label that nothing could ever set: the filter existed, the query existed, both
were reachable — and no code path wrote the field. When a design rests on a piece
of data, verify both ends before believing it works, and prefer an end-to-end
proof through the real surface over two half-proofs.

**3. A design document treated as a constraint.** Covered in the reading list
above. Stated here as conduct: when the tree and a `DESIGN-*.md` disagree,
investigate which is wrong. Do not assume the code.

**4. A limitation reported after the success.** If something works end to end but
is not usable — a feature whose enabling path is unwired, a store that forgets on
restart — say the limitation FIRST. Both occurred here, and in both cases the
accurate-but-late framing left a reader believing something was finished when it
was not. A review that buries the caveat has misled its reader even if every
sentence is true.

---

## 3. After the run

1. Read `STATE-OF-THE-UNION.md`. It is written for you, and if it is not legible
   that is a defect in this prompt — amend §2 before amending the output.
2. Approve or amend the proposed `INTENT.md` additions.
3. File `PLAN.md`'s work items onto the board and dispatch.

## 4. Change log for this process

| Date | Change |
| --- | --- |
| 2026-08-14 | First version. Established the four-artifact shape and the rule that `INTENT.md` accumulates while the review and plan are replaced. |
| 2026-08-18 | Added "Four failure modes this review exists to catch" to §2's conduct section — premise-defended-by-workarounds, one-directional verification, design-doc-as-constraint, limitation-reported-late. All four produced shipped defects in this tree during the 2026-08-17/18 memory program; they are recorded so the review looks for them rather than rediscovering them. |
| 2026-08-18 | Two re-run hazards fixed. (1) The board survey pointed at `.ideate/work-items/*.yaml`, a dead all-done export — a reviewer following it would have concluded there was no open work, while the live MCP board carried 12 open items. Now points at the MCP server and says explicitly not to substitute the YAML. (2) Added the note above on `DESIGN-*.md` documents being hypotheses rather than constraints, after a design claim (`DESIGN-context-path.md` §11.7) was built to as a requirement, failed, and was overridden by the operator — a re-run reading it as authority could have recommended reverting the correction. |
