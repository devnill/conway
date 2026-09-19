#!/bin/zsh
# Dogfood round 1 -- the eight human-gate judgements under board item
# 01M1YY58FT93YFFW5TTS9X870C.
#
# These gates exist because their MECHANICAL halves already moved to
# automated coverage (01M2MEZMH2KJ79XSADQWCBFVFH). What is left is the part
# no assertion reaches: whether the thing is actually good. This script does
# the setup, tells you what to type, asks only those questions, and writes
# your answers to board-ready notes.
#
# Resumable: a gate with an existing note file is skipped. Run again to pick
# up where you stopped. Run one gate directly with:  ./dogfood-round-1.sh 4
#
# Nothing here reads, moves, or modifies ~/.conway/settings.json.

set -u
setopt NO_NOMATCH 2>/dev/null || true

REPO="${REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
OUT="${DOGFOOD_DIR:-/tmp/conway-dogfood-$(date +%Y%m%d)}"
CONWAY="${CONWAY_BIN:-$REPO/target/debug/conway}"
mkdir -p "$OUT"

bold() { print -P "%B$1%b" }
rule() { print -- "────────────────────────────────────────────────────────────────" }
pause() { print -n "  [enter to continue] "; read -r _ }

# ask <file> <question>...  -- appends "**Q**\n\nA\n" per question.
ask() {
  local file=$1; shift
  local q a
  for q in "$@"; do
    print ""
    print -P "%F{cyan}?%f $q"
    print -n "> "
    read -r a
    print -- "**$q**\n\n${a:-(no answer)}\n" >> "$file"
  done
}

verdict() {
  local file=$1
  print ""
  print -P "%F{cyan}?%f VERDICT -- pass / finding / fail, and the one sentence that matters most:"
  print -n "> "
  local v; read -r v
  print -- "## Verdict\n\n${v:-(none)}\n" >> "$file"
}

