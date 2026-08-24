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

Judge against **friction**, not feature count. `INTENT.md` §2 is explicit that
conway is not trying to have fewer features — it is trying to make each one earn
its place. A missing capability and a capability that takes six steps are the
same finding to the person trying to work.

---

## 4. Budget

- **Tool calls:** 25–40, and spend the upper half of that actually running things.
- **Return:** the shape in `CONDUCT.md` §4, **under 1,200 words**.
