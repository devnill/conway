### Fixed

- **A small-window model in a role's fallback chain is no longer made permanently unreachable by a headroom sized for a large-window sibling** — board item `01M2TVEWVMPP69TZ17XSGWEW82`. Adaptive headroom (`routing.headroom_fraction`, on by default) used to take a fraction of the *smallest window it could find in `models.json`* for a role, once, at config-load time, and write that one number into `roles.<alias>.headroom_tokens` for every candidate in the chain. A chain entry absent from `models.json` was invisible to that scan — so a chain of a 32,768-token model conway had no metadata for plus a 1,000,000-token sibling derived `1000000 / 10 = 100000` tokens of headroom, applied it to the 32K model, and rejected every request routed there at the context-window gate. The warning that should have caught this used the identical scan, compared the derived value against the 1M window, and stayed silent. Headroom is now resolved **per candidate, at route time, against that candidate's own context window**, so both models in such a chain stay reachable for a prompt that genuinely fits.

- **A chain entry with no `models.json` record is reported instead of silently skipped** — new `WarningCode::ChainEntryContextWindowUnknown`. `config::merge::validate`'s headroom check now visits *every* chain entry of every role, each against its own window and its own resolved headroom, and names by role and by `"backend/model"` any entry it has no window for, along with the headroom that entry falls back to and the key that fixes it. A warning, not an error: an unnamed model is a legitimate state, and the router still resolves a window for it from dialect capabilities.

### Added

- **`[routing].models."<backend>/<model>".headroom_tokens`** — an explicit per-model headroom reservation, the most specific level of the precedence ladder. It lives in `settings.json` and deliberately **not** in `models.json`, which conway regenerates during first-run setup and provider management, silently dropping fields it does not recognize; headroom is operator policy and a metadata refresh must not be able to erase it. A zero at this level is a hard config error, as at the role and global levels.

- **`ExplainEntry::headroom_tokens`** — the reservation each candidate was actually checked against, carried per row on `RoutingExplain`'s report and rendered by `ExplainReport::render_text`. The report-level `ExplainReport::headroom_tokens` is retained as the role-wide figure but is now true for at most one row.

### Changed

- **Headroom precedence is a documented, tested four-level ladder**, highest first: operator per-model > operator per-role > conway-derived per candidate (`max(candidate_window / headroom_fraction, 2048)`) > `routing.default_headroom_tokens`. A conway-derived value never beats one the operator wrote; among operator-written values, the more specific wins. Nothing on the ladder is clamped — an operator value that makes a candidate unreachable is honoured as written, and the operator is told so by name at startup. The single implementation is `conway_core::routing::resolve_headroom`, shared by the router and the config facade so the two cannot drift.

- **`config::merge::load` no longer rewrites `roles.<alias>.headroom_tokens`.** The derived value is no longer written back into the operator's own override field — that write-back is what made a number conway invented indistinguishable, downstream, from one the operator typed, and made a correct precedence impossible. `roles.<alias>.headroom_tokens` now means only what you put there. `ConwayConfig::headroom_for_model(role, model_ref, window)` answers the per-candidate question at config-load time; `ConwayConfig::headroom_for` remains the role-wide answer for a caller with no candidate in hand.

- **`RoutingError::ContextTooLarge`** now reports the headroom resolved for the candidate it names, rather than the role-wide figure, which after this change is not the number that rejected that model.

- **`conway::config::schema::HEADROOM_FLOOR`** is now a re-export of `conway_core::capabilities::HEADROOM_FLOOR` (same value, `2048`): the fraction arithmetic moved into `conway-core` so the router can apply it per candidate, and the floor travels with the arithmetic rather than being restated on both sides of the crate boundary.

- **`docs/routing.md`** documents the four-level ladder, the `[routing].models` surface, per-candidate resolution, and both headroom warnings' current message shapes.
