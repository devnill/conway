# Plan of attack

**Written 2026-09-10 from [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md),
against the working tree at `90ba356`.**

> For the agents doing the work. Snapshot document — replaced wholesale on the
> next run of [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md), not merged into.
>
> **Every item below is on the board**, filed 2026-09-10 under container
> `01M250A8V7A4AM4EMNT2JS1ZMA`; each item's id is given at its heading in §3.
> Three of this round's findings were already filed before the round and are
> referenced, not re-filed (§0). Three items are gated on an operator ruling
> (§2) and are not worker-executable until it lands.

---

## 0. Board state

Surveyed live (MCP, `work_list` paged to exhaustion) 2026-09-09/10:
**55 open, 0 in progress.** The open set is almost entirely the 2026-09-07
daily-driver review's programme (`01M1YRSA5JGZT5QZMPFFYXF3CQ`): Wave 1's
nine reliability defects are done and its eight operator-only dogfood gates
are open; Waves 2–5 (model control, seams, prompt line, coverage) are open
behind their own dogfood gates; four rung-two items and the CAPSTONE walk
are open and operator-scoped. The 2026-09-04 review's container
(`01M1WVJC8Q6ER7DG4SZ3J0BFMF`) is done, 11 of 11.

Already filed, referenced by this plan:

| Finding | Board item | State |
| --- | --- | --- |
| F1 — `/model`/`/role` dead under environment configuration | `01M24ZJ9ABPP0DGVAA2PS3XVDD` | open, filed this run by the lead at the operator's request |
| F2 — malformed tool argument ends the session | `01M23SDCE6T85Z48CRQ8NBY6PV`, `01M23JSD6DRAR8FMDJAZYBXQMB` | open, unclaimed since 2026-09-09; ruling in hand |
| F8 — `.gitignore` un-ignores `settings.json` | `01M23XKD3RDXZ1AK0NCT6NKHEC` | open, unclaimed |

Filed by this round, all under container `01M250A8V7A4AM4EMNT2JS1ZMA`:

| Item | Board id | Gate |
| --- | --- | --- |
| F3 — `--model` dropped by `--fork-from` | `01M250BBPEJ2XMXCJPZ7G573T7` | — |
| F5 — introspection needs a provider | `01M250BXW12HVMKZBCFPKG3704` | — |
| F9 — stale negative claims | `01M250CJQ6P9YE1CH6ZV7QC9G8` | — |
| F11 — config writer path-splice | `01M250DEEWA6FJZE11PKCAYNDC` | — |
| F12 — `LoopDeps` construction | `01M250E7VSXABG6FH4950SGYGC` | — |
| F13 — atomic write across the boundary | `01M250EZBQP8JQM8Z78H696F40` | — |
| F14 — plugin error taxonomies | `01M250FJFWHD5FV5JFBYZN1W2C` | — |
| F15 — permission guard triplet | `01M250G53W26PMY0XAHX5RJF20` | — |
| F10 — `--session <name>` | `01M250H60AHVSKFW12HX8G5FF9` | after F3 |
| F7 — plugin events invisible | `01M250HW1186RKZRNS3DQAMFYW` | after F5 |
| DECISION-1 — project settings trust | `01M250JJRYMF8C6P42K7AKBGRX` | operator |
| DECISION-2 — script half orchestrates | `01M250K5CGNZXZ93775AAAZ8JC` | operator |
| DECISION-3 — single-implementor ports | `01M250KT24AGKDM02NJ15SSQ2Z` | operator |
| Lens amendments | `01M250MJ4FJS324VH3SKTTDRNG` | operator-owned |

One hygiene note: `P5 Extension surface hygiene` (`01M1FRRHQGTRQSDQXZKY5S8QRD`)
was open with every child done and its verification anchor passing; it was
closed as part of this round's triage.

---

## 1. Domains and the collision table

Fourteen new items in eleven domains that can run in parallel, plus three
DECISION items. Each domain names the files it owns; two pairs share a
file and are serialised below.

