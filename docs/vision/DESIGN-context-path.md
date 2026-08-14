# Design: the first-class context path

**Board item `01M00QDYK4T5MZNCTQ0ZXEBSZX` (D1-1). Produced 2026-08-14.**

> **⚠ Amended 2026-08-14 by `INTENT.md` §5c — read that first.** Reviewing this
> design's five open questions surfaced a specification gap rather than five
> answers: a path is identified **twice**, by its *selection* (which records, in
> what order — model-free, durable, the thing worth naming) and by its *rendering*
> (the wire bytes — model- and config-specific, disposable). `prefix_key` is the
> **rendering** identity.
>
> **This design conflates them, and §2.4's head is keyed on the wrong one.** A head
> holding a `PrefixKey` cannot survive a mid-session model change without being
> rewritten, and two agents with identical curation but different models would
> appear to have different paths. Since changing model mid-session is ordinary
> rather than exceptional, that is backwards.
>
> **What changes:** the design needs a second, model-free selection identity
> (content-addressed over the node list); a head references *that*; `prefix_key`
> stays exactly as it is and keeps its cache job. Open questions 1, 2 and 5 are
> answered by `INTENT.md` §5c, §8.2 and §5d respectively and are struck below.
> Everything else in this design stands — §4.1's two-constructor coherence
> boundary, §4.2's cost model, §4.4's retention rule and §5's line-holding argument
> are unaffected by the split.

Settled inputs are `docs/vision/INTENT.md` §5a and §5b; this page does not re-derive
them.

---

## 1. The approach, in one paragraph

A **path** is an ordered list of *references* to immutable records, plus declarative
references to the non-record (config-derived) static segments, plus an optional
reference to another path **version**. Assembly stops being a fixed rule over
`(inherited, head, own)` and becomes a fold over that node list; the fixed rule
survives as **one** constructor, `default_path`, which selects everything and
excludes nothing. A **version** is a frozen path, identified by the `PrefixKey` the
tree already computes at `builder.rs:389` — no new identity, and for the default path
literally the same number. A **head** is the latest `LogRecord::ContextPathSet` in a
session's own append-only log, which makes "owned by exactly one session" a
consequence of the store's existing one-writer-per-log discipline rather than a new
lock. Referencing another session's *head* is not refused at runtime — it is
**unrepresentable**, because a path body can only hold a `PrefixKey` and there is no
way to spell a head.

The load-bearing coincidence that makes this cheap: `prefix_key`'s boundary is "the
last non-`Volatile` segment" (`prefix.rs:14–18`), which is exactly *statics +
inherited prefix*. So the default path's version renders to exactly the bytes
`prefix_key` already hashes. **A version key is not a new computation; it is the
number already in every `CacheHint`.**

---

## 2. The three layers as concrete types

### 2.1 Record = blob

```
RecordRef { session: SessionId, seq: LogSeq }
```

`LogRecord` carries no session id (`log.rs:159–279`), so `(SessionId, LogSeq)` is the
only addressing that exists and the only one needed. Records are never copied, never
rewritten.

### 2.2 Path node

```
PathNode { source: NodeSource, stamp: NodeStamp, prov: NodeProvenance }

NodeSource::Record(RecordRef)
NodeSource::Assembled(AssembledRef)   // SystemPrompt{agent_def} | Skill{name} | ToolRegistry{hash}

NodeStamp::Static | NodeStamp::Inherited { from: SessionId } | NodeStamp::Head | NodeStamp::Own

NodeProvenance { selected_by: Selector, at: DateTime<Utc> }
Selector::DefaultRule | Selector::Plugin { id: String, op: OpLabel } | Selector::Operator
```

`NodeStamp` makes the existing slot vocabulary explicit, and it decides which of the
two existing mapping functions runs: `Inherited` → `record_role_and_content` +
`Provenance::Inherited`; `Head` → today's `HeadSegment` mapping (which forces
`UserPrompt`/`ForkDirective` regardless of stored provenance, `agent_loop.rs:1723`);
`Own` → `own_segment`. Keeping the stamp on the *node* rather than deriving it in the
builder is what makes byte-identity mechanical.

