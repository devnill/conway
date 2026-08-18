# Design: the first-class context path

**Board item `01M00QDYK4T5MZNCTQ0ZXEBSZX` (D1-1). Produced 2026-08-14, revised the
same day against `INTENT.md` §5c/§5d/§8.2/§8.6.**

> **Revision 2 — what changed and why.** Reviewing revision 1's five open questions
> produced a specification gap rather than five answers, which is `INTENT.md` §8.1
> working. The gap: **a path is identified twice** — by its *selection* (which
> records, in what order: model-free, durable, curatorial) and by its *rendering*
> (the wire bytes: model- and config-specific, disposable). `prefix_key` is the
> second. Revision 1 used it as both.
>
> Three things changed. **(a)** A model-free `SelectionKey` is introduced (§2.3) and
> the head is keyed on it (§2.5); `prefix_key` is untouched and keeps its cache job.
> **(b)** The static prefix moves out of the selection entirely (§2.2, §2.4) — §5c's
> own table names the system prompt and tool set as *rendering* inputs — which
> simplifies the node type and retires most of `PathError::VersionDrift` (§2.7).
> **(c)** The coherence invariants are expressed as a **seam wrapper** rather than
> call-site checks, per §8.6 (§4.1).
>
> Unchanged and not relitigated: §4.1's two-constructor coherence boundary, §4.2's
> cost model, §4.4's retention rule, §5's line-holding argument.

Settled inputs are `INTENT.md` §5a, §5b, §5c and §5d; this page does not re-derive
them.

---

## 1. The approach, in one paragraph

A **path** is an ordered list of *references* to immutable records — nothing else —
plus an optional reference to another path **selection** as its prefix. Assembly
stops being a fixed rule over `(inherited, head, own)` and becomes: render the static
preamble from live config exactly as today, then fold over the path's node list. The
fixed rule survives as **one** constructor, `default_path`, which selects everything
and excludes nothing. A **selection** is a frozen path identified by a new,
model-free, content-addressed `SelectionKey` over its node list. A **head** is the
latest `LogRecord::ContextPathSet` in a session's own append-only log, referencing a
`SelectionKey`, which makes "owned by exactly one session" a consequence of the
store's existing one-writer-per-log discipline rather than a new lock. Referencing
another session's *head* is not refused at runtime — it is **unrepresentable**,
because a path body can only hold a `SelectionKey` and there is no way to spell a
head.

Two identities, two jobs, and the law that relates them:

> **equal selection + equal statics + equal model ⟹ equal `prefix_key` ⟹
> byte-identical wire prefix.**

The second implication is already pinned by
`crates/conway/tests/prefix_key_wire_identity.rs` (D1-2b, landed). The first is what
this design adds, and it is what makes §5b's "one named selection, ten heads"
structural rather than a coincidence somebody maintains. Nothing here changes
`prefix_key`, its inputs, or its value for any existing session.

---

## 2. The layers as concrete types

### 2.1 Record = blob

```
RecordRef { session: SessionId, seq: LogSeq }
```

`LogRecord` carries no session id (`log.rs:159–279`), so `(SessionId, LogSeq)` is the
only addressing that exists and the only one needed. Records are never copied, never
rewritten.

### 2.2 Path node

```
PathNode { record: RecordRef, stamp: NodeStamp, prov: NodeProvenance }

NodeStamp::Inherited { from: SessionId } | NodeStamp::Head | NodeStamp::Own

NodeProvenance { selected_by: Selector, at: DateTime<Utc> }
Selector::DefaultRule | Selector::Plugin { id: String, op: OpLabel } | Selector::Operator
```

**A node is always a record.** Revision 1's `NodeSource::Assembled` arm — declarative
references to the system prompt, skills and tool registry — is **deleted**; see §2.4.
`NodeStamp` loses its `Static` arm with it, and the type gets simpler, which is the
usual signal that the boundary moved the right way.

`NodeStamp` decides which of the two existing mapping functions runs: `Inherited` →
`record_role_and_content` + `Provenance::Inherited`; `Head` → today's `HeadSegment`
mapping (which forces `UserPrompt`/`ForkDirective` regardless of stored provenance,
`agent_loop.rs:1723`); `Own` → `own_segment`. Keeping the stamp on the *node* rather
than deriving it in the builder is what makes byte-identity mechanical (§3).

The stamp is also where the tier boundary comes from: `prefix_key`'s `boundary_index`
is "the last non-`Volatile` segment" (`prefix.rs:14–18`), and under the mapping above
that is **the last `Inherited`-stamped node**, with `Head` and `Own` volatile. So the
frozen portion of a rendering is *statics + `Inherited` nodes*, unchanged from today.
Committing a curated selection so children can share its cache is exactly the act of
**re-stamping** `Head`/`Own` nodes as `Inherited { from: <true owning session> }` — a
selection-layer operation whose caching consequence follows in the rendering layer,
never the other way round.

### 2.3 Selection = the durable identity

```
SelectionKey(String)   // blake3 hex, transparent serde, same shape as PrefixKey

blake3( "conway.selection.v1" ‖ canonical_json_bytes(projection) )
```

