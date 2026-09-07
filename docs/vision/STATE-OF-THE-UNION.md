# State of the Union: conway

**Reviewed 2026-09-04 against the working tree at `6c85ace`, version 0.10.0.**

> Written for the operator. It assumes you care about the shape of the system
> and not about the shape of any particular trait. Everything in it was
> checked against the code in this run; where you might want to check
> something yourself, the file and line is given.
>
> Snapshot document — replaced wholesale on the next run of
> [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md).
>
> **This was a Normal round: 6 reviewers, dispatched in parallel** —
> adherence, surfaces, operator, evidence, and sustainability split
> `core+runtime` / `cli+plugins+harness`. Zero overlapping findings came
> back, which is itself evidence the territory split held. §6 lists what
> even six reviewers didn't reach.

---

## 0. The verdict, in three sentences

**The tree is honest about itself and cheap to keep that way — the previous
review cycle's own discipline (the doc-comment convention, the claims
ledger, the shared-fixture sweeps) is visibly still being followed, not just
documented.** Six reviewers covering adherence, surfaces, the CLI/TUI, one-
directional proofs, and two sustainability territories came back with **zero
critical findings, four significant, and ten minor** — no shipped defect on
the scale of last cycle's unreachable Claude-compat commands, and every
significant finding this round is a real but boundable gap rather than a
misrepresentation. The most consequential single finding is not a code
defect at all: **a fresh install's very first `conway -p` invocation can sit
completely silent for 90+ seconds**, because the default output mode gives
no progress signal and no deadline bounds a slow local backend — exactly
the kind of "works, but reads as broken" failure this process's own conduct
file (§5, failure mode 4) exists to catch. The board is genuinely drained:
of 60+ items surveyed, only three remain open, and two of those three are
explicitly out of scope for anyone but the operator (§7's live-use CAPSTONE,
and an item the operator is editing live in a separate session right now).

Two structural findings from the surfaces reviewer are named here plainly
rather than folded into the findings list, because they are not board-item-
sized: **INTENT §7c (non-Rust embedding) remains entirely unbuilt**, and **no
extension port in this tree has ever been used by anything that is not
conway's own authors** — every worked example, every test double, every
"proven" claim in the docs is, by the surfaces lens's own strict bar
(INTENT §8.5), still *exercised*, never *proven*. Neither is new information
exactly — this project has never been dogfooded by an outside user — but
this is the first review cycle to state it as a named, cited finding rather
than an ambient fact everyone already knew.

---

## 1. What is built, as blocks

Unchanged in shape from the last snapshot.

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
                    │   THE PLUGIN TIER    │   twenty-one crates, all optional
                    └──────────────────────┘

  in-process, compiled in:    routing · backends · history · stepguard ·
                              skills · memory · skeleton · path · discover ·
                              trim · names · claude · idiom · marketplace ·
                              confine · statusline · ui
  OUT of process, no rebuild: subprocess-host · mcp-client
