# Liveness tests: the convention this crate's `*_seam.rs` files follow

Board item `01KYTXVX3SK7DR4X6ZRP8JZ88R` (Guard 2/2) audited liveness-test
coverage for conway's security- and economics-bearing mechanisms and found
the gaps this file exists to prevent from recurring. This is the convention,
written down where a contributor adding or reviewing a test for one of those
mechanisms will find it.

## What a liveness test is

A **liveness test** drives a **production entry point** — `Runtime::start_root`,
a real `Tool` through the real `ToolRunner`, the real `PermissionBroker`, the
real `Router`, `impl SubagentHost for Runtime` — with inputs that reach the
mechanism under test only by passing through every real intermediate layer,
and asserts on the **observable outcome**: the thing a caller outside the
mechanism (a user, a script, another agent) can actually see. Not an internal
signal the mechanism happens to expose for testing's own convenience.

## Why a unit test is insufficient

A unit test on the mechanism's own function — `matches_deny`, `apply_cache_hints`,
`contains_shell_metacharacters` — proves the function is correct for the inputs
the test author typed by hand. It proves nothing about whether production code
ever calls that function with those inputs, or calls it at all.

Every inert mechanism this repository has shipped had passing unit tests:

- `PermissionBroker`'s deny-match logic was unit-tested against a hand-written
  `rendered` string. The bug — `render_call` fusing a laundered control
  character onto the following token — lived entirely in the seam between
  `Tool::render`, `sanitize_rendered`, and `matches_deny`. No unit test on
  `matches_deny` alone could see it, because every such test already skipped
  the sanitizer and typed in a clean string.
- Anthropic prompt caching had a unit test on `apply_cache_hints` that
  hand-constructed segments already carrying a `cache_hint`. It passed for the
  entire outage this fixed, because the outage was upstream: every real call
  site hardcoded `CacheMode::None`, so no segment ever reached
  `apply_cache_hints` with a hint to map in the first place.
