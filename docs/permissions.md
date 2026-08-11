# Permissions

conway asks before an agent's tool calls take effect; this page is the full
reference for controlling that — the modes, the prompt, the rules file,
trust, confinement, and what none of it actually guarantees. Read the last
section (Limits) even if you skim everything else: it says what this
mechanism does *not* protect against, and a wrong assumption there is the
kind of mistake this page exists to prevent.

## Permission modes

How much you are asked. Every TUI session starts in `Prompt`; change
it mid-session with `/settings`, which cycles Prompt → Plan → AutoAllow →
Prompt. (`settings.json`'s own `[permissions.mode]` is a different,
narrower setting — `prompt`/`allowlist`/`deny` — that only matters to a
library embedder assembling a `Conway` with no gate of its own; the `conway`
binary's TUI and `-p` one-shot mode both always supply their own gate and
never consult it.)

| Mode | Effect |
| --- | --- |
| `Prompt` (default) | Every distinct tool call pauses for your decision — see "The prompt" below. Nothing runs without you seeing it first. |
| `Plan` | Allows the non-mutating categories (`Read`, `Search`, `Think`) without asking; denies everything else outright, including a `bash` call that merely reads a file — the category is what `bash` *declares itself as* (`Execute`), not what a given command happens to do, so plan mode never has to parse or guess at shell syntax. For exploring a codebase with a guarantee that nothing changes. |
| `AutoAllow` | Allows every call without asking. |

**`AutoAllow` honestly.** This is the mode with no human in the loop for an
ordinary call, so it is worth being precise about what still applies inside
it and what does not:

- **Still in force:** the confinement root (`--root`, below) — a tool call
  outside it is denied before `AutoAllow` is ever consulted; every `deny`
  rule in `permissions.json`, from any file, trusted or not; and every
  `prompt` rule, which forces the ordinary permission gate for a matching
  call *even in `AutoAllow`* — the one mode a `prompt` rule exists to matter
  in, since it is the one mode with no other human check left to catch what
  it would have caught (verified below).
- **Not in force:** everything else. There is no per-call review, and no
  cached decision to fall back on if you change your mind mid-call — a call
  already dispatched runs to completion.

The status line's `mode` field names `AutoAllow` in capitals
(`ready · AUTO-ALLOW`) specifically because forgetting you are in it is this
mode's real risk, and it is the one field the status line's own narrow-width
degradation refuses to drop (see [`interactive.md`](interactive.md)'s status
line reference).

## The prompt

Under `Prompt` mode (or a `prompt` rule match in any mode), a tool call
pauses with a modal naming the tool, its category, the agent proposing it,
and the command as it would actually run:

```
┌ PERMISSION REQUIRED ────────────────────────────────────────────┐
│echo pong                                                        │
│[y] once  [a] always  [p] pattern  [n] deny  [Esc] deny w/ feedback│
│  [a]/[p] remember for: this session  ([s] cycles)               │
│  [p] grants: `bash` commands starting with `echo pong`          │
└───────────────────────────────────────────────────────────────────┘
```

| Key | Grants | Persistence |
| --- | --- | --- |
| `y` | This exact call, once. | Nothing — the identical call asks again next time. |
| `a` | This exact call — same tool, byte-identical (canonicalized) arguments — remembered at the current grant scope (see `s` below). A different argument to the same tool is a different call and asks again. | In memory only. Gone at restart; never written to a file. |
| `p` | A prefix pattern: `<command> <subcommand>` for a two-token command (`git status`), or the single token for a one-token command (`pwd`) — narrow by design, so accepting the offer never silently covers a sibling subcommand (`git status` does not grant `git push`). For a tool whose rendering is a structured JSON dump rather than a shell command (`read`, `report`, …), the offer is the `tool:*` wildcard — "any `report` call" — because a prefix over a JSON dump is a rule the loader refuses to register (its token boundaries depend on key order and escaping you cannot predict), and the prompt never offers a rule that cannot be registered. The prompt states the exact grant in words (the `[p] grants:` line above) before you press anything. Not offered at all when a shell command isn't safe to prefix-match — see the metacharacter gate in Limits. | Installed immediately at the current grant scope, **and** — at session scope only — appended to the project-scoped `permissions.json`'s `allow` list (best-effort — a write failure loses only the file durability, never the in-session grant). That write changes the file's bytes, which changes its trust digest, so a *previously untrusted* file gains a rule that still won't take effect on its own until you `/trust permissions`; a *previously trusted* file needs no further action this session, but the digest change means the file is effectively re-recorded as trusted only by virtue of `/trust permissions` having installed the very rule that changed it. A per-agent or per-subtree grant is never written to a file: it names live agent ids, which are meaningless at the next launch, and persisting it would silently widen it to the load scope on restart. |
| `s` | Not a decision — cycles the scope the two remembered-grant keys (`a` and `p`) grant at: **this session** (the default; every agent in the session) → **this agent only** → **this agent and its subtree**. The prompt states the current scope in words next to the keys. The choice resets to *this session* for every new prompt, so narrowing is always a deliberate, per-prompt act. | n/a |
| `n` | Denies this call, once. | Nothing. |
| `Esc` | Denies this call and tells the model to try a different approach (rather than just failing silently). | Nothing. |