`AssembledRef` names static segments **declaratively**, not by copying their text.
Their bytes still come from live config each turn, which gives the version an
identity *check* rather than a snapshot (§4.3, and open question 3).

### 2.3 Version = commit

```
PathVersion {
    prefix: Option<PrefixKey>,        // another VERSION, never a head
    nodes:  Vec<PathNode>,
    incoherence: Vec<HarnessDrop>,    // §4.1
}
```

Identity is `prefix_key(model, render(expand(version)))` — the existing function,
unchanged. Immutable, globally readable, freely referenced.

Stated plainly: **the key folds in the model and the static prefix.** That is correct
rather than a leak — a version identifies a *wire prefix*, and two agents with
different system prompts genuinely do not share one. Its body records the selection;
its key identifies the bytes.

`prefix` transitivity is expanded with a depth bound and a
`PathError::PrefixChainTooDeep`, the same shape as `resolver.rs`'s
`MAX_ANCESTRY_DEPTH`. Cycles are impossible by construction (a key depends on its own
expansion) but a hand-edited store is not, so the bound stays.

### 2.4 Head = branch ref

A new `LogRecord` variant, in the owning session's own log:

```
ContextPathSet { seq, ts, version: PrefixKey, covers_upto: LogSeq }
```

`covers_upto` is the watermark in **local units**, the same units `resolver.rs` uses.
Assembly is `expand(version) ++ own records from covers_upto`. That is how a live,
appending session can point at an immutable version: the mutable tail is deliberately
outside it.

**Absence of the record means the default path** — which is the entire migration
story (§6).

Ownership is structural: another session cannot append to a log it does not own, so
it cannot move this head. Nothing enforces ownership at runtime because nothing can
violate it.

### 2.5 `ValidatedPath` — the type that gates assembly

```
ValidatedPath                         // private fields, two constructors only
  ::default_path(..)                  // includes everything; declares, never refuses
  ::derive(base, ops)          -> Result<Derivation, PathError>   // refuses
  ::derive_reordered(base, ops) -> Result<Derivation, PathError>
```

`ContextBuilder::build` accepts only a `ValidatedPath`. There is no third way in.

### 2.6 Where each type lives

| Thing | File |
| --- | --- |
| `RecordRef`, `PathNode`, `PathVersion`, `ValidatedPath`, `PathOp`, `Derivation`, `CostEstimate`, `PathError`, coherence validator | **new** `crates/conway-core/src/path.rs` |
| `ContextPathSet` variant | `crates/conway-core/src/log.rs` |
| `ContextReportEntry::origin`, `ContextReport::path` | `crates/conway-core/src/provenance.rs` |
| `PathStore` port | **new** `crates/conway-core/src/ports/path_store.rs` |
| `FsPathStore` + reverse index | **new** `crates/conway-session/src/path_store.rs` |
| `resolve_path` | `crates/conway-session/src/resolver.rs` |
| `default_path`, head resolution | **new** `crates/conway-runtime/src/context/path.rs` |
| assembly over a path | `crates/conway-runtime/src/context/builder.rs` |

All inside D1's declared ownership in `PLAN.md`. The CLI verbs in §4.5 are a
**request to D4**, not a change specified into their files.

---

## 3. How assembly changes

`ContextInput` loses `inherited`, `head`, `own` and gains `path: ResolvedPath` (nodes
zipped with their already-read records). It keeps `system_prompt`, `skills`, `tools` —
those are live config, and the version references them by name so drift is *detected*,
not snapshotted.

The builder stays **pure, non-async, store-free.** Resolution is the caller's job,
exactly as `TranscriptResolver` is today. `resolve_path` reuses the existing memoised
ancestry walk for the default path so the `Arc::ptr_eq` sibling-sharing property that
`crates/conway/tests/fanout_prefix_sharing.rs` depends on survives untouched.
Everything after node→segment mapping is unchanged.

**Byte identity is by construction and provable against artefacts that already
exist.** `default_path` emits, in order: `Static` for `[0]`/`[1]`/`[2]`; `Inherited`
for every record in the resolved prefix; `Head` for the session's own first record;
`Own` for the rest. Since the per-stamp mapping is the code that runs today, the
segment list is identical — so segment ids, `prefix_key`, cache hints and the report
are identical. The proof is `crates/conway-runtime/tests/context_golden.rs` plus
`tests/golden/*` **unchanged and not regenerated**. A regenerated golden file is the
failure signal, not the fix.

