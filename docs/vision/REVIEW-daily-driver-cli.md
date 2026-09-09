# The daily-driver review: conway's CLI against the field

**Written 2026-09-07 against `main` at `76624ca` (0.10.0 plus the post-release
merges). Scope: the `conway` binary as an interactive coding tool — nothing
about the library surface or the harness internals except where they leak
into what an operator feels.**

> **What this page is.** An answer to one question: *what stands between the
> conway CLI and being the tool its own author reaches for all day, instead
> of Claude Code?* It is judged against `INTENT.md` §7a/§7b (the daily-driver
> ladder), §5c (model changes must stay cheap), §8.3 (no silent loss), §8.7
> (if the philosophy makes the tool unpleasant, the philosophy is wrong), and
> `DESIGN-surface-coherence.md` §1 (the CLI is a legitimately opinionated
> application; "unopinionated" is a property of the harness). Where a
> recommendation would reverse a recorded ruling, it says so and asks rather
> than assumes (§7 below). Findings are cited to `file:line` or to a real
> session transcript under `~/.conway/sessions/` — not to what the docs
> promise.

---

## 0. Verdict

conway already has most of the **mechanism** a daily driver needs, and in a
few places has more than any harness compared here: a forkable, rewindable
log; provenance on every context segment; deny-with-feedback; a real agent
tree with steer/await/cancel; fail-closed hooks; honest accounting. What it
lacks is the **furniture** — the dozen things a developer touches fifty
times a day — plus a handful of reliability holes that no amount of
mechanism survives. The furniture gap is not a philosophy gap. Every item in
Tier A below is either CLI-layer opinion `DESIGN-surface-coherence.md` §1
already licenses, a config-tier or plugin-tier addition `PHILOSOPHY.md` §5
already names as the right rung, or a defect.

Against `INTENT.md` §7b's ladder: **rung one (supplement) is reachable today
for headless delegation** — `conway -p --agent … --model … --max-seconds …
--output-format jsonl` built, reviewed and merged two real board items on
2026-09-07 — and **not yet for interactive daily work.** The three things
holding it there, in order of evidence:

