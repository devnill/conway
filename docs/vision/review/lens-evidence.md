# Lens: evidence

> **Read [`CONDUCT.md`](CONDUCT.md) first, and read §5 of it twice.** This lens
> *is* `CONDUCT.md` §5 turned from a warning into an assignment.

---

## 1. The question

> **Where is this tree believing something it has not verified?**

The other lenses check the tree against a standard. This one checks the tree's
**evidence** — the places where something is asserted, relied on, or reported as
working, and the proof underneath it does not hold. Every one of the four
patterns below produced a shipped defect here.

You are the only reviewer whose findings will mostly be about things that
currently look fine.

---

## 2. The four hunts

### 2.1 A premise defended by a series of workarounds

The highest-value hunt on this page, and the one no other lens can do, because it
requires reading accommodations **together** rather than one at a time.

Method: collect the accommodations first, judge second. Grep the tree and the
board for the vocabulary of accommodation — *for now*, *follow-up*, *open
question*, *ergonomics*, *cap*, *limitation*, *until we*, *tracked separately* —
then **group them by the premise they are protecting**, not by file.

Two or more accommodations around one premise is the signal. Each is locally
reasonable; that is what makes the series invisible.

Then look for **a symptom phrased as a virtue**. "Bounded by construction" was
written in this tree to describe a cap that existed only because the unit of work
was wrong. Reassuring language applied to a workaround is the hardest instance to
see and the most valuable to name.

`INTENT.md` §8.8 is the rule: a design document says what a feature will need,
and that is a prediction, not a requirement.

### 2.2 A capability verified in one direction only

For each piece of data a design rests on, verify **both ends**. Read path and
write path. Producer and consumer. Set and get.

The canonical instance: a memory feature selected sessions by a label nothing
could ever set — filter, query and reachability all present, and no code path
wrote the field.

Method: pick the fields that decisions hang off — anything used in a filter, a
selector, a routing predicate, a capability check — and grep for writes as
deliberately as for reads. `grep -rn 'field_name' | grep -v 'test'` and then ask
which of those lines is an assignment.

### 2.3 A design document treated as a constraint

Where a `docs/vision/DESIGN-*.md` and the shipped code disagree, **investigate
which is wrong**. Do not assume the code, and do not recommend reverting shipped
work because a document predicted a different shape.

`DESIGN-context-path.md` §11.7 is the worked example and carries its own
amendment note. Check whether any *other* design doc is currently being defended
the same way.

### 2.4 A limitation reported after the success

Find things that work end to end and are not usable: a feature whose enabling
path is unwired, a store that forgets on restart, a capability whose default
configuration disables it.

For each, check how it is currently *described* — in `CHANGELOG.md`, in the
board's completion notes, in the docs. **If the caveat comes after the success,
that framing is itself the finding**, separately from the limitation. A review
that buries the caveat has misled its reader even if every sentence is true.

---

## 3. Programming by coincidence

The fifth pattern, and the bridge to `lens-sustainability.md` — which owns the
architectural half. **You own the evidential half:** code whose correctness
depends on something nobody has written down.

- a test that would still pass with the behaviour removed
- an ordering dependency that is real and undocumented
- an assertion that holds by luck of the current call graph

If you find these clustered in one module, say so and hand the module to the
sustainability lens by name rather than diagnosing its structure yourself.

---

## 4. Budget

- **Tool calls:** 30–45. §2.1 deserves the largest share; it is the hunt with no
  substitute.
- **Return:** the shape in `CONDUCT.md` §4, **under 1,200 words**.
- **Findings:** 3–6. This lens is expected to return fewer, heavier findings than
  the others. One correctly-identified failing premise is worth the whole run.
