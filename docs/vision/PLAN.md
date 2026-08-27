# Plan of attack

**Written 2026-08-26 from [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md),
against the working tree at `bc2a174`.**

> For the agents doing the work. Snapshot document — replaced wholesale on the
> next run of [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md), not merged into.
>
> **The board is the authority; this page is the dispatch aid.** Every item
> below already exists on the board, with its full spec, its own size, and
> (where recorded) its own `depends_on` edges — the id is the thing to claim.
> **Every item below was cross-checked against `work_list(status:"done")`
> immediately before writing this page; none is finished work.** What this
> page adds is what the board cannot express on its own: which items collide
> on a file, and the order that forces.
>
> **This round is different from the last one.** The 2026-08-24 plan (REC/OP/
> EMB/CON, 14 items) is the reason this page exists — it is the thing that
> finished, silently, while its own record sat unregenerated for 138 commits.
> This page does not re-propose a single item from it. Everything below is
> either one of today's six audit findings or one of two programs (permission
> modes, plugin dependencies) opened by the two prior agent waves and still
> mid-flight.

---

## 0. Board state

Surveyed live (MCP) 2026-08-26: **12 open, 6 in progress, 0 stale claims, 0
cancelled-but-listed.** Nothing below double-counts a done item — the done
list (110+ items, paged) was read in full for REC/OP/EMB/CON and spot-checked
for every id this page names.

Two board items are explicitly **not** dispatch targets for anyone reading
this page, named here only so the collision table below is honest:

- `01M0XRKQTAB9C2GNQJ72YDM9WA` (in progress) — loops the three living design
  records (`DESIGN-plugin-dependencies.md`, `DESIGN-permission-modes.md`,
  `CATALOGUE.md`) back to the code that closed their gaps same-day. Already
  claimed; its owner already holds those three files this round.
- The plugin-tier enumeration gap `STATE-OF-THE-UNION.md` §1 found (thirteen
  crates listed in `ARCHITECTURE.md` §2b, sixteen on disk) **has no board
  item.** It is real, cited, and deliberately not filed by this item — filing
  is a refine-cycle action, not a docs-regeneration one. Flagged for the next
  cycle rather than silently absorbed here.

---

## 1. Domains and the collision table

Four live programs, not the four domains of the last round — the shape
follows what is actually on the board, not a fixed template.

| Program | What it covers | Board items |
| --- | --- | --- |
| **P-COMPAT** — Claude Code compatibility catching up to itself | commands unreachable, one deny-capable event missing, skills loader unwired, doc staleness, two small debts | 6 items, §2 |
| **P-PERM** — plugin-declared permission modes | premise check → hook registration → the modes themselves → the guard that consumes them | 4 items, §3 |
| **P-PLUGDEP** — plugin-to-plugin capability sharing | the Edge B channel → an altitude ruling → the first real consumer (`conway.ui`) | 3 items, §4 |
| **P-CHAIN** — inherited chain-completion defect | `/resume` drops a plugin-status snapshot, the third link in a chain closed one gap at a time | 1 item, §5 |

Shared files and the serialisation they force:

