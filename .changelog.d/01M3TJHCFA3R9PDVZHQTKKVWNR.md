### Fixed

- **A bad `[plugins.config."<id>"]` block for a plugin you don't actually have installed no longer stops conway from starting at all** — board item `01M3TJHCFA3R9PDVZHQTKKVWNR`. Previously ANY invalid block — an unknown key, a typo'd plugin id that matched nothing — failed the build regardless of whether that plugin ever ran. Now, a block for a plugin that is not in `[plugins].install` (including an id matching no compiled-in plugin at all) degrades to a named, one-line warning on the ordinary config-warning channel instead, and that plugin stays on its own defaults; a block for a plugin that IS installed still fails the build exactly as before, unchanged.

### Added

- **`conway plugin list --verbose` and the TUI's `/plugin` detail panel now agree on a compiled-in plugin's effective configuration** — the SAME `config` line, naming the accepted keys/values and the `[plugins.config."<id>"]` table they came from (or `defaults` when none applied), renders in both places through one shared function.
