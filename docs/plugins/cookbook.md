# Cookbook: five worked examples

Five plugin examples, complete and end to end, ordered by what each one
stresses. Depends on [`concepts.md`](concepts.md) for vocabulary and
[`hooks.md`](hooks.md) for per-point contracts — this page links to both
rather than restating them, and shows what a real author actually writes.

**These are not decoration. They are the architecture's acceptance tests**,
in the sense the original design spike used: *if the architecture makes one
of these awkward, the architecture is wrong, not the example.* Two of the
five — spilling bulky tool output to a file (example 1) and progressive
skill disclosure (example 4) — were named by the operator on 2026-07-30 as
the specific cases the plugin architecture should be judged against,
replacing the design spike's four original illustrative use cases. Both get
an explicit verdict below, not just a code sample.

**A cookbook containing only what happens to work is a marketing
document.** Where an example (or a variant of one) cannot be written against
the tree as it stands, that is stated plainly, the gap is named, and the
board item tracking it is cited — a documented gap is a required outcome of
this page, not a failure of it.

## How to read this page

Every example is labeled one of:

- **Implementable today** — the code below is real, compiles against
  `conway::plugin` alone, and was executed.
- **Partially implementable today** — one variant works now; a stronger
  variant is designed-not-built, and the gap is named.
- **Blocked** — designed-not-built; the section shows what exists instead
  (if anything) and cites the board item.

**Every code block below marked runnable was actually compiled and run**
against a scratch crate outside this workspace, `cookbook-scratch/`, whose
only `conway` dependency is the `conway` crate itself — the same discipline
[`authoring.md`](authoring.md) used, extended to five examples instead of
one. All twelve tests across the five examples' scratch files pass:

```console
$ cargo test
...
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out  # example 1
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out  # example 2
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out  # example 3
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out  # example 4
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out  # example 5
```

