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

Design material, meaning rationale, rejected alternatives, and decision records,
does not satisfy that gate and does not belong in `docs/`. It lives in
`.design/`. A capability documented only in `.design/` is undocumented for this
rule.

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

- Every present-tense claim that is not yet true is listed in
  [`.design/philosophy-debt.md`](.design/philosophy-debt.md), with what exists
  today and what would make the claim true. A claim absent from that ledger is
  expected to be true right now.
- Nothing else inherits it. `docs/`, `CHANGELOG.md`, doc comments, config keys,
  and defaults are governed by this section in full. A settings key that exists
  and does nothing is rung 1 regardless of what the philosophy page says.
- An entry is cleared by building it or by amending the page. An entry that sits
  unexamined stops being design intent and becomes the thing this section is
  about.

**An audit resolves a mismatch in the code, not in the page.** The page states
what conway is meant to be; the tree is what it currently is. When they
disagree, the finding is against the tree by default, and the fix is a work
item. Editing the page to describe what the code happens to do turns a
requirement into a mirror, and it destroys the ledger's contract in the process:
a claim quietly softened to match the tree never appears in the ledger, so the
gap it recorded stops existing without anyone deciding it should.

The page may be edited when it is internally inconsistent, or when it says
something nobody intended. Even then the bar is that the edit makes the
requirement clearer, never that it makes the requirement easier to satisfy. If
you cannot tell which you are doing, you are doing the second one.

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
  test that failed to *compile* under a combination. It now runs
  `cargo test --no-run`, which compiles but still does not execute. That residual
  gap is priced and stated, not papered over.

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

> **Unsettled, flagged rather than papered over.** That constraint's text
> forbids only *unlicensable* dependencies, but it is routinely cited as
> forbidding *new* dependencies at all, and the practice is uniformly the strict
> reading. Either the constraint should say what everyone cites it for, or those
> citations are wrong. Both cannot stand, and it has not been ruled on. Until it
> is, treat the strict reading as operative and justify any new dependency
> against the zero-dependency path.

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
