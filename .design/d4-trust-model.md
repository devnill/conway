# D4 — Trust model for non-Rust extensions

Status: design spec (board item 01KYNNB9E1YGC196GDGXRXFT4X). Written against
HEAD `e240a55` + the v0.6.0 tree. Transport is D1, extension points D2, wire
vocabulary D3, UI template language D5. **This document assigns trust to the
points D2 defines.**

One sentence: **trust is granted to a (kind, id, content-digest) subject, never
to a directory; a deny is always in force and an allow always requires trust;
a change to trusted content de-trusts silently rather than prompting; and the
capability vocabulary names only what the host actually mediates.**

---

## 1. Threat model

### What the attacker controls

The attacker authors a repository the operator clones and opens conway in.
They control every byte under the checkout, which today includes four
discovered configuration surfaces:

- `.conway/plugins.json` (D1 §6) — declares executables to spawn.
- `.conway/permissions.json` — **discovered and installed at `Session` scope at
  TUI startup, today, with no consent** (`crates/conway-cli/src/tui/app.rs:200-216`).
- `.conway/profiles.toml` — provider dialect behavior (landed today).
- `.conway/settings.json` — the merged config root.

They also control any artifact those files point at that lives in the repo, and
they control the source the model reads, so indirect prompt injection is
available to them as a *second* channel that can drive the model toward calling
whatever the first channel authorized.

### What the attacker does not control

The operator's global `~/.conway` (or `$XDG_CONFIG_HOME/conway`), the conway
binary, the operator's keystrokes, and — critically for §5 — any digest
recorded before the hostile content existed.

### What they can achieve today, with no plugins involved

`PermissionFile` has exactly one field, `allow`
(`crates/conway-core/src/permission_pattern.rs:428-433`). **The entire
project-scoped permissions file is a grant file**, it is loaded at startup, and
every rule in it is installed at `PermissionScope::Session`, which
`GrantScope::covers` answers `true` for any requester in the tree
(`crates/conway-runtime/src/permission.rs:239-241`). A cloned repo therefore
ships pattern grants into the operator's live session before the operator has
typed anything.

> **Status (2026-07-30), `d917ba2`.** Both premises here are gone. `PermissionFile`
> now has two fields, `allow` and `deny` (`crates/conway-core/src/
> permission_pattern.rs:697-710`) — §3 below asked for exactly this shape and
> got it. And a project file's `allow` rules no longer install unconditionally:
> they require a recorded trust decision first (`crates/conway/src/conway.rs`,
> `crates/conway/src/config/trust.rs`, wired at
> `crates/conway-cli/src/tui/app.rs`). §11's item 3 (stop defaulting to
> `Session` scope) is the one piece of this section's fix that is **still
> open** — everything else this paragraph describes has shipped.

The realistic blast radius is bounded but real: `PatternRule::matches` refuses
any command containing shell metacharacters
(`permission_pattern.rs:126-131`), and D2 §10 established that this makes
pattern grants structurally inert for every tool except `bash`. So the
achievable attack is a metacharacter-free `bash:` prefix grant —
`{"allow": ["bash:npm run build"]}` in a repo that also controls
`package.json`, or `bash:make`, or `bash:cargo test` in a repo with a
`build.rs`. That is arbitrary code execution with no prompt, from a clone, on
current `main`.

> **Status (2026-07-30), `68ea9b1` — this understated the threat, in the
> permissive direction, and that is the one direction a threat model must
> never get wrong.** "Structurally inert for every tool except `bash`" was
> true when written and false by the time this section's own fix (`d917ba2`)
> landed: `68ea9b1`, earlier the same day, gave every tool a `RenderKind`
> (`ShellCommand` | `Structured`) and taught `PatternRule::matches_render` to
> apply the metacharacter gate only to `ShellCommand` renders
> (`crates/conway-core/src/permission_pattern.rs:214-221`). A `Structured`
> tool's rendering is never a shell string, so gating it on shell
> metacharacters was rejecting `read:*` for the wrong reason and calling the
> rejection a security property. The pre-fix blast radius this paragraph
> describes was not "a `bash:`-shaped prefix in a repo that also controls
> `package.json`" — it was **every built-in tool's full grant surface**,
> `bash` included; the eleven non-`bash` tools were unreachable by *accident*
> (a rendering format that happened to trip the gate), not by design. A repo
> could not yet exploit that accident through `PermissionFile` before
> `d917ba2` closed the consent gap the same day, so the two bugs' windows did
> not overlap in a shipped release — but the document that was supposed to
> bound the blast radius bounded it to the narrower, wrong figure.

**This is the worked example of the threat model, it exists now, and §11
answers it.** Everything else in this document is the same problem with a
larger blast radius.

### What plugins add

