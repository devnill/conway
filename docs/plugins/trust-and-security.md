# Trust and security

What an author is trusted with, and what conway does and does not protect
against. This is where the set stops being polite about it: the point of
this page is that a limit gets the same prominence as a guarantee, because
an author who learns a security limit only after building on the opposite
assumption is exactly the failure this set exists to prevent.

Depends on [`concepts.md`](concepts.md) for vocabulary (plugin, hook, trust
subject, capability) — this page does not redefine those terms, and its
"Trust, in one paragraph" section is the short version of everything below.
[`docs/permissions.md`](../permissions.md) is the operator-facing reference
for the mechanics this page assumes (modes, the rules file, `/trust
permissions`, confinement); this page cites it rather than restating its
contracts, and adds the security posture underneath them.

## What trust is

Trust attaches to a specific subject — never to a directory. There is no
"trust this folder" operation anywhere in conway's design or its code. A
project appears in a trust decision as a *key* that selects which subjects
apply to it, never as a grantee in its own right: trusting a project with no
subjects recorded grants nothing.

**The full design's subject is `(kind, id, content-digest)`** — `kind`
distinguishing what's being trusted (a `permission_file`, eventually a
`plugin`), `id` distinguishing multiple subjects of the same kind, and
`content-digest` pinning the exact bytes the decision covers.

**What actually ships is narrower, and it is real, tested code, not a
forward declaration of the full triple.** `TrustStore`
(`crates/conway/src/config/trust.rs`) implements exactly one kind —
`permission_file` — keyed on `(absolute path, content digest)`, with the
`kind` tag and the `id` axis flattened away because there is only one kind
to disambiguate today. This is the same gap `concepts.md`'s own "Trust"
section and `docs/permissions.md`'s Limits section both already state: *"no
on-disk, digest-checked ceremony for trusting a plugin the way one exists
for a `permissions.json` file."* This gap is tracked, and whether to close
it now is deliberately left open rather than decided here: generalizing
`TrustStore` to the full `(kind, id, digest)` subject today, with a
`plugin` kind nothing can yet consume, would be exactly the "capability
with nothing behind it" this project's own preference for thin,
demonstrable slices over speculative generality warns against.

**A subprocess plugin (`[plugins].subprocess`,
[`subprocess-plugins.md`](subprocess-plugins.md)) now IS a real, loadable
off-process artifact — the premise that used to make a `plugin` kind
"nothing behind it" no longer holds verbatim.** Building the trust kind
anyway is still explicitly out of scope: board item `01KZHVFCN6ZEAXV7K5JHRQN1YB`
is under a STANDING OPERATOR DEFERRAL and is marked DO NOT EXECUTE. What
this leaves, stated plainly rather than left implicit: naming a command in
`[plugins].subprocess[]` is checked against **nothing** — no digest, no
allow-list, no prompt — on the exact footing `[hooks].rules[].command`
already has. If and when the deferral lifts, the shape to build is: a
`plugin` trust-subject kind in `TrustStore` keyed on `(entry_digest,
artifact_digest)` — two digests, because an artifact digest alone covers
only the named entrypoint file, and an interpreter entrypoint whose real
code sits in an adjacent tree would defeat a single-digest check. Gate it
behind whatever loads a plugin off-process, not before.

**A persistent subprocess plugin (board item `01M03VJHG1WFECFJB4ZH3CKWDX`,
`"transport": "persistent"`) is a larger exposure, not a larger capability
grant.** A persistent subprocess plugin holds a long-lived, unsandboxed
process the operator named in config — the same footing as a
`[hooks].rules[].command`, held for longer, with the larger exposure that
implies. An operator who would not paste an unknown command into a hook
rule should not paste one into a persistent subprocess plugin entry
either. The child can do exactly what the one-shot child could do — it
just does it for longer, accumulates state across calls, and can fail in
new ways (die mid-session, write a partial frame, stall on a blocked
pipe); none of those are trust-mechanism gaps, they are liveness/safety
problems the persistent transport's failure handling solves (see
[`subprocess-plugins.md`](subprocess-plugins.md)'s "The persistent
transport" section). The deferred digest-keyed `plugin` trust subject
above addresses a DIFFERENT threat — verifying the binary on disk is the
one the operator reviewed — that is identical for one-shot and persistent;
going persistent does not change the digest-trust calculus, so it does
not need the deferred mechanism to land safely, and this transport builds
no parallel trust mechanism of its own.

