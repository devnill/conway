# .design

This directory is a record of how and why conway was built — including
alternatives that were considered and rejected, and status banners marking
what has since been superseded or landed.

It is **not** user documentation and is not maintained as such. If you are
looking for docs on using conway, go to [`docs/`](../docs/) instead.

`extension-architecture.md` is the synthesis of `d1-transport.md` through
`d5-template-instrumentation.md` and supersedes those five specs where they
disagree with it.

`d7-repetition-resistant-tool-calls.md` is a captured design direction
(out of scope for implementation as of 2026-08-03): solving the
repeated-tool-call loop class by idiomatic tool-result design rather than
detection. It is the framing for any future revisit of the whitepaper §4.5
vs WI-086 (in-core `StepDigest`) question.
