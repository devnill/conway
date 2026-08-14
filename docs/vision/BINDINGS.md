# Non-Rust bindings: how, not whether

**Board item `01M00QFMV84FTD3F6HVHCJRZN2` (D5-0). Research, not design-in-progress —
this is the answer D5-0b builds from.** Written 2026-08-14. Context:
[`PLAN.md`](PLAN.md) domain D5, [`INTENT.md`](INTENT.md) §7c.

---

## Recommendation

**Diplomat.** Not UniFFI, not raw `cbindgen`.

One line: Diplomat is a proc-macro that grows a **new, thin crate** — call it
`crates/conway-c` — around `conway`'s existing public types, generating a C
header (and, for free, C++/JS/Kotlin bindings later) from ordinary-looking Rust
`impl` blocks. It requires no IDL file, no schema kept in sync by hand, and no
change to `conway` or `conway-core` at all. That last property is not
incidental — it is why this is the only one of the three candidates that fits
the constraint already settled in `INTENT.md` §7c and `PLAN.md`'s ownership map
without a fight.

## Why Diplomat over the alternatives

**UniFFI** is IDL-driven (`.udl`, or its newer proc-macro mode) and its
distribution effort is spent on Kotlin/Swift/Python — mobile and scripting
hosts with their own memory-managed runtimes. conway's stated targets are "C,
C++, really anything else that was compiled, including embedded contexts" —
UniFFI's scaffolding assumes a managed-language FFI runtime is on the other
side (object finalizers, GC-friendly handle tables) that a bare C host or an
embedded target does not have. It is the wrong shape for the audience this
item names.

**cbindgen** is the floor, not a contender: it turns `#[repr(C)]` structs and
plain `extern "C" fn`s into a header. It has no opinion on the three hard
parts (async, panics, ownership) because it does no code generation beyond the
header — every one of those has to be hand-built underneath it. Fine as a
fallback if Diplomat turns out not to fit some corner of conway's surface
(D5-0b should say so if it happens), but not the primary answer.

**Diplomat** was built for exactly this shape of problem: ICU4X needed a large,
`async`-adjacent*, fallible, ownership-heavy Rust API reachable from C, C++,
and eventually a dozen other languages, without hand-rolling a marshaling
layer per target and without inventing an IDL that drifts from the Rust source
of truth. Its answer to all three hard parts already exists and is described
below. (*ICU4X's core is not itself async; Diplomat's `async_ffi` support was
added for other embedders with async Rust cores — see the async section.)

**The decisive fit, though, is structural, not featural.** Diplomat's bridge
module — `#[diplomat::bridge] mod ffi { ... }` — lives in *whatever crate runs
the codegen*, and that crate is free to simply wrap types from an upstream
dependency it does not control. ICU4X's own `icu_capi` crate is the
precedent: `icu_calendar`, `icu_collections`, etc. carry no Diplomat
dependency and no `#[diplomat::opaque]` annotation anywhere in their own
source; `icu_capi` alone depends on `diplomat`/`diplomat_runtime` and defines
newtype wrappers (`struct ICU4XCalendar(icu_calendar::Calendar)`) with
Diplomat-annotated methods that delegate to the wrapped type. Applied here:
`crates/conway-c` depends on `conway` (never `conway-core`), defines its own
wrapper structs around `conway::SessionHandle`, `conway::TurnHandle`,
`conway::EventStream`, `conway::ConwayError`, and puts the `#[diplomat::bridge]`
module entirely inside itself. **`conway`'s own source gains zero new
dependencies and zero new attributes.** This is precisely "another consumer
of the facade, the same shape as a first-party plugin" (`INTENT.md` §7c) — not
a special case requiring an exception to the ownership map, just another crate
depending on `conway` the same way `conway-cli` does. UniFFI's proc-macro mode
can technically do the same wrapping, but its idiomatic path (and most of its
tooling/examples) still leans on annotating the source crate directly or
maintaining a separate `.udl`; Diplomat's bridge-module idiom is the cleaner
match for "adapter sitting further out."

---

## The three hard parts

### 1. Async — the runtime is owned by the binding crate, not the host

conway's facade requires a live tokio runtime to do anything at all
(`SessionHandle`/`TurnHandle` are thin wrappers over `Arc<conway_runtime::Runtime>`,
itself a set of spawned tokio tasks — see `crates/conway/src/session_handle.rs:1-2`,
`:91-96`). A C host has no runtime of its own to offer, so — unlike, say,
wasmtime's C API, which can leave scheduling to an embedder that may already
have an executor — **`conway-c` must construct and own the runtime itself.**

