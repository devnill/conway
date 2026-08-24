# State of the Union: conway

**Reviewed 2026-08-24 against the working tree at `7654041`, version 0.9.0.**

> Written for the operator. It assumes you care about the shape of the system
> and not about the shape of any particular trait. Everything in it was checked
> against the code in this run; where you might want to check something
> yourself, the file and line is given.
>
> Snapshot document — replaced wholesale on the next run of
> [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md).
>
> **Note on this run.** First run of the restructured review: six reviewers in
> parallel (adherence, surfaces, operator, evidence, sustainability ×2), a
> shared measurement, non-overlapping territories. One reviewer ran the shipped
> binary against a live model backend — the first review that has. The
> coverage this bought and the coverage it cost are both stated in §6.

---

## 0. The verdict, in three sentences

**conway now works as a tool, not only as a codebase** — this review ran the
shipped binary against a live backend and one-shot answers, piped stdin, JSON
output, permission modes, and session resume all worked first try, exactly as
documented. **All five of the project's own fast gates are green at `7654041`**
(verified in this run: fmt, design-claims, board-citations, doc build, clippy) —
the red-gate era the last snapshot documented is over, and the board is drained
to two open items. **The recurring defect is now small but stubborn: the tree
keeps shipping capability faster than its own record admits** — the same commit
that gave the context-path machinery its first production caller left three doc
comments asserting that caller does not exist, and a built, tested plugin
(`conway.trim`) appears in no document and is unreachable from the shipped
binary.

The first is the news: the daily-driver ladder (§7b of
[`INTENT.md`](INTENT.md)) finally has a foot on rung one. The second means the
honesty machinery built after the last review is working. The third is the same
disease that made the last two snapshots, in a milder strain — details in §3.

---

## 1. What is built, as blocks

conway is one library consumed three ways. Everything below the facade line is
fixed; everything to the right is optional and swappable.

```
   terminal (TUI)  ─┐
   one-shot `-p`   ─┼──▶  ┌──────────┐
   your own app    ─┘     │  conway  │  the facade — one API, three consumers
                          └────┬─────┘
                               │
        ┌──────────────┬───────┴───────┬──────────────┐
        ▼              ▼               ▼              ▼
   agent loop &    tool registry   session log    domain types
   context build   & built-ins     (append-only)  & the ports
   (conway-        (conway-tools)  (conway-       (conway-core)
    runtime)                        session)
        │              │               │              │
        └──────────────┴───────────────┴──────────────┘
                               │
                    ┌──────────┴───────────┐
                    │   THE PLUGIN TIER    │   twelve crates, all optional
                    └──────────────────────┘

  in-process, compiled in:    routing · backends* · history · stepguard ·
                              skills · memory · skeleton · path · discover ·
                              trim (built, currently reachable by embedders only)
  OUT of process, no rebuild: subprocess-host · mcp-client
                                           (* the only one on by default)
```

A "port" is a socket in the core that something else plugs into: where a model
gets called, where permission is decided, where the log is written, where
context is curated. There are sixteen port modules
(`crates/conway-core/src/ports/`), and this run tabulated every one: **each has
a real implementation living outside the core, and most can be swapped by an
embedder through a builder method.** The two that cannot are named in §2 and
§3 — one is a genuine hole, one is a decision nobody has written down.

**The plugin tier is honestly uninstallable.** Every first-party plugin
resolves strictly against the `[plugins].install` list with no fallback when an
id is absent (`crates/conway-cli/src/first_party_plugins.rs:64-78`). Turning a
thing off actually removes it. This was the obvious way for the architecture to
be decorative rather than real, and it is ruled out by the code.

**What changed since the last snapshot.** The context-path machinery — the
5,500-line block the last review found consumerless — now has a real consumer:
`conway.path` ships a `compose_context_path` tool a model can call to assemble
a curated context, exercised end to end
(`crates/conway-plugin-path/tests/compose_context_path_end_to_end.rs`). The
decision the last review put to you was made, and the answer was built.

---

## 2. Scored against [`INTENT.md`](INTENT.md)

### §2 Weight: a small core with capability from outside. **Strong, and holding.**

