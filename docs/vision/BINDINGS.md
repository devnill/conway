# Non-Rust bindings — superseded

**This page has been retired. Its successor is
[`DESIGN-bindings.md`](DESIGN-bindings.md).**

This file was the original §7c binding survey (board item
`01M00QFMV84FTD3F6HVHCJRZN2`, D5-0), written 2026-08-14: a comparison of
Diplomat, UniFFI and `cbindgen` against conway's async streaming public API,
ending in a recommendation of Diplomat. Its findings were re-verified against
the tree and against each candidate's current documentation on 2026-08-24 and
restated at `DESIGN-bindings.md`, in the falsifiable-hypothesis register this
project's `DESIGN-*` pages use. **Read that page, not this one.**

## Why this file is a pointer rather than a deletion

It is cited by board item `01M0TV5PN8RR9NN97AWP09E6K7` (EMB-1) and by
`01M0TWSEH12002BGVG6G25XFB5`, and the reasoning it contained is preserved in
git history at commit `084736f`. A pointer keeps those citations resolving.

**It is also the evidence for a defect worth remembering.** This page was
linked from nothing — no row in [`README.md`](README.md), no reference from
any other page — for the ten days it sat on `main`. The 2026-08-24 review
consequently reported that no discussion of Diplomat, UniFFI or `cbindgen`
existed anywhere in the tree, "verified by search this run", and a board item
was filed to write a survey that already existed. An unindexed document is an
invisible one. `01M0TWSEH12002BGVG6G25XFB5` carries the rest of that finding.
