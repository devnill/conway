### Added

- **`roles.<alias>.params` in `settings.json` now reaches the backend** — board item `01M1YRZ2RJ0JRX6Z1SA4W4JBMB`. `ConwayConfig::routing()` previously always produced `SamplingParams::default()` for every role, discarding `params` entirely; it now threads a new, `deny_unknown_fields`-strict `RoleParams` sub-table (`temperature`, `top_p`, `max_tokens`, `stop`, `seed`, `extra`) into `conway_core::routing::RoleConfig::params`, so a role like `thinking` can carry `params.extra.reasoning_budget_tokens` for Anthropic's extended thinking, or a `fast` role can pin `params.temperature`, switched per-session with `/role`. See `docs/routing.md`'s new "Sampling and reasoning params" section for the worked recipe.
- **`conway routes explain` shows a role's effective sampling params** — board item `01M1YRZ2RJ0JRX6Z1SA4W4JBMB`. A new `params:` line (`--json`'s `"params"` key) prints the role's resolved `[roles.<alias>.params]` — every configured field as `key=value`, or the literal `default` when nothing is configured — so an operator can see what conway will actually send before a request goes out.

### Fixed

- **A `params` field a backend adapter never reads no longer fails silently** — board item `01M1YRZ2RJ0JRX6Z1SA4W4JBMB`. `conway-plugin-backends`' Anthropic and OpenAI-compatible wire adapters now log one `tracing::warn!` naming the ignored field and the backend (today: `seed`, on both adapters' primary request path) the first time a request carrying it is built, instead of silently doing nothing.
