### Added

- **`conway -p` now catches `SIGTERM`/`SIGHUP`** (in addition to the existing `SIGINT` handling), reacting to the first delivery by cancelling the running root and giving it a bounded grace window to publish a real `Cancelled { reason: "signal: SIGTERM" }`/`"signal: SIGHUP"` terminal result before exiting with a documented code (129/143) — a second delivery of any of the three still forces an immediate exit. Closes a gap where a one-shot process killed by an uncaught termination signal (e.g. a backgrounded child hit by another tool's process-group kill) left no terminal record at all.

### Fixed

- **A panicked or grace-timed-out subagent's synthesized terminal result is now persisted to its own session log**, not just published to the in-memory agent tree — `supervisor::supervise`'s `Outcome::Synthesized` branch previously left the durable log ending mid-turn with no `agent_result` record even though a live, same-process awaiter already saw a result. Both this path and `AgentLoop::finish`'s normal path now route through one shared persistence function, each gated on winning the same publish race, so a session's log never gets two terminal records for one race either.