The core stays agnostic and the swap test passes almost everywhere. The
sustainability reviewers went hunting for the usual decay — duplicated policy,
knowledge with two homes — inside core+runtime and found the one place they
expected it (permission text vs permission enforcement) correctly separated by
concern, and a previously-triplicated canonicalizer still consolidated to one
definition (`crates/conway-core/src/canon.rs:21`).

### §5 Context as a tree. **Built, consumed, and one design page hasn't heard.**

The path machinery has its first production caller (above). But INTENT §5e's
answer — *a selection may name any record anywhere; the control belongs on the
composer* — is delivered by the shipped tool, while
[`DESIGN-context-path.md`](DESIGN-context-path.md) §10 (lines 640-644) still
calls cross-ancestry reads "genuinely open" and waiting on you. The shipped
behaviour matches INTENT; the design page is stale, and stale in the dangerous
direction: a reader planning from it would believe an open question exists
where a decision already does. Confirm-and-close is a documentation item (§3,
F1 cluster), flagged to you because closing a "waiting on the operator" note is
yours to do.

### §6 Plugins all the way down. **Strong.** §7 Three surfaces. **Two proven, one absent.**

The terminal and one-shot surfaces are the best-verified they have ever been —
see the rung-one paragraph below. Rust embedding is real and well-covered by
builder methods. But INTENT §7c — non-Rust hosts — is **entirely unbuilt: no
binding crate, zero `extern "C"`, and no mention of Diplomat, UniFFI, or
cbindgen anywhere in the tree** (verified by search this run). §7c itself
blesses a survey as an acceptable first deliverable; even the survey does not
exist. Three surfaces are claimed; two and a half exist for Rust speakers, two
for everyone else.

One port also has no swap point at all: the core unconditionally provides the
subagent host — the thing that forks and spawns child agents — and there is no
builder method to supply your own
(`crates/conway/src/host_caps.rs:68-73`, and no `with_subagent_host` exists in
`crates/conway/src/builder.rs`). Every other port with a production alternative
has one. Whether that is "core-owned mechanism, correctly non-swappable" or "a
builder method nobody wrote yet" is not stated anywhere — an intent gap, put to
you in §7.

### §7a/§7b The daily-driver ladder. **Rung one is under a foot.**

This run did what no previous review did: ran the binary, live. One-shot
text/JSON/JSONL output, stdin piping, `--model` pinning, `--permission-mode
deny`, `routes explain`, and resume all worked on the first attempt, and the
operator docs matched observed behaviour. That is rung-one material — usable
alongside the incumbent for real one-shot and scripting work today. What keeps
it from more: the three operator findings in §3 (a resume-id trap, no way to
kill one runaway subagent, unmemorable session identity), all small, all the
kind of thing §7b says only daily use would have found. Daily use just found
them.

### §8.10 Cost of change. **Good bones, three named debts.** — see §4.

### Honesty gates (§8.1/§8.3, `CONTRIBUTING.md` §2). **Green, and this run re-verified them.**

All five fast gates pass at `7654041`, run fresh for this review
(`scripts/check-fast-gates.sh`: fmt, design-claims, board-citations, `cargo
doc -D warnings`, clippy). All 21 machine-checked claims in
`scripts/board-claims.md` hold. The "Where the tree is today" notes sampled
matched the code exactly. The remaining record drift (§3) lives in the places
no gate reads: doc comments, enumerations in prose, and a design doc's open
question. The gates are winning; the ungated territory is where the drift went.

---

## 3. Findings

Merged from six reviewers; every citation re-verified or spot-checked by the
lead. Ordered by how much each would change what gets built next.

**F1 — The record understates the tree, in four places (three reviewers, independently).**
The same defect class, found separately by the adherence, evidence, and
surfaces lenses, which is weight, not coincidence:

- Three production doc comments say `Selector::Operator`/`write_head` have no
  production caller — `crates/conway-core/src/path.rs:120-123`,
  `crates/conway-core/src/log.rs:363-367`,
  `crates/conway-runtime/src/runtime.rs:168-172` — and the caller shipped **in
  the same commit** (`c1a69de`, the `conway.path` plugin). The enum guard built
  specifically to catch stale variant claims does not watch `Selector`.
