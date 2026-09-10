# State of the Union: conway

**Reviewed 2026-09-10 against the working tree at `90ba356`, version 0.10.0.**

> Written for the operator. It assumes you care about the shape of the system
> and not about the shape of any particular trait. Everything in it was
> checked against the code in this run; where you might want to check
> something yourself, the file and line is given, and where the claim is
> that something is *absent*, the command that shows it.
>
> Snapshot document — replaced wholesale on the next run of
> [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md).
>
> **This was a Normal round under the process amended on 2026-09-09: 7
> reviewers, dispatched in parallel** — adherence, surfaces, operator, the
> new caller lens, evidence, and sustainability split `core+runtime` /
> `cli+plugins+harness`. **The TUI was driven by hand for the first time in
> this review's history**, from a script the operator reviewer wrote; the
> operator completed 4 of its 10 steps before the script itself found a
> defect and stopped (§4 F1). §7 lists everything seven reviewers did not
> reach. §9 says how the amended process behaved, since that was on trial too.

---

## 0. The verdict, in four sentences

**The written record is true, the script-facing surface is genuinely light,
and the tree is being kept that way.** The machine-checked ledger holds on
all 43 claims, no page is orphaned, and for the first time the adherence
reviewer found *no* sentence in the documentation that the code falsifies.
A script with an empty home directory gets an answer out of conway in one
command and one turn for **2,800 input tokens**; the default installation
pays **3,672**, against a figure of 15,856 recorded on 2026-09-07 — the
first time `INTENT.md` §7a's bet ("small without losing function") has
been scored rather than asserted. **The most consequential finding came
from the operator's own hands, not from a reviewer**: on the first
interactive drive this review has ever had, the second command typed —
`/model` — told the operator no models were configured while the session
was answering on one, and `/role` failed the same way (§4 F1, filed as
`01M24ZJ9ABPP0DGVAA2PS3XVDD`). The cause is not exotic: configuration has
two doors, a file and the environment, and the TUI's model controls look
through only the first. Underneath everything, the largest daily-driver
blocker is unchanged and already on the board — a model that mis-formats
one tool argument still ends the whole session (§4 F2) — and this review
confirms it without adding to it.

---

## 1. What is built, as blocks

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
                    │   THE PLUGIN TIER    │   22 crates; 15 bundled in the
                    └──────────────────────┘   binary; 7 installed by default

  bundled, off unless installed:   skeleton · history · stepguard · skills ·
                                   memory · path · discover · idiom · trim ·
                                   names · ui · confine · checkpoint · web ·
                                   toolindex
  default set (`plugin install --defaults`):
                                   idiom · stepguard · skills · memory ·
                                   names · history · checkpoint
  out of process, no rebuild:      subprocess plugins · MCP servers
```

**Configuration has two doors.** `settings.json` is found by walking up
from the working directory (project layer) and then in the home directory
(user layer); `CONWAY_*` environment variables can supply backends and a
few scalars but cannot create a role or a model chain
(`crates/conway/src/config/merge.rs:694-716`). A `--model` flag pins the
session to one model and bypasses roles entirely. This run reached its
provider through the second door alone, which is what §4 F1 and F4 both
turn on.

**What landed since the 2026-09-04 snapshot.** All eleven items that review
filed are closed (board container `01M1WVJC8Q6ER7DG4SZ3J0BFMF`, done):
session labels gained a write path, the two unwired limits were wired, the
permission-broker triplication was consolidated, `FakePlugin` joined the
testkit, atomic writes were unified into `conway::fs`, the memory tools got
an operator-typed `conway memory` command, and three ports gained embedder
injection points. On top of that the 2026-09-07 daily-driver review's Tier
A landed: a real `/model` picker, `conway.checkpoint` file rollback in the
default set, resume into a TUI that shows the conversation, `conway.web`,
diffs in the permission prompt and transcript, MCP patience-then-respawn,
and the nine "defects that end real sessions" (Wave 1, all done). The
board's 55 open items are almost entirely the remaining waves of that
programme plus the operator-only dogfood gates (§8).

---

## 2. Weight

`INTENT.md` §7a: heavy is measured where the person feels it. First-turn
`usage.input_tokens` for `conway -p "reply with exactly the word pong and
nothing else" --output-format json`, against an isolated config dir with
nothing in it but the plugin list, provider Ollama Cloud `glm-5.2` reached
through environment variables, run from a directory outside any repository.

