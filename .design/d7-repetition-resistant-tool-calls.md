# D7 — Repetition-resistant tool calls: solving the loop class by idiomatic usage, not detection

Status: captured design direction, **out of scope for implementation now**
(operator direction, 2026-08-03). Recorded so that when the class of problem
returns — an agent repeating a tool call to no effect — the answer is shaped
by this framing rather than re-derived. Written against HEAD `0d80568`.

Origin: a refine-session discussion about `StepDigest` (repeated-step
detection, WI-086) and the whitepaper §4.5 contradiction — the whitepaper
says behaviors like repeated-step detection "belong in plugins and hooks the
operator opts into," while the code has it baked unconditionally into
`AgentLoop`. The operator's instinct: the whitepaper is right about scope,
**and** the deeper solution is not removal but designing tool calls that
resist the problem idiomatically. This note captures that design. The
§4.5/WI-086 contradiction is deliberately left OPEN; when it is revisited,
this note is the framing to revisit it with.

## The problem class

An agent calls the same tool with the same arguments repeatedly, making no
progress. Documented in the wild and expensive:

- OpenClaw #67399: 754 identical retries of a failing call, ~3.57M input
  tokens, ~$70, zero useful output.
- Hermes agent #67829: the model's text channel wrote "stop the loop" twelve
  times while its tool-call channel emitted the identical invalid call 25
  times in 6 minutes. Text decoding and tool-call decoding are separate
  paths — **a model that "knows" it is looping does not stop looping.**
- IronClaw #2240: identical failing calls retried to the 50-iteration cap
  with no duplicate detection.
- Framework responses: LangGraph RFC #6617 (progress assessment per step),
  DeepEval #2643 (loop-detection metric), the sentinel repo's loop taxonomy
  (hard loops / semantic loops / retry storms / scope creep).

The legitimate counterpoint (operator, 2026-08-03): identical repeated calls
are often USEFUL — polling a file for changes, watching a build, retrying a
flaky network call. Any design that punishes repetition per se is wrong.
The field consensus matches: threshold + escalation, never block early.

## Root cause — why the loop persists

A repeated call survives because **the repeat returns the same observation
as the original**. The model re-samples from an effectively unchanged
context, so the same call is the argmax action again — a sampling local
minimum. Errors-as-results makes it worse: a failure result that reads
identically each time is indistinguishable from "try again."

Consequence for mitigation design: the target is not "detect repeats." It
is: **an identical repeat should never return an identical observation.**
Break that and most loops self-terminate without any enforcement. This is
also why advisory notes (conway's current `StepDigest` mechanism) are the
weakest tier — the Hermes incident is direct evidence that text-channel
advisories do not move the tool-call channel.

## The design: three levers, all in the tool/result layer

Ordered by preference. None lives in the agent loop.

### Lever 1 — Make the repeat unnecessary: results carry their own comparator

The HTTP ETag/If-None-Match pattern. A `read` tool returns a `content_hash`
with the content; a caller checking "has it changed?" calls with
`if_changed_since: <hash>` and gets a 20-token "unchanged" instead of the
file again. Legitimate polling becomes cheap and explicit; pathological
repetition loses its excuse. The tool's own description states the contract,
so the prompt-facing surface teaches the non-repeating idiom **at the point
of use** — the operator's framing: solve the class by defining idiomatic
usage, not by policing usage.

### Lever 2 — Make the repeat informative: identical call, observably different result

If a call repeats with identical arguments, the second result says so:
"unchanged since seq 42," or a pointer to the earlier result instead of a
second full copy. Three conway-flavored wins at once:

- It changes the model's decision state — the argmax loop breaks because the
  observation changed. This IS the mechanism, not a detection layer.
- It serves GP-01's context economy — no duplicate tool output bloating
  context.
- It fits the append-only-record philosophy (whitepaper §4.4) — the log
  stays the truth; a dedup pointer is honest provenance (P-2), not hidden
  mutation. A human reading the transcript sees exactly what happened.

Today's `StepDigest` system-note is a weak, loop-external version of this
lever; levers 1–3 make it mostly unnecessary.

### Lever 3 — Make the repeat classifiable: tools declare determinism

The polling-vs-loop distinction is not "same arguments" but "could the
result change?" A file read between writes is deterministic given its
arguments; a `tail` of a growing log is not. Conway's idiom for this
already exists: capability-flagged interfaces (GP-06). A `Tool` declaring
`result_determinism: Deterministic | Volatile` lets anything downstream
reason about repetition:

- the schema description can say "re-calling with identical arguments
  cannot return new information";
- an operator-installed HOOK can enforce whatever policy it likes on
  deterministic repeats (warn, block, escalate) while volatile calls are
  left alone.

Policy — thresholds, warnings, circuit breakers — becomes a fifty-line hook
because the classification exists. That is GP-11's division of labour
(policy in hooks, mechanisms in core) and GP-03's identical-surface rule,
achieved by design rather than by removal.

## The class-level statement

**Design tool results so that repetition is either unnecessary,
self-announceing, or declaratively judgeable — and enforcement becomes an
operator choice rather than a harness opinion.**

Generalized: when a class of agent misbehavior traces to a sampling local
minimum, prefer changing what the environment returns (so the minimum
disappears) over adding a detector that fires after the fact. Detectors
fight the symptom at runtime; idiomatic design removes the incentive at the
interface. This is the same shape as the permission system's fail-closed
posture (P-13): the safe behavior is the path of least resistance, not a
guard bolted on beside the dangerous one.

## Relationship to existing steering and text

- **GP-11** (policy in hooks, not core): levers 1–3 are mechanisms, not
  policy; the policy they enable lives in hooks. This note is how the
  whitepaper §4.5 position becomes true by design.
- **Whitepaper §4.5 vs WI-086**: left open (see Status). The note's weak
  efficacy (advisory-only, per the Hermes evidence) means the status quo is
  not worth sentimental attachment in either direction.
- **GP-01 / P-2 / §4.4**: lever 2 is context economy and honest provenance,
  not a special case.
- **GP-06**: lever 3 reuses the capability-flag idiom.
- **GP-14**: whatever ships must be live; a determinism flag nothing
  consumes is a declaration trap of exactly the kind GP-14 exists to catch.
  Lever 3 ships with its first consumer or not at all.

## When this becomes work

Triggers to revisit: (a) the §4.5/WI-086 contradiction is picked up;
(b) a tool's result shape is redesigned for another reason — fold lever 1
in then; (c) an operator asks for loop enforcement — implement it as a hook
over lever 3, not in core.

Likely first slices when picked up: `content_hash` + `if_changed_since` on
the read tool (lever 1, self-contained); identical-result dedup pointers in
the session log (lever 2); `result_determinism` on the `Tool` trait with the
description-surface consumer (lever 3), then the StepDigest question on top.
