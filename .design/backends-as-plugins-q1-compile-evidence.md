# Backends-as-plugins: Q1 compile evidence, and the reachability-table staleness it found

Board item 01KZHEY78NCGYZCPDNFENF96N4 (head of charter 01KZACKE05ZNYTYR0TGV3550SD).
This note is the compile evidence the item's Q1 asked for, plus what it found
along the way. The five decisions this item settled are `record_decision`
entries (see the board item's own completion report for their ids); this
file is the raw evidence, not a sixth restatement of the reasoning.

## The test

A scratch crate outside this workspace
(`/private/tmp/.../scratchpad/backend-scratch`, deleted before this item
finished — `git status --porcelain crates/` carries nothing from it), whose
`Cargo.toml` names exactly one dependency:

```toml
[dependencies]
conway = { path = "/Users/dan/code/conway/crates/conway" }
```

`src/lib.rs` wrote a full `impl conway::Backend for MyBackend`, naming every
type the trait's five methods (`id`, `capabilities`, `generate`, `stream`,
`probe`) and its one default method (`admit`) require, entirely through
`conway::` paths, then called `cargo build`.

## The result

`cargo build` fails with **17 unresolved-name errors** (`E0425`/`E0433`),
naming exactly these types/functions as absent from `conway`'s public
surface:

- `BackendId`
- `ModelId`
- `Capabilities`
- `GenerateRequest`
- `GenerateResponse`
- `BackendError`
- `BoxStream`
- `StreamChunk`
- `ProbeReport`
- `Admission`
- `check_admission` (confirmed absent in a second compile pass — the spec
  flagged this one and `BoxStream` as commonly omitted from hand-written
  reachability tables)

`Backend` itself resolves fine (it is re-exported at `conway`'s root); every
type its method signatures name does not. So: **no, a crate depending only
on `conway` cannot implement `Backend` today** — full stop, not "mostly."

Full compiler output: `/private/tmp/claude-501/-Users-dan-code-conway/f67f3a78-bbea-4f36-9014-a0bcfc73161b/scratchpad/q1-build-output-final.txt`
(captured this session; not committed — scratch artifact). The invocation
was plain `cargo build` from the scratch crate's own directory, no flags.

## What this confirms, and what it contradicts

`docs/embedding.md`'s "What's reachable from the library, and what isn't"
table (the document the spec named as the thing to verify rather than
trust) gets the `Backend` row **right**: `Yes` / `No` / `No`, with the type
list `GenerateRequest, GenerateResponse, BackendError, StreamChunk,
ProbeReport, ModelId, Capabilities, …` — missing only `BackendId`,
`BoxStream`, `Admission`, and `check_admission`, which the "…" was already
honestly disclaiming as non-exhaustive. That row is not the stale one.

**The same table's `Router` row is stale, and the staleness is now fixed in
the same commit that would have caught it if anyone had re-run this check
after `e86a77c`.** The row reads `Router | Yes | No (...) | No`, citing
`.design/extension-architecture.md` §13.5 ("the extension architecture
rejects plugin implementations of `Backend`, `SessionStore`, `Router`,
`HealthRegistry`, `SubagentHost`, and `EventSink`"). But `RouterFactory`,
`RouterBuildContext`, `RouterBundle`, `Router`, `HealthRegistry`, and
`RoutingExplainer` are now ALL re-exported at `conway`'s crate root
(`crates/conway/src/lib.rs` lines 71–74), and `RoutingConfig`/
`HeadroomPolicy` — the field types `RouterBuildContext` carries — were
re-exported alongside them specifically so a facade-only crate could spell
them (lines 196–206, with the comment saying so explicitly). A crate
depending only on `conway` can write `impl conway::RouterFactory for
MyFactory` today and reach everything `build()` needs. The very same
`docs/embedding.md` file demonstrates this 130 lines later, in its own
"Installing a router: `RouterFactory` and the `[plugins].install` router
arm" section (line 314+), with a complete working example against
`conway::` paths alone — **the doc contradicts itself**, and the older,
now-wrong table row is the one that survived unedited when the newer
section was added.

`crates/conway/src/lib.rs`'s own doc comment (lines 154–156, on the
`plugin` module) has the identical staleness: "The
`SubagentHost`/`EventSink`/`SessionStore`/`Router`/`HealthRegistry`/
`Backend` implementation surfaces — §13.5 rejects plugin implementations of
those with stated reasons" is asserted twenty lines below the same file's
own `RouterFactory` re-export that falsifies it for `Router`.

`.design/extension-architecture.md` §13.5 itself ("No plugin
implementations of `SessionStore`, `Router`, `HealthRegistry`, `Backend`,
`SubagentHost`, or `EventSink`") is the root of both staleness copies and
was written before `RouterFactory` existed; it was not updated when
`e86a77c` shipped. It needs a status note (in the style of its own §13.6
superseded-clause banner) rather than a silent contradiction — flagged
here as a finding, not fixed here (no production or doc changes are this
item's job; `docs/plugins/` is out of bounds for a different reason, and
`extension-architecture.md`/`docs/embedding.md` are simply out of this
item's scope, which is decisions, not edits).

**Net effect on this item's own question:** `Backend`'s "No" is accurate
today, but it is accurate for a reason that is now *inconsistent* with the
codebase's own most recent precedent, not because the codebase treats
`Backend` differently on purpose going forward. `Router` proved that "No"
is not permanent for a port whose selection must precede its construction;
Q2's decision record works out whether `Backend` needs the same kind of
factory seam to cross from "No" to "Yes" the same way, given the
instance/kind identity asymmetry the spec asks about.