1. **Model control is awkward** (§5c's own test, failed). Switching requires
   an exact `backend/model` string already present in some role's chain; the
   picker exists only if you install an opt-in plugin; making a model the
   default is a two-hop path the tool's own agent could not find when asked;
   children cannot be pinned to a model from inside a session at all, which
   cost 54 minutes of a real session (§4, A1).
2. **Nothing protects the operator from the model's file changes.**
   Conversation rewind exists; file rollback does not. Four of the six
   harnesses compared ship it (§4, A3).
3. **The first minute is silent and the plugin bus is brittle.** 40–344 s to
   a first token on the default local model; a 5-second MCP timeout kills
   the whole plugin session with no reconnect, which ended four separate
   sessions on one day (§4, A2, A5).

Rung two (*better* output than the incumbent, not comparable) is not
addressed here except to note where conway's own design gives it a route
the others lack (§2, §5).

---

## 1. Scope and method

- **Inventory.** Every flag in `crates/conway-cli/src/cli.rs`, every slash
  command in `tui/commands.rs`, every subcommand, every config key with or
  without a TUI/CLI setter, read from source — the full list is in §9 and
  the appendix; nothing below rests on a doc's claim alone.
- **Real use.** All 30 session transcripts under `~/.conway/sessions/`
  (Sep 3 – Sep 7, 2026: the virgin walk, the first board items done inside
  conway, one 11-hour dogfood session, the two-item overnight run, the
  post-crash reviews). Every latency, refusal, death and workaround cited is
  from those files, by session id and event `seq`.
- **The field.** Claude Code v2.1.263, Codex CLI 0.153.4, OpenCode 1.18.29,
  Cursor CLI (`agent`, Cursor 3), Hermes Agent v0.21, Antigravity 2.x
  (`agy` CLI) — checked against current documentation this week, not from
  memory. Where a claim could not be verified it is marked *(unverified)*.
- **Not done.** The TUI was **not driven under a pty this run** — the
  fourth consecutive review to say so; `docs/vision/review/lens-operator.md`
  §3 already calls two in a row a process defect. The session transcripts
  stand in for driving and are, if anything, harsher evidence than a
  reviewer's own half-hour would be.

---

## 2. Where conway already leads

Stated first because it is fair, and because several of the recommendations
below are "expose this," not "build this."

- **Rewind that never destroys.** `/conway.history.rewind <seq>` forks at a
  point instead of truncating; any finished or crashed session can be forked
  at any seq (`--fork-from <sid>@<seq>`). Claude Code's `Esc Esc` restores by
  rewriting; Codex has no file restore at all and closed the requests as
  duplicates. conway's *conversation* half is the strongest in the field —
  the *file* half is missing (A3).
- **Provenance you can read.** `/context [<agent>]` shows every segment's
  origin and cost. None of the six compared tools exposes this granularity;
  OpenCode users are still asking for a `/context` command (issue #10575).
- **Deny with feedback.** `Esc` at the permission prompt hands the model a
  reason it can act on. Claude Code's `Tab` comment is the nearest analogue;
  the others have accept/reject only.
- **`/ask` with three exits** (fork / pull in / discard). Cursor and
  Antigravity added `/btw` in 2026 for the same need; conway had the
  general form first and it composes from primitives rather than being a
  feature.
- **A real agent tree** with `/agents`, `/steer`, `/await`, `/cancel`,
  `/tree`, and children that report terminal results even on
  panic/deadline. Antigravity's "Agent Manager" and Cursor 3's sidebar are
  this idea with more chrome.
- **Typed refusal over silent recovery.** Oversized tool results are refused
  not truncated; overflow routes loudly to the next candidate; a colliding
  session id points at `--resume`. Every incumbent silently compacts.
- **Honest accounting.** When the root died on a budget (see A5) the
  terminal said exactly what happened: "stopped after 76 turn(s), 51 tool
  call(s) dispatched this run, before producing a final report."
- **One-shot as a real delegation vehicle.** `--agent`, `--model`,
  `--allowed-tools`, `--max-turns`, `--max-seconds`, `--output-format jsonl`
  composed on the first try that had `name:` in the agent def; both items
  landed the same morning. This is INTENT §7's second surface working.
- **The transcript is evidence infrastructure.** Every fact in §4 — model,
  `route_reason`, token estimates, `not_admitted`, truncation policy, child
  results — came out of the JSONL with no other source. No other harness
  here makes its own post-mortem this easy.

---

## 3. Scorecard

● present and usable · ◐ partial, opt-in, or several steps · ○ absent.
"Cursor" is the `agent` CLI unless noted; "AG" is Antigravity's `agy` CLI.

| Capability a developer touches daily | conway | Claude Code | Codex | OpenCode | Cursor | Hermes | AG |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Switch model mid-session from a picker | ◐ text list; menu needs `conway.ui` | ● `/model`, `Opt-P` | ● `/model` + effort | ● `/models`, `F2` recent | ● `/model` | ● `/model` | ● ghost-text |
| Set the default model from inside the tool | ◐ two-hop promote in `/settings` | ● | ● | ● | ● | ● wizard | ● dropdown |
| Effort / thinking / fast lane | ○ | ● `/effort`, `/fast` | ● effort, `/fast` | ● `Ctrl-T` variants | ◐ model variants | ◐ `/reasoning` | ● `--effort` |
| Pin a child agent to a model, in-session | ○ agent-def only | ● frontmatter | ● per role | ● per agent | ● `[effort=…]` | ● `delegation.model` | ● per subagent |
| Resume last session / picker with names | ◐ ULID only, empty view | ● `--continue`, search | ● picker, archive | ● `/sessions` | ● `agent resume` | ● `--resume latest` | ● `/resume` |
| Rewind conversation | ● never destructive | ● | ◐ backtrack only | ● | ● | ● | ● |
| Roll back file changes | ○ | ● checkpoints | ○ | ● `/undo` snapshots | ● checkpoints | ● `/rollback diff` | ◐ *(unverified)* |
| Diff shown before/after an edit | ○ raw JSON | ● in prompt, `/diff` | ◐ after, `/diff` | ● inline | ● inline word diff | ○ | ● per-file pause |
| `@file` mention with completion | ○ | ● | ● fuzzy | ● fuzzy | ● | ○ | ● typo-tolerant |
| `!` shell mode | ○ | ● | ● | ● | ◐ `/shell` | ● zero-token | ○ |
| Image input | ○ core supports it | ● paste/drag | ● paste, `-i` | ● paste/drag | ◐ flaky in CLI | ● paste | ● + PDF/audio |
| Queue / steer while working, visible | ◐ silent queue | ● queue, `Up` recalls | ● inject + queue | *(unverified)* | ● queue + steer | ● configurable | ● + Send Now |
| Vim mode, remappable keys | ○ | ● both | ● both | ● both, leader key | ● vim | ○ | ● vim, F2/F4 |
| Web fetch / search tool | ○ | ● | ● | ● | ● | ● | ● |
| Operator-invoked context reduction | ○ `conway.trim` only, fixed window | ● `/compact` | ● `/compact` | ● `/compact` | ● `/summarize` | ● `/compress` | ● |
| Project instructions file | ◐ `.conway/instructions.md`, no walk-up, no AGENTS.md | ● CLAUDE.md + rules | ● AGENTS.md chain | ● AGENTS.md + CLAUDE.md | ● rules + AGENTS.md + CLAUDE.md | ● AGENTS.md | ● rules |
| Durable memory | ● `conway.memory` (explicit) | ● auto-memory | ● memories | ○ | ○ | ● MEMORY.md | ◐ knowledge items |
| Permission mode persists across sessions | ○ session-only | ● | ● | ● | ● | ● | ● |
| Plan mode with an approval step | ◐ category gate only | ● | ● | ◐ agent switch | ● | ○ | ● plan artifact |
| Todo / task list | ○ | ● | ◐ off by default | ● | ● | ○ | ● artifact |
| Background tool execution | ○ | ● `Ctrl-B` | ● | ◐ child sessions | ● | ● `/bg` | ● |
| Subagents, steerable, with an inbox view | ● `/agents` | ● | ● `/agent`, `/ps` | ● child sessions | ● | ● live steer | ● manager |
| Worktree per agent | ○ | ● `--worktree` | ○ CLI | ○ TUI | ● `-w` | ● `--worktree` | ● |
| Desktop notification / bell on attention | ○ | ● | ● osc9/bel | ● + sounds | ● | ◐ via chat gateway | ● |
| Cost in currency | ○ tokens only | ● `/usage`, `/cost` | ◐ credits | ◐ | ◐ | ● status bar | ● quota |
| Headless with typed event stream | ● jsonl | ● stream-json in/out | ● `--json` | ● `--format json` | ● stream-json | ◐ | ● stream-json |
| Structured output schema | ● `--output-schema` | ● `--json-schema` | ● | ● | ○ | ○ | ● |
| MCP client | ● stdio only, no reconnect | ● + OAuth, tool search | ● + OAuth | ● + OAuth | ● | ● + serve | ● + OAuth |
| Hooks that can deny | ● fail-closed | ● 30+ events | ● 12 events | ● plugin hooks | ● ~20 events | ● Python | ● |
| Runs any provider | ● plugins | ○ Claude only | ◐ OpenAI-compat | ● 75+ | ◐ | ● | ○ |
| OS sandbox | ◐ `conway.confine` (bash only, macOS verified) | ● | ● default on | ○ | ● | ● docker etc. | ◐ preview |

Two rows are deliberately absent from the comparison because `INTENT.md` §9
rules them out: a desktop app, and sandboxing conway cannot deliver.

---

## 4. Findings

Each finding: what the operator feels → evidence → what the field does →
what conway should do, and at which rung (`PHILOSOPHY.md` §5:
configuration / hook / plugin / CLI furniture) → what **not** to copy.
Severity is for a daily-driver operator: **blocker**, **coverage** (you
would miss it), **polish**.

### Tier A — blockers for interactive daily use

#### A1. Model control fails INTENT §5c's own test — *blocker*

**What you feel.** You want a cheaper model for a mechanical stretch, a
bigger window when the work grows, or a specific model for a child. Each is
possible; each is awkward.

**Evidence.**
- `/model <backend/model>` needs the exact string, and bare `/model` lists
  only pairs already named in some role's `chain` — a model you have
  credentials for but have not written into a chain is unreachable without
  editing `settings.json` (`commands.rs:445`, `interactive.md:441-456`).
  The Up/Down menu exists only with `conway.ui` installed
  (`commands.rs:2107-2141`), and `conway.ui` is not in the six-plugin
  default opinion set (`first_party_plugins.rs:147`).
- The default model is a read-only derived row; the write path is
  `/settings → defaults → "this session is running X — Enter to make it the
  persistent default"`, which appears only *after* a `/model` switch
  (`app/defaults.rs:256-316`). Asked "how can i change the default model?"
  on Sep 7, conway's own agent answered with `--model`, `--role-override`,
  and a Python rewrite of `settings.json` — it did not find `/model` or the
  promote leaf, and the operator ended up editing the file
  (`01M1YNRZZ0…` seq 13, 31). A control the tool's own model cannot
  discover from inside the tool is not discoverable.
- Children take a model only through an agent def's frontmatter;
  `conway_fork`/`conway_spawn` deliberately carry no model argument
  (`subagent/tools.rs:338-342`). "lets do the items using glm 5.2" cost
  **54 minutes** of discovery and ended in shelling out to
  `conway -p … --model ollama_cloud/glm-5.2 &` from inside the session
  (`01M1X3753D…` seq 28–36, 05:32 → 06:26).
- Every `/model` switch forks a new agent, so `/agents` grows a row per
  switch (`interactive.md:424-428`).
- The router flipped between primary and fallback six times in one
  conversation with `route_reason.after: []` naming no cause
  (`01M1X3753D…` seq 1–42). `/why` shows the last decision only.
- No effort, thinking-budget, or fast-lane control exists for an operator.
  The Anthropic adapter already reads `params.extra["reasoning_budget_tokens"]`
  (`anthropic/wire.rs:109-135`) but `SamplingParams` is only ever
  `default()` (`schema.rs:256`). `RoleEntry.reasoning` is a capability
  *filter*, not a dial.

**The field.** All six: `/model` opens a searchable picker of every model the
configured providers offer; Claude Code adds `Option-P` and a
`--fallback-model` chain; Codex's picker includes the effort level; OpenCode
cycles recent models with `F2` and thinking variants with `Ctrl-T`;
Antigravity ghost-completes the model name. Claude Code `/effort`, `/fast`;
Codex `model_reasoning_effort`, `/fast`; Hermes `/reasoning high`;
Antigravity `--effort`. Per-child model is universal.

**What conway should do.**
1. *CLI furniture.* Make bare `/model` a picker in the base binary — filter
   as you type, `Enter` switches, one more key makes it the persistent
   default by rewriting `roles.<default_role>.chain` so the head is that
   model. This keeps the 2026-08-30 ruling intact (the default stays a
   *derived* read over the chain, no second scalar; `DESIGN-surface-coherence.md`
   §13) while giving it the write path the ruling implied. The roster should
   be every model the configured backends can name — `.conway/models.json`
   plus what a backend declares — not only chain members. Whether the widget
   lives in `conway-cli` or `conway.ui` joins the opinion set is a decision
   (§7, D6); either ends the "the menu is a plugin" situation.
2. *CLI furniture.* `/fork --model <m>` / `/spawn --model <m>` and a
   matching one-shot flag are the operator half of §7a's parity rule.
   Whether the **model-invoked** `conway_spawn` should also accept a model
   pin reverses a recorded decision and is asked in §7, D1 — the evidence
   for revisiting it is the 54 minutes above.
3. *Configuration.* Populate `roles.<alias>.params` from `settings.json`
   (`reasoning_budget_tokens`, `temperature`, …) so a role can be "the
   thinking one." This is exactly where `CATALOGUE.md`'s "explicitly not
   recommended" entry says a thinking dial belongs — the role's routing
   config, provider-shaped — and it is a schema key that already exists and
   is never filled. Then `/role` already *is* the effort switch. A `/effort`
   verb is not needed and would import Claude Code's vocabulary; a
   `thinking` role is conway-shaped.
4. *Polish.* Do not fork a new agent on `/model` when the switch is the only
   change; or collapse switch-forks in `/agents` by default. Record the
   router's reason for a fallback in the transcript notice, not only in
   `route_reason.after` when it is non-empty.

**Do not copy.** Claude Code's alias vocabulary (`opusplan`, `best`) — a
role is conway's alias and already carries more. A harness-level "fast
mode" — that is a provider product, not a harness concept.

#### A2. The first minute is silent, and the window is spent before you type — *blocker on local models, coverage otherwise*

**Evidence.**
- Time to first token on the default `local/qwen3.8:27b-mlx`: 40 s (virgin
  walk), 84 s, 53 s, 220 s, and **344 s** for `/ideate:review` — an 11k-token
  command prompt that took 5.7 minutes to begin answering
  (`01M1JRGRDP…` seq 1–7). The same board question took 6.6 min on qwen and
  42 s forked onto `ollama_cloud/glm-5.2`.
- The tool registry alone costs ~4.4k tokens with the defaults and ~9–10k
  with the ideate MCP server's tools; on a 32.7k window the runway notice
  fires at **56–57% on the first turn** (`01M1WSKZ…` seq 4, `01M1YNRZ…`
  seq 5), before the operator has said anything of substance. The unit
  renders as `10.6kk of 32.7kk`.
- Ollama Cloud models fall to the 32,768 dialect floor: `glm-5.3` hit 80%
  runway after one tool round-trip (`01M1YPZC…` seq 7); `conway routes
  explain default` reports `tokens: unknown` for every candidate. The 0.10.0
  fix that "actually requests Ollama's context window" does not reach the
  `ollama.com/v1` cloud backend in these sessions.
- Permission prompts leave no trace in the record; the only evidence a
  prompt was waiting is a 20 s / 237 s / 737 s / 1,233 s gap between a tool
  call and its result (`01M1X3753D…` seq 5, 36). §5's own rule — an
  intervention goes *in* the record — is not met for the operator's own
  decisions.
- One-shot's 90+ s silence (SOTU F1) is fixed in `[Unreleased]`; the TUI
  has an activity spinner, so this is a *speed and budget* problem more than
  a *feedback* problem.

**The field.** Claude Code defers tool schemas until needed ("tool search")
and lets subagents and skills carry their own tool subsets; Codex and
OpenCode expose per-server `enabled_tools`/globs; every incumbent runs a
model whose window is 200k–1M by default, so none of them meets this cliff
on turn one.

**What conway should do.**
1. *CLI furniture, first run.* When the chosen model's window is smaller
   than the default install's fixed cost (tool registry + idiom + a typical
   command prompt) by a margin, say so at setup and offer a bigger model or
   a narrower tool set. A 32k local model with the ideate server installed
   is not a usable daily driver, and conway can compute that before the
   operator finds out.
2. *Plugin/config.* Narrow MCP tools per agent and per role, and ship the
   skills-style progressive disclosure for tools: an index line per MCP
   server, full schemas loaded on first use. `Plugin::narrowable_keys` and
   `conway.skills`' `read_skill` pattern are the existing seams. This is the
   single largest lever on small windows and it is pure context quality —
   INTENT §3's own argument.
3. *Defect.* Fix the context-window request for the cloud dialect, or make
   `.conway/models.json` the honest fallback the setup wizard writes;
   `routes explain` must never say `unknown` for a model it will route to.
4. *Record.* Write the operator's permission decisions (grant/deny/feedback,
   with elapsed wait) as records so `/context`, `conway sessions show`, and
   the next review can see them.

