# Built-in tools

conway ships twelve built-in tools across four plugins. This page is the
reference table: every tool's name, what it does, its **category** (what
[`permissions.md`](permissions.md)'s plan mode and category-based rules
select by), its **permission class** (`Safe`, `Requires approval`, or
`Dangerous` — what the permission prompt shows and what `AutoAllow` still
has to answer to for a non-`Safe` call), whether its path arguments can be
confined by `--root` (see [`permissions.md`](permissions.md#confinement)),
and its `TruncationPolicy` (how an oversized result is cut down before it
reaches context). Use this page to decide what to name in
`--allowed-tools`/`--deny-tools`, what a `PermissionGate` should default per
category, or what exact tool name a `deny` rule needs.

For the permission *mechanism* itself — modes, the prompt, pattern grants,
`permissions.json`, trust, confinement — see [`permissions.md`](permissions.md).
For fork/spawn semantics, result contracts, and the rest of the delegation
model, see [`agents.md`](agents.md).

## Which tools are actually registered

Only three of the four plugins install by default: `conway.fs`,
`conway.subagent`, and `conway.report`. **`conway.shell` (`bash`) does not.**
Arbitrary shell execution is conway's most dangerous built-in, and getting it
requires a deliberate opt-in — add `"conway.shell"` to `tools.builtin_plugins`
in `settings.json`, or call `ConwayBuilder::with_builtin_plugins` directly.
See [`getting-started.md`](getting-started.md#enabling-bash-shell-commands)
for the exact config and the one-shot-mode exception (`-p` never auto-registers
bash either way — it's always gated by `--allowed-tools`). Every table below
still lists `bash`; "registered by default" and "safe to run" are different
questions, and this page answers neither by omission.

## The `fs` tools (`conway.fs`, on by default)

| Tool | Does | Category | Path arguments confinable | Truncation | Permission class |
| --- | --- | --- | --- | --- | --- |
| `cd` | Changes the agent's working directory for subsequent tool calls, effective next batch. | Move | Yes (`path`) | None | Safe |
| `read` | Reads a file, `cat -n` style, with offset/limit windowing. | Read | Yes (`path`) | Head (65,536 bytes) | Safe |
| `write` | Replaces a file's entire contents, creating it (and parent directories) if needed. | Edit | Yes (`path`) | None | Requires approval |
| `edit` | Replaces one literal, byte-exact substring in a file. | Edit | Yes (`path`) | None | Requires approval |
| `glob` | Finds files matching a glob pattern under a search root, gitignore-aware. | Search | Yes (`path`) | Head (32,768 bytes) | Safe |
| `grep` | Searches file contents for a regex pattern under a search root, gitignore-aware. | Search | Yes (`path`) | Head (32,768 bytes) | Safe |

Every `fs` tool declares exactly one path argument (`path`), so `conway.fs`
evaluates it the same way for all six: a call resolving outside `--root` is
denied. This check runs INSIDE `conway.fs` itself now (open-relative,
closing a symlink-swap race a separate check-then-open step could not) —
not ahead of your permission gate, so the gate may still be consulted
first; the call is refused regardless of what the gate answers.
`glob`/`grep`'s `pattern`/`glob` arguments are search expressions, not
paths, and are never handed to a root check.

### Tilde expansion

Every `path` argument above (and `bash`'s `cwd` — see below) goes through
one shared resolver, and that resolver expands a leading tilde: exactly `~`
resolves to the process's home directory, and a leading `~/` resolves the
rest of the path against it. This is *anchored* — the model can write
`~/notes/todo.md` and get its home-relative meaning — not a substring
replace: a `~` anywhere else in the argument (an ordinary filename
character, or the middle of a later path component) is left exactly as
written, never rewritten.

A path that begins with `~` but that conway cannot honour — no home
directory could be determined, or the argument uses a form conway does not
expand (e.g. `~alice/notes`, the `~user` shorthand some shells support) —
is refused with an error that names tilde explicitly, rather than a generic
"file not found" that gives the model nothing to diagnose. This is the same
resolver a `paths_under` permission rule's prefix uses (see
[the structured `rules` array](permissions.md#the-structured-rules-array)),
so a rule and the call it bounds never disagree about where a `~`-prefixed
path lands.

## The `shell` tool (`conway.shell`, opt-in — see above)

| Tool | Does | Category | Path arguments confinable | Truncation | Permission class |
| --- | --- | --- | --- | --- | --- |
| `bash` | Executes a command with `bash -c`, streaming stdout/stderr and killing the whole process group on cancellation or timeout. | Execute | **No — see below** | HeadTail (15,000 + 15,000 bytes) | Dangerous |

**`bash` is not confinable the way the six tools above are, and this table
would lie by omission if it looked identical to them.** Its `command`
argument is a free-form string handed to a real shell verbatim — a shell
command reaches any path it likes via redirection, substitution, `cd`, or a
subprocess, so there is no finite set of shapes a root check could scan for
and conclude "safe." `bash`'s `command` is therefore **always**
`Unconfinable`: a root check can never statically clear it, and the call
always falls through to your permission gate regardless of `--root`. Its
`cwd` argument (a one-off working directory for that single call) is the one
part of a `bash` call that *is* a declared path and *is* checked against the
root like any other. `--root` plus a tool set that excludes `bash` is the
real, load-bearing guarantee here — see
[`permissions.md`](permissions.md#confinement) for the full boundary and a
live example denying a `read` outside a root, which is the shape of
guarantee `bash`'s presence in the tool set breaks. conway warns at startup
when both are set — `--root` (or `ConwayBuilder::with_root`) alongside
`bash` among the registered tools — rather than leaving this only as prose
on this page.

## The `subagent` tools (`conway.subagent`, on by default)

| Tool | Does | Category | Path arguments confinable | Truncation | Permission class |
| --- | --- | --- | --- | --- | --- |
| `conway_fork` | Forks this agent into a new child continuing its full context, plus a directive. | Delegate | N/A — no path arguments | Tail (16,384 bytes) | Dangerous |
| `conway_spawn` | Spawns an independent child with none of this agent's context. | Delegate | N/A — no path arguments | Tail (16,384 bytes) | Dangerous |
| `conway_ask` | Forks an ephemeral child and returns its full reply text. | Delegate | N/A — no path arguments | Tail (16,384 bytes) | Dangerous |
| `conway_steer` | Sends a text message to a running child, landing at its next turn boundary. | Delegate | N/A — no path arguments | Tail (16,384 bytes) | Requires approval |
| `conway_await` | Blocks for a child agent's terminal result. | Delegate | N/A — no path arguments | Tail (16,384 bytes) | Safe |
| `conway_cancel` | Cancels a running child, immediately or gracefully. | Delegate | N/A — no path arguments | Tail (16,384 bytes) | Requires approval |

`conway_fork`/`conway_spawn`/`conway_ask` are `Dangerous`: starting a child
hands it the ability to make its own tool calls, transitively — the same
risk class as `bash`, one hop removed. None of the six declares a path
argument at all (a model-invoked fork/spawn/ask always inherits the
caller's own `cwd` and root, unchanged), so `PathArgs::None` rather than
`Unconfinable` — there is nothing here for a root check to evaluate, checked
or not. See [`agents.md`](agents.md#a-model-tool-call) for the full
fork-vs-spawn semantics, result contracts, and what a subtree can and can't
reach.

## The `report` tool (`conway.report`, on by default)

| Tool | Does | Category | Path arguments confinable | Truncation | Permission class |
| --- | --- | --- | --- | --- | --- |
| `report` | Declares this agent's terminal result: a bounded summary, optional facts, optional artifacts, optional structured output. | Think | No | None | Safe |

`report` takes no top-level path argument, but each entry in its
`artifacts` array carries an optional `path` — a real filesystem path,
nested inside an array rather than a top-level field. `PathArgs::Named`
only names top-level argument keys, so it cannot express a nested path
either; `report` declares `Unconfinable` with nothing checkable, and a call
always falls through to your permission gate. That artifact path is
metadata the agent asserts about its own output — `report` itself never
reads or writes it.

## Truncation policies

The vocabulary an oversized tool result can declare, and what the runtime
actually does with each one:

| Policy | Effect |
| --- | --- |
| `None` | No truncation. The full result reaches context regardless of size. |
| `Head { max_bytes }` | Keeps the first `max_bytes` bytes, drops the rest. |
| `Tail { max_bytes }` | Keeps the last `max_bytes` bytes, drops the rest. |
| `HeadTail { head_bytes, tail_bytes }` | Keeps the first `head_bytes` and the last `tail_bytes`, drops the middle. |

A truncation is a context-affecting event: the runtime records it in the
session log, not just applies it silently.

An earlier `Artifact` variant — spill the full result to a file, keep a
pointer in context — was declared but never implemented: nothing ever
constructed it, and the runtime handled it identically to `None`, the
inverse of what it promised. It has since been removed from the enum
entirely rather than fixed, so it
is not in the table above because it no longer exists as an option — no
built-in tool, or any tool, can declare it. `TruncationPolicy` stays
`#[non_exhaustive]`, so a future variant is possible; this table reflects
what's reachable today.
