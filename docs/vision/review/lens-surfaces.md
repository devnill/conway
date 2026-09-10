# Lens: surfaces

> **Read [`CONDUCT.md`](CONDUCT.md) first.** This lens assumes it.

---

## 1. The question

> **Is each extension surface real, and has anything that is not its author ever
> reached it?**

`INTENT.md` §8.5 is the standard, and it is stricter than it first reads:

> A surface is proven when **something that is not its author uses it to do a
> thing someone wanted** — not when it compiles, and not when its tests pass.

A consumer written to exercise a seam demonstrates the seam compiles, which was
never in question. It will happily certify a surface that cannot carry the case
nobody had yet.

---

## 2. What to establish, from the code

**2.1 The ports.** For every file in `crates/conway-core/src/ports/`: is there a
**non-test implementation outside the core**, and can a third party reach it?
Produce a table — port, in-tree implementors, external reachability, first real
consumer. A port with no external implementor is a surface that has not been
proven; a port an embedder *cannot* supply is a hole in the embedding story
(`INTENT.md` §7c).

**2.2 The hook event vocabulary.** Which events have production **dispatch
sites**, not just constants. A named event nothing fires is a documented
capability that does not exist.

**2.3 The plugin tier.** What is in `crates/conway-plugin-*`, what
`PHILOSOPHY.md` §5 promises, and the difference. Then the question that
actually measures weight. **Bundle liberally, enable nothing** is the ruling
(`docs/vision/DESIGN-plugin-dependencies.md` §0), so a plugin linked into the
binary is the intended shape, not a suspect. For each first-party plugin: is
it *off* by default, and when it is off does it cost nothing — no entries in
`conway tools list`, no prompt text, no schema tokens in `usage.input_tokens`,
no commands, no startup work? Measure with the plugin off and on, against an
isolated `$HOME`. A plugin that keeps costing after it is switched off is the
finding (`INTENT.md` §7a). A plugin that is compiled in and switchable is not.
`INTENT.md` §2's "uninstallable" is satisfied by a toggle that actually sheds
the capability.

**2.4 The embedding surface.** `INTENT.md` §7c gives non-Rust hosts the right to
embed conway. Walk the path a host would actually take and say where it stops.

**2.5 Trust.** Anything that spawns an operator-named external program is a
surface with a trust question attached. Say plainly what is and is not verified.

---

## 3. The trap

**Do not report a surface as proven on the strength of its test suite.** State
for each surface which of these it has:

| Level | What it means |
| --- | --- |
| **compiles** | the trait exists |
| **exercised** | a test written by the author drives it |
| **consumed** | in-tree code that is not the author's test uses it |
| **proven** | something outside this repository could and would |

Most of this tree's surfaces sit at *exercised*. Saying so is the finding.

---

## 4. Budget

- **Tool calls:** 30–40.
- **Return:** the shape in `CONDUCT.md` §4, **under 1,200 words**. Include the
  port table — it is the artifact the operator gets the most from.
