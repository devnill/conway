# Board-sourced claim-check predicates

This file is data for `scripts/check-design-claims.py`, not a design record.
It replaced `.design/philosophy-debt.md` on 2026-08-13 (board items
`01KZYAHSXDXFDY9FX5MXPRZ4M1` and `01KZY8TEE2FDWQMHEKJDDC3SG9`).

**What changed, and what did not.** The `claim-check` predicate shape —
falsifiable `absent`/`present` patterns evaluated against the tree, exactly as
`check-design-claims.py`'s own module doc argues for — is unchanged and is the
genuinely valuable part; nothing about it was weakened. What moved is the
*narrative*: "what is claimed, what exists today, why" used to live in this
file's prose, entry by entry, renumbered every time one cleared. That prose now
lives on the board item each predicate names in `board_item:` — an open item's
spec for still-open debt, a done item's spec as provenance for a predicate kept
on as a regression guard. This file carries only the falsifiable fact and
enough of a `claim:` sentence to make a failure readable without the board open.

**Why predicates live here and not literally inside each board item's spec
text**, which is where the item retiring this file's predecessor asked for them
to go ("beside the existing VERIFICATION ANCHOR convention"). The board
(`.ideate-work/board.db`) is `.gitignore`d local tooling state — CI has no
access to it, committing it was declined, and a worker in this role has
read-only access to it by design (the claim/complete/release lifecycle belongs
to the orchestrating process, not the implementer). A predicate that only
existed inside a board item's `spec` column could not gate CI at all, which
would be a regression from the ledger this file replaces — it ran in CI.
Keeping the pattern itself in a git-tracked file and only the *citation* to the
board keeps both properties: CI still evaluates every predicate on every
change, and `board_item:` still makes each one someone's specific, findable,
closeable work (or, for a shipped one, its provenance). When board.db is
reachable (a maintainer checkout), the checker additionally resolves each
`board_item:` and reports its live title and status — read-only, matching
`scripts/check-board-citations.py`'s own precedent that this is practical —
and treats a `board_item:` that resolves to nothing as a hard error, the same
class of defect an unresolved `paths:` is.

