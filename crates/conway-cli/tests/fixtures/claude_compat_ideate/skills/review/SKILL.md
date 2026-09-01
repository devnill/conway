---
description: "Comprehensive review of completed work. Spawns specialized reviewers in parallel (code, spec, gap), synthesizes and records their findings by severity, and curates steering. Supports cycle review (default), domain review (--domain name), full audit (--full), and ad-hoc scope (natural language)."
user-invocable: true
argument-hint: "[--domain name | --full | \"natural language scope\"]"
---

# ideate:review

`review` does not review the code itself — it **coordinates** review. It spawns
specialized reviewer subagents, synthesizes their findings into the process
record, and has the domain-curator keep steering coherent. It's the capstone of
a work cycle and feeds `refine`.

## Addressing the human
Any prose this surface writes for a person — a summary, a report, a question,
ordinary conversation, not only a confirm gate — follows the shared rule in
`skills/shared/human-presentation.md` (relative to the plugin root). Read it
and apply it to everything a person will read; it is the single copy — do not
paraphrase or restate it here.

## Tool vocabulary (v3)
- Read: `work_list`, `work_get`, `work_events`, `record_read`, `steering_read`.
- Write: `record_append(kind="finding")`, `record_append(kind="cycle-summary")`,
  `record_append(kind="journal")`, `steering_put` (applying curator proposals).
- Subagents: `ideate:code-reviewer`, `ideate:spec-reviewer`,
  `ideate:gap-analyst` (parallel), then `ideate:journal-keeper`, then
  `ideate:domain-curator` — all read-only; you record what they return.

## Step 1 — Parse mode and scope
- **cycle** (default) — review the work completed since the last review.
  - Primary signal: `git diff` against the last cycle-summary's commit — exact,
    and unaffected by anything below.
  - Supplementary signal, for board-only work with no code diff (decisions,
    process items): `work_list(status:"done")`, paged to TRUE exhaustion —
    follow `next_cursor` until it is `null` (a short page is never the end).
    State this plainly: **`work_list` orders newest-CREATED first
    (`created_at DESC`), which is NOT the same fact as "completed
    recently."** An item created long before this cycle but completed within
    it sorts by its old `created_at` and would be silently dropped by an
    early stop on this ordering; paging to exhaustion avoids that, but the
    ordering still cannot mark where "this cycle" begins. There is no cheaper
    completion signal on the item row — `updated_at` is stamped at create
    time and is bumped only by `update_meta`/`cancel`/`reopen`, never by
    `claim`/`renew`/`complete`/`release`, so it does not track completion and
    cannot substitute. The exact completion time is only reachable per item
    (`work_events(id)`'s `complete` transition), and no board-wide query
    exists for it, so treat it as a one-item spot-check for a genuinely
    ambiguous item, not a way to scan the whole board. Bound the
    supplementary set instead by cross-referencing the exhausted `done` list
    against what the most recent `record_read(scope="cycle-summary")` entry
    already reported as reviewed — anything not covered there is in scope.
- **`--full`** — audit the whole project, not just the last cycle.
- **`--domain <name>`** — review one domain/area against its steering.
- **natural-language arg** — ad-hoc: review exactly the scope described.

## Step 2 — Circuit-breaker check (advisory)
Count how many review cycles this area has been through without converging
(`record_read(scope="cycle-summary")`, paged to exhaustion — follow
`next_cursor` until it is `null`, since a short page undercounts). If it exceeds
a sensible threshold (default 5), **surface it as a reassess signal, not a hard
stop** — tell the user the work may be thrashing and ask whether to continue,
re-scope, or pause.
Continuing past the threshold is fine if the work is genuinely converging;
this is a prompt to reflect, not a gate.

## Step 3 — Load context and set review depth
Read the completed work, its board items and specs (`work_get`/`work_events`),
the applicable steering (`steering_read` — by `domain` when the review is
scoped to one, otherwise paged out the same way), and prior findings
(`record_read`, whose rows carry the `claim` but no prose — fetch one in full
by `id` with `include_content: true`).
Set depth proportionally: a small, low-risk change may warrant only the
code-reviewer (confirm that shortcut with the user); a large or risky change
gets the full panel.

## Step 4 — Spawn reviewers in parallel
Spawn the three reviewers **concurrently** (one message, multiple Task calls),
each scoped to the work under review and handed the relevant steering:
- `ideate:code-reviewer` — correctness, security, quality.
- `ideate:spec-reviewer` — does the work meet its spec and obey steering?
- `ideate:gap-analyst` — what's missing (uncovered requirements, unupdated
  callers, absent tests)?
Each returns severity-classified findings and reads board/record evidence via
the plugin CLIs (they have no MCP tools).

## Step 5 — Synthesize and record findings
Collect all findings; dedupe overlaps; verify each is substantiated (drop
speculative ones). Then:
- Record each surviving finding with `record_append(kind="finding")` —
  severity, location, claim, failure scenario. These are what `refine` and
  `execute` consume. When a finding re-reports and replaces one from an earlier
  cycle (same defect, updated understanding), pass the prior finding's id as
  `supersedes` so the stale one links forward instead of lingering as open.
- Write a `record_append(kind="cycle-summary")` — the roll-up: what was
  reviewed, finding counts by severity, and the verdict (converged / needs
  refinement / blocked).

## Step 6 — Curate steering
Spawn `ideate:domain-curator` with the current steering and this cycle's
findings. It returns proposed steering changes (contradictions to resolve,
redundancy to merge, drift to deprecate, new policy a recurring finding
implies). Apply the ones you agree with via `steering_put` (reuse ids to
amend). Skip proposals you can't justify.

## Step 7 — Journal and report
- Spawn `ideate:journal-keeper` for the cycle narrative; append it with
  `record_append(kind="journal")`.
- Present findings to the user, grouped by severity, with the verdict and the
  recommended next step:
  - findings remain → `/ideate:refine` to plan the fixes.
  - clean / converged → the cycle is done; point to the next planned work.

## Guardrails
- You coordinate and record; reviewers only analyze and report.
- The circuit breaker is **advisory** — never halt the user against their
  wishes; surface the signal and let them decide.
- Only record findings you can substantiate. A review that inflates minor nits
  into blockers is as harmful as one that misses real defects.