```

Plugin uninstallability is real and enforced (`first_party_plugins.rs`); the
extension surfaces under `crates/conway-core/src/ports/` (17 port modules)
each have at least one real, out-of-core implementation — with the three
named exceptions in §3 F6.

**Two mechanisms landed since the last review** and dominate what changed
structurally: the **shared agent-knobs composition** (`AgentKnobs`,
`crates/conway-core/src/agent.rs`), consolidating seven fields that used to
be hand-duplicated across five spec structs into one composed struct with a
proven "add one field, touch one place" property; and the **tool-result
admission gate plus adaptive headroom default**, closing the standing gap
where a genuinely oversized conversation had no recovery path — refused
with a note and two remedies, never silently truncated or compacted, per
the operator's own 2026-09-01 ruling. A ten-file, 47-commit doc-comment
sweep also landed across the engine's largest files, independently reviewed
and confirmed comment-only.

---

## 2. Scored against [`INTENT.md`](INTENT.md)

- **§7a "fully functional and not heavy."** The daily-driver bar is where
  this cycle's most consequential finding lives — §3 F1. Everything else the
  operator lens checked (slash-command/tool parity for agent lifecycle,
  permission-mode cycling, headless `sessions`/`routes explain`/`tools
  list`/`plugin list`) holds.
- **§7c non-Rust embedding.** Unchanged: the survey (`DESIGN-bindings.md`)
  is the acceptable first step per that page's own scope note, and nothing
  beyond it exists. See §3's structural findings.
- **§8.5 "a surface is proven when something that is not its author uses it
  to do a thing someone wanted."** Scored explicitly this cycle for the
  first time, tree-wide, by the surfaces lens: **no port clears this bar
  today.** Every worked example is first-party. See §3.
- **§8.10 cost of change.** Both sustainability reviewers found the tree
  actively practicing this — the `AgentKnobs` refactor (§1) and the shared
  `ConwayConfig` test fixture are both cited as *evidence* of §8.10 being
  followed, not just findings against it. The ten minor findings in §3 are
  the next layer down, not a reversal of that verdict.
- **§8.3 declaration honesty, both directions.** Held tree-wide except one
  case: `[limits].max_parallel_tools`/`tool_timeout_secs` are disclosed
  in a source module doc but not in the operator-facing config reference
  that shows an example using them (§3 F3) — an internal disclosure that
  hasn't reached the surface an operator would actually read.

**What INTENT does not yet answer, surfaced by this round** — six Intent
gaps came back from three reviewers; none is a duplicate. Drafted as
proposed amendments in §7, for you to fold in or reject — not applied to
`INTENT.md` by this run.

---

## 3. Findings

Zero critical. Ordered by how much each would change the plan, per CONDUCT
§4; **no finding below was reported by more than one reviewer** — the
territory split (whole-tree × 4 lenses, `core+runtime` / `cli+plugins+
harness` for sustainability) produced no collisions to merge.

### SIGNIFICANT

**F1 — A fresh install's first one-shot invocation can sit silently for
90+ seconds, with no progress signal and no default deadline.**
`crates/conway-cli/src/cli.rs:53` (`output_format` defaults to `Text`);
reproduced live against an isolated `$HOME` with no prior config —
`conway -p "..."` on this machine's local backend gave no output for 90+
seconds in the default text mode, while `--output-format jsonl` on the
identical invocation showed real activity within milliseconds
(`model_decision` choosing a local model, a 10,055-token tool-registry
segment for a bare prompt). `deadline_secs` defaults to `0` — unbounded
(`crates/conway/src/config/schema.rs:407`). The underlying model latency is
real (a raw `curl` to the same endpoint took 56.6s), not a conway artifact —
but conway's default mode has no mechanism to show anything is happening
while it waits. Against §7a. **What done looks like:** text mode shows
something (a spinner, elapsed-time notice, or first-token stream) before the
full reply lands, and/or a sane default deadline so a stalled backend fails
loud. Size M. **Cost of not doing it:** this is most new operators' literal
first impression of the tool.

**F2 — `SessionMeta.labels` has a complete, documented, tested READ path
and no write path reachable from the shipped CLI or TUI.**
`crates/conway-runtime/src/runtime/root.rs:213-219` — the field's own doc
admits the write side has never existed for anything but a direct facade
caller (`conway::SessionSpec::labels`); confirmed by grep, neither
`oneshot.rs` nor `tui/app/startup.rs` ever sets `.labels`, and no `--label`
write flag or `/label` command exists anywhere. Meanwhile `sessions list
--label`, `conway.discover`'s `label` parameter, and two docs pages all
describe it as usable. Against §8.5. **What done looks like:** either a
`sessions label`/`unlabel` subcommand (mirroring the existing `name`/
`unname` pair) ships, or every doc describing `--label` gets a caveat that
nothing in the shipped product can populate it. Size S either way. **Cost
of not doing it:** an operator who reads the docs and tries `--label` gets
silent, permanent non-results with no error.

**F3 — `[limits].max_parallel_tools` and `tool_timeout_secs` parse,
validate, and round-trip, but never reach a running root session — and the
public config reference doesn't say so.**
Self-disclosed in `crates/conway/src/builder.rs:59-74` — both fields have
"no wiring point" comments naming the exact gap. `docs/agents.md:286-298`
shows a `settings.json` example setting `max_parallel_tools` with no
caveat. The schema default (4) matches the hardcoded fallback
(`crates/conway-runtime/src/subagent.rs:894`), which is why this is
invisible until an operator deliberately changes the value. Against §8.3.
**What done looks like:** wire both fields through `RootSpec`/`AgentSpec`
(M), or at minimum add `builder.rs`'s own disclosure to the operator-facing
doc next to the worked example (S).

**F4 — `PermissionBroker`'s allow/deny/prompt trio triplicates the same
fail-closed validation, and the pattern has already produced one real
shipped bug.** `crates/conway-runtime/src/permission.rs:963-1076` — three
near-identical write-side methods, then the identical split repeated again
on the read side across six more methods (`:1227-1316`), each acknowledged
in its own comment as a copy of the one beside it. `git show d508a5d`
("paths_under deny/prompt on unconfinable tools fails closed") is a real,
previously-shipped defect where this exact check was correct in one branch
and fail-open in another, because the enforcement lived at three call sites
instead of one seam. Against §8.6/§8.10. **What done looks like:** one
shared helper for the write-side canonicalize-and-validate step, one for the
read-side flat/structured split; the six public methods become thin
callers. Size M, no public API change, existing tests pin behavior
throughout.

### MINOR

**F5 — `conway-testkit` has a `Fake*` double for every core port except
`Plugin`, and 8 files in `conway-runtime/tests` hand-roll near-identical
`FakePlugin` copies as a result.** `crates/conway-testkit/src/lib.rs:1-6`
lists 9 doubles, `Plugin` absent; `grep -rl "struct FakePlugin"
crates/conway-runtime` → 8 files. All 8 were correctly updated when
`PluginManifest` gained a field, but by hand, not by the compiler. Size S.

**F6 — Atomic write-then-rename persistence is independently reimplemented
at ≥5 sites, and three of five skip the `fsync` the other two correctly
include.** `session_names.rs:251`, `tui/history.rs:57`, and
`conway-plugin-names/src/lib.rs:392-399` all omit `sync_all` before the
rename; `conway-tools/src/fs/write.rs:84-105` and `fs/beneath.rs:231-286`
correctly include it, and are the pattern the other three's own comments
cite without calling. Size S — one shared helper closes it.

**F7 — 129 plugin-id string literals restate a `PLUGIN_ID` constant that
production code in the same files already imports correctly.**
`crates/conway-cli/src/tui/app/plugin_toggle.rs` alone has 115, almost all
inside `#[cfg(test)]`. Size S, mechanical.

