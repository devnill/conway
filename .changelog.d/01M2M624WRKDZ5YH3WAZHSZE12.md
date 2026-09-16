### Fixed

- **The Anthropic model conway recommends to a new operator is no longer a generation behind** — board item `01M2M624WRKDZ5YH3WAZHSZE12`. Guided setup's Anthropic choice, the TUI model picker's own tests, and the doc-comment example in `tui/state.rs` all named `anthropic/claude-sonnet-4-6`; every non-test site now names `anthropic/claude-sonnet-5` consistently, so the copy-pasteable non-interactive snippet, the guided-setup write path, and the picker's own defaults never disagree. Two new `scripts/board-claims.md` predicates pin the current id and fail if it regresses.