One deliberate legacy carry-over, which must be commented where it is encoded or
someone will "fix" it: `InheritedPrefix.from` stamps every inherited record with the
**immediate parent**, not its true author, at fork depth ≥ 2 (`builder.rs:126–141`).
`default_path` reproduces that exactly. A **derived** path stamps the true owning
session, because it has no bytes to preserve. Unifying the two is a one-time cache
invalidation plus golden regeneration and must be its own item — the cost is a bill
rather than a bug.

---

## 4. The five required answers

### 4.1 Coherence — where the validation boundary sits

**Two constructors, two situations, and the boundary is the constructor.**

- `derive` / `derive_reordered` **refuse.** They run the coherence validator over the
  resolved node list and return `Err(PathError::WouldOrphan{..})`. An invalid derived
  path is never constructed.
- `default_path` **cannot** refuse — whatever incoherence is present was caused by the
  harness (a fork cut mid-batch; a session killed between an assistant append and its
  results, `builder.rs:26–61`). It **tolerates and declares**, recording every
  unanswered `call_id` into `PathVersion::incoherence`.

That declaration is what makes the two impossible to confuse. At render time
`drop_unanswered_tool_calls` stays, but is **reconciled against the declaration**: it
may drop exactly the ids the path declared, and any *other* orphan is a defect in
derivation returning a typed `RuntimeError::IncoherentContext`. The repair set is
announced up front and checked, rather than being an open licence to fix anything.

The validator has **three** rules, not one:

1. every `ToolUse` call_id has an answering `ToolResultBlock` on the path;
2. every `ToolResultBlock` has its `ToolUse` on the path;
3. each result appears **after** its call.

Rules 2 and 3 are new and they matter: `builder.rs:521–526` states the
result-without-call direction "could not be made to fail". True for prefix cuts,
**false for arbitrary selection and for reordering.** That comment must be amended in
the same change that adds the validator.

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
candidate repairs as ops, so a plugin can retry programmatically. **The harness
offers; it never picks.** Choosing silently is guessing at intent, which is the whole
reason refusal was chosen.

### 4.2 Rearranging costs strictly more than omitting

Four mechanisms, each structural rather than advisory:

1. **There is no "set the node list" operation.** `PathOp` is `Omit` / `Include` /
   `Move{node, before}`. You cannot reorder by accident because you cannot express a
   whole-list assignment.
2. **Reordering is a different function.** `derive` refuses any `Move` with
   `PathError::ReorderRequiresExplicitDerivation`; `derive_reordered` accepts it. The
   cheap operation gets the short name, and a plugin cannot fall into the expensive
   one by leaving a flag at its default.
3. **The price returns with the result, before anything is sent:**
   ```
   CostEstimate {
       shared_prefix_nodes, shared_prefix_tokens_est, discarded_prefix_tokens_est,
       first_divergence: Option<RecordRef>, version_key_preserved: bool,
   }
   ```
   For omission-only, `first_divergence` is the first omitted node and everything
   before it is byte-identical by construction. For a reorder it is the first moved
   element and `discarded_prefix_tokens_est` is strictly positive.
4. **The economics fall out of the boundary for free.** Omissions confined to the
   *tail* leave the version key untouched — they cost nothing in cache terms.
   Omitting inside the frozen version changes the key, which is exactly "dropping from
   the head spends the whole cached prefix", now mechanical rather than rhetorical.

### 4.3 Provenance survives a graph drawn from several sessions

- **Every node carries its true `RecordRef`**, for every stamp. Origin is a property
  of the *path*, not reconstructed from the rendered segment — which is what fails
  today at depth ≥ 2.
- **Every node carries why it is there** (`selected_by`, `at`). This is the tree's own
  precedent — the intervention goes *in* the record, never behind it — applied to
  curation. A curated context explains itself without re-running the curator.
- **`ContextReport` carries it to every existing consumer:** add
  `ContextReportEntry::origin` and `ContextReport::path`, both `#[serde(default)]`,
  the exact pattern `dropped` used. `/context` and the durable record pick it up with
  no new plumbing.