`p` and `a` are the only two ways a call ever bypasses the prompt for a
*later* call in the same session; `y`/`n`/`Esc` each resolve exactly the one
call in front of you. A narrowed grant only ever covers less than a session
grant: a per-agent grant never authorizes a sibling agent's identical call,
and a per-subtree grant covers exactly the agents whose path descends from
the granting one — both are proven end to end in
`crates/conway/tests/permission_scope_seam.rs`, and a grant whose scope you
did not intend is best avoided by reading the scope line before pressing a
remembered-grant key.

## Rules in `permissions.json`

A rules file — project-scoped (alongside the nearest `.conway/settings.json`,
or `<cwd>/.conway/permissions.json` if none is discovered yet) or
global-scoped (`~/.conway/permissions.json`, or
`$XDG_CONFIG_HOME/conway/permissions.json`) — is a flat JSON object of
wire-form strings, meant to be read and diffed like any other config file:

```json
// .conway/permissions.json
{
  "allow": ["bash:cargo test", "read:*"],
  "deny": ["bash:curl", "bash:ssh"]
}
```

A rule is `<tool>:<prefix>`, matched against the call's *rendered* form.
For `bash` that form is the bare command string, so `bash:cargo test` means
what it reads as — and the shell-metacharacter gate (see Limits) guards it,
so a chained command can never slip past a prefix grant. For every other
built-in tool the rendered form is a structured JSON dump
(`read({"path":"…"})`) that is never handed to a shell, so the gate does
not apply to it and a wildcard like `read:*` above is the practical grant
shape there. (From 0.5.0 until 0.7.0 only `bash` grants could match at
all — every other tool's rendering tripped the gate and its rules were
inert; before 0.5.0 even `bash` rendered as a structured dump, so no
pattern grant matched anything.)

Both files are loaded, project first, at session start, and their rules are
**merged**, not overridden — a global "I always allow this, everywhere" rule
and a project "this checkout's build command is fine" rule answer different
questions, and one silently discarding the other would surprise you either
way.

**A misspelled top-level key is a load error, not a silent no-op.** The only
keys this file recognizes are `allow`, `deny`, and `rules`. Typing `"denys"`
instead of `"deny"` used to parse cleanly, silently fall back to an empty
deny list, and install nothing — the file "worked" and the rule you wrote
simply never took effect, with nothing telling you. That is fixed: a file
naming any key outside that set fails to load entirely — none of its rules
(`allow`, `deny`, or `prompt`) take effect, and the transcript shows an
error naming the offending key, at the same severity as a registration error
(a red entry, not a routine notice). Fix the key and the file loads on the
next session start, or after `/trust permissions` re-reads it.

## The structured `rules` array

The flat `allow`/`deny` lists above are the surface syntax for a more
general rule form. A `rules` array sits alongside them in the same file and
expresses everything the flat form can, plus the things it cannot:

```json
// .conway/permissions.json
{
  "allow": ["bash:cargo test"],
  "rules": [
    { "select": { "tools": ["bash"] }, "when": { "command_prefix": "cargo test" }, "then": "allow" },
    { "select": { "categories": ["edit", "delete"] }, "when": { "paths_under": "/home/alice/project" }, "then": "deny" },
    { "select": { "tools": ["bash"] }, "when": { "command_prefix": "rm" }, "then": "prompt" },
    { "select": { "tools": ["read", "grep"] }, "when": "always", "then": "allow" }
  ]
}
```

Each rule is `{ select, when, then }`:

