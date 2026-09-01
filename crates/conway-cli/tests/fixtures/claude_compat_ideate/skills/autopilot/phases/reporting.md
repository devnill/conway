# autopilot phase — reporting

Terminal phase for the autopilot loop. The controller runs this once, when the
loop ends, for any reason. Its job: reconstruct and present a truthful account
of the whole run from the durable record — autopilot ran unattended, so this
report is how the user learns what happened.

## Addressing the human
Any prose this surface writes for a person — a summary, a report, a question,
ordinary conversation, not only a confirm gate — follows the shared rule in
`skills/shared/human-presentation.md` (relative to the plugin root). Read it
and apply it to everything a person will read; it is the single copy — do not
paraphrase or restate it here.

## Determine why the loop ended
One of:
- **Converged** — review returned `converged` and the board has no open
  claimable work. The project met its bar.
- **Max cycles** — `cycle >= --max-cycles`. Work may remain.
- **Appetite spent** — the recorded effort budget was reached.
- **Diverged / stalled** — `refine.md`'s divergence guard stopped the loop, or
  a proxy-human decision halted it. Needs human attention.

## Reconstruct the run from the record
Autopilot state lives in records, not memory — rebuild from ground truth:
- `record_read(scope="autopilot")` — the `autopilot-cycle` records: per-cycle
  items completed, findings, verdicts, head commits. "Every cycle" means paging
  to exhaustion: follow `next_cursor` until it is `null`; a page shorter than
  `limit` is not the end, so a one-page read silently under-reports the run.
  Rows carry the `claim`, not the prose — re-read one cycle by `id` with
  `include_content: true` when you need its detail.
- `work_list` — final board state (done / open / cancelled counts, remaining
  claimable frontier). These counts are whole-board: page with `next_cursor`
  until it is `null`; a page shorter than `limit` is not the end.
- `record_read(scope="andon")` — the proxy-human decisions, especially those
  flagged `human: true`. Page this one to exhaustion too — the report claims
  to list them all — and where the flag or rationale isn't in a row's `claim`,
  fetch that decision by `id` with `include_content: true`.
- `git log --oneline <first-cycle-head>..HEAD` — what actually shipped.

## Present the report
- **Outcome** — why it ended (above), stated plainly. If not converged, say so
  clearly; do not dress up an incomplete run as success.
- **Work** — cycles run, items completed vs. still open, commits produced.
- **Findings** — totals by severity across the run, and how many remain open.
- **Proxy-human decisions** — list them, and **highlight the `human:true`
  ones** — these are the autonomous calls a human should still review.
- **Remaining work** — the open board frontier and the recommended next step
  (`/ideate:refine` for a stalled/incomplete run, or the next planned work if
  converged).

Finally append a closing `record_append(kind="journal")` summarizing the run,
and a `record_append(kind="autopilot-complete")` milestone with the outcome.

## Honesty bar
This report is the only window the user has into an unattended run. Report
failures, deferrals, and skipped work explicitly, with evidence (record ids,
commits). A converged verdict must be backed by the derived convergence test,
not asserted.