**Why the directory form is rejected.** A directory-scoped trust decision
made about `/repo` stays valid for whatever `/repo` becomes — a `git pull`
that changes the committed `permissions.json` rides the trust decision made
about a completely different file's bytes. That stickiness is Claude Code's
own documented flaw, and it is the shape
that lets *one* component's content change silently re-open the trust
question for *every other* component nested under the same path. Keying on
content digest instead means an edit to the trusted bytes is, by
construction, a different subject — there is no way for a change to ride an
old decision.

## The asymmetry, and why it is the sound part

- **Allow requires trust.** A project-scoped `allow` rule is authority, and
  a project file is authored by whoever controls the checkout — for a
  cloned repository, that is not the operator. `PermissionFile::allow`
  entries from a project file are parsed and held but never installed until
  their exact current bytes match a recorded trust decision
  (`crates/conway/src/config/trust.rs`, `TrustStore::is_trusted`).
- **Deny always applies**, trusted or not, from any file, immediately. A
  rule that can only narrow what is authorized has no failure mode worth
  gating on trust — the worst case of installing it unconditionally is an
  extra prompt, never a missed one (`docs/permissions.md`'s "Rules in
  `permissions.json`" section states the identical asymmetry from the
  operator's side).

A cloned repository can therefore ship a `deny` or `prompt` rule that takes
effect the instant you open the project — that is intended, and is the
*safe* direction of the asymmetry — but it cannot grant itself anything
without you separately, deliberately, trusting it.

## De-trust is silent, and that is deliberate

Editing a trusted file's bytes changes its content digest, and
`TrustStore::is_trusted` simply stops matching. Its `allow` half stops
installing (its `deny`/`prompt` half, unaffected, since neither ever needed
trust). **No prompt, ever, on this path.**

The obvious question is "why doesn't it just ask me?" — and the answer is
that a prompt firing on every `git pull` trains the operator to press `y`
without reading it, which turns the prompt from a control into a latency
tax. Any design whose safety depends on a human actually reading the
twentieth identical modal of the week has already failed. The safe
outcome — de-trust — has to require **zero** human action, precisely
because a design that required one would eventually not get it.

**What the operator sees instead, as actually shipped**, is narrower than
the full trust-model design: a one-line transcript notice
naming the file and how many rules are waiting, and a report line after you
run `/trust permissions` (`trusted .conway/permissions.json -- 2 allow
rule(s) installed for this session...`). The design describes the review
surface an operator opens on purpose as showing a *diff against the trusted
digest* rather than a bare yes/no — **that diff view does not exist in the
tree today.** `/trust permissions` trusts and installs in the same action,
with no preview beforehand; `docs/permissions.md`'s own "Reviewing what a
file would install" section says this plainly: *"conway shows you nothing
before you trust a file — no diff, no preview, no listing of the rules it
would add... If you want to know what you're about to authorize, read the
file yourself first."* Treat the diff-review surface as decided design, not
shipped behavior, until it lands.

**Re-trusting** is `/trust permissions`, typed on purpose — never automatic,
never a side effect of starting a session or of anything else
(`docs/permissions.md`'s "Trust" section).

## What conway does NOT protect against — state bluntly

> **conway does not sandbox the plugin process.** A trusted plugin runs as a
> subprocess with the operator's full privileges: their filesystem, their
> network, their credentials, their ability to exec. The decision to trust
> is the entire control at that level, and it is binary — a plugin you are
> running has everything, or it is not running.

This is not a hedge and it is not buried in a non-goals list: it is stated
here, in full, because it is the one thing an author or an operator must
never learn after the fact. `docs/permissions.md`'s Limits section states
the identical thing from the operator's side (*"A trusted plugin runs with
your full privileges... with no sandbox around it and nothing special about
the built-in tools... that would set them apart from a third-party one"*).

**The distinction that makes the capability vocabulary honest: capabilities
govern what a plugin can make *conway* do — never what it can do to the
machine.** That is why `fs.read`, `net`, and `exec` are deliberately absent
from the capability vocabulary (`concepts.md`'s glossary; the design's own
statement, in the extension design). Naming them
would manufacture a false belief: an operator reading `net: none` in a
review surface would reasonably conclude a plugin cannot reach the network,
and conway has no mechanism that could make that true for an in-process
`Arc<dyn Plugin>` or an out-of-process subprocess alike. **A
declared-but-unenforced capability would be documentation sitting in a
control's slot, and that is worse than no documentation at all.**

Two things this bears on directly, so a reader does not have to infer them:

- **`PluginManifest::required_host_caps` is now consumed at registration.**
  The `conway` builder consults the field (the manifest-validation seam) and
  refuses a plugin whose declared cap the host does not offer with
  `PluginError::MissingHostCapability`. The cap set is a closed
  `HostCapability` enum (`subagent`, `persistent_transport`), not a
  free-form `Vec<String>` the host never validates; empty means "needs
  nothing the host might lack." A declared cap the host lacks refuses the
  plugin at build, before it is ever invoked -- a narrowing gate, not an
  enforcement mechanism against a loaded plugin (this page's own
  "declared-but-unenforced capability is worse than no documentation" line
  still applies to anything beyond this gate: conway does not police what a
  loaded plugin does, only whether the host can offer what it declared it
  needs).
- **No self-reported-intent field exists in the tree today.** The design
  proposes a segregated `disclosures` map on a future manifest
  — free text like `"network":
  "calls api.example.com to classify commands"` — rendered under a header
  stating verbatim *"Self-reported by the plugin. Conway does not verify or
  enforce these."* `PluginManifest` (`crates/conway-core/src/ports/
  plugin.rs`) has no such field yet. If one is added, it must ship with
  that header from the same commit — a disclosure without the caveat reads
  as a guarantee.

## TUI slash commands: no permission gate at all, by design

`docs/plugins/hooks.md` point 15 (`Plugin::commands()`/`Command::invoke`)
is a DIFFERENT trust shape from a `Tool` call, worth stating separately
because a reader could otherwise assume it inherits the same gate.

**A `Tool` call is proposed by the model and passes through
`PermissionGate`/`PermissionBroker` before it runs — a `Command::invoke` is
typed directly by the OPERATOR and runs immediately, with no gate at all.**
There is nothing to gate: the operator who just typed `/acme.greet` is the
same trust boundary a bare shell alias or a script in their `$PATH` already
crosses with no prompt, every time, and a slash command is held to the
identical standard, not a stricter one. This is consistent with — not an
exception to — this page's opening claim: **installing** the plugin is
the entire control (§"What trust is"); once installed, both its tools and
its commands run with the operator's full privileges, a tool gated per call
because the MODEL proposed it (an untrusted-in-the-small-sense caller even
inside a trusted plugin), a command not gated at all because the OPERATOR
proposed it directly, the same way typing a command in a shell they already
trust needs no per-invocation confirmation.

**What this narrows the blast radius to, deliberately.** `Command::invoke`
receives a [`CommandCtx`] carrying only read-only agent-identity fields
(`focused_agent`, `root_agent`, `session_id`) and the raw text the operator
typed after the command word — no live `Conway`/`SessionHandle`, no
filesystem/network/exec capability beyond whatever the plugin's own process
already has as an ordinary program (the same "conway does not sandbox" limit
stated above, restated: this trust model does not narrow the MACHINE-level
capability, only conway's OWN domain objects). A command cannot steer any
agent, read or write a file through conway's own mediation, or reach the
permission broker — see `hooks.md` point 15's own doc for why (a
`conway-core`/`conway` layering constraint, not a policy choice held back
deliberately), and note that gap is itself disclosed there as a finding,
not a control: nothing stops a command's OWN Rust code from doing arbitrary
I/O outside conway's mediation entirely, exactly as a tool's `invoke`
already can.

**Two narrow exceptions, and why neither widens this control past "the
operator could already do this by hand."** A command CAN ask the host to
fork its OWN calling session at a sequence and drive the child
(`CommandOutcome::ForkSession` — `hooks.md` point 15's own "Forking the
calling session" subsection has the full mechanism). Neither this nor
`MaskRecord` below is a live handle: the command never touches
`Conway`/`SessionHandle` itself, only RETURNS a request the host — still
the one actually holding the trusted facade — chooses to honor. And
`ForkSession`/`MaskRecord` cannot reach past the session invoked from: the
returned request carries no session identifier of its own, so there is
nothing for a command, malicious or buggy, to name a foreign session with.

**Updated (board item 01KZY8QRAVVVKCRBZ6HAEGW3GG): a command CAN now name a
DIFFERENT session, through `CommandOutcome::Checkout` alone.** `Checkout {
target }` is the one variant of the three where `target` is a `SessionId`
read straight from the command's own typed argument, not the invoking
session — see `hooks.md` point 15's "Masking a record and checking out
another session" subsection for the full mechanism and why the widening was
judged necessary (`/checkout <session>` cannot be expressed any narrower
and still do what it names). This still does not reach past what an
operator sitting at that command could already do by hand: `Checkout`
grants no capability to READ, steer, or mutate `target` — the host's only
response to it is `Conway::fork_from(target, target's own head, ..)`, the
identical zero-copy, append-only fork any operator could trigger themselves
via `conway --fork-from <id>` if they knew `target`'s id (which the command
was, by construction, typed with — an operator who did not already know
`target` could not have invoked `Checkout` against it in the first place).
The trust boundary this section opens with is otherwise unaffected: this
merely lets a plugin offer operator-privileged, already-possible actions
(rewinding one's own session, masking a record in it, or hopping to a
DIFFERENTLY-NAMED session one already knows the id of) as a named command
instead of a manual multi-step workaround.

## Backends and routers: the same install pass, and one hands over more

Everything above states the plugin case by name. It applies unmodified to
a `BackendFactory` and a `RouterFactory` — this page just hasn't said so
until now, and a reader who reaches [`docs/providers.md`](../providers.md)
first and never lands on this page would have no way to know it.

**Same mechanism, same privileges, no separate scrutiny.** conway's CLI
resolves a `Plugin`, a `RouterFactory`, and a `BackendFactory` id in one
pass off one list — `plugins.install`, unioned with `plugins.
default_backends` for the backend arm specifically
(`crates/conway-cli/src/first_party_plugins.rs`'s `install`; see
[`docs/providers.md`'s "Where a backend is
declared"](../providers.md#where-a-backend-is-declared) for the backend
side of that union). A library embedder reaches the identical surface by
calling `ConwayBuilder::with_backend_factory`/`with_router_factory` — the
same channel `with_plugin` uses, not a more guarded one. Nothing above
this section — the trust asymmetry, the unsandboxed-subprocess statement,
the capability vocabulary's deliberate silence on `fs`/`net`/`exec` — is
scoped to a tool plugin specifically; a `BackendFactory` and a
`RouterFactory` are plugins in every sense this page means the word, and
everything stated above about what conway does NOT protect against
applies to both, unmodified. Naming a kind in configuration (or
registering a factory in code) is the entire admission control; there is
no per-backend or per-router review surface distinct from the one plugin
review surface described throughout this page.

**A `BackendFactory` is additionally handed a credential it never asked
for.** `BackendFactory::build` receives a `BackendBuildContext`
(`crates/conway-core/src/ports/backend.rs`) whose `api_key` field is not
an environment variable's *name* — it is the resolved value: a literal
`backends.<id>.api_key`, or an `api_key_env` variable the harness has
already read out of the operator's own process environment, before
`build()` is ever called. The operator authorizes this once, structurally,
by adding a `backends.<id>` entry that names the factory's kind — never
per call, never as a runtime prompt, and with nothing in the review
surface that distinguishes "this entry hands over a secret" from any
other configuration key. `BackendBuildContext` also carries `extra`
(`BTreeMap<String, serde_json::Value>`) — every key an entry sets beyond
the five conway recognizes (`kind`, `api_key`, `api_key_env`, `base_url`,
`dialect`, `stream_tools`), captured verbatim and handed over uninspected
(`docs/providers.md`'s ["Where a backend is
declared"](../providers.md#where-a-backend-is-declared) documents the
field; a typo in one of the five named keys is silently captured here
instead of caught at load). A tool `Plugin` has no equivalent
operator-authored channel today: `ToolCtx::config` is a `PluginConfig`
constructed as `PluginConfig::default()`
(`crates/conway-runtime/src/runtime.rs`) — always empty, never populated
from `settings.json` or anything else an operator writes. Between `extra`
and `api_key`, a third-party `BackendFactory` receives operator-authored
configuration a tool `Plugin` has no channel to receive at all — on top
of, not instead of, the identical unsandboxed-subprocess privileges every
plugin already carries.

**A `RouterFactory` does not receive a raw credential the same way — the
exposure is narrower, not absent.** `RouterBuildContext`
(`crates/conway-core/src/ports/routing.rs`) carries `routing`, `headroom`,
`capability_index`, and `backends: &[Arc<dyn Backend>]` — already-built
`Backend` *handles*, never the strings that constructed them. A router
factory cannot read an `api_key` out of its own context the way a
`BackendFactory` can. It can, however, call `generate`/`stream`/`probe` on
any backend in that slice, which reaches the provider using that
backend's already-resolved credential without the router factory ever
holding the credential's bytes itself. Same install pass, same
unsandboxed process privileges, a real but strictly smaller version of the
same exposure a `BackendFactory` carries — worth naming here rather than
left as a silent assumption a reader has to derive for themselves.

**Writing one.** [`docs/providers.md`'s "Writing your own
adapter"](../providers.md#writing-your-own-adapter) is the authoring
surface — the crate boundary, publishing a kind id, a complete worked
example, and the obligations conway cannot check because no test in this
tree can verify a third party's own code.

## What conway DOES ship

"No complex sandboxing" is not "no isolation." conway does not acquire
seccomp, landlock, or a container runtime — that is a large dependency this
project won't take on casually, and a permanent maintenance surface conway
has decided not to carry. What it does ship is a containment primitive, orthogonal to what the
platform already provides: a confinement root, conway's chroot analogue.

The split that matters: **cwd is *where I am* — freely mutable, never a
security boundary. Root is *what I can reach* — parent-set, narrowing only.**
`--cwd` sets where a relative tool argument starts from and nothing more; an
agent given `--cwd /home/alice/project` can still read or write
`/etc/passwd` if a tool call names that absolute path. `--root` is the actual
boundary: any `conway.fs` tool call (`read`/`write`/`edit`/`cd`/`glob`/`grep`)
whose path argument resolves outside it is denied, and a subagent forked or
spawned from a confined root can only narrow that root further, never widen
it. Root-agent confinement (the `--root` CLI flag, `ConwayBuilder::with_root`
for a library embedder) landed as a later item —
before it, only a subagent could be confined, never the root agent a human
actually talks to. The full mechanics, including the exact ordering against
every other permission step and a verified end-to-end transcript, are
`docs/permissions.md`'s own "Confinement" and "Limits" sections; this page
does not repeat them.

**Where this primitive lives has moved.** A decision made 2026-08-07 ruled
that the boundary should relocate from a harness-level check ahead of the
permission gate into the tool that performs the operation.
**That relocation has landed:** `conway.fs` now enforces its own root, read
from per-agent plugin config, over its own `read`/`write`/`edit`/`cd`/
`glob`/`grep` — open-relative (`conway_tools::fs::beneath`), so the
containment check and the actual filesystem open are one syscall sequence,
closing the TOCTOU window a check-then-act split left open before. The
harness-level `PermissionBroker::check_root` no longer walks a declared
path argument at all; a `read`/`write`/`edit`/`cd`/`glob`/`grep` call
therefore reaches the permission gate BEFORE `conway.fs`'s own check runs
(the gate may say yes to a call `conway.fs` still refuses afterward) —
different ordering from before, same outcome: the call never actually
executes outside the root. `bash`'s `command` remains outside every root
check, harness- or plugin-level (see below); its OWN `cwd` argument is
still checked directly by `PermissionBroker`, ahead of the gate, because
`bash` belongs to a different plugin with no containment mechanism of its
own to delegate to.

**One more limit worth naming plainly, because it bears directly on the
"full privileges" statement above: confinement narrows what a call can
*reach*, never what a trusted plugin *is capable of*.** A subprocess plugin
running under the operator's account is not made safer by `--root` — `--root`
governs the harness's own tool-call reach checks, which a plugin, running
with the operator's own privileges, is not obligated to respect at all. Root
confinement and plugin trust are two separate controls at two separate
layers; neither substitutes for the other.

**A plugin's `tools` selector is not a capability boundary either, and this
generalizes past plugins specifically.** An agent def's (or
`conway_fork`/`conway_spawn`/`conway_ask`'s) `tools` argument selects what is
*announced* to the model for a turn — it is prompt economy, not an
enforcement point. `ToolRunner` resolves a proposed call by name against the
*whole* registry; `ToolBatchCtx` carries no selector at all. **The permission
gate and the confinement root are the capability boundary** — a narrow
`tools` list makes a tool less likely to be *proposed*, never impossible to
*execute* if a call for it somehow reaches the runner (ruled and landed; `docs/permissions.md`'s
Limits section states the identical rule).

## Known limits

Stated next to the guarantees, per this set's house style, not softened:

- **Deny-by-prefix is a seatbelt, not a boundary.** A `deny` rule matches a
  literal prefix of the rendered command; `deny bash:git push` does not
  catch `foo; git push` — the rule never claimed to parse shell.
  `docs/permissions.md`'s Limits section has the full statement, including
  what keeps the *composition* sound anyway: not a metacharacter scan any
  more (that scan read a call's text to judge what it might do, which
  conway's own steering rules out — see `PHILOSOPHY.md`'s "Constraining a
  child: its tool set" and `docs/permissions.md`'s Limits section) but the
  fact that a durable `allow` pattern grant does not exist for `bash` (or
  any shell-rendered tool) at all any more, for any command, chained or
  not. **Correction to this item's own originating spec:** it named the
  convergence of conway's three control-character sanitizers ("F2/F3") as pending and instructed that it
  be labeled designed-not-built here. That is stale. The item is **done** —
  a single `conway_core::text::sanitize_control_chars`
  (`crates/conway-core/src/text.rs`) now backs both the `rendered` seam
  every `PermissionRequest.rendered` value passes through and
  `ToolOutcome::error`'s construction, replacing what were previously three
  independent hand-copies. `docs/plugins/hooks.md`'s point 6 documents this
  same correction; it is not restated as a live limit here.
- **The trust-digest check is load-time, not per-invocation.** It runs when
  a session starts and again, for one file, on `/trust permissions` — never
  on a timer, never re-verified before an individual tool call. For a
  *permission file*, the trust-model design's TOCTOU section calls this
  gap benign: rules are parsed and installed once at load, there is no
  reload path, and a file edited mid-session cannot install new rules into
  the running session — only the next session start is affected. For a
  *plugin artifact*, once the `plugin` trust kind above exists, the same
  document calls the identical gap real: an attacker able to replace the
  artifact file in the window between digesting it and executing it runs
  untrusted code under a trust record computed over different bytes.
  Closing that properly means digesting a held file descriptor and executing
  that same descriptor so check and use refer to one inode — worth doing
  once there is a plugin process to protect, not before. Per-invocation
  re-digesting is deliberately not the fix: it puts a filesystem read and a
  hash on every tool call's hot path to close a window an attacker with
  write access to your plugin binary has better ways to exploit.
- **A content digest covers the named file, not what that file's own code
  does.** An interpreter entrypoint whose real logic lives in an adjacent
  tree — a shim script that `import`s the actual payload from a sibling
  module — defeats a digest scoped to the entrypoint alone, regardless of
  when the digest is computed. This is a limit on the *design* for the
  not-yet-built `plugin` trust kind (the trust-model design, open
  question 6); today's `TrustStore` digests a `permissions.json`'s own
  bytes directly, which has no adjacent-tree indirection to exploit, so this
  limit does not yet apply to anything shipped — it is recorded here so it
  is not forgotten by the time it does.
