#!/usr/bin/env python3
"""run_corpus.py -- evidence harness for board item 01M0WX32AKGA9W3S0KCVZHAGED.

THROWAWAY. See README.md. Not to be merged into the product.

Runs every case in corpus.jsonl through classify.sh (the real throwaway
pre_tool_use hook script), against one or more Ollama models, and reports:

  - false allows (dangerous case classified as no_opinion/routine)
  - false denials (routine case classified as deny/dangerous)
  - malformed-output rate (stdout that does not parse as a HookAnswer)
  - latency (median, p90, max)
  - non-determinism on the near_miss group (each case run N times)

Usage:
  python3 run_corpus.py --model gemma4:e4b [--model qwen2.5:14b] \
      [--corpus corpus.jsonl] [--repeats 3] [--out results/gemma4-e4b.json]
"""
from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CLASSIFY = HERE / "classify.sh"


def load_corpus(path: Path) -> list[dict]:
    cases = []
    with path.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            cases.append(json.loads(line))
    return cases


def parse_hook_answer(raw: str) -> tuple[bool, str | None]:
    """Returns (parsed_ok, predicted_label).

    predicted_label is "dangerous" if the answer is a well-formed Deny,
    "routine" if well-formed NoOpinion (explicit or default-shaped), and
    None if the text does not parse as a HookAnswer at all -- which is the
    MALFORMED case. Mirrors conway's actual on-the-wire HookAnswer shape:
    serde(rename_all="snake_case") on HookPermissionVerdict means
    NoOpinion <-> "no_opinion", Deny{reason} <-> {"deny": {"reason": ...}}.
    """
    raw = raw.strip()
    if not raw:
        # Empty stdout on exit 0 is conway's own documented default: an
        # implicit NoOpinion. Not malformed.
        return True, "routine"
    try:
        obj = json.loads(raw)
    except json.JSONDecodeError:
        return False, None
    if not isinstance(obj, dict):
        return False, None
    # Absent "permission" key defaults to NoOpinion (HookAnswer's own
    # #[serde(default)]).
    if "permission" not in obj:
        return True, "routine"
    perm = obj["permission"]
    if perm == "no_opinion":
        return True, "routine"
    if (
        isinstance(perm, dict)
        and set(perm.keys()) == {"deny"}
        and isinstance(perm["deny"], dict)
        and isinstance(perm["deny"].get("reason"), str)
    ):
        return True, "dangerous"
    return False, None


def run_one(model: str, host: str, case: dict, timeout_s: float) -> dict:
    stdin_bytes = json.dumps(case["event"]).encode()
    env = {
        "OLLAMA_MODEL": model,
        "OLLAMA_HOST": host,
        "PATH": "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin",
    }
    start = time.monotonic()
    try:
        proc = subprocess.run(
            [str(CLASSIFY)],
            input=stdin_bytes,
            capture_output=True,
            env=env,
            timeout=timeout_s,
        )
        elapsed = time.monotonic() - start
        stdout = proc.stdout.decode(errors="replace")
        stderr = proc.stderr.decode(errors="replace")
        exit_code = proc.returncode
        timed_out = False
    except subprocess.TimeoutExpired as exc:
        elapsed = time.monotonic() - start
        stdout = (exc.stdout or b"").decode(errors="replace")
        stderr = (exc.stderr or b"").decode(errors="replace")
        exit_code = None
        timed_out = True

    ok, predicted = parse_hook_answer(stdout)
    # A timeout or nonzero exit is conway's real fail-closed path too --
    # pre_tool_use_hook_denial treats an unreachable/unparseable runner as
    # a denial (on_failure: Deny, today's only behaviour). Model that here.
    if timed_out or (exit_code is not None and exit_code != 0):
        ok = False
        predicted = None

    return {
        "id": case["id"],
        "label": case["label"],
        "group": case["group"],
        "predicted": predicted,
        "parsed_ok": ok,
        "latency_s": elapsed,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "raw_stdout": stdout.strip(),
        "raw_stderr": stderr.strip()[:500],
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", action="append", required=True, dest="models")
    ap.add_argument("--host", default="http://localhost:11434")
    ap.add_argument("--corpus", default=str(HERE / "corpus.jsonl"))
    ap.add_argument("--repeats", type=int, default=1, help="repeats for near_miss group")
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("--out-dir", default=str(HERE / "results"))
    args = ap.parse_args()

    corpus = load_corpus(Path(args.corpus))
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    for model in args.models:
        print(f"=== model: {model} ===", file=sys.stderr)
        runs = []
        for case in corpus:
            repeats = args.repeats if case["group"] == "near_miss" else 1
            for rep in range(repeats):
                result = run_one(model, args.host, case, args.timeout)
                result["rep"] = rep
                runs.append(result)
                flag = "OK" if result["parsed_ok"] else "MALFORMED"
                print(
                    f"  [{flag}] {case['id']} rep={rep} label={case['label']} "
                    f"predicted={result['predicted']} lat={result['latency_s']:.2f}s",
                    file=sys.stderr,
                )

        safe_model = model.replace(":", "_").replace("/", "_")
        out_path = out_dir / f"{safe_model}.json"
        out_path.write_text(json.dumps({"model": model, "runs": runs}, indent=2))
        print(f"wrote {out_path}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
