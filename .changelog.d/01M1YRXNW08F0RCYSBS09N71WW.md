### Added

- **`/model` opens an interactive picker with no plugin required** — board item `01M1YRXNW08F0RCYSBS09N71WW`. Bare `/model` now opens the same `Up`/`Down`/`Enter`/`Esc` picker a model-called `ask_question` tool already used, listing every reachable model — every `backend/model` pair named in any role's `chain`, plus every model recorded in `.conway/models.json` even if no chain names it — rather than requiring the optional `conway.ui` plugin to get anything but a plain-text dump. `/model <text>` with `<text>` not itself a valid `backend/model` pair now opens the same picker pre-filtered to entries matching `<text>`, instead of erroring; a syntactically well-formed pair still switches directly, unchanged. Installing `conway.ui` no longer changes `/model`'s behaviour at all, and nothing about `ask_question` itself changed.

- **`/model`'s picker now has a "make default" key** — board item `01M1YRXNW08F0RCYSBS09N71WW`. Pressing `d` on the highlighted model, without leaving the picker or switching this session's own running model, writes it to the head of the default role's `chain` through the exact same writer the `/settings → defaults` promotion row already uses — one source of truth, never a second chain rewriter — and shows the resulting chain in a notice.

### Changed

- **`/model`'s bare listing now includes `.conway/models.json`-only entries** — board item `01M1YRXNW08F0RCYSBS09N71WW`. Previously the listing (text or menu) only ever showed `backend/model` pairs an operator-configured role's own `chain` named; a model recorded in the local model-metadata file but not yet in any chain now shows up too.
