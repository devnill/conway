# Routing requests to a model

Routing is declarative and content-blind: a role names an ordered chain
of `backend/model` candidates, conway walks that chain admitting or
skipping each one against a fixed set of rules, and you can always ask
which model actually served a request and why. Nothing here reads your
prompt to decide where it goes. For how to point conway at the providers
these chains reference, see [`providers.md`](providers.md).

**The capability filtering, health tracking, and circuit breaking this page
mostly describes are an installable first-party plugin, not something
`conway` ships built in**. Add
`"conway.routing"` to `plugins.install`:

```json
// .conway/settings.json
{ "plugins": { "install": ["conway.routing"] } }
```

Absent that entry, `conway` still resolves every role to a model and
completes a turn — using `conway_core::routing::MinimalRouter`, an honest,
config-only resolver that walks `roles.<alias>.chain` in order with **no**
capability filtering, health filtering, or circuit breaking at all: every
candidate is treated as eligible, so an unregistered model or a dead
endpoint is only discovered when the request itself is actually attempted,
not skipped in advance. Everything from ["Capability matching"](#capability-matching)
onward on this page describes the INSTALLED behavior; see ["Installing a
different router"](#installing-a-different-router) for exactly what
`MinimalRouter` does and does not do.

## Roles and fallback chains

A role is a named alias — `default_role`, `--role-override`, or an
agent definition's own role — resolved to a `roles.<alias>` chain in
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

**The backend id `local` above is just a name the operator chose — it
carries no meaning to any code, same as `coder` or `anthropic`.** Whether a
candidate's inference actually stays on this machine is a separate,
*typed* property, `backends.<id>.local` (a bool, default `false`), declared
on the backend entry itself — see [providers.md's "Locality"
section](providers.md#locality) for the field, its exact predicate, and the
case (an SSH tunnel) it cannot see through either way. Nothing on this page
changes because of that field: routing still walks the chain in the order
written, with no preference for, or skipping of, a non-local candidate —
whether a role's *entire* chain is local is a question a caller asks
explicitly (`conway::config::role_is_local`), not something the router
enforces on its own.

Two flags override chain resolution for a single run:

| Flag | Effect |
| --- | --- |
| `--role-override <role>` | Use this role instead of `default_role`. |
| `--model <backend/model>` | Pin a specific model, bypassing the chain entirely (`RoutingReason::PinnedByApi`). |

### Viewing and changing the default from the TUI

The interactive TUI's `/settings` menu has a **defaults** section showing
`default_role` and, beside it, the **default model** — the head of that
role's own `chain`, labelled as a default. The default model is a
computed display, not a separate setting: there is no `default_model` key
anywhere in `settings.json`, on purpose (one source of truth for "which
model" — this page's own `chain`). Changing which model a *new* session
starts on means changing `default_role` (the settings menu's `default
role` row cycles through your configured roles and writes it) or that
role's own `chain` (a `settings.json` edit, same as any other role
config). `/model` and `/role` are unrelated commands, both top-level and
both session-scoped: they change what the *current* session is using
right now, never `settings.json`.

## Installing a different router

**Absent, by default: `conway_core::routing::MinimalRouter`.** `build()`
compiles this whenever nothing else is installed or injected — it needs
nothing but `roles`, walks a role's configured chain in order, and
performs **no** capability filtering, **no** health filtering, and **no**
circuit breaking. `--model`/pin still works (`RoutingReason::PinnedByApi`),
and every candidate still carries a `RoutingReason` (`conway routes explain`
still works, just with a degenerate answer — see below), but nothing about
`.conway/models.json`, a role's capability floor, or endpoint health has any
effect on routing in this configuration: an unregistered model or a dead
endpoint is only discovered when conway actually tries the request, not
skipped in advance the way the rest of this page describes.

**Installed: `conway-plugin-routing`'s `DeclarativeRouter`.** This is the
engine `conway` itself used to compile in unconditionally before this item — capability matching, headroom, health, and
circuit breaking, everything else on this page — now an installable
first-party plugin (see the note at the top of this page for the
`plugins.install` entry). Two ways to install it:

- **By name, in `settings.json`**: `{ "plugins": { "install":
  ["conway.routing"] } }`, resolved by whatever binary links the plugin
  crate (`conway-cli` does, for the TUI and one-shot `-p` alike) against its
  own `RouterFactory::id()` — `conway-plugin-routing::ROUTER_ID`,
  `"conway.routing"`.
- **By an embedder, directly**: `ConwayBuilder::with_router_factory(Arc::new(
  conway_plugin_routing::RoutingRouterFactory))`, the library-embedder shape
  of the SAME mechanism `plugins.install` resolves for a binary — a
  `conway::RouterFactory` names a router *kind* up front (an id plus a
  deferred, fallible `build` step) since `plugins.install` is read long
  before backends exist to build a real router against. See
  [`embedding.md`](embedding.md#installing-a-router-routerfactory-and-the-pluginsinstall-router-arm)
  for the full `RouterFactory` shape.

**A third option overrides routing entirely, bypassing both of the above**:
`ConwayBuilder::with_router(router)` hands `build()` an already-constructed
`Router` directly. This is unconditional: an injected router is never
wrapped, inspected, or validated, and wins over an installed/registered
router factory, which is then never even invoked.

Which of the three is taken changes what `conway routes explain` can show: a
router that is not `conway-plugin-routing`'s own `DeclarativeRouter` (the
absent-plugin `MinimalRouter` default, an injected `with_router`, or a
`RouterFactory` whose `RouterBundle::explain` came back `None`) falls back to
an honest, degenerate answer — no capability filtering, no health filtering,
one entry per configured chain candidate — rather than reporting an empty,
fabricated one. A `RouterFactory` that wants the richer explain answer
supplies its own `RoutingExplainer` as `RouterBundle::explain` at the same
moment it builds the router, since construction is the only point a router
and a matching explainer are guaranteed to agree with each other about
"why".

## Asking why a route was chosen

`conway routes explain <role>` is the direct answer to "which model
served this, and why":

```console
conway routes explain coder
```

```text
role: coder  (est_tokens=0, headroom_tokens=4096)
params: default
  [0] anthropic/claude-sonnet-4-6  SELECTED primary for role `coder`  (breaker: closed, tokens: heuristic, window: 200000 [verified], cache: reported, fixed_cost: 18900/200000 tokens (9%))
  [1] local/qwen3:4b               SELECTED fallback #1 after:   (breaker: closed, tokens: heuristic, window: 1048576 [models.json], cache: not reported, fixed_cost: 18900/1048576 tokens (1%))
```

Each `SELECTED`/`SKIPPED` line is real router output, one per chain
entry, in position order — not just the one that ran. A `SKIPPED` entry
names exactly what disqualified it:

```text
role: coder  (est_tokens=0, headroom_tokens=4096)
params: default
  [0] anthropic/claude-sonnet-4-6  SELECTED primary for role `coder`  (breaker: closed, tokens: heuristic, window: 200000 [verified], cache: reported, fixed_cost: 18900/200000 tokens (9%))
  [1] local/qwen3:4b               SKIPPED  skipped `local/qwen3:4b`: missing capabilities: unknown (backend, model) pair  (breaker: closed, tokens: unknown, window: not indexed [unknown], cache: unknown, fixed_cost: unknown)
```

`params:`, on its own line right after the header, is that role's
effective `[roles.<alias>.params]` — see ["Sampling and reasoning
params"](#sampling-and-reasoning-params) below for what it means and how
to set it. `default` means nothing is configured for this role (never an
empty line, so "nothing configured" reads as a distinct, deliberate
fact rather than a blank you might mistake for a rendering bug); otherwise
every `Some`/non-empty field renders `key=value`, comma-separated,
including `extra`'s own entries (`reasoning_budget_tokens=8000`). Unlike
every per-candidate column below, this line is read straight from your
`settings.json`, not from the `ExplainReport` the router produced — so it
shows a real value even under the `MinimalRouter` fallback (see below)
that leaves every per-candidate column `unknown`.

`fixed_cost:` is the same fixed, per-turn cost guided setup's own
runway preflight computes — the default opinion set's real tool-schema and
instruction-fragment tokens, plus a fixed per-server allowance for each
configured `[plugins].mcp[]` entry and a representative command-prompt
allowance — checked against THIS candidate's resolved window. The shape is
`<total>/<window> tokens (<pct>%)`, with `, over half the window` appended
when the total crosses half of `window` (the identical
`INSTALL_FOOTPRINT_WARN_FRACTION` threshold guided setup's own warning
uses). `unknown` — never a bogus percentage — is exactly the case
`window:` itself reports as `not indexed`: there is no window to compare
the fixed cost against. `--json` carries the identical string as each
chain entry's `"fixed_cost"` key.

`tokens:` is that candidate's backend's declared `Backend::token_fidelity`
(`exact` / `calibrated` / `heuristic`) — the operator-visible answer to "how
much should I trust this backend's own token estimate?" Both shipped
dialects declare `heuristic` honestly (neither vendors a tokenizer nor has a
measured calibration factor); `unknown` means the producing router could not
answer at all, not that the answer was bad — see the `MinimalRouter`
paragraph below. `--json` carries the identical value as each chain entry's
`"token_fidelity"` key.

`window:` is that candidate's resolved `max_context_tokens`, and the
bracketed tag beside it is where that number actually came from
(`ContextTokensSource`, `conway-core`'s `capabilities` module) — never a
bare `unknown` for a candidate this report actually indexed:

- `verified` — a compiled-in, sourced metadata table entry, or a dialect's
  own documented per-provider figure (Anthropic's 200,000, `"openai"`'s
  128,000).
- `models.json` — an operator-editable override: a `.conway/models.json`
  entry, hand-edited or written by conway's own discover-or-ask setup flow
  (see [`docs/providers.md`'s "Establishing the window at
  setup"](providers.md#establishing-the-window-at-setup)).
- `probed` — a live discovery result for this exact model (a
  `probe_on_startup` capability probe, or the setup-time discover step).
- `floor (assumed)` — this dialect's own baseline governs, and that
  baseline is not a sourced fact about any real model of this provider
  (`conway-plugin-backends`'s built-in `"32768"` floor for most dialects) —
  see [`docs/providers.md`'s "Where a context ceiling comes
  from"](providers.md#where-a-context-ceiling-comes-from) for the full
  precedence chain this label reflects.
- `unknown` — this candidate has no capability-index entry at all (an
  undeclared model, or the `MinimalRouter` fallback below); `window:` itself
  reads `not indexed` in this case.

`--json` carries the same two facts per entry as `"context_window_tokens"`
(a number or `null`) and `"context_window_source"` (one of the five strings
above).

`cache:` is that candidate's backend's declared `Backend::cache_reporting`
(board item A5.7, "prompt caching reads zero on every real session") --
whether this backend's wire dialect has ANYWHERE to say "the cache was
hit" at all, answered before a single request is sent. `reported`
(Anthropic; the `openai`/`kimi` `openai-compat` profiles) means this
backend's decoder reads a real cache-usage field whenever the wire response
carries one; `not reported` (every other built-in `openai-compat` profile,
including `ollama`) means either the wire dialect carries no such field at
all (Ollama's native `/api/chat`), or its reporting behavior has not been
verified against real documentation or a real response -- see
[`docs/providers.md`'s "Does Ollama Cloud actually cache
prefixes?"](providers.md#does-ollama-cloud-actually-cache-prefixes) for the
disclosed gap this second case names rather than guesses at; `unknown`
means the producing router could not answer at all (the `MinimalRouter`
fallback below), the identical shape `tokens:`/`window:` already use for
the same reason. **This is a different fact from the per-response
`Usage::cache_accounting`** the status line and turn-end summary render
(see ["Prompt caching: economics, not
correctness"](#prompt-caching-economics-not-correctness) below): that one
is discovered only after a request was sent and answers "did THIS response
say anything"; `cache_reporting` is a declared, ahead-of-time prediction
answering "will ANY response from this backend ever have anywhere to say
so." `--json` carries the identical value as each chain entry's
`"cache_reporting"` key (`"reported"`/`"not reported"`/`"unknown"`).

`--json` renders the same report machine-readably: an object with a
top-level `"params"` string (the `params:` line above) alongside `"chain"`,
`"skipped"`, and `"health"`, each chain/skipped entry additionally carrying
`"fixed_cost"` (the `fixed_cost:` column above). Four things worth
knowing about what this command actually evaluates (the first three
verified against `conway::Conway::explain_routing`; the fourth is NOT part
of that method's own output at all):

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
  `roles.<alias>`'s own configured fields, not just a caller-supplied
  requirement.
- `params:` and `fixed_cost:` are the `conway routes explain` CLI command's
  own additions, layered on top of `ExplainReport` rather than fields on
  it: `params` is read straight off `ConwayConfig::roles` (see above), and
  `fixed_cost` is computed the same way guided setup's own runway preflight
  is (`crate::first_run::default_opinion_set_footprint` /
  `runway_fixed_cost_warning`, `conway-cli`-internal, reused rather than
  re-derived) against each candidate's resolved window. Neither widens
  `ExplainEntry`'s or `ExplainReport`'s own wire shape, so an embedder
  reading `Conway::explain_routing` directly never sees them — only this
  CLI command's own text/`--json` output does.

For the routing decision an actual turn just made, the TUI's `/why`
command shows a short session HISTORY of live `Event::ModelDecision`s for
the focused agent instead (see [`interactive.md`](interactive.md) and
["What you see when a route is skipped"](#what-you-see-when-a-route-is-skipped)
below) — those carry the turn's real `est_tokens`, and a decision's own
`Fallback::after` (when non-empty) names what it skipped past.

**Where the report type lives, and what happens with a non-default
router.** `ExplainReport` (and the field types it's built from --
`ExplainEntry`, `EntryOutcome`, `CapabilitySummary`, `BreakerSnapshot`) are
defined in `conway_core::routing`, not in `conway-plugin-routing` -- so producing
one never requires depending on `conway-plugin-routing`'s filtering logic.
`conway-plugin-routing::RoutingExplain` (the rich, capability- and health-filtered
answer this page's examples above show) is one producer; embedders that
supply their own `Router` via `ConwayBuilder::with_router` get a different
one automatically: `Conway::explain_routing` falls back to
`conway_core::routing::MinimalRouter`, projected over the same
`RoutingConfig` the embedder's `settings.json` declares. That fallback
report is honestly *degenerate*, not empty and not fabricated-rich: one
entry per configured chain candidate (position `0` `SELECTED`, the rest
`SKIPPED`), every `capabilities` field `None`, every `token_fidelity` field
`None` (rendered `tokens: unknown`), every `cache_reporting` field `None`
(rendered `cache: unknown`), and every `breaker` field
`Closed` -- because a `MinimalRouter` genuinely indexes no capabilities,
holds no `Arc<dyn Backend>` to ask about token fidelity or cache
reporting, and tracks no real breaker state, and inventing any of the four
would be claiming a capability the harness doesn't have. Critically, `conway routes explain` still
distinguishes "unknown role" from "configured role, empty report" in this
configuration: it checks `roles` directly against your configuration,
not whether the report came back with zero entries -- a configured role
whose chain happens to be empty gets an honest, entry-less report rather
than being misreported as "unknown".

## Capability matching

Each chain candidate's `Capabilities` are resolved with a fixed
precedence, config closest to you winning: **your `models.json` entry
> a live startup probe (`models.metadata_path` with
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
set directly in `settings.json` — `roles.<alias>` carries
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
two demands more wins; neither side can weaken the other. `conway-plugin-routing`'s
`satisfies` still walks all seven `RequiredCaps` fields against that merged
result, headroom last, exactly as before; a candidate that fails one shows
up as an ordinary `RoutingReason::CapabilitySkip` / `context: ...`-style
entry, e.g. `reliability_tier: requires Verified, has Community`.

## Sampling and reasoning params

`roles.<alias>.params` is a different knob than everything in ["Capability
matching"](#capability-matching) above: those six fields (`tool_calling`,
`structured_output`, `parallel_tool_calls`, `reasoning`, `min_reliability`,
`min_context`) are a FLOOR a candidate model must clear before it is routed
to at all. `params` is what conway actually SENDS once a candidate is
chosen — temperature, top-p, a token cap, stop sequences, a sampling seed,
and a free `extra` map for whatever provider-specific key that request
needs, e.g. Anthropic's extended-thinking token budget or an
OpenAI-compatible server's `reasoning_effort`:

```json
// .conway/settings.json
{
  "roles": {
    "thinking": {
      "chain": ["anthropic/claude-sonnet-4-6"],
      "params": {
        "extra": { "reasoning_budget_tokens": 8000 }
      }
    },
    "fast": {
      "chain": ["anthropic/claude-haiku-4-5"],
      "params": {
        "temperature": 0
      }
    }
  }
}
```

`thinking` turns a hard problem's effort up: `extra.reasoning_budget_tokens`
reaches the Anthropic Messages API's `thinking: {type: "enabled",
budget_tokens: ...}` field verbatim (see `conway-plugin-backends`'
`anthropic::wire` module). `fast` turns a mechanical one's effort down —
a smaller model, and `temperature: 0` for deterministic output — the exact
inverse case. Switch between them with `/role`, the same top-level,
session-scoped command ["Viewing and changing the default from the
TUI"](#viewing-and-changing-the-default-from-the-tui) already covers for
`default_role`: `/role thinking` for the next hard problem, `/role fast`
once it's mechanical again. There is no separate `/effort` command and no
harness-level effort enum — the role IS the effort switch, because a
reasoning budget is provider-shaped (Anthropic's token budget and an
OpenAI-compatible server's `reasoning_effort` string are not the same
value on the same scale), and a role's routing config is already where
every other provider-shaped setting for that chain lives.

`ConwayConfig::routing()` is the one place `roles.<alias>.params` is
resolved into `conway_core::routing::RoleConfig::params` — every backend
adapter reads the resolved value off its request; none re-derives it from
`settings.json`. An `extra` key a backend's own wire layer does not
recognize is passed through untouched (no validation at this layer; the
provider's own request either accepts or rejects it, exactly as if you'd
sent it by hand). A TYPED field a backend has no equivalent for — today,
`seed`, on both shipped adapters' primary request path — logs one
`tracing::warn!` naming the field and the backend the first time that
request is built, rather than silently doing nothing: a setting that has
no effect and no explanation is exactly the kind of gap this project tries
not to leave standing.

To see what a role's effective `params` actually resolved to without
sending a real request, `conway routes explain <role>` prints it on its own
`params:` line (`--json`'s `"params"` key) — see ["Asking why a route was
chosen"](#asking-why-a-route-was-chosen) above.

## Narrowing a role's own tool set

`roles.<alias>.tools` is a different question again from both "Capability
matching" and "Sampling and reasoning params" above: not "which model", not
"what to send it", but "which tools that model ever sees at all" — an
allow/deny filter over the announced tool set, evaluated once, at spawn
time, for every agent routed through this role:

```json
// .conway/settings.json
{
  "roles": {
    "reviewer": {
      "chain": ["anthropic/claude-sonnet-4-6"],
      "tools": {
        "exclude": ["mcp_*"]
      }
    }
  }
}
```

Both `include` and `exclude` are glob patterns in the same vocabulary an
`AgentDef`'s own frontmatter `tools:` list already uses — an exact tool
name, or a trailing `*` for a prefix match. Absent (the default for every
role that names no `tools` table at all) narrows nothing: an agent routed
through such a role announces exactly what its own `agent_def`/call-site
`tools` override would already narrow it to, unchanged. When both `include`
and `exclude` are set, `include` narrows to an allowlist first and
`exclude` then removes any of its own matches from that allowlist —
`exclude` always wins a name matched by both.

This narrows what a root or a fork/spawn child ever ANNOUNCES, on top of
whatever its own `agent_def.tools`/call-site `tools` override already
narrows it to — the two compose (an AND, not an override): a role's own
`tools` table can only ever shrink what an agent would otherwise see, never
widen it. A forked or spawned child routed through a narrowing role gets
the identical narrowing a root started with that same role would.

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
`routing.default_headroom_tokens`. Precedence is per-role override >
global default > conway's own built-in constant (`8192`) if you set
neither. Two env vars reach the same knobs without touching the file:
`CONWAY_ROUTING__DEFAULT_HEADROOM_TOKENS=16000` and
`CONWAY_ROLES__<ALIAS>__HEADROOM_TOKENS=32768` (the latter only applies
to a role that already exists in the merged config; an unknown alias is
ignored, not an error). There's no `--headroom-tokens` CLI flag — only
`settings.json` and these two env vars reach it.

### Adaptive headroom (on by default)

The one thing the precedence chain above does not fix on its own: a flat
`headroom_tokens` reserves a wildly different SHARE of the window
depending which model ends up serving the role. `8192` (conway's own
built-in default) is 25% of a 32K-window model but under 1% of a
976K-window one — either wasteful (a huge model starved of prompt room by
comparison) or dangerously thin (a small model's headroom barely denting
its window at all), depending which end of that range a role's chain
happens to resolve to.

`routing.headroom_fraction` fixes this by making the effective headroom a
FRACTION of the role's own smallest reachable window, computed once at
config-load time, rather than a fixed number: `max(smallest_window / d,
2048)` (`HEADROOM_FLOOR`, disclosed so a tiny window never gets starved
down to something unusably small). Default `d = 10` (10% of the window) —
`headroom_fraction` is set unless you say otherwise, so an ordinary
config with no `[routing]` section at all already gets this. A role whose
chain resolves nothing but small windows gets a small, still-proportional
headroom; a role that can reach a 1M-token model gets a generous one, with
no operator tuning required:

```json
// what an operator sees with NO [routing] section at all
{
  "roles": {
    "coder": { "chain": ["ollama-cloud/glm-5.2"] }
  }
}
```

resolves `coder`'s headroom to `max(32768 / 10, 2048) = 3276` — not the
flat `8192` a config predating this feature would have used (which, at
25% of that same window, is exactly the ratio the walked scenario below
names as the proximate trigger of a real session's context rejection).

**Precedence, unchanged in shape, one link added:** an explicit per-role
`headroom_tokens` always wins (over both the fraction and the flat
default); absent that, `headroom_fraction` (when nonzero) computes the
adaptive value; absent BOTH a model window and an override, the flat
`routing.default_headroom_tokens` is the fallback — "the fixed default is
the fallback only when no model window is known," not the everyday case
it used to be. Set `"routing": { "headroom_fraction": 0 }` to opt back
out entirely (every role falls back to the flat
`default_headroom_tokens`, exactly the pre-adaptive behavior); a fraction
this codebase computes is never silently clamped or overridden at
runtime — the same "not clamped" guarantee the warning below documents
applies here too, just one step earlier (the VALUE that lands in
`headroom_tokens` is adaptively chosen; once resolved, it is as fixed as
if you had typed it yourself).

### A headroom that already exceeds a model's window

Config loading catches one shape of this mistake before you ever hit
`ContextTooLarge`: a role's effective headroom (per-role override, or the
global default) that is `>=` the *smallest* context window reachable
through its own chain. That role's requests would be rejected by the
context-window gate before a single token of real content is ever
counted — so this is surfaced as a startup warning, not left for you to
discover mid-session:

```
conway: warning: headroom for role 'coder' is 200000 tokens, which is not
less than the smallest context window in its chain
(anthropic/claude-haiku-4-5 = 32768 tokens); every request routed to that
model will be rejected by the context-window gate
```

The CLI prints every such warning to stderr at startup, for every
subcommand (`sessions`, `routes`, one-shot `-p`) as well as the
interactive TUI's launch. The TUI additionally puts each warning in the
transcript (as a non-fatal error entry) so it stays visible once the
alternate screen takes over and the startup stderr line has scrolled out
of view. Embedding conway as a library gets the same data through
`Conway::warnings()` — nothing renders it on your behalf, so an embedder
is responsible for surfacing it (or deliberately choosing not to).

This warning does not clamp or reject the config — the value you set
survives unmodified, and the role still routes exactly as configured.
Fix it by raising the model's declared `max_context_tokens` in
`models.json` (if you under-declared it), lowering the role's
`headroom_tokens`, or adding a larger-window candidate to the chain.

### A headroom that consumes most of a model's window, without exceeding it

A second, milder warning catches the shape that actually broke a real
walked session (board item `01M1AVZPTRSWVE33G4DTJY7Q1B`): headroom that
reaches `>= 25%` of the smallest reachable window, but not `>=` it
outright. `8192` (conway's own built-in default) against a
`32768`-token window is exactly `25%`; the check above never fires for that
pair (`8192 < 32768`), so a role could sit at that ratio indefinitely with
no warning at all, right up until a normal-sized prompt to that model hit
`ContextTooLarge` mid-session:

```
conway: warning: routing.default_headroom_tokens is 8192 tokens (25% of the
smallest context window in its chain, ollama-cloud/glm-5.2 = 32768 tokens);
a long-running conversation to that model can hit the context-window gate
well before it would with a smaller reservation -- consider a smaller
headroom_tokens for this role, or a larger-window fallback later in its
chain
```

Same non-clamping guarantee as the literal-exceeds warning above: this
names the ratio and suggests the fix, but the configured value is never
touched. `WarningCode::HeadroomConsumesLargeFractionOfContext` on the same
`Conway::warnings()`/CLI-stderr/TUI-transcript surface `HeadroomExceedsContext`
already uses.

### Estimated, not exact

`est_tokens` is a heuristic, never a real tokenizer count — conway's
context builder names it explicitly (`TOKEN_ESTIMATOR =
"heuristic-chars4"`): each content block contributes `ceil(chars / 4) +
4` tokens (the `+4` standing in for wire-format framing), summed across
the assembled prompt, **plus a dedicated term for the tool schemas
themselves** — approximated from the tool set directly, not from any
segment's content (see the next section for why). This is deliberately
conservative, not precise — don't present a routing decision as if the
token figure gating it were exact. When a candidate is rejected on
context, the message says so in full:

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
context rejected: 34000 prompt + 16000 reserved output = 50000 tokens, but ollama-cloud/glm-5.2 accepts at most 40000 (short by 10000); no truncation or escalation is performed -- to admit this request, lower role planner's headroom_tokens, add a larger-window model later in its fallback chain, or shorten the prompt
```

This is `RoutingError::ContextTooLarge`: it names the input size, the
resolved headroom, and the *largest* window among the candidates that
still didn't fit (so a chain with several too-small models reports its
best case, not an arbitrary one). No truncation or escalation ever
happens on your behalf — this is terminal by design; the trailing clause
(board item `01M1AVZPTRSWVE33G4DTJY7Q1B`) names the operator's actual next
move so the message doubles as the fix, not just the diagnosis: shrink the
turn's content, raise the role's headroom budget, or add a larger-window
candidate to the chain.

**This is a real dead end, not merely an unhelpfully-worded one, when it
fires.** Nothing conway does today shrinks, summarizes, or otherwise
edits an oversized turn on your behalf — the closest thing to that is
[`ContextHook::on_overflow`](plugins/hooks.md), an extension point
`AgentLoop` already retries against (up to `MAX_OVERFLOW_ATTEMPTS = 2`
times) whenever this exact rejection fires, but which no first-party
plugin implements as of this writing; see that doc's `on_overflow` row for
the mechanism a compaction/trim plugin would hook. `conway.trim`
([`plugins/README.md`](plugins/README.md)) addresses a related but
different problem — it drops old tool round-trips proactively, on a fixed
turn window, whether or not a turn is close to overflowing — not this
message's trigger directly.

Every candidate a role's chain configures IS already tried before this
message fires, in order — a too-small candidate is skipped (never
dialed), and the chain falls through to the next one; see ["Advisory vs.
authoritative"](#advisory-vs-authoritative-two-context-checks-not-one)
below for exactly where that happens. `ContextTooLarge` is what you see
only once every configured candidate has been tried and every one of them
failed on context alone — the fix that actually helps most often is a
bigger `chain`, not a smaller prompt.

### What conway does as a window fills, and what it deliberately does not (yet)

Board item `01M1AVZPTRSWVE33G4DTJY7Q1B`'s own framing named four strategies
and asked for a ranked decision, not just a fix. Recorded here rather than
only in a completion report, so the ranking outlives the session that made
it. **Updated by the operator's 2026-09-01 ruling** (superseding an earlier,
partial pass on this same item that had left the fourth strategy as a
follow-up): the ruling reframed "cap or elide oversized tool results" as
"refuse to admit it, with a note" — not silent truncation — and asked for it
built, not deferred. That is items 1, 2, and 4 below, in their current,
actually-built shape:

1. **Escalate along the configured fallback chain — built, and already
   there before this item.** Reading `DeclarativeRouter::resolve`
   (`conway-plugin-routing`) and `AttemptEngine::execute`
   (`conway-runtime`) found both already try every chain candidate in
   order and fall through past one that is too small — `ContextTooLarge`
   only fires once ALL of them have failed. This item added no code for
   it, only a regression test proving it reaches the real seam (see that
   test's own doc for exactly which layer does the skipping). **Cost:**
   none by itself — it is a config authoring discipline (put a
   larger-window model later in the chain), not a mechanism to build.
   **Limit:** useless when no configured candidate has a bigger window,
   which is exactly the shape this item's own framing calls out ("what
   happens when a prompt genuinely does not fit a genuinely correct
   ceiling").
2. **A better DEFAULT headroom, not just a warning about a bad one —
   built.** The proactive `HeadroomConsumesLargeFractionOfContext` warning
   (`>= 25%` of a role's smallest reachable window) shipped first, as a
   milder companion to the pre-existing `HeadroomExceedsContext` check —
   see ["A headroom that consumes most of a model's
   window"](#a-headroom-that-consumes-most-of-a-models-window-without-exceeding-it)
   below. This item's finishing pass went further: `routing.headroom_fraction`
   (["Adaptive headroom"](#adaptive-headroom-on-by-default) above) is now ON
   BY DEFAULT (`d = 10`, computed once at config-load time from each role's
   own smallest reachable window), so the flat `8192`-against-a-32K-window
   shape that triggered the warning in the first place — the exact scenario
   this item's own walked session hit — no longer arises from an unconfigured
   `[routing]` section at all. **Cost:** none to the admission check itself
   (still computed once, at config-load time, never per-request) — an
   operator who explicitly set a flat `headroom_tokens` (per-role) or
   disabled the fraction (`headroom_fraction: 0`) is untouched either way.
3. **Silently auto-shrinking headroom PER CANDIDATE, at request time
   (runtime adaptive headroom) — considered, rejected. Not what item 2
   built.** Do not confuse the two: item 2's adaptive DEFAULT is resolved
   once, from config, before any request exists, and the resulting number
   is then exactly as fixed as if an operator had typed it — this item
   (per-request, per-candidate clamping AFTER a prompt is already
   assembled) is a genuinely different mechanism, and stays rejected for
   the reasons already given: (a) it would reverse an EXISTING,
   deliberately-tested decision — a test named
   `headroom_exceeding_smallest_reachable_context_warns_without_clamping`
   (`crates/conway/tests/config_headroom.rs`) already pins "not clamped:
   the configured value survives unmodified" as the answer to this exact
   class of problem, and reversing THAT without being asked is a product
   decision this item does not own; (b) headroom exists specifically to
   avoid a WORSE failure mode than a pre-flight rejection — trimming it
   trades a safe `ContextTooLarge` (paying nothing) for a live
   mid-generation overflow (paying for tokens already generated) if the
   estimate that shrank it was even slightly optimistic. Silently making
   that trade on an operator's behalf is the thing this item's own
   constraints forbid ("do not weaken the admission check to make this go
   away").
4. **A tool-result admission gate: refuse to render an oversized result,
   with a note — built** (the operator's 2026-09-01 ruling; supersedes an
   earlier pass's "left as a follow-up" on this same line). A single tool
   result larger than a configured bound (`routing.tool_result_bound_fraction`
   / `tool_result_bound_cap_tokens`, default ON — 20% of a role's smallest
   reachable window, capped at 8192 tokens) is not rendered into context at
   all: the model sees a short note in its place, naming the result's size,
   where the full result is kept (the session's own durable log), and two
   concrete remedies (re-invoke the tool narrower, or fork a child to
   distill it) — see `ContextBuilder`'s `admit_tool_result`
   (`conway-runtime`'s `context::builder`) and
   `ContextReport::not_admitted`. **Deliberately NOT truncation, at this
   seam**: the choice is binary (the whole result as this gate receives
   it, or a note), never a lossy middle. This is distinct from — and does
   not touch — `conway_runtime::tools::runner::apply_truncation`, a
   separate, pre-existing, ALREADY-ACTIVE mechanism that caps a few
   built-in tools' own raw output at a fixed byte budget right after the
   tool runs (`read`'s 65536-byte head, `grep`'s 32768-byte head, `bash`'s
   head+tail) — unconditional and model-agnostic, there to bound one
   absurdly large tool invocation, never to fit a specific model's window.
   The two compose: a tool-truncated 65536-byte result can still exceed a
   small role's small admission bound, so this gate still applies to it.
   Applies identically to a fork child's INHERITED tool results, not only a
   session's own, so "forking is cheap" stays true on a non-caching
   provider. **Cost:** none to ordinary results — the gate is a size check
   against already-assembled content, never re-fetches or re-renders
   anything, and a result under the bound is untouched.
5. **Full conversation compaction — the largest piece, genuinely deferred,
   and further along than it looks.** `PHILOSOPHY.md`'s own "Where the
   tree is today" note already says it plainly: the `ContextHook` port
   this would rest on is built and does everything a compaction plugin
   would need — `AgentLoop` already retries `ContextHook::on_overflow` up
   to `MAX_OVERFLOW_ATTEMPTS` times on exactly this rejection (see
   [`plugins/hooks.md`](plugins/hooks.md)) — but no first-party plugin
   implements it, and this item's own operator ruling reaffirmed that gap
   as deliberate: no default curator, `conway.trim` stays opt-in, the
   overflow seam stays empty by default. Building a compaction plugin is a
   genuinely separate, large piece of work (what gets summarized, by which
   model, and how the operator is told) that does not fit this item's own
   appetite alongside the other four — an operator who wants it installs a
   plugin; the core never defaults into it.

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

One circuit breaker exists per backend (its `EndpointId`, 1:1 with the
backend id — every model on the same backend shares one breaker): a
**Transport** breaker fed by real request failures, tuned by `[health]`:

```json
// .conway/settings.json
{
  "health": {
    "transport_failures_to_open": 3,
    "open_duration_secs": 30,
    "half_open_successes_to_close": 1
  }
}
```

Every field above is the built-in default. The breaker is `Closed` (used
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
explain <role>` shows the breaker's current state, and a candidate
skipped for `HealthSkip` names it and until when.

**A dead endpoint recovering is still detected without any periodic
probing.** A crashed backend that comes back up is caught by the next
real request: the breaker's `HalfOpen` state is derived from the clock at
read time (no background task needed), and the router admits a
half-open candidate exactly like a closed one — so the very next request
against that role naturally retries it. `conway-plugin-routing` used to
also carry a periodic `HealthProber` that fed a second, independent
`Probe` breaker from liveness checks decoupled from request traffic; it
was retired rather than wired
because it had no production call site anywhere in the tree, and the only
thing it would have bought — shaving one failed round trip off recovery
for a sparse-traffic role — is a latency optimization this project gates
on a measured baseline that neither existed nor was scheduled. The
`probe_enabled`/`probe_interval_secs`/`probe_timeout_secs`/
`probe_failures_to_open` keys that used to configure it are gone; a
`settings.json` naming any of them under `[health]` now fails to load,
naming the offending key.

(Do not confuse the retired periodic health prober with the *startup*
`models.probe_on_startup` capability probe covered above under
"Capability matching" — same word, two unrelated mechanisms: that one
discovers model capabilities once at startup and is wired; the health
prober would have fed an ongoing liveness signal and no longer exists at
all.)

### What you see when a route is skipped

Board item A1d ("say why a turn fell back") closed a real gap here: a
skip that happened mid-turn used to advance to the next chain candidate
completely silently — `route_reason.after` (the field below) recorded
nothing, and the TUI showed no notice at all beyond the eventual
successful model's name. It now names both WHAT was skipped and WHY,
with numbers.

**`route_reason` (persisted per turn).** Every assistant turn's log
record carries a `route_reason` naming the model AND the reason it was
chosen. When that reason is `Fallback`, its `after` field lists every
earlier-in-chain candidate this turn's routing actually passed over
before reaching the one that ran — each entry naming the model and the
exact reason, reusing the identical vocabulary `conway routes explain`
already renders (a capability floor, a headroom shortfall, a breaker
open, or an admission refusal discovered once the real request was
built). Two distinct sources feed it, both surfaced the same way:
candidates the ROUTER itself pre-filtered before a single backend call
was made, and candidates that passed routing but were then refused by
`Backend::admit`'s own authoritative check over the actually-built
request. `after` is `[]` exactly when nothing was skipped to reach this
candidate (an ordinary primary selection, or a pin) — never a
placeholder for "not implemented."

**In the TUI.** A turn that routed past a named candidate shows a
one-line dim notice naming it, e.g.:

```text
routed to local/qwen3-coder-80b — anthropic/claude-sonnet-4-6 skipped: context too large: 24614 input tokens + 8192 headroom = 32806 exceeds anthropic/claude-sonnet-4-6's window of 32768 tokens (short by 38); not trimmed or escalated
```

**`/why`.** The interactive `/why` command now keeps a short, bounded
session HISTORY of routing decisions (INTENT.md §5c: "changing model
mid-session is ordinary") rather than only the latest one — three
consecutive `/model` switches, or a session that genuinely flip-flopped
between a primary and a fallback several times, are all individually
recoverable, not collapsed down to the newest.

**A breaker actually opening is still separately visible, unchanged.**
The moment a breaker *opens* (as opposed to an ordinary per-turn skip
that never trips one) remains its own live signal: a `BackendDegraded`
event fires, which the TUI renders as a transcript notice and one-shot
mode prints to stderr as `backend degraded: <endpoint>`. A captured
example, chain `[anthropic, local]` with the first candidate
unreachable:

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
  (`conway-plugin-backends/tests/anthropic_cache_mapping.rs`) pins this: strip
  every cache hint and the body is identical except for the absence of
  `cache_control` keys — nothing else about the request changes.
- **OpenAI-compatible providers** (Ollama, vLLM, Kimi's platform API, and
  others) cache implicitly on prefix match — there's no request field to
  set at all. `cache_hint_never_changes_the_serialized_request_body`
  (`conway-plugin-backends/src/openai_compat/wire.rs`) pins the stronger claim:
  this adapter never reads a cache hint in the first place, so a marked
  segment and an unmarked one serialize identically. This describes the
  *mechanism* conway assumes (no request-side hint, so a prefix either
  matches byte-for-byte or it doesn't) — it is not itself a claim that
  every dialect's live server actually runs a caching layer behind that
  mechanism. Kimi's platform API documents that it does; whether Ollama
  Cloud's hosted deployment does is unconfirmed — see
  [`providers.md`'s "Does Ollama Cloud actually cache
  prefixes?"](providers.md#does-ollama-cloud-actually-cache-prefixes) for
  what's established versus assumed there.

A profile's `cache` field (see [`providers.md`](providers.md)) is
informational for exactly this reason — it tells `conway-runtime`
whether it's worth marking a hint at all, never how a request is built.

**The precondition every cache depends on, proven directly (board item
A5.7).** Byte-for-byte identity across whole segments is necessary but not
sufficient: a real, growing conversation adds a new turn to the END of the
prompt on every step, and if anything upstream of the wire layer re-derives
or reorders an EARLIER segment, the shared leading run a provider's cache
lookup depends on breaks silently, with no error and no visible sign —
just a `0%`/`not reported` figure nobody can explain.
`two_conversations_differing_only_in_the_final_user_turn_render_byte_identical_up_to_that_turn`
(both `conway-plugin-backends/src/anthropic/wire.rs` and its
`openai_compat/wire.rs` counterpart) renders the SAME conversation twice
with only the last user turn's content changed, and asserts every message
strictly before that turn is byte-identical between the two renders — the
literal operationalization of "churn at the front breaks caching." Each has
a companion test proving the comparison technique is discriminating rather
than vacuous: changing the FIRST user turn instead of the last must (and
does) make the same prefix comparison fail.
`a_second_turn_over_the_cached_prefix_reports_a_real_hit_and_the_first_turn_placed_the_breakpoint`
(`anthropic_cache_mapping.rs`) closes the loop end to end against wiremock
fixtures: a first turn places the Anthropic breakpoint on a static prefix,
and a second turn reusing that exact prefix — replayed against a response
reporting `cache_read_input_tokens > 0` — both decodes into a nonzero
`Usage::cache_read_tokens` AND still carries the identical breakpoint on
the identical prefix. No live API call gates any of this.

### Tool schemas are sent once, not twice

Every request needs the model's tool schemas — every backend sends them
as the native `tools` array. Earlier, conway *also* rendered the same
schemas into a system-prompt segment purely so it would have something
to anchor a cache breakpoint to and hash for the prefix key, duplicating
every tool's name/description/JSON-Schema on **every turn**, and — since
a fork inherits its parent's whole transcript — at every fork depth, for
every sibling. Measured against conway's own 14-tool built-in set, that
duplicate text ran to roughly 3.4K estimated tokens: often larger than
everything else in a single turn.

Anthropic's Messages API documents `cache_control` as a marker you can
place directly on the *last* entry of the `tools` array itself ("Tool
definitions can be cached by placing `cache_control` on the last tool in
your `tools` array. All tools defined before and including that tool are
cached as a single prefix." — Anthropic's "Prompt caching" docs, "Caching
tool definitions"), so the duplicate segment was unnecessary: breakpoint
A now anchors on the native `tools` array directly, and the system
segment that used to hold a second copy of every schema sends no text at
all. `conway-plugin-backends`'s `anthropic::wire::BreakpointTarget::Tools`
carries that cache hint from the segment to `body["tools"]`'s last
element; OpenAI-compatible dialects have no `cache_control` equivalent to
redirect to, so for them it is a pure size reduction — one fewer system
message, nothing else changes.

The segment itself still exists (empty) and is still the source of the
tool-set identity hash that feeds `PrefixKey` and the estimator's
tool-schema term described above — only its wire *text* is gone.

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
      "base_url": "http://localhost:11434/v1",
      "local": true
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

The backend id `"local"` and the field `"local": true` are two unrelated
things that happen to share a spelling — the id is a name this example
chose, the field is what makes `backends.local` actually checkable as
local (see [providers.md's "Locality" section](providers.md#locality)).
Nothing about this config's routing behaviour changes because that field
is set: `coder`'s chain still tries Anthropic first, same as before. What
the field enables is a question this exact config *cannot* answer yet on
its own — `conway::config::role_is_local(&config, &RoleAlias::new("coder"))`
would return `false` for this `coder` role specifically, because
`anthropic/claude-sonnet-4-6` (correctly) has no `local` key at all: a role
falling through from a real local server to a cloud one is exactly the
mixed chain the query is designed to catch, not something this page's
example is claiming is "local" as a whole.

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
  [0] anthropic/claude-sonnet-4-6  SELECTED primary for role `coder`  (breaker: closed, tokens: heuristic, window: 200000 [verified])
  [1] local/qwen3:4b               SELECTED fallback #1 after:   (breaker: closed, tokens: heuristic, window: 32768 [models.json])
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
