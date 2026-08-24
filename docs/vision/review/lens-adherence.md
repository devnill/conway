# Lens: adherence

> **Read [`CONDUCT.md`](CONDUCT.md) first.** This lens assumes it.

---

## 1. The question

> **Does the tree do what the documents say it does — in both directions?**

Not "is it good." Only: is the written record true. A page that *understates*
what is built is the same defect as one that overstates it, and this tree has
been bitten by the understating kind badly enough that it is the first thing to
check.

---

## 2. What to check

Work outward from the machine-checked layer, because it is the cheapest and the
most reliable.

**2.1 The ledger.** `scripts/board-claims.md` is a set of falsifiable claims and
CI fails when one stops being true in either direction. Run the checker
(`scripts/check-design-claims.py`) rather than reading the file and believing
it. Every red entry is a finding with the diagnosis already done. Every claim in
`INTENT.md` that *should* be in the ledger and is not is also a finding.

**2.2 `PHILOSOPHY.md`.** Present tense throughout. Anything without a **"Where
the tree is today"** note is asserted true right now. Sample the assertions that
would be expensive to be wrong about — security guarantees, extension promises,
what the core does not do — and check each against code. `CONTRIBUTING.md` §2 is
the rule that is supposed to keep the notes honest; check whether it held.

**2.3 `ARCHITECTURE.md`.** Describes mechanism as it is today, so a stale
paragraph here is a straight defect. Focus on the parts a new contributor would
act on.

**2.4 `docs/*.md`.** The practice layer — `getting-started`, `permissions`,
`plugins/`, `embedding`, `scripting`, `interactive`. A capability that ships with
no findable page is a finding of the same weight as a page describing a
capability that does not ship. Check `docs/README.md`'s index against what
exists in `docs/`.

**2.5 `README.md` and `GUIDE.md`.** The two pages a stranger reads first, and
historically the two most likely to be a year behind.

---

## 3. Ranking

Rank findings by **how many readers a false sentence misleads, and how
expensively.** In order:

1. A security-bearing claim that is wrong in the reassuring direction.
2. A claim on `README.md`/`GUIDE.md`, because it is read first and read most.
3. A capability that ships and is documented as absent — everyone in between
   builds a workaround they did not need.
4. Everything else.

**Where the fix is one edit to one page, say so and size it S.** This lens
routinely produces the cheapest items on the board, and they should be dispatched
as a batch rather than spread across rounds.

---

## 4. Budget

- **Tool calls:** 25–35.
- **Return:** the shape in `CONDUCT.md` §4, **under 1,000 words**.
- Every finding quotes the offending sentence and cites the `path:line` that
  falsifies it.
