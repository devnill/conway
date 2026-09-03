# `conway.confine`: a bash tool the OS itself confines

The confined-shell plugin (harness gap review 2026-09-01, decision
`01M1FQG08GDQ71984T0W0RJ019`), shipped by `crates/conway-plugin-confine`.
Depends on [`concepts.md`](concepts.md) for vocabulary and on
[`trust-and-security.md`](trust-and-security.md) for the broader trust model
this plugin sits inside — this page covers only what is specific to this
one plugin: what it confines, what it does not, and the exact primitive it
invokes per OS.

## What this is for

Approving every shell command by hand trains an operator to stop reading.
Approving them all at once — a session `AutoAllow`, or a broad pattern grant
— with the ordinary `bash` tool (`conway.shell`) means one bad command can
write anywhere your user account can. `--root` (`docs/permissions.md`'s
"Confinement" section) confines every OTHER built-in tool's path arguments,
but `bash` runs a free-form shell command verbatim, which reaches any path
it likes via redirection, substitution, `cd`, or a subprocess — no static
check on the command TEXT can confine it. `conway.confine`'s one tool,
`confined_bash`, closes that gap the only way it can actually be closed:
handing the whole command to the operating system's own containment
primitive, which enforces the boundary regardless of what the command says.
This is what makes blanket approval of a confined shell honest — the
guarantee is the kernel's, not conway's reading of your command.

## Mechanism, not policy (P-14)

**This plugin never reads a command to decide anything.** There is no
allow/deny list, no metacharacter scan, no path-extraction heuristic over
`command` — the whole string goes to `/bin/bash -c` verbatim, wrapped in the
primitive's own argv. The containment guarantee comes entirely from the OS:
Seatbelt (`sandbox-exec`) on macOS, Linux namespaces (`bwrap`) on Linux. A
`command` this plugin cannot statically confine (exactly the reason `bash`
itself declares `PathArgs::Unconfinable`) is not a gap this plugin tries to
close by reading text more cleverly — it is the reason a RUN-time OS
boundary is the only honest answer at all.

## What this confines, and what it does not

**Writes only.** The ruling this plugin implements is explicit: a
confinement root stops a filesystem WRITE from landing outside it. It does
**not** restrict:

- **Reads.** `cat /etc/hosts`, or any other read reachable to your user
  account, still succeeds through `confined_bash` exactly as it would
  through plain `bash`.
- **Network.** A command that dials out is not restricted by this plugin at
  all — neither profile below unshares or filters network access.

If you need those confined too, this plugin does not give you that today —
see "What this does not build" below.

## The exact primitive invoked, per OS

### macOS: `sandbox-exec`

```
sandbox-exec -p '<profile>' /bin/bash -c '<command>'
```

where `<profile>` is built fresh for every call:

```scheme
(version 1)
(allow default)
(deny file-write*)
(allow file-write* (subpath "<root>"))
```

Read literally: `(allow default)` starts from nothing restricted (reads and
network stay open); `(deny file-write*)` then blanket-denies every
filesystem write; `(allow file-write* (subpath "<root>"))` re-opens writes
only under the confinement root. Seatbelt evaluates rules in file order and
the last matching rule for a given operation+path wins, so a write under
`<root>` matches both the deny and the later, narrower allow, and the allow
wins; a write anywhere else matches only the deny.

### Linux: `bwrap` (bubblewrap)

```
bwrap --ro-bind / / --bind <root> <root> --dev /dev --proc /proc \
  --die-with-parent -- /bin/bash -c '<command>'
```

`--ro-bind / /` mounts the entire host filesystem back over itself,
read-only; `--bind <root> <root>` re-mounts the confinement root over the
same path, read-write, restoring write access exactly there. `--dev /dev`/
`--proc /proc` give the sandboxed shell a working `/dev`/`/proc` (bwrap's own
default, with neither bound, is an empty namespace that breaks an ordinary
shell). `--die-with-parent` stops an orphaned sandboxed process from
outliving this tool's own cancellation/timeout handling. No
`--unshare-net`/`--share-net` flag is passed — network stays exactly as
reachable as it would be unsandboxed, per the ruling above.

## Verified on

**macOS only, in this tree.** `conway-plugin-confine`'s own in-crate test
(`#[cfg(target_os = "macos")]`) exercises the real `sandbox-exec` profile
above: a write inside a confinement root succeeds and the file exists, a
write outside it fails with the file absent, and a read outside it (`cat
/etc/hosts`) still succeeds. The Linux `bwrap` profile is implemented on the
identical shape but is **not exercised by this tree's own CI** — its mirror
test (`#[cfg(target_os = "linux")]`) skips, printing the reason, when
`bwrap` is not installed on the machine running the suite. Declaration
honesty (GP-14): treat the Linux path as a designed, not-yet-independently-
verified-here implementation until a CI runner with `bwrap` actually
exercises it.

## No fallback to unconfined execution, ever

- **No root configured for this agent** (`--root` was never set, or a
  fork/spawn child's own root config was cleared) — every call is refused,
  naming `--root`, never run unconfined.
- **The primitive binary is missing at plugin construction** — installing
  `conway.confine` on a machine with no `sandbox-exec`/`bwrap` at the
  expected path is a named `ConwayBuilder::build` config error, not a
  silent degrade.
- **The primitive binary has gone missing by call time** (removed out from
  under an already-running process) — the call is refused, never run
  unconfined.

## What this does not build

- **No command-text inspection of any kind.** Never a fallback to reading
  `command` and deciding anything from it — see "Mechanism, not policy"
  above.
- **No Landlock, no `unshare`.** This slice implements exactly two
  primitives (`sandbox-exec`, `bwrap`); a finer-grained Linux sandbox
  (Landlock) or a hand-rolled `unshare` wrapper is a labeled absence, not a
  future promise.
- **Reads and network stay unconfined**, by the ruling this plugin
  implements — see "What this confines, and what it does not" above. A
  future item that wants those confined too is a genuinely separate
  capability, not an extension of this one.
- **`conway.shell`'s own `bash` tool is unchanged**, beyond gaining a
  pluggable launcher seam this plugin reuses (`BashTool::with_launcher`,
  `conway-tools`' own doc). Installing `conway.confine` does not disable or
  alter `bash` — the two are separate tools, separate plugins, and an
  operator installs either, both, or neither.

## Installing it

```json
{ "plugins": { "install": ["conway.confine"] } }
```

`confined_bash` requires `--root` (`ConwayBuilder::with_root`, or the CLI's
own `--root <DIR>`) — the same confinement root `conway.fs` itself enforces
`read`/`write`/`edit`/`cd`/`glob`/`grep` against, read from the identical
per-agent `conway.fs.root` config key so the two tools can never disagree
about the boundary. A call with no root configured for its agent is refused
outright.

First-run guided setup offers `confined_bash` FIRST, ahead of the plain
`bash` question, whenever it detects a containment primitive already
installed on your machine — see `docs/getting-started.md`'s own "Enabling
bash (shell commands)" section for the full first-run flow.

See [`docs/permissions.md`](../permissions.md#confinement) for how `--root`
and the root+unconfinable-shell-tool warning interact, and
[`docs/tools.md`](../tools.md) for `confined_bash`'s full row in the
built-in/first-party tool table.