- `conway_init(config) -> ConwayHandle*` builds a multi-thread `tokio::Runtime`
  once, at startup, and stores it (as an `Arc<tokio::runtime::Runtime>`) inside
  the returned opaque handle. `conway_shutdown(ConwayHandle*)` tears it down.
  Explicit init/shutdown, not lazy-static-on-first-call: an embedded host
  wants control over when a thread pool gets spun up, and an explicit pair
  makes the cost visible rather than ambient.
- **No async ABI is exposed to C.** Every `extern "C"` entry point that needs
  to run a `conway` future does `handle.runtime.block_on(fut)` on the calling
  thread and returns a plain value or error. This is the wasmtime-C-API move.
  wasmtime does not hand a C host a poll-able future either — synchronous
  entry points, an internally-owned executor, "Wasmtime won't manage its own
  thread pools... that's left to the embedder" is about wasm-guest async, not
  about how the *host* calls in, and the host-calling-in story is a blocking
  call every time. conway-c should copy that, not Diplomat's `async_ffi`
  `FfiFuture`/poll-from-C machinery — that exists in the Diplomat ecosystem
  for embedders whose target languages already have a native async concept to
  bridge into (JS `Promise`, Kotlin coroutines). C does not, and "really
  anything else that was compiled, including embedded contexts" skews further
  away from that, not toward it.
- **The event stream is a pull, not a push, by default.** `SessionHandle::events`/
  `TurnHandle::events` return a `futures_core::Stream` (`crates/conway/src/event_stream.rs`).
  The C projection is `ConwayEvent* conway_turn_next_event(ConwayTurn*, int64_t
  timeout_ms, ConwayError**)`, implemented as
  `runtime.block_on(tokio::time::timeout(dur, stream.next()))`. A host that
  owns its own main loop calls this with `timeout_ms = 0` from inside that
  loop (non-blocking poll, exactly the shape an embedded/game-loop host
  needs) or spawns one dedicated thread that blocks with `timeout_ms = -1`
  and hands events back through its own queue. Either way, **the host decides
  the cadence; conway-c never spawns a callback onto a host-owned thread
  uninvited.** A push-style convenience (`conway_session_events_subscribe(fn_ptr,
  user_data)`, invoked from a `conway-c`-owned background task on a runtime
  worker thread) is worth offering *in addition*, for hosts that prefer it —
  but it must be opt-in and its doc must say plainly which thread `fn_ptr`
  runs on and that the host is responsible for its own thread-safety, the same
  disclosure every C callback API of this shape carries (libuv, wasmtime's own
  callback-based wasi hooks).
- Every method on `SessionHandle` this item needs to project (`prompt`, `ask`,
  `fork`, `spawn`, `events`, `events_from`) is already `async fn` returning
  `Result<T>` — the block-on wrapper is uniform across all of them, which is
  what makes hand-writing `crates/conway-c`'s bridge module (rather than
  reaching for `async_ffi`) tractable instead of a special case per method.

### 2. Panics — caught at the boundary, never allowed to unwind into C

Unwinding across an `extern "C"` fn is undefined behavior once it crosses out
of Rust; since Rust 1.71, the compiler aborts the process by default rather
than letting it happen, which is *safe* but is a hard process crash for what
might be a recoverable, single-request failure (a `.unwrap()` deep in a tool
implementation, say). Diplomat's `async_ffi` crate demonstrates the pattern
conway-c should copy even for its synchronous entry points: wrap the call body
in `std::panic::catch_unwind`, and on `Err`, convert to a typed error instead
of letting the default abort fire. `FfiFuture`'s poll implementation does
exactly this (`std::panic::catch_unwind` around the poll, returning
`FfiPoll::Panicked` on capture) — the mechanism, not the async-specific type,
is what to reuse.

Concretely:

- Every `extern "C"` function `conway-c` exports wraps its body in
  `catch_unwind(AssertUnwindSafe(|| { ... }))`. A caught panic becomes
  `ConwayError { kind: Panic, message: <payload as string, or "non-string
  panic payload"> }`, returned through the same out-parameter every other
  failure uses (§3 below) — never a special second error channel.
