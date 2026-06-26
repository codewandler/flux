#!/usr/bin/env bash
# Live-stream the in-container flux agent's terminal during a terminal-bench eval.
#
# terminal-bench runs the agent inside a Docker container in a tmux session named `agent`. This script
# auto-discovers that container (tb names them `<task>-<k>-of-<n>-<timestamp>`) and prints its pane
# whenever it changes, so you can watch flux work in real time while the self-improvement loop runs.
#
# Usage:  bash bench/watch-agent.sh [interval_secs]   (default 6)
# Run it in a second terminal (or via `! bash bench/watch-agent.sh`) alongside the loop. Ctrl-C to stop;
# it also stops on its own once no agent container has been seen for a while (the run finished).
set -uo pipefail
interval="${1:-6}"
prev=""
misses=0
echo "→ watching for a terminal-bench agent container (interval ${interval}s, Ctrl-C to stop)…"
while true; do
  # tb task containers are named like `fibonacci-server-1-of-1-<ts>`; newest first from `docker ps`.
  c=$(docker ps --format '{{.Names}}' | grep -E '\-[0-9]+-of-[0-9]+-' | head -1)
  if [ -z "$c" ]; then
    misses=$((misses + 1))
    if [ "$misses" -ge 6 ]; then echo "→ no agent container seen for a while; stopping."; break; fi
    sleep "$interval"; continue
  fi
  misses=0
  # Strip the braille spinner glyphs (U+2800..U+28FF) so the dedup only fires on real output changes.
  cur=$(docker exec "$c" tmux capture-pane -pt agent 2>/dev/null | sed 's/[⠀-⣿]//g' | grep -v '^[[:space:]]*$' | tail -n 14)
  if [ -n "$cur" ] && [ "$cur" != "$prev" ]; then
    printf '\n──── %s · %s ────\n%s\n' "$(date +%H:%M:%S)" "$c" "$cur"
    prev="$cur"
  fi
  sleep "$interval"
done