The scratch crate itself is not part of the tree (it lived outside the
workspace and was deleted after verification) — every snippet below is
copied from it verbatim, not re-derived for this page. A code block marked
`rust,ignore` is illustrative only: it documents a **designed-not-built**
point (per [`hooks.md`](hooks.md)'s own Status column) and cannot compile
against the tree today, named as such rather than left to imply otherwise.

### 1. Spill bulky tool output to a file

**Implementable today.** Verdict below.

The operator's own framing, 2026-07-30: *"spill to file is a perfect example
of a plugin exercised by a well defined hook."* Board item
`01KYTN3A9SPDMRG610YSB5QQXX` is **done** — `TruncationPolicy::Artifact`, the
core enum variant documented as "spill the full output to an artifact, keep
a pointer in context" while doing neither, was **removed**, not implemented.
`crates/conway-core/src/content.rs`'s `TruncationPolicy` has exactly four
variants today (`None`, `Head`, `Tail`, `HeadTail`) — no `Artifact` case
exists to be a documented lie anymore. The capability moved to a plugin
entirely, per that item's own resolution: *"the feature belongs in a
plugin"* (GP-11).

**The architecture verdict, stated directly.** That item's own spec argued
this case *"fails against the pre-redirect design, which offered only
additive context contribution and a plugin providing its own tool. Neither
lets a plugin narrow another tool's output."* **That claim is now stale
against HEAD, and this page corrects it at the site.** It was accurate
against the design this codebase carried before the hook-first redirect
(decision `01KYTNTRAGX2H72HF4R69XACEX`) landed `ContextHook` with
edit/drop/replace authority over the assembled payload — but
`ContextHook::before_request` has had that authority since WI-126 shipped,
independent of anything added for this item. `concepts.md`'s value-class
boundary table states plainly what a plugin may do to **Context**: "Edit,
drop, replace, mask" — and its own "What exists today" note confirms
`ContextHook::before_request` "genuinely can edit and drop segments
in-process — this is not a forward declaration". A `ToolResult`-provenance
segment is a segment like any other; nothing in the trait singles it out as
append-only.

What *was* genuinely missing until 2026-08-07 (board item
`01KZ84437RMKHP5DJX7RMHH7JY`, commit `c430ca9`) was **somewhere confinement-
checked to put the spilled bytes.** `ContextHookCtx` carried no root, no
cwd, and no write capability at all — a hook wanting to spill had to reach
for ambient filesystem access and guess a path, or receive one out-of-band
at construction that couldn't follow an agent that `chdir`s or a narrower-
rooted subagent. `ContextHookCtx::artifacts` (an
[`ArtifactWriteHandle`](../plugins/hooks.md)) closes exactly that gap:
`ctx.artifacts.write(name, bytes)` resolves `name` the same way a tool's own
path argument would be resolved and confined
(`conway_runtime::permission::resolve_like_the_tool_will`, the one
implementation of that rule, reused rather than restated per P-14) and
either returns the path it actually wrote to or a typed
`ArtifactWriteError` — there is no second, hand-rolled resolution surface a
hook author could get subtly wrong.

**So: yes, this example is writable end to end today, and it needed both
halves.** The narrowing half (`before_request` editing a `ToolResult`
segment) predates this item; the placing-the-file half
(`ContextHookCtx::artifacts`) is what actually closed on 2026-08-07. Neither
alone would have been a complete answer — narrowing with nowhere safe to put
the full text is not spill-to-file, and a write handle with no way to
narrow the segment the model sees is not either.

```rust
use conway::plugin::{
    async_trait, ContentBlock, ContextHook, ContextHookCtx, ContextPayload, Provenance,
};

const SPILL_THRESHOLD_BYTES: usize = 200;
const PREVIEW_CHARS: usize = 120;

/// Narrows a large `ToolResult` segment to a preview plus a pointer,
/// spilling the full text to an artifact file under the agent's
/// confinement root.
struct SpillToFileHook;

#[async_trait]
impl ContextHook for SpillToFileHook {
    async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        mut payload: ContextPayload,
    ) -> ContextPayload {
        for segment in &mut payload.segments {
            let Provenance::ToolResult { call_id, tool } = &segment.provenance else {
                continue;
            };
            let full_text: String = segment
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if full_text.len() <= SPILL_THRESHOLD_BYTES {
                continue;
            }
            let name = format!("spill-{call_id}.txt");
            match ctx.artifacts.write(&name, full_text.clone().into_bytes()).await {
                Ok(path) => {
                    let preview: String = full_text.chars().take(PREVIEW_CHARS).collect();
                    segment.content = vec![ContentBlock::Text {
                        text: format!(
                            "[{tool} output truncated: {} bytes, full output spilled to {}]\n{preview}...",
                            full_text.len(),
                            path.display(),
                        ),
                    }];
                }
                Err(err) => {
                    // Fail closed, VISIBLY -- never drop the fact the spill
                    // failed. The full text is left in place: oversized,
                    // but not silently lost.
                    segment.content.push(ContentBlock::Text {
                        text: format!(
                            "[spill-to-file failed ({err}); leaving the full, unspilled output in place]"
                        ),
                    });
                }
            }
        }
        payload
    }
}
```

**The threshold, the naming scheme, the preview length, and the retention
policy are all this plugin's own opinions** — exactly GP-11's split ("Policy
Complexity Lives in Hooks, Not Core"): core ships the seam
(`ContextHook`/`ArtifactWriteHandle`), and sophisticated policy attaches as
a hook. Nothing above is a recommended default; a real deployment tunes
every one of those four numbers/choices to its own workload.

Tested directly (no session, no network — `authoring.md`'s step 3 pattern,
using its own `ArtifactWriter` test-double shape rather than re-deriving
it):

```rust
#[tokio::test]
async fn oversized_tool_results_are_narrowed_and_spilled() {
    let writer = Arc::new(RecordingArtifactWriter::default());
    let agent_id = AgentId::new();
    let ctx = ContextHookCtx {
        agent_id,
        session_id: SessionId::new(),
        turn: 4,
        model: None,
        estimated_tokens: 20_000,
        artifacts: ArtifactWriteHandle::new(writer.clone(), agent_id),
    };
    let big = "x".repeat(5_000);
    let payload = ContextPayload {
        segments: vec![tool_result_segment("tc_7", "bash", &big)],
        tools: vec![],
    };

    let out = SpillToFileHook.before_request(&ctx, payload).await;

    let ContentBlock::Text { text } = &out.segments[0].content[0] else { panic!() };
    assert!(text.len() < big.len());
    assert!(text.contains("spill-tc_7.txt"));

    let recorded = writer.last_write.lock().unwrap();
    let (name, bytes) = recorded.as_ref().expect("the hook must have called write");
    assert_eq!(name, "spill-tc_7.txt");
    assert_eq!(bytes.len(), big.len(), "the FULL text is spilled, never a truncated copy");
}
```

**The failure path**, exercised, not assumed: `before_request` carries no
`Result` in its signature at all (`hooks.md` point 3's "On error" row), so
an `ArtifactWriteError` (e.g. `OutsideRoot`, if the plugin's own naming
scheme somehow resolved outside the root) cannot propagate as an error — the
only sanctioned response is what the hook *returns*. The test above's
sibling, `a_refused_write_degrades_visibly_instead_of_silently_dropping_
content`, wires a writer that always refuses and asserts the hook appends a
visible `"[spill-to-file failed ...]"` note while leaving the original,
oversized text as the first block — nothing is silently lost, only left
too large, which is a strictly safer failure than either "silently drop the
content" or "silently pretend it was spilled."

**What this example does *not* prove.** `RecordingArtifactWriter` above is
an in-memory double, the same pattern `plugin_surface.rs`'s own
`RecordingArtifactWriter` uses — it proves
the hook calls `write` with the right name and the full bytes, not that a
real filesystem write landed inside a real confinement root. That guarantee
is `conway-runtime`'s `AgentArtifactWriter`, exercised by its own
`artifact_store` tests against a real filesystem (cited in
`ArtifactWriteHandle`'s own module doc); a hook author never re-implements
that check, by construction — `ArtifactWriteHandle::write` is the *only*
place a hook-written path is resolved, so there is no second surface to get
subtly wrong.

**One piece of authoring friction, closed since this page was first
written:** building `ContextHookCtx` by hand for a test used to require a
real `ArtifactWriter` impl even when a hook wrote nothing, and the facade
shipped no no-op one — the ~15 lines of boilerplate `authoring.md`'s own
walkthrough hit, tracked as board item `01KZJ5S3ZC8SPWTX94C4HTEC2R`, now
closed: `ArtifactWriteHandle::noop(agent_id)` (`authoring.md`'s current step
3) covers that case. It does **not** cover this example's own test above,
which keeps its `RecordingArtifactWriter` on purpose — the test asserts on
the exact name and bytes the hook wrote, something a no-op writer cannot
record by construction. `noop` and a purpose-built recording double solve two
different problems (nothing to supply vs. something to observe); this
example genuinely needs the latter, board item or no.

### 2. Compaction

**Partially implementable today.** The ephemeral form works now; the
persisted, reversible form the operator specifically wanted is
designed-not-built, and this section names the gap rather than presenting
the weaker form as if it were the stronger one.

**What the operator actually asked for**: build compaction on the
*persisted, reversible* mechanism — append a summary segment, then emit
`LogRecord::ContextMask` records durably excluding the folded-away
originals. That buys three things: the session log stays intact and
append-only (nothing is deleted, ever); the masking is reversible by
appending a *second* mask record, never by mutating the first; and the
transformation stays inspectable end to end (GP-10).

**That durable form has no producer anywhere in the tree, and no hook can
reach it.** `LogRecord::ContextMask` (`crates/conway-core/src/log.rs`) is
real, persisted, and consumed —
`conway_session::resolver::apply_context_mask` reads and applies one — but
confirmed by search across `conway-runtime`, `conway-tools`, and the facade,
**nothing constructs one**. There is no `SessionHandle`/`Conway` method that
appends a `ContextMask` record, and `ContextHook::before_request`'s only
channel back to the runtime is the `ContextPayload` it *returns* for the
current request — an edit that lasts exactly one request, never persisted.
`hooks.md` states this precisely: *"No hook can express 'mask this durably'
today; every `ContextHook` edit is ephemeral, scoped to one request, and
invisible on the next turn unless the hook repeats it."* No board item names
this gap specifically yet — the closest tracked work is
`01KZ844ZXZMVRWC7ZANT7PSM6X` (the `context.hook/1` replace-primitive gap),
not a perfect match, so treat "produce a durable mask from a hook" as an
open, unfiled gap until someone files it.

**What *is* real and runnable today is the ephemeral form**, and it still
delivers on two of the three things the durable form promises: the
persisted log is untouched (a `ContextHook` never writes to it, only to the
in-flight `ContextPayload`), and the transformation is inspectable —
the summary segment carries its own `SystemNote` provenance, naming itself
as a compaction artifact rather than pretending to be a real tool output.
What it does *not* buy is "computed once, stays folded": because nothing
persists the fold, `ContextBuilder` rebuilds the full, unfolded segment list
from the log every turn, and this hook re-folds it identically every time
`before_request` runs — real work repeated, not a one-time saving.

```rust
use std::collections::HashSet;

use conway::plugin::{
    async_trait, ContentBlock, ContextHook, ContextHookCtx, ContextPayload, PromptSegment,
    Provenance, Role,
};

/// Folds every `ToolResult` segment except the most recent `keep_last` into
/// one summary segment, EPHEMERALLY: this edits only the just-assembled
/// `ContextPayload`, never the persisted, append-only session log.
struct CompactOldToolResultsHook {
    keep_last: usize,
}

#[async_trait]
impl ContextHook for CompactOldToolResultsHook {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        let ContextPayload { segments, tools } = payload;

        let result_idxs: Vec<usize> = segments
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.provenance, Provenance::ToolResult { .. }))
            .map(|(i, _)| i)
            .collect();

        if result_idxs.len() <= self.keep_last {
            return ContextPayload { segments, tools };
        }

        let fold_count = result_idxs.len() - self.keep_last;
        let fold_set: HashSet<usize> = result_idxs[..fold_count].iter().copied().collect();

        let mut summary_lines = Vec::new();
        for &i in &result_idxs[..fold_count] {
            if let Some(ContentBlock::Text { text }) = segments[i].content.first() {
                let head: String = text.chars().take(80).collect();
                summary_lines.push(format!("- {head}"));
            }
        }
        let summary = PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: format!(
                    "[compacted {} earlier tool result(s) -- ephemeral, recomputed every \
                     request; the session log is untouched]\n{}",
                    fold_set.len(),
                    summary_lines.join("\n")
                ),
            }],
            // GP-10: attributable, like every other segment -- this one
            // names itself as a compaction summary, not a real tool output.
            Provenance::SystemNote {
                reason: "compaction: folded earlier tool results".to_string(),
            },
        );

        let mut new_segments = Vec::with_capacity(segments.len() - fold_set.len() + 1);
        let mut inserted = false;
        for (i, seg) in segments.into_iter().enumerate() {
            if fold_set.contains(&i) {
                if !inserted {
                    new_segments.push(summary.clone());
                    inserted = true;
                }
                continue;
            }
            new_segments.push(seg);
        }
        ContextPayload { segments: new_segments, tools }
    }
}
```

Run, both the pass-through and the fold cases:

```rust
#[tokio::test]
async fn older_tool_results_fold_into_one_attributed_summary() {
    let payload = ContextPayload {
        segments: vec![
            /* UserPrompt */ /* tc_1 */ /* tc_2 */ /* tc_3, most recent */
        ],
        tools: vec![],
    };
    let hook = CompactOldToolResultsHook { keep_last: 1 };

    let out = hook.before_request(&ctx(), payload).await;

    // UserPrompt + one folded summary + the kept-verbatim last ToolResult.
    assert_eq!(out.segments.len(), 3);
    let Provenance::SystemNote { reason } = &out.segments[1].provenance else { panic!() };
    assert_eq!(reason, "compaction: folded earlier tool results");
    let ContentBlock::Text { text } = &out.segments[1].content[0] else { panic!() };
    assert!(text.contains("compacted 2 earlier tool result"));
    assert!(!text.contains("third, most recent")); // the kept one stays out of the summary
}
```

(The full test — every segment literal, not elided here — is in
`example2_compaction.rs`'s `older_tool_results_fold_into_one_attributed_
summary`; the elision above is presentational only, inside a `rust,ignore`
excerpt, not a claim about the runnable original.)

**The trigger mechanism, and its exact boundary.** A real compaction hook is
most useful wired to `ContextHook::on_overflow`, which the admission
cluster (2026-08-01) made **genuinely invoked** — before that, it was a
hook point nothing called. State the boundary precisely, because it is easy
to overstate: `on_overflow` fires **only** when the router or the attempt
engine rejects with `RoutingError::ContextTooLarge` — every candidate
failing **solely** on the headroom gate. A candidate that fails on headroom
*and* something else (an unhealthy endpoint, a missing tool-calling
capability) is a **mixed** failure and disqualifies the whole request from
`ContextTooLarge` — resolution falls back to `RoutingError::NoCandidate`
instead, and **no hook fires for `NoCandidate`, ever**. A compaction hook
cannot always shrink a request back under the window; it can only try when
size was the *sole* thing wrong (`hooks.md` point 4's own boundary section
has the full citation trail — `AgentLoop::route_and_attempt`'s destructure
of `RoutingError::ContextTooLarge`).

**GP-12, stated plainly: conway ships no compaction policy, and this is an
example, not a recommendation.** Nothing above has been measured against a
real workload. `arXiv:2607.06906`'s 38% token-reduction figure — the paper
that originally surfaced this idea — was measured on a single vendor's
workload with n=22, a deliberately weak baseline, and an undisclosed
conflict of interest; its own data shows smaller models *regressed* on
orchestration-heavy tasks. Steering GP-12 blocks shipping a compaction hook
as a default on the strength of a paper like that one — build the
measurement against conway's own workload first, then decide.

### 3. A permission guardrail

**Implementable today** for the static form. The inference-evaluated
variant is **designed-not-built**.

A guardrail that narrows without holding authority: it can only make an
outcome *more* restrictive than the floor already in force, never grant
anything. `permissions.json`'s structured `{ select, when, then }` rules
(`hooks.md` point 6, **Implemented**, board item `01KYTJD6CJ1CHJBXZ0GFYMV5MT`
done) are exactly this shape, and `Rule::then` is enforced to only ever be
`Deny`/`Prompt` for a plugin-contributed rule — `Then::Allow` paired with
`PatternOrigin::Plugin` is structurally rejected at
`PermissionBroker::remember_pattern_rule`, not merely a convention an
author is expected to follow.

**Verified against the tree at writing time, per this set's standing
rule**: board item `01KYTP1D3XWEZPW4AKPH54FNB3` ("the `prompt` rule effect
has no slot in the decision ordering") is **done**. `PermissionBroker::
decide`'s real, eight-step ordering (`hooks.md`'s "The permission decision
ordering" section) gives `prompt`-pattern rules their own step, ahead of the
cache, pattern-allow, and `AutoAllow` — `crates/conway-runtime/tests/
permission_broker.rs`'s `a_prompt_rule_forces_the_gate_under_auto_allow` and
`a_prompt_rule_forces_the_gate_over_a_matching_allow_pattern_grant` both
pass against the shipped broker.

```rust
use conway::permission_pattern::{Rule, Then};
use conway::ToolCategory;

const GUARDRAIL_JSON: &str = r#"[
    { "select": { "tools": ["bash"] }, "when": { "command_prefix": "rm -rf" }, "then": "deny"   },
    { "select": { "tools": ["bash"] }, "when": { "command_prefix": "git push" }, "then": "prompt" }
]"#;

#[test]
fn deny_refuses_outright_prompt_forces_the_gate_every_time() {
    let rules: Vec<Rule> = serde_json::from_str(GUARDRAIL_JSON).expect("valid rule JSON");
    let (deny_rule, prompt_rule) = (&rules[0], &rules[1]);
    assert_eq!(deny_rule.then, Then::Deny);
    assert_eq!(prompt_rule.then, Then::Prompt);

    assert!(deny_rule.matches_deny_render("bash", ToolCategory::Execute, "rm -rf /tmp/scratch"));
    assert!(prompt_rule.matches_deny_render("bash", ToolCategory::Execute, "git push origin main"));
}
```

**What each effect actually achieves, since both narrow but do different
things:**

- **`deny`** refuses the call outright, before any allow path is ever
  consulted — no human is asked, the call simply does not run.
- **`prompt`** does not refuse anything. It forces the *same* call to the
  operator's gate even in `AutoAllow` mode and even over a matching `allow`
  grant that would otherwise have skipped the human entirely
  ([`docs/permissions.md`](../permissions.md#the-prompt-rule-effect)). It is
  how a plugin says "this class of call deserves a human look every time,"
  without the authority to say "and here is my answer."

**Neither installation needs a trust decision** — `trust-and-security.md`'s
asymmetry: a `deny`/`prompt` rule installs unconditionally from any file
(project or global, trusted or not), because a rule that can only narrow has
no failure mode worth gating on trust. That is the literal sense in which
this guardrail "narrows without holding authority": it takes effect the
moment it is written, with zero ceremony, precisely *because* it structurally
cannot grant anything.

**The inference-evaluated variant — designed-not-built, `01KZDC0RDRMMMJHX7SAFMM2Q5A`.**
A guardrail is the natural case for judgment-by-model: "deny a commit
message that plausibly leaks a customer's data" cannot be expressed as a
prefix match, only judged. `hooks.md` point 8 (`permission.policy/1`) is the
point this would attach to — no `NarrowingPolicy`/`DecidingPolicy` trait
exists anywhere in the workspace, so the shape below cannot compile against
the tree; it states the decided design (`inference-hooks.md`), not a
runnable path:

```rust,ignore
// ILLUSTRATIVE ONLY -- no such trait exists in the tree. `NarrowingPolicy`
// and `permission.policy/1` are designed, not built (hooks.md point 8).
struct DataLeakGuardrail;

impl NarrowingPolicy for DataLeakGuardrail {
    // A `NarrowingPolicy` returns `Deny { reason } | Abstain` -- no `Allow`
    // variant exists on its type AT ALL. "May only narrow" is a property of
    // the RETURN TYPE, not a runtime flag an inference-evaluated policy
    // could talk its way around.
    async fn decide(&self, req: &PolicyRequest) -> PolicyVerdict {
        // Judge `req.rendered` by inference, spawned zero-tool, `Spawn` mode
        // (the default -- see below), never seeing `req`'s surrounding
        // conversation.
        todo!()
    }

    fn subagent_mode(&self) -> SubagentMode {
        SubagentMode::Spawn
    }
}
```

**This is exactly where fork-vs-spawn earns its place, concretely, not
abstractly.** `Spawn` (the default: `inference-hooks.md`) gives the judge
*only* the tool call it's evaluating — a clean slate, cheap, and structurally
incapable of leaking the surrounding conversation because there is nothing
to leak. `Fork` would inherit the calling agent's entire ancestry as
context, letting the judge reason about *why* the call is happening, not
just what it says — but it is expensive, and doubles the attacker-reachable
surface an inference-evaluated hook already carries: the judge now reads
strictly more attacker-influenced text, and its own verdict (a deny reason)
is itself a channel that text could try to launder something back through.
A guardrail deciding "does this call plausibly leak data" almost never needs
the whole conversation — `Spawn` is the right default here, not merely the
architecture's default.

### 4. Progressive skill disclosure

**Implementable today.** Verdict below.

The operator's second named acceptance case: load a name/description table
into context; fetch a specific skill's full document only once the model
decides it wants it. This stresses **context assembly**, a different seam
from example 1's tool-output narrowing — `ContextBuilder` injects one
`Provenance::Skill` segment per configured skill, **full body, always**
(`crates/conway-runtime/src/context/builder.rs`'s `[1] SkillFragments*`
step) — there is no name/description-only mode built into assembly itself.

**The architecture verdict.** Both halves this example needs are already
**Implemented**, per `hooks.md`'s own Status column, and combining them
needed nothing new: context editing (point 3) can narrow a `Skill` segment
exactly as example 1 narrows a `ToolResult` one — the value-class boundary
draws no distinction between provenance kinds, "Context: Edit, drop,
replace, mask" applies uniformly — and tool execution (point 2) is how "on
invoke" is answered for literally any tool, including a companion one this
plugin provides itself. Neither point needed to be stretched or reinterpreted
to fit this example; the architecture already had both pieces in the shape
they needed to be in.

```rust
use conway::plugin::{
    async_trait, ContentBlock, ContextHook, ContextHookCtx, ContextPayload, PathArgs,
    PermissionClass, Plugin, PluginManifest, Provenance, RenderKind, Tool, ToolCategory,
    ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};

struct SkillEntry {
    name: &'static str,
    description: &'static str,
    body: &'static str,
}

const SKILLS: &[SkillEntry] = &[/* ... */];

fn find_skill(name: &str) -> Option<&'static SkillEntry> {
    SKILLS.iter().find(|s| s.name == name)
}

/// Context-assembly half: narrows a full-body `Skill` segment down to a
/// one-line index entry, pointing at the tool below for the rest.
struct SkillIndexHook;

#[async_trait]
impl ContextHook for SkillIndexHook {
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        mut payload: ContextPayload,
    ) -> ContextPayload {
        for segment in &mut payload.segments {
            let Provenance::Skill { name } = &segment.provenance else { continue };
            if let Some(entry) = find_skill(name) {
                segment.content = vec![ContentBlock::Text {
                    text: format!(
                        "{}: {} (call read_skill(name=\"{}\") for the full document)",
                        entry.name, entry.description, entry.name
                    ),
                }];
            }
        }
        payload
    }
}

/// Tool-execution half: an ordinary `Tool` (`hooks.md` point 2,
/// Implemented) -- "fetch the full document on invoke" needed nothing new
/// at all.
struct ReadSkillTool;

#[async_trait]
impl Tool for ReadSkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("read_skill"),
            description: "Fetch a skill's full document by name, from the index.".to_string(),
            schema: schemars::schema_for!(ReadSkillArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(
        &self,
        call: conway::plugin::ToolCall,
        _ctx: conway::plugin::ToolCtx,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadSkillArgs = serde_json::from_value(call.arguments)
            .map_err(|e| ToolError::InvalidArguments { detail: e.to_string() })?;
        Ok(match find_skill(&args.name) {
            Some(entry) => ToolOutput {
                blocks: vec![ContentBlock::Text { text: entry.body.to_string() }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: vec![],
            },
            None => ToolOutput {
                blocks: vec![ContentBlock::Text { text: format!("no such skill: {}", args.name) }],
                is_error: true, // model-visible feedback, never a hard Err/crash
                truncation: TruncationPolicy::None,
                artifacts: vec![],
            },
        })
    }

    fn path_args(&self) -> PathArgs { PathArgs::None }
    fn render_kind(&self) -> RenderKind { RenderKind::Structured }
}
```

Run:

```rust
#[tokio::test]
async fn a_full_skill_body_is_narrowed_to_a_one_line_index_entry() {
    let out = SkillIndexHook.before_request(&ctx(), payload_with_full_skill_body()).await;
    let ContentBlock::Text { text } = &out.segments[0].content[0] else { panic!() };
    assert!(text.len() < SKILLS[0].body.len());
    assert!(text.contains("read_skill(name=\"git-commit\")"));
}

#[test]
fn read_skill_looks_up_the_full_body_by_name() {
    let entry = find_skill("git-commit").expect("git-commit is in the table");
    assert_eq!(entry.body, SKILLS[0].body);
    assert!(find_skill("does-not-exist").is_none());
}
```

**Why `Tool::invoke` itself is not exercised directly in this page's own
tests, and that is a documented facade limit, not a gap in this example.**
`ToolCtx`'s capability-handle fields (`chdir`/`events`/`subagents`) have
deliberately unexported concrete types (`authoring.md`'s "`ToolCtx`'s handle
fields" section), so a crate depending only on `conway` cannot construct one
by hand — only a live `ConwayBuilder` session can. `read_skill`'s actual
decision logic is factored into `find_skill`, a plain function taking no
`ToolCtx` at all, and *that* is what the test above drives directly; the
`Tool::invoke` wrapper around it is a thin adapter whose *registration*
shape (not its live invocation) is what `plugin_surface.rs`'s own
`plugin_tool_and_hook_register_through_the_builder` test proves end to end,
against a real `ConwayBuilder::build()`.

**The failure path**: an unindexed skill segment (a name the plugin's own
table doesn't know about) is left completely unchanged rather than deleted
— `an_unindexed_skill_segment_is_left_alone` asserts this. Fail SAFE here
means "leave the model with what it already had," the opposite of example
1's spill hook, where the safe failure was "leave the *original* content
in place rather than lose it to a failed write" — same principle
(never silently drop information), different concrete action, because the
two hooks fail in different directions for different reasons.

**GP-12, again: this is a plausible efficiency win, not a measured one.**
Nothing here has been run against a real workload to show the token savings
are real net of the extra tool round-trip a model now has to make to fetch
what it used to get for free. Do not read this section as conway
recommending progressive disclosure as a default; it demonstrates that the
architecture does not stand in the way of building it.

### 5. A status-line contribution

**Blocked.** `hooks.md` point 12 (`status.declare/1`/`status/1`) is
**designed-not-built**: *"No status-line plugin surface exists in the tree;
`conway-cli`'s status line reads only conway's own computed state."* No
board item names this point specifically (checked against `hooks.md`'s own
citation for it, which names none) — the closest tracked work is the
generalized declarative `hooks` surface, `01KZDC0RDRMMMJHX7SAFMM2Q5A`, the
same umbrella most of `hooks.md`'s unbuilt rows cite.

The operator's framing for this example was deliberate: *"the simplest
possible observer... an author's second hook should be easy."* The honest
version of that lesson, since the plugin-declared point doesn't exist yet,
is shown below using the ONE observer mechanism that really is implemented
and invoked constantly today — `EventSink`/`Event`
(`crates/conway-core/src/ports/events.rs`) — with the tier distinction
stated up front rather than blurred: this is **embedder-level**
(`conway::EventStream`), not **plugin-level**. `hooks.md` point 11
(`observe/1`) is *also* designed-not-built specifically as a
*plugin*-reachable point — *"the underlying mechanism... is implemented and
load-bearing; what is missing is any way for an in-process or remote
`Plugin` to subscribe to it the way an embedder's `EventStream` already
can."* A plugin cannot do what the snippet below does, today; an embedder
(the binary that calls `ConwayBuilder`) can.

```rust
use conway::{Envelope, Event};

/// `fn(&Envelope) -> Option<String>` -- no way to deny, retry, or block the
/// run from in here even if this function wanted to. That is the observer
/// shape (`concepts.md`'s "Observers vs participants"), enforced by the
/// type signature itself, not a convention this function happens to follow.
fn render_status_fragment(envelope: &Envelope) -> Option<String> {
    match &envelope.event {
        Event::ToolCallProposed { tool, .. } => Some(format!("running {tool}")),
        Event::AgentProgress { note } => Some(note.clone()),
        _ => None,
    }
}
```

Run:

```rust
#[test]
fn an_observer_reads_an_event_and_returns_no_reply_channel_exists_to_deny_with() {
    let envelope = Envelope {
        seq: 1,
        ts: Utc::now(),
        session: SessionId::new(),
        agent: AgentId::new(),
        event: Event::ToolCallProposed {
            call_id: "tc_1".to_string(),
            tool: ToolName::new("bash"),
            args: serde_json::json!({ "command": "ls" }),
        },
    };
    assert_eq!(render_status_fragment(&envelope).as_deref(), Some("running bash"));

    // An event with nothing to say produces nothing -- "may be absent" is
    // the WHOLE story here, not a special case: there is no `Err` variant
    // this could have returned instead.
    let quiet = Envelope { event: Event::AgentSpawned { /* .. */ }, ..envelope };
    assert_eq!(render_status_fragment(&quiet), None);
}
```

**This is deliberately the easiest example in the set.** It has no failure
path worth demonstrating beyond what its own type signature already
forbids: an observer that cannot compile a `Deny` variant into existence
cannot deny, full stop, and a slow or absent consumer is dropped from
delivery (`Event::Lagged`) rather than allowed to stall the runtime — real,
tested behavior of `EventSink::emit`, not a claim about this toy function.
The contrast with examples 1-4 is the point: every one of those returns a
value the runtime *acts on* and is bounded, fail-closed, order-independent
under composition; this one returns a `String` nobody but a status line's
own renderer ever looks at, and the runtime's behavior is identical whether
this function is registered, absent, or panics on every call (the CLI's own
status renderer would just show nothing).

## The two named acceptance verdicts, restated together

- **Example 1 (spill to file): the architecture supports it, and does so
  today.** The narrowing half (`ContextHook::before_request` editing a
  `ToolResult` segment) was already true before this item's own board
  history began — the design-spike-era claim that "neither additive context
  contribution nor a plugin's own tool lets a plugin narrow another tool's
  output" was accurate against the *pre-redirect* design, not against the
  hook-first one this codebase actually shipped. What was missing was
  narrower and infrastructural, not architectural: somewhere confinement-
  checked to put the spilled bytes, closed by `ContextHookCtx::artifacts`
  on 2026-08-07.
- **Example 4 (progressive skill disclosure): the architecture supports it,
  and needed nothing new to.** Both halves — narrowing a `Skill` segment at
  assembly time, fetching the full body via an ordinary companion tool —
  were already **Implemented** per `hooks.md`'s own Status column before
  this page was written. Nothing about this example stretched or
  reinterpreted either point; it is a straightforward composition of two
  already-real capabilities.

Neither verdict should be read as "the architecture is fully built" — six of
`hooks.md`'s fourteen points are Implemented and eight are
designed-not-built, and this page's examples 2, 3, and 5 each ran into one
of the eight along the way (durable compaction masks, the composed
inference-evaluated policy point, the plugin-reachable observer/status
points, respectively). The verdict is narrower and more specific: on the
*two cases the operator specifically chose to judge this architecture by*,
it holds up.

## Stale spec instructions found while writing this page

This item's own spec is dated 2026-07-30. Two things it names as open have
since landed, checked fresh against HEAD for this page:

- Board item `01KYTN3A9SPDMRG610YSB5QQXX` ("`TruncationPolicy::Artifact` is a
  documented lie") is **done**, not open — the spec's conditional
  instruction (*"if it is still open, ... this example's gap analysis is
  part of the deliverable"*) does not trigger; `TruncationPolicy::Artifact`
  no longer exists in `crates/conway-core/src/content.rs` at all.
- The spec frames example 1 as failing against "the pre-redirect design" and
  leaves open whether `ContextHookCtx::artifacts` alone closes it. It does
  not, alone — see the verdict above: the narrowing half was already real
  independent of that field, and `artifacts` closed the other, genuinely
  missing half. Stated as a correction at the relevant section above rather
  than left implicit.

## Where to go next

[`docs/plugins/README.md`](README.md) routes the rest of the set.
[`hooks.md`](hooks.md) is the contract every example above cites rather than
restates. [`authoring.md`](authoring.md) is the getting-started path this
page assumes; its "Testing your hook" and "Debugging" sections apply to
every example here unchanged. [`inference-hooks.md`](inference-hooks.md) is
the deeper reference for example 3's inference-evaluated half.