where `projection` is a JSON array, one entry per node **of the fully expanded list**
(prefix chains flattened first), each entry being exactly
`{ "record": { "session": …, "seq": … }, "stamp": … }`.

**Deliberately excluded, each for a reason that must survive a later reader** —
stated in the style of `prefix.rs`'s own doc, because that doc is why its exclusions
have held:

- **`ModelId`.** A selection is model-free. This is the whole point of §5c: switching
  models invalidates the rendering, which is a cache and is *supposed* to be
  invalidated, and leaves the curation untouched.
- **Everything in the static preamble** — agent-def name and text, skill names and
  texts, tool-registry hash. See §2.4.
- **`NodeProvenance` (`selected_by`, `at`).** Who curated, and when, is a fact about
  the *act*, not about the selection. Two curators reaching the same selection by
  different routes must hash equal, or the ten-heads-one-selection sharing story
  fails on the first plugin that stamps its own name. Same class of exclusion, for
  the same reason, as `prefix_key` excluding per-agent `SegmentId` so siblings hash
  equal.
- **The `incoherence` declaration (§4.1).** Derivable from the referenced content, so
  including it would change no equality and would make the key depend on reading
  every record.
- **Record content.** A reference names an immutable blob; a git tree names blobs by
  id and does not re-hash them. Disclosed limit: a selection key is therefore only as
  trustworthy as the log's immutability — a hand-edited session file silently changes
  what a stored selection means. That invariant is already load-bearing everywhere
  else in this tree; this design leans on it rather than duplicating it.
- **How the selection was chunked into prefix references.** The projection is over
  the *expanded* list, so `prefix(A) ++ [n1, n2]` and the flat equivalent hash equal.
  Without this, sharing would depend on how a curator happened to batch its commits.

The key is cheap: it hashes references and order, never bytes of content, and never
reaches the wire.

> **Implementation note, verified by the invoking session — this revision has one
> concrete defect here.** It says to reuse `canonical_json_bytes` "in
> `context/prefix.rs`". That function is `pub(crate)` in **`conway-runtime`**
> (`context/prefix.rs:61`), and `SelectionKey` is specified to live in
> **`conway-core`**, which does not and must not depend on `conway-runtime`. It is
> not reachable as written.
>
> The fix is small and improves the tree: **move `canonical_json_bytes` into
> `conway-core`** and have `prefix.rs` consume it from there. `blake3` and
> `serde_json` are already `conway-core` dependencies, and the function is a pure
> canonicalizer with no policy in it, so it fits the contract crate's charter. It
> also retires a duplicate — there is a **third** copy in
> `crates/conway-plugin-stepguard/src/lib.rs:153`, and three independent
> canonicalizers is exactly the drift hazard that makes two hashes disagree for
> reasons nobody can find. Fold this into D1-3a.

### 2.4 Statics belong to the rendering, not the selection

This was the last thing gating implementation, and it now has an answer.

**Decision: the static preamble is part of the rendering. It leaves the selection
entirely.** `ContextInput` keeps `system_prompt`, `skills` and `tools` exactly as they
are; the builder emits `[0]`/`[1]`/`[2]` from live config exactly as today; the path
describes the *record* portion of the context and says nothing about them.

Four reasons, in the order that decided it:

1. **§5c's table says so literally.** *Rendering — depends on: model, system prompt,
   tool set.* Revision 1 was written before that table existed and put two of those
   three inside the selection.
2. **The forcing case generalizes one layer up.** Changing model mid-session is
   ordinary; so is editing an agent definition, adding a skill, or installing a plugin
   that registers a tool. Under revision 1, adding one tool changes the tool-registry
   hash and therefore invalidates *every stored selection in the store*, each refusing
   loudly until re-derived. That is the exact failure §5c identifies for models, and it
   would arrive far more often.
3. **A "selection" that always contains the same three things is not selecting.** The
   statics are unconditional by the fixed ordering — no curator ever chose them, and
   none can omit them. Nobody curating a context is making a claim about a tool-schema
   hash.
4. **§8.2's core-surface test discriminates.** *Does this encode a judgment two
   reasonable people could answer differently?* "Is the tool registry part of what
   identifies this curation?" — plainly yes, people would differ. The answer encoding
   no judgment is the one where identity depends only on what the curator actually
   chose.

### 2.5 Head = branch ref

A new `LogRecord` variant, in the owning session's own log:

```
ContextPathSet { seq, ts, selection: SelectionKey, covers_upto: LogSeq }
```

`covers_upto` is the watermark in **local units**, the same units `resolver.rs` uses.
Assembly is `expand(selection) ++ own records from covers_upto`. That is how a live,
appending session points at an immutable selection: the mutable tail is deliberately
outside it.

Because the key is model-free, **a head survives a model change untouched.** Switching
models re-renders, re-computes `prefix_key`, spends the cache, and changes nothing
about what the head names. That is the behaviour §5c requires and the reason revision
1's head was backwards.

**Absence of the record means the default path** — the entire migration story (§6).

Ownership is structural: another session cannot append to a log it does not own, so it
cannot move this head. Nothing enforces ownership at runtime because nothing can
violate it.

