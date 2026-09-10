# Lens: operator surface

> **Read [`CONDUCT.md`](CONDUCT.md) first.** This lens assumes it.

---

## 1. The question

> **Can someone do a day's real work with this, and is the CLI conway's own team
> would reach for?**

`INTENT.md` §7a says this domain outranks architectural tidiness, and §8.7 says
it plainly: *this is a tool for doing the work, not a demonstration of a
philosophy. If the philosophy makes the tool unpleasant, the philosophy is
wrong.* §7a's operational form is the bar — conway's CLI must be good enough to
replace the harness currently in daily use, and **until it is, everything in
`INTENT.md` is untested.**

The CLI is a legitimately opinionated application (`CONDUCT.md` §1,
`INTENT.md` §8.2). Do not report an opinion in `conway-cli` as a defect. Report
one that is invisible, or that does not go away when switched off.

This is also, historically, the least-reviewed domain in the tree. Weight your
suspicion accordingly.

---

## 2. What to establish

**2.1 The full surface.** Every flag in `crates/conway-cli/src/cli.rs`, every
slash command, every subcommand. Produce the list — it is short enough to be
complete and nobody else has it written down.

**2.2 The daily-driver ladder.** `INTENT.md` §7b frames this as a ladder rather
than a switch. Locate the tree on it. What is the next rung, and what single
missing thing is holding it there?

**2.3 The non-coding user.** conway is meant to be equally usable as a
general-purpose way to reach a model from a script, a pipeline, or another
application (`INTENT.md` §1 — *both halves matter*). Walk that path. What would
someone who is not writing code find missing?

**2.4 Model and role control.** §5c holds that *a design that makes model changes
awkward has failed regardless of what else it gets right.* Check it directly.

**2.5 Things a model can call but an operator cannot type.** A recurring shape
here: a capability exposed as tools the model may invoke, with no operator-facing
command. Enumerate them.

**2.6 Uncommitted work in the TUI.** Check `git status`. Work that compiles, is
in the tree, and has no board item or acceptance is a finding — it is neither
finished nor reverted, and it will be inherited by whoever touches that file
next.

---

## 3. How to judge it

**Drive it, do not read it.** Where you can run the binary, run it. A flag list
read from `cli.rs` tells you what exists; it does not tell you that the thing
you want takes four commands and a config edit.

Run against an **isolated `$HOME`** (or config dir) so nothing touches the
operator's real `~/.conway`. Exercise everything headless yourself: one-shot,
piping, output formats, resume, permission modes, `sessions`/`routes`/`tools`/
`plugin` subcommands.

**You cannot drive the TUI; the operator can.** A subagent has no interactive
terminal, and reading `tui/` source and calling it driven is the failure mode
`CONDUCT.md` §5.2 names. Your brief says whether the operator is available this
run. If they are, the TUI is driven **by hand, from a script you write**:

- Return the script under its own heading, `## Manual TUI script`, after
  **Not checked**. **At most twelve steps.** Each step is one line — *type this
  → expect this* — and covers something only the TUI can show: model switching
  and the picker (§2.4), permission-mode cycling, `/context`, `/plugin` toggling
  and what changes, slash-command parity for agent lifecycle (§2.5), time to
  first token on the default model. Do not script what you already checked
  headless.
- Step 0 is a single command that starts the TUI against the isolated config
  dir you already prepared, copy-pasteable. The operator sets nothing up.
- The operator replies with the step number and one line of what happened.
  Fold those observations into your findings as evidence — cite them as
  "operator-driven, step N" — and only then finalise your return.

If the operator is not available, say so and put "TUI not driven — no pty"
first in **Not checked**. Two consecutive runs like that is a process defect
and the state of the union names it.

Judge against **friction**, not feature count. `INTENT.md` §2 is explicit that
conway is not trying to have fewer features — it is trying to make each one earn
its place. A missing capability and a capability that takes six steps are the
same finding to the person trying to work.

Judge against **weight** in the operator's sense, and measure it. A bare
one-shot run's `usage.input_tokens` (`conway -p "reply with exactly the word
pong and nothing else" --output-format json`) is the price of the first turn;
on 2026-09-07 a default configuration paid 15,856 tokens for "pong"
(`docs/scripting.md`). Record it alongside the length of `conway tools list`
and `conway plugin list`, and say what each is buying. Being strategic about
what reaches context is the bet underneath conway (`INTENT.md` §7a): a default
that commits things the work did not ask for is a finding, and so is a surface
the person has to keep up with that the work did not need. Binary size is not
weight.

---

## 4. Budget

- **Tool calls:** 25–40, and spend the upper half of that actually running things.
- **Return:** the shape in `CONDUCT.md` §4, **under 1,200 words**.