- **`select`** — what the rule applies to. `{ "tools": ["bash", "read"] }`
  matches a tool name (with an optional trailing `*` wildcard, so `"re*"`
  matches `read` and `report`); `{ "categories": ["Read", "Edit"] }` matches
  every tool that declares itself in one of those categories.
- **`when`** — the condition. `"always"` matches every call; `{
  "command_prefix": "git status" }` matches a shell rendering that starts
  with that prefix (the same token-wise prefix the flat form uses, with the
  same metacharacter gate on the allow side); `{ "paths_under": "/dir" }`
  matches a call whose *declared path arguments* resolve under that
  directory (read from the call's arguments and resolved the same way the
  tool itself will resolve them — never from the sanitized display
  rendering); `{ "category_in": ["Read", "Search"] }` matches a call whose
  declared category is in the list.
- **`then`** — the effect: `"allow"`, `"prompt"`, or `"deny"`.

A flat `bash:git status` string and the structured
`{ "select": { "tools": ["bash"] }, "when": { "command_prefix": "git status" }, "then": "allow" }`
are the same rule — the flat form is desugared into the structured one and
evaluated by the same path, so the two produce identical decisions. The
`rules` array is for rules the flat form has no syntax for: a `paths_under`
boundary, a `categories` selector, a `prompt` effect, or a multi-tool
selector. A flat entry and a structured entry can sit in the same file; the
flat list stays the ergonomic default, the `rules` array the superset.

**The same trust asymmetry applies.** `then: deny` and `then: prompt` rules
install from every file immediately, trusted or not — narrowing has no
failure mode worth gating on trust, the same reason a flat `deny` does. A
`then: allow` rule from a project file installs only after an explicit trust
decision (see Trust, below), exactly like a flat `allow` entry.

**`paths_under` reads arguments, not the rendering.** The path a rule gates
on is read from the call's declared path arguments and resolved the way the
tool will resolve it (relative to the agent's cwd, absolute paths passed
through, `..` components resolved away by canonicalization). It never looks
at the sanitized, lossy display rendering — a path like
`/repo/../outside/secret` whose rendered string *contains* the rule's
prefix resolves *outside* it, and the rule correctly refuses to match. A
tool the broker cannot statically confine (`bash`'s free-form command, or
any tool that declares itself `Unconfinable`) never satisfies a
`paths_under` rule regardless of what it names — fail closed, the same
asymmetry the confinement root uses.

**`paths_under` cannot confine an `Unconfinable` tool — and on `deny`/`prompt`
that is a registration error.** A `paths_under` predicate can never be satisfied
for a tool whose `PathArgs` is not `Named` — `Unconfinable` (e.g. `bash`, whose
free-form `command` can reach anywhere) or `None` (no path arguments at all).
For `then: allow` that inertness is fail-CLOSED: the broker simply never matches
the rule and the call falls through to the operator's gate, so it is NOT a
registration error. For `then: deny`/`prompt` the same inertness is fail-OPEN:
the call you expected to be refused instead goes through. The loader refuses to
install such a `deny`/`prompt` rule silently and surfaces a typed
`PathsUnderOnUnconfinedTool` registration error (visible as a red transcript
notice at startup, naming the rule and the reason). Rewrite the rule — scope the
unconfinable tool with `command_prefix` (e.g.
`{ "select": { "tools": ["bash"] }, "when": { "command_prefix": "curl" }, "then": "deny" }`),
or drop the unconfinable tool from the `select`. A `Select::Categories` (whose
member tools may register after the rule is loaded) and a trailing-`*` wildcard
`Select::Tools` are not inspectable at load time, so for those the broker
fail-closes at decision time instead: an `Unconfinable` tool matching the
select under a `paths_under` `deny`/`prompt` rule is refused, never silently
allowed.

**A relative `paths_under` prefix resolves against the project, not wherever
conway happened to be launched from.** In a project file
(`<project>/.conway/permissions.json`), a prefix like `"src"` means
`<project>/src` — the directory containing `.conway/`, derived from the
file's own location (so a file discovered in an ancestor directory resolves
against that ancestor, not your launch directory). In the global file
(`~/.conway/permissions.json`, or `$XDG_CONFIG_HOME/conway/permissions.json`)
there is no containing project, so a relative prefix resolves against the
agent's working directory at load time. An absolute prefix is used exactly
as written in both files.