- **The head's history is the log.** Every `ContextPathSet` append is an ordinary
  record with seq and ts, so "how did this context come to be" is answerable from the
  log alone.
- **No new `Provenance` variant.** That is a breaking wire change, and it would put
  curation vocabulary into the core's wire format. Selection provenance lives on the
  node.

Note precisely what each answers, so nobody collapses them:
`Provenance::Inherited.from` = *who handed me this*; `ContextReportEntry.origin` =
*who wrote it*. Both are true; only the second survives multi-session composition.

### 4.4 Retention — the stated rule

> **A version pins every record it references. A session with any record pinned by a
> reachable version cannot be removed.**

Mechanism reuses what exists: `SessionStore::remove`'s guard matrix
(`ports/session.rs:63–94`) already refuses on dangling-provenance grounds. Add one
clause returning `StoreError::NotRemovable` naming the pinning version. Enforcement
uses a `session → [version keys]` reverse index in the path store, rebuildable by
scanning version bodies — a derived accelerator, never a source of truth, exactly
`SessionIndex`'s stated discipline.

Consequences, all intended:

- **`/ask` discard and `Conway::pull_in`'s purge fail loudly** when the child's
  records are pinned. Correct: putting a record on someone's path is a statement that
  it matters. The TUI's modal-ask residue sweep must treat a pin refusal as *skip and
  report*, never swallow it.
- **Pinning is not retroactive.** A version pins at creation.
- **Unpinning is garbage collection, never mutation.** A version is immutable and
  never edited; it becomes unreachable when no head and no other version references
  it, and unreachable versions are collected only by an explicit `conway path gc`.
  Never automatic, never on a timer — a background process deleting the thing keeping
  a session alive is precisely the surprise this harness refuses.
- **The rejected alternative is kept as the escape hatch.** `Conway::pull_in` copies
  content into the parent and purges the child. That stays unchanged as the explicit
  "I want the content and want the log gone" operation — the one place copying is
  sanctioned, because the operator asked for it by name.

### 4.5 A person must be able to inspect a rearranged context

Three surfaces, one data source, no new truth:

1. **`/context` gains origin** — each line becomes `session/seq · provenance ·
   selected-by · tokens`. Falls out of §4.3 with a render change.
2. **`conway path` verbs** — a **request to D4**: `path show <agent>`,
   `path diff <a> <b>` (kept / omitted / **moved, with distance**, plus cost delta and
   first divergence), `path log <session>`, `path show --node <ref>`.
3. **The persisted per-turn report** already makes this answerable after the fact,
   including for a finished agent.

The bar: a curator's decision must be visible without re-running the curator. That is
why `selected_by` is persisted rather than recomputed, and why `diff` must render a
move as a move rather than a delete plus an insert.

---

## 5. The line that must not move — explicit statement

**This design puts one selection rule in the core, and names it: `default_path`.**

The line is *which records belong on a path is policy*. `default_path` has no opinion
about belonging — it includes everything, in log order, drops nothing, reorders
nothing. It is the identity function over today's behaviour, and it exists because the
core must be able to assemble a path at all. The failure mode is the core acquiring an
opinion: a rule that decides some records *don't* belong.

The bright line, stated so it can be enforced:

> **`conway-core`, `conway-session` and `conway-runtime` ship exactly one path
> constructor, and it excludes nothing. They construct `PathOp::Omit` / `PathOp::Move`
> nowhere outside tests.**

Enforce it with the technique this tree already uses —
`crates/conway/tests/enum_variant_construction_guard.rs` — extended with an allowlist
entry for `PathOp`. Vigilance is not a mechanism; a failing test is.

One standing exception, which must be named or it will be quietly widened:
`drop_unanswered_tool_calls` **is** core code that removes records. It is justified
solely by a provider hard requirement, it is recorded in `ContextReport::dropped`, and
under this design it is additionally reconciled against a declaration. It must never
be widened into a heuristic — the moment it drops something for a reason other than
"the request is otherwise unsendable", the line has moved.

---

## 6. Migration and compatibility

- **Wire bytes:** unchanged for the default path. Proof = existing goldens and
  `fanout_prefix_sharing.rs` passing *without regeneration*.
