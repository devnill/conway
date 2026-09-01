---
description: "Set up an ideate project. Creates the workspace, enables the steering store, captures the project's intent and guiding principles, and — for an existing codebase — surveys the current architecture. It lays the foundation; it does NOT create work items. Decomposing ideas into work is refine's job."
user-invocable: true
disable-model-invocation: true
argument-hint: "[project directory path]"
---

# ideate:init

`init` sets up the ideate project — nothing more. It establishes the
**steering store** (and enables it), captures the project's intent and guiding
principles, and, when there's an existing codebase, records an architecture
survey so later work has a foundation to build on. It deliberately does **not**
decompose anything or create board work items — that is `/ideate:refine`, the
first working step, which runs right after this.

v3 onboarding is lazy: the first record/steering call creates `.ideate.json`
and the stores with defaults. `init`'s value is making that setup meaningful —
turning steering on and seeding it with the project's intent — not scaffolding
a directory tree (there isn't one).

## Addressing the human
Any prose this surface writes for a person — a summary, a report, a question,
ordinary conversation, not only a confirm gate — follows the shared rule in
`skills/shared/human-presentation.md` (relative to the plugin root). Read it
and apply it to everything a person will read; it is the single copy — do not
paraphrase or restate it here.

## Tool vocabulary (v3)
- Steering: `steering_put`, `steering_read` (guiding principles, constraints,
  policies; each a stable `id`, a `kind`, a `domain`, a `statement`). **Gated
  off by default** — see step 3.
- Record: `record_decision`, `record_append` (kinds `interview`,
  `project-setup`, `design`, `journal`, …).
- Subagent: `ideate:architect` (analyze mode) — read-only; you record what it
  returns.
- No board writes here. `init` never calls `work_create`.

## Step 1 — Detect prior setup
Read `.ideate.json` (Read; absent is fine) and call `steering_read`. If steering
already holds principles (or a `project-setup` record exists), this project is
set up — stop and direct the user to `/ideate:refine` to decompose their next
idea into work. Don't re-init over an existing project.

## Step 2 — Resolve directory and actor
- Target directory: the argument, else the current working directory.
- Resolve `actor_human` for any attributed writes: `git config user.name`
  (fallback `$USER`).

## Step 3 — Enable the steering store
Steering ships gated off (GP-23): `steering_put` returns
`{ok:false, code:"GATED"}` until enabled. Read `.ideate.json` (create `{}` if
absent), set `steering.enabled: true`, and write it back with Edit/Write. Tell
the user you enabled steering and why (the project's principles/policies live
there). Confirm with a `steering_read` — a non-GATED response means it took.

## Step 4 — Survey an existing codebase (if any)
Glob the target for source/build files. If code is present, spawn
`ideate:architect` in **analyze mode**: it returns the stack, established
patterns, load-bearing constraints, and a risk map. Record it with
`record_append(kind="design", ...)` as durable project context — `refine` and
`execute` will read it so their work respects what's already there. For a
greenfield project, skip this.

## Step 5 — Capture intent and steering (light interview)
Ask a small, focused set of questions (aim for 3–6): what the project is and
who it's for, the quality bar, hard constraints, and any non-negotiable rules.
This is about **intent and standards**, not a work plan. From the answers (and
the survey), write the durable steering with `steering_put`:
- **Guiding principles** (`kind:"guiding-principle"`) — standing values.
- **Constraints** (`kind:"constraint"`) — hard limits.
- **Policies** (`kind:"policy"`) — specific rules the work must follow.
Give each a stable `id` and a `domain` tag; keep statements crisp and testable.
Record the interview with `record_append(kind="interview")`.

## Step 6 — Close out
- `record_append(kind="project-setup")` — a short marker that the project is
  set up: what it is, the steering captured, whether a codebase was surveyed.
- `record_append(kind="journal")` — the setup narrative.
- Tell the user setup is complete and point them to **`/ideate:refine`** to
  turn their first idea into actionable work. That is the next step every time;
  `init` runs once.

## Guardrails
- `init` sets up; it never decomposes or creates work items. If the user
  describes work they want done, capture it as intent/steering here and tell
  them `refine` will decompose it — don't create board items yourself.
- You are the only writer; the architect subagent only returns its survey.
- User-invocation-only by design (`disable-model-invocation`): it makes broad
  setup writes and enables a gated store, so it should never fire on model
  discretion.
