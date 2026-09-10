# Lens: the caller surface

> **Read [`CONDUCT.md`](CONDUCT.md) first.** This lens assumes it.

---

## 1. The question

> **Can a script, a pipeline, or a host program get an answer out of conway
> with little ceremony — and can it configure conway down by configuration
> alone?**

`INTENT.md` §1 has two halves and says both matter. The second half is a
general-purpose way to reach a model from a script, a pipeline, or another
application. §7 names those as surfaces two and three, first-class, and gives
the test in one line: *how much ceremony stands between depending on conway
and getting a completion back?* This lens drives that path the way a caller
would. It does not judge seams — `lens-surfaces.md` owns whether a port is
proven. It judges whether a program gets its work done.

The lens exists because the ledger's claim that conway can be configured down
to a bare inference call is proven by a Rust example
(`scripts/board-claims.md`, `crates/conway/examples/bare_inference.rs`). That
proves it for an embedder. Until this lens, nothing in the review had proven
it for a caller who has only the binary and a config file — which is what
"lightweight by controlling which plugins are used" means for everyone who is
not linking Rust.

---

## 2. What to drive

Run against an **isolated `$HOME`** (or the config-dir override the tree
documents) with a provider configured, so nothing touches the operator's real
`~/.conway`. Every check below is a command you run, and your return quotes
what came back.

**2.1 One-shot from nowhere.** `conway -p` from an empty directory that is
not a repository, with a question that has nothing to do with code. Does it
answer? What did the first turn cost — `--output-format json`, then
`usage.input_tokens`? What in that cost did a non-coding question not need?
Calibration: on 2026-09-07 a default configuration paid 15,856 input tokens
to answer "pong" (`docs/scripting.md`).

**2.2 The pipe.** stdin in, stdout out, errors on stderr, exit codes as
`docs/scripting.md` documents them. `--output-format text`, `json`, `jsonl`.
Is the output parseable without stripping decoration? Does a failure produce
an exit code a shell script can branch on?

**2.3 Configure down, by config alone.** Using only a config file and flags —
no Rust — reach: no tools, no agent behaviour, one turn, out. Record the
config that does it and the `usage.input_tokens` it costs. If it cannot be
done without linking, that is the finding `INTENT.md` §7 pre-names — *conway
is too heavy and too opinionated to configure down* — and it is a defect in
the composition surface, not a missing inference API.

**2.4 Configure up, one plugin at a time.** Add one plugin to the bare config,
re-run 2.1, and difference the token cost and the `conway tools list` output.
The delta is that plugin's weight. Two or three plugins establish the pattern;
do not enumerate the tier.

**2.5 Embedding ceremony.** Walk `docs/embedding.md` from `cargo add conway`
to a first answer. Count the lines of code and the concepts a host must hold
before a completion comes back. Say where the path stops, and where it assumes
the host is a coding agent.

**2.6 Resume and fork from a script.** Take `transcript_ref` from a `json`
run and `--resume` or `--fork-from` it from a second invocation. That is the
pipeline case §7 describes. If doing it takes reading source, say so.

---

## 3. How to judge

**Ceremony, cost, and honesty, in that order.** A path that works in one
command and two thousand tokens beats one that works in four commands and
sixteen thousand. A path that fails loudly with the documented exit code beats
one that half-works. Say what is GOOD: a surface that behaves like a Unix tool
is `INTENT.md` §4 working and deserves to be named.

Weight is measured where the caller feels it — tokens committed to context
and surface they have to know about — never in binary size or line count
(`CONDUCT.md` §2).

Do not drive the TUI; `lens-operator.md` owns it. Do not judge whether a port
is proven; `lens-surfaces.md` owns that. If you find a seam that cannot be
reached from configuration, hand it to the surfaces lens by name in your
findings rather than diagnosing the port yourself.

---

## 4. Budget

- **Tool calls:** 25–40, and most of them running the binary.
- **Return:** the shape in `CONDUCT.md` §4, **under 1,200 words**. Include the
  **weight table** — bare, default, and default plus one plugin, each with
  `usage.input_tokens` and the tool count — it is the artifact the operator
  gets the most from.