**A `paths_under` prefix that does not resolve on disk is a registration
error, not a silent no-op.** A `paths_under` rule names a directory the
broker must canonicalize into a confinement root; if that directory does not
exist (a typo, or a repo/subdirectory not yet cloned/checked out) the rule
confers no boundary and the broker refuses to install it. The loader surfaces
that as a typed `PathsUnderPrefixUncanonicalizable` registration error
(visible as a red transcript notice at startup, naming the rule and the
reason) instead of silently dropping it — the mirror of the
`read:*`-matched-nothing bug: a rule that can never match is a lie the
operator will not notice. For `then: deny`/`prompt` the hazard is sharpest:
you believed a `paths_under` deny was protecting you when it was never
installed. The same surfacing fires on the `/trust permissions` path, and a
dropped rule is never counted in the "N allow rule(s) installed" report, so
`/trust permissions` cannot report `1 installed` for a rule the broker
dropped. Fix the prefix or create the directory. (Distinct from
`PathsUnderOnUnconfinedTool`: that fires when the prefix canonicalizes fine
but the selected tool's path arguments can never be confined; this fires when
the prefix itself cannot be canonicalized, regardless of the tool.)

**`command_prefix` is for shell renderings only.** A `command_prefix` rule
paired with a tool whose rendering is a structured JSON dump (every built-in
except `bash`) is rejected at load time and surfaced as a typed registration
error rather than installed as a silent inert rule — a JSON dump's token
boundaries are not something an operator can predict, so the rule can never
reliably match. You see each rejected rule as a transcript error (red) at
startup, naming the rule and the reason, so a refused rule is never dropped
silently. Use `when: "always"` or a `paths_under`/`categories`
condition for those tools instead.

The check resolves the `select` against the registered tools and counts
`Structured`- vs `ShellCommand`-rendering members, so it catches every select
shape, not just a single named tool:

- **All-Structured** (every selected tool renders `Structured`) — including a
  single named tool, a multi-tool list, a trailing-`*` wildcard that resolves
  to only `Structured` tools, and a `Select::Categories` whose members are all
  `Structured`. The rule is fully inert, so the loader refuses to install it
  and surfaces a typed `CommandPrefixOnStructuredTool` registration error
  (operator-visible at startup and on `/trust permissions`). Split the rule
  or use `when: "always"` for the `Structured` members.
- **Mixed-kind** (the select resolves to at least one `Structured` and at least
  one `ShellCommand` member, e.g. `{"tools":["bash","read"]}`) — the
  `ShellCommand` members install and match as written; the `Structured`
  members are inert. The rule is NOT refused (rejecting the whole rule would
  discard the working `ShellCommand` members), and it is NOT installed
  silently (the inert `Structured` members would hide). The loader installs
  the rule and surfaces a notice naming the inert `Structured` members, so
  the operator can split the rule if they meant them to match. Unknown tools
  (a name no registered tool answers to, e.g. a plugin tool loaded later)
  are skipped, not errored — a load-order hazard is not a misconfigured rule.

**The `allow`/`deny` asymmetry, explained, not merely stated:**

- **An `allow` rule from a *project* file requires an explicit trust
  decision before it does anything.** `allow` is authority, and a project
  file is authored by whoever controls the checkout — which, for a cloned
  repository, is not you. Installing its `allow` rules unconditionally would
  let a clone auto-grant itself pattern-based tool permissions the moment
  you open it, with no consent given at any point. So a project file's
  `allow` half is parsed and held, but only *installed* once its exact bytes
  match a trust decision you recorded on purpose (see Trust, below) — until
  then you see a transcript notice naming the file and how many rules are
  waiting, and every call it would have covered asks you directly instead.
  A global file needs no such ceremony: it is your own file, and asking you
  to trust your own file is theater that teaches you to click through
  prompts instead of reading them.
- **A `deny` rule applies immediately, from *any* file, trusted or not.** A
  rule that can only ever narrow what is authorized has no failure mode
  worth gating on trust — the worst case of installing it unconditionally is
  an extra prompt, never a missed one. So a safety rule works the moment it
  is written to either file, before you have made any trust decision at all.
