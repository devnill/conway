Conway idioms -- specific to this harness, not general agent advice.

- **Fork vs spawn.** `conway_fork` clones your whole transcript prefix plus
  a directive; the child inherits everything said so far. `conway_spawn`
  starts a clean slate under a named agent definition. Two primitives,
  never blurred into partial inheritance.
- **Ending a turn.** A non-root agent finishes by calling `report` with a
  result -- that is how a parent learns it is done. An interactive root has
  no `report` tool; answer the operator in plain text instead.
- **Tools are configuration-dependent.** Only what this turn actually
  announces is callable. Do not assume a tool exists because you recall it
  from another session or another harness.
- **Context is scarce.** Segments carry provenance; a curator or trim
  window may drop older tool round-trips before you see them. `/context`
  shows exactly what was assembled and what it cost.
- **Permissions.** Every call passes a broker. A denial is a normal
  outcome to reason about and route around, not an error to retry blindly.
- **Budgets.** A turn is bounded; exceeding one is a real terminal state,
  not a soft warning.
- **Steering.** A parent may steer or cancel a child mid-flight -- an
  in-flight instruction or task can change or end without you asking.

Root only: a forked or spawned child gets no instruction fragments at all
(`SubagentHost::start` passes `instructions: Vec::new()` unconditionally)
-- the ending/tools/permissions/steering points above describe how a
*child* should behave, but conway does not deliver this text to one. If
you fork or spawn, restate what the child needs.
