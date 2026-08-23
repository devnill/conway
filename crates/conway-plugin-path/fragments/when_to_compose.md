You have a tool, `compose_context_path`, that changes what is actually on
this session's context path going forward -- not what you say next, but
which logged records the harness sends as context on later turns.

Reach for it when the operator's own request implies a change to what you
are keeping in view, for example: "forget that dead end and just use what
we figured out in the other session", "bring in what we discussed when you
looked at the auth code", or "stop carrying that whole exploratory tangent
forward". Some inference of the operator's intent is expected here, the
same way it is obvious a task needs context to work from at all -- you do
not need the operator to spell out session ids and sequence numbers
themselves.

To name a record from elsewhere, use a session id you already hold -- most
often the `session_id`/`transcript_ref` a completed `conway_fork`/
`conway_spawn`/`conway_ask` call already returned to you, with `seq: 0`
being that child's own first turn. If the operator names a session you
neither started nor spawned (e.g. "yesterday's conversation"), and
`search_sessions` is available, use it first to find the `(session, seq)`
to include here. To leave out one of your own turns, name
its `seq` in `exclude`; you do not need to know another session's internal
numbering to drop your own content.

Your own ongoing conversation stays on the path unless you explicitly ask
to drop it (`drop_own_tail: true`) -- composing is additive by default, not
a silent reset. If a call is refused because it would leave a tool call or
its result stranded, the refusal names both halves and offers a repair
(keep the other half too, or drop it as well); retry with one of those
instead of assuming the call went through.

`drop_own_tail: true` genuinely and durably drops this session's own
earlier history, not just for this reply -- it will not quietly come back
on a later turn.
