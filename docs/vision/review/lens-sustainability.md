# Lens: sustainability

> **Read [`CONDUCT.md`](CONDUCT.md) first.** This lens assumes it.
>
> **You have been assigned exactly one territory.** It is named in your dispatch
> brief. Other agents are running this same lens on the other territories in
> parallel. Do not read outside yours — you will produce a duplicate finding and
> cost the review a merge.

---

## 1. What this lens is for

Every other lens asks *does the tree do what it promised?* This one asks a
question that a passing test suite cannot answer:

> **The code works, and it is as designed. Is it built in a way that will still
> be affordable to change in six months?**

conway is over 220,000 lines across 20 crates. At that size the dominant cost is
no longer writing code — it is that a single conceptual change now requires
edits in places nobody remembers are related. That cost is invisible in a green
CI run and invisible in a feature review. It shows up only as work getting
slower, which is the symptom the operator reported and the reason this lens
exists.

**You are hunting cost-of-change, not ugliness.** A module can be untidy and
cheap to change; a module can be beautiful and cost a week to touch. Only the
second is a finding.

---

## 2. The one thing that disqualifies a finding

> **A consolidation finding must name a change that is currently hard and would
> become easy. If you cannot name that change, it is not a finding — it is a
> preference, and it does not go in the report.**

This is the guardrail. A reviewer given a DRY brief will reliably produce a
list of things that look alike and a three-week refactor to merge them, and
that refactor will be net-negative. Three rules keep it honest:

**Similar is not duplicate.** `INTENT.md` §8.10 states the unit as *knowledge,
not text*, and Hunt and Thomas are explicit that not all code duplication is
knowledge duplication. Two functions that look identical but
change *for different reasons* are correctly separate, and merging them couples
two things that had no reason to be coupled. The test is not "do these look
alike?" — it is **"when this decision changes, must both edits happen, and would
forgetting one be a bug?"** If yes, it is one piece of knowledge in two places.
If no, leave it alone.

**Some duplication is the price of a boundary conway chose to have.**
`INTENT.md` §8.2 makes the core agnostic: an opinion in the core is a thing every
extension has to route around forever. If removing a repetition would move a
judgment into `conway-core`, the repetition is correct and the finding is
wrong. Say so explicitly rather than staying silent — a "we deliberately repeat
this, here is why" note is a useful finding in its own right.

**An abstraction with one caller is not shared knowledge.** It is indirection
with a cost and no payer. `INTENT.md` §8.5 already forbids building on theory;
this is the same rule pointed at your own recommendations. Do not propose a
generalisation whose second consumer is hypothetical.

---

## 3. What to hunt

Six checks. Do them in this order — the earlier ones are cheaper and their
answers narrow the later ones. **You will not finish all six.** Stop at the
budget and report what you did not reach.

### 3.1 Knowledge with more than one authoritative representation

The DRY check, in its real form: *every piece of knowledge should have a single,
unambiguous, authoritative representation within a system.* Knowledge, not text.

Look for a **decision** written down in more than one place:

- a default value, a timeout, a limit, a precedence order
- a name-to-meaning mapping (event names, capability names, config keys)
- a validation rule or a parse of the same format
- an error taxonomy — the tree has ~20 distinct `*Error` enums; how many encode
  the same failure classes with different names?
- a piece of policy that a plugin and the core both decide

For each candidate, answer the §2 test, then measure the extent with **one**
command rather than by reading every site.

> **Known live instance, use it as calibration, do not spend budget re-finding
> it:** `kill_group` is implemented in `conway-plugin-mcp/src/session.rs`,
> `conway-plugin-subprocess/src/session.rs` and `conway-tools/src/process.rs`
> because `conway-tools::process` is private. That is already on the board. What
> makes it the right *shape* of finding is that it names the mechanism — a
> module that is private when it should be a surface — rather than the symptom.

### 3.2 The test harness

