#!/usr/bin/env bash
# C-264: bounded deterministic adversarial corpus runner.
#
# Every cargo selector is enumerated before execution and must expose its named sentinel test. This
# is intentionally stricter than `cargo test <filter>`, which exits zero when a renamed filter runs
# zero tests. Inputs are committed fixtures plus deterministic seeds; failure artifacts contain
# only cargo output and reproduction coordinates, never environment values or provider traffic.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${FLUX_ADVERSARIAL_ARTIFACT_DIR:-$ROOT/target/adversarial-artifacts}"

fail() {
  echo "adversarial-assurance: $*" >&2
  return 1
}

listing_has() {
  local listing="$1" sentinel="$2" minimum="$3"
  local count
  count="$(printf '%s\n' "$listing" | grep -cE ': test$' || true)"
  [ "$count" -ge "$minimum" ] || fail "selector is vacuous: expected >=$minimum tests, found $count"
  printf '%s\n' "$listing" | grep -Fq "$sentinel" || fail "selector lost sentinel test: $sentinel"
}

workflow_policy() {
  local path="$1"
  ruby -ryaml - "$path" <<'RUBY'
path = ARGV.fetch(0)
doc = YAML.safe_load(File.read(path), aliases: true)
abort "adversarial workflow is not a mapping" unless doc.is_a?(Hash)
jobs = doc.fetch("jobs")

def exact_job(jobs, name, expected_if = nil)
  job = jobs.fetch(name)
  condition = job["if"]
  if expected_if
    abort "#{name} has the wrong activation condition" unless condition == expected_if
  else
    abort "#{name} may not be conditional or disabled" if condition
  end
  job
end

def action_step(job, pattern, label)
  matches = job.fetch("steps").select { |step| step.fetch("uses", "").match?(pattern) }
  abort "#{label} must have exactly one active SHA-pinned action step" unless matches.length == 1
  step = matches.fetch(0)
  abort "#{label} action step may not be conditional or disabled" if step.key?("if")
  step
end

def run_step(job, needle, label)
  matches = job.fetch("steps").select { |step| step.fetch("run", "").include?(needle) }
  abort "#{label} must have exactly one active run step" unless matches.length == 1
  step = matches.fetch(0)
  abort "#{label} run step may not be conditional or disabled" if step.key?("if")
  step
end

def pinned_upload(job, label)
  matches = job.fetch("steps").select do |step|
    step.fetch("uses", "").match?(/\Aactions\/upload-artifact@[0-9a-f]{40}\z/)
  end
  abort "#{label} must preserve failure artifacts with one pinned action" unless matches.length == 1
  abort "#{label} artifact preservation must run under always()" unless matches.fetch(0)["if"] == "${{ always() }}"
end

codeql = exact_job(jobs, "codeql-rust")
abort "CodeQL lost security-events: write" unless codeql.fetch("permissions", {})["security-events"] == "write"
init = action_step(codeql, /\Agithub\/codeql-action\/init@[0-9a-f]{40}\z/, "CodeQL init")
analyze = action_step(codeql, /\Agithub\/codeql-action\/analyze@[0-9a-f]{40}\z/, "CodeQL analyze")
abort "CodeQL init/analyze are not pinned to the same revision" unless init["uses"].split("@").last == analyze["uses"].split("@").last
init_with = init.fetch("with", {})
abort "CodeQL lost the Rust language" unless init_with["languages"] == "rust"
# `none` is the only build mode CodeQL's Rust extractor accepts; `manual` is a FATAL config error
# ("Rust does not support the manual build mode"), so pinning manual here guarded a lane that could
# never run. Pin `none` and reject manual explicitly so the mistake cannot come back.
abort "CodeQL Rust must use build-mode none (manual is a fatal config error)" unless init_with["build-mode"] == "none"
abort "CodeQL lost security-extended queries" unless init_with["queries"] == "security-extended"
# build-mode none extracts from source, so there is deliberately NO workspace build step to require;
# the dependency fetch is what the extractor needs to resolve the locked graph offline.
run_step(codeql, "cargo fetch --locked", "CodeQL dependency fetch")
abort "CodeQL analyze must follow initialization" unless codeql["steps"].index(analyze) > codeql["steps"].index(init)

smoke = exact_job(jobs, "corpus-smoke", "${{ github.event_name != 'schedule' }}")
run_step(smoke, "timeout 10m ./scripts/run-adversarial-assurance.sh --smoke", "PR corpus smoke")
pinned_upload(smoke, "PR corpus smoke")

deep = exact_job(jobs, "corpus-deep", "${{ github.event_name == 'schedule' }}")
run_step(deep, "timeout 20m ./scripts/run-adversarial-assurance.sh --deep", "scheduled deep corpus")
pinned_upload(deep, "scheduled deep corpus")

miri = exact_job(jobs, "miri", "${{ github.event_name == 'schedule' || github.event_name == 'workflow_dispatch' }}")
toolchains = miri.fetch("steps").filter_map { |step| step.fetch("with", {})["toolchain"] }
abort "Miri lost its date-pinned nightly" unless toolchains.one? { |value| value.match?(/\Anightly-[0-9]{4}-[0-9]{2}-[0-9]{2}\z/) }
miri_run = run_step(miri, "./scripts/run-adversarial-assurance.sh --miri-preflight", "Miri selector preflight").fetch("run")
abort "Miri plugin protocol target disappeared" unless miri_run.include?("cargo miri test --locked --offline \\") && miri_run.include?("codewandler-flux-plugin-protocol")
abort "Miri Flux-Lang target disappeared" unless miri_run.include?("codewandler-flux-lang --lib lexer::tests::")
unsupported = miri.fetch("env", {}).fetch("FLUX_MIRI_UNSUPPORTED", "").split(",").sort
expected_unsupported = %w[pack-extraction provider-streams url-dialing]
abort "Miri exclusion inventory is missing or changed" unless unsupported == expected_unsupported
pinned_upload(miri, "Miri")
RUBY
}