| Domain | Covers | Owns |
| --- | --- | --- |
| **D-ONESHOT-ARMS** | F3 (`--model` dropped by `--fork-from`), F10 (`--session <name>` two-step) | `crates/conway-cli/src/oneshot.rs`; `crates/conway-cli/src/cli.rs` lines 182–194 (the `--session` doc comment only); `docs/scripting.md` flag table |
| **D-INTROSPECT** | F5 (`plugin list`/`tools list` need a provider) | `crates/conway-cli/src/commands/plugin.rs`, `crates/conway-cli/src/commands/tools.rs`, and the startup provider gate in `conway-cli` (locate by the error text "can't reach a working model provider yet") |
| **D-EVENTS-LIST** | F7 (plugin-declared events invisible) | `crates/conway-cli/src/commands/plugin.rs` (a `--verbose` row); `docs/plugins/hooks.md` |
| **D-STALE-NEG** | F9 (stale negative claims) | `CHANGELOG.md` lines 6737–6745 only (a historical section — the one sanctioned direct edit; everything else goes through `.changelog.d/`); `docs/scripting.md` around line 156 |
| **D-WRITER** | F11 (config writer path-splice ×7) | `crates/conway/src/config/writer.rs` only |
| **D-LOOPDEPS** | F12 (17-field bundle, 8 sites) | `crates/conway-runtime/src/agent_loop.rs`, `crates/conway-runtime/src/runtime.rs`, and the five test files under `crates/conway-runtime/tests/` that construct it |
| **D-ATOMIC2** | F13 (atomic write across the crate boundary) | `crates/conway-tools/src/fs/write.rs`, `crates/conway/src/fs.rs`; `crates/conway/Cargo.toml` only if the feature gate must change |
| **D-ERRVARIANTS** | F14 (MCP/subprocess error enums) | `crates/conway-plugin-mcp/src/lib.rs`, `crates/conway-plugin-subprocess/src/lib.rs`, and the module in `crates/conway/src/` where `ChildSession` lives |
| **D-PERMTRIPLET** | F15 (permission guard triplet) | `crates/conway-runtime/src/permission.rs` only |
| **D-SETTINGS-TRUST** | F4 — *gated on DECISION-1* | `crates/conway/src/config/discovery.rs`, `crates/conway/src/config/trust.rs`, the TUI trust command, `docs/getting-started.md` lines 55–75 |
| **D-AGENT-CLI** | F6 — *gated on DECISION-2* | new `crates/conway-cli/src/commands/agent.rs`; `crates/conway-cli/src/cli.rs` (`Command` enum); `docs/scripting.md` |
| **D-PORTS-DECLARE** | F16 — *gated on DECISION-3* | `crates/conway-core/src/ports/{context_path,discovery,session,path_store}.rs` doc comments, or new `crates/conway/examples/*.rs` |
| **D-REVIEW-PROCESS** | §9's six lens amendments | `docs/vision/review/lens-operator.md`, `lens-caller.md`, `lens-sustainability.md`, `REVIEW-PROMPT.md` change log |

**Shared files and the serialisation they force.**

- `crates/conway-cli/src/commands/plugin.rs` — **D-INTROSPECT first**, then
  D-EVENTS-LIST. The second adds a row to a listing the first makes
  reachable without a provider.
- `crates/conway-cli/src/cli.rs` — **D-ONESHOT-ARMS first** (one doc
  comment), then D-AGENT-CLI (a new enum variant). Trivial to rebase either
  way; the order only avoids a merge.
- `docs/getting-started.md` — the filed F1 item (`01M24ZJ9ABPP0DGVAA2PS3XVDD`,
  which documents environment-only configuration) **first**, then
  D-SETTINGS-TRUST, which rewrites the ancestor-walk paragraph at lines
  63–71.
- `docs/scripting.md` — D-ONESHOT-ARMS (flag table) and D-STALE-NEG (line
  156) touch different sections; **D-STALE-NEG first** because it is a
  one-line edit, D-ONESHOT-ARMS rebases.
- `crates/conway-runtime/src/agent_loop.rs` — on the standing watchlist;
  **D-LOOPDEPS is its single owner this round.** Nothing else touches it.
