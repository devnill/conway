# Writing your first hook

This is the onboarding half of the set: `concepts.md` says what a hook is,
`hooks.md` says exactly what each point promises, `trust-and-security.md` and
`compatibility.md` say what you're accountable for and what conway won't
break under you. This page says how to actually build something, today,
against the tree as it stands — not the tree the design corpus describes.

**Read `concepts.md` first**, specifically "Hook-first," "Observers vs
participants," and "The value-class boundary" — this page assumes that
vocabulary and does not re-explain it.

## The one thing to get straight before you start

`concepts.md`'s own opening line matters more here than anywhere else in the
set: *"conway's plugin architecture is being documented ahead of being fully
built."* The declarative surface — a `hooks` block in `settings.json` naming
an event and a command, no Rust required — is **decided, not built** (board
item `01KZDC0RDRMMMJHX7SAFMM2Q5A`). If that's what you came here for, there is
nothing to install yet.

What you *can* build today, and what this page teaches, is an **in-process
Rust hook**: you implement `Tool`, `Plugin`, and/or `ContextHook` against the
curated `conway::plugin` facade, and wire it in through `ConwayBuilder`. This
is real, F8 (board item `01KYTJC4S3Q7ZMH4EDRKNNN5G5`) landed 2026-08-01, and
`crates/conway/tests/plugin_surface.rs` is a complete, compile-guarded worked
example of exactly this shape — every snippet below is lifted from it or from
`crates/conway/examples/minimal_session.rs`, not invented for this page.

## Ten minutes to a working hook

The fastest way to see a hook do something visible, in three steps: write it,
prove it transforms a payload with no session or network involved, then wire
it into a real (but fake-backed, no-credentials) session and watch it fire.

### 1. Depend on `conway`

conway isn't published to a registry yet (`docs/embedding.md`'s own note); if
you're working inside a checkout of this repository, add a path dependency
from your own crate:

```toml
# Cargo.toml
[dependencies]
conway = { path = "../conway" }               # adjust to your layout
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`async-trait`, `serde`, and `serde_json` are not re-exported by the facade on
purpose — they're plain data-type crates you name yourself, at the version
conway uses, so the types line up (`docs/embedding.md`'s "Writing a plugin"
section explains why).

### 2. Write the smallest hook that does something visible

A `ContextHook` that appends a marker segment to every outgoing request is
about as small as "visible" gets — you can see the segment in the assembled
payload, and later, in the transcript. This is `plugin_surface.rs`'s
`MarkerHook`, trimmed to just the append half:

```rust
use conway::plugin::{
    async_trait, ContextHook, ContextHookCtx, ContextPayload, OverflowInfo,
    PromptSegment, Provenance, Role, ContentBlock,
};

struct MyFirstHook;

#[async_trait]
impl ContextHook for MyFirstHook {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        mut payload: ContextPayload,
    ) -> ContextPayload {
        payload.segments.push(PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: "my-first-hook was here".to_string(),
            }],
            Provenance::SystemNote {
                reason: "getting-started demo".to_string(),
            },
        ));
        payload
    }

    // The trait supplies a default `on_overflow` that returns `None`
    // (give up) -- fine for a demo hook that never sees an overflow.
    async fn on_overflow(
        &self,
        _ctx: &ContextHookCtx,
        _payload: ContextPayload,
        _overflow: OverflowInfo,
    ) -> Option<ContextPayload> {
        None
    }
}
```

Every type named above (`ContextHook`, `ContextHookCtx`, `ContextPayload`,
`OverflowInfo`, `PromptSegment`, `Provenance`, `Role`, `ContentBlock`) comes
from `conway::plugin` alone — no `conway_core` import anywhere in this file,
which is the property `plugin_surface.rs` itself is a standing compile guard
for (delete one re-export from `conway::lib::pub mod plugin` and this file
stops compiling).

### 3. Confirm it fired — without a session, a backend, or a network call

The fastest possible proof that your hook does what you think: call
`before_request` directly and assert on the result. No `Conway`, no
`ConwayBuilder`, no credentials, sub-second:

```rust
use conway::plugin::{ArtifactWriteError, ArtifactWriteHandle, ArtifactWriter};
use std::path::PathBuf;
use std::sync::Arc;

