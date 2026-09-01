---
description: "Decompose an idea into actionable work. The first working step after init and the recurring planning engine: takes a described goal, change, or set of review findings, analyzes it against the existing code and steering, and produces board work items with dependency and containment edges. Plans work; execute builds it."
user-invocable: true
argument-hint: "[description of the idea or change to decompose]"
---

# ideate:refine

`refine` is where big ideas become actionable work. It's the **first step after
`init`** and the engine you return to whenever there's something new to plan.
Give it an idea, a change, or a pile of review findings; it analyzes them
against what already exists and produces **board work items** — decomposed,
sequenced, self-contained — for `execute` to build. It plans; it does not write
code.

## Addressing the human
Any prose this surface writes for a person — a summary, a report, a question,
ordinary conversation, not only a confirm gate — follows the shared rule in
`skills/shared/human-presentation.md` (relative to the plugin root). Read it
and apply it to everything a person will read; it is the single copy — do not
paraphrase or restate it here.

## Tool vocabulary (v3)
- Read: `work_list`, `work_get`, `work_events`, `record_read`, `steering_read`.
- Write: `work_create` (title, spec, spec_format, `depends_on[]`, `parent_id`,
  `actor_human`), `work_update_meta` (edit an open item via optimistic CAS on
  `version`), `steering_put` (amend a rule; prior version kept in history),
  `record_decision`, `record_append`.
- Subagents: `ideate:architect` (analyze/design), `ideate:researcher`,
  `ideate:decomposer` — all read-only; **you** perform every write.
- `actor_human`: `git config user.name` (fallback `$USER`), resolved once.

## Step 1 — Confirm the project is set up
Call `steering_read`. If it returns `{code:"GATED"}` or is empty and there's no
`project-setup` record, the project isn't set up — direct the user to
`/ideate:init` first (it enables steering and captures intent, which refine
plans against).

## Step 2 — Understand the input
`refine` decomposes whatever it's handed. Identify which case you're in — it
shapes the output:
- **A fresh idea / new goal** (the primary case) — the argument describes work
  the user wants. Decompose it into new board items.
- **Requirement evolution** — a change to existing behaviour. Plan it against
  the current architecture and board.
- **Post-review correction** — the input is `review`'s findings
  (`record_read(scope="finding")`, paged to exhaustion — see Step 3). Each
  unresolved significant/critical finding becomes (or updates) a board item.
- **Alignment recalibration** — the plan and the code have drifted. This case
  may legitimately produce **zero new work items** — the honest output can be
  amended steering and recorded decisions that realign intent, nothing more.
  Don't invent busywork.

## Step 3 — Load context
Assemble what the decomposition must respect. Every read below is paged —
follow `next_cursor` until it is `null`; a page shorter than `limit` is **not**
the end:
- **Steering** (`steering_read`) — the principles, constraints, and policies
  the new work must honour. One page is not the ruleset; page it out, or filter
  by `domain` if the idea is confined to one.