**Naming.** §8.2 settles that the core may hold a name→selection binding — a pointer
encodes no judgment — while *which* names exist is policy and stays out. The binding
lives where the head lives, as a sibling record
(`ContextPathNamed { name, selection }`) in the owning session's own log. Consequence,
accepted deliberately: names are **session-scoped**. A global mutable namespace would
need an owner, and no session owns it, which reintroduces the head-ownership problem
one layer up. Two sessions may use the same name for different selections, and that is
not a collision because neither can see the other's.

### 2.6 Selection object = commit

```
PathSelection {
    prefix: Option<SelectionKey>,     // another SELECTION, never a head
    nodes:  Vec<PathNode>,
    incoherence: Vec<HarnessDrop>,    // §4.1
}
```

Immutable, globally readable, freely referenced, stored content-addressed under its
`SelectionKey`. Because the key is model-free, **ten siblings routing to four models
share one stored object**, where revision 1 would have stored four identical bodies
under four keys.

`prefix` transitivity is expanded with a depth bound and
`PathError::PrefixChainTooDeep`, the same shape as `resolver.rs`'s
`MAX_ANCESTRY_DEPTH`. Cycles are impossible by construction but a hand-edited store is
not, so the bound stays.

Write ordering, stated because a crash in the window is otherwise silent: **the
selection object is stored before the `ContextPathSet` record is appended**, so a head
never points at a missing body. Same discipline as appending the assistant record
before persisting its context report.

### 2.7 What remains of `VersionDrift` — and the discriminator that settles the next case

Revision 1 refused when the statics behind a referenced version had drifted. With
statics out of the selection there is nothing to drift *from*: the selection made no
claim about them. Editing an agent definition changes the rendering, changes
`prefix_key`, spends the cache, and leaves the curation exactly honoured. No refusal.

That is not §8.3 being softened, and the discriminator must be written down or the next
reader will get it wrong:

> **Refuse when the thing referenced cannot be produced. Report when it was produced
> exactly, but costs more.**

A cache miss is the second kind. `PHILOSOPHY.md` already holds that caching is never
correctness-bearing; refusing on a price change would be conway acquiring an opinion
about which prices are acceptable, which is policy. But a silent miss "looks exactly
like an expensive workload rather than like a bug" (§5b), so it must be **observable**:
`RenderDivergence { expected, actual, first_divergence }`, surfaced on the context
report and as an event when an expected shared prefix was not achieved. Loud, free,
never fatal.

The refusals that survive, so the rule keeps teeth:

- **`PathError::UnresolvableNode { record, detail }`** — a `SelectionKey` absent from
  the store, or a `RecordRef` whose session is gone. Retention (§4.4) makes this
  unreachable through sanctioned operations; a corrupt store is not.
- **A selection that no longer fits after a model change** — §5c assigns this to
  admission, which already exists: the backend refuses and names the shortfall. The
  path layer must **not** pre-empt it by trimming. See §10.

### 2.8 `ValidatedPath` — the type that gates assembly

```
ValidatedPath                          // private fields, two constructors only
  ::default_path(..)                   // includes everything; declares, never refuses
  ::derive(base, ops)           -> Result<Derivation, PathError>   // refuses
  ::derive_reordered(base, ops) -> Result<Derivation, PathError>
```

`ContextBuilder::build` accepts only a `ValidatedPath`. There is no third way in.

### 2.9 Where each type lives

| Thing | File |
| --- | --- |
| `RecordRef`, `PathNode`, `SelectionKey`, `PathSelection`, `ValidatedPath`, `PathOp`, `Derivation`, `CostEstimate`, `PathError`, coherence validator | **new** `crates/conway-core/src/path.rs` |
| `canonical_json_bytes` (moved from runtime — see §2.3 note) | `crates/conway-core/src/` |
| `ContextPathSet`, `ContextPathNamed` variants | `crates/conway-core/src/log.rs` |
| `ContextReportEntry::origin`, `ContextReport::path`, `RenderDivergence` | `crates/conway-core/src/provenance.rs` |
| `PathStore` port (keyed by `SelectionKey`) | **new** `crates/conway-core/src/ports/path_store.rs` |
| `GuardedContextHook` wrapper (§4.1) | `crates/conway-core/src/ports/plugin.rs` |
| `FsPathStore` + reverse index | **new** `crates/conway-session/src/path_store.rs` |
| `resolve_path` | `crates/conway-session/src/resolver.rs` |
| `default_path`, head resolution | **new** `crates/conway-runtime/src/context/path.rs` |
| assembly over a path | `crates/conway-runtime/src/context/builder.rs` |

All inside D1's declared ownership in `PLAN.md`. The CLI verbs in §4.5 are a **request
to D4**, not a change specified into their files.

---

## 3. How assembly changes

`ContextInput` loses `inherited`, `head`, `own` and gains `path: ResolvedPath` (nodes
zipped with their already-read records). It **keeps** `system_prompt`, `skills`,
`tools` unchanged — they are the rendering's static preamble and the path says nothing
about them (§2.4).

The builder stays **pure, non-async, store-free.** Resolution is the caller's job,
exactly as `TranscriptResolver` is today. `resolve_path` reuses the existing memoised
ancestry walk for the default path so the `Arc::ptr_eq` sibling-sharing property that
`crates/conway/tests/fanout_prefix_sharing.rs` depends on survives untouched.
Everything after node→segment mapping is unchanged.