- **A plugin may only NARROW (`deny`/`prompt`); an `allow` is operator-owned.**
  An `allow` rule is a durable grant of authority, and grants belong to the
  operator, not to code the operator did not write. So a plugin-contributed
  rule with `then: "allow"` is refused at the broker boundary with a typed
  `false` — it never enters the active allow store, regardless of which
  transport contributed it. A plugin `deny`/`prompt` rule installs
  unconditionally, exactly like one from any other file: narrowing has no
  failure mode worth gating. The invariant rests on a structural guard at
  the broker, not on the absence of a plugin transport, so a future transport
  that reuses the plugin origin to call the allow path hits a structural
  refusal rather than silently installing a grant the operator never
  authorized.

## The `prompt` rule effect

A third rule kind, symmetric with `deny` in every way that matters for
trust (no scope, no ceremony, applies from any file immediately) but with a
different effect: a matched `prompt` rule doesn't deny the call, it forces
it to reach the ordinary permission gate — skipping the cache, every pattern
grant (including one that would otherwise have matched the same call), and
`AutoAllow` — so you are asked *every time*, not just the first time. It is
the mechanism a plugin (or a hand-written rule) uses to say "this class of
call deserves a human look every time, not the first time," and it forces
the gate in every mode, `AutoAllow` included, and over a matching `allow`
pattern grant. This was verified directly against the real production
path (`Conway::load_permission_files` → `PermissionBroker::decide`, not a
hand-built fixture): `crates/conway-runtime/tests/permission_broker.rs`'s
`a_prompt_rule_forces_the_gate_under_auto_allow` and
`a_prompt_rule_forces_the_gate_over_a_matching_allow_pattern_grant`, and
`crates/conway/tests/permission_prompt_seam.rs`'s seam-level counterparts,
all pass against the shipped broker.

## Trust

Trust is keyed on `(absolute path, content digest)` — **never on a
directory.** There is no "trust this folder" operation anywhere in conway.
Trusting `.conway/permissions.json` records a digest of the exact bytes you
trusted; the moment those bytes change — a `git pull`, a hand edit, anything
— the digest no longer matches and the file is untrusted again.

**This happens silently, without prompting.** No modal fires, at startup or
ever, when a trusted file's content changes; it simply stops taking effect
for its `allow` half (its `deny` half, if any, keeps applying regardless —
see the asymmetry above) until you trust it again. This is deliberate: a
prompt that fires on every `git pull` trains you to press `y` without
reading it, which turns the prompt into a latency tax rather than a
control — and any protection that depends on someone actually reading the
twentieth identical modal has already failed. Silent de-trust costs you
nothing but degraded convenience (more prompts) until you look at what
changed and decide again.

**Granting trust deliberately.** The only path that ever writes a trust
record is `/trust permissions`, typed on purpose — never automatic, never a
side effect of starting a session or of anything else. It trusts the
project-scoped candidate file at its *current* bytes and installs its
current `allow` rules immediately, for this session as well as the next.

**Reviewing what a file would install.** conway shows you nothing before
you trust a file — no diff, no preview, no listing of the rules it would
add. `/trust permissions` trusts and installs in the same action; the only
report you get is afterward, in the transcript: `trusted
.conway/permissions.json -- 2 allow rule(s) installed for this session, and
will load automatically until its content next changes`. If you want to
know what you're about to authorize, read the file yourself first — it is
plain, diffable JSON for exactly this reason.

**Load-time, not continuous.** The trust check runs when a session starts
(`Conway::load_permission_files`) and again, for that one file, whenever you
run `/trust permissions` — never on a timer, never re-verified per tool
call. A consequence worth knowing: editing a file's `deny` rules mid-session
adds nothing until the next session starts (they load once, at startup,
regardless of trust); and editing an *already-trusted* file's `allow` rules
mid-session silently de-trusts it as above, but the rules already installed
from its earlier, still-trusted content keep working for the rest of this
session even though the file is no longer trusted for the *next* one.

## Confinement

These two flags are easy to conflate, and mixing them up is the mistake
most likely to cost you real damage — read this before you set either one.

- **`--cwd <DIR>`** sets the process's (and the root agent's own) working
  directory: where the agent *works*, and where a relative tool argument
  starts from. It is **not** a security boundary. It never limits what a
  tool call can reach — an agent given `--cwd /home/alice/project` can
  still read or write `/etc/passwd` if a tool call names that absolute
  path.
- **`--root <DIR>`** confines the root agent — and, by inheritance, every
  subagent it forks or spawns — to that directory: any tool call whose
  path argument resolves outside it is denied before your permission gate
  is ever consulted. This **is** the security boundary. A subagent can
  only narrow its inherited root further, never widen it.

