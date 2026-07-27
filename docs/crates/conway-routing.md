# conway-routing

`conway-routing` implements the [`Router`/`HealthRegistry` ports](conway-core.md)
from `conway-core`: declarative role resolution, per-endpoint circuit
breakers, and background health probing. See
[`/ARCHITECTURE.md`](/ARCHITECTURE.md) for the whole-system picture.

## Responsibility and boundary

`conway-routing` owns four things:

- **`DeclarativeRouter`** (`router.rs`) — the `Router` implementation: pure,
  synchronous resolution of a `RoleAlias` to an ordered, capability- and
  health-filtered candidate list.
- **`BreakerRegistry`** (`breaker.rs`) — the `HealthRegistry`
  implementation: dual per-endpoint circuit breakers.
- **`HealthProber`** (`prober.rs`) — a periodic, per-endpoint background
  probe loop that keeps the Probe breaker fed independently of request
  traffic.
- **`RoutingExplain`** (`explain.rs`) — the "why did this model run, and
  why not the others" report (`conway routes explain <role>`), implemented
  *solely* as a projection of the router's own evaluation, never a
  reimplementation of filtering.

`conway-core` owns the port traits and the content-free request/response
config types this crate operates on; this crate provides the
implementations. No classifier, embedding model, or other learned component
may be linked into this crate — routing never inspects prompt content, and
`RouteRequest`'s field set is enforced elsewhere (in `conway-core`) to make
that a compile-time property, not just a convention here.

Content-aware routing is therefore expressed by choosing the *role*, above
this crate, rather than by teaching the resolver to read text. See
`ARCHITECTURE.md` §3.3.

## Capability-based routing

`CapabilityIndex` (`capability.rs`) is the startup-built `(backend, model)
-> Capabilities` lookup; `satisfies` is the pure predicate the router
filters candidates through. Filter order is fixed and binding:

```
pin -> capability (headroom-aware) -> health -> chain order
```

A pinned model (`RouteRequest::pin`, or an agent-def pin) bypasses the
chain entirely; otherwise the router walks the role's configured fallback
chain in order, skipping any candidate that fails the capability check or
whose endpoint has an open breaker, and returns every surviving candidate
ordered by chain position, each carrying a `RoutingReason` explaining its
position (`AliasPrimary`, `Fallback { position, after }`, or, for a skipped
candidate reported by `RoutingExplain`, `CapabilitySkip`/`HealthSkip`).

### 0.2.0 capability-system unification: headroom-aware gating

0.2.0 unifies the context-window check into a single `RequiredCaps::
satisfied_by(&Capabilities, est_tokens)` rule, defined once in
`conway-core` (see [`conway-core`](conway-core.md)) and mirrored here as
`capability::satisfies` over four scalars for this crate's own skip-reason
wording. The two formulations are pinned together by a test
(`satisfies_agrees_with_core_on_accept_reject`): for identical inputs they
must always agree on ACCEPT vs. REJECT, even though their message text can
differ.

`DeclarativeRouter::new` resolves each role's effective headroom exactly
once, at construction, from `RoutingConfig`'s override chain
(`RoutingConfig::headroom_for`) — a caller-supplied
`RouteRequest.required.headroom_tokens` is never separately consulted by
this router, since the whole point of resolving once at construction is
that `resolve()` is a lookup, never a computation. When a candidate is
skipped purely for lacking headroom, the router reports it uniformly as
`RoutingReason::CapabilitySkip` (folded into `RoutingError::NoCandidate`
when every candidate is rejected) rather than a separate error variant —
this is a documented, intentional divergence from an earlier draft of the
`Router::resolve` doc comment in `conway-core` (which described a
standalone `ContextTooLarge` return), flagged in-source for reconciliation
rather than silently worked around. `RoutingExplain`'s report carries the
effective `headroom_tokens` so a headroom-caused exclusion is visible
rather than looking like an arbitrary skip.

## Circuit breakers: transport and probe, independently

`BreakerRegistry` tracks **two independent breakers per endpoint** — a
*Transport* breaker fed by request-path failures, and a *Probe* breaker fed
by the background health prober — because a slow-but-alive local server
and a genuinely dead one are different states that should be handled
differently. All endpoint health *state* lives in this registry; routing
*policy* (`DeclarativeRouter`) never mutates it, only reads it through the
`HealthRegistry` port — `state`/`kind_state` are read-only by construction.

`failure::classify(&BackendError) -> FailureClass` is the single authority
on whether an error advances the fallback chain and/or feeds a health
observation, resolving design tension T-2 with a three-class model:

- `FailoverRetryable` — trip the relevant breaker and advance the chain
  (`Transport`, `ServerError`, `RateLimit`).
- `RequestIncompatible` — advance the chain (a different candidate may
  serve this request), but never trips a breaker (`BadRequest`,
  `ContextOverflow`: request problems, not endpoint-health signals — a
  too-large prompt must never trip a breaker for an otherwise-healthy
  endpoint).
- `Fatal` — neither.

This table is deliberately distinct from `conway-core`'s coarser
`BackendError::is_failover_worthy()` (same-request transport-retry policy,
answered in [`conway-backends`](conway-backends.md)): the two intentionally
disagree on `BadRequest` — never worth re-sending as-is, but the chain
still advances past it because a different model may accept the request
outright (e.g. a larger context window).

### Dialect-aware health probing (0.2.0)

`probe_error_observation` (`prober.rs`) reuses `failure::classify` to map a
probe's `BackendError` to a health observation, so probe-path and
request-path health signals can never diverge on what counts as an
endpoint problem. Only `FailureClass::FailoverRetryable` trips the Probe
breaker (`Observation::ProbeFail`); `RequestIncompatible` (for example, a
`BadRequest` from a 404 on a liveness path a particular dialect simply
doesn't serve) and `Fatal` errors yield **no observation at all**, leaving
breaker state untouched. This is the dialect-aware refinement: "this path
isn't served by this backend's dialect" must not be counted the same as
"the server is down" — without it, probing an endpoint whose dialect
lacks a conventional health-check route would spuriously flip its breaker
open.

`HealthProber::spawn` runs one concurrent probe per configured backend on a
fixed interval (each wrapped in a timeout and a panic guard, so one bad
backend never affects another or the loop itself), firing its first tick
immediately so startup health is known before the first turn is routed.

## Fallback chains

A role's `chain: Vec<ModelRef>` (`RoleConfig`, `conway-core`) is the
ordered list `DeclarativeRouter` walks; `Route::reason` on each surviving
candidate records `AliasPrimary` (position 0) or `Fallback { position,
after: Vec<AttemptFailure> }` for later positions, so a caller can see not
just which model was chosen but what was tried and skipped before it. The
actual walk-the-chain-on-failure behavior at request time lives in
[`conway-runtime`](conway-runtime.md)'s attempt loop, which consults
`resolve`'s ordered `Vec<Route>` and this crate's breaker/failure
classification as it retries.

## How it fits the whole

`conway-routing` depends on [`conway-core`](conway-core.md) (the port
traits and config types) and [`conway-backends`](conway-backends.md)
(`Backend::capabilities`/`probe`, to build `CapabilityIndex` and to drive
`HealthProber`). [`conway-runtime`](conway-runtime.md) is this crate's
consumer: it calls `Router::resolve` once per turn and walks the returned
candidate list, recording health observations as it attempts each one. See
[`/ARCHITECTURE.md §3.3`](/ARCHITECTURE.md) and the turn data-flow diagram
in [`/ARCHITECTURE.md §4`](/ARCHITECTURE.md).