- `crates/conway-core/src/ports/*` — on the standing watchlist;
  **D-PORTS-DECLARE is its single owner this round**, and only after
  DECISION-3.
- `PHILOSOPHY.md`, `ARCHITECTURE.md`, `Cargo.toml` (workspace) — untouched
  by every domain above. `crates/conway/Cargo.toml` may be touched by
  D-ATOMIC2 alone.

---

## 2. DECISION items — operator-only, before three domains can size

Per this process's convention (`ideate/human-gate`), not worker-executable
until you rule. Each ties to a proposed `INTENT.md` amendment in
`STATE-OF-THE-UNION.md` §8.

**DECISION-1** (`01M250JJRYMF8C6P42K7AKBGRX`) — Does a project `settings.json` reached by the ancestor walk
get the consent gate `permissions.json` has (§8 Q1)? *If yes:* D-SETTINGS-TRUST
becomes an M item — extend the digest-scoped trust store to
`settings.json`, prompt on first encounter, and make `HOME` (not only
`CONWAY_CONFIG_DIR`) exempt the operator's own file from the project walk.
*If no:* a one-paragraph S item stating the direnv-style trust model in
`docs/getting-started.md` and `docs/permissions.md` where the walk is
described.

**DECISION-2** (`01M250K5CGNZXZ93775AAAZ8JC`) — Does the script half of `INTENT.md` §1 include orchestrating
agents (§8 Q3)? *If yes:* D-AGENT-CLI, size L — `conway agent
spawn|fork|steer|await|cancel` over the same facade the TUI slash commands
use, with the `--input-format jsonl` item (`01M1YVWPXWTPZT73R9AK1TVG1M`)
as its natural sibling. *If no:* no item; fold the boundary into §7.

**DECISION-3** (`01M250KT24AGKDM02NJ15SSQ2Z`) — For the four single-implementor ports (§8 Q5): third-party
example, or a stated "harness-fixed" ruling at the definition? *Either
way:* D-PORTS-DECLARE, size S per port for a ruling, M per port for an
example. The 2026-09-04 review asked this and it was not ruled; a second
review asking is itself the signal `STATE-OF-THE-UNION.md` §8 Q5 names.

---

## 3. The dispatchable items

Ordered by the rank of the finding they close. Every item states done,
owner, dependencies and size.

**D-ONESHOT-ARMS / F3** *(S)* `01M250BBPEJ2XMXCJPZ7G573T7` — The `--fork-from` arm of the one-shot
dispatcher (`crates/conway-cli/src/oneshot.rs:780`) reads `--model` through
`parse_model_pin` and sets it on the fork spec, exactly as the arms at
`:682`, `:719` and `:760` do; or, if forking must not accept a pin, the
combination is a usage error naming both flags, as `--cwd`+`--fork-from`
already is. `docs/scripting.md`'s flag table gains the row either way.
*Done:* `conway --model X -p "…" --fork-from <ref> --output-format json`
runs on X, or exits 2 with a message naming both flags; a test pins each
arm's handling of the pin so a fifth arm cannot forget it. *Depends on:*
nothing. Highest-value S item this round: it is a documented script recipe
that currently fails three layers downstream.

**D-ONESHOT-ARMS / F10** *(S, same item or a sibling)* `01M250H60AHVSKFW12HX8G5FF9` — `--session <name>`
on an unclaimed name creates the session and binds the name in one step.
*Done:* `conway --session daily -p "…"` twice: first run creates, second run
exits with the documented "already exists, use --resume" error. *Depends
on:* nothing; lands with F3 to touch `oneshot.rs` once.

**D-INTROSPECT / F5** *(S)* `01M250BXW12HVMKZBCFPKG3704` — `conway plugin list` and `conway tools list`
enumerate without a configured provider. *Done:* both commands succeed
against an empty config dir and print the same rows they print with a
provider; the provider gate still fires for `-p` and the TUI. *Depends on:*
nothing. Note the caller reviewer's observation that these are the commands
a script runs *before* it knows what to configure.

