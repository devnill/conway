# Routing requests to a model

Routing is declarative and content-blind: a role names an ordered chain
of `backend/model` candidates, conway walks that chain admitting or
skipping each one against a fixed set of rules, and you can always ask
which model actually served a request and why. Nothing here reads your
prompt to decide where it goes. For how to point conway at the providers
these chains reference, see [`providers.md`](providers.md).

## Roles and fallback chains

A role is a named alias — `default_role`, `--role-override`, or an
agent definition's own role — resolved to a `[roles.<alias>]` chain in
`.conway/settings.json`:

```json
// .conway/settings.json
{
  "default_role": "coder",
  "roles": {
    "coder": {
      "chain": ["anthropic/claude-sonnet-4-6", "local/qwen3:4b"]
    }
  }
}
```

`chain` is an ordered list of `"backend/model"` strings, first-eligible-
wins: conway tries position `0`, and only tries position `1` if the
attempt at `0` fails in a way that's worth trying elsewhere (see
[health and failover](#health-and-failover) for exactly which failures
qualify). Every entry in the chain needs a backend configured (see
[`providers.md`](providers.md)) and a matching `.conway/models.json`
entry, or it's skipped before conway ever contacts that provider.

Two flags override chain resolution for a single run:

| Flag | Effect |
| --- | --- |
| `--role-override <role>` | Use this role instead of `default_role`. |
| `--model <backend/model>` | Pin a specific model, bypassing the chain entirely (`RoutingReason::PinnedByApi`). |

## Asking why a route was chosen

`conway routes explain <role>` is the direct answer to "which model
served this, and why":

```console
conway routes explain coder
```

```text
role: coder  (est_tokens=0, headroom_tokens=4096)
  [0] anthropic/claude-sonnet-4-6  SELECTED primary for role `coder`  (breaker: closed)
  [1] local/qwen3:4b               SELECTED fallback #1 after:   (breaker: closed)
```

Each `SELECTED`/`SKIPPED` line is real router output, one per chain
entry, in position order — not just the one that ran. A `SKIPPED` entry
names exactly what disqualified it:

```text
role: coder  (est_tokens=0, headroom_tokens=4096)
  [0] anthropic/claude-sonnet-4-6  SELECTED primary for role `coder`  (breaker: closed)
  [1] local/qwen3:4b               SKIPPED  skipped `local/qwen3:4b`: missing capabilities: unknown (backend, model) pair  (breaker: closed)
```

`--json` renders the same report machine-readably. Two things worth
knowing about what this command actually evaluates (both verified
against `conway::Conway::explain_routing`):

- It always runs with `est_tokens = 0` — a synthetic, content-free probe
  of eligibility right now, not a re-evaluation of any real conversation.
  A capability or health skip shows up exactly as it would live; a skip
  caused specifically by *your current conversation's* size does not —
  headroom alone still has to exceed a candidate's window to show as
  skipped here.
- "SELECTED" on more than one entry is normal: it means each of those
  candidates would be picked if routing reached that position, not that
  conway tried all of them. Position `0` is what actually runs first.

For the routing decision an actual turn just made, the TUI's `/why`
command shows the last live `Event::ModelDecision` for the focused agent
instead (see [`interactive.md`](interactive.md)) — that one carries the
turn's real `est_tokens`.

## Capability matching

Each chain candidate's `Capabilities` are resolved with a fixed
precedence, config closest to you winning: **your `models.json` entry
> a live startup probe (`[models.metadata_path]` with
`probe_on_startup`) > the backend's declared `Profile` defaults**. Only
two of `models.json`'s four fields actually reach this resolution —
`max_context_tokens` and `reliability_tier`; `tool_calling` and
`reasoning` are informational only (`getting-started.md` says the same).
A `(backend, model)` pair with no `models.json` entry at all fails
admission immediately, before conway contacts the provider — the exact
error is in [`getting-started.md`](getting-started.md).