- **Log format:** one new variant. `LogRecord` is `#[non_exhaustive]` and both mapping
  functions already end in `_ => None`, so `ContextPathSet` never becomes a segment
  and older readers ignore it.
- **Existing sessions:** no migration at all. No `ContextPathSet` record ⟹ default
  path ⟹ today's behaviour.
- **`ContextReport`:** two `#[serde(default)]` fields.
- **`Provenance`:** no new variants.
- **`ContextMask`:** becomes sugar for a persisted `Omit` op on the owning session's
  head, and `apply_context_mask` retires **in the same change** that lands the
  replacement. Flag the semantic change explicitly: today a mask affects only
  fork-prefix resolution and *not* the owning session's own assembly; under the path
  model an omission on a session's own head affects its own context too. That is the
  mechanism finally meaning what its doc always said, and it must be a stated decision
  rather than a side effect.
- **`ContextHook`:** unchanged signature; its output becomes coherence-checked (§8.1).

---

## 7. Decomposition

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D1-3a | **Path vocabulary in `conway-core`** — types, three-rule validator, `PathError` with `offers`, the `PathOp` construction guard. No wiring. | L | D1-1 |
| D1-3b | **`PathStore` port + `FsPathStore`** — content-addressed write-once version objects, rebuildable reverse index. | M | D1-3a |
| D1-3c | **Head record + `default_path`** — `ContextPathSet`, head resolution, `resolve_path` preserving `Arc` sibling sharing. | M | D1-3a |
| D1-3d | **Assembly over a path** — byte-identity proof against unregenerated goldens. | L | D1-3c |
| D1-3e | **Refusal/repair reconciliation** — scope the render-time repair to declared drops; hard error otherwise; coherence-check `ContextHook` output. Behaviour change, own item. | M | D1-3d |
| D1-2 | **Cost legibility** — produce and expose `CostEstimate`; shape fixed above. | M | D1-3d |
| D1-4 | **Structural selection predicates** — mechanism only. | M | D1-3d |
| D1-5 | **Fold in `ContextMask`** (filed: `01KZY8QRAVVVKCRBZ6HAEGW3GG`) with the semantic change stated. | M | D1-3d |
| D1-6 | **Inspection** — report origin fields + `/context` render. | S | D1-3d |
| D1-7 | **`conway path` verbs** — request to D4. | M | D1-3d, D1-6 |
| D1-8 | **Curation plugin capability** — transcript read + `derive` + set-head for plugins. **Without this, no curation plugin can exist and all of D1 is inert.** | L | D1-3d |
| D1-9 | **Retention enforcement** — `remove` guard, sweep behaviour under pins, explicit `path gc`. | M | D1-3b, D1-3c |

D1-8 is the one most likely to be dropped and the one that decides whether any of this
ships value.

---

## 8. Findings in the tree that contradict or complicate the settled inputs

1. **Deliberate incoherence is already possible and is neither refused nor repaired.**
   **Verified by the invoking session.** `ContextHook::before_request` runs *after*
   assembly, `agent_loop.rs` takes `transformed.segments` wholesale, and `retotal`
   only re-estimates tokens — `drop_unanswered_tool_calls` has already run. A hook
   dropping a tool-result segment produces a request every provider rejects, today,
   silently. "An invalid path is unrepresentable" is false at this seam until D1-3e
   lands. **Filed separately as a live defect.**
2. **`builder.rs:521–526` is false under selection.** "A branch for result-without-call
   could not be made to fail" holds for prefix cuts only.
3. **Provenance already does not survive multi-session composition.**
   `builder.rs:126–141` stamps the immediate parent at fork depth ≥ 2 whatever the true
   origin. Hazard 3 is a present defect, not a future risk. Fixed in the path and the
   report, deliberately **not** in the wire bytes.
4. **`prefix_key` is graph-version identity only with two caveats:** it folds in the
   `ModelId` and the whole static prefix, and its boundary is "last non-`Volatile`
   segment". So a version identifies a *wire prefix*, not a bare selection; and any
   record a curator wants inside a frozen version must be stamped non-`Volatile`.
5. **`remove`'s dangling-reference guard sees children only.** Cross-session path
   references are a new dangling class it cannot see.
