# Manual test plan: virgin install to a working app

A repeatable procedure a person follows to verify conway end to end, from a
machine with no conway state to using conway to build something small.

**Why this lives in `docs/` rather than beside the design pages.** It is
maintainer material, not a user guide, which normally argues against `docs/`.
[`dogfooding.md`](dogfooding.md) is the precedent: process a maintainer
follows, kept here because it is read alongside the pages it tests. This plan
is the executable twin of [`getting-started.md`](getting-started.md) — that
page is a *claim* about what happens when you follow it, and this plan is how
the claim gets tested.

**Status: UNWALKED.** Every step below is written from the code and the
existing docs, not from a completed run. That is the same prose-checked-
against-prose weakness `dogfooding.md` warns about, and it is why the first
walk's job is as much to correct this plan as to follow it. **Annotate as you
go; do not fix silently.**

---

## Before you start

### Record friction where it happens

```console
scripts/dogfood-note.sh friction --title "…" --body "…" --human "Dan Singer"
```

`dogfooding.md`: *"The moment something is awkward, record it — do not
reconstruct it later. Reconstructed friction is the same failure mode as prose
checked against prose."*

**Do not work around problems silently.** A walk that quietly routes around
three things and reports success has destroyed its own evidence. Note it, then
decide whether to continue or stop.

### P0 — the binary must be current

`conway` on `PATH` is a symlink to `target/release/conway`. A release build is
**not** produced by `cargo test` or `cargo clippy`, so it can be arbitrarily
stale while every gate passes.

| | |
| --- | --- |
| **Do** | `conway --version`, then `ls -la $(which conway)` and check the mtime of what it points at against `git log -1 --date=short` |
| **Pass** | The binary is newer than the last commit you intend to test |
| **Fail** | The binary predates the work under test — **stop and rebuild**, or every result below is about old code |

```console
cargo build -p conway-cli --release
```

> **Known at the time of writing (2026-08-30):** the installed binary dated
> from **Aug 25**, predating this entire cycle. If you skip P0 you are testing
> none of it.

### P1 — virgin state

Conway reads a **user** layer (`~/.conway/`) and a **project** layer
(`<project>/.conway/`). A virgin walk needs the user layer absent and a
project directory that has never seen conway.

| | |
| --- | --- |
| **Do** | Confirm `~/.conway` is absent or moved aside; work in a **new scratch directory**, not the conway repo |
| **Pass** | `ls ~/.conway` reports no such file; your scratch dir has no `.conway/` |
| **Fail** | Either exists — the run inherits state and proves nothing about first use |

> **Do not delete the conway repo's own `.conway/`.** It holds
> `permissions.json`, which is **tracked by git**, plus 18 dogfood records and
> the memory plugin's store. Walking in a separate scratch directory avoids it
> entirely.

---

## Part 1 — first run

