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
[`subprocess-plugins.md`](subprocess-plugins.md)) and an MCP-over-stdio
plugin (`[plugins].mcp`, [`mcp.md`](mcp.md)) are now both real, loadable
off-process artifacts — the premise that used to make a `plugin` kind
"nothing behind it" no longer holds verbatim.** Board item
`01KZHVFCN6ZEAXV7K5JHRQN1YB` was reopened on exactly that basis (decision
`01M0R4RWCDJJ6RMNVFYCNHW0NK` lifted the 2026-08-12 standing deferral) and
worked to a conclusion: **a `plugin` trust-subject kind was considered and
DECLINED**, not left open for lack of a consumer this time.

The reasoning. Both transports' own crate docs state, deliberately, that a
plugin's `command` sits on the identical footing as
`[hooks].rules[].command` — full, unsandboxed operator privilege either
way, with the operator's own review as the only control point in both
cases. A load-time digest check on a plugin's entrypoint file *would* be a
real, honest integrity primitive — digest equality is a decidable claim it
can actually keep, unlike the shell-metacharacter blocklist this page's own
"Known limits" section below records as a cautionary tale (see "Deny-by-
prefix is a seatbelt, not a boundary"): that scan tried to infer *safety*
from a command's text and could not deliver it, which is why it was removed
rather than tightened. But gating `[plugins].subprocess[].command` and
`[plugins].mcp[]`'s command with one, while leaving
`[hooks].rules[].command` permanently ungated, would not shrink the actual
threat: both surfaces already grant full, unsandboxed process privilege,
and the extra in-conway capabilities a plugin can additionally declare
(tools, hooks, curators) are dominated by that privilege rather than
adding to it — this page's own "capabilities govern what a plugin can make
conway do, never what it can do to the machine" argument, turned on
itself. A digest check that exists for one of two identically-privileged
surfaces and not the other would read as "plugins are vetted, hooks are
not," which is false, and would be exactly the kind of declared control
sitting in a control's slot that this page's "declared-but-unenforced
capability is worse than no documentation" line (below) warns against —
except here the control WOULD be enforced, just enforced selectively
enough to imply a distinction that is not real.

**Requirement 5 (design note `01M0R3D57PDXCWM5TC6KX851YW`) — plugins
appending durable records to the log — does not change this conclusion for
either transport that ships today.** That capability is reachable only
through the `Curator` port (`CurateCtx::store: Arc<dyn SessionStore>`,
which carries `append`), and neither `conway-plugin-subprocess` nor
`conway-plugin-mcp` overrides `Plugin::curators()` — both use the
trait's default (empty) implementation, so no out-of-process plugin can
reach a `SessionStore` at all today. Writing durable records remains an
IN-PROCESS-only capability, on the identical trust footing as any other
compiled-in `Arc<dyn Plugin>` (see "What trust is", above: an in-process
plugin is trusted by whoever assembles the `Conway` that installs it, not
through `TrustStore`). If a future item wires a curator, or any other
durable-record-writing capability, onto an out-of-process transport, that
changes the grant this section evaluates, and the conclusion above should
be re-examined against it then — it is not re-examined here because the
capability does not exist on either shipped transport.

**What this leaves, stated plainly rather than left implicit:** naming a
command in `[plugins].subprocess[]` or `[plugins].mcp[]` is checked
against **nothing** — no digest, no allow-list, no prompt — on the exact
footing `[hooks].rules[].command` already has, and this is now a
considered position rather than an open gap. **If this project later wants
stronger integrity assurance for a named external command**, the honest
shape covers `[hooks].rules[].command` and every plugin transport's
`command` uniformly, in one mechanism — not a `plugin`-only kind bolted
onto `TrustStore` first. Such a mechanism would still need to key on
`(entry_digest, artifact_digest)` — two digests, because an artifact
digest alone covers only the named entrypoint file, and an interpreter
entrypoint whose real code sits in an adjacent tree would defeat a
single-digest check — but that shape is recorded here for whoever designs
the uniform mechanism, not built by this item.

**What would reopen the decline above, recorded at decline time so it would
not need re-deriving:** a transport that overrides `Plugin::curators()` and
can append durable, addressable records to the log — a materially larger
grant than running a tool, and the thing "Requirement 5" just above already
checked and found absent; a decision to gate `[hooks].rules[].command`,
which would remove the asymmetry objection that was the deciding argument;
or a plugin distribution story where an artifact arrives from somewhere
other than a path the operator typed.

**The third condition has been met and ruled on — decision
`01M0VS2M8FC25QYCATQ8PKQ73Y` (2026-08-25), superseding the 2026-08-23
decision above.** Planning marketplace-sourced plugin distribution put a
fetched artifact in front of the operator for the first time, meeting that
condition exactly. The ruling: **a plugin artifact conway fetches — from a
marketplace or any other distribution mechanism this project ever
builds — sits on the SAME footing as `[hooks].rules[].command`**: named by
the operator, run with the operator's full privileges, checked against
**nothing** — no digest, no allow-list, no prompt. The operator's decision
to install it IS the control point, exactly as it already is for a hook
command or for a command typed directly into `[plugins].subprocess[]`/
`[plugins].mcp[]`. A marketplace-sourced artifact is not safer than a
command path the operator typed by hand; both are checked against nothing,
and that is the point of this ruling, not an oversight in it. Only the
third condition above is retired by this ruling — the other two, curator
override and hook-gating, have not happened and remain live.

**This extends the 2026-08-23 decline rather than reversing it, and the
distinction is worth preserving because it is easy to lose.** The 2026-08-23
argument rested on asymmetry: a digest gate on a plugin's command while
`[hooks].rules[].command` stayed ungated "would read as 'plugins are vetted,
hooks are not,' which is false" — an objection a future decision to gate
both surfaces could defeat, which is exactly why it was named as the second
reopen condition above. The 2026-08-25 ruling supplies a second, independent
reason that gating both surfaces cannot answer: running third-party code
carries a risk that is inherent and expected, and a digest check does not
deliver the safety it appears to promise against that risk. Gating hooks
too would still leave unaddressed the thing that actually carries the
risk — the operator's choice to run someone else's code — which is why this
condition's retirement does not turn on whatever becomes of the other two.

**What this ruling does NOT cover, stated so its shape is not misread as
broader than it is.** It rules on exactly one mechanism — artifact digest
trust for a plugin, fetched or not — and is not license to weaken anything
else. The permission broker's fail-closed defaults, the rule that a `deny`
rule always applies while an `allow` rule may require trust (see "The
asymmetry, and why it is the sound part", below), and containment roots
enforced by the plugin performing the operation (see "What conway DOES
ship", below) all still apply, unmodified, to whatever a fetched plugin
does once it is running. The harness operating as safely as it can remains
a live constraint on everything except this one mechanism.

**Fetching a git-sourced entry is still a network trust boundary, stated
explicitly.** Board item `01M0Y6RYZA94BK6YXJ7X8TNEGR` (2026-08-29) gave
[`marketplace.md`](marketplace.md) a second fetch path alongside its
original per-file HTTP one: a real Claude Code marketplace's `git-subdir`/
`github` source is now fetched by invoking the SYSTEM `git` binary against
a URL the marketplace's own response names. The ARTIFACT trust question
this section already settled is unchanged by that — a git-cloned artifact
sits on the identical "checked against nothing, the operator's decision to
install is the control point" footing as an HTTP-fetched one, and this
paragraph does not reopen that. What is worth stating on its own, because
"conway now clones arbitrary third-party git repositories on operator
command" is a materially different-sounding sentence from "conway fetches
a JSON file" even though the underlying trust posture is identical:

- **conway shells out to the operator's own `git`, never a git library**
  (`git2` is not a dependency) — the child process runs with the
  operator's own credentials, SSH agent, and `.gitconfig`, exactly as if
  the operator had typed the clone by hand.
- **The URL a `git-subdir` source names is untrusted, network-supplied
  input**, and git's transport model has real teeth beyond "fetch a
  repository": `ext::<command>` and `fd::<n>` remote helpers can run an
  arbitrary command or open an arbitrary file descriptor rather than talk
  to a network remote at all. `conway-plugin-marketplace` refuses any
  `git-subdir` URL that is not `http://`/`https://` before `git` is ever
  invoked (an ALLOW-by-prefix, the inverse of this page's own
  "deny-by-prefix is a seatbelt, not a boundary" lesson below, applied
  correctly in that direction: everything not on the allow-list is
  refused, nothing is inferred past it) — this is the one place in this
  crate where refusing a whole CLASS of otherwise-syntactically-valid
  input is load-bearing, not merely tidy. A `git-subdir` URL whose
  authority embeds userinfo (`https://user:pass@host/...`) is refused the
  same way, before `git` is ever invoked: a legitimate public marketplace
  has no reason to embed a credential, and the credential would otherwise
  survive into an error message and into the operator-facing "fetched via
  git from {url}" install notice, both of which can be copied,
  screen-shared, or logged.
- **A git checkout can contain a symlink**, the one hazard class a
  files-map entry's own "no archive, so no archive-traversal" argument
  does not cover (a checkout is not an archive, so it needed its own
  check): the checked-out plugin root — the root itself, not only its
  descendants — is refused outright if it (or an intermediate path
  component leading to it) is a symlink, and the whole tree beneath it is
  then walked and refused if a symlink appears anywhere further down,
  before a single byte is copied into conway's own plugin store. A
  narrower version of P-10's symlink-in-an-extracted-archive hazard, not
  an absent one.

No new trust MECHANISM is created by any of this — no digest, no
allow-list, no prompt beyond the one install action's own disclosure,
unchanged from the ruling above. What changed is the SURFACE a
network-supplied value can reach (a subprocess argument, not only an HTTP
request path), and the three bullets above are what closes each concrete
way that surface could otherwise be abused, at the boundary, per P-10.

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
transport" section). The declined digest-keyed `plugin` trust subject
above would have addressed a DIFFERENT threat — verifying the binary on
disk is the one the operator reviewed — that is identical for one-shot and
persistent; going persistent does not change that calculus, so it is not
an argument for revisiting the decline above, and this transport builds no
parallel trust mechanism of its own.

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

**What the operator sees, as actually shipped**: a one-line transcript
notice naming the file and how many rules are waiting, and then, when you
run `/trust permissions`, a preview card showing the file's current
content — bottom-anchored over the transcript, the same modal idiom the
permission prompt and `/ask` use — BEFORE the trust decision, not after.
Only `[y]` (confirm) actually records anything; `[n]`/`Esc` walks away
having written nothing. This closes the gap this page used to describe
here: install and trust no longer happen in the same action with nothing
shown first.

**What remains narrower than the full trust-model design, stated
plainly rather than left implicit: this is a preview, not a diff.** The
design describes the review surface as showing *a diff against the trusted
digest*. `TrustStore` (`crates/conway/src/config/trust.rs`) never retained
the bytes of a PRIOR trust decision — only its digest — so there is nothing
on disk to diff the current content against, even for a file that changed
since it was last trusted. The preview card says so directly when that is
the case (`"this file changed since you last trusted it ... the previous
version is not retained, so it cannot be shown or diffed"`), rather than
implying a comparison it cannot produce. Building a real diff would mean
`TrustStore` starts retaining a copy of every trusted file's content — new
storage, new staleness questions, a materially bigger change than showing
what you are about to trust — and is left as a distinct, undecided future
step, not silently assumed here. `docs/permissions.md`'s own "Reviewing
what a file would install" section states the identical preview-not-diff
posture from the operator's side.

**One-shot (`conway -p`) has no preview surface at all, and needs none: it
has no trust surface of any kind.** `/trust permissions` is a TUI-only
slash command — `conway -p` never parses slash commands, never calls
`Conway::preview_trust_target` or `Conway::trust_permission_file`, and
never even reads `permissions.json`'s rules or `trust.json`: one-shot mode
builds its gate solely from `--allowed-tools`/`--deny-tools`
(`conway::gates::AllowListGate` — see this page's own note on the
one-shot gate being a different mechanism, above). A prior trust decision
made through the TUI still applies at ordinary session startup either way
(load-time, not per-invocation — see "Load-time, not continuous" in
`docs/permissions.md`'s own "Trust" section), including a one-shot run; what
cannot happen in one-shot mode is MAKING a new trust decision, because
there is no operator present to show a preview to and no code path that
would try.

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
  `PluginError::MissingHostCapability`. The `HostCapability` enum is now
  **open**: two core-blessed bare names (`subagent`, `persistent_transport`)
  plus a shape-checked, catch-all `Named` variant for anything else a
  plugin declares -- not a free-form `Vec<String>` the host never
  validates, since a malformed name still fails to parse; empty means
  "needs nothing the host might lack." A declared cap the host lacks --
  whether one of the two core names or an open, well-formed one -- refuses
  the plugin at build, before it is ever invoked -- a narrowing gate, not an
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

**Newer (board item 01M0VSMF71S6VXX81YRAAF5S8Q): a command can submit a new
turn, through `CommandOutcome::SubmitPrompt` alone.** `SubmitPrompt { text
}` puts `text` into the conversation exactly as if the operator had typed
it — see `hooks.md` point 15's "Submitting a prompt" subsection for the
full mechanism. This widens WHAT a command can cause (a new agent turn,
where before it could only act on already-persisted history), but not WHO
can cause it or what they could not already do by hand: the operator who
installed the plugin and typed the command word could have typed the exact
same text into the prompt box themselves in the next keystroke. The
distinguishing property from `ForkSession`/`MaskRecord`/`Checkout` above is
provenance, not privilege — the resulting turn is attributed to the
command that produced it (`Provenance::CommandPrompt`, `hooks.md`'s own
"Submitting a prompt" subsection), never disguised as the operator's own
typed text, so a reviewer of the durable log can always tell a
command-submitted turn apart from one the operator actually typed. Bound to
the invoking agent and session exactly like `ForkSession`/`MaskRecord`: the
returned request carries no session or agent identifier of its own, so
there is nothing for a command, malicious or buggy, to name a foreign
target with.

## Composing a context path: a gated tool, cross-session read, same-session write only

See [`hooks.md` point 18](hooks.md#18-context-path-composition--toolctxcontext_path-contextpathhost)
for the full mechanical contract; this section is its trust posture.

`ContextPathHost` (board item `01M0PEFMG96SVBBD5D2E06H34A`, decision
`01M0K4QT6MBXPD6PXMBBBD2P7B`) is a new extension point: a `ToolCtx` field
(`ToolCtx::context_path`) every dispatched tool receives, mirroring
`ToolCtx::subagents`'s own "caller-bound handle, never a raw host" shape.
It backs exactly one first-party consumer today, `conway.path`'s
`compose_context_path` tool — an ORDINARY `Tool`, proposed by the model and
gated through `PermissionGate`/`PermissionBroker` before it runs, on the
identical footing every other tool call in this page's opening section
already describes. Nothing about it skips the gate the way a `Command`
does.

**What it can read: any session's records, honestly.** Composing a path
from "what we discussed in that other session" needs to resolve a
cross-session `RecordRef`, so `ContextPathHost::resolve_records` — like
`CurateCtx::store` before it (a `Curator`'s own §11.5 read surface) — is
deliberately not confined to the calling session. This is the SAME
widening `CommandOutcome::Checkout` argues for above, applied to a
different mechanism: reading a record already logged somewhere in this
process is not a new capability an operator could not already exercise (any
record in the store is, definitionally, something conway itself already
produced), and the read is honest rather than a bypass — it resolves
through the same masked, ancestry-aware transcript resolution the ordinary
per-turn path assembly uses, so a record an operator excluded via
`ContextMask` stays excluded here too.

**What it can write: only the CALLING session's own head, never another's.**
`ContextPathHost::default_path`/`::set_head` are narrowed by
`ContextPathHandle` to the ONE session the invoking tool call belongs to —
there is no parameter through which a call could name a different session
to freeze a head onto, mirroring `SubagentHandle`'s identical "no `caller`
parameter to override" structural guarantee. A composed path can pull
CONTENT in from anywhere; it can only ever change what SESSION renders
next for the session that asked.

**No new store is exposed.** `PathStore` itself stays engine-internal, not
re-exported through `conway::plugin` (board item `01M0EMCK55628YJXGBQY8YGXHE`,
unchanged by this addition) — `ContextPathHost` is a narrow, purpose-built
capability an implementation backs with `PathStore`/`SessionStore`
internally, the same "narrow handle, not a raw port" shape `SubagentHandle`
established for fork/spawn. A plugin author reaches it only by calling
`ctx.context_path`'s methods; the trait and its production implementation
are never nameable from a crate depending only on `conway`.

**This section covers WRITING a path from a reference the model already
holds.** "Finding a session" below covers the other half: how a model gets
a reference to a session it neither started nor spawned in the first place.

## Finding a session: a gated tool, read-only, bounded

See [`hooks.md` point 20](hooks.md#20-cross-session-discovery--toolctxsession_discovery-sessiondiscoveryhost)
for the full mechanical contract; this section is its trust posture. See
"Composing a context path" immediately above for the tool this one feeds —
worth reading first, since the two share most of their argument.

`SessionDiscoveryHost` (board item `01M0PS8J3AK7Z7253Z3E3RD3GY`) is a new
extension point, structurally identical to `ContextPathHost` above: a
`ToolCtx` field (`ToolCtx::session_discovery`) every dispatched tool
receives. It backs exactly one first-party consumer today, `conway.discover`'s
`search_sessions` tool — an ORDINARY `Tool`, proposed by the model and gated
through `PermissionGate`/`PermissionBroker` before it runs, on the identical
footing every other tool call in this page's opening section already
describes.

**What it can read: this project's own sessions by default, every project's
under an explicit widening — never a record CONTENT read unless asked.**
`SessionSearchQuery::text` omitted means metadata only (which sessions
exist, when, labeled how — the SAME header-only information
`conway_session::SessionIndex` already keeps for every session store, never
a new index over record content). `text` supplied is a real content scan,
but bounded by `SessionSearchQuery::max_sessions` — the tool cannot be made
to read an unbounded number of records in one call, and its reply always
states how many it actually read. `scope: "all_projects"` is the ONE
explicit widening beyond the calling project: every project directory
under the central sessions root, resolved by one directory listing, never a
filesystem crawl and never a registry (see `hooks.md` point 20's own
"Reach" note).

**Content search does not re-check `ContextMask`, and that is not a new
hole.** `ContextMask` (`docs/plugins/hooks.md` point 15's `Checkout`
section) affects fork-PREFIX resolution only — what a CHILD session
inherits from a parent — never a session's own raw log. `search_sessions`
reads a session's own records directly, the same reads
`SessionStore::read`/`list` already permit any caller with a store handle
to perform; masking was never a redaction mechanism over a session's own
content and this tool does not change that. A masked record's `(session,
seq)` CAN still be found by a search and handed to `compose_context_path`
— which then, correctly, refuses to resolve it (`hooks.md` point 18's own
masked-read contract), reported back to the model as an ordinary "could not
resolve" failure, not a leak.

**What it can write: nothing, ever.** `SessionDiscoveryHost::search` has no
write path at all — unlike `ContextPathHost`, this port carries no
`set_head` counterpart. Finding a session changes nothing about what any
session renders next; only `compose_context_path` does that.

**No new store is exposed.** The production implementation
(`conway::discovery_host::FsSessionDiscoveryHost`) backs this capability
with the SAME `SessionStore`/`conway_session::discovery` machinery
`ContextPathHost`'s own implementation uses internally — nothing new is
re-exported through `conway::plugin`. A plugin author reaches it only by
calling `ctx.session_discovery`'s one method; the trait and its production
implementation are never nameable from a crate depending only on `conway`.

## Instruction fragments: text that reaches the model, with no gate either

`Plugin::instructions()` (board item `01M0K5MD59YZRSHE31JKZKFRMY`;
mechanism and obligations in [`hooks.md`](hooks.md) point 17) is a new
extension point, and it is worth naming carefully: it is the first
`Plugin` contribution whose entire effect is putting a paragraph of TEXT
directly into the model's context, as its own `Role::System` segment,
positioned ahead of an operator's own directory-authored skills.

**Installing the plugin is still the entire control — nothing new is
gated.** A tool call is proposed by the model and passes through
`PermissionGate`/`PermissionBroker`; an instruction fragment has no call to
gate at all — it is static text assembled once per turn, the same "nothing
to gate" shape this page's slash-command section states for
`Command::invoke`. There is no reachability-adjacent trust check either:
the reachability rule (`ContextBuilder::build` withholds a fragment naming
a tool id no installed plugin provides) is a CORRECTNESS mechanism against
an accidentally-stale fragment, not a security boundary — it says nothing
about whether the TEXT of a reachable fragment is honest, and enforces
nothing about what that text asks the model to do. A malicious or careless
plugin can declare a fragment whose text instructs the model to act against
the operator's interest, and nothing here catches that; it is the same
prompt-injection surface every other source of `Role::System`/inherited
context already carries, just with a shorter path to the top of the
prompt.

**This grants no capability a `ContextHook` did not already have.**
`ContextHook::before_request` can already add, edit, or drop ANY segment in
an assembled request (this page's own "What conway DOES ship" section, and
`conway.skills`'s own `SkillIndexHook`, prove it) — `Plugin::instructions()`
is a NARROWER, declarative way to do one specific thing `before_request`
could already do arbitrarily. Trust-wise, a plugin author who could already
inject arbitrary text via a hook gains nothing new here; what changes is
legibility (`/context`'s preamble section names which plugin a paragraph
came from) and structural reachability (the text ships and leaves with
`with_plugin`, per that method's own doc) — properties for the OPERATOR
inspecting what is installed, not new restrictions on what an installed
plugin's text may say.

## Plugin-to-plugin capability calls: a name is trusted, not an implementation

See [`hooks.md` point 21](hooks.md#21-plugin-to-plugin-capability-calls--plugincapabilities--capabilityprovider-conway_coreportscapability)
for the full mechanical contract; this section is its trust posture.

`conway_core::ports::capability` (board item `01M0WWNHQQYN1EVTH8WPZ33EBF`) is
a new extension point, and it is the first thing on this page that is not a
tool call, a command, or static text — it is one installed plugin calling
directly into another. `Plugin::capabilities()` lets a plugin register a
live `CapabilityProvider` under a namespaced `HostCapability` name; any
OTHER installed plugin reaches it through `ToolCtx::capabilities:
CapabilityCallHandle`, present on every dispatched `Tool::invoke`, and calls
it by that name.

**This is a genuinely different trust shape from every gated `Tool` call
described above, not a variant of one.** A `Tool` call is proposed by the
model and passes through `PermissionGate`/`PermissionBroker` before it
runs — an operator gate sits between the caller and the code that executes.
A capability call has neither: no model proposal step, no permission check,
nothing an operator sees before or after it runs. And unlike a `Tool` call,
which the model proposes by name but which the harness still resolves
through one fixed, build-time-registered tool registry the operator can
audit, **the calling plugin here names only a capability — a string — never
an implementation.** `CapabilityRegistry::call` matches that string against
whichever plugin registered a provider for it (`CapabilityRegistry::
from_registrations` refuses to build at all if two plugins claim the same
name — fail-closed, never "first wins"); the calling plugin has no way to
know or choose which plugin's code answers, only that something did.

**Installing one plugin can therefore cause a second, unrelated plugin's
code to run**, purely because the first named a capability the second
happens to provide. Nothing about the mechanism requires the two to be
authored by the same party, to know about each other, or to have been
reviewed together — an operator who has reasoned carefully about what
installing plugin A exposes them to now also has to reason about every
OTHER plugin installed alongside it, because A can reach into any of them
by name at any time A's own code runs.

**No built-in privilege — everything installed is a plugin, and a compiled-in
one holds no more standing than a downloaded one.** A `CapabilityProvider` registered by a
first-party, compiled-in plugin sits on the identical footing as one
registered by a downloaded third party — nothing about being linked into
the binary rather than installed from a marketplace changes who can call it
or what it is trusted with.

**What the harness hands a provider that it did not have to ask for:
nothing beyond the call itself.** `CapabilityProvider::call` receives
exactly the `serde_json::Value` payload the calling plugin sent — no
credential, no `ToolCtx`, no session identity threaded through by the host.
The exposure here is not an extra grant from the harness; it is that ANY
installed plugin, not merely the one the operator meant to authorize for a
given action, can trigger another's code, with the operator having approved
only "install this plugin," never "let this plugin be called by that one."
`CapabilityCallHandle::caller_plugin_id` is carried through for tracing and
audit only — it is never consulted to decide whether a call is allowed.

**No first-party built-in registers a capability by default today.** The
one shipped consumer is generic, not a specific grant: `conway-plugin-
subprocess` forwards `Plugin::capabilities()` for a configured subprocess
plugin that declares `provides` (`docs/plugins/subprocess-plugins.md`'s
"Providing a capability" section). This module registers no capability of
its own — it is the channel, exactly as its own module doc states.

**Versioning (decision `01M189XS6Z9VKYENAHNY1B54CM`) changes nothing about
this trust shape.** A provider now declares a `semver::Version` and a
consumer may check it via `CapabilityCallHandle::call_versioned`, refusing
with `CapabilityCallError::VersionMismatch` on a mismatch. That is a
COMPATIBILITY refusal, not an authorization boundary: it changes whether an
incompatible pair calls each other successfully, never who is allowed to
call whom, what payload a provider receives, or what
`CapabilityCallHandle::caller_plugin_id` is trusted with (still
tracing/audit only, per the paragraph above). An operator's trust review of
"what can this installed set of plugins reach into each other" is
unaffected by whether either side happens to pin a version.

## Plugin-registered hooks: a downloaded plugin's deny rule, at the operator's own tier, with structural provenance

See [`hooks.md` point 13](hooks.md#13-declarative-script-fired-hooks--the-hooks-configuration-block)
for the full mechanical contract; this section is its trust posture for the
`Plugin::hooks()` registration surface specifically (board item
`01M129QW0GV90QTQS6B3BY3DAR`).

Before this method existed, the only rules dispatched against a real tool
call or a submitted prompt came from `[hooks].rules[]` — something the
operator typed, or approved by trusting a project's `permissions.json`.
`Plugin::hooks() -> Vec<PluginHookRule>` changes that: a plugin, downloaded
and installed like any other, can now register its OWN `pre_tool_use` or
`prompt_submitted` rule directly, through the same `with_plugin` surface its
tools already use.

**It reaches the identical tier an operator-authored deny reaches — not a
softer one.** `ConwayBuilder::build` folds a plugin's returned
`PluginHookRule`s into the SAME `PreToolUseHookSpec` list a config-declared
`[hooks].rules[]` entry populates. For `pre_tool_use`, that means
`PermissionBroker::decide`'s hook-check step, checked BEFORE the mode gate,
the cache, pattern-allow grants, and the `AutoAllow` shortcut. **A plugin
you installed can therefore deny a tool call or refuse a prompt you typed,
under every permission mode including `AutoAllow` — the one mode with no
human in the loop to catch what it denied.** A hook can only narrow, never
widen: `HookPermissionVerdict` has no `allow` variant at all, by
construction, so nothing here lets a plugin grant a call the gate would
otherwise refuse.

**Provenance is structural — an active rule an operator cannot attribute or
revoke on its own is a trap, not a policy — not merely a comment an
operator may scroll past.** Every rule `Plugin::hooks()` returns is folded in carrying
`HookOrigin::Plugin(plugin_id)` — never `HookOrigin::Operator`, which
`ConwayBuilder::build` alone may set — so `Conway::
active_deny_capable_hook_rules`'s review list reports it as `"plugin
'<id>'"` rather than the operator-authored `"settings.json (merged
config)"` label. An operator inspecting active rules can tell which plugin
contributed a given deny, and — because it is a distinct, labeled entry —
revoke it independently of every other rule, rather than discovering only
that "some hook denied this" with no way to attribute or remove it on its
own. Before `HookOrigin` existed, every dispatched hook rule really did come
from `[hooks].rules[]`, so this field is what keeps that no-longer-true
claim from becoming a silent trap.

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

## `conway.statusline`: an unattended command, on the identical footing as a hook

[`docs/plugins/statusline.md`](statusline.md) is `conway.statusline`'s own
page (`StatusLinePlugin`, `crates/conway-plugin-statusline`) and states its
trust posture directly — read it, not a summary of it. This entry exists
only so this page names the surface too, in both directions, the way every
other extension point above does.

Naming a command in `[tui.status_line_command].command` runs it with the
operator's own process privileges — no sandboxing, no digest check, no
confirmation prompt, the identical footing `[hooks].rules[].command` and
`[plugins].subprocess[].command` already have on this page. What is
different about this one, worth stating here rather than left for the
reader to notice on their own: it is the first surface on this page that
runs **repeatedly and unattended, on a fixed schedule**, not once per event
or once per call — up to 60 process spawns a minute, for the life of the
process, with no per-run confirmation of any kind. An operator who would
not paste an unfamiliar command into `[hooks]` should not paste one into
`[tui.status_line_command].command` either, and should additionally weigh
that this one keeps running whether or not anything is watching it.

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
  *plugin artifact*, the same document calls the identical gap real: an
  attacker able to replace the artifact file in the window between
  digesting it and executing it runs untrusted code under a trust record
  computed over different bytes. Closing that properly means digesting a
  held file descriptor and executing that same descriptor so check and use
  refer to one inode — one of the reasons a plugin-specific digest check
  was declined for now rather than built half-strength (see "What trust
  is", above): a load-time-only check on a plugin artifact would deliver a
  narrower guarantee than "trusting a plugin" sounds like it promises, on
  top of the asymmetry-with-hooks argument that was the deciding one.
  Per-invocation re-digesting is not the fix either: it would put a
  filesystem read and a hash on every tool call's hot path to close a
  window an attacker with write access to your plugin binary has better
  ways to exploit.
- **A content digest covers the named file, not what that file's own code
  does.** An interpreter entrypoint whose real logic lives in an adjacent
  tree — a shim script that `import`s the actual payload from a sibling
  module — defeats a digest scoped to the entrypoint alone, regardless of
  when the digest is computed. This is a limit on the *design* for a
  possible future uniform command-integrity mechanism ("What trust is",
  above, and the trust-model design's own open question 6) — today's
  `TrustStore` digests a `permissions.json`'s own bytes directly, which has
  no adjacent-tree indirection to exploit, so this limit does not apply to
  anything shipped; it is recorded here so it is not forgotten if that
  mechanism is ever built.