Omit `--root` and the agent is **unconfined**: it can reach anywhere your
user account can reach, exactly like every invocation before this flag
existed. Set `--root` whenever you want a hard guarantee that conway
cannot touch anything outside a directory tree, regardless of what a tool
call asks for or what permission you grant it.

**When you set `--root`, also pass `--cwd` as an absolute path.** conway
must be able to verify the agent's own working directory sits inside the
root before it will start; a relative `--cwd` (or no `--cwd` at all, which
leaves the working directory at its default) can't be checked against the
root and conway refuses to start rather than guess:

```console
conway --cwd /home/alice/project --root /home/alice/project
```

The same setting exists for a library embedder as
`ConwayBuilder::with_root`.

**Confinement outranks every grant.** The root check runs first, before the
deny rules, the prompt rules, the cache, every pattern grant, and
`AutoAllow` — a call whose path argument resolves outside the root is
denied before any of those are even consulted, and a call the root
mechanism cannot statically verify (e.g. `bash`'s free-form command
argument) is forced to the ordinary gate rather than silently allowed, even
under `AutoAllow`. Verified live: with `--root` set to a project directory,
a `read` call naming a file inside it succeeded; the identical call naming
an absolute path one directory above the root was denied before it ever
reached a permission decision, with no prompt shown at all —

```console
$ conway --cwd ~/project --root ~/project --allowed-tools read -p \
    'use the read tool to read the file at exactly this absolute path: /path/outside/secret.txt'
conway: warning: tool call proposed: read (call_v5auu9s6)
conway: warning: permission denied for call call_v5auu9s6
```

— against the identical call for a file inside the root, which ran and
returned its contents normally.

## Inspecting and revoking

`/settings`'s permissions group shows the active mode and three sections:
**allow**, **deny**, and **prompt**. They are separate sections because
they compose by different rules (deny beats everything except root
containment; prompt narrows below deny; allow grants) — one
undifferentiated list would misrepresent the model.

The **allow** section lists every active *pattern* grant (`allow` rules —
from `[p]`, or installed from a permissions file), each labeled with its
origin: `[interactive]` for one you granted through the prompt, or the
originating file's path for one loaded from `permissions.json`. Selecting
one grant's row and pressing `Enter` revokes exactly that grant; a
separate "revoke all grants" row clears every pattern grant *and* every
`[a]`-style cached "allow always" decision at once.

Structured allow rules — the `rules`-array form, like
`{"select": {"tools": ["read"]}, "when": {"paths_under": "/repo"}, "then": "allow"}`
— appear in the same allow section, alongside the flat rows and in the
same shape, and are individually revocable the same way: select the row,
press `Enter`, and exactly that rule is removed (from the session *and*
from the `rules` array of the file it came from — the flat `allow` list in
the same file is untouched, and vice versa). A structured rule granted at
a scope narrower than the whole session (an embedding application's
per-agent or per-subtree grant) says so on its row with a `scope:` note;
session-wide grants, the only kind the TUI itself creates, carry no note.

The **deny** and **prompt** sections list every active deny and prompt
rule — flat and structured alike — each labeled with its origin the same
way. This list matters precisely *because* these rules install from every
permissions file, trusted or not: a cloned repo can ship a deny or prompt
rule that takes effect the moment you open the project, and this is the
surface that shows you which rule — and which file — is gating or refusing
a call. Their rows are read-only (the cursor skips over them; `Enter`
does nothing because they are never selected), by design. Both kinds only
ever narrow what's authorized, most come from a file you don't control
(or read as carefully as your own `allow` list), and a safety rule
offering a one-keystroke way to remove it would be the wrong shape for a
rule whose entire job is to be hard to evade — even by your own accidental
keypress. To change one, edit (or delete, or untrust-by-editing) the file
it came from; the change takes effect at the next session start.

A few things worth knowing about what this surface does and doesn't cover:

- **An `[a]` "allow always" decision has no individual row.** It is not a
  pattern grant — it is an exact-call cache entry, invisible to
  `active_permission_patterns()` — so you cannot see or revoke it one at a
  time; "revoke all grants" is the only way to clear it, and doing so
  clears every other cached "always" decision along with it.
- **`deny` and `prompt` rules are listed but never revocable from the
  menu**, by the design above — visible, with origin, but not actionable.