**Do not copy.** Auto-compaction as the answer to a small window — INTENT §3.

#### A3. No way back from the model's file changes — *blocker*

**Evidence.** `/conway.history.rewind` forks the *conversation* at a seq. No
checkpoint, snapshot, or undo of the *files* exists (grep `checkpoint`,
`snapshot`, `undo`: 0 hits in `conway-cli`). The 11-hour session's root died
mid-`edit` with `old_string not found` (`01M1JRHSSQ…` seq 258–265); the
operator's only recovery was git.

**The field.** Claude Code checkpoints every edit and `Esc Esc` restores code,
conversation, or both; OpenCode snapshots tracked and untracked files into a
private git object store before and after every step and `/undo` restores
them (`snapshot: false` to opt out); Hermes `/rollback` uses a shadow git
store, shows `/rollback diff N`, preserves the operator's own hand edits,
and rewinds the conversation turn to match; Cursor checkpoints in the
editor. Codex conspicuously does **not** — and "rollbacks coupled with
conversation and code" is the first thing a developer leaving Codex for
Claude Code named (HN 45650188).

**Against the record.** `CATALOGUE.md` explicitly rejected "filesystem
checkpointing as a harness feature" — *git already solves this*. Two things
have changed since it was written. First, `DESIGN-surface-coherence.md`
§8/§12 adopted a convergence test and named its own falsifier: *a fifth
independent harness converges on a paradigm conway rejected → the test
obligates re-examination.* Four independent harnesses now ship it, three of
them on top of git rather than instead of it. Second, the objection was to a
*harness* feature; the recommendation here is a plugin.