// `ContextHookCtx` requires an `artifacts` handle even if your hook never
// writes a file, and there is no no-op writer in the facade yet, so a unit
// test has to supply one. See "Artifacts" below for the real thing; board
// item 01KZJ5S3ZC8SPWTX94C4HTEC2R tracks removing this boilerplate.
struct NoopWriter;

#[async_trait]
impl ArtifactWriter for NoopWriter {
    async fn write(
        &self,
        _agent_id: conway::AgentId,
        name: &str,
        _bytes: Vec<u8>,
    ) -> Result<PathBuf, ArtifactWriteError> {
        Ok(PathBuf::from(name))
    }
}

#[tokio::test]
async fn my_first_hook_appends_its_marker() {
    let hook = MyFirstHook;
    let ctx = ContextHookCtx {
        agent_id: conway::AgentId::new(),
        session_id: conway::SessionId::new(),
        turn: 0,
        model: None,
        estimated_tokens: 0,
        artifacts: ArtifactWriteHandle::new(Arc::new(NoopWriter), conway::AgentId::new()),
    };
    let payload = ContextPayload { segments: vec![], tools: vec![] };

    let out = hook.before_request(&ctx, payload).await;

    assert_eq!(out.segments.len(), 1);
    assert!(matches!(out.segments[0].provenance, Provenance::SystemNote { .. }));
}
```

You will also need `tokio` with the `macros` and `rt-multi-thread` features
as a dev-dependency for `#[tokio::test]`.

This is exactly the shape of `plugin_surface.rs`'s own
`authored_hook_transforms_payloads` test. Treat it as a floor, not a ceiling —
see "Testing your hook" below for why a test at this level alone is not
enough to claim the hook is *live*.

### 4. See it fire inside a real session

A direct call proves your logic; it doesn't prove the wiring. Register the
hook through `ConwayBuilder` and drive one real turn — no API key needed,
because you point the builder at a `FakeBackend` that echoes the prompt back,
the same no-network, no-credentials shape
`crates/conway/examples/minimal_session.rs` already demonstrates end to end:

```console
cargo run -p conway --example minimal_session
```

```text
prompt -> Hello, conway!
ask    -> (ephemeral) just checking something
main-session log head: LogSeq(4) before the ask, LogSeq(4) after -> the ephemeral ask left no trace in the main session
```

To see *your* hook in that same run, add one line to that example's builder
chain:

```rust
let conway = ConwayBuilder::from_parts(minimal_config())
    .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
    .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
    .with_session_store(store.clone())
    .with_router(Arc::new(FakeRouter::single(ModelRef { .. })))
    .with_context_hook(Arc::new(MyFirstHook))   // <- the one line this adds
    .build()?;
```

The confinement root, backend routing, and event pipeline this now runs
through are the real ones — only the backend and router are fakes standing in
for a network call. This is the honest meaning of "see it in the UI" for a
facade-only author today: there is no `/plugins` panel and no TUI surface for
a hook that isn't a declarative one (`concepts.md`'s "Hook-first" section —
no `hooks()` method exists on `Plugin` yet), so "the UI" is whatever your own
binary prints from the session — the transcript (`turn.text().await`), or the
raw event stream (`docs/embedding.md`'s "Consuming the event stream"
section), or the session log itself. If you build the actual `conway`
CLI/TUI experience with your hook wired in, you're building your own thin
binary around `ConwayBuilder` the same way `conway-cli` does — there is no
way to load an in-process Rust plugin into the shipped `conway` binary
without recompiling it, because a plugin is `Arc<dyn Plugin>`, linked into
the binary before the process starts (`concepts.md`'s "What exists today"
list).

**Steps 1-3 of this walkthrough were executed verbatim** against a scratch
crate outside the workspace whose only conway dependency is `conway`
itself, and the test passes. Running it is what caught the missing
`artifacts` handle above: the snippet previously left that field as a
comment, which is not valid Rust, so the page did not compile as written.

## Where things live

There is no config file to point you at yet, because the declarative surface
that would read one doesn't exist (`concepts.md`'s "Hook-first," "Not yet
implemented as a generalized surface"). Two things to know about "where":

