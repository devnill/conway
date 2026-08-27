# Local-model permission classifier -- premise experiment

**THROWAWAY. Do not merge any of this into the product.**

This directory exists to answer one question, cheaply, before any of the
plumbing in `docs/vision/DESIGN-permission-modes.md` gets built: can a
small local model, given `{tool, category, arguments, rendered, cwd}`,
usefully distinguish a dangerous tool call from a routine one? Design §7's
second falsifier says plainly that if it cannot, no amount of plumbing
helps and the honest answer is pattern rules.

It is evidence attached to board item `01M0WX32AKGA9W3S0KCVZHAGED`, not an
implementation of it. Nothing here is wired into conway's config, its
`[hooks].rules[]`, or any real session.

## Contents

- `classify.sh` -- the throwaway `pre_tool_use` hook script itself. Reads a
  conway `HookEvent` JSON object on stdin (`{"name": "pre_tool_use",
  "payload": {tool, category, arguments, rendered, agent_id, agent_path,
  session, cwd}}`, matching `crates/conway-runtime/src/permission.rs:1572`
  and `crates/conway-core/src/hook.rs` exactly), asks a local Ollama model,
  and prints a `HookAnswer` to stdout (`{"permission": "no_opinion"}` or
  `{"permission": {"deny": {"reason": "..."}}}`).
- `corpus.jsonl` -- 48 labelled cases, one JSON object per line:
  `{id, label: "dangerous"|"routine", group, notes, event}`. Groups:
  `destructive` (12, genuinely dangerous), `superficially_alarming` (10,
  routine but looks scary), `plain` (12, routine and unremarkable),
  `near_miss` (14 = 7 pairs, differing by one token/field, split
  dangerous/routine).
- `run_corpus.py` -- the harness. Runs every case through `classify.sh`
  against one or more models, measuring latency and parsing each answer
  against conway's real `HookAnswer` wire shape (not a looser heuristic --
  see its `parse_hook_answer` for the exact rule, including that a
  timeout/nonzero exit is scored as `pre_tool_use`'s real fail-closed
  behaviour would score it: unparseable). Repeats the `near_miss` group
  `--repeats` times to measure non-determinism. Writes one JSON file per
  model to `results/`.
- `results/*.json` -- raw per-case run records (prediction, latency, raw
  stdout/stderr, exit code) for each model tested.
- `evidence.md` -- the report: false allows, false denials, latency,
  non-determinism, malformed-output rate, and the proceed/adjust/abandon
  recommendation.

## Running it

Requires Ollama running locally (`curl -s http://localhost:11434/api/tags`)
with the target model(s) pulled.

```
python3 run_corpus.py --model gemma4:e4b --model qwen2.5:14b --repeats 3
```

## What this deliberately does not do

- It does not touch conway's config, `[hooks].rules[]`, or any real
  session -- `classify.sh` is invoked directly by `run_corpus.py`, never
  registered as a hook.
- It does not answer design spec questions 3 (the fail-closed hazard felt
  live, at the TUI, with the model server killed mid-session) or 4 (whether
  the `AUTO-ALLOW` status line misleads while a guard is running). Those
  require a human at a live terminal and are explicitly left for the
  operator -- see `evidence.md`'s closing section.