**D-EVENTS-LIST / F7** *(S)* `01M250HW1186RKZRNS3DQAMFYW` — Each installed plugin's declared hook events
are listed by name and summary in `conway plugin list --verbose` (and, if
cheap, a `/help` section). Reads the field `crates/conway-core/src/ports/
plugin.rs:1372` documents as unread. *Done:* the skeleton plugin's
`pong_dispatched` event appears in the listing; `docs/plugins/hooks.md`
points an author at it. *Depends on:* D-INTROSPECT (same file).

**D-STALE-NEG / F9** *(S, doc-only)* `01M250CJQ6P9YE1CH6ZV7QC9G8` — Retire or re-date the "Known
limitations (deliberate for 0.1.0)" section at `CHANGELOG.md:6737`,
verifying all four bullets against the tree (only the `--model` bullet is
confirmed false; check the other three); recapture or annotate the
`docs/scripting.md:156` example so a reader sees what a clean install pays
(`STATE-OF-THE-UNION.md` §2). *Done:* no sentence in either place says
something the tree does not do, and the changelog section carries the
version it was true at or is gone. *Depends on:* nothing.

**D-WRITER / F11** *(M)* `01M250DEEWA6FJZE11PKCAYNDC` — One "find or create this dotted path" primitive
in `crates/conway/src/config/writer.rs` owning skip-whitespace, object
confirmation, member scan, last-wins on duplicate keys and
create-if-absent; the seven `patch_*` functions (`:483,579,1242,1383,1477,
1585,1738`) become thin callers supplying the path and the value-shape
edit. *Done:* `grep -c "iter().rev().find"` on the file drops from 10 to 1;
every existing writer test passes unchanged; the change is reversible in
an afternoon (pure extraction, no format change). *Depends on:* nothing.
**Do this before the model-window item `01M23M2P79R5G28TPGG7PPJQ32`
adds an eighth copy.**

**D-LOOPDEPS / F12** *(S–M)* `01M250E7VSXABG6FH4950SGYGC` — A builder or `new(required).with_x()`
constructor for the runtime's dependency bundle
(`crates/conway-runtime/src/agent_loop.rs:265`), matching the facade
builder's own `with_*` idiom; the two production and five test
construction sites use it. *Done:* adding a new optional dependency is one
method plus one call at the sites that care; `git show --stat` of the
change that adds the next one touches fewer than seven files. *Depends
on:* nothing. Single owner of `agent_loop.rs` this round.

**D-ATOMIC2 / F13** *(S)* `01M250EZBQP8JQM8Z78H696F40` — One atomic-write core in `conway-tools`
(the lower crate, `crates/conway-tools/src/fs/write.rs:88`), with
`conway::fs::atomic_write` (`crates/conway/src/fs.rs:70`) as the sync
wrapper the three CLI callers already use; one `tmp_sibling`; add the
parent-directory sync both copies lack, once. *Done:* one implementation,
the three callers unchanged, a test that would have failed with the
directory sync missing. *Depends on:* nothing. If the `builtin-tools`
feature gate (`crates/conway/Cargo.toml:58`) prevents the wrapper from
reaching the core unconditionally, the item says so and proposes the gate
change rather than keeping the copy.

**D-ERRVARIANTS / F14** *(S)* `01M250FJFWHD5FV5JFBYZN1W2C` — A shared spawn/timeout failure type living
beside `ChildSession` in `conway`, embedded by `McpPluginError` and
`SubprocessPluginError` (`crates/conway-plugin-mcp/src/lib.rs:254`,
`crates/conway-plugin-subprocess/src/lib.rs:256`) via `#[from]` or a thin
variant; the crate-specific variants stay local. *Done:* one place to add
"not found" versus "not executable" when either doc comment's promise is
kept; both crates' `Display` text unchanged. *Depends on:* nothing.

**D-PERMTRIPLET / F15** *(S)* `01M250G53W26PMY0XAHX5RJF20` — A private `desugar_flat(rule, then)` in
`crates/conway-runtime/src/permission.rs` carrying the "flat rules never
desugar to PathsUnder" assertion once; the three `remember_*_pattern`
methods (`:1004,1080,1118`) call it. *Done:* `grep -c "flat rules must
never desugar"` → 1; existing tests pass. *Depends on:* nothing. The
residual of the 2026-09-04 round's `D-PERM`; land it as the same shape.