Once resolved, a candidate is checked against the role's requirement
floor and, last, against context headroom. **As shipped, the requirement
floor `settings.json` actually lets you set is headroom** — verified
against `crates/conway/src/config/schema.rs`'s `RoleEntry`: it carries
`chain` and `headroom_tokens` and nothing else, so every role's other
capability requirements (`tool_calling`, `structured_output`,
`parallel_tool_calls`, `reasoning`, `min_reliability`, `min_context`)
resolve to "no requirement" for every role today. The richer
`RequiredCaps` struct these map to is fully implemented and enforced —
`conway-routing`'s `satisfies` walks all seven fields, headroom last —
it's just not reachable from the config file yet; setting it requires
embedding conway as a library and constructing a `RouteRequest`
directly.

## Headroom

Headroom is tokens reserved for the model's own output and reasoning,
added to your estimated prompt size before the context-window check:

```
est_tokens + headroom_tokens <= max_context_tokens
```

It's declarative config, never computed from content — a global default
with a per-role override:

```json
// .conway/settings.json
{
  "routing": { "default_headroom_tokens": 8192 },
  "roles": {
    "coder": {
      "chain": ["anthropic/claude-sonnet-4-6"],
      "headroom_tokens": 4096
    },
    "planner": {
      "chain": ["anthropic/claude-sonnet-4-6"]
    }
  }
}
```

`coder` uses its own `4096`; `planner`, with no override, falls back to
`[routing].default_headroom_tokens`. Precedence is per-role override >
global default > conway's own built-in constant (`8192`) if you set
neither. Two env vars reach the same knobs without touching the file:
`CONWAY_ROUTING__DEFAULT_HEADROOM_TOKENS=16000` and
`CONWAY_ROLES__<ALIAS>__HEADROOM_TOKENS=32768` (the latter only applies
to a role that already exists in the merged config; an unknown alias is
ignored, not an error). There's no `--headroom-tokens` CLI flag — only
`settings.json` and these two env vars reach it.

### Estimated, not exact

`est_tokens` is a heuristic, never a real tokenizer count — conway's
context builder names it explicitly (`TOKEN_ESTIMATOR =
"heuristic-chars4"`): each content block contributes `ceil(chars / 4) +
4` tokens (the `+4` standing in for wire-format framing), summed across
the assembled prompt. This is deliberately conservative, not precise —
don't present a routing decision as if the token figure gating it were
exact. When a candidate is rejected on context, the message says so in
full:

```
context: needs 34000 input + 16000 headroom = 50000, model max_context_tokens is 40000
```

## Health and failover

Two independent circuit breakers exist per backend (its `EndpointId`,
1:1 with the backend id — every model on the same backend shares one
breaker pair): a **Transport** breaker fed by real request failures, and
a **Probe** breaker fed by periodic liveness checks, tuned by `[health]`:

```json
// .conway/settings.json
{
  "health": {
    "transport_failures_to_open": 3,
    "open_duration_secs": 30,
    "probe_interval_secs": 15,
    "probe_timeout_secs": 2,
    "probe_failures_to_open": 3,
    "half_open_successes_to_close": 1,
    "probe_enabled": true
  }
}
```

Every field above is the built-in default. A breaker is `Closed` (used
normally), `Open` (skipped until `until`, a fixed duration — no
backoff), or `HalfOpen` (one probationary attempt after `until` passes;
one more failure reopens it for another full `open_duration_secs`, one
success closes it).

What actually trips the Transport breaker is scoped narrowly: a
transport error, a `5xx`, or a rate limit counts; an auth failure, a
malformed request, or a too-large prompt does not — those either abort
the whole chain immediately (auth) or advance to the next candidate
without touching breaker state (a bad request or an oversized prompt may
still be perfectly servable by a different model). `conway routes
explain <role>` shows every breaker's current state, and a candidate
skipped for `HealthSkip` names which breaker and until when.

**Verified: only the Transport breaker is live in a running `conway`
process today.** `conway-routing`'s periodic prober
(`conway_routing::prober::HealthProber`, the component that would feed
the Probe breaker independently of request traffic) is fully implemented
and tested in that crate, but no call site in `conway`, `conway-runtime`,
or `conway-cli` ever spawns it — checked directly, it's referenced
nowhere outside `conway-routing` itself. `probe_enabled`,
`probe_interval_secs`, and `probe_timeout_secs` validate and load without
error; they currently have no observable effect. The Transport breaker,
by contrast, is wired end to end and does exactly what's described above
— every example in this section was captured against a real run.

### What you see when a route is skipped

