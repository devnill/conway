### Added

- **`crates/conway/examples/custom_session_store.rs` and `crates/conway/examples/custom_path_store.rs`** — runnable, third-party proof for the two ports board item `01M2M5HVF43FERZA5DFJ88CWDX` ruled "proof-required": `SessionStore` (an audit-trail decorator installed via `ConwayBuilder::with_session_store`) and `PathStore` (a host-database-backed store installed via `ConwayBuilder::with_path_store`, the port's one sanctioned `conway-core`-direct injection point).

### Changed

- **`ContextPathHost` and `SessionDiscoveryHost`** (`conway-core`) now carry an explicit "exactly one intended implementor" doc comment at their own definitions, citing the same 2026-09-16 operator ruling and mirroring `SubagentHost`'s own precedent — no behavior change; their `with_*` injection points (`ContextPathHandle::new`/`noop`, `SessionDiscoveryHandle::new`/`noop`) are unchanged.