start_note() {
  local file=$1 id=$2 title=$3
  cat > "$file" <<EOF
# $title

Board item: \`$id\`
Gate: child of review container \`01M1YRSA5JGZT5QZMPFFYXF3CQ\`
Run: $(date -u +%Y-%m-%dT%H:%M:%SZ) on $(hostname -s)
conway: $($CONWAY --version 2>/dev/null || echo "not built")
HEAD: $(git -C "$REPO" rev-parse --short HEAD 2>/dev/null)

## Answers

EOF
}

done_note() {
  local file=$1
  rule
  bold "Saved: $file"
  print ""
}

need_binary() {
  if [[ ! -x "$CONWAY" ]]; then
    bold "conway is not built at $CONWAY"
    print "  Build it first:  cargo build -p conway-cli"
    print "  Or point at another binary:  CONWAY_BIN=/path/to/conway $0"
    exit 1
  fi
}

# A scratch config dir per gate, so nothing here can touch your real
# ~/.conway. CONWAY_CONFIG_DIR is what the binary honours.
scratch_cfg() {
  local name=$1
  local dir="$OUT/cfg-$name"
  mkdir -p "$dir"
  print -- "$dir"
}

# ─────────────────────────────────────────────────────────────────────────
gate_1_statusline() {
  local f="$OUT/1-statusline.md"
  [[ -f $f ]] && { print "1. status line -- already answered, skipping"; return }
  need_binary
  rule; bold "GATE 1/8 -- Is a refreshing status line something you'd keep?"
  print "  Board item 01M1ZJSBY6QXSSZF7PMV7N9FCX. Smallest gate; start here."
  print ""
  local cfg; cfg=$(scratch_cfg statusline)
  cat > "$cfg/conway.json" <<'EOF'
{
  "tui": {
    "status_line_command": {
      "command": ["sh", "-c", "git status --porcelain | wc -l | tr -d ' '"],
      "key": "dirty",
      "refresh_interval_ms": 2000,
      "timeout_ms": 1000
    }
  }
}
EOF
  print "  Wrote a status-line config to: $cfg/conway.json"
  print "  It shows your uncommitted-file count, refreshing every 2s."
  print ""
  bold "  DO THIS:"
  print "    CONWAY_CONFIG_DIR=$cfg $CONWAY"
  print ""
  print "  Then do a small real edit in another terminal (touch a file, revert it)"
  print "  and watch the number move. Stay in the session a few minutes -- the"
  print "  question is about attention over time, not whether it works."
  pause
  start_note "$f" "01M1ZJSBY6QXSSZF7PMV7N9FCX" "Dogfood 1 -- refreshing status line"
  ask "$f" \
    "After a session with it on, did you LOOK at it -- or did it become furniture you stopped seeing?" \
    "Is the cadence right: fast enough to feel live, slow enough not to twitch distractingly?" \
    "Did a value updating mid-turn ever pull your attention from something that mattered more?" \
    "The status line is shared with the permission-mode SAFETY field. Did your command crowd it?"
  verdict "$f"; done_note "$f"
}

# ─────────────────────────────────────────────────────────────────────────
gate_2_windows() {
  local f="$OUT/2-windows.md"
  [[ -f $f ]] && { print "2. window numbers -- already answered, skipping"; return }
  need_binary
  rule; bold "GATE 2/8 -- Were the window numbers good enough to pick a model on?"
  print "  Board item 01M1ZJR3DB48FSHVGGGBDGC13S."
  print ""
  bold "  DO THIS (1) -- read the numbers:"
  print "    $CONWAY routes explain default"
  print ""
  print "  Capturing it for your note now:"
  { print "### routes explain default\n\n\`\`\`"; \
    "$CONWAY" routes explain default 2>&1 | head -40; print "\`\`\`\n"; } > "$OUT/2-routes.txt"
  "$CONWAY" routes explain default 2>&1 | head -20
  print ""
  bold "  DO THIS (2) -- then actually choose on it, and give that model real work:"
  print "    $CONWAY"
  print "    > read docs/permissions.md end to end and propose the three changes"
  print "      that would most reduce an operator's confusion"
  print ""
  print "  The point is to make a real choice from these numbers, not to audit them."
  pause
  start_note "$f" "01M1ZJR3DB48FSHVGGGBDGC13S" "Dogfood 1 -- window numbers"
  cat "$OUT/2-routes.txt" >> "$f"
  ask "$f" \
    "Which candidate did you pick, and did you still have to GUESS about its window?" \
    "Did the model you picked then hit a runway warning it should not have?" \
    "Is the four-way vocabulary (verified / models.json / probed / floor (assumed)) meaningful AT THE MOMENT OF CHOOSING, or jargon you had to translate?" \
    "Did the status line agree with routes explain, and did you need to check?"
  verdict "$f"; done_note "$f"
}

# ─────────────────────────────────────────────────────────────────────────
gate_3_fallback() {
  local f="$OUT/3-fallback-agents.md"
  [[ -f $f ]] && { print "3. fallback + /agents -- already answered, skipping"; return }
  need_binary
  rule; bold "GATE 3/8 -- Is the fallback reason useful, and is /agents readable?"
  print "  Board item 01M1ZJVF7RY0BM4235974X53KB."
  print ""
  print "  Needs a SMALL-WINDOW chain so the file overflows the head and falls"
  print "  back on its own. Use your usual small-window/Ollama-shaped chain."
  print ""
  bold "  DO THIS:"
  print "    $CONWAY"
  print "    > review crates/conway-cli/src/tui/commands.rs and tell me the three"
  print "      riskiest things about its size"
  print ""
  print "    ...then switch models twice and ask the SAME question each time:"
  print "    > /model <second-model>     (ask again)"
  print "    > /model <third-model>      (ask again)"
  print ""
  print "    Then, without hunting:"
  print "    > /agents"
  print "    > /why"
  print ""
  print "  Note: that file is ~$(wc -l < "$REPO/crates/conway-cli/src/tui/commands.rs" 2>/dev/null | tr -d ' ') lines, which is the point."
  pause
  start_note "$f" "01M1ZJVF7RY0BM4235974X53KB" "Dogfood 1 -- fallback reason and /agents readability"
  ask "$f" \
    "When the fallback notice appeared MID-TURN, did it explain anything -- or did you have to go read /why to understand the one line that was meant to save you the trip?" \
    "Could you read /agents WITHOUT HUNTING after three switches? (one lineage, not three rows)" \
    "Was per-turn attribution recoverable -- could you tell which model gave which answer?" \
    "Comparing three models on the same real file is the actual reason to switch. Was that comparison EASY?"
  verdict "$f"; done_note "$f"
}

# ─────────────────────────────────────────────────────────────────────────
gate_4_warned_child() {
  local f="$OUT/4-warned-child.md"
  [[ -f $f ]] && { print "4. warned child -- already answered, skipping"; return }
  need_binary
  rule; bold "GATE 4/8 -- Did the warned child hand back something you could USE?"
  print "  Board item 01M1ZJSV0CJKQR68FRGYDSY4EJ."
  print ""
  bold "  DO THIS:"
  print "    $CONWAY"
  print "    > delegate this to a child with max_steps=5: list every TODO and"
  print "      FIXME under crates/, grouped by crate, with file and line for each"
  print ""
  print "  That is genuinely useful work and it will NOT finish in five steps."
  print "  The child should get an 80% notice and wrap up with a partial answer."
  print ""
  print -P "  %F{yellow}The bar:%f a child that got the notice and still handed back"
  print "  nothing usable is a FINDING, not a pass. The notice exists to buy a"
  print "  partial answer; silence after a warning means it bought nothing."
  pause
  start_note "$f" "01M1ZJSV0CJKQR68FRGYDSY4EJ" "Dogfood 1 -- warned child's partial answer"
  ask "$f" \
    "Did it hand back a few crates covered and HONESTLY LABELLED incomplete -- something you could act on?" \
    "Or did it take the warning and still die with everything in its head?" \
    "If it wrapped up, did it tell you WHERE IT STOPPED so you could resume rather than restart?" \
    "Did the /agents row show the !budget tag, and did you notice it without looking for it?"
  verdict "$f"; done_note "$f"
}

# ─────────────────────────────────────────────────────────────────────────
gate_5_background() {
  local f="$OUT/5-background.md"
  [[ -f $f ]] && { print "5. background job -- already answered, skipping"; return }
  need_binary
  rule; bold "GATE 5/8 -- Could you keep working while the backgrounded job ran?"
  print "  Board item 01M1ZJQEZGWFGND6M51EDX8N4F."
  print ""
  print "  The bug this replaced: conway froze for two minutes, then returned a"
  print "  result claiming the job BOTH succeeded and timed out -- and killed it."
  print ""
  bold "  DO THIS -- a small real change, with its tests started in the background:"
  print "    $CONWAY"
  print "    > fix a typo or tighten a sentence in docs/tools.md, and start"
  print "      cargo test -p conway-plugin-idiom in the background so I can read"
  print "      the result after"
  print ""
  print "  Then KEEP GOING -- ask it something else immediately. The gate is about"
  print "  whether the session felt continuous, not whether the call returned fast."
  pause
  start_note "$f" "01M1ZJQEZGWFGND6M51EDX8N4F" "Dogfood 1 -- background job flow"
  ask "$f" \
    "Did you keep typing, or did you find yourself WAITING on conway before the next thing?" \
    "When you came back for the result, was getting it obvious -- or did you have to work out how?" \
    "Did you TRUST the job was still running, or did you check? (if you reached for ps, the result text did not reassure you)" \
    "Did any result claim both an exit code and a timeout? (that was the original defect -- report loudly if so)"
  verdict "$f"; done_note "$f"
}

# ─────────────────────────────────────────────────────────────────────────
gate_6_killed_worker() {
  local f="$OUT/6-killed-worker.md"
  [[ -f $f ]] && { print "6. killed worker -- already answered, skipping"; return }
  need_binary
  rule; bold "GATE 6/8 -- After killing a worker mid-edit, could you tell what it did?"
  print "  Board item 01M1ZJTBK4WM5MFMWXWJZXWF2K."
  print ""
  print -P "  %F{yellow}THIS GATE DELIBERATELY LEAVES EDITS IN YOUR WORKING TREE.%f"
  print ""
  if ! git -C "$REPO" diff --quiet || ! git -C "$REPO" diff --cached --quiet; then
    print -P "  %F{red}Your tree is NOT clean.%f Commit or set aside your work first --"
    print "  this gate's whole question is 'could you tell what the agent changed',"
    print "  which is unanswerable if your own edits are mixed in."
    print ""
    git -C "$REPO" status --short | head -20
    print ""
    print -n "  Continue anyway? [y/N] "; local yn; read -r yn
    [[ $yn == [yY] ]] || { print "  Skipped."; return }
  fi
  local branch="dogfood/killed-worker-$(date +%H%M%S)"
  print "  Suggested: work on a throwaway branch so undo is trivial:"
  print "    git -C $REPO switch -c $branch"
  print ""
  bold "  DO THIS -- delegate something substantial, let it get underway, then kill it:"
  print "    $CONWAY"
  print "    > add a module-level doc comment to every file in"
  print "      crates/conway-plugin-web/src/ explaining what that file owns"
  print ""
  print "    Let it run ~1 minute so it is genuinely mid-edit, then kill it"
  print "    (Ctrl-C, or 'kill <pid>' from another terminal -- try the realistic one:"
  print "    you deciding it is going the wrong way and stopping it)."
  print ""
  print "  Afterwards, BEFORE running git diff, ask yourself the questions below."
  print "  Whether you REACHED FOR conway.checkpoint -- or even remembered it"
  print "  existed at that moment -- is itself the finding."
  pause
  start_note "$f" "01M1ZJTBK4WM5MFMWXWJZXWF2K" "Dogfood 1 -- killed worker, tree legibility"
  print "### git status after the kill\n\n\`\`\`" >> "$f"
  git -C "$REPO" status --short >> "$f" 2>&1
  print "\`\`\`\n" >> "$f"
  ask "$f" \
    "The log says the run was cancelled. Did anything tell you WHICH FILES it had already changed?" \
    "Reading the half-finished doc comments, could you tell which were the agent's and which were yours?" \
    "Was recovering -- finishing or undoing -- obvious, or did you reconstruct it from git diff?" \
    "Did you reach for conway.checkpoint? Did you remember it existed at that moment?"
  verdict "$f"
  print ""
  print "  To undo everything this gate did:"
  print "    git -C $REPO restore crates/conway-plugin-web/src/"
  done_note "$f"
}

# ─────────────────────────────────────────────────────────────────────────
gate_7_under_load() {
  local f="$OUT/7-under-load.md"
  [[ -f $f ]] && { print "7. under load -- already answered, skipping"; return }
  need_binary
  rule; bold "GATE 7/8 -- Under real load, did the session survive in a way you'd trust?"
  print "  Board item 01M1ZJRTKB2JRTYC03SFY8G2BH. This is the gate that ended a"
  print "  real session: you cancelled three /ideate:review attempts and gave up."
  print ""
  print -n "  Start 'cargo build --workspace' now to create the load? [Y/n] "
  local yn; read -r yn
  if [[ $yn != [nN] ]]; then
    ( cd "$REPO" && cargo build --workspace > "$OUT/7-build.log" 2>&1 & )
    print "  Started. Log: $OUT/7-build.log"
    print "  (It competes for CPU -- that is the point. It will finish on its own.)"
  fi
  print ""
  bold "  DO THIS -- while the build runs, use the ideate plugin for real:"
  print "    $CONWAY"
  print "    > read the board, read the findings from this dogfood round, and"
  print "      file one of them as a real work item"
  print ""
  print "  Real ideate tool calls competing with a build for CPU is EXACTLY the"
  print "  shape that broke last time."
  print ""
  print -P "  %F{red}If you see ANY sign a call ran twice%f -- a duplicated board write,"
  print "  a doubled record append -- stop and report it. That means the"
  print "  no-resend guarantee failed, and it outranks everything else."
  pause
  start_note "$f" "01M1ZJRTKB2JRTYC03SFY8G2BH" "Dogfood 1 -- ideate plugin under load"
  ask "$f" \
    "When a call ran slow, did you WAIT -- or did you reach for cancel out of learned distrust?" \
    "A slow-response warning now appears where a failure used to. Did it read as 'this is handled' or as 'here it goes again'?" \
    "THE GATE: having finished the task under load once, would you start an unattended run with this plugin TONIGHT? If no, why?" \
    "Any sign a call ran TWICE -- duplicated board write, doubled record append? (report prominently)"
  verdict "$f"; done_note "$f"
}

# ─────────────────────────────────────────────────────────────────────────
gate_8_caching() {
  local f="$OUT/8-caching.md"
  [[ -f $f ]] && { print "8. caching -- already answered, skipping"; return }
  need_binary
  rule; bold "GATE 8/8 -- On a real Anthropic key, does the caching claim hold up?"
  print "  Board item 01M1ZJTXRWRA93KRWZSZBA745G."
  print ""
  print "  conway's whole economic argument rests on prompt caching. Across every"
  print "  recorded session the number has been zero or 'not reported'. Either"
  print "  providers don't cache, or something defeats it -- and the design's"
  print "  central claim is unrealised and invisible."
  print ""
  print -P "  %F{yellow}This gate needs a REAL Anthropic key.%f This script does not read,"
  print "  move, or modify ~/.conway/settings.json. Supply the key by exporting"
  print "  it in YOUR shell before the run, or point at your own config:"
  print ""
  print "    export ANTHROPIC_API_KEY=...        # your call"
  print "    $CONWAY --model anthropic/<model>"
  print ""
  bold "  DO THIS -- two turns on ONE file you have not discussed:"
  print "    > what does crates/conway-plugin-checkpoint/src/store.rs do?"
  print "    > now explain its eviction policy, and whether the per-project"
  print "      bound can starve a single large file"
  print ""
  print "  The prefix is unchanged; only your question moves. THE SECOND TURN"
  print "  SHOULD SHOW A NON-ZERO CACHE READ."
  print ""
  print "  Then check the raw numbers:"
  print "    $CONWAY sessions show <session-id>"
  print ""
  print -P "  %F{red}If the prefix turns out not to be byte-stable in real use, say so"
  print -P "  prominently.%f That is a design-level finding about conway's central"
  print "  economic claim, not a display bug."
  pause
  start_note "$f" "01M1ZJTXRWRA93KRWZSZBA745G" "Dogfood 1 -- caching on a real provider"
  ask "$f" \
    "Did the SECOND turn show a non-zero cache read? What did sessions show report as raw counts?" \
    "If it was zero: is the prefix byte-stable in real use? (design-level finding if not -- say so plainly)" \
    "On your usual Ollama-shaped chain, does 'cannot report' read as an HONEST LIMITATION or as conway covering for itself?" \
    "Could you tell WHICH of the two facts you were looking at -- 'not reported' vs 'not supported'?"
  verdict "$f"; done_note "$f"
}

# ─────────────────────────────────────────────────────────────────────────
summary() {
  rule; bold "ROUND 1 SUMMARY"
  print "  Notes in: $OUT"
  print ""
  local n=0
  for g in 1-statusline 2-windows 3-fallback-agents 4-warned-child \
           5-background 6-killed-worker 7-under-load 8-caching; do
    if [[ -f "$OUT/$g.md" ]]; then
      n=$((n+1))
      print -P "  %F{green}✓%f $g"
      grep -A2 '^## Verdict' "$OUT/$g.md" | tail -1 | sed 's/^/      /'
    else
      print -P "  %F{yellow}·%f $g  (not yet answered)"
    fi
  done
  print ""
  print "  $n of 8 answered."
  if (( n == 8 )); then
    print ""
    print "  All eight done. Each note is a board item body: file each as a child"
    print "  of the review container 01M1YRSA5JGZT5QZMPFFYXF3CQ with no edges,"
    print "  then close 01M1YY58FT93YFFW5TTS9X870C -- which unblocks Wave 2 and,"
    print "  through it, the ~32 items currently gated behind these judgements."
    print ""
    print "  Or hand me the directory and I will file them."
  fi
  rule
}

main() {
  local only="${1:-}"
  print ""; bold "conway dogfood round 1 -- eight judgements only you can make"
  print "  Notes: $OUT"
  print "  Binary: $CONWAY"
  print ""
  print "  Each gate does its own setup and asks 4 questions plus a verdict."
  print "  Answers are saved as you go; rerun to resume. Ctrl-C is safe."
  print ""
  case "$only" in
    1) gate_1_statusline ;;
    2) gate_2_windows ;;
    3) gate_3_fallback ;;
    4) gate_4_warned_child ;;
    5) gate_5_background ;;
    6) gate_6_killed_worker ;;
    7) gate_7_under_load ;;
    8) gate_8_caching ;;
    "") gate_1_statusline; gate_2_windows; gate_3_fallback; gate_4_warned_child
        gate_5_background; gate_6_killed_worker; gate_7_under_load; gate_8_caching ;;
    *) print "usage: $0 [1-8]"; exit 2 ;;
  esac
  summary
}

main "$@"