**D-SETTINGS-TRUST / F4** *(S or M — see DECISION-1)* — Gated. When ruled,
the item also fixes the isolation hazard that confused two reviewers this
run: with `HOME` overridden and `CONWAY_CONFIG_DIR` unset, the operator's
real `~/.conway/settings.json` is still discovered as a project layer for
any working directory beneath the real home (`crates/conway/src/config/
discovery.rs:74`, `docs/getting-started.md:63-71`). *Done, minimum:* the
documented exemption applies to whichever mechanism names the user layer,
`HOME` included. *Depends on:* DECISION-1, and the filed F1 item for the
shared `docs/getting-started.md` section.

**D-AGENT-CLI / F6** *(L — see DECISION-2)* — Gated. *Done, if yes:* `conway
agent spawn|fork|steer|await|cancel` drive the same facade methods the TUI
slash commands use, print JSON under `--output-format json`, and a script
can spawn, await and read a child's result without the TUI. *Depends on:*
DECISION-2; D-ONESHOT-ARMS for `cli.rs`.

**D-PORTS-DECLARE / F16** *(S–M per port — see DECISION-3)* — Gated. *Done:*
each of the four ports (`context_path`, `discovery`, `session`,
`path_store`) carries at its definition either a runnable third-party
example under `crates/conway/examples/` or a stated ruling that it is
harness-fixed, citing the decision. *Depends on:* DECISION-3. Single owner
of `ports/*` this round.

**D-REVIEW-PROCESS** *(S, docs-only)* `01M250MJ4FJS324VH3SKTTDRNG` — The six lens amendments
`STATE-OF-THE-UNION.md` §9 lists: isolation means `CONWAY_CONFIG_DIR` plus
a working directory outside `$HOME`; record `steps_taken` and discard
weight runs where it is not 1; refresh or remove the sustainability lens's
fully-fixed calibration table; revisit tool-call ranges for lenses that
run the binary; say how to bound a run on macOS; say the operator
reviewer's first return is provisional. *Done:* each lands in the lens it
belongs to and the `REVIEW-PROMPT.md` change log gains a row. *Depends
on:* the operator's reading of §9 (REVIEW-PROMPT.md §3 step 4). Not a
worker item by default — the lenses are the part of the process meant to
accumulate under the operator's hand.

---

## 4. Dispatch

Nine dispatchable items with no ruling needed, in two lanes by file
ownership:

- **Lane A (CLI):** D-STALE-NEG → D-ONESHOT-ARMS → D-INTROSPECT →
  D-EVENTS-LIST. All S; the arrows are the shared-file order in §1, not
  dependencies of substance, and any two non-adjacent ones can run in
  parallel.
- **Lane B (harness and plugins):** D-WRITER, D-LOOPDEPS, D-ATOMIC2,
  D-ERRVARIANTS, D-PERMTRIPLET — five disjoint file sets, fully parallel.

Before either lane: **claim the three items already on the board** (F2's
pair and F8). F2 is the daily-driver blocker, its ruling is in hand, and it
has sat unclaimed since 2026-09-09; nothing in this plan outranks it.

The three DECISIONs are quick and unblock D-SETTINGS-TRUST, D-AGENT-CLI and
D-PORTS-DECLARE; they also settle §8 Q1, Q3 and Q5. Worth ruling in the same
sitting as the `INTENT.md` amendments, since each decision *is* one.

**Coverage debt for the next review**, from `STATE-OF-THE-UNION.md` §7: TUI
steps 5–10 were not driven and should be the first thing the next operator
script covers (the launch command must not reproduce F1 — use a config file
with a role chain, and `CONWAY_CONFIG_DIR`, not `HOME`); the config writer
should be re-measured after D-WRITER lands to confirm the eighth field cost
what the item promised; and the four never-checked `CHANGELOG.md` bullets
and three unread `DESIGN-*.md` documents remain owed.