| File | Claimed by | Order |
| --- | --- | --- |
| `crates/conway-cli/src/claude_compat_plugins.rs` | `01M0XRCAFD7DD7N64RNRM3P8W9` (full ownership), `01M0XRD8VMWD273W0W51T8ECCM` (full ownership) | Commands item first — **already in progress** — then the deny-capable-event item. Both touch this file's dispatch logic; do not run concurrently. |
| `docs/plugins/claude-compat.md` | `01M0XRCAFD7DD7N64RNRM3P8W9` (the `commands/*.md` bullet only), `01M0XKP5BWCPY3BHPJZHXKR4H3` (the "What works" section only) | Same order as above — the commands item's correction is a precondition for the staleness item's own "state what is true of each" instruction (that item's own guard rail names this explicitly). |
| `crates/conway-core/src/ports/plugin.rs` | `01M0XREWGA03EDQ5PK2C18KW75` (one doc comment only), `01M0WX3WSXWYF6N3G6SWN0DSHP` (full ownership, adds `hooks()`) | Small-debts item first (doc comment, S, quick) — then the hooks-registration item, which is L and will rewrite around it. |
| `crates/conway-runtime/src/permission.rs` | `01M0XREWGA03EDQ5PK2C18KW75` (one test only), `01M0WX3WSXWYF6N3G6SWN0DSHP` (full ownership) | Same order as above, same reason. |
| `crates/conway/src/builder.rs` | `01M0XRE2N96ATHEXJ1617E133P` (skills loader wiring), `01M0WX3WSXWYF6N3G6SWN0DSHP` (hooks registration) | Skills-loader item first — **already in progress**, S-sized — then hooks registration. |
| `docs/vision/DESIGN-plugin-dependencies.md`, `DESIGN-permission-modes.md`, `docs/vision/CATALOGUE.md` | `01M0XRKQTAB9C2GNQJ72YDM9WA` only | Not touched by any item below; named per §0. |
| `PHILOSOPHY.md`, `ARCHITECTURE.md`, `Cargo.toml`, `crates/conway-core/src/ports/*` (the other fifteen files), `crates/conway-runtime/src/agent_loop.rs` | — | **Untouched by every item on this page.** Standing collision risks per this process's own template — carried forward, not newly at risk this round. |

Everything else below is a single-owner file this round; only the five rows
above force an order.

---

## 2. P-COMPAT — Claude Code compatibility catching up to itself

The critical finding first, because two other items in this program collide
on the file it touches (table above).

**`01M0XRCAFD7DD7N64RNRM3P8W9`** — *(M, in progress)* Make a translated
command actually invokable: `command_registrations()` gets a real call site
in `conway-cli`, and `docs/plugins/claude-compat.md`'s `commands/*.md` bullet
stops claiming it already does. The audit's one CRITICAL finding.

**`01M0XRD8VMWD273W0W51T8ECCM`** — *(M, open, depends: none; serialise after
the item above — same file)* One deny-capable event is invisible
(`DENY_CAPABLE_EVENT` hardcodes `pre_tool_use`; a translated
`UserPromptSubmit` hook can silently deny every prompt) and the `/plugin`
browser still tells an operator a live, deny-capable hook is "not wired."
Two defects, one root cause, one item.

**`01M0XRE2N96ATHEXJ1617E133P`** — *(S, in progress)* The multi-root skills
loader has a reading half (tested) and no calling half: no config surface,
no production caller. Its agents-loader twin got both; this finishes the
other one. Serialise before the hooks-registration item in P-PERM (table
above — shared file, unrelated concern).

**`01M0XKP5BWCPY3BHPJZHXKR4H3`** — *(S, open)* Six documentation locations
(five plus a sixth the audit found after this item was filed) still describe
the host-capability vocabulary as closed, or claim parse-time fail-closed
behaviour that is now half-true. Sweep-shaped: `grep -rn "HostCapability"
docs/ crates/*/src`, correct every hit, report the sweep whether or not it
finds a seventh. Serialise after the commands item (table above).

**`01M0XREWGA03EDQ5PK2C18KW75`** — *(S, open)* Two unrelated one-line debts
bundled for economy: an `on_failure: Prompt` two-hook interaction with no
test, and `EventDecl::summary`'s doc claiming a CLI consumer that does not
exist. Land first among anything touching `ports/plugin.rs` or
`permission.rs` (table above) — it is the smallest change to either file.

**`01M0X3AMASEJGHZ6ZDMDFWCHSE`** — *(S — elapsed time, not code; open;
depends: `01M0X1FCQ80C9ET97HENXSAW2K`, `01M0X1G29EZSFEWB1YAG40SE69`,
`01M0XBZNBPXEESX8VNTJDKNG0J`, all three done — unblocked)* Smoke-test the
hook translation for real: sounds on, use conway, report what fired.
Produces evidence and board items, not code — do not let it grow into an
audit of all 25 events.

---

## 3. P-PERM — plugin-declared permission modes

A strict chain — each item's `depends_on` is already recorded on the board,
reproduced here so the fan-out order is visible without four separate
`work_get` calls.

**`01M0WX32AKGA9W3S0KCVZHAGED`** — *(S — elapsed time, not code; open;
depends: none — unblocked, first)* Can a ~4B local model actually classify
a tool call as dangerous or routine? Run the shell-script version for real
against real hardware and a real prompt. Everything downstream rests on
this answer.

**`01M0WX3WSXWYF6N3G6SWN0DSHP`** — *(L, open; depends:
`01M0WX32AKGA9W3S0KCVZHAGED`)* `Plugin::hooks()` — the registration surface
two design pages (`docs/plugins/hooks.md`, `docs/plugins/inference-hooks.md`)
are already written against and no code has ever had. Owns
`crates/conway-core/src/ports/plugin.rs`, `crates/conway/src/builder.rs`,
`crates/conway-runtime/src/permission.rs` — see the collision table for what
must land first in each.

**`01M0X4YDNVP7TZ0PVSRJ0388SS`** — *(L, open; depends:
`01M0X1B7Z41J57N6YP2JFZ2AZW`, done — unblocked in parallel with the two
items above)* Plugin-declared permission modes: "auto, gated" becomes a mode
you can name and cycle to. Independent of the hooks-registration item's file
set; can run concurrently with it.

**`01M0WX6ZW84A1G0RV20GBY93J1`** — *(L, open; depends:
`01M0WX3WSXWYF6N3G6SWN0DSHP`, `01M0X4YDNVP7TZ0PVSRJ0388SS`, plus four already-done
items)* `conway.permissions` — the inference-gated guard itself, and the
item the operator originally asked for. Last in the chain; do not start
until both items above are merged.

---

## 4. P-PLUGDEP — plugin-to-plugin capability sharing

**`01M0WWNHQQYN1EVTH8WPZ33EBF`** — *(L, in progress; depends: two done
items — unblocked)* Edge B: the channel that lets one plugin call a
capability another plugin provides, which does not exist today at all.
Report, as part of the item's own acceptance, whether an out-of-process
plugin reaches this on the same terms as an in-process one — the surfaces
lens's open question from §2 of the state of the union, not assumed here.

**`01M0WWM0ZB6BR45XJ8HMTJWZ0Z`** — *(S, **DONE 2026-08-26**)* Two operator
rulings, not an agent's to make: the host/toolkit altitude for `conway.ui`,
and whether `[S1.5]`'s "first slice" scope is over. No code change. **Both
ruled** — the extensible declarative widget tree, built narrow first
(§7a), and the first slice is over, per-plugin config opens with a declared
schema (§6). Recorded in `DESIGN-plugin-dependencies.md` §9; §7a and §6 are
edited in place. §7b (versions on capability edges) was raised alongside
and deliberately left unruled.

**`01M0WWPA70E8YAAN981EK10D3D`** — *(L, open; depends: both items above)*
`conway.ui` — the first bundled provider, and the proof that a
plugin-provided capability is real end to end. Do not start until the
altitude ruling lands.

---

## 5. P-CHAIN — the inherited chain-completion defect

**`01M0XDEDBR5YDF71Q7ZRXYMT85`** — *(S if a snapshot fix, M if it needs to
be live; in progress)* `/resume` drops `plugin_status_contributions` because
`commands.rs`'s `Resume` arm hand-carries two process-lifetime fields across
an `AppState` reset and not this third, same-shaped one — the third link in
a chain (`status_contributions` → render → populate → resume) closed one gap
at a time, disclosed by the writer of the item before it, outside that
item's own file fence. Single-owner file (`tui/commands.rs`); no collision
with anything else on this page.

---

## 6. Dispatch

No single suggested fan-out number — the four programs are already
differently shaped (P-COMPAT is six small-to-medium items with one internal
serialisation pair; P-PERM and P-PLUGDEP are each a strict three/four-item
chain; P-CHAIN is one item). Reasonable read: one worker per program (4-wide),
with P-COMPAT's worker handling its own internal ordering (commands →
{deny-capable-event, staleness-docs} in either order after; skills-loader and
small-debts land whenever convenient, ahead of anything that touches their
shared files per §1's table) and P-PERM/P-PLUGDEP's workers following their
chains strictly since each item's `depends_on` is enforced by the board, not
just documented here.

**Coverage debt for the next review**, carried from
`STATE-OF-THE-UNION.md` §6: no independent reviewer fan-out ran this cycle;
the TUI has not been driven under a real pty since 2026-08-24; the
plugin-tier enumeration gap (§0) needs its own board item; a sustainability
pass has not yet looked at the Claude-compat or plugin-dependency surfaces
landed in this window.