**Byte identity is by construction and provable against artefacts that already exist.**
The builder emits `[0]`/`[1]`/`[2]` from config as today, then folds the path:
`default_path` emits `Inherited` for every record in the resolved prefix, `Head` for
the session's own first record, `Own` for the rest. Since the per-stamp mapping is the
code that runs today, the segment list is identical — so segment ids, `prefix_key`,
cache hints and the report are identical. The proof is
`crates/conway-runtime/tests/context_golden.rs` plus `tests/golden/*` **unchanged and
not regenerated**. A regenerated golden file is the failure signal, not the fix.

One deliberate legacy carry-over, which must be commented where it is encoded or
someone will "fix" it: `InheritedPrefix.from` stamps every inherited record with the
**immediate parent**, not its true author, at fork depth ≥ 2 (`builder.rs:126–141`).
`default_path` reproduces that exactly. A **derived** path stamps the true owning
session, because it has no bytes to preserve. Unifying the two is a one-time cache
invalidation plus golden regeneration and must be its own item. Note the stamp is
inside `SelectionKey`, so the two are honestly different selections — correct, because
they render differently.

---

## 4. The five required answers

### 4.1 Coherence — where the validation boundary sits, and the seam that holds it

**Two constructors, two situations, and the boundary is the constructor.**

- `derive` / `derive_reordered` **refuse.** They run the coherence validator over the
  resolved node list and return `Err(PathError::WouldOrphan{..})`. An invalid derived
  path is never constructed.
- `default_path` **cannot** refuse — whatever incoherence is present was caused by the
  harness (a fork cut mid-batch; a session killed between an assistant append and its
  results, `builder.rs:26–61`). It **tolerates and declares**, recording every
  unanswered `call_id` into `PathSelection::incoherence`.

That declaration is what makes the two impossible to confuse. At render time
`drop_unanswered_tool_calls` stays, but is **reconciled against the declaration**: it
may drop exactly the ids the path declared, and any *other* orphan is a defect in
derivation returning a typed `RuntimeError::IncoherentContext`.

**Where that check lives is now a settled matter of form, not taste (§8.6).** An
invariant belongs to the seam, not to its call sites. So:

- The runtime does **not** hold `Option<Arc<dyn ContextHook>>`. It holds a concrete
  `GuardedContextHook`, whose only constructor takes an `Arc<dyn ContextHook>` and
  which implements the trait by delegating and then running the coherence check on
  whatever came back — for `before_request` **and** `on_overflow`, and for every method
  the trait ever gains. "Did someone remember to wrap it?" is not a question a call
  site can get wrong, because there is no other way to populate the field. Same
  structural taste as `PromptSegment` having no `Default` and `HookPermissionVerdict`
  having no `Allow` — **both verified present in the tree.**
- `on_overflow` is the evidence for the rule, not an afterthought: it went unguarded
  while `before_request` was the one being discussed. A wrapper prevents the *next*
  such miss.
- Assembly's own output goes through one
  `finish_assembly(segments, declared_drops) -> Result<AssembledContext, _>` so every
  producer of a segment list passes the same gate rather than each remembering to.
- The pattern is already satisfied at the third seam: plugin-facing derivation
  validates inside `derive` itself, which is the seam.

**Scope note.** The live `ContextHook` coherence hole this design found is filed and
being fixed ahead of the path work (`01M00RGARPESWXYAVY960KDE7S`). The recommendation
this design owes that item: **implement it as the wrapper, not as two call-site
checks** — otherwise the fix reproduces the exact shape §8.6 was written to retire.
D1-3e then narrows to reconciling the render-time repair against declared harness
drops, inside the same wrapper.

The validator has **three** rules, not one:

1. every `ToolUse` call_id has an answering `ToolResultBlock` on the path;
2. every `ToolResultBlock` has its `ToolUse` on the path;
3. each result appears **after** its call.

Rules 2 and 3 are new: `builder.rs:521–526` states the result-without-call direction
"could not be made to fail" — true for prefix cuts, **false for arbitrary selection and
for reordering.** That comment must be amended in the same change. All three trace to a
provider requirement, which is what makes them legitimate under §5d, and each must be
documented where a curator will hit it — which is the `PathError` `Display` text.

**What a curation plugin receives when its derivation is rejected** — not prose, a
re-submittable value:

```
PathError::WouldOrphan {
    orphans: Vec<Orphan>,   // { call_id, tool, call_node, result_node, rule: 1|2|3 }
    offers:  Vec<PathOp>,   // each, applied to the same base, validates
}
```

`Display` renders the human sentence — *"omitting session 01H…/seq 7 orphans call
`tc_3` issued in seq 6; also omit seq 6, or keep seq 7"* — and `offers` carries both
candidate repairs, so a plugin can retry programmatically. **The harness offers; it
never picks.**

### 4.2 Rearranging costs strictly more than omitting

1. **There is no "set the node list" operation.** `PathOp` is `Omit` / `Include` /
   `Move{node, before}` / `Restamp`. You cannot reorder by accident.