self_test() {
  local good bad workflow_good workflow_disabled workflow_comment
  good=$'alpha::sentinel: test\nalpha::other: test'
  bad=$'alpha::renamed: test'
  listing_has "$good" 'alpha::sentinel: test' 2
  if listing_has "$bad" 'alpha::sentinel: test' 2 >/dev/null 2>&1; then
    fail "self-test failed: a renamed/zero-width selector was accepted"
  fi
  if listing_has '' 'alpha::sentinel: test' 1 >/dev/null 2>&1; then
    fail "self-test failed: an empty cargo listing was accepted"
  fi
  workflow_good="$(mktemp)"
  workflow_disabled="$(mktemp)"
  workflow_comment="$(mktemp)"
  trap 'rm -f "${workflow_good:-}" "${workflow_disabled:-}" "${workflow_comment:-}"' EXIT
  cp "$ROOT/.github/workflows/adversarial-assurance.yml" "$workflow_good"
  ruby -ryaml - "$workflow_good" "$workflow_disabled" "$workflow_comment" <<'RUBY'
source, disabled_path, comment_path = ARGV
doc = YAML.safe_load(File.read(source), aliases: true)
disabled = Marshal.load(Marshal.dump(doc))
disabled.fetch("jobs").fetch("codeql-rust")["if"] = "${{ false }}"
File.write(disabled_path, YAML.dump(disabled))

comment = Marshal.load(Marshal.dump(doc))
steps = comment.fetch("jobs").fetch("codeql-rust").fetch("steps")
removed = steps.reject! { |step| step.fetch("uses", "").start_with?("github/codeql-action/analyze@") }
abort "self-test fixture could not remove CodeQL analyze" unless removed
File.write(comment_path, YAML.dump(comment) + "\n# uses: github/codeql-action/analyze@2222222222222222222222222222222222222222\n")
RUBY
  workflow_policy "$workflow_good"
  if workflow_policy "$workflow_disabled" >/dev/null 2>&1; then
    fail "self-test failed: a disabled CodeQL job was accepted"
  fi
  if workflow_policy "$workflow_comment" >/dev/null 2>&1; then
    fail "self-test failed: comment-only CodeQL analyze was accepted"
  fi
  echo "PASS adversarial-assurance self-test rejects vacuous selectors and disabled/comment-only workflow decoys"
}