- **Revocation never fails open.** The in-session grant is dropped first,
  unconditionally; only afterward does conway try to remove the rule's wire
  form from the file it came from (tmp-then-rename). If that file write
  fails, the rule is still gone for this session — you're told the
  persistence failed and that it may come back at the next restart, never
  told a plain "done" that the file would go on to contradict.
- **Revoking a rule from a trusted project file re-trusts the rewritten
  file** (the removal changes its bytes, which would otherwise silently
  de-trust it per the mechanism above) — this is the one case where conway
  writes a trust record automatically, because the rewrite is itself the
  direct, on-purpose result of an action you took narrowing authority you
  already trusted, never an automatic side effect of anything else.

## Limits

What this mechanism does *not* guarantee, stated as plainly as what it
does:

- **Deny-by-prefix is a seatbelt, not a boundary.** A `deny` rule matches a
  literal prefix of the rendered command. `deny bash:git push` does **not**
  catch `foo; git push` — the rule never claimed to parse shell, and a
  semicolon is real, visible shell syntax the rule simply wasn't asked to
  look past. What keeps the composition sound anyway is the *allow* side's
  own, separate gate: for a tool whose rendered form a shell would actually
  interpret (`bash`, the only built-in of that kind), a command carrying a
  shell metacharacter (`;`, `&`,
  `|`, backtick, `$(`, a redirect, a brace) can never be satisfied by a
  pattern grant regardless of what patterns exist, so a chained command
  falls through to whatever the mode does without a grant — a prompt in
  `Prompt` mode, silent execution in `AutoAllow`. (`Plan` mode never gets
  this far: it denies a `bash` call outright, before any grant or mode
  fallback is consulted.) (The gate is
  scoped to shell renderings on purpose: a structured tool's JSON rendering
  always carries `(){}`, and no shell ever parses it — see the rules-file
  section above.) But a `deny`
  rule you were counting on to block something outright can still be walked
  around by chaining it onto something else. Anything that must never
  happen at all belongs in the confinement root, not in a `deny` prefix.
- **A trusted plugin runs with your full privileges.** conway's only
  extension mechanism today is in-process (an `Arc<dyn Plugin>`, built-in or
  supplied by whoever assembles the `Conway` you're running) — filesystem,
  network, credentials, the ability to exec, everything the `conway`
  process itself can do, a plugin can do too, with no sandbox around it and
  nothing special about the built-in tools (`bash`, `read`, `write`, `edit`,
  `cd`)
  that would set them apart from a third-party one. There is currently no
  on-disk, digest-checked ceremony for trusting a *plugin* the way one
  exists for a `permissions.json` file — the only trust decision that
  exists at this level is whichever code wired the plugin in at all, made
  once, in Rust, before the process starts. It is binary: a plugin you're
  running has everything, or it isn't running.
- **The trust-digest check on a permissions file is load-time, not
  per-invocation.** It runs once when a session starts, and again for one
  file when you explicitly run `/trust permissions` — never on a timer,
  never re-verified before any individual tool call. A file that changes
  underneath a running session does not retroactively pull back what it
  already granted; it only affects what installs at the *next* session
  start (see "Load-time, not continuous," above).
- **An agent def's (or `conway_fork`/`conway_spawn`/`conway_ask`'s) `tools`
  selects what is announced to the model, not what it may execute.** A
  narrow `tools` list — on an `AgentDef`, or passed to `conway_fork`,
  `conway_spawn`, or `conway_ask` — keeps a tool out of the schema list the
  model is shown for that turn, so it is far less likely to be *proposed*.
  It is not a capability boundary: `ToolRunner` resolves a proposed call by
  name against the whole registry, with no selector in the picture at all,
  so if a tool is registered and a call for it somehow reaches the runner,
  the permission gate — not the selector — is what decides whether it runs.
  A `tools` argument passed to `conway_fork`/`conway_spawn`/`conway_ask`
  makes this concrete: it *replaces* whatever the child would otherwise
  inherit (a def's own selector, if any) rather than narrowing it, so it can
  name a tool an inherited def excludes. What actually bounds what a call
  can do is the permission gate (this page) and the confinement root
  (`--root`, above) — never the announcement list.

See also [`getting-started.md`](getting-started.md) for installing conway
and configuring a provider, and [`interactive.md`](interactive.md) for the
full TUI reference, including where the permission prompt and `/settings`'
permission group actually live on screen.