- **The binding crate's build profile must keep `panic = "unwind"`.** cdylibs
  are sometimes built with `panic = "abort"` to shrink codegen; if `conway-c`
  is compiled that way, `catch_unwind` cannot catch anything and this whole
  section is silently defeated. D5-0b should add a guard test (parse the
  crate's own resolved profile, the same shape `crates/conway/tests/
  architecture_invariants.rs` already uses for other Cargo.toml-shaped
  invariants) so this can never regress unnoticed.
- **This only needs to sit at the `conway-c` boundary, not inside
  `conway-runtime`'s own spawned tasks.** `SessionHandle`'s methods this item
  projects are cheap awaits over channels/store I/O (`prompt` appends and
  returns once the runtime accepts the turn; it does not await the model's
  reply — see `session_handle.rs:141-143`). If one of those direct calls
  panics, it panics on the OS thread that called `block_on`, and
  `catch_unwind` around that `block_on` call catches it, full stop. A panic
  inside the *background* `AgentLoop` task the runtime already runs for a
  turn's actual inference is a separate, pre-existing concern of
  `conway-runtime`'s own supervision (it already has to turn a backend/tool
  failure into `ResultStatus::Failed` rather than crash the task pool — that
  is existing behavior this item does not change or depend on).

**How the error taxonomy survives the crossing** (the second half of this hard
part): `conway_core::error`'s types are already `#[non_exhaustive]`, serde
round-trippable, externally-tagged, `Display`-rendered via `thiserror`
(`crates/conway-core/src/error.rs:1-4`; `crates/conway/src/error.rs` layers
one more umbrella on top). That is nearly the ideal shape for an FFI error
already — the crossing does not need to reinvent one:

- Project a coarse, stable `enum ConwayErrorKind { Backend, Store, Routing,
  Runtime, Plugin, Config, Parse, Panic, ... }` (Diplomat generates a real
  target-language enum from a Rust one) for cheap branching — this is the
  thing a caller's `switch` statement wants.
- Alongside it, hand back the error's full `serde_json::to_string(&err)` output
  through the same `DiplomatWrite` buffer strings use (§3) for anyone who
  wants full fidelity — a debugger, a log sink, a host language with its own
  JSON tooling. This gives "a projection of the same public API," not a
  lossy summary: nothing in the taxonomy is dropped, just re-rendered at the
  boundary.
- `#[non_exhaustive]` on the Rust side needs a deliberate answer on the
  Diplomat side (its enum codegen wants either exhaustiveness or an explicit
  catch-all arm) — flag this to D5-0b as a concrete implementation
  question to resolve while wiring the bridge module, not something this
  survey needs to settle in the abstract.

### 3. Ownership — Diplomat's two answers, applied directly

**Strings never cross as an owned, host-freed `char*`.** Diplomat's
`DiplomatWrite` (formerly `DiplomatWriteable`) pattern has the *caller*
supply a growable buffer and Rust writes UTF-8 bytes into it; there is no
`conway_string_free` a host can forget to call, because there is no
Rust-allocated string handed across in the first place. Every text-bearing
projection (`TurnHandle::text`, an event's delta text, an error's rendered
message/JSON) uses this pattern.

**Everything else that must persist across calls is an opaque handle,
Diplomat's `#[diplomat::opaque]` boxed-pointer pattern: `ConwaySession*`,
`ConwayTurn*`, `ConwayEventStream*`, `ConwayHandle*` (the runtime owner)** —
one generated `_destroy` function per type, called explicitly by the host,
exactly like every C API of this shape (and exactly what `SessionHandle_destroy`-
style generated bindings already look like for ICU4X). No implicit
finalization, because C has none to hook.

**The lifetime question the task calls out specifically — what happens when
the host holds a handle past the harness's lifetime — has a clean answer
because of what `SessionHandle` already is on the Rust side, not because of
anything conway-c has to invent:** `SessionHandle` is `Clone`, and every field
is `Arc`/`Copy` (`crates/conway/src/session_handle.rs:90`, doc: "Cheap to
`Clone` -- every field is `Arc`/`Copy`"). So the design instruction for
`conway-c` is: **every opaque wrapper struct stores its own `Arc` clone of
whatever it depends on** — a boxed `ConwaySession` holds a full owned
`SessionHandle` (which itself already carries `Arc<Runtime>`), not a borrowed
reference into the `ConwayHandle` that produced it. Ownership on the C side is
then structurally correct by construction, the same way it already is for a
Rust caller who clones a `SessionHandle` and outlives the `Conway` that built
it: dropping (destroying) the top-level `ConwayHandle` releases *its own*
reference, and the runtime/store stay alive exactly as long as any live
`ConwaySession*`/`ConwayTurn*` still holds a clone — no reference-counting
registry needs to be built for this, Rust's own `Arc` is already doing it
underneath.

What this does **not** solve, and no C API can: a host that calls
`conway_session_destroy` and then dereferences the freed pointer anyway (use-
after-free) or calls `_destroy` twice (double-free). That is inherent to any
manually-memory-managed target language and is mitigated the conventional way
— document "null the pointer after destroy" as the calling convention, the
same posture Diplomat's own generated bindings take, and do not pretend a
handle-liveness registry inside `conway-c` would close a gap C's memory model
does not allow closing.

---

## Should this share machinery with the out-of-process plugin host (D3c, `01KZY8PATND84AKY0J376E3DWV`)?

**No — different transports, and forcing a shared one would cost both.**

D3c is conway calling *outward* to a program over IPC (a subprocess, most
likely stdio-framed); D5 is a non-Rust host calling *inward* through an
in-process compiled boundary. They are the same *class* of problem
("non-Rust code needs to talk to conway") pointed in opposite directions, but
the mechanics do not overlap:

- An in-process C ABI passes scalars, opaque pointers, and caller-supplied
  buffers directly across a function call — no framing, no serialization on
  the hot path, no process lifecycle to manage. Diplomat's codegen is built
  entirely around that shape.
- An out-of-process protocol needs message framing, request/response
  correlation, schema/version negotiation, and child-process supervision —
  none of which Diplomat (or any in-process FFI tool) has any opinion about,
  because none of it exists at a function-call boundary.

Building one shared "protocol layer" that tries to serve both would mean
either forcing D3c's IPC framing overhead onto every in-process C call (wrong
for the embedded targets this item names explicitly), or forcing D5's
zero-copy handle/buffer conventions onto a wire protocol that has to survive
process boundaries and partial writes (wrong for D3c). **D3c should commit to
its own subprocess/IPC protocol independently and should not wait on this item
any further** — the only reason it was blocked was to avoid the two designs
colliding on the same transport, and they don't share one.

**The one thing worth carrying over, and it already exists:** conway's
tool-facing and error types are already serde round-trippable with an
externally-tagged JSON shape (`crates/conway-core/src/error.rs`'s own doc:
"serde round-trippable (externally tagged, owned data only)"). D3c's
out-of-process wire format should use that same JSON shape for anything it
needs to serialize (tool calls, results, errors) rather than inventing a
second one — and D5's error projection (§2, the `DiplomatWrite`-carried JSON
payload alongside the coarse enum) uses the *identical* serialization for the
same reason. That is sharing a **data model**, which was already true before
either item started; it is not sharing **transport machinery**, which the two
should not attempt to share.