**Include test code in this lens.** Every other part of this review subtracts
test lines; this one does not, because test-harness duplication is the purest
form of the defect — the same fake, the same builder, the same fixture, written
again in every file that needs it, changing in lockstep forever.

`conway-testkit` exists and **every crate in the workspace already depends on
it.** Establish what that crate actually offers, then measure what the tree
built anyway. Starting points, already measured — confirm and explain rather
than re-count:

| Helper | Files defining their own |
| --- | --- |
| `fake_router` | 36 |
| `text_response` | 52 |
| `build_conway` | 46 |

The finding is not "there is duplication." The finding is **why the testkit was
not reached for** — is it missing the shape people need, is it hard to
discover, is it under-documented, or did it arrive after the tests did? That
answer determines whether the fix is consolidation or a testkit redesign, and
they are different board items.

Test line counts are also a sustainability signal in their own right: several
crates carry more test lines than production lines, and `conway` carries roughly
twice. That is not automatically wrong — but where the ratio is extreme, ask
whether the tests are testing the same knowledge repeatedly.

### 3.3 Orthogonality, measured from history

Orthogonality is the property that a change to one component does not require
changes to another. It is measurable, and guessing at it from module names is
worthless.

Take **three to five real commits** from `git log` that each changed one
concept. For each:

```
git show --stat <sha>
```

Then answer:

- How many files did it touch?
- Are those files conceptually related, or did one idea have to be re-expressed
  in the TUI, the CLI, the runtime and the core separately?
- Was there a file it touched that a reader would not have predicted?

**A concept that must be re-expressed at every layer it passes through is the
cross-cutting the operator is worried about.** Name the concept, name the layers,
and say what a single representation would look like.

### 3.4 Easier to change (ETC)

Good design is easier to change than bad design. Applied to the largest modules
in your territory:

For the three biggest, ask: **what is the change that would be hardest to make
here, and why is it hard?** Then say whether "hard" is essential (the problem is
genuinely entangled) or accidental (the code entangled it).

Size is a prompt to look, not a finding by itself. `conway-cli/src/tui/commands.rs`
is ~4,900 lines and `conway-core/src/ports/plugin.rs` ~3,100; a large file that
is a flat list of independent cases is fine, and a 400-line file where every
function reaches into every other is not.

### 3.5 Programming by coincidence

Code that works, and nobody can say why. In a tree this size it looks like:

- an ordering dependency that is real and undocumented — B works because A ran
  first, and nothing says so
- a contract enforced by convention at call sites rather than by the seam.
  `INTENT.md` §8.6 already rules on this: **an invariant belongs to the seam, not
  to its call sites.** Scattered enforcement is many independent judgments that
  can drift apart; one check at the seam is a single mechanical fact. Find the
  invariants currently enforced by everyone remembering.
- a test that passes for a reason other than the one it names — especially one
  that would still pass with the behaviour removed
- a comment that explains *what* where the *why* is the thing nobody knows

### 3.6 Consolidation and reversibility

Now propose. For each candidate, the finding must carry:

- **the merge**: what becomes one thing
- **the change it makes cheap**: from §2, non-negotiable
- **the cost**: files touched, and whether it is a one-way door

Prefer consolidations that are **reversible** — decisions you can back out of if
they turn out wrong. Maintain flexibility over anticipating an uncertain future;
a consolidation that can be undone in an afternoon may be attempted on a
hunch, and one that cannot must be justified on evidence.

Rank everything you found by:

> **(how often this knowledge changes) × (how many places it lives)**

The top of that ranking is what goes in the report. Something written in nine
places that has never changed is not urgent; something written in two places
that changes every fortnight is.

---

## 4. The named principles, and how they sit with `INTENT.md`

The operator asks for *The Pragmatic Programmer*'s tests by name. They are
admitted here **as tests**, not as authority — the authority is `INTENT.md`
§8.10, which now says most of what the first four rows say, in the operator's
own words. The value of the table is that it shows which of the book's tests
conway has adopted and which it has deliberately not.

