# `docs/getting-started.md` on a default build — executed walkthrough

Evidence for board item `01KZVYSQ7QHT8XRN1M39BP7X9S` ([S0f]), whose acceptance
required the page be **followed verbatim to a working session, executed rather
than reasoned about**. Same genre as
`.design/context-hook-noop-writer-compile-evidence.md` and
`.design/router-installation-q2-compile-evidence.md`.

Run on 2026-08-13, against `main` at `7e020d5`, on macOS (Darwin 25.5.0).

## What was being checked

The page taught `.conway/models.json` as a mandatory step and quoted a routing
error to show what happens without it. That error lives at exactly one place in
the tree — `crates/conway-plugin-routing/src/router.rs` — so it cannot occur
unless the routing plugin is installed. A reader on a default build would hit
neither the requirement nor the error, and could not tell which half of the
instruction was real.

## Setup — a fresh default build, following the page's own steps

Binary built per the page's Install section:

```console
cargo build -p conway-cli --release      # exit 0
```

Fixture: an empty directory with `HOME` and `XDG_CONFIG_HOME` redirected into
it, so nothing on the operator's real machine could contribute config. Its
entire contents were `.conway/settings.json`, copied **verbatim** from the
page's "An OpenAI-compatible endpoint" example:

```json
{
  "default_role": "coder",
  "backends": {
    "local": {
      "kind": "openai-compat",
      "dialect": "ollama",
      "base_url": "http://localhost:11434/v1"
    }
  },
  "roles": {
    "coder": { "chain": ["local/qwen3:4b"] }
  }
}
```

No `models.json`. No `[plugins]` key, so no routing plugin. Backend was a local
Ollama serving `qwen3:4b` — the exact model the page's example names.

## The four runs

| # | routing plugin | `models.json` | exit | result |
| --- | --- | --- | --- | --- |
| 1 | not installed | absent | **0** | served — stdout `4` |
| 2 | not installed | absent | (killed) | served, but the model looped on tool calls; see below |
| 3 | **installed** | absent | **4** | **the exact quoted error** |
| 4 | **installed** | present | **0** | served — stdout `4` |

### Run 1 — the load-bearing one

```console
$ conway -p "What is 2+2? Answer with just the number. Do not use any tools."
4
$ echo $?
0
```

No routing error. No warning about the missing `models.json`. A session log was
written (`.conway/sessions/01KZWT0ET8E669YPNQWXB3GQZA.jsonl`, 5 records, plus
`index.jsonl`), which is what makes this a *working session* rather than a
process that merely exited zero.

**`grep "unknown (backend, model) pair\|routing error"` over every run's
captured output: no match in runs 1, 2 or 4.**

### Run 2 — an aside worth recording

The first attempt used the prompt `"Reply with exactly the word: pong"` and ran
past 45s. It was not a routing or config failure — stderr showed the turn had
reached the backend and was cycling:

```text
conway: warning: tool call proposed: report (call_0x50ay28)
conway: warning: permission denied for call call_0x50ay28
conway: warning: tool call proposed: bash (call_kuvr24fd)
conway: warning: permission denied for call call_kuvr24fd
```

That is one-shot mode failing closed with an empty `--allowed-tools`, exactly
as the page's "Running non-interactively" section documents. A 4B model simply
kept reaching for tools it could not have. It is included here because it is
*positive* evidence: routing had already succeeded and the request had reached
the provider before any of it happened.

### Run 3 — the counterfactual, which is what proves the correction

`[plugins].install: ["conway.routing"]` added, `models.json` still absent:

```text
conway: error: runtime error: routing error: no candidate for role coder (1 considered): local/qwen3:4b: capability: capabilities: unknown (backend, model) pair
$ echo $?
4
```

The exact error the page quotes, reproduced — and reproduced **only** with the
plugin installed. Exit 4 is `NoHealthyBackend`, matching `docs/scripting.md`'s
exit-code table.

### Run 4 — and it clears once the file exists

`models.json` added verbatim from the page's example, plugin still installed:
exit 0, stdout `4`.

## What this establishes

- A default build **routes and serves without `models.json`**. The page's "it's
  not optional" was false for the configuration the page itself hands a new
  reader.
- The quoted error **belongs to the routing plugin**. Runs 1 and 3 differ in
  exactly one line of config and produce opposite outcomes.
- `models.json` is still worth writing on a default build, which is why the
  corrected page recommends rather than drops it: without it the TUI status
  line falls back to a raw token count instead of `ctx N%`
  (`crates/conway-cli/src/tui/app.rs`'s `model_max_context` is empty, and
  `view/status.rs`'s `ctx_label` takes its no-max branch). Startup is not
  blocked and no error is raised.

## Limits of this evidence — stated rather than glossed

The served-turn evidence is via `-p`, which is a documented step on the same
page. **The interactive TUI was not captured.** Two attempts to record its
screen under `/usr/bin/script` produced no readable output (the alternate
screen does not survive the capture), and driving it further was judged a
rabbit hole rather than evidence. What that leaves unproven is the TUI's
*rendering*, not its configuration: both surfaces construct through the same
`conway-cli` `build_conway` path, which is what runs 1, 3 and 4 exercised, and
every claim this item corrected is a config-and-routing claim.