---

## Sketch: ask a model a question, get an answer

The simplest case, matching D5-2's "configure down to a bare inference call"
experiment, projected through the C API this item recommends. Illustrative,
not a compilable header — exact names are `conway-c`'s (D5-0b's) to choose.

```c
#include <conway.h>

int main(void) {
    ConwayError* err = NULL;

    /* Builds and owns a multi-thread tokio runtime; loads
     * ~/.conway/settings.json + project config the same way
     * conway::ConwayBuilder::discover() does on the Rust side. */
    ConwayHandle* conway = conway_init_discover(&err);
    if (!conway) {
        /* err is a boxed opaque handle: conway_error_kind(err) for the
         * coarse enum, conway_error_write_json(err, &buf) for full fidelity. */
        goto fail;
    }

    ConwaySessionSpec spec = conway_session_spec_default();
    ConwaySession* session = conway_new_session(conway, &spec, &err);
    if (!session) goto fail;

    ConwayTurn* turn = conway_session_prompt(session, "What is the capital of France?", &err);
    if (!turn) goto fail;

    /* Pull-based: this call blocks the CALLING thread for up to timeout_ms,
     * returning NULL (with err == NULL) on a clean timeout. A host with its
     * own main loop passes timeout_ms = 0 and calls this every tick instead. */
    ConwayEvent* ev;
    while ((ev = conway_turn_next_event(turn, /*timeout_ms=*/-1, &err)) != NULL) {
        if (conway_event_is_text_delta(ev)) {
            DiplomatWrite buf = conway_write_buffer_new();   /* caller-owned */
            conway_event_text_delta_write(ev, &buf);
            fwrite(buf.data, 1, buf.len, stdout);
            conway_write_buffer_destroy(&buf);
        }
        bool finished = conway_event_is_agent_finished(ev);
        conway_event_destroy(ev);
        if (finished) break;
    }
    if (err) goto fail;

    conway_turn_destroy(turn);
    conway_session_destroy(session);
    conway_shutdown(conway);   /* releases this handle's own runtime reference */
    return 0;

fail:
    if (err) {
        DiplomatWrite buf = conway_write_buffer_new();
        conway_error_write_message(err, &buf);
        fprintf(stderr, "conway: %.*s\n", (int)buf.len, buf.data);
        conway_write_buffer_destroy(&buf);
        conway_error_destroy(err);
    }
    return 1;
}
```

