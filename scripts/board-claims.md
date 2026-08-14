# Claim-check predicates

Data for `scripts/check-design-claims.py`, not a design record.

**What this is.** `PHILOSOPHY.md` is written in the present tense, including
for a few things the tree does not do yet (`CONTRIBUTING.md` §2). That is only
safe if a claim which stops being true — in either direction — fails
something. Each block below declares a falsifiable predicate: an `absent`
pattern that must match nothing, or a `present` pattern that must match
something. CI evaluates every one on every change.

**The failure that motivates it.** Two documents describing the same hook work
diverged: `docs/plugins/hooks.md` stayed exact while a prose ledger of the same
claims went stale on its highest-traffic entry, in the *understating*
direction, telling readers a shipped capability was unbuilt. The difference was
enforcement, not care.

**Each block carries four fields.** `why` says what the predicate is protecting
and is the sentence a failure prints — write it for whoever hits the failure,
not for whoever wrote the claim. `claim` states the fact in one line. `paths`
scopes the grep. `absent` or `present` is the pattern itself.

**Two kinds of block live here, and they read differently.** An `absent`
predicate over an unbuilt capability fails the moment that capability ships —
which is the point: it is the alarm that says a `PHILOSOPHY.md` "Where the tree
is today" note has become false and must be deleted in the same change. A
`present` predicate is a plain regression guard over something that already
works.

**What it does not catch** is stated in the checker's own module doc, because a
gate whose blind spots are unstated is the same defect one level up.

---

<!-- claim-check
why: regression guard: these two events shipped last, and a refactor that silently unwires one would make PHILOSOPHY.md §5's open-event-vocabulary claim false again
claim: all seven core events dispatch -- request_assembled and child_reported were the last two, wired 2026-08-13
paths: crates/conway-runtime/src/hook_dispatch.rs
present: "(request_assembled|child_reported)"
-->

<!-- claim-check
why: PHILOSOPHY.md §5's canonical example (run the formatter after a write) is unusable without a matcher, so this field is what makes declarative hooks worth having
claim: hook rules carry a tool-name matcher, wire-spelled `match` per PHILOSOPHY.md §5
paths: crates/conway/src/config/schema.rs
present: match_tool
-->

<!-- claim-check
why: regression guard on the observation tier: these three fail open, so an unwiring would be silent by design
claim: the three observation-only events ARE dispatched -- post_tool_use, session_starting, child_spawned
paths: crates/conway-runtime/src/hook_dispatch.rs
present: pub const SESSION_STARTING
-->

<!-- claim-check
why: whole-charter regression guard: the tree used to say only pre_tool_use dispatched, and that sentence outlived the code twice
claim: no declaration anywhere re-asserts that only pre_tool_use dispatches -- a general regression guard over the whole declarative-hooks charter, not any one child
paths: crates/conway/src/config/schema.rs docs/plugins/scripts.md docs/plugins/hooks.md
absent: (nothing dispatches a rule|Every OTHER .event. value: still forward-declared|value other than .pre_tool_use. remains exactly)
-->

<!-- claim-check
why: whole-charter regression guard on the other side -- the schema doc is where an operator looks to learn which events are real
claim: the config schema's own reachability contract names the dispatched events -- another whole-charter regression guard
paths: crates/conway/src/config/schema.rs
present: child_spawned.*DISPATCHED
-->

<!-- claim-check
why: prompt_submitted is the one event that may deny but may never modify; a regression that let it rewrite the user's words would be the worst kind, so its dispatch is pinned
claim: prompt_submitted IS dispatched, deny-capable and fail-closed
paths: crates/conway-runtime/src/hook_dispatch.rs
present: pub const PROMPT_SUBMITTED
-->

