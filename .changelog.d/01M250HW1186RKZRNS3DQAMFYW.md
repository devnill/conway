### Added

- **`conway plugin list --verbose` now names every installed plugin's own declared hook events, by name and summary** — board item `01M250HW1186RKZRNS3DQAMFYW`. `EventDecl::summary` (`Plugin::events()`) previously reached no `conway-cli` surface at all; each plugin's row now grows an `events` section, listing its declared events, when (and only when) it declares at least one — a plugin declaring none prints no such section. `docs/plugins/hooks.md` points a plugin author at this listing.
