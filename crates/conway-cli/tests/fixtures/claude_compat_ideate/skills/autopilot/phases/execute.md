# autopilot phase — execute

Inline execution logic for the autopilot loop. This mirrors `/ideate:execute`
but runs **inside** the autopilot context (no skill call) and routes Andon to
`ideate:proxy-human` instead of the user. Read this at the start of each
cycle's build phase and run it.

## Preconditions (already held by the controller)
- `actor_human` resolved; board validated non-empty; steering and intent
  loaded.

## Loop over the claimable frontier
Read the whole board — `work_list`, following `next_cursor` until it is `null`
(a short page is not the end) — and take its derived `claimable` items. Rows
carry no `spec`; the body arrives with the claim. First, sweep for stale
state: any `in_progress` item from a prior interrupted cycle — check
`work_events`, and `work_release` it (with a note) if its claim is dead so it
becomes claimable again.

For each claimable item, apply **board claim discipline**:
1. `work_claim(id, actor_human, lease_ms)` → `claim_token`. Size `lease_ms` to
   the item; `work_renew` if a worker runs long. A lapsed lease is reclaimed by
   the board and a stale-token complete fails `INVALID_CLAIM` — that's the fence.
2. **Human-gate?** If the item's `spec_format` is `ideate/human-gate`, it needs
   a HUMAN, not a worker. Do NOT spawn `ideate:worker` — route it to
   `ideate:proxy-human` with the item + intent (unattended Andon), record its
   decision (`record_append(kind="andon")`), and act on it. A code item may
   `depends_on` a human-gate; the derived `claimable:false` holds the downstream
   frontier until the gate completes (GP-27). Then `work_complete` or
   `work_release` the gate item itself and continue to the next claimable item.
3. Assemble context: the item `spec` (authoritative — `work_claim` returned it)
   + applicable steering (`steering_read`, paged to exhaustion as above — one
   page is not the ruleset) + scoped decisions (`record_read`; its rows carry
   no prose, so pull the body of each decision you pass with
   `record_read(id, include_content: true)`, one id at a time).
   Spawn `ideate:worker` with all of it.
4. Incremental review: spawn `ideate:code-reviewer` (and `ideate:spec-reviewer`
   for spec-sensitive items) on the change.
5. Findings by severity:
   - `minor` → fix inline, note in journal.
   - `significant` → fix if cheap; else `record_append(kind="finding")` and
     `work_create` a follow-up.
   - `critical` (including startup/smoke-test failure and test-infra failure) →
     **Andon**: `record_append(kind="finding")`, `work_release` the current
     claim, and spawn `ideate:proxy-human` with the finding + intent. Record its
     decision (`record_append(kind="andon")`) and act on it — fix as directed,
     defer (leave open, flag for human), or drop.
6. Complete or release: complete only on evidence — the worker's report must
   include the spec's `VERIFICATION ANCHOR` command and its fresh output, and
   the `work_complete(id, token, note)` note references that evidence. Verified
   `complete` with no unresolved critical/significant finding → `work_complete`.
   Otherwise (blocked, Andon'd, or verification not run) → `work_release(id,
   token, note)` — never complete blind. **Never leave an item claimed.**
7. Re-read `work_list` (the same full walk) — completing items unblocks
   dependents. Continue until the frontier is empty or a proxy-human decision
   halts the cycle.

## Output of this phase (hold for the controller / cycle record)
- Items completed this cycle (ids).
- Findings raised, by severity, and how each was handled.
- Any proxy-human decisions (with their `human:true` flags).
- Per-item journal notes → fold into the cycle journal in `reporting.md`.

Then return to the controller, which proceeds to `phases/review.md`.
