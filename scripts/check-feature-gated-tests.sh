#!/usr/bin/env bash
#
# check-feature-gated-tests.sh — run the test targets `cargo test --workspace` cannot reach, and
# refuse to let a new one hide.
#
# C-308: `cargo test --workspace` builds every member with its *default* features (plus whatever
# feature unification turns on for a dependency). A target behind a non-default feature is therefore
# invisible to it — and a test no gate exercises is a test that does not exist. It decays silently
# the moment the code moves under it. That is not hypothetical: `codewandler-flux-lang/cli`'s
# `fluxlang` binary tests were red on `main` for as long as it took someone to run the dev loop in
# `crates/flux-lang/AGENTS.md` by hand, while every gate reported green.
#
# So this script owns two obligations:
#
#   1. **The ledger is exhaustive.** Every `(package, feature)` pair in BOTH workspaces (root and the
#      nested `plugins/` one) must appear below with a disposition. A feature added without one
#      fails this script — the point is that "nobody thought about the gate" stops being possible.
#   2. **The `run` legs actually run.** Anything marked `run` is compiled and executed here.
#
# Dispositions:
#   run        — executed by this script; a failure reds CI.
#   covered    — some workspace member already enables it, so `cargo test --workspace` compiles and
#                runs it through feature unification. Re-verify with the recipe below.
#   elsewhere  — exercised by a different, named CI job.
#   skip       — deliberately not run here, with the cost or the blocker stated.
#
# Re-verifying a `covered` claim (this is how the C-308 audit was done — no claim below is folded
# from memory):
#
#   cargo test --workspace -- --list | grep ': test' | sort -u > /tmp/ws.txt
#   cargo test -p <pkg> --features <feat> -- --list | grep ': test' | sort -u > /tmp/f.txt
#   comm -23 /tmp/f.txt /tmp/ws.txt        # non-empty  =>  the feature hides tests from the gate
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# package<TAB>feature<TAB>disposition<TAB>why
#
# Keep this sorted by package. The `why` is load-bearing: it is what a reviewer checks against the
# recipe above.
LEDGER=$(
  cat <<'EOF'
codewandler-flux-a2a	utoipa	run	4 schema_tests behind the utoipa derives; nothing in the workspace enables it
codewandler-flux-capabilities	embeddings	run	4 OpenAiEmbedder tests; flux-cli's passthrough feature is off by default
codewandler-flux-capabilities	local-embeddings	skip	fastembed pulls an ONNX runtime (hundreds of MB) and gates zero tests
codewandler-flux-capabilities	postgres	elsewhere	the `postgres` CI job, against a real server
codewandler-flux-capabilities	sqlite-vec	run	1 SqliteVecStore round-trip
codewandler-flux-events	otel	run	6 OTLP export tests; no member enables it
codewandler-flux-events	postgres	elsewhere	the `postgres` CI job, against a real server
codewandler-flux-events	sqlite	covered	default-on, so the workspace build compiles it
codewandler-flux-flow	sqlite	covered	default-on, so the workspace build compiles it
codewandler-flux-lang	cli	run	the `fluxlang` binary's 8 tests — the C-308 defect itself
codewandler-flux-markdown	ratatui	covered	flux-tui enables it; unification carries it into --workspace
codewandler-flux-markdown	terminal	covered	flux-cli enables it; unification carries it into --workspace
codewandler-flux-plugin	hooks	covered	the root manifest pins host+hooks+pack on for every consumer
codewandler-flux-plugin	host	covered	the root manifest pins host+hooks+pack on for every consumer
codewandler-flux-plugin	pack	covered	the root manifest pins host+hooks+pack on for every consumer
codewandler-flux-providers	realtime	run	17 OpenAI Realtime config/event tests; no member enables it
codewandler-flux-sdk	plugins	run	2 end-to-end plugin-op tests behind the fixture plugin binary; nothing in the workspace enables it. Un-quarantined by C-309, which fixed what made it red: the effect-less projection default synthesized a process effect no capability backed, so the fixture could not satisfy the authority contract
codewandler-flux-sdk	pricing	run	1 loader test
codewandler-flux-sdk	providers	run	1 test + 1 doctest on from_spec
codewandler-flux-sdk	test-kit	covered	flux-cli takes flux-sdk with test-kit as a dev-dependency
codewandler-flux-tools	png	covered	flux-cli's default features enable it
flux-auth	introspect	covered	flux-cli -> flux-server/introspect -> flux-auth/introspect
flux-channels	room-media	run	the D-208 media-sidecar suite (11 tests in tests/room_media.rs + 12 unit tests under rooms::media); off by default and nothing in the workspace enables it, so `cargo test --workspace` compiles none of it
flux-channels	slack	covered	default-on, so the workspace build compiles it
flux-cli	embeddings	skip	a pure passthrough to codewandler-flux-capabilities/embeddings (run above); gates no test of its own
flux-cli	png	covered	default-on
flux-cli	slack	covered	default-on
flux-server	introspect	covered	flux-cli enables it
EOF
)

fail=0

# --- 1. the ledger is exhaustive -------------------------------------------------------------
declared=$(printf '%s\n' "$LEDGER" | cut -f1,2 | sort -u)

actual=$(
  for manifest in Cargo.toml plugins/Cargo.toml; do
    cargo metadata --no-deps --format-version 1 --manifest-path "$manifest" |
      jq -r '.packages[] | . as $p | ($p.features | keys[]) | select(. != "default") | "\($p.name)\t\(.)"'
  done | sort -u
)

undeclared=$(comm -13 <(printf '%s\n' "$declared") <(printf '%s\n' "$actual"))
if [ -n "$undeclared" ]; then
  echo "error: feature(s) with no disposition in scripts/check-feature-gated-tests.sh:" >&2
  printf '  %s\n' $(printf '%s\n' "$undeclared" | tr '\t' '/') >&2
  echo "  Say how the gate reaches them (run / covered / elsewhere / skip) before adding a feature." >&2
  fail=1
fi

stale=$(comm -23 <(printf '%s\n' "$declared") <(printf '%s\n' "$actual"))
if [ -n "$stale" ]; then
  echo "error: ledger entries for feature(s) that no longer exist:" >&2
  printf '  %s\n' $(printf '%s\n' "$stale" | tr '\t' '/') >&2
  fail=1
fi

[ "$fail" -eq 0 ] || exit 1

# --- 2. the `run` legs ------------------------------------------------------------------------
# One `cargo test` per package, with that package's `run` features joined — a package's features are
# additive here, and one invocation per package keeps the compile count down.
while IFS= read -r pkg; do
  feats=$(printf '%s\n' "$LEDGER" | awk -F'\t' -v p="$pkg" '$1 == p && $3 == "run" { print $2 }' | paste -sd, -)
  echo "==> cargo test -p $pkg --features $feats"
  cargo test -p "$pkg" --features "$feats"
done < <(printf '%s\n' "$LEDGER" | awk -F'\t' '$3 == "run" { print $1 }' | sort -u)

# --- 3. what is deliberately not run ----------------------------------------------------------
echo
echo "not run here (declared):"
printf '%s\n' "$LEDGER" | awk -F'\t' '$3 == "skip" { printf "  %s/%s — %s\n", $1, $2, $4 }'
