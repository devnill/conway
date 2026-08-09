# Router/RouterFactory/HealthRegistry/SessionStore: compile evidence for the
§13.5 dated status note

Board item 01KZHF46C80HFAQN2CJEXXVYY5 (backends-as-plugins charter closer),
which also carries the fix for board item 01KZHMNABS6HC0KT1D1CKM9W8H's own
"Router" staleness. This note is the compile evidence both items' own
acceptance criteria ask for — "answer by compiling, not by reading" — kept
in the same style as `.design/backends-as-plugins-q1-compile-evidence.md`,
the precedent this repeats for a second port.

## The method

A scratch crate outside this workspace (built under this session's own
scratchpad directory, never added to `crates/`, deleted before this item
finished — `git status --porcelain crates/` carries nothing from it),
whose `Cargo.toml` named exactly one dependency:

```toml
[dependencies]
conway = { path = "/Users/dan/code/conway/crates/conway" }
```

Four `src/lib.rs` bodies were compiled in turn, one per claim below, each
naming every type the trait's own methods require through `conway::`
paths alone, then `cargo build` was run with no flags.

## Claim 1: a facade-only crate cannot write `impl Router` — TRUE, still

```rust
use conway::{Route, RouteRequest, Router, RoutingError};

struct MyRouter;

impl Router for MyRouter {
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        let _ = req;
        todo!()
    }
}
```

```text
error[E0432]: unresolved imports `conway::Route`, `conway::RouteRequest`, `conway::RoutingError`
 --> src/lib.rs:1:14
  |
1 | use conway::{Route, RouteRequest, Router, RoutingError};
  |              ^^^^^  ^^^^^^^^^^^^          ^^^^^^^^^^^^ no `RoutingError` in the root
  |              |      |
  |              |      no `RouteRequest` in the root
  |              no `Route` in the root
  |              help: a similar name exists in the module: `Router`
```

`Router`'s own trait is re-exported (it resolves fine); the three types its
one method, `resolve`, names in its own signature are not. So: no, a crate
depending only on `conway` cannot implement `Router` today — the SAME
shape of finding `.design/backends-as-plugins-q1-compile-evidence.md`
recorded for `Backend`, before that port got its own curated
`conway::backend` module. `docs/embedding.md`'s table row for `Router`
(`Yes | No (RouteRequest, Route, RoutingError) | No`) is accurate, not
stale, on this specific question.

## Claim 2: a facade-only crate CAN write `impl RouterFactory` — TRUE, and new

```rust
use conway::{CoreConwayError, RouterBuildContext, RouterBundle, RouterFactory};

struct MyRouterFactory;

impl RouterFactory for MyRouterFactory {
    fn id(&self) -> &str {
        "my.router"
    }

    fn build(&self, ctx: RouterBuildContext<'_>) -> Result<RouterBundle, CoreConwayError> {
        let _ = ctx.routing;
        let _ = ctx.headroom;
        let _ = ctx.backends;
        let _ = ctx.capability_index;
        todo!()
    }
}
```

```text
warning: struct `MyRouterFactory` is never constructed
 --> src/lib.rs:3:8
  |
3 | struct MyRouterFactory;
  |        ^^^^^^^^^^^^^^^
  |
  = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `router-scratch` (lib) generated 1 warning
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
```

