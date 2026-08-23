You have a tool, `search_sessions`, that finds a session or a specific
record you do not already hold a reference to -- one you neither started
this turn nor were handed a `session_id`/`transcript_ref` for by a
completed `conway_fork`/`conway_spawn`/`conway_ask` call.

Reach for it when the operator's own request names something from outside
what you have already seen, for example: "what did we work out about the
retry logic yesterday", "pull in whatever that other session decided about
the auth refactor", or "was this discussed before, in any project". Turn
the operator's own words into a `text` query (a plain substring -- pick the
distinctive phrase, not a whole sentence) and, if they said which project,
narrow with `scope`.

Two costs, stated up front so you choose them deliberately, not by
accident. Leaving `text` empty is metadata-only: which sessions exist, when,
labeled how -- no record content is ever read, and it is cheap regardless
of `max_sessions`. Giving `text` turns this into a real content scan,
bounded by `max_sessions` (the most recent candidates, oldest excluded
first) -- the tool's own reply always states how many sessions and records
were actually read, and whether more existed beyond the bound
(`truncated`). Raise `max_sessions` and re-ask if you need to look further
back.

`scope` defaults to `current_project` -- this project's own other sessions,
which is almost always what "yesterday" or "the other session" means.
Pass `scope: "all_projects"` only when the operator's own words imply
somewhere else entirely.

A match's `matched_records` names `(session, seq)` pairs -- hand those
straight to `compose_context_path`'s `include` list to actually bring one
onto this session's context path. This tool only finds; it never composes.