6. **`Conway::pull_in` copies content and purges the child** — a copy-based merge
   predating "nodes are referenced, never copied". Retained deliberately as the
   discard-the-log variant.
7. **Plugins have no transcript-read capability. Verified by the invoking session:**
   `CommandCtx` carries exactly `focused_agent`, `root_agent`, `session_id`, `args` —
   no handle, no transcript. The curation plugin this whole domain exists to enable
   cannot be written until D1-8.
8. Minor: assistant turns map to `Provenance::SystemNote{reason:"assistant_turn"}`.
   A node's true `RecordRef` makes this a non-problem for inspection. **Do not "fix"
   it during D1** — it changes bytes.

---

## 9. Key decisions

1. **A head is the latest `ContextPathSet` in the owning session's own log.**
   Single-session ownership follows from one-writer-per-log; no lock, no new file
   format, free history.
2. **A path body may reference a version and cannot reference a head.** Structural.
3. **Version identity is `prefix_key` over the version's rendering — no second
   identity.** Cost accepted: identity is model- and static-scoped.
4. **Statics are referenced declaratively and checked, not snapshotted** — assembly
   recomputes and refuses on `PathError::VersionDrift`.
5. **Two constructors: `derive` refuses, `default_path` declares.** Render-time repair
   may drop only what was declared.
6. **Reordering is a separate function with a returned price.**
7. **A version pins its records; `remove` refuses; unpinning is explicit GC only.**
8. **Provenance lives on the path node; no new `Provenance` variant.**
9. **The core ships exactly one path constructor and it excludes nothing**, guarded by
   a construction test.

---

## 10. Open questions — four resolved by specification, one cost decision left

Reviewing these produced a **specification gap rather than five answers**, which is
`INTENT.md` §8.1 working as intended. Four now follow from written principle and need
no per-case ruling; they are recorded here with the principle that settles each, so
the next reader derives them instead of asking again.

1. ~~**Model-scoped version identity.**~~ **Resolved by `INTENT.md` §5c.** A path is
   identified twice — by its *selection* (model-free, durable) and by its *rendering*
   (`prefix_key`; model- and config-specific, disposable). Ten siblings share one
   selection and get N renderings. **This design must change:** §2.4's head references
   a `PrefixKey`, which is the rendering identity, and a head keyed on it cannot
   survive an ordinary mid-session model change. The head must reference a
   content-addressed *selection* identity instead; `prefix_key` keeps its cache job
   unchanged.
2. ~~**Do versions need human names?**~~ **Resolved by `INTENT.md` §8.2's core-surface
   test** — *does this encode a judgment two reasonable people could answer
   differently?* Binding a name to a selection encodes no judgment, so the core may
   hold the binding. *Which* names exist and what they mean is policy and stays out.
   The design's instinct to defer naming entirely was over-cautious: it confused a
   pointer with an opinion.
3. **Static drift: refuse or re-key? — rule settled, cost not.** `INTENT.md` §8.3 now
   reads as a rule rather than a list: *when conway cannot honour a request or a
   reference exactly, it refuses and names what changed.* So drift refuses. **What
   that costs is still the operator's call:** under §2.2's declarative static
   references, editing an agent definition or changing a tool set invalidates every
   selection that referenced it, and every one of them refuses loudly until
   re-derived. If that is too sharp, the fix is not to soften the refusal — it is to
   reconsider whether statics belong in the selection at all, since under §5c they are
   arguably part of the *rendering*. **Answer this before D1-3a.**
4. ~~**Persist the harness-drop declaration on the version body?**~~ **Resolved by
   `INTENT.md` §5b's recording rule** — an intervention is recorded *wherever the
   thing it affected is read from*. The version body is what assembly reads, so the
   declaration goes there. Yes.
5. ~~**Ordering rule 3 makes some reorderings illegal.**~~ **Resolved by `INTENT.md`
   §5d.** Constraints a provider requires are legitimate and must be stated;
   constraints conway would impose from an opinion are refused. A result following its
   call is the first kind. "Freely rearranged" was an overclaim in §5b and now reads
   "rearranged freely, subject to what the wire permits" — so rule 3 needs no
   confirmation, only documentation where a curator will hit it.
