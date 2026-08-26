# State of the Union: conway

**Reviewed 2026-08-26 against the working tree at `bc2a174`, version 0.9.0.**

> Written for the operator. It assumes you care about the shape of the system
> and not about the shape of any particular trait. Everything in it was
> checked against the code in this run; where you might want to check
> something yourself, the file and line is given.
>
> Snapshot document — replaced wholesale on the next run of
> [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md).
>
> **Note on this run — narrower than the process it followed, and said
> plainly.** `REVIEW-PROMPT.md` §2 calls for a lead dispatching 3–9 parallel
> reviewers with named territories. This run had no fan-out: one agent did
> the Step 1 measurement, the Step 4 board survey, and re-verified the prior
> snapshot's six findings and this cycle's six audit findings by direct
> citation. It did **not** re-derive an independent review of the whole
> tree. Where the process asks a reviewer to hunt (sustainability duplication,
> a live pty session, the security-bearing operator pages), this run instead
> **carries forward the just-completed full audit's verdict** rather than
> risk contradicting it with a second, weaker pass over the same ground. See
> §6 for exactly what that leaves uncovered.

---

## 0. The verdict, in three sentences

**The previous snapshot's own diagnosis had become true of the snapshot
itself.** It reported the tree shipping capability faster than its record
admits, then sat unregenerated through 138 commits, 256 files, and 27
completed board items — during which its six open findings were all resolved
and its fourteen-item plan was completed in full. **A full audit has just run
against the current tree and returned NEEDS-REFINEMENT** (1 critical, 3
significant, 2 minor — cycle-summary `01M0XQRS96KZNNS5CPKJPMBE4N`), and every
one of those six findings is the same disease in a new instance: a same-day
sibling change updated one declaration of a mechanism and left another one,
elsewhere, still describing the old world. **The board is not drained — it
carries 18 live items** (12 open, 6 in progress): six from today's audit,
seven from two prior waves building plugin-declared permission modes and
plugin-to-plugin capability sharing, and one carried over from an inherited
chain-completion defect (§3).

The last snapshot's F1–F6 are gone — verified below, not assumed. The
document that diagnosed staleness is, this time, current with what it
describes.

---

## 1. What is built, as blocks

Unchanged in shape from the last snapshot; the plugin tier has grown.

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
                    │   THE PLUGIN TIER    │   sixteen crates, all optional
                    └──────────────────────┘

  in-process, compiled in:    routing · backends* · history · stepguard ·
                              skills · memory · skeleton · path · discover ·
                              trim · names · claude · idiom · marketplace
  OUT of process, no rebuild: subprocess-host · mcp-client
                                           (* the only one on by default)
