#!/usr/bin/env bash
# Source-policy guard for C-259: release jobs may download data, but may not execute an unauthenticated
# remote installer, and every core release must publish + verify provenance attestations.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

check_workflow_text() {
  local text="$1"
  if printf '%s\n' "$text" | grep -Eq 'curl[^|]*\|[[:space:]]*(sh|bash)|irm[^|]*\|[[:space:]]*iex|matrix\.install_dist\.run'; then
    echo "release workflow executes an unverified remote/generated installer" >&2
    return 1
  fi
  printf '%s\n' "$text" | grep -Eq 'uses:[[:space:]]+actions/attest@[0-9a-f]{40}' || {
    echo "release workflow does not publish a commit-pinned artifact attestation" >&2
    return 1
  }
  printf '%s\n' "$text" | grep -Eq 'attestations:[[:space:]]+write' || {
    echo "release workflow lacks attestation publication permission" >&2
    return 1
  }
}

check_workflow_semantics() {
  local path="$1"
  ruby -ryaml - "$path" <<'RUBY'
path = ARGV.fetch(0)
doc = YAML.safe_load(File.read(path), aliases: true)
abort "release workflow is not a mapping" unless doc.is_a?(Hash)
permissions = doc.fetch("permissions")
abort "release workflow default contents permission is not read" unless permissions["contents"] == "read"
jobs = doc.fetch("jobs")
host = jobs.fetch("host")
host_permissions = host.fetch("permissions")
required = {
  "actions" => "read",
  "attestations" => "write",
  "contents" => "write",
  "id-token" => "write",
}
required.each do |name, value|
  abort "host permission #{name} must be #{value}" unless host_permissions[name] == value
end

jobs.each do |name, job|
  next if name == "host"
  granted = job.fetch("permissions", {})
  %w[attestations contents id-token].each do |permission|
    abort "publication permission #{permission}: write escaped into job #{name}" if granted[permission] == "write"
  end
  contains_release_token = lambda do |value|
    case value
    when Hash then value.any? { |key, child| contains_release_token.call(key) || contains_release_token.call(child) }
    when Array then value.any? { |child| contains_release_token.call(child) }
    when String then value.match?(/\$\{\{\s*secrets\.RELEASE_TOKEN\s*\}\}/)
    else false
    end
  end
  abort "RELEASE_TOKEN escaped into job #{name}" if contains_release_token.call(job)
end

steps = host.fetch("steps")
attest = steps.each_index.select do |index|
  steps[index].fetch("uses", "").match?(/\Aactions\/attest@[0-9a-f]{40}\z/)
end
abort "host must contain exactly one SHA-pinned actions/attest step" unless attest.length == 1
attest_index = attest.fetch(0)
attest_step = steps.fetch(attest_index)
abort "attestation step may not be conditional or disabled" if attest_step.key?("if")
abort "attestation subject must be artifacts/*" unless attest_step.fetch("with", {})["subject-path"] == "artifacts/*"

release_index = steps.index { |step| step["name"] == "Create GitHub Release" }
abort "host has no Create GitHub Release step" unless release_index
abort "artifacts are attested only after publication" unless attest_index < release_index
verify_index = steps.index do |step|
  step.fetch("run", "").include?("scripts/verify-github-release.sh")
end
abort "host has no post-publication provenance verifier" unless verify_index && verify_index > release_index

install_jobs = %w[plan build-local-artifacts build-global-artifacts host]
install_jobs.each do |name|
  install_steps = jobs.fetch(name).fetch("steps").select { |step| step["name"] == "Install dist" }
  abort "#{name} does not have exactly one release-tooling install" unless install_steps.length == 1
  step = install_steps.fetch(0)
  abort "#{name} bypasses the verified release-tooling installer" unless step["run"] == "scripts/install-release-tooling.sh"
end
RUBY
}

if [ "${1:-}" = "--self-test" ]; then
  bad=$'permissions:\n  contents: write\nsteps:\n  - run: curl https://example.invalid/install.sh | sh'
  good=$'permissions:\n  contents: read\njobs:\n  host:\n    permissions:\n      attestations: write\n    steps:\n      - uses: actions/attest@508db95dd578ae2727ebd6217d5ba78e4fbda05d # v4'
  if check_workflow_text "$bad" >/dev/null 2>&1; then
    echo "self-test accepted pipe-to-shell release bootstrap" >&2
    exit 1
  fi
  check_workflow_text "$good"
  semantic_good="$(mktemp)"
  semantic_bad="$(mktemp)"
  trap 'rm -f "$semantic_good" "$semantic_bad"' EXIT
  # Exercise structure, not substrings: the bad fixture carries a perfectly pinned attestation in
  # the wrong job, where a grep-only policy would accept it.
  sed -n '1,9999p' .github/workflows/release.yml >"$semantic_good"
  awk '
    /- name: Attest core release artifacts/ { in_attest=1; next }
    in_attest && /- name: Create GitHub Release/ { in_attest=0 }
    !in_attest { print }
  ' "$semantic_good" >"$semantic_bad"
  printf '\n# decoy: uses: actions/attest@508db95dd578ae2727ebd6217d5ba78e4fbda05d\n' >>"$semantic_bad"
  check_workflow_semantics "$semantic_good"
  if check_workflow_semantics "$semantic_bad" >/dev/null 2>&1; then
    echo "self-test accepted an attestation that existed only as a decoy comment" >&2
    exit 1
  fi
  scripts/verify-github-release.sh --self-test
  echo "PASS self-test: unauthenticated bootstrap and structural attestation regressions rejected"
  exit 0
fi

check_workflow_text "$(<.github/workflows/release.yml)"
check_workflow_semantics .github/workflows/release.yml
grep -Fq 'scripts/install-release-tooling.sh' .github/workflows/release.yml || {
  echo "release workflow does not use the digest-verifying tooling installer" >&2
  exit 1
}
grep -Fq 'gh attestation verify' scripts/verify-github-release.sh || {
  echo "release verifier does not authenticate downloaded release artifacts" >&2
  exit 1
}
for binding in '--signer-workflow' '--source-ref' '--source-digest' '--deny-self-hosted-runners'; do
  grep -Fq -- "$binding" scripts/verify-github-release.sh || {
    echo "release verifier does not bind attestations with $binding" >&2
    exit 1
  }
done
grep -Fq 'SHA-256 mismatch' scripts/install-release-tooling.sh || {
  echo "release tooling installer has no fail-closed digest comparison" >&2
  exit 1
}
echo "PASS release bootstrap and artifact provenance policy"
