# autopilot phase — refine

Inline refinement logic for the autopilot loop. Mirrors `/ideate:refine` but
runs inside the autopilot context. The controller runs this only when the
review verdict was `needs-refinement`. Its job: turn this cycle's findings into
the next cycle's board work — and detect when the loop is failing to converge.

## Turn findings into work
Read this cycle's findings (`record_read(scope="finding")` / the verdict output,
paged to exhaustion as below; record rows carry the `claim`, not the prose, so
fetch a finding by `id` with `include_content: true` when you need the detail)
and the current board (`work_list`, paged to exhaustion — follow `next_cursor`
until it is `null`; a short page is not the end — since the guard below counts
the whole board):
- For each unresolved `critical`/`significant` finding, either update an
  existing open item (`work_update_meta`, using its current `version` as
  `expected_version`) or `work_create` a new item that fixes it. Wire
  `depends_on`/`parent_id` so the fix sequences correctly.
- For a structural finding, spawn `ideate:architect` (design mode) first, then
  `ideate:decomposer`, and create the returned items (map `ref`→id in creation
  order). Record the design choice with `record_decision`.
- Amend steering (`steering_put`) where a finding implies a rule change.
- Do **not** edit `done` items — supersede with new work.

## Divergence guard (load-bearing)
Compare the count of open/pending board items now against the previous cycle's
count (from the last `autopilot-cycle` record):
- If pending work is **decreasing**, the loop is converging — continue.
- If pending work is **flat or growing** across cycles, the loop is **not
  converging** (every fix spawns as much work as it closes). Do not start
  another cycle blindly: raise an Andon to `ideate:proxy-human` with the trend;
  if it can't resolve the stall, **stop the loop** and hand back to the human
  via `reporting.md`. Burning cycles on a diverging project is the failure mode
  this guard exists to prevent.

## Output (hold for the controller / cycle record)
- New/updated board items (ids) and steering amendments.
- The pending-work trend (this cycle vs. last) and the divergence decision.

Return to the controller, which increments the cycle and checks limits.
