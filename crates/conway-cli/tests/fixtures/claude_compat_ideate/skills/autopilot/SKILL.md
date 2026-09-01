---
description: "Autonomous SDLC loop that runs execute → review → refine over the work board until the project converges — zero open claimable work and no unresolved critical/significant findings — or a cycle/appetite limit is hit. Unattended: Andon escalations go to a proxy-human agent instead of stopping for the user."
user-invocable: true
argument-hint: "[project directory path] [--max-cycles N]"
---

# ideate:autopilot

`autopilot` runs the whole build/review/refine cycle unattended until the
project converges. It is **self-contained**: it does **not** call
`/ideate:execute`, `/ideate:review`, or `/ideate:refine`. Instead it loads its
phase logic from `phases/*.md` in this skill directory and runs it inline, so
the loop keeps one continuous context. When work hits an Andon escalation there
is no user to stop for — it routes the decision to the `ideate:proxy-human`
agent and records the outcome.

## Addressing the human
Any prose this surface writes for a person — a summary, a report, a question,
ordinary conversation, not only a confirm gate — follows the shared rule in
`skills/shared/human-presentation.md` (relative to the plugin root). Read it
and apply it to everything a person will read; it is the single copy — do not
paraphrase or restate it here.

## State (v3 has no autopilot-state store)
There is no `manage_autopilot_state` tool in v3. Persist loop state as process
records and reconstruct it on resume:
- At each cycle boundary write `record_append(kind="autopilot-cycle", ...)`
  with: cycle number, items completed this cycle, findings by severity,
  convergence verdict, and the head commit (`git rev-parse HEAD`).
- On (re)start, `record_read(scope="autopilot")` to find the last cycle
  record — records come newest-first, so it is on the first page, and its `id`
  with `include_content: true` gets that one record's body; the board
  (`work_list`, paged out via `next_cursor`) and `git log` are the ground truth
  for what's done.
  Resume from `last_cycle + 1`; a fresh project starts at cycle 1.

Resolve `actor_human` once: `git config user.name` (fallback `$USER`).

## Controller
1. **Config & args.** Parse `--max-cycles` (default 20). Target dir = arg or
   cwd.
2. **Validate.** `work_list` must show a populated board; if empty, stop and
   tell the user to run `/ideate:refine` to populate the board (or
   `/ideate:init` first if the project isn't set up yet).
3. **Load intent.** `steering_read` for principles/policies — paged to
   exhaustion (follow `next_cursor` until it is `null`; a short page is not the
   end), since the whole ruleset governs the run — and `record_read` for the
   stated appetite and success criteria, whose rows carry the `claim` only, so
   re-read the record that states them by `id` with `include_content: true`.
   (Autopilot honours these and passes them to proxy-human on escalations.)
   Appetite default: 10 cycles of effort unless recorded otherwise.
4. **Resume check.** Reconstruct state (above); decide resume vs. fresh and
   confirm the starting cycle with the user before the loop begins.
5. **Main loop** — each cycle:
   - a. **Execute** — read and run `phases/execute.md`.
   - b. **Review** — read and run `phases/review.md`. It returns a convergence
        verdict: `converged` / `needs-refinement` / `unknown`.
   - c. **Converged?** If `converged` → exit the loop to reporting (project
        complete; that verdict already carries the exhaustive board test —
        don't re-derive it from a single `work_list` page). If `unknown`
        (review couldn't decide) → route to proxy-human as an Andon; act on its
        decision.
   - d. **Refine** — if not converged, read and run `phases/refine.md` to turn
        findings into new board work.
   - e. **Limits.** Write the cycle record (state, above). Stop if
        `cycle >= max-cycles` or the appetite is spent, or if `refine.md`
        reports the pending-work count is **not decreasing** across cycles
        (divergence guard — the loop is spinning, not converging).
6. **Report** — read and run `phases/reporting.md` for the final activity
   report.

## Andon routing (unattended)
Anywhere a phase would stop for the user, autopilot instead spawns
`ideate:proxy-human` with the escalation + the loaded intent (principles,
policies, appetite, success criteria). It returns a decision; autopilot records
it with `record_append(kind="andon")` and continues. Decisions flagged
`human: true` by proxy-human are collected for the final report — those are the
calls a human should still review.

## Guardrails
- Self-contained: load phase logic from `phases/*.md`; never invoke the sibling
  skills (that would fork the context and defeat the loop).
- Convergence is **derived**, not asserted: no open claimable work **and** no
  unresolved critical/significant findings. Don't declare victory early.
- The divergence guard is load-bearing — an autopilot that can't converge must
  stop and hand back to a human, not burn cycles forever.
- All board/record/steering writes happen here in the loop; subagents only
  build, review, and advise.