2. **Reordering is a different function.** `derive` refuses any `Move` with
   `PathError::ReorderRequiresExplicitDerivation`; `derive_reordered` accepts it. The
   cheap operation gets the short name.
3. **The price returns with the result, before anything is sent** — measured
   structurally, at the selection layer, where it is model-free and honest:
   ```
   CostEstimate {
       shared_prefix_nodes, shared_prefix_tokens_est, discarded_prefix_tokens_est,
       first_divergence: Option<RecordRef>,
       divergence_kind: None | Omission | Reorder,
       divergence_inside_frozen_tier: bool,
   }
   ```
   Whether a given structural divergence actually costs a cache hit is a *rendering*
   question, answered at assembly and reported as `RenderDivergence` — the derivation
   promises structure, not prices it cannot know.
4. **The economics fall out of the tier boundary for free, and the boundary is now
   model-free too.** `divergence_inside_frozen_tier` is "does the first divergence fall
   at or before the last `Inherited`-stamped node" — a pure function of the node list.
   That is §5b's "dropping from the tail is nearly free, dropping from the head spends
   everything", mechanical rather than rhetorical, and computable without the model.

### 4.3 Provenance survives a graph drawn from several sessions

- **Every node carries its true `RecordRef`**, for every stamp.
- **Every node carries why it is there** (`selected_by`, `at`) — deliberately *outside*
  `SelectionKey` (§2.3): provenance travels with the selection but does not identify
  it.
- **`ContextReport` carries it to every existing consumer:**
  `ContextReportEntry::origin` and `ContextReport::path` (selection key, head session,
  derivation chain, any `RenderDivergence`), all `#[serde(default)]` — the pattern
  `dropped` established.
- **The head's history is the log.** Every `ContextPathSet` append is an ordinary
  record with seq and ts.
- **No new `Provenance` variant.** Selection provenance lives on the node.

Note what each answers: `Provenance::Inherited.from` = *who handed me this*;
`ContextReportEntry.origin` = *who wrote it*. Only the second survives multi-session
composition.

### 4.4 Retention — the stated rule

> **A selection pins every record it references. A session with any record pinned by a
> reachable selection cannot be removed.**

Mechanism reuses `SessionStore::remove`'s existing guard matrix
(`ports/session.rs:63–94`): one added clause returning `StoreError::NotRemovable`
naming the pinning selection and the session owning its head. Enforcement uses a
`session → [selection keys]` reverse index, rebuildable by scanning bodies — a derived
accelerator, never a source of truth, exactly `SessionIndex`'s discipline.

- **`/ask` discard and `Conway::pull_in`'s purge fail loudly** when pinned. The TUI's
  modal-ask sweep must treat a pin refusal as *skip and report*, never swallow it.
- **Pinning is not retroactive.**
- **Unpinning is garbage collection, never mutation** — collected only by an explicit
  `conway path gc`. Never automatic, never on a timer.
- **`Conway::pull_in` stays unchanged** as the explicit "I want the content and want
  the log gone" escape hatch — the one place copying is sanctioned, because the
  operator asked for it by name.

### 4.5 A person must be able to inspect a rearranged context

1. **`/context` gains origin** — `session/seq · provenance · selected-by · tokens`,
   plus a `RenderDivergence` line when an expected shared prefix was not achieved.
2. **`conway path` verbs** — a **request to D4**: `show`, `diff` (kept / omitted /
   **moved, with distance**, plus cost delta), `log`, `show --node`, `gc`. Load-bearing
   rather than convenience — see §10.
3. **The persisted per-turn report** already answers this after the fact.

The bar: a curator's decision must be visible without re-running the curator. That is
why `selected_by` is persisted, and why `diff` must render a move as a move rather than
a delete plus an insert.

---

## 5. The line that must not move

**This design puts one selection rule in the core, and names it: `default_path`.** It
includes everything, in log order, drops nothing, reorders nothing — the identity
function over today's behaviour. Against §8.2's test: two reasonable people
implementing "include everything, in order" produce the same thing, so it is mechanism.

> **`conway-core`, `conway-session` and `conway-runtime` ship exactly one path
> constructor, and it excludes nothing. The path constructors (`default_path`,
> `derive`, `derive_reordered`) emit no `PathOp` internally — they receive ops from
> curators. The one place core *does* construct `Omit` / `Include` / `Move` is
> `offers_for` (§4.1: "the harness offers; it never picks"), which builds repair
> *suggestions* handed to a curator — not path-constructor output. They construct
> `PathOp::Restamp` nowhere outside tests (no curator exists yet).**

Enforced by extending `crates/conway/tests/enum_variant_construction_guard.rs` to
`PathOp`: the guard catches the variant that stays inert (`Restamp`, allowlisted +
disclosed as not-yet-implemented until a curator constructs it); `Omit` / `Include` /
`Move` pass as constructed. Vigilance is not a mechanism; a failing test is.

One standing exception, named so it is not quietly widened:
`drop_unanswered_tool_calls` **is** core code that removes records. Justified solely by
a provider hard requirement — §5d's first kind — recorded in `ContextReport::dropped`,
and now reconciled against a declaration inside the seam wrapper. The moment it drops
something for a reason other than "the request is otherwise unsendable", the line has
moved.

