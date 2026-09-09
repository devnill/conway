### Added

- **`conway.trim`'s turn window is now operator-configurable, and the first per-plugin `settings.json` setting** — board item `01M1YVM9CHFCJ6112XDYHCFS84`. A new `[plugins.config.<id>]` table (`PluginsConfig::config`) carries free-form JSON per plugin, validated and applied by the plugin itself through a new `Plugin::configure(&mut self, value: &serde_json::Value) -> Result<(), PluginConfigureError>` trait method (defaulted, so every existing `Plugin` implementor keeps compiling unmodified). `conway.trim` is the first real consumer: `[plugins.config.conway.trim] { "keep_turns": 3 }` overrides the default 8-turn window; `keep_turns` must be a JSON integer `>= 1`, and any other key under that table is refused by name rather than silently ignored. `crates/conway-cli/src/first_party_plugins.rs`'s new `apply_plugin_config` threads the table from `settings.json` to each installed plugin's own `configure` call, run immediately after `bundle()` and before `ConwayBuilder::install_selected` filters to what was actually selected.
- **`docs/plugins/trim.md`** — a dedicated page for `conway.trim`, which previously had none in this doc set.

### Changed

- **`docs/plugins/authoring.md`'s "Configuration" section** now describes `[plugins.config.<id>]`/`Plugin::configure` as built, with `conway-plugin-trim`'s `keep_turns` as the worked example, and adds a `configure()` entry to "What else a plugin can declare."
- **`PHILOSOPHY.md` §6** no longer states that a first-party plugin ships with "no `settings.json` field of its own" — that rule described the tree accurately until this item, and now describes the built `[plugins.config.<id>]` seam instead.
- **`docs/plugins/README.md`** — the "What `[plugins].install` decides, and what it does not" paragraph and the `conway.trim` bullet are updated to match; `conway.trim` now links to its own page.
