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
why: PHILOSOPHY.md §6 names a first-party compaction plugin as a thing you would install; this predicate fails the moment one lands so that note gets updated in the same change
note: narrowed 2026-08-20 from the five-capability claim this block used to be. That wider claim ("four of five unbuilt") went stale when memory and skills shipped as installable plugins (`conway_plugin_skills::SkillsPlugin`/`conway_plugin_memory::MemoryPlugin` in `first_party_plugins.rs`) and MCP client support shipped as `[plugins].mcp` (`conway_plugin_mcp`, wired by `mcp_plugins.rs`) -- the checker caught it on the next run, which is the mechanism working, not a defect. Compaction alone remains genuinely unwritten (`PHILOSOPHY.md` §6's own "Where the tree is today" note still says so); the sibling present-guard immediately below pins the three that shipped against silent regression.
claim: compaction is the one first-party-plugin-tier capability still unbuilt, so nothing installs conway.compaction
paths: crates/conway-cli/src crates/conway/src
absent: conway\.compaction
-->

<!-- claim-check
why: regression guard: memory, skills, and MCP client support shipped after the claim above went stale on them; an unwiring of any one would make PHILOSOPHY.md's first-party-tier note false again in the same understating direction that made this item necessary
note: inverted from the absent claim above on 2026-08-20 when the checker's own STALE report showed memory/skills/MCP matching an absent pattern -- the same shape as the two 2026-08-13 inversions elsewhere in this file (search this file for "inverted from absent to present"). Pinned to the actual call sites (`SkillsPlugin::from_dir`, `MemoryPlugin::new`, `McpPlugin::discover`) rather than a comment or an id string, so removing the wiring -- not merely renaming a doc comment -- is what trips this.
claim: conway.skills, conway.memory, and the MCP client are all built and installed through first_party_plugins.rs/mcp_plugins.rs, not merely named as intent
paths: crates/conway-cli/src
present: conway_plugin_skills::SkillsPlugin::from_dir|conway_plugin_memory::MemoryPlugin::new|McpPlugin::discover
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
why: the other half of the same gap: the tool has to do its own checking for the guarantee to be exact. This flipped from absent to present when per-agent plugin config landed and conway.fs began reading its own root -- which is precisely the precondition the sibling retirement item was blocked on, so the predicate now guards that the enforcement does not silently disappear again while the harness-level root is being retired.
note: the ONLY consumer of CanonicalRoot inside conway.fs must remain a real check on the I/O paths, not an unused import. `check_root` is called by read, write and cd before their I/O runs; if it stops being called this predicate still matches on the import alone, which is this predicate's known limit -- the behavioural guarantee is pinned by crates/conway/tests/per_agent_plugin_config.rs, not here.
claim: conway.fs enforces a root of its own, using the same symlink-aware CanonicalRoot the harness-level check uses
paths: crates/conway-tools/src/fs
present: CanonicalRoot
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

<!-- claim-check
why: INTENT.md §7 nominated this claim for the ledger by name and it was never added; bare_inference.rs is the runnable proof of it, so if this predicate disappears the claim reverts to unverified prose with nothing catching a silent regression
note: added 2026-08-21 (board item 01M0EMEQJHPR3XVNAN39YX7C38), landing after 01M0EM97X118CZ43CGEPH2PB8F's same-day narrowing of the block above. Pinned to the `builtin_plugins: Vec::new()` struct literal in `bare_inference_config()` rather than the filename or the doc comment's `vec![]` paraphrase: a file can be renamed while the capability survives (paths scans the whole examples directory, not one filename), and a file can survive while its body is gutted, so the pattern is the exact call that empties `tools.builtin_plugins` and makes `ConwayBuilder::build`'s plugin-selection step match nothing -- the one line that, if deleted, means the example no longer configures conway down to zero tools at all.
claim: conway can be configured down to a bare inference call using only mechanisms a third party also has -- proven by crates/conway/examples/bare_inference.rs, which reaches no tools, no agent behaviour, one turn out, using only ConwayConfig fields and ConwayBuilder methods
paths: crates/conway/examples
present: builtin_plugins: Vec::new\(\)
-->

<!-- claim-check
why: decision 01M0K4S2S1NBW63KNF1NEY5XT3's obligation ("an instruction may only name a capability that is actually reachable") is exactly the shape this project prefers to express as a mechanical predicate rather than a paragraph nobody re-checks -- this pins that a plugin-declared instruction fragment naming an unreachable tool is WITHHELD from the assembled context, not merely logged about, so the model can never read an instruction assuming a tool it cannot call
note: added with board item 01M0K5MD59YZRSHE31JKZKFRMY (Plugin::instructions()). Pinned to the exact withholding call site in ContextBuilder::build, not to the struct field alone, so a refactor that keeps recording the omission but stops excluding the segment would still fail this check.
claim: an instruction fragment naming a tool this turn's assembled tool set does not provide never becomes a Role::System segment -- it is withheld and recorded, checked at context assembly (per turn), not in CI
paths: crates/conway-runtime/src/context/builder.rs
present: if unreachable_tool_ids.is_empty\(\)
-->

<!-- claim-check
why: regression guard: the wiring that makes EventSink reachable has been unwired-by-prose once already, and if it disappears again the corrected claim in crates/conway/src/lib.rs's module doc -- which now names Plugin::observe_sink as the injection point -- silently overstates what a plugin author can reach
note: added 2026-08-24 (board item 01M0V52D30PQF2C6BC9NN1CG1B). The history it guards against: lib.rs once grouped EventSink/EventSinkHandle with SubagentHost as sharing "no builder injection point at all" -- true when written (3cb3068, 2026-08-10), false six days later when Plugin::observe_sink shipped (94299c4, 2026-08-16), unrevisited for two months until 01M0TW809NJT8P4G111N046CGH found it. Pinned to the actual call site that collects each installed plugin's observe_sink() handle in ConwayBuilder::build (crates/conway/src/builder.rs:1567-1618, forwarded to per-sink tokio tasks), not to lib.rs's doc-comment prose, so a refactor that drops the wiring trips it rather than a rewording leaving it green. Consequence worth stating: this watches the wiring, not the sentence -- reintroducing a false absence claim in lib.rs while the wiring stands trips nothing.
claim: EventSink/EventSinkHandle DO have a real production builder injection point -- Plugin::observe_sink(), collected and forwarded to per-sink tasks in ConwayBuilder::build
paths: crates/conway/src/builder.rs
present: p\.observe_sink\(\)
-->

<!-- claim-check
why: board item 01M0XVAMA0N0TH8CX324EC9593 -- this enumeration went stale by hand twice (nine of twelve, then twelve of sixteen), and each fix corrected the prose without adding a check, so the next crate landed unmentioned again. A `glob` predicate re-derives the crate set from the filesystem on every run instead of trusting the last person who counted, so it is the one shape of this check that does not need editing when crate seventeen lands
claim: every crates/conway-plugin-* directory is named somewhere in ARCHITECTURE.md
paths: ARCHITECTURE.md
glob: crates/conway-plugin-*
-->

<!-- claim-check
why: docs/plugins/hooks.md point 14 and docs/plugins/inference-hooks.md both now say an inference-evaluated hook modality was abandoned for want of a consumer, not merely unbuilt; if `subagent_mode`/`hook.fork` ever land anyway, both pages' abandonment framing (and DESIGN-permission-modes.md's §8 entry) need re-reading before anyone trusts them, and nothing else here would notice the seam landing
note: added 2026-08-27 (board item 01M128C6X9SBJNP2DBF5V8JRMF), alongside the abandonment of conway.permissions (decision record 01M128AP39WXE01BBZV4RENC4M). Deliberately NOT pinned to `Plugin::hooks()` itself: that registration method is being built in parallel, for an unrelated shipped consumer (claude-compat, board item 01M129QW0GV90QTQS6B3BY3DAR), so a predicate over `hooks()`'s existence would fail for a reason having nothing to do with this claim. Pinned instead to the two fields specific to an inference-evaluated hook, which that item does not touch.
claim: no inference-evaluated hook modality exists in the tree -- no subagent_mode field and no hook.fork capability anywhere in HookEntry or conway_core::hook
paths: crates/conway/src/config/schema.rs crates/conway-core/src/hook.rs
absent: (subagent_mode|hook\.fork)
-->
