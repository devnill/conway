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

Run with **`CONWAY_CONFIG_DIR` pointed at a scratch directory AND your working
directory outside `$HOME`.** `HOME=<scratch>` alone is not isolation — conway's
config search walks ancestor directories looking for `~/.conway`, so a scratch
`$HOME` that still sits beneath the operator's real home finds it anyway,
silently mixing real config into what you thought was a clean run. Confirm both
before trusting anything you measure. Exercise everything headless yourself:
one-shot, piping, output formats, resume, permission modes,
`sessions`/`routes`/`tools`/`plugin` subcommands.

**Bounding a run.** macOS ships no `timeout`(1) — do not assume it exists.
Inside a `tmux` pane, you already bound the run yourself: poll `capture-pane`
in a loop with a hard cap on iterations, then `tmux kill-session` once you hit
it, whether or not the command finished. Outside `tmux`, background the
command, capture its PID, `sleep` your bound, then `kill` it if it is still
running (`gtimeout`, from Homebrew's coreutils, works too if it happens to be
installed — do not depend on it being there).

**Drive the TUI yourself. `tmux` is a pty.** For three rounds this lens said a
subagent cannot drive the TUI and must hand a script to the operator. That
premise was false, and it cost three rounds of coverage: `tmux` gives you a real
pty, the TUI cannot tell it from a terminal, and you drive it from `Bash` with no
MCP server and no operator in the loop. The loop is Playwright's, for a terminal:

```
tmux new-session -d -s rev -x 200 -y 50 -c <cwd> "sh -c 'CMD; echo [EXIT=$?]; sleep 600'"
tmux send-keys -t rev "/model" Enter      # also: Escape, or a bare key like "n"
tmux capture-pane -t rev -p               # the screen, as greppable text
tmux kill-session -t rev
```

Two failures that cost a retry each, so do not rediscover them. **The pane dies
the instant your command exits**, taking the scrollback with it — always wrap in
`sh -c '...; sleep 600'`. And **size the window explicitly** (`-x 200 -y 50`) or
the TUI wraps at 80 columns and your assertions on rendered lines break.

**This is now the expected coverage, not a stretch goal.** End-to-end is where
this project's defects actually live: the first tmux-driven run of guided setup
found that it writes `settings.json` to the config dir and `models.json` to the
project dir, so `cd` collapses the context window 12.8x — the root cause of a
session loss that had been on the board for a week as an unexplained symptom. No
amount of source reading had found it, and two reviewers had already looked.
Budget the upper half of your run for driving, and drive the paths an operator
actually walks: first launch with nothing configured, guided setup end to end,
model switching and the picker, permission-mode cycling, `/context`, `/plugin`
toggling, slash-command parity for agent lifecycle (§2.5), time to first token.

**Reading `tui/` source and calling it driven is still the failure mode**
`CONDUCT.md` §5.2 names. `capture-pane` output is the evidence — paste the lines
that prove the finding, exactly as they rendered.

**The operator is for judgement, not transcription.** A human describing a screen
is lossy and slow, and you no longer need them for it. Ask them only what
`capture-pane` cannot answer: whether something *feels* wrong, whether a wait was
tolerable, whether an error read as helpful. If you still want a hand-run script,
it is a supplement to your own driving, never a substitute for it.

**If you asked the operator something and have not heard back, say your return
is provisional on it.** Your own `tmux` driving no longer waits on anyone, but a
judgement question you put to the operator still does — file your findings from
what you drove yourself, and name the open judgement question separately rather
than silently treating an unanswered one as settled.

"TUI not driven — no pty" is **no longer an acceptable line in Not checked.**
If `tmux` is genuinely unavailable in your environment, say that specifically and
name what you tried.

Judge against **friction**, not feature count. `INTENT.md` §2 is explicit that
conway is not trying to have fewer features — it is trying to make each one earn
its place. A missing capability and a capability that takes six steps are the
same finding to the person trying to work.

Judge against **weight** in the operator's sense, and measure it. A bare
one-shot run's `usage.input_tokens` (`conway -p "reply with exactly the word
pong and nothing else" --output-format json`) is the price of the first turn;
on 2026-09-07 a default configuration paid 15,856 tokens for "pong"
(`docs/scripting.md`). Record `steps_taken` alongside `usage.input_tokens` and
**discard the run if `steps_taken` is not 1** — only a single-step run is
comparable across rounds; anything else already paid for a second turn the
"first token" number was not supposed to include. Record the token count
alongside the length of `conway tools list` and `conway plugin list`, and say
what each is buying. Being strategic about
what reaches context is the bet underneath conway (`INTENT.md` §7a): a default
that commits things the work did not ask for is a finding, and so is a surface
the person has to keep up with that the work did not need. Binary size is not
weight.

---

## 4. Budget

- **Tool calls:** 25–40, and spend the upper half of that actually running things.
- **Return:** the shape in `CONDUCT.md` §4, **under 1,200 words**.