---

## 6. Migration and compatibility

- **Wire bytes:** unchanged for the default path. Proof = existing goldens and
  `fanout_prefix_sharing.rs` passing *without regeneration*.
- **`prefix_key`:** untouched. `SelectionKey` is additive and never reaches the wire.
  `prefix_key_wire_identity.rs` continues to protect the rendering half of the
  composition law; a new test protects the selection half.
- **Log format:** two new variants, both ignored by older readers.
- **Existing sessions:** no migration at all.
- **Model changes:** a head survives one untouched.
- **Agent-definition / tool-set changes:** no longer invalidate anything in the
  selection layer. Reported as `RenderDivergence` when a shared prefix was expected.
- **`ContextReport` / `Provenance`:** additive only / no new variants.
- **`ContextMask`:** becomes sugar for a persisted `Omit` on the owning session's head;
  `apply_context_mask` retires **in the same change**. The semantic change must be
  stated: today a mask affects only fork-prefix resolution and not the owning session's
  own assembly; under the path model it affects both.
- **`ContextHook`:** unchanged trait signature; the runtime's field type becomes the
  guard wrapper.

---

## 7. Decomposition

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D1-3a | **Path vocabulary in `conway-core`** — types, `SelectionKey` with its exclusion doc, the three-rule validator, the `PathOp` construction guard, **plus moving `canonical_json_bytes` into core** (§2.3 note). No wiring. | L | D1-1 |
| D1-3b | **`PathStore` port + `FsPathStore`** — content-addressed write-once objects, rebuildable reverse index. | M | D1-3a |
| D1-3c | **Head + naming + `default_path`** — `ContextPathSet`, `ContextPathNamed`, head resolution, `resolve_path` preserving `Arc` sharing. | M | D1-3a |
| D1-3d | **Assembly over a path** — byte-identity proof against unregenerated goldens, plus the composition-law test. | L | D1-3c |
| D1-3e | **Render-time repair reconciliation** — inside the existing guard wrapper. Narrowed by `01M00RGA…`. | S | D1-3d |
| D1-2 | **Cost legibility** — `CostEstimate` and `RenderDivergence` reporting. | M | D1-3d |
| D1-4 | **Structural selection predicates** — mechanism only. | M | D1-3d |
| D1-5 | **Fold in `ContextMask`** (`01KZY8QRAVVVKCRBZ6HAEGW3GG`). | M | D1-3d |
| D1-6 | **Inspection** — report origin fields + `/context` render. | S | D1-3d |
| D1-7 | **`conway path` verbs** — request to D4. **Gating, not cosmetic** (§10). | M | D1-3d, D1-6 |
| D1-8 | **Curation plugin capability.** Without it no curation plugin can exist and all of D1 is inert. | L | D1-3d |
| D1-9 | **Retention enforcement.** | M | D1-3b, D1-3c |

---

## 8. Findings in the tree

1. **A `ContextHook` can produce an incoherent request today, silently. Verified.**
   Filed as `01M00RGARPESWXYAVY960KDE7S`, fixed ahead of the path work.
2. **`builder.rs:521–526` is false under selection.**
3. **Provenance already does not survive multi-session composition**
   (`builder.rs:126–141`). A present defect, fixed in the path and report, deliberately
   not in the wire bytes.
4. **`prefix_key` is a rendering identity, and revision 1's settled input mis-typed
   it.** "Do not invent a second identity" was correct about *cache* identity and is
   honoured: `prefix_key` is unchanged and remains the only rendering identity.
   `SelectionKey` is the identity of a different thing.
5. **`remove`'s dangling-reference guard sees children only.**
6. **`Conway::pull_in` copies and purges** — retained deliberately as the
   discard-the-log variant.
7. **Plugins have no transcript-read capability. Verified.** `CommandCtx` carries only
   `focused_agent`, `root_agent`, `session_id`, `args`.
8. Minor: assistant turns map to `Provenance::SystemNote{reason:"assistant_turn"}`.
   **Do not "fix" during D1** — it changes bytes.

---

## 9. Key decisions

1. **A path is identified twice, and this design holds both.** *Verify:* a selection
   key is equal before and after a model change; the `prefix_key`s differ.
2. **A head references a `SelectionKey`, never a `PrefixKey`.** *Verify:* switching
   models re-renders while the `ContextPathSet` record is untouched.
3. **The static preamble belongs to the rendering.** *Verify:* editing an agent def
   changes no selection key anywhere in the store.
4. **Refuse when a reference cannot be produced; report when it was produced exactly
   but costs more.** *Verify:* a drifted preamble produces a reported divergence and a
   successful turn; a missing selection object refuses.
5. **A head is the latest `ContextPathSet` in the owning session's own log; names bind
   alongside it and are session-scoped.** A global name registry is refused because no
   session could own it.
6. **A path body may reference a selection and cannot reference a head.**
7. **Two constructors: `derive` refuses, `default_path` declares.**
8. **Coherence invariants live in a seam wrapper, not at call sites.** *Verify:* the
   runtime's field type makes an unwrapped hook unrepresentable.
