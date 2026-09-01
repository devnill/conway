# Presenting to a human — the shared rule

This file is the single copy of ideate's rule for prose a person reads.
Surfaces that address the user point here instead of restating the rule — a
second copy drifts, and this project has paid for that defect before. Read
this file before writing output a human will read, and apply it to ALL of
that output: ordinary conversation, summaries, close-outs, reports,
questions — not only the confirm gates where work is approved. The gates
(`execute` Step 2, `refine` Step 7) add gate-specific shape on top of this
rule (the four-part story, the review-not-approval framing); this file is
the layer underneath, and it governs everywhere a person is the reader.

## What this does not govern
Machine-facing payloads stay dense. A worker's spec, a reviewer agent's
findings returned to the coordinating skill, a decision struct returned to
the autopilot loop — density is correct there, because the reader is another
agent that needs precision, not narration. The coordinator is the translation
layer: it reads the dense payload and applies this rule when it re-presents
the content to a person.

## The rule
- **Expand references on first use.** The first time a policy id, a record
  ULID, a board item id, or any internal identifier appears in prose a human
  reads, inline what it means in plain language — "the rule that demo
  machinery can't ship in the real binary (GP-03)", never a bare "GP-03".
  The bare id rides along in parentheses at most, never as the label itself.
- **Never make a reader resolve an identifier to follow a sentence.** If
  understanding a sentence requires knowing what an id refers to, the
  sentence is not finished.
- **Plain language first, depth on pull.** Lead with what the reader needs
  in order to understand or decide; offer the dense artifact (the spec body,
  the raw finding, the full list) as something they can ask for, not
  something they must wade through.