- `ExitCode`'s exit-4 mapping (`classify_runtime_or_routing`) has a passing
  unit test today (`crates/conway-cli/src/exit.rs`,
  `no_candidate_via_bare_routing_variant_is_four`) and is still unreachable
  from `-p` one-shot mode: no real error `oneshot::run` can produce ever
  reaches that classifier. The unit test exercises the mapping function, not
  the path that would call it. (`crates/conway-cli/tests/oneshot.rs` found
  and disclosed this — see "Two components, each tested, the connection
  isn't" below.)

A unit test tells you the mechanism works when triggered exactly as written.
It cannot tell you whether anything in production ever triggers it that way,
or at all.

## The observable-outcome rule

Even a test that drives the real production entry point can still fail to
prove anything, if what it asserts on is an **intermediate signal** rather
than the actual outcome a caller would see — because a correct result and a
silent bypass can produce the identical intermediate signal.

The canonical example: in `PermissionMode::AutoAllow`, the broker returns
`Allow` **without ever calling the gate** — that is the entire point of the
mode. So "the gate received zero calls" is what a *correct* AutoAllow
refusal-via-deny-rule looks like, **and** it is exactly what a *silently
broken* deny check that lets everything through would also look like. A test
that asserts only `gate.requests().is_empty()` cannot tell those two states
apart — it would pass against a fully bypassed permission system.

`permission_deny_laundering_seam.rs`'s AutoAllow test asserts on
`result.is_error` and the actual refusal text persisted on the `ToolResult`
instead — the thing an agent (and, downstream, a user) actually sees. That is
the observable outcome; `gate.requests()` was the intermediate signal, kept
in the test only as a secondary check, never as the proof.

**Any check that cannot fail is not a check.** Before trusting an assertion,
ask what it would look like if the mechanism were completely broken. If the
answer is "the same as when it works," the assertion is decoration.

## Two components, each tested, the connection isn't

The second failure shape this repository has produced is not a mechanism with
no test — it is a mechanism split across two components, each individually
well-tested, where nothing tests the seam between them. `attach_route_cache_hints`
landed with real tests; every call site that was supposed to feed it a live
`CacheMode` landed separately, hardcoding a placeholder, because choosing the
`CacheMode` felt like a later item's job. That item never came, and each
individual item's own tests stayed green throughout.

Watch for this pattern specifically when a mechanism spans a producer and a
consumer landing in different work items: test the producer's actual output
reaching the consumer, not each one in isolation against a value a test
author supplied by hand.

## The break-the-guard practice

Before trusting a liveness test as proof (not just as an assertion that
happens to pass), break the mechanism it is supposed to be guarding and
confirm the test fails — then restore the mechanism and confirm it passes
again. Report the broken-state output, not just the fact that you ran it.

Worked example (`eee6641`, the deny-laundering fix): with the fixed
`matches_deny` predicate stubbed back out, `permission_deny_laundering_seam.rs`'s
AutoAllow test was re-run and the persisted `ToolResult` showed `curl`
**actually executing** — not a refusal, not an error, the literal output of
the command the deny rule exists to block. That is what made the fix credible:
the test did not merely pass against the fix, it demonstrably failed against
the bug it was written to catch, in the same shape the bug actually took in
production (a full silent bypass, not a hard error). Restoring the fix made
the test pass again with no other change.

If you cannot describe what your test's failure output looks like against the
broken mechanism, you have not yet confirmed it is testing the mechanism at
all.

## Where these tests live

`crates/conway/tests/*_seam.rs` (`permission_pattern_seam.rs`,
`permission_trust_seam.rs`, `permission_revoke_seam.rs`,
`permission_deny_laundering_seam.rs`, `root_containment_seam.rs`,
`subagent_control_seam.rs`, `subagent_exfiltration_seam.rs`,
`context_admission_seam.rs`, `context_probe_overlay_seam.rs`) and
`crates/conway-runtime/tests/prompt_cache_e2e.rs`, `agent_loop_e2e.rs`,
`ask.rs` are the reference examples — each file's own header comment states
which seam it drives and why a unit test on the mechanism alone would have
missed the bug it regression-tests.

`context_admission_seam.rs` (board item `01KYXNB5TBJM2G8ZTJF85K1N09`) is the
"two components, each tested, the connection isn't" failure shape applied
to context admission: a real `ContextBuilder`'s `est_tokens`, through a real
`DeclarativeRouter` compiled from real config via `ConwayBuilder::build`
(no `.with_router` override), into a real `AttemptEngine` — with only the
`Backend` faked. Its primary assertion is the fake backend's own call count
(never called for an oversized context); its negative control (committed,
not run by hand) widens the model's window on an otherwise identical
fixture and asserts the flip: the backend IS called, exactly once.

`context_probe_overlay_seam.rs` (board item `01KYXNBKWK2DZ7JE3VRKC5FRJB`) is
the reachable divergence T-1's backstop actually exists to catch: not the
router-reads-config-vs-`AttemptEngine`-reads-live premise the item was
originally filed with (that one is architecturally precluded by
`CapabilityIndex::from_backends`'s own design — see that file's module doc
for the correction), but `builder.rs`'s optional `probe_on_startup` overlay,
which composes a model's capabilities from a *different* input set
(`ModelMetadataStore::defaults()` plus an empty override table) than
`Backend::capabilities()` does (the facade's own `models.json`-projected
override, which wins on precedence). A real `openai-compat`/`vllm_hermes`
backend, wired through the real `ConwayBuilder::build` against a loopback-only
`wiremock::MockServer` (the same already-in-tree technique
`conway-backends/tests/capability_probe.rs` uses for this identical
mechanism), lets the probe report an inflated window while `models.json`
pins a real, small one the backend's own `capabilities()` still honors. The
primary assertion is the mock server's own request log: no `POST
/chat/completions` was ever recorded. The committed negative control widens
only `models.json`'s override to match the probed value and asserts the
flip: the backend IS called, exactly once. Break-the-guard (T-1's
`caps.max_context_tokens >= required` check stubbed to always admit)
confirmed the backend is then actually invoked with the full oversized
request — the literal P-9 violation — before the stub was reverted.