**What conway should do.** *Plugin.* A `conway.checkpoint` plugin: a
`post_tool_use` hook on `write`/`edit` snapshots the touched paths into a
shadow object store keyed by session and seq; `/conway.checkpoint.rollback
<seq>` restores them; `/conway.checkpoint.diff <seq>` shows what would
change; `rewind` and `rollback` compose so "restore code and conversation"
is one command. In the default opinion set — it is the safety net a daily
driver assumes — but removable like everything else there. Hermes' choices
(preserve hand edits by default, snapshot before rollback so undo is
undoable) are the ones to match. Decision asked in §7, D2.

**Do not copy.** Claude Code's 100-checkpoint / 30-day retention as a hidden
policy — make the bounds config.

#### A4. Resuming inside the TUI is a blind, two-step operation — *blocker*

**Evidence.** The TUI refuses `--resume`, `--session`, and `--fork-from` at
startup (`app/startup.rs:48-52`). `/resume` takes a 26-character ULID only —
names are rejected here though `--resume` accepts them (`commands.rs:2003`) —
and lands you in an **empty transcript**: `AppState::new` is reset and the
code discloses "no LogRecord → Entry mapping exists" (`commands.rs:2007-2016`).
`sessions.md:237` presents `/resume` as `--resume`'s equivalent. There is no
"continue the last session." Sessions are keyed by cwd, so `conway sessions
list` from a subdirectory shows a different set (`discovery.rs:376-380`).

**The field.** `--continue` is universal; every picker searches by title and
shows a preview; Claude Code auto-titles from the first prompt and offers
"resume from summary" for stale sessions; Codex archives; OpenCode
`/sessions`; Hermes `--resume latest`.