- `conway.trim`: 224 lines, compiled, tested, absent from every enumeration of
  the plugin tier (`PHILOSOPHY.md` §6, `docs/plugins/README.md`,
  `ARCHITECTURE.md` §2b), and unreachable from the shipped binary —
  conway-cli's own Cargo.toml comment admits naming `"conway.trim"` in
  `[plugins].install` "reaches nothing today"
  (`crates/conway-cli/Cargo.toml:103-105`).
- `ARCHITECTURE.md` §2b enumerates nine plugin crates; twelve exist
  (`discover`, `path`, `trim` missing) — a new contributor's crate map
  undercounts the plugin surface by a quarter.
- `DESIGN-context-path.md` §10 still lists as open the cross-ancestry question
  INTENT §5e answers and the shipped tool implements (see §2).

All are S-sized documentation fixes; the pattern is the finding. The gated
record is honest and the ungated record is where drift now accumulates —
worth extending the gates' reach (watch `Selector`; consider the enumeration
pages) rather than only patching the four instances.

**F2 — The operator can start and steer subagents but cannot stop one (operator lens).**
The model gets seven lifecycle tools (`conway_fork` … `conway_cancel`); the
operator gets `/fork`, `/spawn`, `/ask`, `/steer` — and no `/cancel` or
`/await` exists in the slash-command surface (verified by grep of
`crates/conway-cli/src/tui/commands.rs`). A runaway subagent burning tokens can
only be stopped by ending the whole session. M-sized; the sharpest daily-driver
gap this run found.

**F3 — The first thing a scripting user tries after the docs' own example fails (operator lens, live).**
One-shot JSON leads with `agent_id`; `--resume` rejects it and accepts
`transcript_ref`, which appears once in the operator docs
(`docs/scripting.md:141`), in an example, with no prose saying it is the
resume handle. Reproduced live in this run. One sentence of documentation, or
an alias — S.

**F4 — Non-Rust embedding has not started, and nothing says whether that is a queue position or an abandonment (surfaces lens).**
See §2. The survey INTENT §7c names as the acceptable first step is S-sized
and would convert "absent" into "decided and sequenced."

**F5 — `SubagentHost` is the one port an embedder cannot supply (surfaces lens).**
See §2. Either an M-sized builder method or one written sentence of intent;
currently it is neither, which fails INTENT §8.1 (the open question is the
defect).

**F6 — Two enums named `ConwayError` (sustainability, core+runtime).**
`crates/conway-core/src/error.rs:634` and `crates/conway/src/error.rs:19` —
different variant sets, not a wrapper relationship, significant enough that the
facade carries a 15-line comment and a `CoreConwayError` alias to manage the
shadowing (`crates/conway/src/lib.rs:98-114`). A recurring which-one tax on
every contributor; S-sized mechanical rename.

Positives verified in passing, because a review that only lists problems is not
a state of the union: plugin uninstallability is real (§1); every hook event
has a production dispatch site; the `focus_agent` invariant is centralized
behind a single wrapper rather than scattered (checked while hunting for the
opposite); and the operator docs are written from captured output, not
aspiration — the live run kept matching them.

---

## 4. Sustainability: where the tree is getting more expensive to change

The standing question, now with its own reviewers. The headline: **core and
runtime are disciplined; the cost is accumulating at the edges** — in the
plugin transport pair and in the test harness.

**The subprocess twins.** `conway-plugin-mcp` and `conway-plugin-subprocess`
(6,270 lines between them) independently implement the same
child-process-lifecycle mechanism: same module shape, near-verbatim error
taxonomies (spawn / timeout / session-died / malformed-frame), and a shared
default written down twice — `DEFAULT_TIMEOUT_MS = 5000` declared in both
crates, one carrying a comment that it must match the other, nothing enforcing
it (`conway-plugin-mcp/src/lib.rs:77`,
`conway-plugin-subprocess/src/lib.rs:130`). This pair has already produced one
real divergence: `kill_group` drifted five ways before being consolidated, and
that consolidation held (verified this run — both crates now wrap
`conway_tools::process::kill_group`). The wire protocols themselves correctly
stay separate — they change for different external reasons. The lifecycle layer
is one piece of knowledge written twice, and its fail-closed behaviour is a
safety property. M-sized, and it needs one shape decision from you first (§7).