A skip that happens mid-turn, silently advancing to the next chain
candidate without tripping a breaker, produces no dedicated notice —
you'll just see the eventual successful model. The moment a breaker
actually *opens* is the one point this becomes visible live: a
`BackendDegraded` event fires, which the TUI renders as a transcript
notice and one-shot mode prints to stderr as `backend degraded:
<endpoint>`. A captured example, chain `[anthropic, local]` with the
first candidate unreachable:

```text
conway: routed role 'coder' to anthropic/claude-sonnet-4-6
conway: warning: backend degraded: anthropic
conway: routed role 'coder' to local/qwen3:4b
```

Breaker state lives in memory for the life of one `conway` process —
each TUI session or one-shot invocation starts with every breaker
`Closed`, regardless of a previous run's history.

## Prompt caching: economics, not correctness

Caching changes what a request costs, never what it returns. conway
produces identical results whether a provider's cache is warm, evicted,
or unavailable entirely — a guarantee backed by a per-adapter test, not
just a claim:

- **Anthropic** uses explicit cache breakpoints (`cache_control`),
  attached in a strictly additive post-pass over an already-built request
  body. `body_with_hints_stripped_equals_body_with_hints_minus_every_cache_control_key`
  (`conway-backends/tests/anthropic_cache_mapping.rs`) pins this: strip
  every cache hint and the body is identical except for the absence of
  `cache_control` keys — nothing else about the request changes.
- **OpenAI-compatible providers** (Ollama, vLLM, Kimi's platform API, and
  others) cache implicitly on prefix match — there's no request field to
  set at all. `cache_hint_never_changes_the_serialized_request_body`
  (`conway-backends/src/openai_compat/wire.rs`) pins the stronger claim:
  this adapter never reads a cache hint in the first place, so a marked
  segment and an unmarked one serialize identically.

A profile's `cache` field (see [`providers.md`](providers.md)) is
informational for exactly this reason — it tells `conway-runtime`
whether it's worth marking a hint at all, never how a request is built.

## Worked example: a fallback chain across a cloud and a local provider

A `coder` role that tries Anthropic first and falls back to a local
Ollama server, assembled in one place — the three files this needs, none
of them requiring anything beyond what [`providers.md`](providers.md)
already covers:

```json
// .conway/settings.json
{
  "default_role": "coder",
  "backends": {
    "anthropic": {
      "kind": "anthropic",
      "api_key_env": "ANTHROPIC_API_KEY"
    },
    "local": {
      "kind": "openai-compat",
      "dialect": "ollama",
      "base_url": "http://localhost:11434/v1"
    }
  },
  "routing": {
    "default_headroom_tokens": 8192
  },
  "roles": {
    "coder": {
      "chain": ["anthropic/claude-sonnet-4-6", "local/qwen3:4b"],
      "headroom_tokens": 4096
    }
  }
}
```

```json
// .conway/models.json
{
  "models": {
    "anthropic/claude-sonnet-4-6": {
      "max_context_tokens": 200000,
      "tool_calling": "yes",
      "reasoning": true,
      "reliability_tier": "verified"
    },
    "local/qwen3:4b": {
      "max_context_tokens": 32768,
      "tool_calling": "yes",
      "reasoning": false,
      "reliability_tier": "community"
    }
  }
}
```

```console
export ANTHROPIC_API_KEY=sk-ant-...
```

That's the whole thing: no `profiles.toml` needed unless your local
server needs a dialect conway doesn't already ship. Confirm it resolves
the way you expect before spending a real request on it:

```console
conway routes explain coder
```

```text
role: coder  (est_tokens=0, headroom_tokens=4096)
  [0] anthropic/claude-sonnet-4-6  SELECTED primary for role `coder`  (breaker: closed)
  [1] local/qwen3:4b               SELECTED fallback #1 after:   (breaker: closed)
```

Both candidates are eligible right now, in the order they'll be tried.
This exact config was then run for real, with Anthropic deliberately made
unreachable to prove the fallback: the turn routed to Anthropic, the
connection failed repeatedly, the breaker opened
(`backend degraded: anthropic`), and the same turn's fallback candidate —
a real local Ollama server — answered instead, with no session restart
and no manual intervention. That's the exact `--verbose` sequence shown
under ["what you see when a route is skipped"](#what-you-see-when-a-route-is-skipped)
above.