| Step | Do | Pass | Fail |
| --- | --- | --- | --- |
| 1.1 | Run `conway` with no config anywhere | The guided first-run setup appears | It errors, or drops to a prompt with no backend and no explanation |
| 1.2 | Watch the local probe, which runs FIRST, before any hosted menu | It reports looking for Ollama on `127.0.0.1:11434` | It scans something else, or hangs |
| 1.3 | If Ollama answers, read the offer | It names the model it found and offers `Enter` to use it, any other key to see hosted providers instead, or `Esc` to skip setup entirely | The wording says "any other key" without distinguishing `Esc`, or `Esc` browses instead of skipping |
| 1.4 | If Ollama does **not** answer (stop it first if it's running, to reach this path), read the hosted menu | Exactly three choices — **Anthropic**, **OpenAI**, and **Ollama Cloud** | A fourth option appears, one of the three is missing, or the list is empty |
| 1.5 | Configure one provider (either path above) | It saves, verifies with a real request ("it works"), then asks `Add another provider? [y/N]` | No follow-up prompt appears at all, or it asks before verifying |
| 1.6 | Decline the follow-up (anything but `y`/`Y`) | You land straight in a working session with the ONE provider just configured | It declines the whole run, discarding the provider that just succeeded |
| 1.7 | Answer `y` instead and configure a second, different provider | Both backends are saved, and the default role's chain names both, in the order you added them | Only the second provider is reachable, or the first is silently dropped |
| 1.8 | With one or two providers configured this way, send a real prompt | It completes normally | `routing error: no candidate for role default (0 considered)` — the exact defect board item `01M1A2HKMDGNK961ZFV1EGZDQ0` fixed: guided setup used to save `backends.<id>` but never `default_role`/`roles`, so the file it left behind could not route even though its own verification step had just proven a working chain |
| 1.9 | Decline setup instead, at the very first opportunity (`Esc`) | The app stays open and usable rather than exiting | It exits, or leaves a half-written config |

> **Closed 2026-08-30, board item `01M19XZPZD5CKRB83JJS42E8JN`:** `HOSTED_CHOICES`
> now has three entries and Ollama Cloud is one of them (`dialect: "ollama"`,
> `base_url: "https://ollama.com/v1"`, default model `glm-5.2`). Part 2.1 below
> is a guided path now — if the wizard does not offer it, or picking it does not
> reach a working entry, that is a **regression**, not the expected gap this
> note used to describe.
>
> **Closed 2026-08-30, board item `01M1A2HKMDGNK961ZFV1EGZDQ0`:** this Part's
> rows used to describe the hosted menu as always shown alongside the local
> probe ("three hosted choices ... plus a local-server probe"), which was never
> accurate — a **successful** local probe short-circuits the hosted menu
> entirely (`run_guided_setup`'s own structure: the hosted `loop` is only
> reached when the local branch does not `return`). The same fix also added
> the "add another?" follow-up (rows 1.5–1.7) and closed the gap where a
> completed guided run produced a config that could not route at all (row
> 1.8) — if 1.8 fails, that is the regression this whole item exists to catch.

---

## Part 2 — backends

### 2.1 Ollama Cloud (hosted, credentialed, OpenAI-compatible)

**No longer a fall-off case as of board item `01M19XZPZD5CKRB83JJS42E8JN`
(2026-08-30): first-run and `/settings` both offer it directly, third in the
list after Anthropic and OpenAI.** If step 2.1.1 below does not reach a
working entry through the guided path, that is a regression against this
item, not the expected gap the earlier version of this row described.

| Step | Do | Pass | Fail |
| --- | --- | --- | --- |
| 2.1.1 | Add it through first-run or `/settings` -> providers -> add Ollama Cloud, using an `OLLAMA_API_KEY` you have (try once with it already exported, once without, to walk both credential styles) | You reach a working, verified entry both ways | Either path fails, or only one credential style works — **record which** |
| 2.1.2 | If you need to configure by hand instead: an `openai-compat` backend, `dialect: "ollama"`, `base_url: "https://ollama.com/v1"`, and your key | `conway routes explain <role>` names it | Config is rejected — capture the exact error |
| 2.1.3 | Send one real prompt, then a prompt that forces at least one tool call | Both return real completions | Transport or auth error on the first; on the second, `bad request: invalid message content type: <nil>` would mean the `glm-5.2` content-type workaround (`openai_compat/wire.rs`) regressed — capture verbatim |

> Ollama Cloud has bitten conway before and the fixes were reactive, so watch
> these specifically: a rejected request field
> (`openai_compat/wire.rs`), and a capability probe that **used to 404 on
> both `/models` and `/api/tags`** (`probe_impl.rs`) — live as of 2026-08-30
> both return 200, so the three-tier fallback to `/api/version` should not
> even trigger; if it does, that is itself worth noting (a regression on
> Ollama Cloud's side, not conway's). If either the wire quirk or the old
> 404 shape shows up, it is a known shape, not a new mystery.

### 2.2 A local model

llama.cpp **is** supported — dialect `llamacpp-server`. Five dialects ship:
`openai`, `ollama`, `vllm-hermes`, `lm-studio`, `llamacpp-server`.

| Step | Do | Pass | Fail |
| --- | --- | --- | --- |
| 2.2.1 | Start a local server — llama.cpp preferred; local Ollama if impractical. **Say which you chose and why** | Server answers on its port | — |
| 2.2.2 | Configure it, using the matching dialect | Accepted | Rejected — capture the error and whether the dialect name was the problem |
| 2.2.3 | Send one real prompt | A completion returns | Capture verbatim |
| 2.2.4 | Check `/settings` shows both backends and their status | Both listed, status accurate | A backend is missing or its status is wrong |

---

## Part 3 — a first session

| Step | Do | Pass | Fail |
| --- | --- | --- | --- |
| 3.1 | Follow [`getting-started.md`](getting-started.md) **literally**, as a new user | Each instruction works as written | **Any** step that is wrong, incomplete, or assumes knowledge a new user lacks is a finding against that page |
| 3.2 | Enable bash per that page | A shell command runs after you approve it | The permission prompt does not appear, or approving does not run it |
| 3.3 | Ask for a file to be read using a `~/`-prefixed path | The file is read | **Regression** — this is item `01M10HSEN`; a "could not be found" here means the tilde fix did not hold |
| 3.4 | Ask for a path that cannot be expanded, e.g. `~nobody/x` | The refusal **names tilde explicitly** | A generic "not found" — the whole point of that item was to kill the plausible dead end |

---

## Part 4 — what this cycle shipped

One row per item. This is the acceptance test for the work, so record a
result for each even when it is "nothing visible."

| Item | Do | Pass | Fail |
| --- | --- | --- | --- |
| `01M10HSEN` tilde | Covered by 3.3/3.4 | As above | As above |
| `01M0Y6RYZ` marketplace | `/plugin install https://github.com/devnill/claude-marketplace beepboop` — the exact command the defect was reported from | Installs, or fails with an error naming what conway expected | A JSON parse error about HTML means layer 1 regressed |
| `01M1895V6B` semver edges | Nothing consumes `ui.form` visibly yet | No spurious version-mismatch error anywhere | A version mismatch surfaces during normal use |
| `01M11XYAD` surface coherence | **Sit in the app and judge it.** Six rules in [`vision/DESIGN-surface-coherence.md`](vision/DESIGN-surface-coherence.md) | It reads as one tool; each thing has one obvious home | Anything feels scattered — **this is the subjective half and it is the point**; the page was written from a complaint about feel |
| `01M0WWPA7` conway.ui | Enable `conway.ui`; check `/plugin` and its docs | Behaviour matches what the docs say about it | The docs claim something you cannot observe |
| `01M18Q7P25` default model | Open `/settings` → defaults | Default role and default model both shown, **both labelled as defaults**; default model is read-only; the role cycle offers **only roles you declared** | A `default` role you never wrote appears — that was a real defect, fixed; its return is a regression |
| `01M18Q8YWW` citations gate | Nothing operator-visible | — | Skip unless something surprises you |

---

## Part 5 — plugins

**beepboop first, deliberately.** 25 hook events, one audible cue each, no
MCP/skills/agents to confound the signal. If hooks fire wrong you hear it in
minutes. ideate is the opposite kind of test and takes real sessions.

| Step | Do | Pass | Fail |
| --- | --- | --- | --- |
| 5.1 | Install beepboop via `[plugins].claude_compat` | Loads; `/plugin` lists it | — |
| 5.2 | Drive a session and listen | Cues fire at sensible moments | A cue on `/ask` would mean the fork/spawn narrowing regressed |
| 5.3 | Run `/beepboop:config` | The command completes | **Note:** beepboop's own body names a stale cache path (`.../cache/beepboop` vs `.../cache/marketplace/beepboop/1.4.0`). If it fails **there**, that is beepboop's bug, not conway's — report the distinction rather than scoring it against the tilde fix |
| 5.4 | Install ideate via `claude_compat` | Loads | — |
| 5.5 | MCP tools | Confirm rather than assume they work | — |
| 5.6 | Skills — `/ideate:refine`, `/ideate:execute` | Load, appear, run; cross-references to each other and `shared/` survive translation | A skill that loads but cannot reach its siblings |
| 5.7 | Agents — does `ideate:worker` resolve and run? | Its declared tool restrictions survive | **A dropped restriction is a permission change you did not ask for — flag it loudly** |
| 5.8 | Hooks — ideate declares 7 events | They fire at the right moments | ideate's hooks record process history, so a hook at the **wrong time corrupts a record** rather than playing a wrong noise |

> ideate ships an **empty** `commands/` directory, so it proves nothing about
> command translation. beepboop's `config.md` is that path's only real proof.

---

## Part 6 — build something small

The point of the whole plan: not "does it start" but "can you work in it."

| Step | Do | Pass | Fail |
| --- | --- | --- | --- |
| 6.1 | In the scratch directory, use conway to build a small app — something with a few files that actually runs | It gets built and runs | — |
| 6.2 | Use more than one agent if the work suggests it (fork or spawn) | The tree behaves; `/tree` and `/context` show what you expect | You cannot tell where context came from |
| 6.3 | Recover from at least one bad turn | You can steer, cancel, or fork away from it without losing the session | — |
| 6.4 | Watch the per-turn cache percentage | It is non-zero and moves sensibly | A steady zero means caching has stopped working and looks exactly like an expensive workload |
| 6.5 | Do one real board operation through conway — `/ideate:refine` or `/ideate:execute` on a real item | It completes; **cite the item id** | — |

---

## Part 7 — the verdict

**Rung one: is conway usable for this class of work alongside the incumbent
harness — yes or no?**

**A hedged answer is a no.** This is the operator's judgment and nobody
else's; it cannot be delegated, inferred, or assembled from the steps above.

Then:

1. File every conway defect the walk found as **its own board item** — not as
   a workaround buried in prose.
2. Correct [`getting-started.md`](getting-started.md) so a literal
   follow-through works, including all three hosted choices (Ollama Cloud
   joined the wizard 2026-08-30, board item `01M19XZPZD5CKRB83JJS42E8JN` — if
   your walk still finds a path that does not exist, say so plainly rather
   than implying one that does).
3. Correct **this plan** where the walk showed it wrong, and change its status
   line from UNWALKED.
