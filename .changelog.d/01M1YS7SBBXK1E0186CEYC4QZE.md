### Added

- **A child's parent is now warned, live, when the child crosses 80% of one of its own budgets** — board item A5.6. `AgentLoop`'s existing per-agent `runway` warning (unchanged threshold arithmetic, one implementation, root and child alike) now also forwards a budget-dimension crossing to the parent as a `SystemNote { reason: "child_budget" }` appended to the parent's own session log, and as a live `Event::BudgetWarning { agent_id, limit, text }`; the TUI's `/agents` panel marks the crossing agent's row with a `!budget` tag the instant the event arrives, before the child is gone.
- **A child killed by its own deadline while a tool call was genuinely still in flight now names that call in its terminal result**, e.g. `"interrupted mid-call: bash(cargo test --workspace)"`, instead of a bare, unexplained `"cancelled"`. Scoped narrowly to the exact race this covers (no already-attributed cancel reason, and the agent's own deadline has actually elapsed) — an ordinary hard cancel that happens to also catch a tool call in flight is unchanged.

### Changed

- **`docs/agents.md`**'s "Budgets" section gains a "Warned before it trips, and told about the neighbors" subsection documenting both of the above and what remains unchanged (no automatic extension, no default value changes).
