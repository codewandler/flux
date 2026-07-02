#!/usr/bin/env python3
"""Derive time-to-first-feedback (TTFF) from record_run.py recordings — I-03.

Reads a results tree written by run-ttff.sh:

    <outdir>/<leg>/<prompt-id>/t<N>/chunks.jsonl

and derives, per trial:

  - t_first_output   first byte the CLI wrote (any output at all)
  - t_planning       first planning-spinner frame (braille tick or a phase
                     label: orienting…/gathering…/planning…/revising…/
                     composing plan…) — absent on the pre-cutover baseline,
                     whose normal mode was silent while planning (the A-12 bug)
  - t_first_artifact first chunk with rendered CONTENT: streamed thinking,
                     the ◆ brief, the plan tree, step output — anything that
                     is not just spinner frames/labels/elapsed ticks. This is
                     the story's "first rendered artifact".
  - t_exit           process end

TTFF = t_first_artifact (measured from spawn; both legs pay the same ~ms of
binary startup, and prompt submission is argv so spawn IS submission).

Prints a per-prompt baseline-vs-post table (median over trials) and writes
report.json next to the recordings. `--ignore REGEX` (repeatable) drops
matching lines from artifact classification — for banner lines that render
before the turn starts; recordings are raw, so reclassification is free.

Stdlib only.
"""

import argparse
import base64
import codecs
import json
import re
import statistics
import sys
from pathlib import Path

CSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
OSC = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")
BRAILLE = re.compile(r"[⠀-⣿]")
PHASE_LABELS = ("orienting…", "gathering…", "planning…", "revising…", "composing plan…")
ELAPSED = re.compile(r"\d+µs|\d+ms|\d+(?:\.\d+)?s")
# The CLI prints `<model> · session s_<n>` before the turn starts — state, not artifact.
DEFAULT_IGNORE = [re.compile(r"\S+ · session s_\d+")]
# A turn that failed still exits 0; the CLI renders this marker instead of an answer.
TURN_ERROR_MARKER = "couldn't complete the turn"


def clean(text: str) -> str:
    """Strip ANSI escapes and carriage returns; keep printable content."""
    text = CSI.sub("", text)
    text = OSC.sub("", text)
    return text.replace("\r", "")


def is_planning_frame(cleaned: str) -> bool:
    return bool(BRAILLE.search(cleaned)) or any(l in cleaned for l in PHASE_LABELS)


def artifact_content(cleaned: str, ignore: list[re.Pattern]) -> str:
    """What remains of a cleaned chunk once spinner dressing is removed."""
    s = cleaned
    for label in PHASE_LABELS:
        s = s.replace(label, "")
    s = BRAILLE.sub("", s)
    s = ELAPSED.sub("", s)
    for pat in DEFAULT_IGNORE:
        s = pat.sub("", s)
    for pat in ignore:
        s = pat.sub("", s)
    return s.strip()


def analyze_trial(path: Path, ignore: list[re.Pattern]) -> dict:
    dec = codecs.getincrementaldecoder("utf-8")(errors="replace")
    m = {
        "t_first_output": None,
        "t_planning": None,
        "t_first_artifact": None,
        "first_artifact_text": None,
        "t_exit": None,
        "exit_code": None,
        "timed_out": None,
        "turn_error": False,
    }
    for line in path.read_text(encoding="utf-8").splitlines():
        row = json.loads(line)
        if row["type"] == "chunk":
            t = row["t_ms"]
            text = dec.decode(base64.b64decode(row["b64"]))
            if not text:
                continue
            if m["t_first_output"] is None:
                m["t_first_output"] = t
            cleaned = clean(text)
            if TURN_ERROR_MARKER in cleaned:
                m["turn_error"] = True
            if m["t_planning"] is None and is_planning_frame(cleaned):
                m["t_planning"] = t
            if m["t_first_artifact"] is None:
                content = artifact_content(cleaned, ignore)
                if content:
                    m["t_first_artifact"] = t
                    m["first_artifact_text"] = content[:120]
        elif row["type"] == "exit":
            m["t_exit"] = row["t_ms"]
            m["exit_code"] = row["exit_code"]
            m["timed_out"] = row["timed_out"]
    return m


def median_or_none(values: list) -> float | None:
    vals = [v for v in values if v is not None]
    return round(statistics.median(vals), 1) if vals else None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("outdir", help="results tree root (contains <leg>/<prompt>/t<N>/)")
    ap.add_argument(
        "--ignore",
        action="append",
        default=[],
        help="regex for pre-turn banner lines to exclude from artifact detection (repeatable)",
    )
    args = ap.parse_args()
    root = Path(args.outdir)
    ignore = [re.compile(p) for p in args.ignore]

    # results[leg][prompt] = list of per-trial metric dicts
    results: dict[str, dict[str, list[dict]]] = {}
    for chunks in sorted(root.glob("*/*/t*/chunks.jsonl")):
        trial_dir = chunks.parent
        prompt_id = trial_dir.parent.name
        leg = trial_dir.parent.parent.name
        m = analyze_trial(chunks, ignore)
        m["trial"] = trial_dir.name
        results.setdefault(leg, {}).setdefault(prompt_id, []).append(m)

    if not results:
        print(f"no recordings under {root}", file=sys.stderr)
        return 1

    legs = sorted(results)
    prompts = sorted({p for by_prompt in results.values() for p in by_prompt})

    summary: dict[str, dict] = {}
    header = f"{'prompt':<20}" + "".join(f"{leg + ' ttff':>16}" for leg in legs) + f"{'delta':>12}"
    print(header)
    print("-" * len(header))
    for prompt in prompts:
        medians = {}
        for leg in legs:
            trials = results.get(leg, {}).get(prompt, [])
            bad = [
                t
                for t in trials
                if t["timed_out"] or (t["exit_code"] or 0) != 0 or t["turn_error"]
            ]
            medians[leg] = {
                "ttff_ms": median_or_none([t["t_first_artifact"] for t in trials]),
                "planning_ms": median_or_none([t["t_planning"] for t in trials]),
                "total_ms": median_or_none([t["t_exit"] for t in trials]),
                "trials": len(trials),
                "failed_trials": len(bad),
            }
        summary[prompt] = medians
        row = f"{prompt:<20}"
        for leg in legs:
            v = medians[leg]["ttff_ms"]
            row += f"{(str(v) + 'ms') if v is not None else '—':>16}"
        if len(legs) == 2:
            a, b = (medians[legs[0]]["ttff_ms"], medians[legs[1]]["ttff_ms"])
            row += f"{(str(round(b - a, 1)) + 'ms') if a is not None and b is not None else '—':>12}"
        print(row)

    failed = sum(m["failed_trials"] for by_prompt in summary.values() for m in by_prompt.values())
    if failed:
        print(f"\n⚠ {failed} trial(s) failed or timed out — inspect before trusting medians")

    report = {"legs": legs, "prompts": summary, "per_trial": results}
    out = root / "report.json"
    out.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(f"\nwrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