```

**Sixteen plugin crates exist on disk today** (`ls crates | grep plugin`);
`ARCHITECTURE.md` §2b enumerates thirteen. The three missing —
`conway-plugin-claude` (reads an on-disk Claude Code plugin directory,
board `01M0VR89FB1F3Q4FQ8852K2A5E`), `conway-plugin-idiom` (the
system-prompt-fragment plugin, board `01M0VR3BKW5N3V3WS28H7FV8ZK`), and
`conway-plugin-marketplace` (fetches and installs from a Claude Code
marketplace, board `01M0VR96Y87FF2BVNTBSC6GEYR`) — all landed in the same
138-commit window this snapshot catches up on. **This is a new instance of
exactly the pattern §3 documents**, found by this run's own verification of
§1's block diagram rather than by the full audit (whose territories did not
include re-counting the plugin tier). It has no board item yet; flagged for
the next refine cycle rather than filed here, since filing is outside this
item's scope.

Everything else in §1 holds as last reported: plugin uninstallability is
real and enforced by `[plugins].install`'s strict resolution
(`crates/conway-cli/src/first_party_plugins.rs:64-78`); the sixteen port
modules under `crates/conway-core/src/ports/` each have a real out-of-core
implementation; the context-path machinery (`conway.path`) has its
production caller.

**What changed since the last snapshot, structurally.** Two new mechanisms
landed and are mid-build, not finished: **Claude Code compatibility**
(reading a Claude-format plugin directory, translating its hooks and
commands, dispatching hooks for real) and **plugin-to-plugin dependency
edges** (`requires`/`optional` on the manifest, an open host-capability
vocabulary, and an as-yet-nonexistent plugin→plugin call channel, "Edge B").
Both are why the board carries more than the six audit findings — see §3
and §7.

---

## 2. Scored against [`INTENT.md`](INTENT.md)

The six questions the last snapshot put to the operator (§7, 2026-08-24) were
answered the same day and are folded into `INTENT.md` §7/§7a/§7b/§7c/§8.10.
Re-verified this run, each still holds:

- **§7 core-owned subagent host.** `crates/conway/src/host_caps.rs:68-80`
  now cites the ruling in prose — *"fork and spawn are mechanism with
  exactly one implementation… INTENT.md §7."* No `with_subagent_host` exists
  in `builder.rs`, on purpose, stated.
- **§7a/§7b daily-driver ladder, rung one.** Still standing; not re-driven
  live this run (§6).
- **§7c non-Rust embedding.** `DESIGN-bindings.md` still the acceptable
  first step; unstarted beyond it, as scoped.
- **§8.10 cost of change.** `crates/conway/src/error.rs:29` is now
  `FacadeError`, not a second `ConwayError` — F6 closed, verified below.

**What INTENT does not yet answer, surfaced by the two programs now on the
board:** whether an out-of-process (subprocess-host) plugin gets Edge B's
plugin→plugin channel on the same terms as an in-process one
(`01M0WWNHQQYN1EVTH8WPZ33EBF`'s own acceptance asks this and does not
assume yes), and where the host/toolkit altitude boundary sits for
`conway.ui` (`01M0WWM0ZB6BR45XJ8HMTJWZ0Z`, an explicit operator-ruling item
already filed, not a gap this snapshot is raising new). Both are already on
the board as the right kind of question — an item, not a silent assumption.

---

## 3. Findings

**Carried forward from cycle-summary `01M0XQRS96KZNNS5CPKJPMBE4N` (full
audit, boundary `7654041..bc2a174`), not re-derived.** Re-stating this
cycle's verdict is deliberate: a second, single-agent pass over the same
138 commits with a fraction of the audit's reviewer budget would be weaker
evidence, and a snapshot that quietly produced a different answer would be
exactly the drift this page exists to stop.

**CRITICAL**

**F1 — Translated Claude Code commands are unreachable, and the docs say
otherwise.** `command_registrations()` is never called anywhere in
`conway-cli`; typing a translated command does nothing. `docs/plugins/
claude-compat.md:49` calls them "wired" under a header claiming what does
NOT run. Board `01M0XRCAFD7DD7N64RNRM3P8W9`, in progress.

**SIGNIFICANT**

**F2 — One of conway's two deny-capable events is invisible to the compat
layer.** `DENY_CAPABLE_EVENT` hardcodes `"pre_tool_use"`; a translated
`UserPromptSubmit` hook can deny every prompt an operator types and a
default run prints nothing about it. Board `01M0XRD8VMWD273W0W51T8ECCM`,
open — bundled with the second half of this finding, the `/plugin` browser
still calling live hooks "(not wired)".

**F3 — The multi-root skills loader has no caller and no config surface.**
Built and tested; its agents-loader twin gained an operator-settable field
and this one did not. Board `01M0XRE2N96ATHEXJ1617E133P`, in progress.

**F4 — (folded into F2 above in the board's own filing.)** The `/plugin`
browser understates a live permission boundary by calling wired hooks
"not wired."

**MINOR**

**F5 — The `on_failure: Prompt` outage guarantee has no two-hook test.**
Board `01M0XREWGA03EDQ5PK2C18KW75`, open (bundled with F6).

**F6 — `EventDecl::summary`'s stated discovery purpose has no CLI
consumer.** Pre-existing, not new. Same board item as F5.

**The pattern above the findings.** All six are declaration-honesty defects —
a written claim about what a mechanism does or does not do going stale
against the code beside it — the fifth consecutive cycle that class has
dominated — and three
of six were falsified by a *same-day* sibling change, not weeks of drift.
Eight-wide parallel waves compress into one day what used to take a
fortnight; an ownership fence that makes parallelism conflict-free is the
same mechanism that stops a writer sweeping the sibling declarations its own
change just falsified. Recorded in full as process finding
`01M0XDG8VSTSY1BEGCSERW57WM` (the "chain pattern," four instances as of
that record, a fifth added same day — the skills loader above).

**F1–F6 from the 2026-08-24 snapshot are gone,** re-verified fresh this run
rather than assumed carried-over:

- The three "no production caller" doc comments now name `conway.path`'s
  `compose_context_path` as the caller
  (`crates/conway-core/src/path.rs:120-128`,
  `crates/conway-core/src/log.rs:363-370`,
  `crates/conway-runtime/src/runtime.rs:165-175`).
- `/cancel` exists (`crates/conway-cli/src/tui/commands.rs:365,550`), tested
  (`:2612,3813`).
- `docs/scripting.md:154-157` and `docs/sessions.md:216-217` both now state
  in prose that `transcript_ref`, not `agent_id`, is the resume handle.
- `DESIGN-bindings.md` is the survey INTENT §7c asks for (already existed
  at the last review, was linked from nothing then; now indexed in
  `README.md`).
- `crates/conway/src/host_caps.rs:68-80` cites the operator's ruling instead
  of describing an unexplained absence.
- `crates/conway/src/error.rs:29` is `FacadeError`; only one `ConwayError`
  remains, in `conway-core`.

Positives verified in passing: the `kill_group` and `canonical_json_bytes`
consolidations from earlier cycles still hold; the CON-1/CON-2 subprocess
timeout-and-lifecycle consolidation (§4) also verified in place this run —
both plugin crates now implement a shared `ChildSessionError` trait rather
than hand-rolling four failure causes each
(`crates/conway-plugin-mcp/src/lib.rs:183-190`); the CON-4 test-support
tier exists (`crates/conway/src/test_support.rs`,
`crates/conway-cli/src/tui/test_support.rs`) and `build_conway` is down to
3 hand-rolled copies from the 46 the last review counted.

---

## 4. Sustainability: where the tree is getting more expensive to change

**Not independently re-reviewed this run** — no sustainability-lens
territory pass was dispatched (§6). What follows is verification that last
cycle's two named debts are closed, not a fresh hunt for new ones.

**The subprocess twins — closed.** `conway-plugin-mcp` and
`conway-plugin-subprocess` now consume a shared `ChildSession`/
`ChildSessionError` lifecycle layer (board `01M0TV7ZDS8X4F4TEJPRZB9P6T`,
verified above); `DEFAULT_TIMEOUT_MS` is declared once, in
`conway::plugin`, and re-exported (`01M0TV6E2K6QF9VXP6C7TFH06X`). The wire
protocols correctly stayed separate.

**The test harness's missing tier — closed.** A facade-level test-support
module exists and the hand-rolled copies of `build_conway` collapsed from
46 to 3 (`01M0TV8MSFRHHQ5BNZV3NHZCEW`).

**New surface not yet assessed for this concern:** the Claude-compat and
plugin-dependency mechanisms landed in the same window (§1) have not had a
sustainability pass — whether `conway-plugin-claude`, `-idiom`, and
`-marketplace` duplicate policy with each other or with the native plugin
path is an open question for the next full audit, not answered here.

---

## 5. What is good, said plainly

- **The honesty gates keep winning.** Every drift this cycle's audit found
  lives in prose the gates do not read (doc comments, enumerations, a
  hardcoded constant's doc claim) — none of it is a gate regression.
- **The chain-pattern is being tracked as a named process defect, not
  re-discovered from scratch each time** (`01M0XDG8VSTSY1BEGCSERW57WM`), and
  two of today's writers found the fix (drive-the-real-binary tests the
  build lane executes) without being told to.
- **Two prior cycles' plans both completed in full** — REC/OP/EMB/CON (14
  items, this snapshot's own trigger) and the 27 items surveyed via the
  board's done list. Nothing rotted silently; it rotted in the *record*,
  which this regeneration exists to fix.
- **The subprocess and test-harness consolidations held** — verified fresh,
  not assumed (§3, §4).

---

## 6. What this review did not check

**Larger than usual, and said plainly because of it — this was a
single-agent regeneration, not a fan-out.**

- **No independent reviewer territory was run this cycle.** Adherence,
  surfaces, operator, evidence, and sustainability×N were not separately
  dispatched; this page instead re-verified the full audit's own six
  findings and the prior snapshot's six findings by direct citation.
- **The TUI was not driven under a real pty this run** (last driven
  2026-08-24; not repeated here).
- **The full workspace test suite was not re-run in this pass** — this
  worktree's lane runs no cargo. The reported state, carried from the
  orchestrating context rather than re-derived: 244 suites, 3,556 passed, 0
  failed, all 6 fast gates green, at `bc2a174`.
- **Security-bearing operator pages** (`docs/permissions.md`,
  `docs/tools.md`) — still unverified against code, as last cycle.
- **The plugin-tier crate count discrepancy (§1) was found incidentally**,
  by re-drawing the block diagram, not by a targeted sweep — there may be
  other enumeration pages with the same gap (`PHILOSOPHY.md` §6,
  `docs/plugins/README.md`) that this run did not check line-by-line.
- **Sustainability beyond the two named debts** (§4) — no fresh hunt for
  duplication in the Claude-compat or plugin-dependency mechanisms.

---

## 7. Questions for you

1. **The plugin-tier enumeration gap (§1) — worth a board item now, or
   folded into the next refine cycle's own sweep?** This snapshot did not
   file one; it is real and cited.
2. **Edge B's parity question** (`01M0WWNHQQYN1EVTH8WPZ33EBF`'s own
   acceptance criterion) **and the host/toolkit altitude ruling**
   (`01M0WWM0ZB6BR45XJ8HMTJWZ0Z`) are both already filed as the operator
   rulings they are — not new, listed here only so this page's own claim
   that the board is current is checkable against it.
3. **Is a sustainability-lens pass over the Claude-compat + plugin-dependency
   surface worth dispatching before or after the current 18-item board
   clears?** Both landed fast, in the same window a duplication would be
   easiest to introduce and hardest to see yet (§4).
