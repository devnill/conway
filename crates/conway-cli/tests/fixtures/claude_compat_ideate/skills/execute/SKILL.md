---
description: "Execute the work board produced by init/refine. Claims claimable items under lease-fenced discipline, delegates each to a worker subagent, runs incremental review, handles findings by severity, and flags unresolvable issues to the user via an Andon stop. Builds work; does not design or re-plan."
user-invocable: true
argument-hint: "[project directory path]"
---

# ideate:execute

`execute` turns the board into working code. It walks the claimable frontier,
claims each item with a fencing lease, hands it to a `ideate:worker`, reviews
the result, records findings, and completes or releases the claim. It never
designs or re-plans — if work reveals the plan is wrong, it stops (Andon) and
sends you to `refine`.

## Addressing the human
Any prose this surface writes for a person — a summary, a report, a question,
ordinary conversation, not only a confirm gate — follows the shared rule in
`skills/shared/human-presentation.md` (relative to the plugin root). Read it
and apply it to everything a person will read; it is the single copy — do not
paraphrase or restate it here.

## Board claim discipline (the core loop invariant)
Every board write is fenced by a **claim token**. For each item:
1. `work_claim(id, actor_human, [lease_ms])` — succeeds only if the item is
   `open` and **every** `depends_on` item is `done`. It returns a
   `claim_token`. Default lease is 4h; pass a longer `lease_ms` for big items
   (max 30 days).
2. While the worker runs, if it approaches the lease, `work_renew(id,
   claim_token, lease_ms)` to extend. A lapsed lease is reclaimed by the board
   (the item flips back to `open`) — a subsequent complete with the stale token
   fails `INVALID_CLAIM`, which is the fence doing its job.
3. On success: `work_complete(id, claim_token, note)` → `done`.
4. On failure/blocked: `work_release(id, claim_token, note)` → back to `open`
   with a handoff note. Never leave an item claimed after you stop.

Resolve `actor_human` once from `git config user.name` (fallback `$USER`).

## Human-effort items — `spec_format: "ideate/human-gate"`
Not every board item is code a worker can build. An item whose `spec_format`
is `ideate/human-gate` marks work that needs a HUMAN — an approval, an
outward-facing action (a push, a PR, a publish), a per-project judgment call,
or a decision. It is claimed and completed like any item (the board fences and
actor-attributes it), but you do **not** spawn a worker for it.

When a claimed item is a human-gate:
1. **Surface it, don't dispatch.** Present the item to the user — the human
   action it needs and why — instead of spawning `ideate:worker`.
2. **The human acts.** Either pause for the user to complete it out-of-band
   (then `work_complete(id, token, note)` with the human as actor), or — if it
   can't be done now — `work_release(id, token, note)` and continue to the
   next claimable item.
3. **It blocks dependents by contract.** A code item may `depends_on` a
   human-gate item; the derived `claimable:false` keeps the downstream
   frontier blocked until the human completes the gate (GP-27 — the data
   contract, not an orchestrator, holds it).

