### Added

- **`edit`/`write` tool calls now show as a colored diff, not raw JSON** — board item `01M1YVEJB6GAPST5YZET4KZZE2`. The permission prompt for a pending `edit`/`write` shows a unified diff (current file bytes vs. what the call would produce) instead of the raw arguments dump, colored through new `[tui.theme.diff_add]`/`[tui.theme.diff_del]` slots; the raw arguments stay reachable underneath. Once approved, the settled transcript entry shows the identical diff, computed once at result time and folded under the existing `tool_preview_lines` cap/`Ctrl-E` toggle. A new `/diff` TUI command shows the cumulative diff of every path the session's agents have edited/written so far, against the bytes each path had the first time the session touched it; `conway sessions show <id> --diff` prints the same cumulative diff headlessly.

### Changed

- **`docs/interactive.md`/`docs/sessions.md`** document the diff-shaped permission prompt, `/diff`, and `sessions show --diff`, including the honest limits of reconstructing a "before" state from the call log alone.