**F8 — The `ConwayConfig` test-fixture consolidation didn't reach
everywhere: 21 test files still hand-roll `ConwayConfig { .. }` against 63
that now use the shared fixture.** Measured directly (`grep -rl`), not
sampled. Size S — re-run the original sweep's own grep against current
`main`.

**F9 — `docs/plugins/README.md` calls its own twelve-plugin section
"eleven" in three cross-reference sentences (`:200`, `:224`, `:242`), after
correctly updating the section header and bullet list to twelve.** Verified
directly. Size S, three-word fix.

**F10 — `remember`/`forget`/`list_memories` are reachable by the model with
no operator-typed equivalent anywhere — no slash command, no CLI
subcommand.** The only operator-visible trace is a token-count line in
`/context`. Whether this is a real gap depends on an Intent question — see
§7 Q1. If it is: Size S, a `/memory` command or `conway memory
list/forget` mirroring the pattern.

**F11 — Three ports have no embedder-supplied path at all, with no stated
rationale for two of them.** `SessionStore` — facade doesn't re-export what
implementing it needs; `ArtifactWriter` — no `with_*` method and no
`Plugin` contribution point exist; `HealthRegistry` — the facade states the
gap with no rationale, unlike `SubagentHost`'s explicit single-authority
argument. Depends on the Intent question at §7 Q3. Size S–M each once
scoped.

**F12 — Three ports (`RouterFactory`, `ToolObserver`, `PluginEventEmitter`)
have exactly one implementor each.** Not automatically a defect — the
surfaces lens predicts most of this tree sits here — but worth a second
implementor before trusting any of the three traits' shapes are general
rather than fitted to their one user. No board item; a note for whoever
next touches one of these traits.

### Structural — named, not board-item-sized

**F13 — INTENT §7c (non-Rust embedding) is entirely unbuilt.** Zero binding
code exists anywhere in the workspace; `DESIGN-bindings.md` itself states
the gap. Size L, and there is no acceptance bar in `INTENT.md` for what
"done" would even mean here — see §7 Q4.

