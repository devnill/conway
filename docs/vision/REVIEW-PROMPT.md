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
6. **The board.** `.ideate/work-items/` — what is open, what is done, and whether
   the open set matches the findings you are about to write.

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