- **Board** (`work_list`, plus `work_events`/`work_get` where history or an
  item's `spec` matters, since list rows carry `spec_length`, not the body) —
  existing open/in-progress/done items, so new work wires to them and doesn't
  duplicate them.
- **Record** (`record_read`) — the `init` architecture survey and prior
  decisions; findings if this is a correction. Rows carry the `claim` and
  `content_length`, not the prose — fetch a specific record by `id` with
  `include_content: true` when the body matters.

## Step 4 — Clarify (short interview)
Targeted, not a full interview — you have context. Pin down only what you need
to decompose well: the precise intent, scope boundaries, acceptance criteria,
and any new constraints. Record it with `record_append(kind="interview")` if it
produced anything load-bearing.

## Step 5 — Analyze
- Spawn `ideate:architect`: **analyze mode** to surface blast radius (other
  consumers of a changed interface, patterns to follow, implied migrations);
  **design mode** when the idea needs a structural shape before it can be cut
  into items. Record key design choices with `record_decision`; when a choice
  overturns an earlier decision, pass that decision's record id as `supersedes`
  so the old decision surfaces the replacement on read instead of misleading a
  later reader.
- Spawn `ideate:researcher` for genuine unknowns the idea introduces. Skip if
  there are none.

## Step 6 — Decompose into board work (the core output)
Turn the analyzed idea into items:
- Spawn `ideate:decomposer` with the goal + architect's design + steering. It
  returns item JSON: `ref` handles, self-contained `spec`s, `depends_on` refs,
  and `parent` refs. (For a trivially small idea you may decompose inline
  instead of spawning it.)
- For work that needs a **human** — an approval, an outward-facing action, a
  per-project judgment call, a decision — author the item with
  `spec_format: "ideate/human-gate"` so `execute`/`autopilot` **surface** it
  instead of dispatching a worker. A code item can `depends_on` a human-gate
  to hold the frontier until the human acts (see docs/workflow-guide.md
  "Human-effort items").
- Create the board, mapping `ref` handles → real ids:
  1. Create parent/container items first (a phase or feature grouping),
     capturing each returned id.
  2. Create children in dependency order (`work_create`), resolving `ref`s to
     real ids for `depends_on` and `parent_id`. Every `spec` must stand alone —
     a worker builds from it with no other context. Every `spec` MUST open
     with a short plain-language block — two to three sentences, no file
     paths, no policy ids, no ULIDs, no ideate jargon — stating what this is
     and why it matters, before any dense section begins; write it so someone
     new to the codebase can follow it. It is a handle, not a summary: it
     does not need to cover the spec, only make a reader want to continue or
     decide they don't need to — Step 7's presentation lifts it verbatim, so
     word it as something a human should actually read, not spec-writer's
     shorthand. Every `spec` MUST end with a concrete `ACCEPTANCE` section
     (testable conditions, no "handle edge cases") and a `VERIFICATION
     ANCHOR` (the exact runnable command/path that proves the work) —
     verify-before-done lives in the requirements, not the board.
- For a change to **existing open** items, use `work_update_meta` (read the
  current `version`; pass it as `expected_version`; on `VERSION_CONFLICT`
  re-read and retry). Never edit `done` items — supersede them with new work.
- Amend steering (`steering_put`, reusing ids) where the idea changes a rule.
- Verify with `work_list`, paged to exhaustion as in Step 3 (claimability is
  per item, so one page can hide the claimable frontier): there should be at
  least one `claimable` item and no orphaned refs.

## Step 7 — Close out
Before recommending `/ideate:execute`, present what was decomposed the way a
human can actually judge it — not the `spec` bodies, which are written
densely for a worker to build from with no other context. **Never present
spec bodies for review.** Tell each new or updated item's story, one item at
a time, in the same shape as `execute`'s confirmation gate:
- **Background you need** — what a newcomer to this codebase would need to
  follow the rest.
- **The problem** — what's wrong or missing, and why it matters now.
- **The work** — what will be built, in plain language.
- **How we know it's done** — the acceptance bar, in plain terms.

**Expand references at the boundary** the same way: inline what a policy id
or record ULID means rather than citing it bare, keeping the id in
parentheses at most, never as the label itself. **Default to progressive
disclosure** — title → one-line summary → the full four-part story → the raw
spec — letting the user pull depth instead of receiving it all at once; this
governs any confirm-question option text this step (or Step 4's clarifying
interview) poses too, so enumerate real options in plain language rather than
leaving them implied or arguing from a set the user was never shown.

- `record_append(kind="journal")` — what was decomposed and why; the items
  created/updated; any steering amended.
- `record_append(kind="plan-complete")` — the planning milestone.
- Summarize for the user, item by item per the story shape above: the case,
  the items created (count + the claimable frontier), and the next step —
  **`/ideate:execute`** to build the new work (or, for a recalibration with
  no new items, the realignment you recorded).

## Guardrails
- You are the only writer; subagents return analysis and item JSON.
- Every `spec` is self-contained — the decomposition's whole point is that
  `execute` and its workers need nothing beyond the board item.
- Append-only discipline: corrections supersede, `done` items are never edited,
  and a recalibration that yields zero items is a valid, honest outcome —
  report it plainly.
