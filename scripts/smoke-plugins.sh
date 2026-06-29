#!/usr/bin/env bash
#
# smoke-plugins.sh — live, env-gated smoke for the integration plugin pack (C-02 / D-08).
#
# For each integration whose credential is present in the environment, it builds the plugin, registers a
# descriptor in an ISOLATED plugin registry (a temp HOME — never touches your real ~/.flux), and drives
# one operation through `flux plugin call`, asserting a non-error result. Plugins whose key is absent are
# SKIPPED (not failed), so this is safe to run anywhere — it only exercises what you have keys for.
#
# Env keys (set the ones you want to exercise):
#   TAVILY_API_KEY                          → websearch.search
#   GITLAB_PERSONAL_TOKEN  (+ GITLAB_URL)   → gitlab.project.list
#   JIRA_API_TOKEN + JIRA_EMAIL + JIRA_URL  → jira.project.list
#   CONFLUENCE_API_TOKEN/.._EMAIL/.._URL    → confluence.space.list
#   SLACK_BOT_TOKEN                         → slack.channel.list
#   PROMETHEUS_URL                          → prometheus.targets
#   LOKI_URL                                → loki.labels
#   FLUX_SMOKE_KUBERNETES=1 (needs kubectl) → kubernetes.namespace.list
#   FLUX_EMBEDDINGS_API_KEY (or OPENAI_API_KEY) → an embeddings build note (see end)
#
# Override the flux binary with FLUX_BIN. Run before releasing anything that touches the plugins.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PLUGINS="$ROOT/plugins"
FLUX="${FLUX_BIN:-$ROOT/target/release/flux}"
BIN="$PLUGINS/target/release"

pass=0
fail=0
skipped=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf '  \033[33mSKIP\033[0m %s\n' "$1"; skipped=$((skipped + 1)); }
step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

step "pre-flight"
if [ ! -x "$FLUX" ]; then
  echo "  building flux (release)…"
  ( cd "$ROOT" && cargo build --release -p flux-cli ) || { echo "flux build failed"; exit 1; }
fi
echo "  building plugins (release)…"
( cd "$PLUGINS" && cargo build --release ) || { echo "plugins build failed"; exit 1; }

# Isolated registry — do NOT touch the user's ~/.flux/plugins.
SMOKE_HOME="$(mktemp -d)"
trap 'rm -rf "$SMOKE_HOME"' EXIT
export HOME="$SMOKE_HOME"
mkdir -p "$SMOKE_HOME/.flux/plugins"

# run_case <name> <op> <json> <gate-env-var>
run_case() {
  local name="$1" op="$2" json="$3" gate="$4"
  if [ -z "${!gate:-}" ]; then skip "$name.$op ($gate not set)"; return; fi
  local exe="$BIN/flux-plugin-$name"
  if [ ! -x "$exe" ]; then bad "$name.$op (binary missing: $exe)"; return; fi
  "$FLUX" plugin add "$name" "$exe" >/dev/null 2>&1
  local out
  if out=$("$FLUX" plugin call "$name" "$op" "$json" 2>&1); then
    ok "$name.$op → $(printf '%s' "$out" | head -c 120 | tr '\n' ' ')"
  else
    bad "$name.$op → $(printf '%s' "$out" | head -c 200 | tr '\n' ' ')"
  fi
}

step "plugin op round-trips (skipped when the key is absent)"
run_case websearch  websearch.search     '{"query":"warm transfer","max_results":2}' TAVILY_API_KEY
run_case gitlab     gitlab.project.list  '{}'                                         GITLAB_PERSONAL_TOKEN
run_case jira       jira.project.list    '{}'                                         JIRA_API_TOKEN
run_case confluence confluence.space.list '{}'                                        CONFLUENCE_API_TOKEN
run_case slack      slack.channel.list   '{}'                                         SLACK_BOT_TOKEN
run_case prometheus prometheus.targets   '{}'                                         PROMETHEUS_URL
run_case loki       loki.labels          '{}'                                         LOKI_URL

# kubernetes needs a reachable cluster + kubectl; opt in explicitly.
if [ -n "${FLUX_SMOKE_KUBERNETES:-}" ]; then
  if command -v kubectl >/dev/null 2>&1; then
    run_case kubernetes k8s.namespace.list '{}' FLUX_SMOKE_KUBERNETES
  else
    skip "kubernetes.k8s.namespace.list (kubectl not on PATH)"
  fi
else
  skip "kubernetes.k8s.namespace.list (set FLUX_SMOKE_KUBERNETES=1 + a reachable cluster)"
fi

step "embeddings"
if [ -n "${FLUX_EMBEDDINGS_API_KEY:-}${OPENAI_API_KEY:-}" ]; then
  echo "  an embeddings key is set — validate the live /v1/embeddings path with a feature build:"
  echo "    cargo run --release -p flux-cli --features embeddings -- app run <prog-with-knowledge>.flux"
  echo "  (the SemanticIndex rerank logic itself is covered by the default-build unit test)"
  skip "embeddings live round-trip (manual, feature build)"
else
  skip "embeddings (FLUX_EMBEDDINGS_API_KEY / OPENAI_API_KEY not set)"
fi

step "result"
printf '  %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
if [ "$fail" -gt 0 ]; then
  echo "  FAIL"
  exit 1
fi
echo "  PASS"