**The test harness has a missing tier.** `build_conway` is hand-rolled in 46
test files, `text_response` in 52, `fake_router` in 36 — because
`conway-testkit` deliberately depends only on `conway-core` and structurally
cannot offer facade-level helpers. Any change to how a `Conway` is assembled
for tests is a 46-file edit. This is the same shape as the slash-palette drift
fixed in `9bbe6c6`, not yet caught only because nothing exhaustively matches
the copies. The fix is a thin facade-level test-support tier, not stuffing
testkit; M-sized.

**What is *not* expensive, checked deliberately.** The context-path concept has
high fan-out — 23 files reference it, and landing its write path touched 117 —
but it has exactly one authoritative representation
(`crates/conway-core/src/path.rs`), so this is a foundational abstraction
threading every layer by design, not five copies waiting to disagree. The
right response is budgeting for wide commits when path semantics change, not
consolidation. Similarly the two big files people worry about (`builder.rs`,
`tui/commands.rs`) read as flat, single-purpose aggregations — and the raw line
counts overstate them: `commands.rs` is over half inline tests.

---

## 5. What is good, said plainly

- **It runs, live, first try.** The strongest sentence in this document and it
  was not available to any previous review.
- **The gates are winning.** Two reviews ago the honesty gates were red; this
  run re-ran them fresh and all five pass. The drift that remains is in
  ungated prose, which is exactly what the gate strategy predicts.
- **The board is clean.** Two open items, both real, both claimable, no stale
  claims, no umbrella rot (surveyed via the live MCP board this run).
- **Uninstallability is real**, ruled in by code rather than claimed.
- **The consolidations hold.** `kill_group` and `canonical_json_bytes` — the
  two duplications previous cycles paid to fix — are still fixed.
- **The docs tell the truth the gates can reach**, and one reviewer's live
  session kept confirming operator docs against actual behaviour.

---

## 6. What this review did not check

The union of every reviewer's declared gaps, stated so this snapshot cannot
read as more complete than it is:

- **The TUI was never driven under a real terminal** — no pty in the review
  environment. Interactive behaviour was verified source-against-docs only.
  The single largest gap this run.
- Most operator doc pages (`getting-started`, `interactive`, `embedding`,
  `permissions`, `tools`, and the `docs/plugins/*` normative pages) were index-
  or spot-checked, not read against code. Security-bearing claims in
  `docs/permissions.md` specifically were not verified this round.
- `PHILOSOPHY.md` beyond its "Where the tree is today" notes; `GUIDE.md`.
- Full reads of `ports/plugin.rs` (3,087 lines), the permission broker's
  internals, and the routing health/breaker dispatch path.
- Fixture duplication inside `#[cfg(test)]` modules (as opposed to `tests/`
  directories).
- The full workspace test suite was not re-run (the five fast gates were; the
  last full run on record is `da9813c`: 223 suites, 3,209 passed, 0 failed).

---

## 7. Questions for you

Each is an INTENT §8.1 moment: the guidance, not the code, is what is missing.

> **Answered 2026-08-24, same day.** The operator accepted all six proposed
> sentiments; they are folded into `INTENT.md` (§7, §7a, §7b, §7c, §8.10) and
> `DESIGN-context-path.md` §10 is closed. The list below stands as the record
> of what this run put to the operator.

1. **Is the subagent host core-owned on purpose?** (F5) One sentence settles
   it; an M-sized builder method un-settles it the other way.
2. **Confirm the §5e reading and close `DESIGN-context-path.md` §10** — the
   shipped cross-ancestry behaviour matches INTENT §5e as written; the design
   page still says the call is yours to make. Is it made?
3. **Does every model-invocable lifecycle action owe the operator a command?**
   (F2) A yes makes `/cancel` a rule, not a feature request.
4. **May a session carry an operator-chosen name?** ULID-only identity is
   either a deliberate trade or a missing convenience; INTENT does not say.
5. **Non-Rust embedding: queued or dormant?** If queued, the §7c survey is the
   S-sized next step; if dormant, INTENT §7c should say so rather than imply
   progress.
6. **When plugin-tier crates need shared code, what shape is blessed** — a new
   shared crate, or another facade re-export like `kill_group`? The subprocess
   consolidation (§4) is blocked on this one sentence.