Compiles clean (one unrelated dead-code warning). Every field of
`RouterBuildContext` is *readable* by field access without naming its
type explicitly, including `capability_index` — confirmed separately: a
helper function signature naming `conway::CapabilityIndex` explicitly
fails with `error[E0425]: cannot find type` `CapabilityIndex` in crate
`conway`, so that one field's own type is not independently spellable,
the identical asymmetry `RoutingConfig`/`HeadroomPolicy` were re-exported
specifically to close for the other two fields (`crates/conway/src/
lib.rs`'s own comment on that re-export). This does not block `build()`'s
body above, which only ever needs to *read* the field, never to name its
type in a signature of its own.

**What this claim does and does not prove.** `RouterFactory`'s own two
methods are fully facade-only spellable, so the *shell* above — and
`docs/embedding.md`'s "Installing a router" example, which is this same
shell — is real and compiles today. What it does NOT prove is that the
`todo!()` can be filled in with a hand-rolled `Router` using only facade
types: claim 1 already showed that specific step still needs
`conway-core` directly. A working `RouterFactory::build` today either
returns a `Router` built by a fuller-access crate elsewhere in the same
binary (the shape `conway-plugin-routing` takes, confirmed by its own
`Cargo.toml`: `conway-core = { path = "../conway-core" }`, the ONLY
workspace crate it depends on beyond plain data-type crates), or, for a
crate willing to add that one dependency, a freshly written one.

## Claim 3: a facade-only crate cannot write `impl HealthRegistry` — TRUE

```rust
use conway::{BreakerState, HealthRegistry};

struct MyHealth;

impl HealthRegistry for MyHealth {
    fn state(&self, ep: &conway::EndpointId) -> BreakerState {
        let _ = ep;
        todo!()
    }
    fn record(&self, ep: &conway::EndpointId, obs: conway::Observation) {
        let _ = (ep, obs);
    }
}
```

```text
error[E0425]: cannot find type `EndpointId` in crate `conway`
 --> src/lib.rs:6:34
  |
6 |     fn state(&self, ep: &conway::EndpointId) -> BreakerState {
  |                                  ^^^^^^^^^^ not found in `conway`

error[E0425]: cannot find type `EndpointId` in crate `conway`
  --> src/lib.rs:10:35
   |
10 |     fn record(&self, ep: &conway::EndpointId, obs: conway::Observation) {
   |                                   ^^^^^^^^^^ not found in `conway`

error[E0425]: cannot find type `Observation` in crate `conway`
  --> src/lib.rs:10:60
   |
10 |     fn record(&self, ep: &conway::EndpointId, obs: conway::Observation) {
   |                                                            ^^^^^^^^^^^ not found in `conway`
```

`HealthRegistry`'s trait is re-exported at `conway`'s root (unlike
`SubagentHost`/`EventSink`, which are not nameable at all — see §4/§13.5's
own reasoning and `crates/conway/src/lib.rs`'s "Deliberately NOT here"
list, both unaffected by anything in this note); `EndpointId` and
`Observation`, which its two methods name, are not.

## Claim 4: a facade-only crate cannot write `impl SessionStore` — TRUE

```text
error[E0425]: cannot find type `SeqRange` in crate `conway`
 --> src/lib.rs:8:76
  |
8 |     async fn create(&self, meta: SessionMeta) -> Result<SessionId, conway::SeqRange> {

error[E0425]: cannot find type `StoreError` in crate `conway`
  --> src/lib.rs:11:87
   |
11 |     async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, conway::StoreError> {
```

Matches `docs/embedding.md`'s existing, unchanged `SessionStore` row
(`Yes | No (SeqRange, StoreError) | No`) — this claim was not stale and
this note does not disturb it.

## Net effect

Of the six ports §13.5 names: `Backend` was already corrected (prior
compile evidence, `.design/backends-as-plugins-q1-compile-evidence.md`).
`Router` gains a real, compiling, facade-only *installation* point
(`RouterFactory`) that did not exist when §13.5 was written, without
gaining facade-only *authorship* of the raw trait — both halves verified
above, not merely asserted. `HealthRegistry` and `SessionStore` are
unchanged: neither is facade-only implementable, by compilation. `SubagentHost`
and `EventSink` are not facade-reachable at all (not re-exported anywhere,
checked by reading `crates/conway/src/lib.rs`'s full re-export surface
rather than by a compile — there is no signature to attempt compiling
against). `.design/extension-architecture.md` §13.5's dated status note
records this breakdown.