**One predicate from the predecessor was dropped rather than migrated:** an
`absent: pub matcher` claim ("there is no tool-name matcher") that the
predecessor's own 2026-08-13 edit had already superseded with the `present:
match_tool` claim below when the matcher shipped, but had not removed. It held
(vacuously — the shipped field is spelled `match_tool`, not `matcher`) and
guarded nothing the surviving claim does not already guard, so carrying it
forward would have been dead weight, not migrated debt.

**`board_item: UNFILED`** is an explicit escape hatch for exactly one case: a
predicate whose debt is real (discovered while migrating this file's
predecessor) but which currently has no board item covering it, and this file
was moved by a worker without board-write access. It says so loudly — in
`--list` output, in a normal run's summary — rather than silently dropping the
predicate, because a claim quietly removed from tracking during a migration is
the exact failure class the predecessor's own contract existed to prevent
("a claim not in this list is expected to be true right now"). Filing the item
and swapping in its id is the intended, final state; it is not this file's job
to invent one.

---

<!-- claim-check
board_item: 01KZYAXSGDS8AP7YK1CN7H680G
claim: all seven core events dispatch -- request_assembled and child_reported were the last two, wired 2026-08-13
paths: crates/conway-runtime/src/hook_dispatch.rs
present: "(request_assembled|child_reported)"
-->

<!-- claim-check
board_item: 01KZYAWQ6011Q6CJVG6CCMQPF1
claim: hook rules carry a tool-name matcher, wire-spelled `match` per PHILOSOPHY.md §5
paths: crates/conway/src/config/schema.rs
present: match_tool
-->

<!-- claim-check
board_item: 01KZS019NHG11RVQYSVT7RG0P5
claim: the three observation-only events ARE dispatched -- post_tool_use, session_starting, child_spawned
paths: crates/conway-runtime/src/hook_dispatch.rs
present: pub const SESSION_STARTING
-->

<!-- claim-check
board_item: 01KZDC0RDRMMMJHX7SAFMM2Q5A
claim: no declaration anywhere re-asserts that only pre_tool_use dispatches -- a general regression guard over the whole declarative-hooks charter, not any one child
paths: crates/conway/src/config/schema.rs docs/plugins/scripts.md docs/plugins/hooks.md
absent: (nothing dispatches a rule|Every OTHER .event. value: still forward-declared|value other than .pre_tool_use. remains exactly)
-->

<!-- claim-check
board_item: 01KZDC0RDRMMMJHX7SAFMM2Q5A
claim: the config schema's own reachability contract names the dispatched events -- another whole-charter regression guard
paths: crates/conway/src/config/schema.rs
present: child_spawned.*DISPATCHED
-->

<!-- claim-check
board_item: 01KZS01ZBNEY12DBDNW2Y861SQ
claim: prompt_submitted IS dispatched, deny-capable and fail-closed
paths: crates/conway-runtime/src/hook_dispatch.rs
present: pub const PROMPT_SUBMITTED
-->

<!-- claim-check
board_item: 01KZS03BFE720EQZG7Q2768N2H
claim: plugin-declared events are wired -- validate_event_name has callers on BOTH sides, the operator-written subscriber side and the plugin declaration side
note: inverted from absent to present 2026-08-13 when the capability shipped. The absent form was written hours earlier during the ledger migration and was falsified by the very item it names; the checker caught it on the next run, which is the mechanism working. As a present guard it now protects the shipped capability against silent removal.
paths: crates/conway-runtime/src/hook_dispatch.rs
present: validate_event_name\(
-->

<!-- claim-check
board_item: 01KZS02HYXGTW42R8G4HP10GHX
claim: /settings lists deny-capable hook rules as a fourth revocable group, so revocation no longer means hand-editing the config file
note: inverted from absent to present 2026-08-13 when the capability shipped, same day and same reason as the sibling above. The bare [Hh]ook pattern was a coarse absence probe; as a present guard it is pinned to the specific revoke-action constant instead, which is what would actually disappear if the feature were removed.
paths: crates/conway-cli/src/tui/view/settings.rs
present: LEAF_REVOKE_HOOK_PREFIX
-->

<!-- claim-check
board_item: 01KZS00JP5QNBJSSHNFP9C47GM
claim: pre_tool_use IS dispatched -- the shipped half, which must not silently regress
paths: crates/conway/src/builder.rs
present: rule\.event == "pre_tool_use"
-->

<!-- claim-check
board_item: 01KZYM81YFE08ASM225A1R5H5X
note: filed 2026-08-13, closing the one UNFILED gap this migration surfaced. PHILOSOPHY.md §5 lists "dynamic routing, context compaction, memory, skills, MCP support" as the first-party tier's members ("You get them by choosing them"), and §6 goes further, stating in the present tense that "there is a first-party compaction plugin to install or fork." Only routing and the provider adapters exist. The migration worker had no board-write access, marked this UNFILED rather than silently dropping real debt, and surfaced it on every run -- which is how it got filed within the hour. The item covers the §6 wording decision and the decomposition into one item per surviving capability; it is deliberately NOT a charter that builds all four, which would recreate the defect 01KZVZ6XCZVHD2YFVJQEGC61YV exists to fix.
claim: four of the five named first-party-plugin-tier capabilities -- compaction, memory, skills, MCP -- are unbuilt, so nothing installs one
paths: crates/conway-cli/src crates/conway/src
absent: conway\.(compaction|memory|skills|mcp)
-->

<!-- claim-check
board_item: 01KZDC30CBY9CPJ8YEM7HSRV0Y
claim: confinement is still harness-level -- PathArgs and the broker's pre-gate root check have not retired
paths: crates/conway-core/src/ports
present: PathArgs
-->

<!-- claim-check
board_item: 01KZDC30CBY9CPJ8YEM7HSRV0Y
claim: CanonicalRoot still lives in conway-core, which is also why that crate still does I/O
paths: crates/conway-core/src/containment.rs
present: canonicalize\(\)
-->

<!-- claim-check
board_item: 01KZDC0269171BZDB3HH00179B
claim: conway.fs does not yet enforce a root of its own
paths: crates/conway-tools/src/fs
absent: (AgentRoot|CanonicalRoot)
-->

<!-- claim-check
board_item: 01KZWT5NE4VGW84HSCN3FV24S7
note: regression guard added 2026-08-13 by 01KZY8TEE2FDWQMHEKJDDC3SG9, extending checker coverage to the leaked class this item fixed by hand. The whitepaper used to assert capability-based candidate filtering and health-aware failover as present-tense default-build fact; a default build has neither (MinimalRouter/AlwaysClosedHealthRegistry). This predicate pins the honest disclosure clause the fix added -- if it disappears while the surrounding claims of filtering/failover remain, the false-present-tense defect is back.
claim: routing §4.3 states capability filtering and health-aware failover as something conway.routing adds, not a default-build fact
paths: docs/whitepaper.md
present: does \*\*not\*\* do is filter candidates
-->

<!-- claim-check
board_item: 01KZVZ6XCZVHD2YFVJQEGC61YV
note: regression guard added 2026-08-13, pinning the exact defect phrasing this item's fix removed from docs/plugins/hooks.md -- four rows cited the declarative-hooks charter (01KZDC0RDRMMMJHX7SAFMM2Q5A) for work outside its nine children (Plugin::rules()/PatternOrigin::Plugin producer). Does not catch a NEW mis-citation phrased differently; see check-board-citations.py's own disclosed limits for the general class.
claim: point 7 (Plugin-contributed permission rules) no longer cites the hooks charter as its umbrella tracker
paths: docs/plugins/hooks.md
absent: Tracked under the same umbrella as the declarative `hooks` surface, `01KZDC0RDRMMMJHX7SAFMM2Q5A`
-->

<!-- claim-check
board_item: 01KZVZ6XCZVHD2YFVJQEGC61YV
note: sibling regression guard, point 8 (composed inference-evaluated policy).
claim: point 8 no longer calls the hooks charter "the closest tracked work"
paths: docs/plugins/hooks.md
absent: is the closest tracked work and is the umbrella this document cites for it
-->

<!-- claim-check
board_item: 01KZVZ6XCZVHD2YFVJQEGC61YV
note: sibling regression guard, point 10 (tool-hide selector).
claim: point 10 no longer cites the hooks charter for a Plugin tool-hide selector
paths: docs/plugins/hooks.md
absent: tracked under `01KZDC0RDRMMMJHX7SAFMM2Q5A` alongside the rest of the generalized point vocabulary
-->

<!-- claim-check
board_item: 01KZVZ6XCZVHD2YFVJQEGC61YV
note: sibling regression guard, point 11 (plugin subscription to observe/1). Narrower than the other three guards on purpose -- point 13's Status row legitimately says "remain tracked under the umbrella `01KZDC0R...`", which this pattern does not match (no "the umbrella" between "under" and the id).
claim: point 11 no longer bare-cites the hooks charter for plugin event subscription
paths: docs/plugins/hooks.md
absent: Tracked under `01KZDC0RDRMMMJHX7SAFMM2Q5A`
-->