A plugin config inverts the historical situation: until now, trusting a plugin
and trusting the binary were the same act, because plugins were compiled in. A
plugin declared in a file makes *cloning a repo* the act that decides what code
runs. The additional achievable outcomes are: arbitrary process execution as
the operator (a plugin is a subprocess, §6); participation in permission
decisions (D2's `permission.policy/1`); spawning agents that spend money and
run tools (§7); and injecting text into the model's context through a denial
reason (§8.3).

---

## 2. What trust is, and what it is not

**Trust is not a sandbox boundary, and conway is not acquiring one.** GP-08
places isolation in tools and in the deployment, not in the harness; P-7 keeps
the harness out of the containment business; C-04 forbids a new dependency
without justification, and a real sandbox (seccomp, landlock, a container
runtime) is exactly such a dependency plus a platform matrix conway does not
have. This design adds **zero new dependencies** — `blake3` is already a
workspace dependency and already used by the broker's `CacheKey`
(`permission.rs:219-223`).

The consequence must be stated bluntly rather than implied:

> A trusted plugin runs as a subprocess with the operator's full privileges:
> their filesystem, their network, their credentials, their ability to exec.
> Conway cannot and does not constrain that. **The decision to trust is the
> entire control at the process level, and it is binary.**

Everything §6 calls a "capability" governs what a plugin can make *conway* do —
not what it can do to the machine. That distinction is the difference between a
control and a decoration, and this document keeps it visible everywhere.

**The trust subject is `(kind, id, digest)`, not a directory.** There is no
"trust this folder" act. A project appears in the trust record as a *key*
selecting which subjects apply, never as a grantee. Trusting a project with no
subjects grants nothing. This is the structural fix for Claude Code's
directory-scoped boolean: de-trusting is granular, so one plugin's change does
not silently re-open the question for the others, and one plugin's change does
not drop the others.

Two kinds in v1:

| kind | id | digest covers |
|---|---|---|
| `plugin` | the manifest id | the plugin's declaring entry in `plugins.json` **and** the artifact it names |
| `permission_file` | the file's absolute path | the file's bytes |

**Explicitly considered and excluded: `profiles.toml`.** A hostile profile can
change wire behavior (`tool_call_style`, field naming) but `Profile` carries no
`base_url` — `chat_path` is a path relative to the operator's own configured
base URL (`crates/conway-backends/src/profile.rs:218-222`), so it cannot
redirect credentials to an attacker's host. Severity is "your requests get
malformed," not "your key leaves the machine." Adding it as a subject later is
a one-line addition to the `kind` enum; requiring it now would tax an operator
for a non-threat. Recorded so the exclusion is a decision, not an oversight.

---

## 3. The asymmetry: adopted, and completed

**Adopted from Claude Code, because it is the sound part:**

> Allow requires trust. Deny applies immediately, trusted or not. Declining
> trust leaves user-scope authority intact and still prompts.

The reason it is sound is worth stating in conway's own vocabulary rather than
borrowed: a permission system's failure modes are not symmetric. An
unnecessary prompt costs a keystroke; a missed one costs arbitrary execution —
which is precisely the argument `permission_pattern`'s module doc already makes
for its metacharacter gate (`permission_pattern.rs:35`). A rule that only ever
*narrows* has no failure mode worth gating, so gating it buys nothing and costs
the operator a safety net. A rule that *widens* is authority, and authority
requires consent. **A malicious repo must not be able to grant itself
permissions; a safety rule must work the moment it is written.**

**Completing it: conway has no deny half today.** `PermissionFile` is
allow-only. So adopting the asymmetry is not a matter of gating an existing
field — it requires adding one:

```jsonc
// .conway/permissions.json — the completed shape
{
  "allow": ["bash:cargo test"],   // requires trust; inert until then
  "deny":  ["bash:curl", "bash:ssh"]  // in force immediately, always
}
```

`deny` is additive and `#[serde(default)]`, so every existing file keeps
parsing unchanged.

> **Status (2026-07-30), `d917ba2`.** Implemented faithfully to this sketch.
> `PermissionFile` now carries `deny: Vec<String>`, `#[serde(default)]`
> (`crates/conway-core/src/permission_pattern.rs:704-709`), and project
> `allow` rules require a recorded trust decision while `deny` applies
> immediately regardless of trust, exactly as proposed above.

### The deny half's matching rule, and its honest limit

An allow rule refuses any command containing shell metacharacters
(`permission_pattern.rs:126-131`) because a prefix match on a chained command
authorizes the chain. **A deny rule must not inherit that gate** — inverted,
the gate would mean `deny bash:curl` stops `curl x` but not `curl x; y`, i.e.
adding a metacharacter would defeat the rule. So:

- **Deny compares the prefix without the metacharacter gate.** A metacharacter
  in the command disqualifies an *allow* and does not disqualify a *deny*.
- **Composition is D2 §7's most-restrictive-wins**: any deny beats every allow,
  independent of order or authorship scope.

> **Status (2026-07-30), `d917ba2`.** Implemented exactly as specified:
> `PatternRule::matches_deny` takes no `RenderKind` parameter at all and does
> not consult `contains_shell_metacharacters`
> (`crates/conway-core/src/permission_pattern.rs:232-233`), and
> `PermissionBroker::decide` short-circuits on the first matching deny rule
> immediately after the root check, ahead of every allow path
> (`crates/conway-runtime/src/permission.rs:649-654`).

The limit, said plainly rather than papered over: `deny bash:git push` does not
catch `foo; git push`. Prefix matching is not a containment boundary in either
direction. What makes the composition sound anyway is the *other* half — a
command containing metacharacters can never be auto-allowed, so the chained
form always reaches the human gate. **A deny rule is a seatbelt for the obvious
case, not a boundary.** Anything that must not happen belongs in the
confinement root or in a capability not granted. Overselling deny-by-prefix
would be the same mistake as `readOnlyHint`.

---

## 4. Where trust lives

### Decision: one global-only file, `<XDG or ~/.conway>/trust.json`

**Global scope only. Deliberately breaking the project-then-global precedent,
for a reason the precedent does not cover.**

`permission_file_paths` and `provider_profile_file_paths`
(`crates/conway/src/config/discovery.rs:65`, `:101`) are project-then-global
because they carry *content the project legitimately authors*. A trust record
is not content; it is a **decision about** that content, and a decision whose
subject can author it is not a decision. A project-scoped `trust.json` would let
the attacker trust themselves. There is no version of that which is safe, so
the layering does not apply here and the file has exactly one location.

Same directory resolution as everything else —
`xdg_config_path(env).parent().join("trust.json")` — so this is a third
consumer of machinery that already exists, not a new discovery paradigm
(GP-03). No env-var override, for D1 §6's reason: `settings.json` merges five
sources including env, and an env-injectable trust file is a trust bypass.

### Shape

```jsonc
{
  "version": 1,
  "projects": {
    "/abs/canonical/path/to/checkout": {
      "subjects": {
        "plugin:acme-linter": {
          "granted_at": "2026-07-30T18:04:11Z",
          "granted_by": "dan",
          "entry_digest": "blake3:9f2c…",     // the plugins.json entry
          "artifact_digest": "blake3:41ab…",  // the executable/script
          "caps": ["tool", "observe"],        // §6 — granted, not requested
          "may_allow": false,                 // §8.2
          "tool_class": {                     // §8.2 — operator classification
            "lint": { "category": "Read", "permission": "Safe" }
          },
          "timeout_ms": 20000
        },
        "permission_file:/abs/.../.conway/permissions.json": {
          "granted_at": "…", "granted_by": "dan",
          "content_digest": "blake3:c70d…"
        }
      }
    }
  }
}
```

Written **only** by an explicit operator action (§5). Never by conway
automatically, never by a plugin, never as a side effect of starting a session.

**Recommended, unix-only, cheap:** refuse to read a `trust.json` that is
group- or world-writable, the way `ssh` refuses a loose private key.
`std::os::unix::fs::PermissionsExt` is in std, so this costs no dependency
(C-04). Degradation on a non-unix host is a documented no-op, matching
`bash.rs`'s existing `#[cfg(not(unix))]` precedent.

### Scope determines load-trust; capabilities are always explicit

- A **global-scope** `plugins.json` entry is authored by the operator, so it is
  **trusted by authorship** and needs no record to load. Asking an operator to
  trust their own file is theater that teaches people to click through.
- A **project-scoped** entry requires a subject record to load. Absent → the
  plugin is not loaded.
- **Capabilities are explicit in both cases.** "I put this in my config" is not
  "I want it spawning agents." This is the same shape D2 already chose for
  `may_allow` (default `false` regardless of who registered the policy).

One rule, one line: **scope decides whether it loads; the record decides what
it can do.**

---

## 5. Re-confirmation: de-trust, never prompt

### The flaw being fixed

Claude Code stores trust per directory and does not re-prompt when the settings
file changes. A hostile edit to a trusted repo then runs under a decision the
operator made about entirely different content, and the documented mitigation —
"review diffs before pulling" — is advice, not a control.

The obvious fix (re-prompt on change) has its own failure mode, and it is
worse than it looks: **a prompt that fires on every `git pull` trains the
operator to press `y`.** At that point the prompt is not a control either; it
is a latency tax that manufactures consent. Any design whose safety depends on
a human reading the twentieth identical modal of the week has already failed.

### Decision: a change to trusted content de-trusts. It never prompts.

```
digest(subject) != recorded digest
   ⇒ subject is Untrusted{ reason: DigestChanged }
   ⇒ the plugin is NOT loaded / the allow rules are NOT installed
   ⇒ one notice line in the transcript, one row in /plugins
   ⇒ the session starts, degraded, and keeps working
```

**There is no modal on this path, at startup or ever.** The only
prompt-shaped surface in the whole design is one the operator opens on purpose,
and it shows a *diff against the trusted digest* rather than a yes/no. This is
the entire answer to the click-through failure mode: we do not ask, so there is
nothing to click through, and the safe outcome is the one that requires no
human action at all (P-10 applied to attention as a resource).

Re-trusting is an explicit act, out of band from starting work:

- `conway plugins trust <id>` / `conway trust permissions` (CLI)
- `/plugins` → select → review → trust (TUI)

Both open the review surface described in §9. First trust and re-trust are the
same act with the same surface; the only difference is that a re-trust shows a
diff and a first trust shows the whole thing.

### When the digest is checked — and the TOCTOU limit, stated

**The digest is verified at load, not per invocation.** A plugin is digested
when it is about to be loaded; permission rules are digested when the file is
read at session start. Neither is re-verified on every subsequent call.

That leaves a **time-of-check/time-of-use gap**, and it is worth naming
precisely rather than leaving a reader to assume it away — this is the same
shape as the confinement-root TOCTOU already stated honestly in
`ARCHITECTURE.md:206`, and the same shape as the self-modifying-hooks problem
found in other harnesses.

What the gap does and does not cover:

- **Permission rules: benign.** Rules are parsed and installed at load. An
  attacker editing the file mid-session cannot install *new* rules into the
  running session — there is no reload path — and the rules still active are
  the ones the operator actually consented to at that digest. The next start
  re-digests and de-trusts.
- **Plugin artifacts: a real gap.** The artifact is digested, then executed.
  An attacker who can replace the file in that window runs untrusted code
  under a trust record for different content. Closing it properly means
  digesting a held file descriptor and executing *that* descriptor, so check
  and use refer to the same inode — worth doing when the transport lands
  (F19/F20), not before, since there is no plugin process to protect yet.

Per-invocation re-digesting is deliberately **not** the fix: it would put a
filesystem read and a hash on every tool call's hot path to close a window an
attacker who already has write access to your plugin binary has better ways to
exploit. Note the related and larger limit in §12 open question 6 — an
interpreter entrypoint whose real code is an adjacent tree defeats
`artifact_digest` regardless of when it is computed.

### What is digested — the granularity that makes this workable

Three candidates were on the table:

1. **The directory** (Claude Code) — sticky and silent. Rejected; it is the
   flaw.
2. **The whole repo tree** — re-confirms on every commit. Rejected: it is the
   click-through generator, and it conflates "the code changed" (constant,
   expected) with "what may run changed" (rare, alarming).
3. **The trust-relevant configuration surface only.** Chosen.

For a `plugin` subject, that is two digests kept separately because they change
for different reasons and the operator should be told which:

- `entry_digest` — the canonicalized `plugins.json` entry (command, args, env,
  config, declared caps). Changes when *what would be spawned* changes.
- `artifact_digest` — the file the entry's command names, resolved and hashed.
  Changes when *the code that would run* changes.

This granularity is what keeps the design quiet in normal use. A plugin pinned
to an immutable artifact (a versioned wheel, a released binary, a
content-addressed path) has a digest stable across every `git pull`, so it
never de-trusts and never nags. A plugin pointing at a script inside the repo
de-trusts every time that script changes — which is correct, because that is
exactly the case where the repo can change what runs.

**"Trust this plugin at this version" is therefore the primitive**, and "trust
this repo" does not exist as an operation. Version is spelled as a digest
rather than a semver string because `PluginManifest.version` is a free-form
`String` the plugin itself supplies (`ports/plugin.rs:163`) — a self-reported
version is not evidence, and this design never treats a plugin's claim about
itself as evidence (the same rule §8.2 applies to `readOnlyHint`).

### The cost, stated

A plugin you depend on can stop being available after a pull, and the session
runs with fewer tools until you notice. Three things bound that:

1. It is not silent — one notice per de-trusted subject at startup, plus a
   `/plugins` row reading `Untrusted{DigestChanged}` until resolved.
2. D1's `required: true` converts "runs without it" into "refuses to start" for
   a plugin whose absence is unacceptable. The operator picks which failure
   they get, per plugin.
3. It is a de-trust, not a deletion — the caps, `may_allow`, and tool
   classifications survive in the record, so re-trusting after review is one
   confirmation, not a re-configuration.

The trade this makes explicit: **conway spends availability to buy the property
that authority never silently follows content across a change.** That is the
right side of the trade for a tool that runs shell commands.

---

## 6. Capability vocabulary: what it means, and what it does not

### Decision: the vocabulary names exactly the host-mediated surfaces. Nothing else.

`PluginManifest.required_host_caps` exists and is read by nothing — only
`vec![]` literals in tests (`ports/plugin.rs:165`, D2 §11's finding). This
design gives it a job, and the job is narrow on purpose.

A capability belongs in the vocabulary **iff the host is the only path to the
thing it names.** That test admits exactly the D2 extension points and excludes
everything else:

| cap | admits | enforced by |
|---|---|---|
| `tool` | registering tools (`tool/1`, `tool.spec/1`) | registration; ungranted ⇒ tools not registered |
| `observe` | `observe/1` event subscription | the subscription is the host's to make |
| `status` | `status/1` contributions | the registry is host-side |
| `context.append` | `context.append/1` segments | the assembly is host-side |
| `context.tools` | `context.tools/1` announcement hiding | the announcement is host-side |
| `permission.rules` | contributing `deny`/`prompt` rules | the rule set is host-evaluated |
| `permission.policy` | joining the policy chain | the chain is inside `PermissionBroker` |
| `subagent.spawn` | the `SubagentHost` callback (§7) | the callback is a host method |

**Deliberately absent: `fs.read`, `fs.write`, `net`, `exec`.** A plugin is a
separate OS process with the operator's privileges. Conway has no mechanism by
which `"fs": "none"` could be made true, and it is not acquiring one (§2,
GP-08/C-04). Putting those words in a capability list would be worse than
omitting them, because an operator reading `net: none` in a review surface
would reasonably conclude the plugin cannot reach the network — a false belief
manufactured by the very surface meant to inform them. **A declared-but-
unenforced capability is documentation, not a control, and this design refuses
to let documentation sit in the control's slot.**

### The honest channel for the rest: `disclosures`

Self-reported intent is genuinely useful for review; it just must not
masquerade as enforcement. So the manifest carries a second, segregated field:

```jsonc
"disclosures": {
  "network": "calls api.example.com to classify commands",
  "filesystem": "reads ~/.acme/config"
}
```

Free text, rendered in the review surface under a header that says, verbatim:
*"Self-reported by the plugin. Conway does not verify or enforce these."* It
informs the trust decision — which is the decision that actually matters — and
it is structurally unable to be mistaken for a boundary.

### Request vs. grant

- `required_host_caps: Vec<String>` (existing field, keeps its name and its
  meaning): caps without which the plugin refuses to run. **Any one not granted
  ⇒ the plugin fails to load, naming the cap.** Loud, per D2 §11's handshake
  rule.
- `optional_host_caps: Vec<String>` (new): caps used if granted, degraded
  without.
- The **granted** set lives in the trust record and nowhere else. **A manifest
  can only request.** A plugin that asks for `permission.policy` and is not
  granted it simply has no policy in the chain, and `/plugins` shows the
  delta — which is the single most interesting column in the whole inspection
  surface, because "what does this want that I did not give it" is the question
  a reviewer actually has.
- Calling an ungranted point on the wire returns a typed `capability_denied`
  error. **Not silence, not a hang, and not a method missing from the schema**
  (see §7's note to D3).

The effective set is the intersection of requested and granted. A cap granted
but not requested does nothing — a grant is a ceiling, never a floor.

---

## 7. Subagent spawning, and P-6

`ToolCtx.subagents: Arc<dyn SubagentHost>` (`ports/plugin.rs:303`) is the most
consequential thing on the wire. A holder can `start`, `steer`, `ask`,
`cancel`, and read the whole tree — spending tokens, running tools, and
fanning out without awaiting (`SubagentSpec.await_result: false`,
`agent.rs:176-178`) under a default budget of 40 steps and no token cap
(`agent.rs:149-157`).

Two facts from the current tree materially bound the danger, and one does not:

- **Confinement inherits and may only narrow.** `subagent.rs:231-280` resolves
  a child's root from the parent's, and a `requested` root wider than or
  disjoint from the parent's **fails the spawn with a typed error** rather than
  being silently clamped. So a plugin cannot spawn its way out of a root. That
  is a genuine, already-shipped floor.
- **Every child's tool calls still go through `PermissionBroker::decide`**,
  with the same root check first and the same gate at the end.
- **Tool selection does *not* inherit-narrow.** A child's selector is
  `spec.tools` or its `AgentDef`'s (`subagent.rs:357-360`) — never intersected
  with the parent's. An agent restricted to `Only(["read"])` can spawn a child
  with `All`. That is a real widening of the *announced* surface (registration-
  time filtering, `tools/registry.rs:120-138`), it is pre-existing and applies
  to the built-in subagent tool identically, and it is a reason to gate the
  capability rather than a reason to think it harmless.

### Decision: `subagent.spawn` is a capability, default off, granted per plugin, never implied by trust

Two keys, independent: the plugin must be trusted at all (or it does not load),
**and** the operator must have granted `subagent.spawn`. Trust is necessary and
not sufficient. Rationale: trust answers "may this code run as me," which is
already a large question; "may this code spend my tokens and start agents that
run tools" is a different question with a different answer for most plugins,
and collapsing the two would mean an MCP shim for a documentation server
carries the same authority as an orchestration plugin.

### Reconciling with P-6 — the sharpest tension, argued

The tempting implementation is: remote plugins get a reduced `ToolCtx` without
`subagents`; built-ins get the full one. **That would be a P-6 violation, and
this design rejects it.** P-6 says built-ins get no privileged API. A field
that is present for compiled-in code and absent for third-party code, keyed on
*which* it is, is the definition of a privileged API — and D2's tier argument
does not rescue it, because both are on the same side of D2's tier line (both
are extensions; neither is a host port).

The move that resolves the tension is to change the key:

> **The reduction is keyed on the operator's grant, not on the plugin's
> provenance, and the identical mechanism applies to a compiled-in
> `Arc<dyn Plugin>`.**

Concretely: `ToolCtx.subagents` stops being a bare `Arc<dyn SubagentHost>` and
becomes a per-plugin guarded handle. Every method checks the calling plugin's
granted caps and returns `RuntimeError::CapabilityDenied { plugin, cap }` when
`subagent.spawn` is absent. The built-in subagent tool ships in a plugin whose
manifest requests `subagent.spawn` and which is **seeded** with that grant for
built-in-scope plugins.

That seed is the one place the word "built-in" appears, and it appears as a
**default value in an inspectable, revocable config table — not as a branch in
code.** The distinction is thin and I will not pretend otherwise, so here is
why it is the right thin line: revoke `subagent.spawn` from `builtin.subagent`
in the trust record and `/fork` stops working, through exactly the same code
path that stops a third-party plugin. There is no path a built-in can reach
that a third party cannot reach with the same grant. **P-6's substance is "no
code path privileges a built-in," and that survives intact.** A grant seeded in
config is visible and revocable; a branch in code is neither.

Cost, stated: built-ins hold `subagents` unconditionally today, so this is a
behavior change that depends on the seed existing. If the seed were dropped,
`conway_subagent` would break — loudly, with a typed error naming the cap,
which is the correct failure and is worth a test that pins it.

### Note to D3 — the wire shape, flagged not assumed

D3 is deciding the reduced-`ToolCtx` question concurrently from the wire side.
**D4's position: the wire schema is uniform for every plugin; the host's
dispatcher enforces.** The host-callback method exists on the wire regardless
of grant, and calling it without the cap returns a typed `capability_denied`.

Two reasons, both structural:

1. If the schema differs by grant, a plugin's own code path depends on a fact
   it cannot discover, and the failure is "method not found" — indistinguishable
   from a version mismatch. A typed refusal is diagnosable; a missing method is
   a mystery.
2. If the schema is the enforcement, the enforcement lives in schema generation
   — a place with no natural test and no single call site. One checked
   dispatcher is one place to test and one place to audit.

This is a dependency on D3, not an assumption about it. If D3 lands
schema-level omission instead, the capability model still holds; it just moves
where the check lives, and this document's §10 failure posture must be re-read
against that choice.

---

## 8. Interaction with the permission system

### 8.1 Circularity

Three distinct shapes, and only one of them can be prevented structurally.
Saying which is which is the point.

**(a) Direct self-authorization — prevented.** A plugin's policy authorizing a
call to a tool that same plugin provides.

> **Rule: a policy's `Allow` half is not consulted for a call to a tool
> provided by the same plugin.** One condition in the chain loop, keyed on
> `plugin_id`, not configurable.

The `Deny` and `Abstain` halves **are** still consulted from the same plugin,
deliberately: deny is narrowing, and a plugin refusing its own dangerous tool
under conditions it alone understands is useful and harmless. The exclusion is
exactly as wide as the danger, and its asymmetry is the same one running
through this entire document.

**(b) Mutual authorization — detected, not prevented.** Plugin A allows B's
tools; B allows A's. No `plugin_id` check catches this, and no structural
mechanism can — mutual back-scratching is indistinguishable from two
independent policies that happen to agree. Say so plainly, and bound it:

- It requires **two** explicit operator grants, since `may_allow` is per-plugin
  and defaults `false` (D2 §6).
- Every policy allow is **attributed on the event stream**. D2 open question 3
  proposes an optional `by: Option<String>` on `Event::PermissionResolved`;
  **D4 upgrades that from "wanted" to required.** An allow nobody can attribute
  is an audit trail with a hole exactly where the interesting event is.
- A policy allow is **never cached** (D2 §6), so it is re-decided and
  re-attributed on every call rather than becoming a durable, unattributed
  grant.

Detection, not prevention. Recorded as such.

**(c) Self-induced authorization — the honest residue.** Plugin A's tool spawns
a subagent; that child calls a built-in tool; A's policy allows it. Not
self-authorization by the letter — the tool belongs to no plugin A provides —
but it is by the spirit, because A's own action produced the call A authorized.
This is (b) in single-plugin form and it is bounded only by `may_allow` +
attribution + the root floor + `subagent.spawn` being a separate grant. It is
the strongest argument for keeping `subagent.spawn` and `may_allow` as two
independent grants: holding both is what makes this reachable, and requiring
two deliberate operator acts for it is proportionate.

### 8.2 Escalation: narrow-only, with one operator-enabled exception

The safe rule is that a plugin may only narrow. This design has **exactly one**
place a plugin can widen, it is inherited from D2 §6, and it is worth being
precise that it *is* a widening: a policy `Allow` at step 7 of `decide`
short-circuits a human prompt that would otherwise have happened. Relative to
no plugin, that is more allowed, not less.

Ratified, with four independent containments — and the count is the
justification:

1. It requires two operator acts (trust the plugin; set `may_allow: true`).
2. It sits **below both floors**: the root check
   (`permission.rs:476-501`) and plan mode (`:502-526`) run above the entire
   chain, and the allow half is skipped outright when `must_reach_gate` — so a
   policy allow is exactly as authoritative as `AutoAllow` and lives in the
   same slot, never above it.
3. It is per call, never cached, never a grant, never a `PatternRule`.
4. It is attributed on the stream (§8.1b).

Everything else a plugin touches is narrow-only, with these trust-layer
corollaries:

**Plugin rules are `deny`/`prompt` only** (D2 §10), ratified. The ceremony for
suggested `allow` rules — D2's open question 5, D4's to answer — is:

> `/plugins rules <id>` shows the plugin's suggested allow rules **verbatim**,
> in the same wire form the operator's own file uses. Accepting transcribes
> them into the operator's `permissions.json` (project or global, operator's
> choice) with a comment naming the origin plugin and its digest at accept
> time. **From that moment they are operator-authored**: they survive the
> plugin being revoked, they appear in `active_patterns()`, and they are
> visible in a `git diff`.

The trade-off is deliberate and must be stated: the rules **outlive the
plugin**. That is what "operator-authored" means, and hiding it would be worse
— an operator who thinks revoking a plugin revokes its rules is holding a false
model. §9 requires the grant list to show rule origin so this is discoverable
rather than surprising.

**Self-declared classification may only tighten.** A remote plugin's declared
`ToolSpec.category` / `permission` is a claim about itself. Honoring a claim of
`Read` on a tool that executes is a direct plan-mode bypass, and plan mode is
"the mode an operator selects when they want a guarantee"
(`permission.rs:506-509`). So:

- An undeclared or newly-seen remote tool defaults to `Execute` / `Dangerous`
  (D2 §11, generalizing `PathArgs`' own fail-closed default).
- A plugin's declaration may make a tool **more** gated than that default,
  never less.
- **Only the operator may lower a classification**, via `tool_class` in the
  trust record.

Cost, stated: an MCP shim's read-only tools are plan-mode-denied until the
operator classifies them. That is real friction. It is mitigated by placing
`tool_class` in the trust record, so the classification happens **in the same
review, in the same act** as trusting the plugin — with the plugin's own
declarations shown as a pre-filled suggestion the operator can accept
wholesale. The suggestion is visible; the act is the operator's. That is the
same shape as the rule ceremony above, and it is not a coincidence: **the trust
record is the single place all operator-side plugin authority lives** — granted
caps, `may_allow`, tool classifications, bare-name pinning (D2 §7), and
timeouts. One file, one review surface, one diff.

**Never widenable, under any grant:** the confinement root (R3 — the check is
above all four allow paths and no extension point may be placed above, beside,
or in a position to widen it), plan mode's denial, argument rewriting (D2 §9),
and `AllowAlways` / pattern-grant installation.

### 8.3 Plugin strings are untrusted *inbound*, not only outbound

**The finding.** `PermissionOutcome::Deny { rendered_error }` flows to
`ToolOutcome::error(call_id, tool_name, rendered_error)`
(`crates/conway-runtime/src/tools/runner.rs:307-309`), which wraps it in
`ContentBlock::Text` with `is_error: true` (`runner.rs:98-109`). That block
enters the model's context. **`sanitize_rendered` is applied only to `rendered`
(`runner.rs:396`), not to this path.** So a denial reason is model-visible text
that has passed through no filter at all.

With plugin policies returning `Deny { reason }`, that becomes a prompt-
injection channel from an extension straight into the model's context — and a
uniquely well-positioned one, because the model reads a denial while deciding
what to do next and is primed to treat it as instruction-shaped.

**Rules, in force for every plugin-supplied string that can reach the model:**

1. **Attribute and delimit; never concatenate.** A policy reason is rendered as
   `permission denied by policy "<plugin_id>":` followed by the reason in a
   delimited block. The plugin cannot forge the harness's own voice. This is
   the mitigation that actually buys something: content filtering is
   unwinnable, but *provenance* is cheap and makes "conway says you should now
   run X" unavailable to a plugin.
2. **Cap the length** (recommendation: 1 KiB) with an explicit elision. A
   denial reason is not a payload channel. Precedent: the runner already
   truncates and records truncation (`runner.rs:447`).
3. **Apply control-character sanitization to this path too.** This is a
   present-tree gap independent of plugins — an operator-authored gate's message
   already reaches the transcript unsanitized. `sanitize_rendered`'s guarantee
   should cover *every* string that reaches the transcript or the terminal, and
   the fix is to apply it at `ToolOutcome::error`'s construction rather than at
   each producer.
4. **`context.append/1` provenance must be rendered, not merely recorded.** D2
   stamps `Provenance::Plugin { id }` on appended segments; D4 requires that
   attribution appear **in the prompt text the model sees**. A segment
   attributed only in the log is attributed to the auditor, not to the reader
   who is about to act on it.
5. **`StatusContribution` never reaches the model.** Screen only. Stated so it
   cannot drift into the context assembly later.

---

## 9. "What is loaded right now, from where, and what can it do?"

D1 §6 already requires `Conway::plugins()`, a `conway plugins` subcommand, and
a TUI `/plugins` panel. D4 specifies the trust columns, because the question is
three questions and Claude Code answers none of them.

**Per row:**

| column | answers |
|---|---|
| id, version, origin file, scope | *from where* |
| trust state — `Trusted{since,by}` / `TrustedByAuthorship(global)` / `Untrusted{reason}` | *may it run* |
| artifact path + digest, and match/mismatch against the record | *is it the code I trusted* |
| **granted caps**, and **requested-but-not-granted** as a distinct column | *what can it do* |
| `may_allow` yes/no | *can it approve my tool calls* |
| tool classifications, with operator-set marked distinctly from plugin-declared | *what can it walk past* |
| points wired, with **resolved selector match sets** (D2 §8's `plugins explain`) | *what does it actually see* |
| disclosures (§6), under the "not verified, not enforced" header | *what does it claim* |

**Two commands that do not exist anywhere and should:**

- **`conway plugins diff <id>`** — what changed since trust was granted.
  Re-confirmation without "compared to what" is theater, and this is the
  surface the §5 review opens into.
- **`conway trust list`** — every subject in the record, across projects, with
  its grant date and grantor. The answer to "what have I trusted, ever," which
  is the question an operator asks after reading a security advisory.

**One existing surface gains a column:** `PermissionBroker::active_patterns()`
exists because "a rule set nobody can inspect is a trap"
(`permission.rs:416-419`). With rules arriving from three authors — the
operator, a project file, and an accepted plugin suggestion — that list must
show **origin per rule**. A grant list that cannot say where a rule came from
is precisely the trap its own doc comment warns about, and §8.2's "rules
outlive the plugin" trade-off is only honest if this column exists.

**TUI integration point** (not a widget design): the review surface is a
decision-owed modal and belongs in the surface queue `Mode` already serializes
through `promote_next_surface`
(`crates/conway-cli/src/tui/state.rs:1583-1604`), alongside
`AwaitingPermission`, `AskModal`, and `IntentConfirm`. Note the priority
question that raises: a trust review must not preempt a pending permission
prompt, so it enters at the **back** of the queue, and it is only ever opened
by an explicit operator action — which is exactly why §5's de-trust path emits
a transcript notice rather than a surface. **Nothing in this design ever
enqueues a modal the operator did not ask for.**

**Startup is not the trust surface.** D1 spawns plugins eagerly, before
`ConwayBuilder::build()`, which is before the TUI event loop and its `Mode`
machinery exist. Rather than invent a pre-TUI prompt, untrusted subjects are
simply not loaded and the session starts. That constraint and §5's
no-prompt decision point the same way, which is a good sign about both.

---

## 10. Failure posture (P-10) for this surface

Restated for trust specifically: **a plugin that crashes, hangs, returns
garbage, is untrusted, or is absent must degrade, never authorize.**

| failure | result |
|---|---|
| plugin process crashes | its policies contribute `on_failure` (default `Deny`); its tools become `unknown tool` errors; never an allow |
| plugin hangs | the point's timeout fires — the deadline is the host's, not the plugin's (D1 §4) — then `on_failure` |
| plugin returns garbage | frame dropped and counted (D1 §8); the call hits its deadline; `on_failure` |
| plugin untrusted / de-trusted / never loaded | contributes nothing |
| `trust.json` missing | every project-scoped subject is untrusted |
| `trust.json` corrupt | **treated as empty, with a loud diagnostic** — never partially applied, never read optimistically |
| artifact digest uncomputable (missing, unreadable, a directory) | untrusted |
| granted-cap lookup fails for any reason | cap absent |

**The structural guarantee behind the whole table.** A plugin allow exists only
as a *return value* at step 7 of `decide`. Absence produces no return value, so
control falls through to `gate.check` — the human. There is no default-allow
branch keyed on "no policies registered," and there must never be one. A test
should pin exactly that: **with zero policies registered, `decide`'s behavior is
byte-identical to today's.**

**Undecidable is untrusted.** `Containment::Undecidable` is fused with
`Outside` everywhere in this codebase (`permission.rs:384`) because "can't
check" is never "allow." The trust layer adopts the identical rule verbatim:
any subject whose trust cannot be *decided* is untrusted.

**The second reason never to put a guarantee in a plugin.** R3 already says a
guarantee implemented as plugin policy fails open when the plugin is absent.
Trust adds an independent one: a plugin's availability now depends on a **file
digest**, so any guarantee living in a plugin can be revoked by an attacker
touching a byte in the repo. Guarantees stay in the harness. Root is the floor
policy sits on, and this document does not create a path around it —
`check_root` runs above all four allow paths, the policy chain enters below it,
and nothing in the trust model can move it.

---

## 11. The path that already exists: project-scoped `permissions.json`

The design above applies to a mechanism that does not exist yet. §1 established
that the same threat is live on `main` today. **This is the shippable half, and
it does not depend on the plugin work at all.**

Changes, in order of value:

1. **A project-scoped `permissions.json` is a trust subject.** Its `allow` list
   is installed only if the file's digest matches a record; otherwise it is
   skipped with one notice. The global file is unaffected — operator-authored,
   trusted by authorship (§4).
2. **Add the `deny` half** (§3), applying immediately and regardless of trust,
   from either scope. This is the asymmetry's other half and conway currently
   has none of it.
3. **Stop installing project rules at `Session` scope by default.**
   `app.rs:210-214` passes `PermissionScope::Session`, which
   `GrantScope::covers` answers `true` for every requester including every
   subagent (`permission.rs:239-241`). A project file's grants should default
   to the narrowest scope that makes them useful; `Session` should be the
   operator's explicit choice, not the loader's default. (Recorded as a
   recommendation with a compatibility cost: it narrows behavior for anyone
   relying on the current default. The narrowing direction is the safe one.)
4. **Origin tracking on `active_patterns()`** (§9), without which none of the
   above is inspectable.

> **Status (2026-07-30), `d917ba2`.** Items 1, 2, and 4 shipped, all three
> the same day this document's HEAD banner names. Item 1: project `allow`
> rules now require a recorded trust decision (`crates/conway/src/conway.rs`,
> `crates/conway/src/config/trust.rs`). Item 2: the `deny` half exists, see
> §3's status note above. Item 4: origin tracking landed as `PatternOrigin`
> (`crates/conway-core/src/permission_pattern.rs:778-801`), and — not
> anticipated here — turned out to be F12's stated prerequisite in
> `extension-architecture.md` §9.5, and enabled per-rule revocation as a
> side effect. **Item 3 is the one piece still open** — project rules still
> install at `PermissionScope::Session` by default; tracked as F13 in
> `extension-architecture.md` §12.

The current loader's silence is correct where it is silent and wrong where it
is not: `app.rs:195-199` argues that every failure is "silent and narrowing,"
and that is right, because a corrupt file yields *fewer* grants. **Skipping an
untrusted file is the same kind of narrowing and belongs on the same path** —
but it deserves a notice rather than silence, because unlike a corrupt file it
is a state the operator can resolve.

---

## 12. Open questions

1. **D3's wire shape for host callbacks** (§7). D4's position is a uniform
   schema with a typed `capability_denied`; D3 owns the decision. If D3 lands
   schema-level omission, §10's table must be re-derived against it.
2. **Board item 01KYKPAW2AFYE284WCC894T87J** (the permission-policy port).
   Whichever of D2/D4/that item lands first fixes `PermissionPolicy`'s shape;
   D4 requires `may_allow` default `false`, the same-plugin allow exclusion
   (§8.1a), and `by`-attribution on `PermissionResolved`.
3. **The built-in seed grant** (§7). Seeding `subagent.spawn` for built-in-
   scope plugins is the thin line P-6 survives on. It deserves a human's
   explicit sign-off, not a designer's assertion.
4. **Scope narrowing for project permission rules** (§11 item 3) is a
   behavior change for existing users. Needs a call on whether it ships with
   the trust gate or separately.
5. **Multi-operator / shared-home hosts.** `granted_by` is recorded but nothing
   enforces it. Is a trust record granted by another user on a shared machine
   honored, ignored, or a warning?

   > **Status (2026-07-30), `d917ba2`.** This question is malformed as
   > written — the premise doesn't hold. The landed record,
   > `TrustedRecord { content_digest, trusted_at }`
   > (`crates/conway/src/config/trust.rs:99-101`), has **no author
   > attribution field at all**; `granted_by` does not exist in the shipped
   > shape, so "recorded but nothing enforces it" is not the state of the
   > code. The `"granted_by"` sketches elsewhere in this document (e.g. §9's
   > table, `Trusted{since,by}`) describe a shape that did not land either.
   > The real open question this item should have been: **on a shared-home
   > host, `~/.conway`'s trust store has no notion of which user recorded a
   > given entry, so any user able to write that store can silently vouch
   > for content on behalf of every user who reads it.** Whether to add
   > `granted_by` at all — and if so, whether it is enforced or advisory —
   > is still undecided; this is now a "does the field need to exist"
   > question, not a "does the existing field need enforcement" one.
6. **Digesting an artifact that is not a single file** (a `node`/`python`
   entrypoint whose real code is an adjacent tree). The `artifact_digest`
   covers the named file only, and a script that `import`s the rest of the repo
   defeats it. Options: digest a declared file list, require a lockfile, or
   accept and document the limit. Recommend documenting the limit in v1 and
   surfacing "this entry's artifact is an interpreter" as a review-surface
   warning — pretending otherwise would be its own `readOnlyHint`.

---

## 13. Findings in the current tree (independent of this design)

- **A cloned repo can install pattern grants into a live session with no
  consent.** `.conway/permissions.json` is discovered and installed at
  `Session` scope at TUI startup (`tui/app.rs:200-216`); `PermissionFile` has
  only an `allow` field (`permission_pattern.rs:428-433`), so the whole file is
  grants. Bounded to `bash`-shaped, metacharacter-free prefixes by
  `PatternRule::matches` — which still leaves `bash:npm run build` in a repo
  that owns `package.json`. §1, §11.

  > **Status (2026-07-30), `d917ba2`.** Fixed. Project `allow` rules now
  > require a recorded trust decision before they install; see §1's and
  > §11's status notes above. (The "bounded to `bash`-shaped" clause was
  > also wrong independent of this fix — see §1's status note on `68ea9b1`.)
- **Denial reason text reaches the model's context unsanitized.**
  `PermissionOutcome::Deny { rendered_error }` → `ToolOutcome::error` →
  `ContentBlock::Text` (`runner.rs:307-309`, `:98-109`). `sanitize_rendered`
  covers `rendered` only (`runner.rs:396`). §8.3.
- **A subagent's tool selector does not inherit-narrow from its parent**
  (`subagent.rs:357-360`), unlike its confinement root, which does and may only
  narrow (`subagent.rs:231-280`). §7.
- **`PluginManifest.required_host_caps` is inert** — declared, read by nothing
  (`ports/plugin.rs:165`). Also D2 §13. §6 gives it a job.
- **`PluginManifest.version` is a free-form self-reported `String`**
  (`ports/plugin.rs:163`) and is not evidence of anything. §5 uses digests
  instead.
