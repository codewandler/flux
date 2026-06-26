#!/usr/bin/env bash
# Replay a saved terminal-bench agent session — observe a past flux run with no API spend.
#
# terminal-bench records every agent session as an asciinema v2 cast (`sessions/agent.cast`) and the
# grader session as `sessions/tests.cast`. This replays one. With `asciinema` installed and --play you
# get real-time playback; otherwise it decodes the cast and prints the reconstructed terminal output
# (ANSI stripped by default), which needs nothing but python3.
#
# Usage:
#   bash bench/replay-agent.sh                 # newest agent.cast under /tmp/flux-tbench-*
#   bash bench/replay-agent.sh <RUN_DIR>       # find agent.cast inside a tb output dir
#   bash bench/replay-agent.sh <file.cast>     # a specific cast (e.g. a tests.cast)
# Flags: --raw (keep ANSI colour) · --play (use `asciinema play` if available)
set -uo pipefail

raw=0; play=0; target=""
for a in "$@"; do
  case "$a" in
    --raw) raw=1 ;;
    --play) play=1 ;;
    *) target="$a" ;;
  esac
done

# Resolve the cast file.
if [ -z "$target" ]; then
  cast=$(find /tmp/flux-tbench-* -name 'agent.cast' 2>/dev/null -printf '%T@ %p\n' | sort -nr | head -1 | cut -d' ' -f2-)
elif [ -f "$target" ]; then
  cast="$target"
elif [ -d "$target" ]; then
  cast=$(find "$target" -name 'agent.cast' 2>/dev/null | head -1)
fi
[ -n "${cast:-}" ] && [ -f "$cast" ] || { echo "no cast found (arg: '${target:-<latest>}')" >&2; exit 1; }
echo "→ replaying: $cast" >&2

if [ "$play" -eq 1 ] && command -v asciinema >/dev/null 2>&1; then
  exec asciinema play "$cast"
fi

# No asciinema: decode the JSONL cast and stream the output events as text.
RAW=$raw python3 - "$cast" <<'PY'
import json, os, re, sys
strip = os.environ.get("RAW") != "1"
ansi = re.compile(r'\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[]].*?(?:\x07|\x1b\\)|\x1b[()][AB012]')
with open(sys.argv[1], encoding="utf-8", errors="replace") as f:
    next(f, None)  # header line
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)
        except Exception:
            continue
        if len(ev) >= 3 and ev[1] == "o":          # "o" = terminal output
            s = ev[2]
            sys.stdout.write(ansi.sub("", s) if strip else s)
sys.stdout.write("\n")
PY