**What conway should do.** *CLI furniture.* Build the `LogRecord → Entry`
backfill so a resumed session shows its history; let `conway --resume
<id|name>` and `conway --continue` open the TUI; make `/resume` a picker
over `sessions list` (name, first prompt, last activity, labels) that
accepts names; auto-name a session from its first prompt when the operator
has not named it (`INTENT.md` §7b: "the name is furniture, and furniture
follows convention"). Consider a project key that walks up to the git root
so `docs/` and the repo root share one list.

#### A5. Reliability holes real use hit in one week — *blocker*

Each of these is a defect, not a design question, and each ended or derailed
a real session.

1. **The TUI panics on a multi-byte character at a fixed byte offset.**
   `transcript.rs:553` sliced `&flat[..120]`; an em dash at that offset took
   the whole TUI down mid-review on Sep 7 (fixed in the working tree,
   uncommitted at the time of writing).
2. **One slow MCP call kills the plugin for the rest of the process.** The
   first `record_read` exceeded the 5,000 ms per-call default; `ChildSession`
   kills the process group on timeout by design and marks the session dead;
   nothing re-discovers, so every later call fails `session died: closed
   stdout (EOF)`. This happened in four sessions on Sep 7 (16:18, 16:36,
   16:38, 19:54); the operator cancelled three `/ideate:review` attempts.
   The plugin that died hosts the operator's actual workflow. The
   fail-closed teardown is right; the missing pieces are a retry with
   backoff before declaring death, and a re-discover on next use after it
   (`conway-plugin-mcp/src/lib.rs:198-205` documents "the caller must
   re-discover" — no caller does).
3. **One-shot children die with no terminal record.** Two workers' transcripts
   end at an `edit`/`read` result with no `agent_result`, no error
   (`01M1X8WE5GY5…` seq 49, `01M1X8WE5G16…` seq 51); the parent saw only
   `warning: tool call failed`. `PHILOSOPHY.md` §1 promises a terminal
   result "synthesized on panic, budget exhaustion, or cancellation." This
   is a violation of a stated guarantee.
4. **The shell status line never refreshes.** `conway.statusline` shipped,
   but the host snapshots contributions once at startup
   (`conway-plugin-statusline/src/lib.rs:30-40`); the documented JSON example
   uses a key (`plugins.statusline`) the schema rejects.
5. **Backgrounding a process holds `bash` to its full timeout** and reports
   `exit code: 0` and `timed out after 120000ms` on the same `is_error`
   result (`01M1X3753D…` seq 84–85, 121–122).
6. **A child fork dies on a 10-minute deadline mid-verification**
   (`01M1JRHSSQ…` seq 169–170) with no warning to the parent before it
   happens; the root's own death at `max_steps=40` with `steps_taken: 76`
   is fixed in 0.10.0 (turn budgets now end the turn, and warn first).
7. **Zero prompt-cache hits on every session mined** (`cache_read_tokens:
   0`, `cache_accounting: not_reported`). The cache percentage is the
   feedback loop `PHILOSOPHY.md` §4 builds the whole tree idiom on; today
   it reads as a steady zero on the operator's providers.

---

### Tier B — coverage: what you would miss on day one

#### B1. No web fetch or search tool — *coverage*

Absent entirely (no HTTP client in `conway-tools`; `WebSearch` appears only
as an unsupported Claude tool name). All six competitors have both. A
daily driver without "read this docs page" is a daily driver with a
second tab. *Plugin:* a first-party `conway.web` (fetch with size bound and
read-only category; search behind a configured provider), or document the
MCP route as the supported path today.

#### B2. Edits are approved from raw JSON and never shown as a diff — *coverage*

The permission prompt renders `edit({"path":…,"old_string":…})`; the result
is `edited {path}: 1 replacement(s)` (`fs/edit.rs:127`). No `/diff`, no
syntax highlighting, no per-file review. Claude Code shows a colored diff in
the prompt and a live diff panel; Cursor renders word-level diffs inline
and has `Ctrl-R` file review; OpenCode renders inline diffs; Antigravity
pauses on a per-file diff. Nobody has per-hunk accept in a terminal yet.

*CLI furniture:* render `old_string → new_string` as a unified diff in the
prompt and the transcript entry; a `/diff` **view** (surface-coherence
VIEW kind) over the session's cumulative changes, `git diff`-shaped;
syntax highlighting is optional and secondary.

#### B3. Input ergonomics — *coverage*

Missing versus the field: `@file` mentions with fuzzy completion (all six;
`@` in conway addresses agents only), `!` shell mode (five of six — Hermes'
runs at zero token cost and is the shape to copy), image paste (five of six;
`ContentBlock::Image` already exists in core, `content.rs:44`), `Ctrl-G`
external editor, vim mode, remappable keys (`keybindings.json`-style, five of
six), a fuzzy rather than prefix-only `/` palette (`view/palette.rs:96,104`),
and a bound `Tab` (`input.rs:1475`). Two small ones from the transcripts:
the operator typed `exit` as a prompt twice and got prose back
(`01M1JP5X…` seq 9, `01M1JPDH…` seq 84) — a bare `exit`/`quit`/`q` should
be caught; and typing while a turn runs is queued to the next turn boundary
by construction but nothing on screen says so (Claude Code shows the queue
and `Up` recalls it; Hermes makes interrupt/queue/steer a setting).

#### B4. Context lifecycle has no operator verbs — *coverage, philosophy-loaded*

There is no `/new`, no `/compact`, no way to shorten a context on purpose;
`conway.trim`'s 8-turn window has no config key
(`conway-plugin-trim/src/lib.rs:127-148`). INTENT §8.3 forbids *silent*
loss, and says in the same breath that composing a context takes nothing
away and that conway "may work out what a stated task needs and act on it,
provided it says what it did." An operator typing a verb is the opposite of
silent.

*CLI furniture:* `/new` (fresh session, same cwd — `Ctrl-N` in spirit).
*CLI furniture over primitives:* `/distill [instructions]` — fork the
current agent, have the child write the briefing, spawn a clean agent with
it, and point the TUI at that agent. That is `PHILOSOPHY.md` §3's own
"reshaping" idiom (`fork → distill → spawn`) as one typed command, the same
liberty `/ask` already takes; the records stay, the old path resolves, and
the cost is knowable. It is the conway-shaped answer to `/compact`, and it
is better than `/compact` because the briefing is an artifact you can read.
*Configuration:* expose `conway.trim`'s window. *Plugin:* ship
`CATALOGUE.md` #5 (the ephemeral compaction hook) off by default, labelled
as the weak form it is.

#### B5. Project instructions: shipped, but off the convergence path — *coverage*

`.conway/instructions.md` and `~/.conway/instructions.md` are read by
`conway.idiom` (`conway-plugin-idiom/src/lib.rs:243-393`) — CATALOGUE #1 is
built. Two gaps: the file is joined to `cwd` and never walked up, so a
monorepo sub-directory sees nothing; and neither `AGENTS.md` nor `CLAUDE.md`
is read. `AGENTS.md` is now the cross-vendor convention (Codex, OpenCode,
Cursor, Hermes read it; Claude Code is the holdout), and OpenCode and Cursor
also read `CLAUDE.md` because switching cost is the point. Under
surface-coherence §8 (familiarity as convergence), reading `AGENTS.md` as a
fallback is exactly the case the rule exists for; reading `CLAUDE.md` is a
single-vendor accommodation and is not recommended. Decision asked in §7, D5.

#### B6. Permissions: the mode does not persist, and three config keys lie — *coverage*

The permission mode is session-only; no config key or flag sets the TUI's
starting mode (`app/run.rs:550-562`), so every session opens in `Prompt`.
`settings.json`'s `permissions.mode`/`allowed_tools`/`denied_tools` parse
and do nothing for the binary (`schema.rs:434-441`) — a declaration-honesty
defect by the repository's own rule. The fourth mode ("auto, gated") was
filed and closed without shipping (`docs/plugins/permission-modes.md`), so
the answer to Claude Code's default-on `auto` mode remains "AutoAllow plus
a hook you configured." Durable `bash` allow grants are deliberately absent
and well argued (`permissions.md`, `migrating-from-claude-code.md`); that
is a real cost every day — six of the operator's seven Claude Code rules
were exactly that — and the honest thing is to keep saying so rather than
soften it.

*Configuration:* a `permissions.default_mode` the binary reads, homed in
`/settings → permissions` (persistent → `/settings`, per surface-coherence
rule 1); remove or wire the three inert keys. *Record:* see A2.4.

#### B7. Plan mode is a gate, not a workflow; no task list — *coverage*

`Plan` allows `Read|Search|Think` categories and denies the rest
(`permission_mode.rs:71-81`). There is no plan artifact, no "here is the
plan, approve it, switch to Prompt" step, and no todo tool (CATALOGUE #3,
unbuilt). Claude Code presents the plan for approval with "yes, and use
auto mode"; Antigravity makes the plan an artifact you comment on; Codex
`/plan`; OpenCode switches agents with `Tab`.

*CLI furniture:* on leaving `Plan`, show the agent's last message as the
plan with approve / edit / discard. *Plugin:* a `todo` tool rendered as a
compact status segment (CATALOGUE #3). Both are rung one.

#### B8. Background execution and "needs you" visibility — *coverage*

No background `bash` (CATALOGUE #4). `/agents` already is the inbox the
field is converging on; what it lacks is an *awaiting-approval* state and
any way to know a child needs you without watching (see B9). *Plugin:*
`bash_start`/`bash_poll` extending `conway.shell`; *CLI furniture:* an
attention marker in `/agents` and the status line.

#### B9. No notification when the tool needs you — *coverage*

Nothing rings the bell or sends OSC 9/777 when a turn ends or a prompt
waits (grep: 0). Five of six harnesses do; OpenCode's "attention" setting
(notify only when the terminal is unfocused, per-event sound packs) is the
best shape. `migrating-from-claude-code.md` lists notifications as
*declined*, but the ruling there is about importing Claude Code's
`settings.json` keys, not about the capability. *Configuration:* a
`tui.attention` toggle (bell / OSC 9 / off, unfocused-only). *Hook:* a
`turn_finished`/`attention_needed` event would let an operator run their
own command — that needs a decision on the hook vocabulary (§7, D3).

#### B10. Cost is tokens only, and the cache reads zero — *coverage*

The status line and turn summary show tokens and cache-hit %; no currency
anywhere (grep `usd`, `pricing`: 0). Hermes puts `$0.06` in the status bar;
Claude Code `/usage` attributes cost to skills, MCP and subagents. With
`.conway/models.json` already carrying per-model metadata, a price per
million is one field away. The zero cache reading (A5.7) is the more
important half: `PHILOSOPHY.md` §4 says "restructure, then look at the
number" — today the number is not there to look at.

#### B11. Git and PR flow — *polish*

No `/commit`, no review command. Codex `codex review --uncommitted`,
Claude Code's skills, Cursor Bugbot. Convergence is weak and conway's answer
is right: a skill or a Claude-compat command. Not recommended as a built-in.

#### B12. IDE and editor protocols — *rung two, watch*

No ACP, no editor extension. OpenCode, Cursor and Codex speak ACP so Zed,
JetBrains and Neovim drive them. `INTENT.md` §9 forbids a desktop app, not
an editor protocol; but this is a large surface with no consumer in the
operator's workflow today. Record it; do not build it yet.

---

## 5. Things the others have that you might want

Beyond parity — ideas that translate to a terminal and fit conway's shape.
One line each; the last column says how it would land.

| Idea | Who | Why it is worth wanting | Fit |
| --- | --- | --- | --- |
| Queue / steer / interrupt as a choice | Hermes `busy_input_mode`, Cursor steering, Antigravity "Send Now" | Typing mid-turn is already queued; making the behaviour visible and selectable turns an accident into a control | config + furniture |
| Shadow-git rollback that preserves hand edits and shows `diff N` first | Hermes, OpenCode | The best-designed rollback in the field; composes with `rewind` | plugin (A3) |
| Walkthrough artifact after a task | Antigravity | A structured "what I did, how I verified it" the operator reads instead of the transcript — this *is* the distillate | furniture over `report` |
| Comment-to-steer on a plan or diff | Antigravity, Claude Code plan editor | Annotate lines, send annotations as one steer | furniture over `/steer` |
| `!` shell at zero token cost, approvals still enforced | Hermes, Claude Code | Look something up without paying for it | furniture |
| Deferred tool schemas ("tool search") | Claude Code | The largest single context saving available on small windows | plugin (A2.2) |
| Smart approvals: a cheap model triages flagged commands, a hard blocklist nothing overrides | Hermes, Claude Code `auto`, Codex Guardian | The "approval fatigue produces bad judgement" argument `DESIGN-permission-modes.md` §3b already makes; the plugin that would carry it was abandoned | plugin, decision |
| `/goal` that persists across turns | Cursor, Codex, Antigravity | A standing objective the agent keeps returning to | plugin or agent def |
| Session `--continue` and auto-titles | everyone | See A4 | furniture |
| `/export` to Markdown | Codex, Claude Code | JSONL is right for machines; humans want Markdown | furniture over `sessions export` |
| `--input-format stream-json` driver mode | Claude Code, Antigravity, Cursor | Lets a host script hold a session open and drive it | one-shot surface |
| LSP diagnostics fed back to the agent after an edit | OpenCode (unique) | Real "better output" leverage: the model learns it broke the build before the operator does | plugin, rung two |
| Self-authoring skills | Hermes | CATALOGUE #7; still the clearest rung-two differentiator in the field | plugin, rung two |
| Remote control / push approvals to a phone | Claude Code, Antigravity, Hermes gateway | The daily reason people leave a terminal tool running unattended | out of scope now; note |
| Theme presets, `NO_COLOR`, `system` theme | OpenCode, Codex, Claude Code | 35 colour slots exist (`tui/config.rs:169-200`) and no way to pick a set | furniture |
| `/doctor` | Claude Code, Codex, Hermes | One command that checks providers, windows, plugins, PATH — most of §A2/A5 would have been self-diagnosed | furniture |

---

## 6. Do not build

Named so the catalogue keeps its shape (`INTENT.md` §2: nothing may
accumulate unexamined).

- **An OS sandbox conway cannot deliver.** §9. `conway.confine` is the honest
  partial and should say so; Codex's kernel sandbox is the field's best and
  it is not conway's job.
- **Auto-compaction.** §3. Operator-invoked verbs (B4) are the line.
- **Silent model fallback.** Routing already refuses loudly; keep it.
- **Durable `bash` allow grants.** Deliberately removed with a measured 68%
  false-positive rate behind the decision. Keep saying what it costs.
- **Global env injection, `SessionEnd`.** Settled (`migrating-from-claude-code.md`).
- **Claude Code's vocabulary** for modes, aliases, or commands where conway's
  own primitive already carries the meaning (`/role` over `/effort`;
  `/distill` over `/compact`). Familiar gesture, not identical names —
  `DESIGN-permission-modes.md`'s own rule.
- **Cron, teams/swarms, a marketplace registry, a desktop app.** Already
  ruled (`CATALOGUE.md`, §9); fork/spawn plus one-shot compose the first two
  today and did so this week.
- **A `default_model` scalar.** Ruled 2026-08-30; the picker in A1 writes
  the chain head instead.

---

## 7. Decisions this review asks the operator to make

Proposals only — none applied. Each names the recorded position it touches.

- **D1 — Model pin on the model-invoked `conway_spawn`/`conway_fork`.**
  Deliberately absent today (`subagent/tools.rs:338-342`). Evidence: 54
  minutes lost, and the operator's stated workflow ("do these items using
  glm 5.2"). Options: (a) keep the tools pure and add only the operator-side
  `/spawn --model`; (b) allow a `role` argument (not a raw model) so the
  choice stays inside routing's vocabulary; (c) allow a model pin.
  Recommendation: (b).
- **D2 — File checkpoints as a first-party plugin in the opinion set.**
  Reverses `CATALOGUE.md`'s rejection on the convergence test
  (`DESIGN-surface-coherence.md` §12). Recommendation: yes, off-core,
  on-by-default, Hermes semantics.
- **D3 — A `turn_finished` / `attention_needed` hook event.** The hook
  vocabulary is deliberately drawn from the primitives; `SessionEnd` was
  declined. "The operator needs to look" is arguably an event conway
  actually emits. Recommendation: add the config-tier bell first (B9), decide
  the hook event on demand.
- **D4 — `/distill` and `/new` as CLI verbs over the primitives.**
  Consistent with `/ask`'s precedent and §8.3's non-silence test; touches
  nothing in the core. Recommendation: yes.
- **D5 — Read `AGENTS.md` as a fallback instructions file; walk up to the
  git root.** Convergence-justified; `CLAUDE.md` is not. Recommendation:
  yes to `AGENTS.md`, yes to the walk-up.
- **D6 — Where the `/model` picker lives.** Either `conway.ui` joins the
  default opinion set or the picker moves into `conway-cli` proper.
  Recommendation: the CLI proper — a model picker is furniture, not a
  capability, and the CLI is allowed furniture (surface-coherence §1).
- **D7 — A persistent `permissions.default_mode` for the binary, and removal
  of the three inert `permissions.*` keys.** Recommendation: yes to both.

---

## 8. If work is sequenced from this page

Rung one first; each row is one board item's worth or less unless marked.

1. **Reliability (A5):** MCP retry-with-backoff and re-discover on next use;
   guaranteed terminal result for one-shot children; statusline refresh;
   cloud context window; `bash` timeout vs background. *(Commit the
   `transcript.rs` fix already in the tree.)*
2. **Model control (A1, D1, D6):** the picker with make-default; `/spawn
   --model`/`--role`; `roles.<alias>.params` wired; router-reason notice.
3. **Resume (A4):** `LogRecord → Entry` backfill; `--continue`; `--resume`
   into the TUI; named picker; auto-titles.
4. **Diff rendering (B2):** in the prompt and the transcript; `/diff` view.
5. **Input (B3):** `@file`, `!`, image paste, `exit` catch, fuzzy palette,
   queue indicator.
6. **Web tool (B1).**
7. **Checkpoints (A3, D2).** *(Large.)*
8. **Attention (B9) and default permission mode (B6, D7).**
9. **Context verbs (B4, D4):** `/new`, `/distill`, `conway.trim` window;
   tool-schema deferral (A2.2). *(The deferral is large and the highest
   leverage.)*
10. **Plan exit and todo (B7); AGENTS.md fallback (B5, D5); cost field (B10).**
11. Rung two: LSP feedback, self-authoring skills, ACP, remote control.

---

## 9. Documentation and declaration defects found on the way

Each is a doc that promises what the code does not do, or the reverse.

- `interactive.md:569-570`: tool-preview line count "persists to
  `settings.json`" — no writer exists; session-only.
- `statusline.md:30-39`: the JSON example uses `plugins.statusline`, which
  `PluginsConfig`'s `deny_unknown_fields` rejects; the real key is
  `tui.status_line_command`. The refresh cadence it describes does not exist.
- `sessions.md:237`: TUI `/resume` presented as `--resume`'s equivalent; it
  rejects names and shows an empty transcript.
- `migrating-from-claude-code.md:428-434,457-460`: shell status line and
  a fourth permission mode "filed, not built" — the first shipped
  (statically), the second closed without shipping.
- `CATALOGUE.md` #1, #6 (project instructions, MCP client) shipped; #9 is
  marked done; the "explicitly not recommended" filesystem-checkpoint entry
  is contested by this page (D2).
- Plugin counts disagree: `plugins/README.md:69` twelve, `statusline.md:148`
  eleven, `README.md:186` nineteen; the code has thirteen ids plus routing.
- `scripts.md:1-19,69` says script hooks "do nothing when loaded";
  `hooks.md:370` says every event dispatches.
- `manual-test-plan.md` status is still **UNWALKED**; the TUI has not been
  driven under a pty in four consecutive reviews.
- Wired but undocumented in `interactive.md`: `Shift-Tab` mode cycle,
  `/plugin install|uninstall`, every `/conway.<plugin>.<cmd>`, the fact that
  typed input during a turn is queued, and every `CONWAY_*` env override.

---

## 10. Not checked

- The TUI itself, under a pty. Session transcripts were the substitute.
- Windows and Linux terminals; every session mined ran on macOS.
- Output quality against the incumbent (rung two's second condition) —
  out of scope for this page and not measurable from one operator's week.
- Whether the six competitors' features behave as their documentation says;
  claims are cited to primary docs and marked *(unverified)* where a doc and
  a release note disagreed (Codex file undo, Codex fallback chains, OpenCode
  queueing, Antigravity per-checkpoint restore).

---

## Appendix — the surface, as read on 2026-09-07

**Flags.** `-p/--print [PROMPT]`, `--output-format text|json|jsonl`,
`--allowed-tools`, `--deny-tools`, `--permission-mode allowlist|deny`,
`--role-override`, `--model`, `--agent`, `--system-prompt`,
`--append-system-prompt`, `--max-turns`, `--max-tokens`, `--max-seconds`,
`--output-schema`, `--session`, `--resume`, `--fork-from`, `--config`,
`--cwd`, `--root`, `-v`, `--version`, `--help`.

**Subcommands.** `sessions {list,show,tree,export,name,unname,label,unlabel}`,
`routes explain <role>`, `tools list`, `memory {list,forget}`,
`plugin {list,install,install --defaults,remove}`, `<plugin-id>.<command>`.

**Slash commands.** `/ask`, `/agents`, `/settings`, `/plugin [install|uninstall]`,
`/trust permissions`, `/steer`, `/cancel`, `/await`, `/context`, `/tree`,
`/why`, `/fork`, `/spawn`, `/resume`, `/model`, `/role`, `/help`, `/quit`,
`/exit`; plugin: `/conway.history.{rewind,mask,checkout}`,
`/conway.names.{rename,unname,list}`, `/conway.memory.{list,remember,forget}`,
`/conway.plugin_skeleton.ping`, `/<compat-id>.<command|skill>`.

**Keys.** Enter · Alt/Shift-Enter newline · Ctrl-P/N history · Ctrl-W ·
Ctrl-E expand all tool output · Ctrl-C interrupt (twice quits) · Ctrl-D ·
Home/End/PgUp/PgDn · Shift-Tab permission mode · Esc · `v` in `/agents`.
`Tab` is unbound; mouse capture is off by design.

**Default opinion set** (installed by the first-run flow): `conway.idiom`,
`conway.stepguard`, `conway.skills`, `conway.memory`, `conway.names`,
`conway.history`. Not in it: `conway.ui`, `conway.trim`, `conway.path`,
`conway.discover`, `conway.statusline`, `conway.confine`, `conway.routing`,
`conway.shell` (bash).

**Config keys with no TUI/CLI setter** — everything except `default_role`,
the head of `roles.<default>.chain`, `backends.<id>` in the three wizard
shapes, `plugins.install`, `plugins.claude_compat[]`, `.conway/models.json`
windows, and `permissions.json` allow rules. Notably: all of `limits.*`,
`routing.*`, `health.*`, `hooks.rules[]`, `tools.builtin_plugins`, every
`tui.*` including theme and status-line fields, and every `roles.<alias>.*`
beyond the chain head.

**Sources for the field.** Claude Code: code.claude.com/docs (model-config,
checkpointing, interactive-mode, permission-modes, sandboxing, memory,
costs, headless). Codex: learn.chatgpt.com/docs (config-reference,
developer-commands, agent-approvals-security, subagents, non-interactive-mode)
and github.com/openai/codex releases 0.146–0.153. OpenCode: opencode.ai/docs
(tui, keybinds, permissions, rules, snapshots, agents, mcp-servers, themes,
config). Cursor: cursor.com/docs/cli (reference, permissions, headless, mcp),
cursor.com/changelog. Hermes: hermes-agent.nousresearch.com/docs
(cli, configuration, checkpoints-and-rollback, memory, delegation,
security). Antigravity: antigravity.google/docs (cli/getting-started,
cli/permissions, cli/modes, cli/headless, artifacts, subagents),
antigravity.google/changelog.
