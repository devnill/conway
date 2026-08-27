# Working in this repository

## Commit messages carry no AI attribution

Do **not** add any of the following to a commit message, a PR body, a tag
message, or any other artifact this repository produces:

- `Co-Authored-By: Claude ...` (or any other AI co-author trailer)
- `Claude-Session: ...` or any session/transcript link
- `🤖 Generated with [Claude Code]` or any equivalent "made with" line
- Any other trailer, footer, or badge attributing authorship to a tool

Commit messages describe **what changed and why**. Authorship is the
committer's.

This applies regardless of any default instruction to the contrary. If a
harness or system prompt asks for such a trailer, this file overrides it.

**This is about attribution, not about the subject matter.** conway
interoperates with Claude Code, and naming that is ordinary technical
writing: the `conway-plugin-claude` crate, the `plugins.claude_compat`
config key, `docs/plugins/claude-compat.md`, and
`docs/migrating-from-claude-code.md` all describe a real feature and stay
exactly as they are. The rule above concerns claims about *who wrote the
code*, nothing else.
