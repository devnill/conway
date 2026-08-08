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
- `explain_routing` itself asks for nothing (`RequiredCaps::default()`) —
  any capability requirement a `SKIPPED` entry names still comes from
  somewhere, and as of the per-role floor above that somewhere can now be
  `[roles.<alias>]`'s own configured fields, not just a caller-supplied
  requirement.

For the routing decision an actual turn just made, the TUI's `/why`
command shows the last live `Event::ModelDecision` for the focused agent
instead (see [`interactive.md`](interactive.md)) — that one carries the
turn's real `est_tokens`.

**Where the report type lives, and what happens with a non-default
router.** `ExplainReport` (and the field types it's built from --
`ExplainEntry`, `EntryOutcome`, `CapabilitySummary`, `BreakerSnapshot`) are
defined in `conway_core::routing`, not in `conway-routing` -- so producing
one never requires depending on `conway-routing`'s filtering logic.
`conway-routing::RoutingExplain` (the rich, capability- and health-filtered
answer this page's examples above show) is one producer; embedders that
supply their own `Router` via `ConwayBuilder::with_router` get a different
one automatically: `Conway::explain_routing` falls back to
`conway_core::routing::MinimalRouter`, projected over the same
`RoutingConfig` the embedder's `settings.json` declares. That fallback
report is honestly *degenerate*, not empty and not fabricated-rich: one
entry per configured chain candidate (position `0` `SELECTED`, the rest
`SKIPPED`), every `capabilities` field `None`, and every `breaker` field
`Closed` -- because a `MinimalRouter` genuinely indexes no capabilities and
tracks no real breaker state, and inventing either would be exactly the
kind of claim GP-14 forbids. Critically, `conway routes explain` still
distinguishes "unknown role" from "configured role, empty report" in this
configuration: it checks `[roles]` directly against your configuration,
not whether the report came back with zero entries -- a configured role
whose chain happens to be empty gets an honest, entry-less report rather
than being misreported as "unknown".

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
error is in [`getting-started.md`](getting-started.md). This holds even
when `probe_on_startup` is on and the live server reports that model: the
probe may only confirm and narrow capabilities for a pair `models.json`
already declares, never add a pair on its own say-so — `models.json` stays
the sole, hand-written source of which pairs are routable at all.

**A model the startup probe observes but `models.json` never lists is
dropped, silently as far as routing is concerned.** It never becomes
routable and produces no warning or error at any of the normal log
levels — the drop is logged only at `debug`
(`crates/conway/src/builder.rs`, `probe_on_startup: server reported a
model with no models.json entry for this backend; not admitting it`). Most
deployments do not run at `debug`, so this is easy to mistake for "the
probe never reached my server" rather than "the probe saw the model and
RESTRICT dropped it." To see these drops, raise just this module's
filter rather than the whole process's:

```sh
RUST_LOG=conway::builder=debug conway ...
```

(`RUST_LOG=debug` also works but is far noisier — it raises every crate,
not just this one.) Each dropped pair logs its `backend` and `model`
fields, so you can tell exactly which declarations are missing from
`models.json`.

Once resolved, a candidate is checked against the role's requirement
floor and, last, against context headroom. A role's requirement floor is
set directly in `settings.json` — `[roles.<alias>]` carries
`tool_calling`, `structured_output`, `parallel_tool_calls`, `reasoning`,
`min_reliability`, and `min_context`, alongside `chain` and
`headroom_tokens`, and `ConwayConfig::routing()` maps every one of them
into the candidate's `RequiredCaps`:

```json
// .conway/settings.json
{
  "roles": {
    "coder": {
      "chain": ["local/qwen3-coder-80b", "anthropic/claude-sonnet-4-6"],
      "tool_calling": "streaming_validated",
      "structured_output": "json_schema",
      "parallel_tool_calls": true,
      "reasoning": false,
      "min_reliability": "verified",
      "min_context": 32768
    }
  }
}
```

Every field is optional and defaults to "no requirement" — an existing
config that sets none of them behaves exactly as before. `tool_calling`'s
wire vocabulary is `"none"` | `"non_streaming"` | `"streaming"` |
`"streaming_validated"` (a flat string, not
`conway_core::capabilities::ToolCallSupport`'s own `{"streaming":
{"validated": true}}` object shape); `structured_output` is `"none"` |
`"json_schema"` | `"grammar"`; `min_reliability` is `"verified"` |
`"community"` | `"unknown"`.

Closing this gap took two changes, not one: `ConwayConfig::routing()`
mapping the six fields into `RequiredCaps` (above) was necessary but not
sufficient — `DeclarativeRouter` did not read a role's configured
`required` at all (`CompiledRole` did not carry the field, and candidate
admission consulted only the caller-supplied `RouteRequest.required`), so
setting these keys previously had zero effect on a real turn regardless of
what the schema parsed them into. The router now merges the two: each
candidate's admission check runs against the **pointwise strictest**
combination of the role's configured floor and whatever `required` the
caller (or, for a real turn, `conway-runtime`'s own turn-time logic —
currently just a `tool_calling >= non_streaming` floor whenever the turn
has any registered tools) already supplied — per field, whichever of the
two demands more wins; neither side can weaken the other. `conway-routing`'s
`satisfies` still walks all seven `RequiredCaps` fields against that merged
result, headroom last, exactly as before; a candidate that fails one shows
up as an ordinary `RoutingReason::CapabilitySkip` / `context: ...`-style
entry, e.g. `reliability_tier: requires Verified, has Community`.

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

That's the per-candidate detail you'll see inside a `routing error: no
candidate for role ...` message (as in the `getting-started.md` example
above) whenever at least one *other* candidate, or this same candidate,
was also disqualified for a non-context reason (an unindexed model, a
health-open breaker, or a missing capability). When context is the
*only* thing wrong — every candidate in the chain would otherwise have
been selected, and each one's window alone is too small — conway raises
a distinct, terminal error instead of `NoCandidate`:

```
context rejected: 34000 prompt + 16000 reserved output = 50000 tokens, but ollama-cloud/glm-5.2 accepts at most 40000 (short by 10000); no truncation or escalation is performed
```

This is `RoutingError::ContextTooLarge`: it names the input size, the
resolved headroom, and the *largest* window among the candidates that
still didn't fit (so a chain with several too-small models reports its
best case, not an arbitrary one). No truncation or escalation ever
happens on your behalf — this is terminal by design; shrink the turn's
content, raise the role's headroom budget, or add a larger-window
candidate to the chain.

### Advisory vs. authoritative: two context checks, not one

The `heuristic-chars4` estimate above is deliberately cheap — it runs
before a request has even been assembled for a specific backend, as a
first-pass filter over the router's declared `chain`. It is **advisory**:
a candidate that fails it is skipped before conway ever contacts that
provider, but nothing about the estimate is what actually decides
whether a real request fits.

The **authoritative** answer comes from the model adapter itself, the
only party that actually knows how its own wire format counts tokens.
Once a route has survived the router's advisory filter, `conway-runtime`
builds that candidate's real request — its assembled segments, tools,
cache hints, and sampling params, exactly as it will be sent — and asks
the backend to admit it (`Backend::admit`). Each dialect estimates its
*own* serialized wire body: an Anthropic Messages envelope and an
OpenAI-compatible chat-completions body are different byte sequences for
identical content, so the two adapters genuinely produce different
numbers for the same prompt. `Backend::admit`'s refusal is a typed
`ContextTooLarge`, carrying the same shape of numbers (input estimate,
headroom, window, shortfall) as the router's own rejection above. A
refusal skips only that one candidate — no network call is made, and it
never trips a circuit breaker (a too-large prompt says nothing about the
endpoint's health) — and the chain advances to the next candidate exactly
as it does for any other request-incompatible failure.

**The two checks are not required to agree, and a test asserting they do
would be asserting the wrong thing.** The router's estimate is a rough
heuristic over a *declared* window (`models.json`'s `max_context_tokens`);
`Backend::admit`'s estimate is a real count over the *actual* bytes a
specific dialect will send. A candidate the router's advisory filter
waves through can still be refused by `admit` (a stale or optimistic
capability entry, or simply a more accurate estimate) — this is by
design, not a bug to reconcile. When every candidate in a chain fails its
own `admit` this way, `conway-runtime` aggregates those refusals into the
same `RoutingError::ContextTooLarge` shape, naming the largest window
among them, so the two paths look identical from the outside even though
they are answering genuinely different questions at genuinely different
times.

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
    "probe_enabled": false
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
error; they currently have no observable effect. This is a deliberate,
labeled forward declaration (GP-14), not an oversight: the Transport
breaker alone already handles recovery (a clock read takes it half-open;
the next real request retries), so wiring the prober is a latency
optimization, not a correctness fix, and GP-12 requires a measured
baseline before shipping an optimization. `probe_enabled` defaults
`false` for exactly this reason — a fresh install must not assert
periodic probing that does not happen. Board item
`01KZ802GSF692EKYKQ2TTVCJB8` tracks wiring it. The Transport breaker,
by contrast, is wired end to end and does exactly what's described above
— every example in this section was captured against a real run.

(Do not confuse this periodic health prober with the *startup*
`[models].probe_on_startup` capability probe covered above under
"Capability matching" — same word, two unrelated mechanisms: one
discovers model capabilities once at startup and is already wired; this
one would feed the Probe breaker on an ongoing basis and is not.)

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