9. **Reordering is a separate function with a structurally-measured price.**
10. **A selection pins its records; unpinning is explicit GC only.**
11. **Provenance lives on the node and outside the selection key.**
12. **The core ships exactly one path constructor and it excludes nothing.**

---

## 10. What is settled, and what is genuinely open

All five of revision 1's questions now follow from written principle, recorded with the
principle that settles each so the next reader derives them instead of asking:

1. ~~Model-scoped version identity.~~ **§5c.** Two identities; the head takes the
   model-free one.
2. ~~Do selections need human names?~~ **§8.2.** The core may hold the binding; which
   names exist is policy. Session-scoped, in the log.
3. ~~Static drift: refuse or re-key?~~ **§5c + §8.3.** Neither — statics leave the
   selection, so there is nothing to drift from. §8.3 keeps its teeth on unresolvable
   references and non-fitting selections; a price change is reported, not refused.
4. ~~Persist the harness-drop declaration?~~ **§5b's recording rule.** Yes, on the body
   — and excluded from the key, being derivable.
5. ~~Ordering rule 3 makes some reorderings illegal.~~ **§5d.** A provider-required
   constraint: legitimate, and stated plainly where a curator hits it.

**Genuinely open — one, and it is the operator's:**

> **May a selection include records from a session outside the current session's
> ancestry** — a sibling's, another project's, an unrelated tree's? Retention (§4.4)
> makes it *safe*; nothing in `INTENT.md` makes it *permitted*, and it is adjacent to
> confinement (`SubagentSpec::root`, D2's domain) rather than to paths. The mechanism
> admits it by construction — a `RecordRef` names any session — so if the answer is
> "no", that must be an explicit rule with an owner, not an accident of what nobody
> tried.

**One consequence to check before D1-7 is scheduled as polish.** §5c assigns a
no-longer-fitting selection to a loud refusal from admission, and the operator or a
plugin curates again. But a default build ships **no curation plugin**. So the honest
Tuesday experience of switching to a smaller model mid-session is: a loud, correct
refusal and no installed way to act on it. Holding the line is right; being unusable is
not (§8.7). The resolution costs nothing extra and is already here — **`conway path`
verbs make a human a first-class curator with no plugin at all** — which is why D1-7 is
gating rather than cosmetic, and why the admission refusal should name the shortfall
*and* point at those verbs.

**Disclosed limits:**

- A `SelectionKey` names content indirectly, so it is only as trustworthy as the log's
  immutability. Hashing content instead would require reading every referenced record
  from every referenced session to compute a key; the trade was made deliberately.
- A selection is durable but its *rendering* is not reproducible after configuration
  changes: replaying an old selection under a new agent definition produces different
  bytes, correctly and by design. Byte-level reproduction of a past turn is what the
  persisted `ContextReport` is for.

---

## 11. The curation plugin (D1-8), elaborated

*Added 2026-08-14. D1-8 was listed as "transcript read + derive + set-head for
plugins" and flagged as the item that decides whether any of D1 ships value. That is
too thin a description for something load-bearing, and elaborating it produced a
finding about which seam curation belongs on.*

### 11.1 What it is

A **curator** is a plugin that changes *what is on a path* — which records, in what
order — as opposed to changing what those records render to. Compaction is a curator.
Memory is a curator. A "drop the exploration, keep the diff" policy is a curator.

Nothing else in this design ships a curation policy, by construction (§5). So the
curator capability is the thing that turns the whole path mechanism from a data model
into something anyone can use, and until it exists D1 is inert.

### 11.2 The capability gap today

`CommandCtx` carries exactly `focused_agent`, `root_agent`, `session_id`, `args` — no
session handle, no transcript, nothing. **A plugin today cannot read the conversation
it is attached to.** `conway-plugin-history` discloses this in its own module doc, and
it is why `/conway.history.rewind` takes a sequence number as an argument rather than
choosing one: it cannot see what it would be rewinding past.

So D1-8 is not "add three methods". It is the first time a plugin is given read access
to conversational state at all, which makes its capability boundary a real design
question rather than a formality.

### 11.3 The finding: `ContextHook` is the wrong seam for curation

The obvious implementation is "curation is a `ContextHook`" — that port already exists,
already runs every turn, already returns a modified payload. Under the path model it is
the wrong layer, and the mismatch is exact:

| | `ContextHook` | what a curator needs |
| --- | --- | --- |
| Runs | **after** assembly | **before** assembly |
| Operates on | `Vec<PromptSegment>` — rendered | `PathSelection` — references |
| Sees | bytes, per-model | records, model-free |
| Its edit is | a rewrite the harness cannot validate | a `derive` the harness validates |
| Cache cost of its edit | unknowable until it has already happened | returned before it commits (§4.2) |

Every advantage this design claims for mechanical cherry-picking — byte-identical
records, knowable cache cost, refusal instead of silent repair, structural predicates —
is available at the selection layer and **not** at the segment layer. A compactor
written as a `ContextHook` is exactly the summarize-and-rewrite shape `INTENT.md` §5a
argues against, because segments are all it can see.

So the recommendation is a **second, separate port** at the selection layer, and the
two do not compete:

- **Curator** — selection layer. Chooses what belongs. Model-free. Validated.
- **`ContextHook`** — rendering layer. Keeps the jobs that genuinely are about bytes:
  masking a secret in rendered text, instrumenting a system prompt, narrowing the
  announced tool set (which is not a path concern at all and has no selection-layer
  equivalent).

This is a net simplification even though it adds a port, because it moves compaction —
the single most-wanted context plugin — off a seam where it could only be implemented
badly.

### 11.4 Shape

```
trait Curator {
    async fn curate(&self, ctx: &CurateCtx, base: &ValidatedPath)
        -> CurateOutcome;
}

enum CurateOutcome {
    Unchanged,                      // the overwhelmingly common case, cheap
    Derived(Derivation),            // already validated by derive(); carries CostEstimate
    Failed { reason: String },      // recorded, non-fatal (§11.6)
}
```

The plugin never constructs a path directly — it calls `derive`/`derive_reordered`,
which is where refusal and `offers` live (§4.1). `CurateOutcome::Derived` can only be
built from a `Derivation`, so **an unvalidated path cannot reach the runtime**: the
same "make it unrepresentable" move as `GuardedContextHook`, one layer up.

### 11.5 What it receives, and one nuance worth stating

`CurateCtx` carries the current resolved path with per-node token estimates and
provenance, plus the routing outcome and the target model's window.

The nuance, because it will otherwise be read as a contradiction of §5c: **a curator
may *read* model-dependent facts to decide whether to act, while what it *produces*
stays model-free.** "Am I close to the limit?" is necessarily a question about a
model. "Which records belong" is not. Reading the first to answer the second does not
make the output model-dependent, and a compactor that could not see the window would be
guessing.

Cross-session reach follows `INTENT.md` §5e: a curator may reference **any** record in
the store. That is what makes a memory plugin expressible here rather than as a
separate subsystem (§11.7).

### 11.6 Failure behaviour

A curator that errors, panics, or returns `Failed` is **contained and recorded**, and
the turn proceeds on the uncurated path.

This is fail-open, which is unusual for this project and needs its justification
stated: a curator is an *optimization*, not a correctness requirement, and the
consequence of not curating is already caught downstream — the request either fits or
admission refuses it loudly and names the shortfall (§2.7). Fail-closed would mean a
broken plugin bricks a session that would otherwise have worked. Fail-open with a
silent swallow would be the thing this project actually refuses, which is why the
failure lands in the context report next to `dropped` and `RenderDivergence`.

Same posture, same reason, as `ToolObserver` (`ARCHITECTURE.md` §3.9).

### 11.7 What becomes a curator

- **`conway.compaction`** — the first one, and the reason to build this. Mechanical
  cherry-picking with structural predicates, cache cost known before it commits.
- **`conway.memory`** — under §5e this is a curator whose selections reach outside the
  current tree. It needs no storage of its own, no retrieval semantics of its own, and
  no new port: "recall what I learned in a past session" is a cross-session selection.
  Retention (§4.4) is what keeps those old sessions alive, so the two mechanisms
  compose rather than needing to know about each other.

  > **AMENDED 2026-08-18 — this prediction was built to, and it failed.**
  > Operator ruling, after `conway.memory` shipped in exactly the shape this
  > paragraph describes (label a session, select its records through the path
  > machinery) and proved unusable in practice.
  >
  > The claim was treated as a REQUIREMENT rather than a hypothesis, and the
  > evidence against it accumulated as separate "follow-ups" instead of being
  > read as one signal: marking could only happen at session creation — the one
  > moment you cannot yet know a conversation mattered; nothing could be
  > un-remembered; growth was "bounded" only by truncating a session to 8
  > arbitrary records; and a memory could never be a distillation, only a
  > verbatim excerpt. That cap was the tell, and it was written down as a virtue.
  >
  > The type system had already said so: `CurateOutcome::Derived` carries a
  > `Derivation`, which can only reference records that already exist, so
  > freeform memory text is structurally unrepresentable at the curator seam.
  >
  > **What shipped instead (`5beb741`):** a mutable `MemoryStore` port — a memory
  > is freeform text with OPTIONAL provenance, addressable and removable,
  > injected as a segment by a `ContextHook` and attributed with
  > `Provenance::Memory`. So memory DOES have storage of its own and DOES need a
  > new port. It still needs no retrieval semantics of its own, and it does not
  > touch record-log immutability — a memory is an annotation ABOUT sessions, not
  > content IN one.
  >
  > §11.3's ruling that `ContextHook` is the wrong seam still stands and is not
  > contradicted: that ruling is about CURATION, which edits selections. Memory
  > turned out to be INJECTION, which is what `Provenance::AgentDef`/`Skill` have
  > always been.
  >
  > The rest of this section — `conway.compaction`, the human through
  > `conway path` — is untouched by this amendment. Both are genuinely selection
  > over existing records, which is what the curator seam is for and why it was
  > kept.
- **A human, through `conway path`** (D1-7) — the same operations, no plugin installed.
  This is why D1-7 is gating rather than cosmetic (§10).

Three consumers, one mechanism. If a fourth wanted something none of these express,
that is a finding against the port rather than a reason to widen the core.
