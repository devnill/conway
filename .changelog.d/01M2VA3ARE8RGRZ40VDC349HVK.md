### Fixed

- **`conway plugin list` reports a plugin's configured value, not its unconfigured default** — board item `01M2VA3ARE8RGRZ40VDC349HVK`. With `plugins.config."conway.trim".keep_turns = 3` in `settings.json`, the listing printed the compiled-in default (`8`) while the running system correctly used `3`, because it read the unfiltered candidate scan, which never applied `[plugins.config.<id>]`. Both `conway plugin list` and the TUI's `/plugin` browser now read through `first_party_plugins::configured_bundle_plugins` — the same unfiltered scan (so available-but-off plugins still appear) with each named candidate's own config table applied through the same `apply_plugin_config` the real build already runs, never a second config-resolution path.

### Added

- **`conway plugin list --verbose` states where a plugin's configuration came from.** Each compiled-in plugin's block now carries a `config` line naming the `[plugins.config."<id>"]` table that was applied and the keys conway accepted (`keep_turns = 3 -- from [plugins.config."conway.trim"] in settings.json`), or saying plainly `defaults -- no [plugins.config."conway.trim"] entry in settings.json`. A mistyped key inside a table was already a hard error; a mistyped plugin **id** matches no candidate and is silently never applied, and the `defaults` line is how an operator sees that their table did not land.

### Changed

- **`docs/plugins/trim.md`** named `first_party_plugins::installed_plugins` as the function `conway plugin list` calls to apply config. It never called it (and that function also filters to `[plugins].install`, which would drop every uninstalled row). The page now names the function actually called, records that the TUI browser agrees, and discloses the one remaining divergence: the `config` provenance line is printed by the headless listing only.
