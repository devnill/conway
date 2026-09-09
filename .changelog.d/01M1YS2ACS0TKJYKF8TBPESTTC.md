### Added

- **Permission decisions are now recorded in the transcript** — board item `01M1YS2ACS0TKJYKF8TBPESTTC`. `PermissionBroker::decide` writes a `permission_decision` record (call, decision, whether it came from the operator/a rule/a hook/the current mode, how long a prompt actually waited, and the reason behind any denial) for every call it resolves — prompted or not — from the ONE place it already decides, never a second write site. `conway sessions show` prints the record like any other; one-shot `conway -p` in `text` mode now prints a denied call's reason to stderr (closing a known dogfooding friction), and `jsonl` carries the full record on the live event stream.
- **The TUI transcript now shows prompted permission decisions, and `/context` counts them** — board item `01M1YS2ACS0TKJYKF8TBPESTTC`. A dim one-line note (`"allowed once · waited 4m 12s"`, `"denied with feedback: …"`) renders directly under the tool call it belongs to, for a decision that actually reached the operator's own gate — a pattern/rule/hook/mode resolution stays silent. `/context` now also reports a `permission decisions: N` count, read from the agent's own record history (never from `ContextReport::segments`, which this record kind deliberately never appears in).

### Changed

- **`docs/permissions.md`, `docs/sessions.md`, `docs/scripting.md`** document the new `permission_decision` record and its live `jsonl`/`text` visibility.
