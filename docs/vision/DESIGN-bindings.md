# Design: the §7c binding survey — Diplomat, UniFFI, cbindgen

**Board item `01M0TV5PN8RR9NN97AWP09E6K7` (EMB-1). Written 2026-08-24. Context:
[`PLAN.md`](PLAN.md) domain D-EMB, [`INTENT.md`](INTENT.md) §7c.**

> **This is a design record, not a plan.** Per this project's own standing
> rule (`INTENT.md` §8.8): a design document says what a feature *predicts*
> it will need, not a requirement the feature must satisfy. Everything below
> that reads as a decision is a hypothesis stated plainly enough to be
> falsified — most concretely by whoever builds `crates/conway-c` and finds
> a place where conway's real surface does not fit the shape this page
> assumes. §8 names what would falsify the recommendation.

---

## 0. This item found a research doc already sitting in the tree, unindexed

Before anything else: **[`BINDINGS.md`](BINDINGS.md) already exists**, in
this same directory, on `main`, since 2026-08-14 (commit `084736f`). It is a
397-line comparison of Diplomat, UniFFI, and `cbindgen` against conway's
async, streaming public API, ending in a recommendation (Diplomat), a
worked C API sketch, and a sources list. It is, in substance, this survey.

`STATE-OF-THE-UNION.md` (reviewed 2026-08-24, ten days later, against the
tree at `7654041` — a descendant of the commit that added `BINDINGS.md`)
states in §2: *"INTENT §7c — non-Rust hosts — is entirely unbuilt: no
binding crate, zero `extern "C"`, and no mention of Diplomat, UniFFI, or
cbindgen anywhere in the tree (verified by search this run)."* That claim is
false as stated, and demonstrably so — `git log --diff-filter=A -- docs/
vision/BINDINGS.md` shows it added ten days before the run that claimed no
mention exists, and `git branch --contains 084736f` shows that commit on
`main` at the review's own base. This is not a dispute about interpretation;
it is a file the review's own search missed. EMB-1 was filed *because* of
that false negative, and — per `INTENT.md` §8.1, an open question is a
failure of the spec, not a gap in the code — the correct read is that the
*survey itself* was never missing. What was missing is a document at the
location `INTENT.md` §7c's board item names, in the register this project's
design pages use (falsifiable hypothesis, `INTENT.md`-cited, revision-
markable) rather than a research memo.

**What this document is, given that:** not a rewrite from a blank page. It
re-verifies `BINDINGS.md`'s claims against the tree as it stands today
(`crates/conway/src/{lib.rs,session_handle.rs,event_stream.rs,error.rs,
builder.rs}`, read in full for this item, 2026-08-24) and against each
candidate's own current documentation (fetched this run, cited in §6), then
restates the findings at `docs/vision/DESIGN-bindings.md` — the home this
item's own acceptance criteria name. Where re-verification found drift, it
is called out explicitly (§3.1). Where it found the original claim still
holds, that is stated as confirmed, not re-derived from scratch.