This is a direct C projection of `crates/conway/examples/minimal_session.rs`'s
real (non-fake) path: `ConwayBuilder::discover()` → `new_session` →
`session.prompt(text)` → drain the returned turn's text. Nothing here needed
inventing a second, C-specific entry point — the point `INTENT.md` §7's "going
straight to the model is a composition, not a feature" makes for Rust callers
holds unchanged for this one.

---

## What D5-0b inherits from this item

1. **Tool: Diplomat.** New crate `crates/conway-c`, depending on `conway`
   only, per the ownership map's existing D5 row (this item does not need a
   ownership-map amendment — `crates/conway-c` is a new crate, and per
   `PLAN.md`'s "Cargo.toml (workspace)... Members are a `crates/*` glob"
   note, adding it needs no workspace-manifest edit beyond appending any new
   `[workspace.dependencies]` entry Diplomat itself requires).
2. **Own the runtime, block on every call, pull-based events by default,**
   push-based subscription offered as an explicit, documented opt-in.
3. **`catch_unwind` at every `extern "C"` boundary**, `panic = "unwind"`
   pinned and guarded by a test, panics surfaced as a typed error variant
   through the same channel every other failure uses.
4. **Strings via `DiplomatWrite`, everything persistent via opaque
   `#[diplomat::opaque]` handles that each hold their own `Arc` clone** of
   what keeps them alive — no handle-liveness registry to build.
5. **Errors: a coarse generated enum plus the existing serde JSON shape**,
   carried through the same `DiplomatWrite` string mechanism as everything
   else.
6. **No shared transport with D3c.** D3c should proceed independently on its
   own out-of-process protocol; both should serialize through
   `conway_core::error`'s and the tool-facing types' existing JSON shape
   rather than each inventing one.
7. **Open items D5-0b should resolve while building, not re-litigate:** the
   `#[non_exhaustive]`-enum codegen question (§2), and whether a push-style
   event subscription ships in the first cut or is deferred — this item's
   recommendation is that pull-based is sufficient for "ask a model a
   question and get an answer" and push can follow once a real embedding
   consumer asks for it.

## Sources consulted

- `crates/conway/src/session_handle.rs`, `crates/conway/src/event_stream.rs`,
  `crates/conway/src/error.rs`, `crates/conway/src/lib.rs`,
  `crates/conway/Cargo.toml`, `crates/conway/examples/minimal_session.rs`,
  `crates/conway-core/src/error.rs` — read in full for this item.
- [Diplomat repository](https://github.com/rust-diplomat/diplomat) and [the
  Diplomat book](https://rust-diplomat.github.io/diplomat/) — bridge-module
  idiom, `#[diplomat::opaque]`, `DiplomatWrite`.
- [`async-ffi` docs](https://docs.rs/async-ffi/latest/async_ffi/) — the
  `catch_unwind`-around-poll pattern this item recommends reusing for
  synchronous entry points too.
- [Manish Goregaokar, "Diplomat: Multi-language FFI for Rust
  Libraries"](https://manishearth.github.io/blog/2026/06/14/diplomat-multi-language-ffi-for-rust-libraries/)
  — design rationale, callback support across backends.
- [Wasmtime's embedding API docs](https://docs.wasmtime.dev/api/wasmtime/) and
  [`wasmtime_wasi::runtime`](https://docs.wasmtime.dev/api/wasmtime_wasi/runtime/index.html)
  — the "own the runtime, expose a synchronous C boundary" precedent.
