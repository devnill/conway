# Contributing to conway

Build and test commands are in [`README.md`](README.md#development). The system
overview is [`ARCHITECTURE.md`](ARCHITECTURE.md), and the design intent behind
it is [`PHILOSOPHY.md`](PHILOSOPHY.md).

This page is the review discipline: what "done" means here, and the rules a
change is held to. Nearly every one of them was bought with a failure, and where
that failure is known it is named. A rule with its cost attached is one you can
argue with; a rule stated as taste is one you can only obey or ignore.

---

## 1. What "done" means

**Thin, testable slices.** A slice that cannot be demonstrated through the CLI
or a library test is not done. Prefer a narrow vertical (one backend, one tool,
one fork) over a broad horizontal layer nothing exercises. This also settles
ambiguous scope calls: take the smaller slice that keeps the system
demonstrable.

**Every mode reachable.** conway has three consumption modes (embedded library,
interactive TUI, one-shot) and no capability may exist in only one of them. The
TUI leads, because it is the fastest dogfooding loop and gets design attention
first, but modes are renderers over the same facade. A behavioral difference
between modes is by definition a renderer bug.

The acceptance question: can a terminal user drive it interactively, and can a
script or host application drive the same capability through the facade?

**Docs and CHANGELOG in the same change.** A user-facing capability, meaning a
tool, a CLI flag, or a facade method, lands with its `docs/` entry and its
`CHANGELOG.md` entry in the same commit. `docs/` is organized around what a user
*does*, not around workspace crates: someone looking for "how do I configure
permissions" should not have to know which crate implements it.

Design material — rationale, rejected alternatives, why a thing is shaped the
way it is — does not satisfy that gate and is not what `docs/` is for. It goes
where the thing it explains is: a doc comment beside the code, `docs/plugins/`
for the extension architecture, `CHANGELOG.md` for what changed and why. There
is no separate design directory, deliberately. A design document that lives
apart from its subject goes stale without anyone noticing, and the reader who
needed it is the one least likely to have found it.

A forward-looking design for work not yet done still goes beside the code that
will have to change — see `crates/conway-core/src/containment.rs` for the
worked example of that shape.

---

## 2. Nothing may claim to be reached that isn't

This is the rule the project has broken most often, and the one it has spent the
most on.

A capability may ship ahead of its consumer. **Inertness is not the defect.** For
a framework, a declared-but-unused capability is frequently correct: forward
compatibility, wire stability, deliberate seams, vocabulary coherence.
`ContextHook` shipping with no default implementation was right, not an
oversight, because keeping policy out of the core is the whole architecture.

The defect is the **mismatch between what a declaration promises and what a
reader would believe**. `TruncationPolicy::Artifact` was not a bug because
nothing constructed it. It was a bug because its doc said "spill the full output
to an Artifact, keep a pointer in context" while a tool declaring it got no
truncation at all, which is the inverse. Labeled "reserved; not yet
implemented," it would have been unremarkable.

**Rank harm by who believes the promise**, not by whether the mechanism runs:

1. **Worst: user-facing configuration that does nothing.** `probe_enabled` was
   settable in `settings.json` and defaulted to `true` while the prober was
   never spawned. A user bought a behavior and got silence indistinguishable
   from success.
2. **Bad: a documented contract that is wrong.** One-shot exit codes 3 and 4,
   declared and unreachable, so a script has dead branches.
3. **Mild: an internal declaration nobody relied on.**

**A label is not enough on its own, because the default is often the actual
lie.** Before labeling a forward declaration, check what it asserts when nobody
configures it. A default-on key asserting an unbuilt behavior is rung 1, no
matter what the doc comment says. "Designed, not built," stated plainly and off
by default, is a known limitation; the same thing over a default-on key is a
lie.

A legitimate forward declaration is labeled at **every** declaration site, not
only in the documentation, in any docs that mention it, and points at the open
work that will wire it. All of that is load-bearing: `docs/routing.md` carried
its half of the health-prober label candidly while the module doc, the crate
re-export, and the field's own doc comment were all silent.

A capability shipping as a labeled forward declaration is not exempt from the
docs gate in [What "done" means](#1-what-done-means). The label *is* the
documentation entry, and it says "not yet implemented" rather than describing
the behavior in the present tense. Documentation of an unreached capability is
worse than none, because it manufactures confidence a test would have denied.

### Triage: what to do about a mismatch, in order

0. **First ask whether the declaration belongs on that axis at all.** Sometimes
   none of the three answers below is right and the mechanism should go. The
   cargo feature table advertised backend combinations that did not build; a
   backend was never a compile-time choice, so retiring the features dissolved
   the defect by construction. Ask this first, or you will fix the symptom
   faithfully and preserve the wrong shape.
1. **Implement.** The consumer exists and only plumbing is missing. This is the
   cheapest answer, and the reason "delete the lie" must never be the default.
2. **Delete.** Three distinct sub-cases, not to be conflated: the missing part
   is an *opinion*, so the core keeps the seam and drops the declaration; *no
   truthful implementation exists* without machinery the project has declined to
   build; or *nothing ever intended it*.
3. **Label.** Working code exists and wiring is deliberately deferred behind a
   stated gate, default included.

**Before deleting, check the mirror-image error.** Removing a declaration that
*is* still reached is the same defect inverted. And verify the name you are
deleting is the name you think it is: one deletion's own confirming check was
wrong, because a differently-scoped live field shared the identifier.

### The one exemption, and its conditions

[`PHILOSOPHY.md`](PHILOSOPHY.md) is written in the present tense throughout,
including for a few things the code does not do yet. That is deliberate. The
page states the shape the system is meant to have and the code is built to match
it, which is a legitimate method and a better one than letting the page trail
whatever happened to get built.

It is an exemption for that page's prose and nothing else. The conditions:

- **Every present-tense claim that is not yet true is an open board item
  carrying a falsifiable predicate.** The owner's
  ruling was that `PHILOSOPHY.md` should describe the ideal 1.0 state, not
  carry the debt required to get there, and that the debt belongs on the board
  instead. The ledger's load-bearing sentence survives in the new form: **a
  claim not carried by an open board item is expected to be true right now.**
  The mechanical, falsifiable part of what the ledger tracked — an `absent`
  pattern for "not built yet", a `present` pattern for "this exists", each
  failing the moment it stops being true — moved to
  [`scripts/board-claims.md`](scripts/board-claims.md), evaluated by
  [`scripts/check-design-claims.py`](scripts/check-design-claims.py) and gated
  in CI on every change; the narrative each claim used to carry as ledger prose
  now lives on the board item that owns it. Board items have ids, not numbers,
  so the renumbering-on-clearance problem the ledger carried (see its own git
  history) does not recur.
- Nothing else inherits it. `docs/`, `CHANGELOG.md`, doc comments, config keys,
  and defaults are governed by this section in full. A settings key that exists
  and does nothing is rung 1 regardless of what the philosophy page says.
- A gap is cleared by building it (close the board item, and the predicate that
  names it starts failing until someone updates or removes it in the same
  change) or by amending the page. A gap that sits open unexamined stops being
  design intent and becomes the thing this section is about.

**An audit resolves a mismatch in the code, not in the page.** The page states
what conway is meant to be; the tree is what it currently is. When they
disagree, the finding is against the tree by default, and the fix is a work
item. Editing the page to describe what the code happens to do turns a
requirement into a mirror, and it destroys the completeness contract in the
process: a claim quietly softened to match the tree, with no board item filed
for the gap it used to name, means the gap stops existing without anyone
deciding it should.

The page may be edited when it is internally inconsistent, or when it says
something nobody intended. Even then the bar is that the edit makes the
requirement clearer, never that it makes the requirement easier to satisfy. If
you cannot tell which you are doing, you are doing the second one.

### Module docs and other doc comments are review-only, not gated

`scripts/check-design-claims.py`'s predicates pin specific sentences in
specific files named by path, and `cargo doc`'s link check verifies that an
intra-doc link *resolves*, not that the sentence around it is true. Neither
mechanism asks a `//!`/`///` block the same "is this still true" question
`board-claims.md` answers for `PHILOSOPHY.md`. `crates/conway-cli/src/
first_party_plugins.rs`'s module doc said memory, skills, and MCP support
were unbuilt roughly 120 lines above the lines in the same file that install
all three (board item `01M0HDK6CDSE1QJW3HD58ND8WY`) — invisible to both
gates: the predicate that would have caught the equivalent prose claim in
`PHILOSOPHY.md` names paths, not sentences, and it happened not to name this
one; `cargo doc` had nothing to check here at all, since the sentence links
to nothing.

**This class is caught by review, not by CI.** A `board-claims.md` predicate
can pin one sentence once someone notices it is load-bearing; it does not
scale to every module doc in the tree, and inventing a predicate per doc
comment would just move the same staleness one file over, onto whichever
predicates nobody thought to write. Until a mechanized check exists for this
shape, a reviewer reading a module doc adjacent to code that changed should
treat "does this comment's claim about what's built still match what the
file does" as part of the review — the same discipline this section already
asks for prose, applied without a gate behind it.

### Citing a board item, and keeping the citation honest

The tree cites board items by id in roughly 920 places. Two rules:

- **A citation that implies pending work must name something that is actually
  open.** "Tracked under `01K…`", "deferred to `01K…`", "`01K…` tracks that" —
  each is a promise that a reader can follow to live work. Pointing one at a
  `done` or `cancelled` item is a dangling promise, and pointing it at a
  *decision* is a category error: a decision is a settled ruling, not trackable
  work.
- **A citation naming a closed item is fine as provenance.** "Implemented by
  `01K…`", "the prober was retired (`01K…`)" — these say why the code is the
  shape it is, and closing the item is what makes them true. Do not "fix" these.

`scripts/check-board-citations.py` enforces the distinction. It resolves every
cited id against **both** id namespaces — the work board and the record store —
because they share an id shape and a resolver that checked only one would report
half the tree as dangling.

**It is not a CI job, and that is a limitation rather than a design choice.**
Both stores are local-only tooling state excluded by `.gitignore`, so CI and a
plain clone have nothing to resolve against. When the stores are missing the
script exits **2** and prints `SKIPPED` — never 0, because a run that verified
nothing must not report success. Run it on a maintainer checkout when you add or
retire a citation, and when you close a board item that the tree cites.

## 3. A check is not established until it has been shown to fail

"Any check that cannot fail is not a check" started as a corollary of
[the declaration rule](#2-nothing-may-claim-to-be-reached-that-isnt) and was
violated three times in a single cycle by people working under it. That is
evidence not that the rule was wrong, but that it had no procedure attached.
The procedure:

- **Assert on the observable outcome, not an intermediate signal.** The
  `AutoAllow` deny test asserts on the persisted `ToolResult` text rather than
  gate-call count, because a correct refusal and a silent full bypass both
  produce zero gate calls. An assertion on the intermediate passes against the
  broken guard.
- **A unit test is not a liveness test.** Exit code 4's unit test passes and the
  behavior is still wrong: it exercises the mapping function, not the path that
  would call it. Drive a production entry point.
- **Name the discriminating observable before writing a break-the-guard stub.**
  Ask which specific value, read through which specific API, would differ if the
  bug were still present. If the answer is "the error would say...", stop. Error
  text is usually shared across construction sites by one `Display` impl, and it
  collapses exactly the distinction the test needs.
- **A coverage claim is not established until a stub has been run.** Say "I could
  not find a test that does X" until a run turns it into "nothing catches X."
  Reading fixtures is not measurement. One claim that a regression "would pass
  every test in the workspace" was disproved by two stub runs: four pre-existing
  tests already caught it, and the real gap was much narrower.
- **A stub must break exactly the thing under test and nothing upstream of it.**
  Stubbing a process-group kill to send *no* signal meant the shell never
  exited, so the test failed on its own invoke timeout rather than on the
  assertion being measured. If the failure message is not the one the assertion
  would produce, the experiment did not run.
- **Pair a stub with a scoped, fast verification, never a workspace-wide run.**
  The first attempt at that measurement hit the command timeout and was killed
  before its restore step, leaving the mutation in the working tree. A mutation
  you cannot reliably undo is worse than no measurement.
- **Acceptance must not depend on one machine's local configuration.** A
  criterion needing a particular API key, a running local server, specific
  hardware, or a model that happens to be pulled is unavailable to CI, to a
  contributor, and to the same operator on another machine. This tree has the
  credential-free machinery already: `ScriptedBackend`, the fakes family, and
  `wiremock`. The strongest form is compile-guarded, environment-free by
  construction. A live run against a real provider is legitimate *evidence* and
  never a *criterion*. The second-order hazard matters more: letting
  reachability decide coverage silently privileges whatever the author can hit
  today, so the dialect nobody has credentials for is the one that rots.
- **Audit a guard's scope against its name.** A check narrower than its name is a
  declaration/behavior mismatch one level up, and
  [the declaration rule](#2-nothing-may-claim-to-be-reached-that-isnt) applies to
  it. CI ran `cargo check` across the feature matrix, so it could not catch a
  test that failed to *compile* under a combination. It then ran
  `cargo test --no-run`, which compiled but still did not execute -- and that
  residual gap hid a real one: 19 of 27 `crates/conway/tests/builder.rs` tests
  failed the moment anyone actually ran them under a non-default combination
  (board item `01M0JMEK24ZPRSP3PT3HJG0VHD`). It now runs the suite for real
  under all six combinations, at negligible added cost over the compile time
  already paid. Any gap still open is priced and stated in `.github/workflows/ci.yml`
  itself, not papered over.
- **A verification instruction that names some of the gates is read as naming
  all of them.** CI runs eight gates (`.github/workflows/ci.yml`); a worker
  told to run "build, test, clippy and fmt" reasonably reads that as
  complete, because nothing said otherwise. Two workers in the same batch, on
  2026-08-24, each shipped a public doc comment linking to a private item —
  `error: public documentation for X links to private item Y` under
  `RUSTDOCFLAGS=-D warnings` — because neither instruction, nor either
  worker's own list of commands, named the `cargo doc` job (board item
  `01M0TJC2GY2C81Y9R1KKWT5PJJ`). CI's `doc` job would have caught both before
  merge either way, so the real cost was a wasted round-trip and a manual
  catch, not a shipped defect — but a check that only runs when someone
  happens to remember it is not the check its job name promises.
  `scripts/check-fast-gates.sh` now runs the five CI gates that are fast,
  network-free, and need no extra toolchain (fmt, design claims, board
  citations, rustdoc, clippy) in one invocation, naming each gate as it runs
  so a bare pass/fail can't hide which one refused; `.github/workflows/ci.yml`
  invokes the same script per gate rather than a second, hand-kept spelling of
  each command, so the local and CI invocations cannot drift apart the way
  this incident's absence did. See [`README.md`](README.md#development).

---

## 4. Measure before you optimize

Whenever feasible, measure a baseline **before** implementing an efficiency
improvement, and gate the change on it demonstrating value. If a change cannot
be measured, say so explicitly and treat that as an argument against shipping
it, not as a reason to skip the step.

This is a rule rather than a preference because conway has no benchmark, no
profiling harness, and no timing instrumentation. An efficiency change cannot
currently be shown to have helped, which is precisely the condition the rule
exists to stop us shipping into.

**A correctness or security fix is never gated on a baseline**, but that
exception is not self-certifying. Anything can be relabeled a repair to skip the
baseline, or an optimization to defer the work. **The test: name a failure mode
the existing mechanism structurally cannot catch.** If you can name one, it is a
repair and ships ungated. If an honest attempt cannot find one, it is an
optimization and the rule binds. The health prober failed exactly this test. A
crashed local server that comes back up *is* detected by the next real request,
so probing shaves a failed round trip: latency, not correctness. Record the
attempt and its answer; do not assert the classification.

**A deferral incurs an obligation under
[the declaration rule](#2-nothing-may-claim-to-be-reached-that-isnt).**
Deferring an optimization does not license the declaration to keep claiming the
behavior. Label it at every site, say where the measurement will happen, and
make sure the default does not assert what was deferred.

**Deletion is a legitimate outcome of the measurement**, and if taken it must go
all the way. A labeled-but-dead mechanism sitting beside a live one is the same
trap one level down.

---

## 5. Safety-bearing code

**One implementation, and callers call it.** Path canonicalization, token and
headroom arithmetic, and any computation a security guard depends on call a
single shared function rather than restating it at a second callsite. A
restatement drifts, and the guard in the duplicate is silently omitted: the NUL
guard was missing from inlined path resolution at two root-enforcement sites.
When a function is extracted to be the single source, the rule is that callers
*call* it, not that they reproduce it.

**Deny and prompt rules fail closed, never silently open.** A safety rule must
either match its intended targets or fail closed and surface. It must never
silently match *no* targets, as a `paths_under` rule does on a tool it cannot
confine, nor the *wrong* ones, as a relative prefix does when canonicalized
against the process cwd rather than the project root. A rule that silently does
other than what the operator wrote is a defect even when it is syntactically
valid.

**Permission rules stay visible and individually revocable.** An operator must
be able to inspect every active allow, deny, and prompt rule, including
structured rules and rules contributed by an untrusted repo, and revoke any
single one without disabling the whole permission system. A rule that is applied
but cannot be seen or individually revoked is a trap, not a policy. Typed
registration failures are surfaced, never produced and discarded.

**Model-supplied tool arguments are untrusted input.** Bounded arguments are
range-checked at the tool or adapter boundary; out-of-range values map to a
typed `ToolError`, never a panic. A model-input-triggered panic is a defect
class on par with a crash, so `unwrap`, `expect`, and duration overflow on
model-supplied values are forbidden.

**When a dangerous surface needs constraining, constrain reach or remove the
tool. Do not add policy to the gate.** The shell-metacharacter blocklist is the
cautionary example: core policy logic with conflicting use cases, which
manufactured confidence it could not deliver (three documents claimed it
guaranteed human review, and none was true under `AutoAllow`), and strengthening
it measured a 68% false-positive rate against this repo's own logged bash
commands. What actually helped was making `bash` opt-in.

**State limits plainly, not reassuringly.**
[`ARCHITECTURE.md`](ARCHITECTURE.md#34-tools-as-plugins-behind-a-permission-gate)
says outright that a confinement root does not confine what a shell command
does, and that the check has a TOCTOU window. Security documentation that
reassures beyond what the mechanism delivers is the failure mode
[the declaration rule](#2-nothing-may-claim-to-be-reached-that-isnt) exists to
prevent, in the place it costs the most.

---

## 6. Standing constraints

**Rust core.** The library and CLI are Rust so the library embeds in-process in
a host application and ships as a single-binary CLI. Other-language SDKs are
thin clients added later, never an alternative implementation of core logic.

**Release-ready from the start.** conway is developed privately and intended for
open-source release, so code hygiene, licensing, and dependencies must be
release-ready throughout, with no proprietary or unlicensable dependencies.
`docs/` is the public face and is held to that standard.

**Dependency minimalism is operative, and it is now the rule rather than a
practice.** This was flagged as unsettled from 2026-08-06: the constraint's text
forbade only *unlicensable* dependencies, while it was routinely cited as
forbidding *new* ones, and the practice was uniformly the strict reading. Ruled
on 2026-08-12 in favour of the strict reading, so the two now agree:

- **Any new dependency is justified against the zero-dependency path**, in the
  change that adds it. "What would it cost to not have this?" is the question,
  and for a harness this small the answer is often "less than you think" — the
  worked examples are `directories` (kept: cross-platform home-directory
  resolution is a real contract, not arithmetic — though note the original
  justification was XDG precedence, which conway no longer implements, so this
  case is weaker than it was and deserves re-testing when it is next touched)
  and the several places a helper was written inline instead, each noted at its
  own site.
- **Expect the answer to be no.** The bar is not "this crate is good"; it is
  "the zero-dependency path is worse, and here is why".
- **Tooling whose purpose is enforcing licensing or dependency policy is
  exempt.** Without this carve-out the rule forbids its own enforcement:
  `cargo-deny` is a new dependency, and under the strict reading with no
  exception the tool that checks the constraint would be forbidden by it. The
  exemption is narrow — it covers tools that *check* the policy, not tools that
  are merely useful to have.

The exemption is not a general "CI tools are free" clause. A dependency added
under it must be a policy checker, must run in CI rather than ship in the
binary, and must be named here or in the change that adds it.

**Publishing to crates.io is deferred with intent**, not refused and not
undecided. It will happen; it is not a priority now, and it has no connection to
the AGPL-3.0-only licence. `publish = false` across the workspace guards the
deferral structurally. Only the timing is open.

---

## 7. How these rules change

They are amendable, and an amendment carries its reasoning.

[The declaration rule](#2-nothing-may-claim-to-be-reached-that-isnt) originally
demanded a test proving every declared capability is *reached*. That was too
strong for a framework, since inert capabilities are often exactly right, so it
was narrowed to what it should always have said: nothing may claim to be reached
that isn't. A rule elsewhere that kept propagating the retired wording was found
and corrected at the same time.

Three habits follow, and they are the actual working discipline:

**Write decisions down, and do not silently revisit them.** A decision is worth
recording only with something a future reader can check, plus the conditions
that would justify reopening it. Documentation that still poses a question a
decision has already closed will get it reopened by the next reader, so it gets
updated when the decision lands. Reversal needs new evidence, not a fresh
opinion.

**Correct your own work in the open.** Calling work done against unmet
acceptance criteria, misreporting a test matrix because of a shell-quoting bug,
deriving a licensing conclusion from a premise that was wrong: all of these
happened, and all are written down with what actually caused them. The point is
not confession. A rediscovered surprise costs more than a recorded one.

**Prefer stating a gap to closing it prematurely.** The residual CI gap in
[the check rule](#3-a-check-is-not-established-until-it-has-been-shown-to-fail),
the TOCTOU window in [`ARCHITECTURE.md`](ARCHITECTURE.md), and the permission
gap left open when confinement stayed opt-in are each priced and written down
rather than obscured. A known limitation is a working state. An unstated one is
the defect [the declaration rule](#2-nothing-may-claim-to-be-reached-that-isnt)
is about.