## Step 1 — Locate and read the plan
- Target directory: argument or cwd. Confirm a board exists (`work_list`); if
  it's empty, direct the user to `/ideate:refine` to decompose work onto the
  board (or `/ideate:init` first if the project isn't set up yet).
- Read the board: `work_list` for the full picture and the derived `claimable`
  frontier — page it to exhaustion (follow `next_cursor` until it is `null`; a
  page shorter than `limit` is **not** the end, so never read completion off a
  short page). Rows are summaries with no `spec` body (just `spec_length`) —
  fetch the body per item with `work_get` when you need it. Note `in_progress`
  items — an interrupted prior run may have left claims; check `work_events`
  and either resume or release stale ones.
- Read steering (`steering_read`, paged to exhaustion the same way — the whole
  ruleset governs the work, and one page is not it) and recent decisions
  (`record_read`, whose rows carry the `claim` and `content_length`, not the
  prose) — this is the context workers need to match the project's rules.

## Step 2 — Present the execution plan and confirm
The board's `spec`s are written for a worker — dense with file paths, policy
ids and record ULIDs, because that is exactly what a worker needs to build
with no other context. That density is exactly wrong for a human reviewer,
who is reading a different document for a different reason. **Never present
spec bodies for review.**

Tell each item's story instead, one item at a time, in the shape that has
demonstrably worked:
- **Background you need** — what a newcomer to this codebase would need to
  follow the rest.
- **The problem** — what's wrong or missing, and why it matters now.
- **The work** — what will be built, in plain language.
- **How we know it's done** — the acceptance bar, in plain terms.

**Expand references at the boundary.** A bare policy id or record ULID means
nothing to someone reading cold — inline what it means ("GP-03, the rule that
demo machinery can't ship in the real binary") and keep the bare id in
parentheses at most, never as the label itself.

**Progressive disclosure, not a wall of text.** Default to title → one-line
summary → the full four-part story → the raw spec, in that order, and let the
user pull more depth rather than handing them all of it up front. This same
rule governs confirm-question option text — including the execution-mode
choice in Step 3: enumerate the actual options in plain language rather than
implying them or arguing from a set the user was never shown; a dense or
unstated option list produces answers that talk past it, not through it.

Show the user, item by item: the claimable frontier, the dependency order,
item count, and the execution mode you'll use (below). **Frame this as
review, not approval** — objections and redesigns are cheaper now, before a
worker builds anything, than after. Invite them explicitly and give the user
room to reshape an item before it's claimed. Get confirmation before
building.

## Step 3 — Choose an execution mode
- **Sequential** (default, safest) — one item at a time. Use when items share
  files or the graph is mostly linear.
- **Batched-parallel** — claim several mutually-independent claimable items at
  once, each worker in its own `git worktree` to avoid collisions, then merge
  (`git merge --no-ff`) and resolve conflicts before completing. Use when the
  frontier has genuinely independent items.
- **Teams** (only if `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`) — full parallel
  worker teams. Otherwise fall back to batched-parallel.

## Step 4 — Execute each item
For each claimed item:
1. **Human-gate?** If the item's `spec_format` is `ideate/human-gate`, do NOT
   spawn a worker — surface it per the Human-effort section above (present it;
   the human completes it and you `work_complete` with the human as actor, or
   you `work_release` and continue). Skip the worker/review steps for it.
2. **Assemble the worker's context.** The item's `spec` (returned by
   `work_claim`, or `work_get`) is authoritative; add the applicable steering
   rules and any decisions scoped to this area (`record_read`), pulling the
   body of each decision you pass with `record_read(id, include_content: true)`
   — one id at a time, not a bulk read. Pass all of it in the worker prompt —
   the worker has no board/record access of its own.
3. **Spawn `ideate:worker`** with that context. It implements, verifies (build
   + tests), and returns a completion report (`complete` or `blocked`, what
   changed, verification output, follow-ups).
4. **Incremental review.** Spawn `ideate:code-reviewer` on the item's change.
   For spec-sensitive items, also `ideate:spec-reviewer`. Collect findings.
5. **Handle findings by severity:**
   - `minor` → fix inline (or spawn a quick worker), no ceremony; note in the
     journal.
   - `significant` → fix this cycle if cheap; otherwise `record_append(
     kind="finding")` and `work_create` a follow-up item so it isn't lost.
   - `critical` → **Andon**: stop the loop. Record it
     (`record_append(kind="finding")`), release the current claim with a note,
     and present it to the user (batched with any others) for a decision. Do
     not silently push past a critical finding.
   - Two special cases inherited from v2, treat as `critical`: a **startup/
     smoke-test failure** (the thing doesn't run) and **test-infrastructure
     failure** (can't tell if it works) — both block completion.
6. **Complete or release.** Complete only on evidence: the worker's report must
   include the spec's `VERIFICATION ANCHOR` command and its fresh output (run
   this cycle — "if you haven't run the command, you can't claim it passes"),
   and your `work_complete` note must reference that evidence (it lands as a
   durable record entry). If the worker reported `complete` and no unresolved
   critical/significant finding remains, `work_complete(id, token, note)`. If
   blocked, Andon'd, or the verification wasn't actually run, `work_release(id,
   token, note)` — never complete blind.
7. Re-read the frontier (`work_list`, the same full walk) — completing an item
   may unblock dependents. Continue until the frontier is empty or an Andon
   halts you.

## Step 5 — Close out
- Journal the run: `record_append(kind="journal")` — items completed, findings
  and how each was handled, anything deferred.
- `record_append(kind="execution-complete")` marking the milestone.
- Summarize for the user: items done vs. still open, follow-ups created,
  Andon stops (if any), and the next step — `/ideate:review` to review the
  completed work, or `/ideate:refine` if an Andon needs re-planning.

## Guardrails
- Never fake completion: an item reaches `done` only on a verified worker
  report. A false `work_complete` corrupts the board — `release` instead when
  unsure.
- Never leave an item claimed when you stop. Always `complete` or `release`.
- You do the board/record writes; workers and reviewers only build and report.
- Respect the dependency gate — never hand a worker an item whose prerequisites
  aren't `done` (the board enforces this on claim, but don't try to
  circumvent it).