<!-- claim-check
why: PHILOSOPHY.md §5 says the event vocabulary is open to plugins; an unwired declaration side would make that decorative
claim: plugin-declared events are wired -- validate_event_name has callers on BOTH sides, the operator-written subscriber side and the plugin declaration side
note: inverted from absent to present 2026-08-13 when the capability shipped. The absent form was written hours earlier during the ledger migration and was falsified by the very item it names; the checker caught it on the next run, which is the mechanism working. As a present guard it now protects the shipped capability against silent removal.
paths: crates/conway-runtime/src/hook_dispatch.rs
present: validate_event_name\(
-->

<!-- claim-check
why: a deny-capable hook is security-bearing, and PHILOSOPHY.md §5 requires it be individually revocable -- hand-editing a config file does not count
claim: /settings lists deny-capable hook rules as a fourth revocable group, so revocation no longer means hand-editing the config file
note: inverted from absent to present 2026-08-13 when the capability shipped, same day and same reason as the sibling above. The bare [Hh]ook pattern was a coarse absence probe; as a present guard it is pinned to the specific revoke-action constant instead, which is what would actually disappear if the feature were removed.
paths: crates/conway-cli/src/tui/view/settings.rs
present: LEAF_REVOKE_HOOK_PREFIX
-->

<!-- claim-check
why: the shipped half of declarative hooks, and the one whose regression would be quietest
claim: pre_tool_use IS dispatched -- the shipped half, which must not silently regress
paths: crates/conway/src/builder.rs
present: rule\.event == "pre_tool_use"
-->

<!-- claim-check
why: PHILOSOPHY.md §5 names five first-party plugin capabilities; four are unwritten, and this predicate fails the moment one lands so the page's Where-the-tree-is-today note gets updated with it
note: PHILOSOPHY.md §5 lists "dynamic routing, context compaction, memory, skills, MCP support" as the first-party tier's members ("You get them by choosing them"), and §6 goes further, stating in the present tense that "there is a first-party compaction plugin to install or fork." Only routing and the provider adapters exist. The migration worker had no board-write access, marked this UNFILED rather than silently dropping real debt, and surfaced it on every run -- which is how it got filed within the hour. The item covers the §6 wording decision and the decomposition into one item per surviving capability; it is deliberately NOT a charter that builds all four, which would recreate the defect 01KZVZ6XCZVHD2YFVJQEGC61YV exists to fix.
claim: four of the five named first-party-plugin-tier capabilities -- compaction, memory, skills, MCP -- are unbuilt, so nothing installs one
paths: crates/conway-cli/src crates/conway/src
absent: conway\.(compaction|memory|skills|mcp)
-->

<!-- claim-check
why: PHILOSOPHY.md §1 specifies confinement as a property of conway.fs; while it is harness-level the page carries a Where-the-tree-is-today note, and this predicate is what makes removing that note mandatory rather than optional
claim: confinement is still harness-level -- PathArgs and the broker's pre-gate root check have not retired
paths: crates/conway-core/src/ports
present: PathArgs
-->

<!-- claim-check
why: the same move retires conway-core's one I/O exception and architecture invariant T2 -- see containment.rs's own module doc for the four questions it has to answer first
claim: CanonicalRoot still lives in conway-core, which is also why that crate still does I/O
paths: crates/conway-core/src/containment.rs
present: canonicalize\(\)
-->

<!-- claim-check
why: the other half of the same gap: the tool has to do its own checking for the guarantee to be exact
claim: conway.fs does not yet enforce a root of its own
paths: crates/conway-tools/src/fs
absent: (AgentRoot|CanonicalRoot)
-->

<!-- claim-check
why: capability filtering and health failover are what installing conway.routing buys; a default build has neither, and the whitepaper said otherwise once
note: regression guard extending checker coverage to the leaked class it fixed by hand. The whitepaper used to assert capability-based candidate filtering and health-aware failover as present-tense default-build fact; a default build has neither (MinimalRouter/AlwaysClosedHealthRegistry). This predicate pins the honest disclosure clause the fix added -- if it disappears while the surrounding claims of filtering/failover remain, the false-present-tense defect is back.
claim: routing §4.3 states capability filtering and health-aware failover as something conway.routing adds, not a default-build fact
paths: docs/whitepaper.md
present: does \*\*not\*\* do is filter candidates
-->

<!-- claim-check
why: de-citation ratchet: this citation was removed once and must not come back
note: regression guard pinning the exact defect phrasing removed from docs/plugins/hooks.md -- four rows cited the declarative-hooks charter (01KZDC0RDRMMMJHX7SAFMM2Q5A) for work outside its nine children (Plugin::rules()/PatternOrigin::Plugin producer). Does not catch a NEW mis-citation phrased differently; see check-board-citations.py's own disclosed limits for the general class.
claim: point 7 (Plugin-contributed permission rules) no longer cites the hooks charter as its umbrella tracker
paths: docs/plugins/hooks.md
absent: Tracked under the same umbrella as the declarative `hooks` surface, `01KZDC0RDRMMMJHX7SAFMM2Q5A`
-->

<!-- claim-check
why: de-citation ratchet: this citation was removed once and must not come back
note: sibling regression guard, point 8 (composed inference-evaluated policy).
claim: point 8 no longer calls the hooks charter "the closest tracked work"
paths: docs/plugins/hooks.md
absent: is the closest tracked work and is the umbrella this document cites for it
-->

<!-- claim-check
why: de-citation ratchet: this citation was removed once and must not come back
note: sibling regression guard, point 10 (tool-hide selector).
claim: point 10 no longer cites the hooks charter for a Plugin tool-hide selector
paths: docs/plugins/hooks.md
absent: tracked under `01KZDC0RDRMMMJHX7SAFMM2Q5A` alongside the rest of the generalized point vocabulary
-->

<!-- claim-check
why: de-citation ratchet: this citation was removed once and must not come back
note: sibling regression guard, point 11 (plugin subscription to observe/1). Narrower than the other three guards on purpose -- point 13's Status row legitimately says "remain tracked under the umbrella `01KZDC0R...`", which this pattern does not match (no "the umbrella" between "under" and the id).
claim: point 11 no longer bare-cites the hooks charter for plugin event subscription
paths: docs/plugins/hooks.md
absent: Tracked under `01KZDC0RDRMMMJHX7SAFMM2Q5A`
-->