**A recommendation for whoever plans next, not actioned here (out of this
item's file ownership):** `docs/vision/BINDINGS.md` should be retired or
turned into a pointer at this file once this lands, and `docs/README.md`'s
index (if it lists `BINDINGS.md`) should be corrected to cite this page
instead. Both of those files belong to other work this round; this item
owns only the new file.

---

## 1. The recommendation, stated first

**Diplomat.** Not UniFFI, not raw `cbindgen`. First target language: **C**,
generated from a new crate, `crates/conway-c`, depending on `conway` alone.

One sentence for each rejection: **UniFFI** is the wrong tool because its
entire async story — the thing conway's public API is built around — is
implemented by piggybacking on a *foreign* async runtime (§3.2), and its
code generation targets managed languages (Kotlin, Swift, Python, Ruby,
WASM) with no path to a plain C header at all, which is disqualifying for
"C at the lowest level" before any of its other tradeoffs matter. **cbindgen**
is disqualified for the opposite reason: it is not a contender, it is a
floor — it turns already-`#[repr(C)]`/`extern "C"` Rust into a header and
has no opinion on any of the three hard parts (async, panics, ownership),
so choosing it *as the answer* would mean conway hand-rolls exactly the
marshaling layer this survey exists to avoid rebuilding.

**What would falsify this recommendation:** if `crates/conway-c`'s author
finds a real corner of conway's surface — event backpressure, a callback
conway needs to invoke *into* C, a type Diplomat's bridge macro cannot
express — that Diplomat's bridge-module idiom cannot project without
contortions cbindgen-plus-hand-written-glue would have handled more simply.
Absent that, the recommendation stands.

---

## 2. What is being projected — conway's real public surface, read for this item

Not a sketch. Verified against the crate as it stands at this item's own
commit, `crates/conway/src/`:

- **`ConwayBuilder` → `Conway` → `SessionHandle` → `TurnHandle`**
  (`lib.rs`, `builder.rs`, `session_handle.rs`) is the chain — the same one
  `docs/embedding.md` documents for Rust hosts. `ConwayBuilder::discover()`/
  `::from_config()`/`::from_parts()` load config; `.build()` is
  **synchronous** despite constructing async infrastructure underneath (it
  bridges the one genuinely async step, `JsonlSessionStore::open`, onto a
  throwaway `tokio::Runtime` via `block_on` on a fresh OS thread — see
  `builder.rs`'s own module doc, "Reconciliations against the binding
  spec"). `Conway::new_session` is `async fn -> Result<SessionHandle>`.
- **`SessionHandle`** (`session_handle.rs:129`) is `#[derive(Clone)]`, and
  every field is `Arc`/`Copy` — the doc comment says exactly that:
  *"Cheap to `Clone` — every field is `Arc`/`Copy`."* It wraps
  `Arc<conway_runtime::runtime::Runtime>`, itself a set of spawned `tokio`
  tasks. Its methods relevant here: `prompt`/`prompt_agent`/`ask` (each
  `async fn -> Result<TurnHandle>`), `fork`/`spawn`/`steer`/`await_agent`/
  `cancel_with` (subagent control), `events()`/`events_from(seq)`/
  `agent_events(agent)` (each returns or resolves to an `EventStream`).
- **`TurnHandle`** (`session_handle.rs:1204`) is *not* `Clone` — it owns an
  `AsyncMutex<TurnHandleInner>` directly, not an `Arc`-wrapped one. Its
  three relevant methods: `text() -> Result<String>` (drains until
  `TurnFinished`/matching `AgentFinished`), `result() -> Result<AgentResult>`
  (drains until the matching `AgentFinished`, buffering it if `text()`
  already consumed it — "`text()` then `result()` on the same handle must
  not deadlock" is a stated binding criterion, already met on the Rust
  side), and `events() -> EventStream` (a fresh, independent subscription).
  **This is a live discrepancy `BINDINGS.md` did not flag**, worth stating
  precisely for whoever builds the bridge crate: an opaque `ConwayTurn*`
  cannot be `Arc`-shared the way `ConwaySession*` can (§4.3's `SessionHandle`
  story does not repeat here) — it must be boxed and owned exactly once,
  which is the ordinary Diplomat opaque-handle shape anyway, so this is a
  note, not a blocker.
- **`EventStream`** (`event_stream.rs:41`) implements `futures_core::
  Stream<Item = Envelope>`. It is **pull**, not push, at the Rust API level
  — a caller drives it via `poll_next`/`.next()`, there is no callback
  registration on the Rust side to project.
- **`ConwayError`** (`error.rs:19`) is `#[non_exhaustive]`,
  `#[derive(Debug, thiserror::Error)]`, and layers over `conway_core::
  error`'s own `#[non_exhaustive]` types (`RuntimeError`, `StoreError`,
  `BackendError`, `RoutingError`, `PathStoreError`) via `#[from]`. Every
  variant has a `Display` impl (the `#[error(...)]` attributes). It is
  **not** currently asserted serde-round-trippable in this crate's own
  file — `BINDINGS.md` cited that property from `conway_core::error`
  specifically (`crates/conway-core/src/error.rs`'s own doc: *"serde
  round-trippable (externally tagged, owned data only)"*), which is a
  narrower and still-accurate claim than "the facade's umbrella type is."
  §4.2 below states precisely what a C error projection can and cannot
  promise given that distinction.

Nothing above needed inventing: it is the same crate `docs/embedding.md`
already documents for a Rust host. Projecting it for a non-Rust host is the
whole content of this survey — not a new API, per `INTENT.md` §7c's own
closing note: *"No second API, no divergence in capability. A non-Rust host
gets a projection of the same public interface a Rust host uses."*

---

## 3. The four questions, per candidate

`INTENT.md` §7c names four questions any of this tooling has had to answer
already: **who drives the tokio runtime; how a stream of events crosses the
boundary; what happens to a panic that would otherwise cross; who owns
returned memory.** Answered per candidate, verified against each project's
own current documentation (fetched 2026-08-24; §6 has the exact pages).

### 3.1 Diplomat

**Runtime.** Diplomat has no opinion here — it generates a marshaling layer
around whatever Rust the bridge module names; it does not construct or own
an executor. That is the binding crate's job. The precedent this item
leans on is **wasmtime's C API**: a C host has no runtime of its own to
hand in, so the embedding crate builds and owns one, and every exported
entry point is synchronous from C's point of view — `handle.runtime.
block_on(fut)` — never a poll-from-C async ABI. `crates/conway-c` should
copy that shape exactly, for the same reason wasmtime did: C has no native
concept of a future to hand a poll-able value to.

**Confirmed and updated this run:** Diplomat's own async support has moved
since `BINDINGS.md` was written. It is no longer accurately described as
"available only via the separate `async_ffi` crate" — Diplomat now exposes
async Rust functions over FFI *natively*, mapped to each target language's
own async primitive where one exists (`async`/`await` in JS and Python,
`suspend fun` in Kotlin). This strengthens rather than weakens the
recommendation to avoid it for the C target specifically: Diplomat's native
async path exists *because* those languages have an async concept to map
onto, and C does not — so the wasmtime-style block_on/pull-based shape
remains the correct answer for `conway-c`'s first target even though the
tool itself has grown more async-capable in the interim. A later target
language with a real async primitive (Python, say) is exactly the case
where reaching for Diplomat's native async support, instead of copying the
C shape, would be the right call — flagged for that future work item, not
solved here.

**Events.** No native stream/iterator primitive crosses an FFI boundary
generically (C has none to receive one into), so this is a pull-based
handle-plus-poll-method design regardless of tool: an opaque
`ConwayEventStream*`/`ConwayTurn*` and a projected `conway_turn_next_event
(turn, timeout_ms, err) -> ConwayEvent*` that internally does
`runtime.block_on(tokio::time::timeout(dur, stream.next()))`. `timeout_ms =
0` gives a non-blocking poll for a host with its own loop; `timeout_ms = -1`
blocks a dedicated thread. A push-style callback subscription is a
reasonable *addition* later, once a real consumer wants it — not part of
what a v0 needs (§5).

**Panics.** Unwinding across an `extern "C"` boundary is undefined behavior
regardless of which tool generated the boundary — this is a property of
Rust's ABI, stated as such by Rust's own documentation, not a Diplomat
feature. Every `extern "C"` function `conway-c` exports must wrap its body
in `std::panic::catch_unwind`, converting a caught panic into a typed
`ConwayError { kind: Panic, .. }` returned through the same error channel
every other failure uses. Diplomat's own `async_ffi` crate demonstrates
this exact pattern for its generated poll functions (`catch_unwind` around
`poll`, returning a `Panicked` variant on capture) — the mechanism to
reuse, not the async-specific type. **One build-time hazard that must be
tested, not merely stated**: a cdylib built with `panic = "abort"` defeats
`catch_unwind` silently. `conway-c`'s own Cargo profile must pin `panic =
"unwind"`, guarded by a test that parses the crate's own resolved profile —
the same shape `crates/conway/tests/architecture_invariants.rs` already
uses for other `Cargo.toml`-shaped invariants.

**Ownership.** Two answers, both native to Diplomat and unchanged by this
run's re-verification: strings never cross as an owned, host-freed `char*`
— Diplomat's `DiplomatWrite` pattern has the *caller* supply a growable
buffer that Rust writes UTF-8 into, so there is no `conway_string_free` a
host can forget to call. Everything else that persists across calls —
`ConwaySession*`, `ConwayTurn*`, `ConwayHandle*` — is an opaque,
`#[diplomat::opaque]`-boxed pointer with one generated `_destroy` per type,
called explicitly by the host. §2's `SessionHandle`/`TurnHandle` asymmetry
(one is cheaply `Arc`-clonable, the other is not) maps directly: a boxed
`ConwaySession` holds its own owned `SessionHandle` clone (cheap, correct
by construction, no registry needed — Rust's own `Arc` already does the
counting), while a boxed `ConwayTurn` owns its `TurnHandle` outright and
must not be duplicated. What no C API can solve — a host that
double-frees or dereferences-after-destroy — is inherent to any
manually-memory-managed target language; the mitigation is the
conventional one (document "null the pointer after destroy"), not a
liveness registry `conway-c` would have to invent and maintain.

### 3.2 UniFFI

**Runtime — the actual discriminator (see the closing note in §3.4).**
UniFFI's own documentation states its position plainly: *"UniFFI can't
rely on a Rust async runtime. We don't want to force library authors into
a particular runtime."* Concretely, it does **not** construct or own a
tokio runtime at all — it "piggybacks off the runtime from the foreign
bindings," generating four scaffolding functions per async Rust function
(a constructor returning a `RustFuture` handle, plus `poll`/`complete`/
`free`) that the *foreign* side calls repeatedly to drive completion. For
Kotlin or Swift, "the foreign side" already has a real async runtime
(coroutines, Swift's structured concurrency) to drive that polling loop
from, and the exported async function becomes genuinely `async`/`suspend`
on that side. **For plain C, "the foreign side's own async runtime" does
not exist** — there is no C-side event loop UniFFI could piggyback onto,
so the entire mechanism this design leans on has nothing to attach to.
This is not a secondary concern; it is the reason UniFFI does not generate
C bindings at all (next paragraph) — the two facts are the same fact seen
from two directions.

**Confirmed: no C target, at all.** UniFFI generates bindings for Kotlin,
Swift, Python, Ruby, and (more recently) WASM — its own README/user guide
names exactly these, and does not include C or C++ among them. It is not a
tool that happens to be worse-suited to C; it produces no C header as an
output. For "the survey should reach Diplomat, UniFFI, and cbindgen at the
lowest level" (`INTENT.md` §7c's own wording — cbindgen is explicitly
named as the low-level end), UniFFI is disqualified on this axis alone,
independent of the async story above.

**Events.** Not evaluated in depth — disqualified already on runtime/
target-language grounds. For the record: UniFFI's model for a Rust-side
stream generalizes the same `RustFuture`-polling machinery per item, which
would face the identical "no foreign runtime to drive it" problem for C
that the base async story does.

**Panics.** UniFFI's generated scaffolding wraps every exported call in a
panic-catching helper before a result crosses the boundary — the same
`catch_unwind`-at-the-seam discipline every tool here converges on, because
it is required by Rust's own unwinding-across-FFI rule, not a choice
specific to UniFFI. Not a differentiator between candidates.

**Ownership.** UniFFI's object model allocates interface instances on the
heap via `Arc`, projected across the boundary as opaque `u64` handles
(a leaked/recovered `Arc` pointer for Rust-owned objects), with an explicit
free function per interface that the foreign side calls to return
ownership and let the value drop. Structurally similar to Diplomat's
opaque-handle-plus-`_destroy` pattern — also not a differentiator.

### 3.3 `cbindgen`

**Runtime, events, panics, ownership — no opinion on any of them, by
design.** `cbindgen`'s job (confirmed against its current documentation,
crate version `0.29.4`, still `mozilla/cbindgen`, actively maintained) is
exactly this: it walks Rust source already written as `#[repr(C)]` structs
and `extern "C" fn`s and emits a matching C/C++ header. It performs no code
generation beyond that header — every one of the four questions above
would have to be answered by hand underneath it, in the same crate,
before `cbindgen` ever runs. This confirms `BINDINGS.md`'s original framing
exactly: `cbindgen` is the floor this project would be standing on if it
built its own marshaling layer, not a competing answer to "how do the
async/panic/ownership questions get answered."

### 3.4 What actually separated the three

**The runtime-ownership question is the discriminator, and it is the one
that also decides the target-language question — they turn out not to be
separate axes.** cbindgen never enters this comparison on the runtime axis
at all (it has no opinion, full stop, §3.3). Between Diplomat and UniFFI,
the deciding fact is not a feature checklist — it is that UniFFI's entire
async design assumes the foreign side already has a runtime capable of
driving a poll loop, which is true of Kotlin/Swift/Python and definitionally
false of C, and that assumption is *why* UniFFI generates no C bindings at
all. Diplomat makes the opposite assumption — the binding crate constructs
and owns the runtime itself, the wasmtime-C-API shape — which is exactly
what a runtime-less target language needs. `INTENT.md` §7c names conway's
targets as "C, C++, really anything else that was compiled, including
embedded contexts" — hosts that, like C, cannot be assumed to already run
an async event loop of their own. That is the sentence UniFFI's own
architecture fails against, independent of any other comparison point.

---

## 4. Tested against §7c's constraints

`INTENT.md` §7c states three constraints, in order of importance, and this
recommendation is checked against each rather than assumed to satisfy them.

**"It does not belong in the core or the engine. ... another consumer of
the public API, the same shape as a first-party plugin: its own crate,
never reaching into the internals. The core learns nothing about C."**
Diplomat's bridge-module idiom is what makes this achievable rather than
aspirational: `#[diplomat::bridge] mod ffi { ... }` lives entirely inside
whatever crate runs the codegen, and that crate is free to wrap types from
an upstream dependency it does not control. ICU4X is the existing,
shipping precedent for exactly this shape — `icu_capi` alone depends on
`diplomat`/`diplomat_runtime`; `icu_calendar`, `icu_collections`, and the
rest of ICU4X's own crates carry no Diplomat dependency and no
`#[diplomat::opaque]` annotation anywhere in their own source. Applied
here: `crates/conway-c` depends on `conway` only (never `conway-core`),
defines its own newtype wrappers around `SessionHandle`/`TurnHandle`/
`EventStream`/`ConwayError`, and the `#[diplomat::bridge]` module lives
entirely inside `conway-c`. `conway`'s own source gains zero new
dependencies and zero new attributes — confirmed against the crate as it
stands today (§2): nothing in `crates/conway/Cargo.toml` names Diplomat,
and no workspace member named `conway-c` exists yet, so this constraint is
currently satisfied by construction (there is nothing to violate) and
stays satisfied by the shape above once the crate is built. UniFFI's
proc-macro mode can technically achieve the same wrapping, but its
idiomatic path still leans toward annotating the source crate directly or
maintaining a separate `.udl` — Diplomat's bridge-module idiom is the
cleaner match for "adapter sitting further out," which the constraint
explicitly blesses as an acceptable outcome, not a fallback.

**"Follow the prior art; do not invent a binding layer. ... Look at how
comparable projects expose themselves before choosing."** §3 above and §6
below are that look, redone this run rather than assumed from the prior
research memo. The wasmtime-C-API precedent (own-the-runtime,
synchronous-from-C's-perspective entry points) is the one comparable
async-Rust-with-a-C-API project whose shape this recommendation borrows
directly, for the reason wasmtime itself gives: a C host has no executor
of its own to be handed scheduling by.

**"The hard part is async, and it is a design constraint rather than an
objection. ... Who drives the runtime, how a stream of events crosses the
boundary, what happens to a crash that would otherwise cross it, and who
owns returned memory are all real questions — and they are questions every
one of the tools above has had to answer. Read their answers first."** §3
is that reading, done against each project's current documentation rather
than from memory (per this item's own quality bar). The answer is not
"async is unsolved" — it is "async is solved differently by each
candidate, and the difference that matters for C is who owns the runtime,"
which §3.4 states as the actual discriminator.

---

## 5. The v0 shape, and what it deliberately leaves out

**v0 covers:** `conway_init_discover`/`conway_init` (build+own a
multi-thread `tokio::Runtime`, load config the way `ConwayBuilder::
discover()` does) and `conway_shutdown`; `conway_new_session`/
`conway_session_destroy`; `conway_session_prompt` returning an opaque
`ConwayTurn*`; pull-based event draining (`conway_turn_next_event` with a
`timeout_ms`); text/result extraction; a coarse `ConwayErrorKind` enum plus
a full-fidelity JSON string behind the same `DiplomatWrite` mechanism every
other string uses. This is a direct C projection of `crates/conway/
examples/minimal_session.rs`'s real path — `ConwayBuilder::discover()` →
`new_session` → `session.prompt(text)` → drain the returned turn — matching
`INTENT.md` §7's own "going straight to the model is a composition, not a
feature" for a non-Rust caller, the same as it already does for a Rust one.

**v0 deliberately does not cover, named so a gap reads as a gap and not a
silent omission (per §7c's own closing note):**

- **Fork/spawn/steer/cancel/tree.** `SessionHandle`'s subagent-control
  surface (`fork`, `spawn`, `steer`, `await_agent`, `cancel_with`, `tree()`)
  is real and documented (§2) but is not part of "ask a model a question,
  get an answer," which is what a v0 projection is for. Projecting it is
  more surface, not more difficulty — the same opaque-handle-plus-block_on
  shape applies uniformly — so it is a follow-up item's scope, not a
  design risk deferred.
- **Push-style event subscription.** A callback-based
  `conway_session_events_subscribe(fn_ptr, user_data)` is a legitimate
  addition for hosts that prefer it, but it must document plainly which
  thread `fn_ptr` runs on (a `conway-c`-owned background task on a runtime
  worker thread) and that the host owns its own thread-safety from there —
  the same disclosure every callback API of this shape carries. Left out
  of v0 deliberately: pull-based is sufficient for the target use case, and
  a real embedding consumer asking for push is the right trigger to build
  it, per this project's own "nothing is built on theory" rule
  (`INTENT.md` §8.5).
- **`ConwayError`'s `#[non_exhaustive]` enum, projected exactly.** Diplomat's
  enum codegen wants either full exhaustiveness or an explicit catch-all
  arm; `ConwayError` is deliberately open-ended (new variants are expected
  as sibling work lands — its own doc says so). The v0 projection is the
  coarse-enum-plus-full-JSON shape §2 already names, which sidesteps the
  exhaustiveness question rather than resolving it — flagged as a concrete
  implementation question for whoever builds `conway-c`, not settled here.
- **A non-C target language.** C is the first target per this
  recommendation; C++, JS, or Kotlin bindings Diplomat can generate "for
  free" from the same bridge module are real, deferred capability, not
  part of what v0 promises.
- **Any shared transport with the out-of-process plugin host.** conway
  already has an out-of-process extension mechanism (subprocess/MCP
  plugins) answering a *different* question — non-Rust code conway calls
  *outward* to, not non-Rust code calling *inward* into conway. An
  in-process C ABI (scalars, opaque pointers, caller-supplied buffers, no
  framing) and an out-of-process protocol (message framing, request/
  response correlation, process supervision) do not share mechanics, and
  forcing one design to serve both would cost both — wrong for the
  embedded targets this item names, wrong for a wire protocol that has to
  survive process boundaries. The one thing legitimately shared is a data
  model, not a transport: conway's error and tool-facing types are already
  serde round-trippable in an externally-tagged JSON shape
  (`conway_core::error`'s own doc), and both the out-of-process protocol
  and this item's error projection should serialize through that same
  shape rather than each inventing one.

---

## 6. Sources consulted, this run (2026-08-24)

- `crates/conway/src/{lib.rs, session_handle.rs, event_stream.rs, error.rs,
  builder.rs}`, `crates/conway/Cargo.toml`, `crates/conway/src/host_caps.rs`
  — read in full or by targeted section for this item, to re-verify the
  public surface against `BINDINGS.md`'s 2026-08-14 citations.
- `docs/vision/BINDINGS.md`, `docs/vision/PLAN.md`, `docs/vision/
  STATE-OF-THE-UNION.md`, `docs/embedding.md` — read for this item's own
  context and the discrepancy noted in §0.
- [Diplomat repository](https://github.com/rust-diplomat/diplomat) and its
  [README](https://github.com/rust-diplomat/diplomat/blob/main/README.md)
  (fetched this run) — confirmed current target-language list: "C, C++,
  Dart, Javascript/Typescript, .NET (C#), Kotlin (using JNA), Python (using
  nanobind)"; current published crate version `0.16.1` (`docs.rs/diplomat`,
  queried this run).
- [async-ffi docs](https://docs.rs/async-ffi/latest/async_ffi/) — the
  `catch_unwind`-around-poll pattern.
- [Manish Goregaokar, "Diplomat: Multi-language FFI for Rust
  Libraries"](https://manishearth.github.io/blog/2026/06/14/diplomat-multi-language-ffi-for-rust-libraries/)
  — design rationale.
- [UniFFI Async Overview](https://mozilla.github.io/uniffi-rs/latest/internals/async-overview.html)
  (fetched this run) — *"UniFFI can't rely on a Rust async runtime... it
  piggybacks off the runtime from the foreign bindings"*; the
  `RustFuture`/poll/complete/free scaffolding shape.
- [UniFFI object references guide](https://mozilla.github.io/uniffi-rs/latest/internals/object_references.html)
  (fetched this run) — the `Arc`-backed opaque-handle ownership model.
- UniFFI's own README/user-guide target-language list (Kotlin, Swift,
  Python, Ruby, WASM — no C/C++ code-generation target) — cross-checked
  across the async-overview and object-references pages plus general
  project search this run; UniFFI generates no C header at any point in
  its pipeline.
- [cbindgen repository](https://github.com/mozilla/cbindgen) and
  [docs.rs/crate/cbindgen](https://docs.rs/crate/cbindgen/latest) (fetched
  this run) — current version `0.29.4`, actively maintained, header-only
  scope confirmed.
- [Wasmtime's embedding API docs](https://docs.wasmtime.dev/api/wasmtime/)
  and [`wasmtime_wasi::runtime`](https://docs.wasmtime.dev/api/wasmtime_wasi/runtime/index.html)
  — the own-the-runtime, synchronous-C-boundary precedent this
  recommendation borrows.
- Rust's own documented rule that unwinding across an `extern "C"` boundary
  is undefined behavior (general FFI panic-safety literature, cross-checked
  this run) — the reason every candidate converges on `catch_unwind` at the
  boundary regardless of tool.

---

## 7. Key decisions

1. **Diplomat, first target C, new crate `crates/conway-c` depending on
   `conway` alone.** *Falsify by:* a real corner of conway's surface
   Diplomat's bridge macro cannot express without contortions a hand-rolled
   `cbindgen` layer would have handled more simply (§1).
2. **The binding crate owns the tokio runtime; no async ABI crosses to C.**
   *Verify:* every `extern "C"` entry point is `block_on`-shaped, matching
   wasmtime's own C API precedent, not Diplomat's `async_ffi`
   poll-from-C machinery (§3.1, §3.4).
3. **UniFFI is disqualified on the runtime question, and that is the same
   fact as "UniFFI generates no C bindings at all."** *Verify:* UniFFI's
   own async-overview page states it relies on the *foreign* runtime to
   drive polling, and its target-language list (Kotlin/Swift/Python/Ruby/
   WASM) contains no C (§3.2, §3.4).
4. **cbindgen is the floor, not a contender — it answers none of the four
   questions.** *Verify:* its own scope is header generation from
   already-`#[repr(C)]`/`extern "C"` Rust, confirmed against its current
   docs (§3.3).
5. **Events cross as a pull-based opaque handle plus a poll method with a
   timeout, not a push callback, in v0.** *Verify:* `EventStream` is
   `futures_core::Stream` on the Rust side — nothing to project except a
   drive loop (§2, §5).
6. **Panics are caught with `catch_unwind` at every `extern "C"` boundary,
   converted to a typed error through the ordinary error channel, with
   `panic = "unwind"` pinned and guarded by a test.** *Verify:* the crate's
   resolved Cargo profile, checked the same way `crates/conway/tests/
   architecture_invariants.rs` already checks other `Cargo.toml`-shaped
   invariants (§3.1).
7. **Strings cross via a caller-supplied buffer (`DiplomatWrite`); every
   persistent value is an opaque, explicitly-destroyed handle; `TurnHandle`
   is boxed-and-owned-once, not `Arc`-shared like `SessionHandle`.**
   *Verify:* `TurnHandle` has no `#[derive(Clone)]` in `session_handle.rs`,
   confirmed this run — a discrepancy from `BINDINGS.md`'s original text,
   which discussed only `SessionHandle`'s clonability (§2, §3.1).
8. **No shared transport with the out-of-process plugin host; a shared
   data model (the existing serde-JSON error/tool shape) is not the same
   thing and is fine to share.** *Verify:* `conway_core::error`'s own doc
   already states the serde-round-trippable, externally-tagged shape (§5).

---

## 8. What this settles, and what stays genuinely open

**Settled by this document — "absent" becomes "decided and sequenced," per
this item's own acceptance bar, and no schedule is promised beyond that:**

1. ~~Which tool?~~ Diplomat (§1, §3.4).
2. ~~Which target language first?~~ C (§1).
3. ~~Does this need core changes?~~ No — tested directly against §7c's
   first constraint and confirmed satisfied by construction today, since
   nothing in `conway`'s own source references Diplomat and no
   `crates/conway-c` exists yet to have drifted from that shape (§4).
4. ~~What does v0 cover, and what does it knowingly not?~~ §5, itemized.
5. ~~Should this share machinery with the out-of-process plugin host
   (subprocess/MCP)?~~ No — different transports; a data model may be
   shared, a transport should not be (§5's last bullet).

**Genuinely open, named so the next reader does not have to rediscover
it:**

- **Whether to build `crates/conway-c` at all yet, versus queue it, is a
  prioritization call this document does not make.** `PLAN.md`'s own entry
  for this item says as much: *"Q5 asks only whether to prioritise it."*
  This page answers *how*, not *when* — consistent with this item's own
  acceptance criterion that it must not promise a schedule.
- **The `#[non_exhaustive]`-enum codegen question for `ConwayError`** (§5)
  is a concrete implementation decision for whoever writes the bridge
  module, not resolved in the abstract here.
- **Whether push-style event subscription ships in the first cut** is
  deferred to a real consumer asking for it (§5), per `INTENT.md` §8.5's
  "nothing is built on theory."
- **What `docs/README.md` or `docs/vision/`'s own index should say about
  this page and about `BINDINGS.md`** is not settled here — §0 names the
  recommendation (retire or redirect `BINDINGS.md`, add an index row for
  this page) but does not act on it, since both files are owned by other
  work this round.