| Configuration | `usage.input_tokens` | tools announced | plugins | previous |
| --- | --- | --- | --- | --- |
| Bare — no `.conway` at all | **2,800** | 14 from 4 | 0 installed (15 bundled, all off) | — |
| Default — the 7-plugin default set | **3,672** | 18 from 6 | 7 | 15,856 (2026-09-07, `docs/scripting.md:156`, operator's own config; not like-for-like) |
| Default + `conway.web` | 7,719 ¹ | 19 from 7 | 8 | — |

¹ Inflated: the model chose to make a tool call on that run (`steps_taken`
2), so the figure is behaviour plus schema, not schema alone. The lead's own
default-set run showed the same effect (7,369, `steps_taken` 2). Only
`steps_taken` = 1 runs are comparable; the process note in §9 records this.

Two things the table says. **The default set costs about 870 tokens over
bare** for memory, skills, checkpoint, names, history, stepguard and the
environment block — cheap for what it buys. **"Bundle liberally, enable
nothing" is real**: fifteen plugins are compiled in and the bare run pays
for none of them (`conway plugin list` shows all fifteen `[ ]`; `tools
list` shows only the 14 built-ins). The number is also sensitive to where
you stand: inside a git repository the environment block grows, and — see
F4 — a config file found by the ancestor walk can add tools you did not
ask for. The 2026-09-04 review recorded no weight table, so this is the
baseline the next run scores against.

---

## 3. Scored against [`INTENT.md`](INTENT.md)

- **§1, both halves.** The script half works and is cheap (§2; the caller
  reviewer: "one command, one turn, 2,800 tokens, documented exit codes").
  The coding half is close on the headless surface and blocked on the
  interactive one by F1 and F2. Multi-agent orchestration is unreachable
  from a script (F6) — whether §1's second half is meant to include that is
  a question for you (§8 Q3).
- **§5c, changing model is ordinary and cheap.** Fails in two places this
  run could reproduce: the TUI picker under environment configuration
  (F1), and `--model` silently ignored when continuing a branch with
  `--fork-from` (F3). Both are CLI dispatch, not routing; the routing path
  honoured the pin every time it was asked.
- **§7, ceremony before a completion.** Low for a script. Non-zero in one
  avoidable place: `plugin list` and `tools list` refuse to run until a
  provider is configured, though neither talks to a model (F5) — you need
  a provider to see what there is to turn off.
- **§7a, fully functional and not heavy.** Scored for the first time (§2).
  Holds. The disabled-plugin-costs-nothing test passes on the two plugins
  measured.
- **§7c, non-Rust embedding.** Unchanged: the survey exists, nothing beyond
  it, and `INTENT.md` itself says so. Not a finding.
- **§8.3, declaration honesty.** Held across the documentation layer
  (adherence: zero falsified sentences). Two stale *negative* claims
  survive in places the adherence lens does not own: `CHANGELOG.md:6740`
  says `--model` is not wired (it is), and the first-turn figure in
  `docs/scripting.md:156` is four times what a clean install pays (F9).
  And one live message is false about the state it describes: "no models
  are configured" while a model is answering (F1).
- **§8.5, a surface is proven by a consumer that is not its author.** Four
  ports are proven (backend, permission gate, hook runner, plugin), seven
  are consumed by in-tree code that is not their author, and four have
  exactly one implementor — the harness itself (F16). Progress from
  2026-09-04, when the surfaces reviewer scored every port at *exercised*.
- **§8.10, cost of change.** Actively practised: every calibration instance
  the sustainability lens carries (`fake_router` ×36, `text_response` ×52,
  `build_conway` ×46, `kill_group`, `DEFAULT_TIMEOUT_MS`) is already fixed
  at HEAD. What remains is the same shape one level out (§5).

**Seven Intent gaps** came back from five reviewers; three are new this
round and two repeat questions the 2026-09-04 review asked. All are drafted
as proposed amendments in §8, none applied.

---

## 4. Findings

Ranked by `INTENT.md` §8.7's test: a finding whose *Operator sees* sentence
can be written outranks one whose cannot, whatever section it cites.
Where two lenses hit the same thing, it is one finding and the fact is
noted. Three findings were already on the board before this run; they are
listed so the picture is complete, not re-filed.

### Significant — the operator sees it

**F1 — `/model` and `/role` say nothing is configured while the session is
answering.** *Operator sees:* "I can't list models (no models are
configured -- add a provider and a role chain first). The first prompt
does work though." (operator-driven, step 2); `/role` → "role 'tester' is
not a configured routing role alias (configured roles: default)" (step 4).
The launch had a backend supplied through `CONWAY_BACKENDS__*` and a
`--model` pin, and no role chain, because the environment cannot create one
(`crates/conway/src/config/merge.rs:707-716`). The headless path routed the
same configuration correctly (§2's 2,800-token run). The operator's own
file-based configuration lists models normally. Against §5c and §8.3.
**Filed this run as `01M24ZJ9ABPP0DGVAA2PS3XVDD`** with the exact
reproduction; size M. *Cost of not doing it:* anyone who reaches conway
through the environment — CI, containers, this review — gets a TUI whose
two most basic controls are dead, with a pointer to `/settings` that does
not describe the real state.

**F2 — One mis-formatted tool argument ends the whole session, and a
finished child's report can be destroyed with it.** *Operator sees:* the
planning tool cannot complete a task; the error blames the router ("no
candidate for role default"). Evidence reviewer: `BackendError::ToolParse`
is classified as a request problem, never failover-worthy
(`crates/conway-core/src/error.rs:62,90`, pinned by the test at `:1297`),
and no other path recovers a schema-rejected call — two sites, one
unexamined premise, which is `CONDUCT.md` §5.1's series shape. **Already on
the board**: `01M23SDCE6T85Z48CRQ8NBY6PV` and `01M23JSD6DRAR8FMDJAZYBXQMB`
(both open, unclaimed, 2026-09-09); the coerce-versus-refuse question is
already ruled (decision `01M23XAJ3ZKPDKSFTB25TFMH0Y`). Size M. This is the
biggest daily-driver blocker in the tree and it is not waiting on a
decision, only on a worker.

**F3 — `--model` is silently dropped when combined with `--fork-from`.**
*Operator sees:* a script continues a branch on a pinned model and gets
exit 4, "routing error: no candidate for role default (0 considered)", with
no hint the flag was ignored. Reproduced live by the caller reviewer and
confirmed in source: the flag-free, `--session` and `--resume` arms of the
one-shot dispatcher all read the pin (`crates/conway-cli/src/oneshot.rs:682,
719,760`); the `--fork-from` arm at `:780` never does. `docs/scripting.md`'s
flag table lists restrictions for `--cwd`, `--system-prompt` and the budget
flags with `--fork-from`, and none for `--model`. Against §5c and §7.
*Done looks like:* the fourth arm reads the pin like the other three, or
the combination is a named usage error. Size S.

**F4 — A project `settings.json` found by walking up the directory tree is
merged with no consent, and a home override is not isolation.** *Operator
sees:* `tools list` from inside `/Users/dan/code/conway` reports 35 tools
from 8 plugins, including `work_claim`, `steering_put` and `record_append`
at dangerous risk; the same command from `/tmp` with the same isolated
`HOME` reports 18 from 6 (reproduced by the lead). **Two lenses hit this**:
the operator reviewer reported it as a trust gap, the surfaces reviewer as
a methodology artifact. The lead's verification says both are right about
different halves. The repository carries *no* `.conway/settings.json`
(`git ls-files .conway/` → `instructions.md`, `permissions.json`; the file
is absent on disk). The extra tools came from the operator's real
`/Users/dan/.conway/settings.json`, discovered as a *project* layer because
the walk (`crates/conway/src/config/discovery.rs:74-89`) passes through
`/Users/dan` on its way up, and only `CONWAY_CONFIG_DIR` — not `HOME` —
exempts that file (`docs/getting-started.md:63-71`, documented). So the
mechanism the operator reviewer described is real: any checkout's
`.conway/settings.json` between you and the root is applied, and the
consent gate that exists for `permissions.json` (`crates/conway/src/config/
trust.rs`, whose own header says it implements "exactly one kind --
permission_file") does not exist for `settings.json`. Against: nothing in
`INTENT.md` — §8 Q1. Size M once ruled. *Cost of not doing it:* a cloned
repository can install plugins and tools the moment you `cd` into it, and
two of this review's own reviewers were confused by the effect.

**F5 — `plugin list` and `tools list` refuse to run without a working
provider, though neither uses one.** *Operator sees:* "conway can't reach
a working model provider yet" instead of a listing (lead and caller
reviewer, independently, against an empty config). Against §7: ceremony
before you can even decide what to configure. *Done looks like:* both
commands enumerate from the compiled-in bundle and the merged config
without the provider gate. Size S.

### Minor — the operator sees it

**F6 — Fork, spawn, steer, await and cancel exist as model tools and TUI
slash commands, and nowhere a script can reach.** The `Command` enum has
`Sessions`, `Routes`, `Tools`, `Memory`, `Plugin`, `External`
(`crates/conway-cli/src/cli.rs:277-319`); `ls crates/conway-cli/src/commands/`
→ `fmt memory mod plugin routes sessions tools`. §7a's parity rule is
satisfied for the terminal; whether §1's script half is meant to
orchestrate agents is §8 Q3. Size L if yes. Related open item: `--input-format
jsonl` (`01M1YVWPXWTPZT73R9AK1TVG1M`).

**F7 — A plugin can declare a hook event, and no surface lets the operator
see its name.** `crates/conway-core/src/ports/plugin.rs:1372-1382`, the
field's own doc: "nothing in `conway-cli` reads this field ... a forward
declaration an operator has no way to see". An operator writing a
`[hooks].rules[]` entry against a plugin event is guessing a name only the
plugin's Rust source knows. Against `PHILOSOPHY.md` §5's own rule that an
open vocabulary nobody can enumerate is worse than a closed one. Size S:
a `plugin list --verbose` row or a `/help` section.

**F8 — `.gitignore` un-ignores `settings.json` on the strength of a comment
that was never true.** `.gitignore:15-19` re-admits it as "a file a team is
meant to share"; `git ls-files .conway/` → `instructions.md`,
`permissions.json`; `git log --all --oneline -- .conway/settings.json | wc -l`
→ `0`. A file that can hold a literal API key shows up ready to stage; it
happened once on 2026-09-09 and was caught by hand. **Already on the board**
as `01M23XKD3RDXZ1AK0NCT6NKHEC` (open, unclaimed). Size S.

**F9 — Two stale negative claims in places the ledger does not reach.**
`CHANGELOG.md:6740` ("`--model` is accepted by the CLI parser but not yet
wired") is false — `crates/conway-cli/src/oneshot.rs:317,682,719,760` — and
the "Known limitations (deliberate for 0.1.0)" section it sits in has no
version anchor (`grep -c "0.1.0\]:" CHANGELOG.md` → `0`), so a reader
cannot tell it is nine minor versions old. `docs/scripting.md:156`'s
captured 15,856-token example is what a reader skimming for "what does a
turn cost" anchors on, and a clean install pays a quarter of it (§2). One
doc batch, size S. Understating what is built is the defect this tree has
been bitten by before (`CONDUCT.md` §2).

**F10 — `--session <name>` cannot create a session under a chosen name in
one step.** Documented and deliberate (`crates/conway-cli/src/cli.rs:182-194`);
two commands where a script wants one. Size S. Listed because the operator
reviewer met it while driving, not because it is wrong.

### Philosophy and cost of change — no operator sentence

**F11 — The config writer re-derives "find or create this dotted path in a
JSON document" once per writable field.** Seven `patch_*` functions
(`crates/conway/src/config/writer.rs:483,579,1242,1383,1477,1585,1738`), each
re-implementing skip-whitespace, confirm-object, scan members, last-wins on
duplicate keys, create-if-absent; the last-wins idiom appears ten times
(`grep -c "iter().rev().find" crates/conway/src/config/writer.rs` → `10`),
and the file's own comments name the shipped defect the idiom guards
against (`:257-259,314`). Added one field at a time across four board items,
each locally reasonable — `CONDUCT.md` §5.1's series. Against §8.10 and
§8.6. *Change made cheap:* the next writable field costs a path and a value
shape, not 150–450 hand lines. Size M.

**F12 — The runtime's 17-field dependency bundle is built by hand at eight
sites.** `crates/conway-runtime/src/agent_loop.rs:265` defines it with no
constructor; `git show --stat 4964fde` shows that adding one field touched
seven files. The fix's shape already exists in the same tree (the facade
builder's `with_*` methods). Against §8.10. Size S–M.

**F13 — The atomic-write protocol is written twice, across a crate boundary,
and both copies share the same gap.** `crates/conway/src/fs.rs:70` and
`crates/conway-tools/src/fs/write.rs:88`, each with its own temp-sibling
helper; neither syncs the parent directory after the rename. The 2026-09-04
round's consolidation reached the three CLI callers (`session_names.rs:261`,
`tui/history.rs:69`, `conway-plugin-names/src/lib.rs:389`); this is the
residual, and `conway` already depends on `conway-tools`
(`crates/conway/Cargo.toml:58`). Against §8.10. Size S.

**F14 — The MCP and subprocess plugin crates keep two error enums identical
by hand.** `crates/conway-plugin-mcp/src/lib.rs:254-259` and
`crates/conway-plugin-subprocess/src/lib.rs:256-262`: same variants, same
fields, same doc text. The consolidation that unified their mechanism
(commit `fd25565`) left the taxonomy split on purpose, per its own message.
Against §8.10; classification of a spawn failure is mechanism, not policy.
Size S.

**F15 — Three permission methods triplicate a guard their siblings already
consolidated.** `crates/conway-runtime/src/permission.rs:1004-1018,1080-1088,
1118-1126` (`grep -c "flat rules must never desugar"` → `3`), the residual of
the 2026-09-04 round's `PermissionBroker` item. Against §8.10 and §8.6.
Size S.

**F16 — Four ports have exactly one implementor, and it is the harness.**
Verified by search: `grep -rln "impl ContextPathHost for" crates --include='*.rs'`
→ core definition and `conway-runtime/src/context/path_host.rs` only;
`SessionDiscoveryHost` → `conway/src/discovery_host.rs` and the testkit;
`SessionStore` → `conway-session/src/store.rs` and the testkit;
`PathStore` → session, runtime, testkit. (The surfaces reviewer listed a
fifth, the subagent host; `INTENT.md` §7 declares that one single-authority
by design, so it is excluded here.) Each has a `with_*` injection point no
example exercises. Against §7c. *Done looks like:* per port, either a
runnable third-party example or a stated ruling that it is harness-fixed —
the 2026-09-04 review asked the same question (§8 Q5 then, Q5 now). Size M.

**F17 — Trust in operator-named external programs is "you typed the path",
and the tree does not say so anywhere.** `crates/conway-tools/src/hook_runner.rs:79`,
`crates/conway-plugin-subprocess/src/lib.rs:471`,
`crates/conway-plugin-confine/src/launcher.rs:52,110` spawn the configured
path directly; `grep -rniE "signature|checksum|sha256|TrustPolicy"
crates/conway-plugin-subprocess crates/conway-plugin-marketplace` returns
only doc-comment false positives. A disclosure per the surfaces lens, not a
defect; whether anything more is ever in scope is §8 Q6.

### Observations, not findings

- The 10,003-line TUI command file makes every new slash command a
  three-site edit (`crates/conway-cli/src/tui/commands.rs:399,641,2422`). The
  sustainability reviewer could not name the change this would make cheap,
  so under `lens-sustainability.md` §2 it does not ship; noted for the next
  round.
- One-shot always announces all 14 built-in tools; `tools.builtin_plugins`
  does not apply to `-p` (`docs/tools.md:38`, documented). Whether that floor
  is intended is §8 Q7.
- One caller-reviewer run sat idle for over 3.5 minutes at zero CPU and was
  killed; the identical retry finished in under 20 seconds. Unattributed.

---

## 5. Sustainability: where the tree is getting more expensive to change

Both territories came back with the same verdict in different words:
**the discipline is working, and it works reactively.** Where a defect has
shipped, the tree consolidates hard and leaves a tripwire test — the
config-defaults single source, the permission canonicalization, the shared
child-session teardown, the test fixtures. Every calibration instance the
lens carries from 2026-08-24 is fixed at HEAD, and the 2026-09-04 round's
five consolidation items all closed within three days.

What the reviewers found is **the residual of each fix, one level out**:
the permission guard that the consolidation beside it did not reach (F15),
the atomic write that was unified inside one crate and still lives twice
across the crate boundary (F13), the child-session mechanism that was
unified while its two error taxonomies stayed split (F14). None is
expensive; each is the same shape. The standing question from 2026-09-04 —
"we consolidated this once; does every future call site reach for it?" —
has a sharper form now: **when a consolidation lands, does the item name
the boundary it stopped at?** Three of this round's five sustainability
findings sit exactly on such a boundary, and in two cases the commit
message says so.

The one place accumulating *proactively* rather than as residual is the
config writer (F11): seven copies of one splice algorithm, each added by a
different board item for a different field, each reasonable alone. That is
the series `CONDUCT.md` §5.1 describes and it is the sustainability item
most worth doing first, because the next writable field is already on the
board (the model-window item `01M23M2P79R5G28TPGG7PPJQ32` writes
`models.json`).

**Not assessed this cycle:** 14 of 17 port modules, `conway-session`'s
store, the 4,300-line facade builder, the TUI input file, the first-run
wizard, and the subprocess wire module were not read for internal
cost-of-change structure (§7). The programming-by-coincidence hunt was not
run as its own pass by either the evidence or the sustainability lens.

---

## 6. What is good, said plainly

- **The documentation is true.** 43 of 43 ledger claims hold
  (`python3 scripts/check-design-claims.py`); 0 of 136 tracked pages are
  orphaned (`python3 scripts/check-orphan-docs.py`); the adherence reviewer
  sampled the security-bearing claims first and found every one accurate.
  This is the first round with an empty adherence findings section.
- **The script surface is what `INTENT.md` §4 asks for.** One command, one
  turn, 2,800 tokens, clean JSON, documented exit codes, and `--resume`
  from a script needs nothing but the field the previous run printed.
- **The weight bet holds on first measurement.** Fifteen plugins compiled
  in, a bare run pays for none; the default set adds about 870 tokens.
- **The hook vocabulary is fully wired**: every named event has a
  production dispatch site, and the hooks reference cross-references each
  correctly.
- **Four ports are proven by a consumer that is not their author** —
  backend, permission gate, hook runner, plugin — where the 2026-09-04
  review scored zero.
- **The previous review's plan was executed in full within three days**,
  and the sustainability lens's own calibration table is now stale because
  everything in it was fixed.
- **The evidence lens found the tree's historical defects fixed in both
  directions** — the memory label that nothing could set now has a remove
  path; the context-window floor now has a persisted, verified value — and
  found the design documents correcting themselves against the code rather
  than the reverse.
- **The TUI was driven.** Four steps, one real defect with an exact
  reproduction, filed the same night. That is what the process change was
  for.

---

## 7. What this review did not check

The union of every reviewer's *Not checked* section.

- **TUI steps 5–10 were not driven**: permission-mode cycling, `/context`,
  `/plugin` toggling and its effect on the tool count, `/spawn` and
  `/await` parity, and clean `/exit` with session persistence. The operator
  stopped at step 4 when the script found F1. The first four steps are the
  first interactive evidence this review has ever collected.
- `PHILOSOPHY.md` was not read end to end (873 lines; its five "Where the
  tree is today" notes were sampled). Of `docs/`, the pages for agents,
  sessions, routing, embedding, interactive, providers, manual-test-plan,
  dogfooding, migration and the whitepaper were not read line by line; 22
  per-plugin pages were spot-checked, not read.
- `docs/embedding.md` was walked only as far as the getting-started
  example (which ran clean); the real-provider example, the custom
  permission gate example and the `cargo add` dependency graph were not
  exercised. Exit codes 0, 2 and 4 were exercised; 129, 130, 143 and the
  `--output-schema` rejection path were not.
- The marketplace install-from-URL flow, `conway diag`, `--config`
  precedence beyond one case, `--agent` definitions, `--output-schema`,
  `--fork-from`/`--continue` in one-shot, and `conway plugin remove` were
  not driven.
- `CHANGELOG.md`'s stale "Known limitations" section: one of four bullets
  verified stale, three unchecked. `DESIGN-bindings.md` and
  `DESIGN-plugin-dependencies.md` were grepped for amendment language, not
  read against code.
- Sustainability: 14 of 17 port modules, `conway-session/src/store.rs`,
  `conway/src/builder.rs`, `conway-cli/src/tui/input.rs`, `first_run.rs`,
  `conway-plugin-subprocess/src/wire.rs`, and the large runtime/facade
  test files were not read structurally; error enums beyond the MCP/
  subprocess pair were not swept.
- The board's done and cancelled items were not walked for stale "fixed"
  claims. Five of seven reviewers had no MCP access and said so; the lead
  and the evidence reviewer did.

---

## 8. Questions for you — proposed `INTENT.md` amendments

Seven Intent gaps from five reviewers. Each is drafted as the sentiment
that would have settled it, worded to fold into the argument where it
belongs. **None has been applied.** Q5 and Q6 repeat questions the
2026-09-04 review asked and `INTENT.md` does not yet answer.

**Q1 — Is a project `settings.json` found by walking up the directory tree
as trusted as the directory you are standing in, or does it deserve the
consent gate `permissions.json` already has?** (F4; two lenses.) *Proposed,
for §8.9 after "Where a convention already exists, borrow it":* "The
convention for configuration found in a checkout is direnv's, not the
shell's: it is applied only after the operator has said yes to *these
bytes*, and an edit un-says it. What a project file may do without consent
is bounded by what it cannot do harm with — a plugin list, a backend, a
tool it enables are all authority, and authority found in a directory
somebody else controls is offered, never installed."

**Q2 — Is configuration through the environment alone a supported way to
run conway, or a convenience for tests and CI?** (F1.) *Proposed, for §7b
after "Familiarity is the on-ramp":* "Every door into configuration is a
full door. Whatever can be said in the settings file can be said in the
environment and on the command line, and every surface that reports what
is configured reads the merged result, never one door. A control that
works when the file says it and fails when the environment says the same
thing is a defect in the control."

**Q3 — Does §1's script-and-pipeline half include orchestrating agents, or
only getting a completion back?** (F6.) *No proposed text — this is an
appetite call.* If yes, §7a's parity rule extends to the one-shot surface
and F6 is a board item sized L. If no, say so in §7 so the next review does
not ask again.

**Q4 — Is a persistent "set the default model" verb in scope for the CLI,
or is the TUI's picker the everyday path and the file the advanced one?**
(Operator reviewer.) *Proposed, for §5c:* "Cheap means cheap from every
surface. The terminal picker and a one-shot flag are the same decision
made from two places; if the decision is worth persisting from one it is
worth persisting from the other, and hand-editing a file is not one of the
places."

**Q5 — For a port with one implementor and that implementor is the harness,
is the intended end state a third-party proof or a stated ruling that it is
fixed?** (F16; asked on 2026-09-04 as Q5.) *Proposed, for §7 after "Every
other socket an embedder can supply its own implementation of":* "A socket
nobody outside the harness has filled is either waiting for its first
caller or is not a socket. Say which, at the definition, so a reader can
tell a seam from a fixture. A socket that has waited through two reviews
without a caller has answered the question."

**Q6 — Is any verification of an operator-named external program — a hook
script, a subprocess plugin, a confined shell — ever in scope, or is "the
operator typed the path" the permanent trust model?** (F17.) *Proposed, for
§8.9:* "A program the operator named runs with the operator's trust; conway
verifies nothing about it and says so where the path is configured. What
conway does verify is *which bytes* the operator agreed to, the same rule
Q1 applies to a project file."

**Q7 — Who re-verifies a dated list of known limitations as the tree moves
past the version it described, and does one-shot's fixed tool floor belong
in such a list?** (Evidence and caller reviewers.) *Proposed, for §8.3:* "A
statement that conway cannot do something is a claim like any other and
ages worse, because nobody is harmed by believing it until they build the
workaround. It carries the version it was true at, or it goes in the
ledger, or it goes."

---

## 9. How the amended process behaved

Five changes to the review were on trial this run (`REVIEW-PROMPT.md`
change log, 2026-09-09). What happened to each:

- **The "Operator sees" line worked.** Every reviewer used it; the ranking
  in §4 fell out of it mechanically, and reviewers wrote "nothing;
  philosophy finding" where that was true rather than inventing a
  consequence.
- **The fifth failure mode held.** The evidence reviewer wrote F2
  operator-first and explicitly declined to re-litigate the coerce-versus-
  refuse ruling. No returned finding opened by asking whether a fix fit
  conway's stance.
- **The layering rule was applied.** No reviewer reported a CLI opinion as a
  §8.2 violation; F1 was filed as a `conway-cli` finding with the layer
  named.
- **The weight measurement worked and taught something:** only
  `steps_taken` = 1 runs are comparable, and "isolated `$HOME`" is not
  isolation when the working directory is beneath the real home (F4). Both
  belong in the lenses.
- **The operator-driven TUI script worked on its first outing** — four
  steps, one defect, filed — and the script's own launch command was the
  reproduction. The operator reviewer's first return was necessarily
  provisional; the lens does not say so.
- **The caller lens earned its place**: F3 and F5, the weight table, and the
  cleanest "what is good" paragraph in the round came from it.

Lens amendments this round suggests, for `REVIEW-PROMPT.md` §3 step 4 —
noted, not applied: (1) `lens-operator.md` §3 and `lens-caller.md` §2 should
say `CONWAY_CONFIG_DIR` plus a working directory outside `$HOME`, not
"isolated `$HOME`"; (2) both should say to record `steps_taken` and discard
weight runs where it is not 1; (3) `lens-sustainability.md` §3.1–3.2's
calibration table is fully fixed and needs new instances or none; (4) four
of seven reviewers exceeded their tool-call range (surfaces 46 of 40,
caller 43 of 40, core sustainability 46 of 45) — the ranges may be tight for
lenses that run the binary; (5) macOS has no `timeout`, and lenses that
bound runs should say how; (6) `lens-operator.md` §3 should say the first
return is provisional when the operator is available.