**F14 — No extension port in this tree has ever been used by anything that
is not conway's own authors.** Every worked example — `conway-plugin-
skeleton`, `custom_permission_gate.rs`, `router_factory.rs` — is written
inside this repository. By the surfaces lens's own bar (INTENT §8.5,
verbatim), every port sits at *exercised*, none at *proven*. This is the
same fact this project's own memory already carries as "never dogfooded";
this cycle is the first to score it against §8.5 explicitly and find every
single port, not just the product as a whole, on the wrong side of the
line.

---

## 4. Sustainability: where the tree is getting more expensive to change

Two dedicated territories ran this cycle — `core+runtime` and
`cli+plugins+harness` — the first full sustainability pass since the
process gained this lens. Both verdicts: **fundamentally healthy, five
small, well-scoped consolidation opportunities between them** (F4-F8
above), plus two pieces of *evidence the discipline is working*: the
`AgentKnobs` refactor and the `ConwayConfig` fixture sweep were both cited
by their respective reviewers as the tree actively practicing §8.10, not
just claiming to.

The one recurring shape worth naming across both territories: **a correct
pattern gets written once, correctly, and then reimplemented nearby instead
of called** — the atomic-write helper (F6), the `FakePlugin` gap (F5), and
the `PLUGIN_ID` literals (F7) are three instances of the same thing: the
shared version exists, is even documented as the canonical shape in at
least two cases, and gets copied-by-hand anyway rather than imported. None
of the three is expensive to fix; the pattern itself — closing the loop
between "we consolidated this once" and "every future call site actually
reaches for it" — is the standing question worth watching.

**Not assessed this cycle:** the tool-result admission gate and
`AgentKnobs` composition landed too recently for either reviewer's own
`git log -S`/orthogonality sampling to have covered — both are candidates
for the next round's sustainability territory, once they've accumulated a
commit history to measure.

---

## 5. What is good, said plainly

- **Zero critical findings, from six independent reviewers with no
  coordination between them.** The last full audit found one critical and
  three significant from the same class of review; this cycle is a real
  improvement, not a quieter one.
- **The claims ledger and doc-comment discipline are visibly load-bearing,
  not decorative.** The adherence reviewer sampled five `PHILOSOPHY.md`
  "Where the tree is today" notes against code and found all five accurate,
  including in the direction that would embarrass the project (understating
  a shipped feature) if it were wrong.
- **Two real consolidation passes landed and both are already being cited
  as the model to follow** — the shared `ConwayConfig` fixture and
  `AgentKnobs`. This is the sustainability lens's own stated goal (§8.10
  showing up as practice, not aspiration) actually happening.
- **The independent-review discipline this project already runs on every
  landed item is visibly catching things before they ship** — none of this
  cycle's ten minor findings are the kind of thing a build-lane test suite
  would ever catch; all ten are exactly what a second pair of eyes, reading
  for the right thing, is for.
- **The board is genuinely drained.** 60+ items surveyed this run; three
  remain open, and two of those three are explicitly the operator's alone
  (§0).

---

## 6. What this review did not check

- **The TUI was not driven under a real pty — the third consecutive review
  cycle without one.** The operator lens's own conduct rule (its §3) says
  two pty-less runs in a row should be treated as a process defect, not
  absorbed silently a third time. **Flagged here as exactly that: this
  review process currently has no way to drive the interactive surface,
  and everything §7a's daily-driver bar promises about that surface has now
  gone three cycles unverified live.** Worth fixing the process before the
  next round, not carrying the caveat forward a fourth time.
- **The root cause of F1's 90+-second wait was not isolated** — MCP
  startup cost, the 10k-token tool-registry segment's own prompt-eval cost,
  and multi-candidate fallback retries are all plausible contributors; the
  finding is the observed silence, not its full attribution.
- Several individual doc pages (`docs/getting-started.md`,
  `docs/embedding.md`, `docs/scripting.md`, most per-plugin pages) were not
  read end to end — the adherence reviewer relied on the claims ledger's
  coverage of the highest-risk assertions within them rather than
  re-deriving each page by hand.
- `conway-plugin-mcp`/`conway-plugin-subprocess`'s wire-protocol internals
  were checked structurally, not compared line-by-line for duplicated
  JSON-RPC-adjacent logic; `conway-plugin-names`, `-statusline`, and
  `-skeleton` were not examined for one-directional read/write pairs.
  `conway-session` and two of the three largest facade/runtime files
  (`conway/src/builder.rs`, `conway-runtime/src/context/builder.rs`) were
  not read for internal cost-of-change structure.
- Three of five `DESIGN-*.md` documents were not checked against shipped
  code this round (only `-permission-modes.md` and `-context-path.md`
  were, both already correctly maintained).
- The "programming by coincidence" hunt the evidence lens names was not
  run as its own pass; nothing surfaced incidentally.
- `conway plugin install`/`remove`'s write path was exercised only as
  `list`; the embedding surface and structured-output round-trip were read,
  not driven live.

---

## 7. Questions for you

Six Intent gaps came back from three reviewers. Each is drafted below as a
proposed amendment — the sentiment that would have settled the question,
worded to fold into `INTENT.md` where it belongs, per that page's own
instruction not to append dated notes. **None of these six has been applied
to `INTENT.md` by this run** — that is your call, not this review's.

**Q1 — Does §7a's parity rule ("anything a model can do to the session's
agents, the operator can do... with one typed command") extend to *any*
tool a model can call, or only to agent lifecycle specifically?**
Determines whether F10 (memory has no operator-typed equivalent) is a
direct §7a violation or outside that rule's scope. *Proposed addition, for
§7a:* "This parity rule is scoped to what a model can do to control the
session and its agents — fork, spawn, cancel, steer, await — not to every
tool a model can call. A tool whose effect an operator would want to audit
or reverse (writing to durable state the model can recall later, for
instance) is a separate, narrower question: does the operator have *some*
supervised way to see and undo what the model did, not necessarily the
identical one-command mirror this rule requires for agent control."

**Q2 — Does an internal module-doc disclosure (like `builder.rs`'s for F3)
satisfy §8.3's "says what it did," or must a gap of that shape also surface
in operator-facing docs?** *Proposed addition, for §8.3:* "A disclosure
that lives only in source — a doc comment, a code-level module note — is
honest to the next engineer, not to the operator reading `docs/`. §8.3's
'says what it did' means the operator-facing surface, not merely that a
truthful sentence exists somewhere in the tree. A gap disclosed in code but
silent in the docs an operator actually reads is not yet disclosed by this
rule's own standard."

**Q3 — Does a facade-only write path (reachable only by an embedder linking
`conway` directly, never by `conway-cli`) count as "shipped" for §8.5's
"ships with a consumer," given §7a makes the CLI the daily-driver bar?**
Determines whether F2 (session labels) and F11 (three ports with no
embedder path) are §8.5 violations or already-satisfied by their facade-
only reachability. *Proposed addition, for §8.5:* "A facade method with no
`conway-cli` caller is proven for an embedder, not for the daily-driver
bar §7a names — the two are different audiences and a capability can
legitimately satisfy one without the other. State which audience a given
capability is FOR when it ships facade-only on purpose, so a reviewer does
not have to guess whether the gap is a defect or a scope line."

**Q4 — §7c gives no acceptance bar for "done."** A working binding crate?
One real non-Rust caller? A published package? Worth settling before F13
becomes a board item with a size anyone can estimate. *No proposed text —
this is a scope/appetite call, not a sentiment to draft.*

**Q5 — Does `INTENT.md` say whether `HealthRegistry`/`ArtifactWriter`
belong in the "declined by design" bucket (like `SubagentHost`, which
`host_caps.rs` cites a ruling for) or the "unfilled seam" bucket?** The
tree currently treats them as the latter without saying so anywhere.
*Proposed addition, for §7:* every port without an embedder-supplied path
should carry, at its own definition site, either a cited ruling (like
`SubagentHost`'s) or an open acknowledgment that no ruling has been made —
never silence, which reads as an oversight either way.

**Q6 — Who owns periodically re-measuring which rung of the §7a/§7b
daily-driver ladder this tree is actually standing on?** Raised by the
operator reviewer after noting real dogfood history exists (80+ session
files) but no page states who re-checks the rung. *No proposed text — this
is a process-ownership question, not a sentiment for `INTENT.md` itself.*

---

**One more standing question, not from a reviewer's Intent gap but from
this run's own §6:** given three consecutive review cycles without a pty,
is fixing that a prerequisite for the next full review, or does the daily-
driver bar get verified some other way (a scripted expect-style driver, a
committed session recording) that doesn't require live operator time?