> **Where a principle below conflicts with `INTENT.md`, `INTENT.md` wins and the
> conflict is a finding about `INTENT.md`.** The review's standing rule is to
> score against the operator's intent rather than against general
> software-quality intuitions; naming these principles does not repeal that
> rule, it just stops the lens from having to reinvent the vocabulary.

| Principle | The test it gives you | Where conway already says it |
| --- | --- | --- |
| **ETC** — good design is easier to change than bad design | §3.4 | **§8.10**, whole point |
| **DRY** — one authoritative representation per piece of knowledge | §3.1, §3.2 | **§8.10**'s test; §8.6 for invariants |
| **Orthogonality** — a change to one component does not require changes to another | §3.3 | **§8.10**; §8.2 from the agnostic-core end |
| **Reversibility** — there are no final decisions; keep them backable-out | §3.6 | **§8.10**'s back-out-in-an-afternoon rule; §2 on uninstallability |
| **Tracer bullets** — one thin real path through every layer, early | the proof `INTENT.md` §8.5 demands | §8.5, near-verbatim |
| **Programming by coincidence** — understand *why* it works | §3.5 | §8.6 |
| **Broken windows** — small rot signals and accelerates abandonment | the restraint note below | none |
| **Refactor early, refactor often** — small low-risk steps, not campaigns | §3.6's reversibility preference | **§8.10**, second consequence |
| **Tests are the first user of your code** | §3.2 | none |

Two of these deserve their own note.

**Tracer bullets are already conway's idiom under another name.** `INTENT.md`
§8.5 requires a real path through the shipped binary on the day a capability
lands — that is a tracer bullet, and the failure it prevents ("a capability
added silently, to be used later") is exactly what the book describes. If you
find machinery with no path through it, cite §8.5; you do not need the book.

**Broken windows is the one to apply with restraint.** Do not return a lint
report. `TODO`/`FIXME`/`#[allow]` counts are a *signal to look somewhere*, never
a finding. A broken-window finding needs a cluster in one place plus an argument
that the cluster is why that area is now avoided.

---

## 5. The section you score against

`INTENT.md` **§8.10** is this lens's authority, and it was written *for* this
lens — read it before §3, not after.

> **The test: when this decision changes, how many places must change with it —
> and would forgetting one of them be a bug?**

That is the same test §2 gives you, and it is not a coincidence: §8.10 carries
all three of §2's guardrails as operator sentiment rather than reviewer
convention. Repetition that exists to keep a judgment out of the core is
correct. An abstraction whose second consumer is hypothetical is indirection,
not shared knowledge. A consolidation proposal must name a change that is
currently hard and would become easy.

So findings from §3.1, §3.2, §3.3, §3.4 and §3.6 **cite `INTENT.md` §8.10** in
their `Against:` field. They are no longer intent gaps.

**What still belongs under Intent gaps.** §8.10 settles cost-of-change; it does
not settle everything this lens can turn up. Route these to the operator instead
of deciding them yourself:

- A consolidation that is right by §8.10 and would put a judgment in the core,
  where §8.2 forbids it. §8.10 says which way that tie breaks in the ordinary
  case; a case where it plainly gets the wrong answer is a gap.
- §8 is ordered most-important-first, and §8.10 is last. Where doing it would
  cost something §8.7 (genuinely usable) or §8.9 (safe) protects, the ordering
  has already answered — but say so, because a reviewer silently dropping a
  finding on those grounds looks the same as one that missed it.
- Anything where the honest answer is "this is expensive to change and I cannot
  say what change it makes cheap." That is not a §8.10 finding. It may still be
  worth the operator's attention as a question.

## 6. Budget

- **Tool calls:** 30–45. Counting commands are cheap; whole-file reads are not.
- **Return:** the shape in `CONDUCT.md` §4, **under 1,200 words**.
- **Findings:** aim for 3–6. More than eight means you are inventorying.
- Every finding carries `path:line` evidence, a measured extent, and the change
  it makes cheap. A finding missing the last one does not ship.