- **Project-then-global precedence is real, but it's for permissions, not
  hooks.** `.conway/settings.json`, discovered by walking up from your
  current directory, takes precedence over `~/.conway/settings.json`
  (`docs/getting-started.md`'s "Configure a provider"). Nothing in that file
  configures a hook today — `permissions.json` and `settings.json`'s
  `[permissions]`/`[backends]`/`[roles]` sections are the only discovered
  config surfaces a hook-adjacent author interacts with right now (for
  example, to grant your plugin's tools through the pattern-rule mechanism
  `hooks.md` point 6 documents).
- **There is no runtime inventory surface.** `hooks.md` names this directly:
  other harnesses' documented failure mode is "a rule set nobody can
  inspect," and conway's own `/plugins` display — a place to list what
  hooks are active and whether each is healthy — is itself designed, not
  built (`hooks.md` point 12, `.design/extension-architecture.md` §9.4).
  Today, "what's active" is answered by reading your own `ConwayBuilder`
  call — the `.with_plugin(..)`/`.with_context_hook(..)`/
  `.with_permission_gate(..)` chain *is* the inventory, because it's the
  only place registration happens.

## Writing a Rust plugin

The facade path taught above is the real, current answer to "how do I write
a plugin," not a stopgap. `conway::plugin` re-exports everything you need to
implement `Tool`, `Plugin`, and `ContextHook`: the three traits, their
method-argument and return types, and the field types of the structs you
construct. `docs/embedding.md`'s "Writing a plugin" section is the canonical
list of what's exported and why each name is there — this page doesn't
duplicate that list, only the shape of using it. Register through the
builder:

```rust,ignore
let conway = ConwayBuilder::from_parts(config)
    .with_plugin(Arc::new(MyPlugin))
    .with_context_hook(Arc::new(MyHook))
    .with_permission_gate(Arc::new(MyGate))
    .build()?;
```

**The known boundary, stated without smoothing it.** The facade parity gap
board item `01KYYB2T8AHB4SJFHNG4ZETYN8` opened naming four types a plugin
author couldn't construct or match through `conway::` paths:
`SubagentSpec`, `RuntimeError`, `Fact`, and `CwdError`. That gap is now
**resolved for three of the four** — `Fact`, `CwdError`, and `SubagentError`
are exported from `conway::plugin` and pinned by name in
`plugin_surface.rs`'s `fact_and_capability_handle_errors_are_constructible_
and_matchable` test. The remaining two are **deliberately not exported**,
not overlooked, per `crates/conway/src/lib.rs`'s own doc comment on `pub mod
plugin`:

- **`SubagentSpec`** — a third-party fork/spawn goes through this crate's own
  `ForkSpec`/`SpawnSpec` instead (the visibly-distinct-types shape P-1/GP-02
  require), which convert into `SubagentSpec` via `From` but are never
  themselves that type. You have no reason to construct or match
  `SubagentSpec` directly.
- **`RuntimeError`** — `ToolCtx.subagents`'s fallible methods return
  `SubagentError`, never `RuntimeError`; there is no reachable call site for
  `RuntimeError` from this facade's surface at all.

Everything else `docs/embedding.md`'s reachability table names as
implementable from a facade-only crate — `PermissionGate`, `Tool`, `Plugin`,
`ContextHook`, and, since board item `01KZHEZF8XCD0TMDYZQP06J2KH`, `Backend`
via the separate `conway::backend` module — is implementable the same way:
name the trait at the facade root, name its supporting types from the
matching curated module, register through the builder.

### `ToolCtx`'s handle fields

If you're writing a `Tool` rather than just a `ContextHook`, `ToolCtx` hands
you capability handles you call methods on but never name the type of:
`ctx.chdir`, `ctx.events`, `ctx.subagents`. This is deliberate, not a gap —
`docs/embedding.md`'s "Writing a plugin" section explains why those types
stay unexported. `ctx.cancel` is the one exception (`CancellationToken` is
exported, so a helper function can take `&CancellationToken`).

### Artifacts

`ContextHookCtx::artifacts` is an `ArtifactWriteHandle` — a hook can write a
file inside the agent's confinement root without reaching for the filesystem
directly:

```rust
let path = ctx.artifacts.write("spill.txt", b"overflow content".to_vec()).await?;
```

This is real and exercised end to end by `plugin_surface.rs`'s own
`authored_hook_transforms_payloads` test (construct via
`ArtifactWriteHandle::new(writer, agent_id)` when you're driving a hook
directly, as in the test above — the real containment-checked writer is
supplied by the runtime when your hook runs inside an actual session).

## Testing your hook

**A unit test on your hook's transform logic is not proof the hook is
reached in a real run.** This is not a stylistic preference — it's GP-14/
P-15's rule, sharpened by this exact codebase's own history: five separate
mechanisms shipped, documented, and unit-tested in 2026-07-30/31 while never
being called from any production path (`read:*` pattern grants inert for 12
of 13 tools, `Plugin::on_init` never invoked, prompt caching hardcoded off,
`TruncationPolicy::Artifact` silently a no-op, `LogRecord::ContextMask`
lacking any producer). Every one of them had a passing unit test. None of
them had a test proving the *host* actually called them.

What this means for you, concretely:

1. **Test the transform in isolation first** (step 3 above) — fast, no
   network, catches logic bugs cheaply.
2. **Then drive it through `ConwayBuilder::build()`** and a real turn (step 4
   above), and assert on something only your hook's presence could produce
   — the appended segment surviving into `turn.text()`'s output, or a
   changed `session_usage`, or an artifact file existing on disk. **Assert
   on the observable outcome, not an intermediate signal**: a hook that
   silently no-ops and a hook that's correctly wired but has nothing to do
   both produce "nothing changed" — pick an assertion that a broken wiring
   genuinely cannot satisfy (P-15's own worked example: the `AutoAllow` deny
   test asserts on the persisted `ToolResult` text, not on gate-call count,
   because a correct refusal and a silent full bypass both produce zero gate
   calls).
3. **Simulate failure deliberately, don't just hope you never hit it.** The
   biggest surprise waiting for you here: `ContextHook::before_request` and
   `on_overflow` have **no `Result` in their signatures** (`hooks.md` point
   3's "On error" row). There is no sanctioned way to signal "this hook
   failed" other than returning the payload unchanged — and a panic inside
   your hook is *not* caught the way a tool's panic is (`ToolRunner` wraps
   tool invocation in `catch_unwind`; the call site for `before_request`/
   `on_overflow` inside `AgentLoop::run_inner` does not). Write a test that
   feeds your hook a payload your logic doesn't expect and confirm it
   degrades the way you intend, on purpose, rather than discovering the
   panic behavior live.

## Debugging

- **The event stream is your primary window.** `docs/embedding.md`'s
  "Consuming the event stream" section covers `conway::EventStream` in full;
  for a hook specifically, look at what changed between the request you
  handed the runtime and what actually reached the model — a hook that
  silently does nothing produces a request identical to the one you'd get
  with no hook registered at all, which is easy to miss unless you diff.
- **A hook that's wired but never runs looks identical to one that isn't
  registered.** `ContextHook` is `Option<Arc<dyn ContextHook>>` under the
  hood (`hooks.md` point 3's "When absent" row) — there is no error, no log
  line, nothing, if you forget the `.with_context_hook(..)` call. This is
  exactly why step 4 above (a real turn, not just a direct call) matters:
  step 3's test would pass unchanged even if you never wired the hook in at
  all.
- **The common failure mode named across this whole set: a matcher that
  silently matches nothing.** `.design/extension-architecture.md` §9.3 calls
  this out directly as another harness's documented failure — a hook
  declared against a pattern that happens to match zero calls looks, from
  the outside, identical to a hook that correctly decided not to act. The
  in-process surface you're building against today sidesteps the matcher
  half of this (there's no declarative pattern language yet to mismatch
  against), but the *shape* of the mistake still applies: double-check that
  the `ContextHook` you registered is the one actually reaching
  `AgentLoop::run_inner` for the session you're testing, not a second
  builder instance you constructed and never used.
- **No `Result`, remember.** If your hook is silently doing nothing and you
  expected an error somewhere, re-read "Testing your hook" above — there is
  no error channel for `before_request`/`on_overflow` to use. "Nothing
  changed" and "my hook decided not to act" and "my hook is completely
  disconnected from this session" are three different situations that
  produce the exact same observable output unless you've built the kind of
  test that distinguishes them.

## Where to go next

[`docs/plugins/README.md`](README.md) routes the rest of the set.
[`scripts.md`](scripts.md) covers the any-language script convention (a
different authoring surface, layered on the same mechanism, not yet built
either). [`inference-hooks.md`](inference-hooks.md) covers a hook whose
decision is made by a model rather than by code.
[`cookbook.md`](cookbook.md) carries worked, end-to-end examples beyond
this page's single demo hook — five of them, each labeled
implementable-today, partially-implementable, or blocked.