run_target() {
  local name="$1" sentinel="$2" minimum="$3"
  shift 3
  local -a command=("$@")
  local listing log
  listing="$("${command[@]}" -- --list)"
  listing_has "$listing" "$sentinel" "$minimum"
  log="$ARTIFACT_DIR/$name.log"
  printf 'target=%s\nsentinel=%s\ncommand=' "$name" "$sentinel" >"$log"
  printf '%q ' "${command[@]}" >>"$log"
  printf '\n' >>"$log"
  if ! "${command[@]}" -- --nocapture 2>&1 | tee -a "$log"; then
    printf 'reproduce: FLUX_ADVERSARIAL_CASES=%q ' "${FLUX_ADVERSARIAL_CASES:-}" >>"$log"
    printf '%q ' "${command[@]}" >>"$log"
    printf -- '-- --nocapture\n' >>"$log"
    return 1
  fi
}

miri_preflight() {
  local plugin_listing lang_listing
  plugin_listing="$(cargo test --locked --offline \
    -p codewandler-flux-plugin-protocol --test adversarial_frames -- --list)"
  listing_has "$plugin_listing" \
    'generated_plugin_frames_are_total_and_protocol_checked: test' 1
  lang_listing="$(cargo test --locked --offline \
    -p codewandler-flux-lang --lib lexer::tests:: -- --list)"
  listing_has "$lang_listing" 'lexer::tests::lexer_is_lossless: test' 4
  echo "PASS Miri selectors enumerate their sentinel tests"
}

run_suite() {
  local mode="$1"
  mkdir -p "$ARTIFACT_DIR"
  printf 'mode=%s\ncases=%s\ninputs=committed-fixtures-and-deterministic-seeds\n' \
    "$mode" "${FLUX_ADVERSARIAL_CASES:-default}" >"$ARTIFACT_DIR/manifest.txt"

  local provider_filter='envelope_corpus::responses_stream_survives_single_frame_corruption'
  local provider_min=1
  if [ "$mode" = deep ]; then
    provider_filter='envelope_corpus'
    provider_min=10
  fi

  run_target provider-envelopes \
    'envelope_corpus::responses_stream_survives_single_frame_corruption: test' "$provider_min" \
    cargo test --locked --offline -p codewandler-flux-providers "$provider_filter"
  run_target flux-lang \
    'random_draft_asts_round_trip_exactly: test' 1 \
    cargo test --locked --offline -p codewandler-flux-lang --test roundtrip_property
  run_target plugin-ndjson \
    'generated_plugin_frames_are_total_and_protocol_checked: test' 1 \
    cargo test --locked --offline -p codewandler-flux-plugin-protocol --test adversarial_frames
  run_target url-redirect \
    'generated_redirect_targets_never_normalize_around_private_net_policy: test' 1 \
    cargo test --locked --offline -p codewandler-flux-system --test adversarial_urls
  run_target pack-extraction \
    'pack::tests::generated_pack_archives_are_total_and_write_only_after_complete_extraction: test' 1 \
    cargo test --locked --offline -p codewandler-flux-plugin \
      pack::tests::generated_pack_archives_are_total_and_write_only_after_complete_extraction
}

case "${1:-}" in
  --self-test)
    self_test
    ;;
  --smoke)
    export FLUX_ADVERSARIAL_CASES="${FLUX_ADVERSARIAL_CASES:-128}"
    run_suite smoke
    ;;
  --deep)
    export FLUX_ADVERSARIAL_CASES="${FLUX_ADVERSARIAL_CASES:-512}"
    run_suite deep
    ;;
  --miri-preflight)
    miri_preflight
    ;;
  *)
    echo "usage: $0 --self-test|--smoke|--deep|--miri-preflight" >&2
    exit 2
    ;;
esac
