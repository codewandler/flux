#!/usr/bin/env bash
# Source-policy guard for C-259: release jobs may download data, but may not execute an unauthenticated
# remote installer, and every core release must publish + verify provenance attestations.
#
# C-354 split the old all-powerful `host` job into three: `host` assembles and CHECKS the asset set
# with no authority at all, `attest` holds only the attestation identity, and
# `publish-github-release` holds only the Release credential. The C-412 orderings this file exists
# to protect did not change — they are now expressed across the `needs` graph instead of inside one
# step list, and they are checked that way here.
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
abort "release workflow must fix hard-coded target consumers to target" unless
  doc.fetch("env", {})["CARGO_TARGET_DIR"] == "target"
permissions = doc.fetch("permissions")
abort "release workflow default contents permission is not read" unless permissions["contents"] == "read"
jobs = doc.fetch("jobs")

host = jobs.fetch("host")
attest_job = jobs.fetch("attest") { abort "attestation must be its own job (C-354)" }
publish_job = jobs.fetch("publish-github-release") { abort "GitHub Release publication must be its own job (C-354)" }

# The assembling job holds no authority: it reads Actions state and the repository, nothing more.
{ "actions" => "read", "contents" => "read" }.each do |name, value|
  abort "host permission #{name} must be #{value}" unless host.fetch("permissions")[name] == value
end
attest_permissions = attest_job.fetch("permissions")
%w[attestations id-token].each do |permission|
  abort "attest permission #{permission} must be write" unless attest_permissions[permission] == "write"
end

# C-696 — the attestation-identity allowlist, and why it has two names rather than one.
#
# `attestations`/`id-token: write` is the identity that mints a provenance statement. It was
# confined to `attest` because nothing else produced an artifact to attest. Publishing the container
# image does: the image is a second artifact, and it comes into existence AFTER `attest` has already
# run against `artifacts/*` and finished. Its statement therefore cannot be minted there.
#
# The alternative was to widen `attest` into a job that also holds `packages: write` and an
# authenticated registry session — which would put the release archives' signing identity in the
# same job as a registry push, and give a defect in the image build reach over the archive
# attestation. A separate job is strictly narrower, so the image publisher is named here instead.
#
# The rule is unchanged in kind: exactly the jobs that publish an attestation, and no other. Every
# name on this list is checked structurally below for being the thing it claims to be. A third name
# is a decision to take deliberately, not a formatting change.
ATTESTING_JOBS = %w[attest publish-container-image].freeze

jobs.each do |name, job|
  granted = job.fetch("permissions", {})
  abort "repository write permission escaped into job #{name}" if granted["contents"] == "write"
  next if ATTESTING_JOBS.include?(name)

  %w[attestations id-token].each do |permission|
    abort "publication permission #{permission}: write escaped into job #{name}" if granted[permission] == "write"
  end
end

contains_release_token = lambda do |value|
  case value
  when Hash then value.any? { |key, child| contains_release_token.call(key) || contains_release_token.call(child) }
  when Array then value.any? { |child| contains_release_token.call(child) }
  when String then value.match?(/\$\{\{\s*secrets\.RELEASE_TOKEN\s*\}\}/)
  else false
  end
end
jobs.each do |name, job|
  next if name == "publish-github-release"

  abort "RELEASE_TOKEN escaped into job #{name}" if contains_release_token.call(job)
end

attest_steps = attest_job.fetch("steps")
attest = attest_steps.each_index.select do |index|
  attest_steps[index].fetch("uses", "").match?(/\Aactions\/attest@[0-9a-f]{40}\z/)
end
abort "the attest job must contain exactly one SHA-pinned actions/attest step" unless attest.length == 1
attest_step = attest_steps.fetch(attest.fetch(0))
abort "attestation step may not be conditional or disabled" if attest_step.key?("if")
abort "attestation subject must be artifacts/*" unless attest_step.fetch("with", {})["subject-path"] == "artifacts/*"

# C-696 — the second name on ATTESTING_JOBS has to be the container publisher, and has to publish
# the binary the release already attested. Without these, "publish-container-image" would be a name
# any job could take to obtain the attestation identity.
container_job = jobs.fetch("publish-container-image") do
  abort "container image publication must be its own job `publish-container-image` (C-696)"
end
container_permissions = container_job.fetch("permissions")
%w[attestations id-token packages].each do |permission|
  abort "publish-container-image permission #{permission} must be write" unless
    container_permissions[permission] == "write"
end
container_steps = container_job.fetch("steps")
abort "publish-container-image does not consume the checked asset set" unless
  container_steps.any? do |step|
    step.fetch("uses", "").start_with?("actions/download-artifact@") &&
      step.fetch("with", {})["name"] == "release-staged-assets"
  end
# The whole provenance claim rests on this: the image carries the archive the release published, so
# a compile here — or a build from a loose binary — would put an unattested binary in the layer
# while every downstream check still reported green.
container_steps.each do |step|
  run = step.fetch("run", "")
  abort "publish-container-image compiles the binary it publishes instead of repacking the released one (C-696)" if
    run.match?(/(?:^|\s)(?:cargo|dist)\s+build\b/)
  abort "publish-container-image builds the image from an unreleased binary (C-696)" if
    run.match?(/build-image\.sh[^\n]*--binary/)
end
abort "publish-container-image does not repack the staged release archive (C-696)" unless
  container_steps.any? { |step| step.fetch("run", "").match?(%r{deploy/container/build-image\.sh[^\n]*--staged}) }
image_attestations = container_steps.select do |step|
  step.fetch("uses", "").match?(%r{\Aactions/attest-build-provenance@[0-9a-f]{40}\z})
end
abort "publish-container-image must contain exactly one SHA-pinned image provenance attestation" unless
  image_attestations.length == 1
image_attestation = image_attestations.fetch(0)
abort "the image attestation may not be conditional or disabled" if image_attestation.key?("if")
image_attestation_with = image_attestation.fetch("with", {})
# By digest: a tag is mutable, so an attestation naming one would describe whatever that tag points
# at later rather than the bytes this run pushed.
abort "the image attestation must name the pushed manifest digest, not a mutable tag" unless
  image_attestation_with["subject-digest"].to_s.include?("steps.push.outputs.digest")
abort "the image attestation is not pushed to the registry beside the image" unless
  image_attestation_with["push-to-registry"] == true
%w[host attest publish-github-release].each do |dependency|
  abort "publish-container-image does not wait for #{dependency}" unless
    Array(container_job["needs"]).include?(dependency)
end

publish_steps = publish_job.fetch("steps")
release_index = publish_steps.index { |step| step["name"] == "Create GitHub Release" }
abort "the publication job has no Create GitHub Release step" unless release_index
# The post-publication verifier specifically: it is the mode that checks provenance attestations,
# which only exist once the assets are downloadable. `--staged` is the same script in its
# pre-publication mode and is asserted separately below — matching it here would satisfy this check
# with a step that never verifies an attestation at all.
verify_index = publish_steps.index do |step|
  run = step.fetch("run", "")
  run.include?("scripts/verify-github-release.sh") && !run.include?("--staged")
end
abort "the publication job has no post-publication provenance verifier" unless verify_index && verify_index > release_index

install_jobs = %w[plan build-local-artifacts build-global-artifacts host]
install_jobs.each do |name|
  install_steps = jobs.fetch(name).fetch("steps").select { |step| step["name"] == "Install dist" }
  abort "#{name} does not have exactly one release-tooling install" unless install_steps.length == 1
  step = install_steps.fetch(0)
  abort "#{name} bypasses the verified release-tooling installer" unless step["run"] == "scripts/install-release-tooling.sh"
end

# release.yml is cargo-dist-generated. Every `dist build` is a Cargo-output frontend and must remain
# inside the checked-in pre-Cargo ownership wrapper after regeneration.
dist_builds = jobs.values.flat_map { |job| job.fetch("steps", []) }.select do |step|
  step.fetch("run", "").match?(/(?:^|\s)dist\s+build\b/)
end
abort "release workflow changed its expected Unix/Windows/global dist-build inventory" unless dist_builds.length == 3
dist_builds.each do |step|
  run = step.fetch("run", "")
  abort "cargo-dist build bypasses build ownership" unless
    run.include?("build_ownership.py shared") && run.include?("-- dist build")
end

# C-412 — the asset set is verified BEFORE it is published, not only after.
#
# `Create GitHub Release` publishes `artifacts/*` and the post-publication verifier then reports on
# what came out. On v0.47.0's tag run the create step succeeded with a directory holding only
# `dist-manifest.json` and the verifier failed afterwards, against a Release that was already public
# with `/releases/latest` pointing at it. The pre-publication check is what makes that unreachable.
#
# Since C-354 the check and the publication live in different jobs, so "before" is a `needs` edge
# plus a step order inside `host` — the check must precede the handoff that makes the bytes
# available to the authority-bearing jobs at all. A check that drifts below either boundary is the
# defect, not a style regression.
host_steps = host.fetch("steps")
staged_index = host_steps.index do |step|
  step.fetch("run", "").match?(/scripts\/verify-github-release\.sh\s+--staged/)
end
abort "host does not verify the artifact set before publishing it (C-412)" unless staged_index
abort "the pre-publication asset check may not be conditional or disabled" if host_steps.fetch(staged_index).key?("if")
handoff_index = host_steps.index do |step|
  step.fetch("uses", "").start_with?("actions/upload-artifact@") &&
    step.fetch("with", {})["name"] == "release-staged-assets"
end
abort "host does not hand the checked asset set to the publication jobs" unless handoff_index
abort "host hands the asset set to the publication jobs before checking it" unless staged_index < handoff_index
abort "the attestation job does not wait for the checked artifact set" unless
  Array(attest_job["needs"]).include?("host")
abort "publication does not wait for the checked artifact set" unless
  Array(publish_job["needs"]).include?("host")
abort "artifacts are attested only after publication" unless
  Array(publish_job["needs"]).include?("attest")

# C-355 — the promotion source is authenticated before anything consumes it.
#
# The tag run used to take the promotion source with `pattern: artifacts-*` + `merge-multiple: true`
# and hand the merged directory straight to `dist host`. The receipt now binds seven immutable IDs,
# sizes and digests, and the consumer hashes the raw ZIP bytes and extracts into per-record
# namespaces. That step is worthless if it drifts below the first consumer of the bytes, so its
# position relative to `dist host`, the staged asset check and the handoff is the invariant.
candidate_index = host_steps.index do |step|
  step["name"] == "Verify and safely assemble the receipt-bound candidate bytes"
end
abort "host does not verify and safely extract the receipt-bound candidate bytes (C-355)" unless candidate_index
dist_host_index = host_steps.index { |step| step.fetch("run", "").match?(/(?:^|\s)dist\s+host\b/) }
abort "host has no dist host step" unless dist_host_index
abort "the candidate bytes reach `dist host` before they are verified" unless candidate_index < dist_host_index
abort "the candidate bytes are verified only after the staged asset check" unless candidate_index < staged_index
abort "the candidate bytes are verified only after the publication handoff" unless candidate_index < handoff_index
host_steps.each do |step|
  next unless step.fetch("uses", "").start_with?("actions/download-artifact@")

  with = step.fetch("with", {})
  next unless with["run-id"]
  next unless with["pattern"] || with["merge-multiple"]

  abort "the promotion source is downloaded from the candidate run by pattern; " \
        "`merge-multiple: true` is not the trust boundary (C-355)"
end

# The planning job may inspect hosting (`--steps=check`) but may not ask to create it. In the pinned
# dist 0.32.0 `--steps=create` is inert on the GitHub backend — it is NOT what published v0.47.0, and
# anyone reading this check should not conclude that it was. It is forbidden because a planning job
# should not hold a verb that asks to create a public object, and because `create` is inert only by
# an upstream implementation detail that a future dist may fill in.
jobs.fetch("plan").fetch("steps").each do |step|
  next unless step.fetch("run", "").match?(/dist\s+host\b[^\n]*--steps=create/)

  abort "the plan job asks dist to create hosting; planning may inspect it (--steps=check) but not create it (C-412)"
end

# ...and no job may publish without first establishing that the builds ran. `host`'s own `if:`
# tolerates skipped build jobs because promoting a prepared candidate legitimately skips them, so
# "skipped" has to be told apart from "nothing was built" inside the job, before publication.
build_results = %w[build-local-artifacts build-global-artifacts].map { |job| "needs.#{job}.result" }
checks_build_results = lambda do |step|
  text = [step.fetch("run", ""), step.fetch("env", {}).values.join("\n")].join("\n")
  build_results.all? { |reference| text.include?(reference) }
end
guard_index = host_steps.index { |step| checks_build_results.call(step) }
abort "host never checks whether the build jobs actually ran" unless guard_index
abort "host checks the build results only after handing them on" unless guard_index < handoff_index

# The preparation half of the same defect: a candidate run whose builds skipped must FAIL, not skip
# its way to a green `success` that a human then reads as "ready to tag".
candidate = jobs.fetch("record-release-candidate")
build_results.each do |reference|
  next unless candidate.fetch("if", "").include?(reference)

  abort "record-release-candidate gates itself on #{reference}, so a run that builds nothing skips silently (C-412)"
end
abort "record-release-candidate does not fail when the build jobs did not run" unless
  candidate.fetch("steps").any? { |step| checks_build_results.call(step) }
RUBY
}

# Structural fixtures. Each one reverts a load-bearing part of the fix by editing the PARSED
# workflow and re-serializing it — the shape a well-meaning refactor actually takes. Text-order
# mutation is not used, because since C-354 the invariants span jobs rather than lines.
mutate_release() {
  ruby -ryaml - .github/workflows/release.yml "$2" "$1" <<'RUBY'
source, dest, fixture = ARGV
doc = YAML.safe_load(File.read(source), aliases: true)
jobs = doc.fetch("jobs")

def step_index(job, &predicate) = job.fetch("steps").index(&predicate)

case fixture
when "attestation-as-a-decoy-comment"
  # The perfectly pinned attestation still exists in the file — as a comment. A grep-only policy
  # accepts it; a parsed one cannot see it at all.
  attest = jobs.fetch("attest")
  index = step_index(attest) { |step| step.fetch("uses", "").start_with?("actions/attest@") }
  attest.fetch("steps").delete_at(index)
  File.write(dest, doc.to_yaml + "\n# decoy: uses: actions/attest@508db95dd578ae2727ebd6217d5ba78e4fbda05d\n")
  exit 0
when "bare-cargo-dist-build"
  # Generated cargo-dist workflows restore bare `dist build` unless the local ownership override is
  # re-applied. An otherwise-valid regeneration must still be rejected.
  jobs.each_value do |job|
    Array(job["steps"]).each do |step|
      run = step["run"].to_s
      next unless run.include?("-- dist build")

      step["run"] = run.sub(/scripts[\/\\]run-python3\.\w+ scripts[\/\\]build_ownership\.py shared --workspace-root "[^"]+" -- dist build/, "dist build")
    end
  end
when "no-pre-publication-asset-check"
  host = jobs.fetch("host")
  index = step_index(host) { |step| step.fetch("run", "").include?("verify-github-release.sh --staged") }
  host.fetch("steps").delete_at(index)
when "asset-check-after-the-handoff"
  # The way this regresses in practice: the step is moved, not deleted.
  host = jobs.fetch("host")
  index = step_index(host) { |step| step.fetch("run", "").include?("verify-github-release.sh --staged") }
  moved = host.fetch("steps").delete_at(index)
  host.fetch("steps") << moved
when "attestation-does-not-wait-for-the-check"
  jobs.fetch("attest")["needs"] = ["plan"]
when "publication-does-not-wait-for-the-attestation"
  jobs.fetch("publish-github-release")["needs"] = %w[plan host]
when "hosting-create-verb-in-plan"
  plan = jobs.fetch("plan")
  plan.fetch("steps").each do |step|
    next unless step["run"].to_s.include?("dist plan --tag=")

    step["run"] = step["run"].gsub("dist plan --tag=", "dist host --steps=create --tag=")
  end
when "publishes-without-checking-the-builds"
  host = jobs.fetch("host")
  index = step_index(host) { |step| step["name"] == "Refuse to publish a release with nothing built" }
  host.fetch("steps").delete_at(index)
when "candidate-receipt-skips-when-nothing-built"
  candidate = jobs.fetch("record-release-candidate")
  candidate["if"] = "${{ always() && needs.plan.outputs.preparing == 'true' && " \
                    "needs.build-local-artifacts.result == 'success' && " \
                    "needs.build-global-artifacts.result == 'success' }}"
when "candidate-bytes-consumed-before-verification"
  host = jobs.fetch("host")
  index = step_index(host) { |step| step["name"] == "Verify and safely assemble the receipt-bound candidate bytes" }
  moved = host.fetch("steps").delete_at(index)
  host.fetch("steps") << moved
when "promotion-source-downloaded-by-pattern"
  host = jobs.fetch("host")
  index = step_index(host) { |step| step["name"] == "Download the candidate receipt" }
  step = host.fetch("steps").fetch(index)
  step.fetch("with").delete("name")
  step.fetch("with")["pattern"] = "artifacts-*"
  step.fetch("with")["merge-multiple"] = true
when "release-token-escapes-the-publication-job"
  jobs.fetch("host")["env"]["GH_TOKEN"] = "${{ secrets.RELEASE_TOKEN }}"
when "attestation-write-escapes-into-a-build-job"
  jobs.fetch("build-global-artifacts")["permissions"] = { "attestations" => "write" }
when "container-image-built-from-source"
  # The regression that makes the whole provenance claim false while every job still goes green:
  # the image is built, tagged and attested exactly as before, from a binary nothing attested.
  container = jobs.fetch("publish-container-image")
  index = step_index(container) { |step| step.fetch("run", "").include?("build-image.sh --staged") }
  container.fetch("steps").fetch(index)["run"] =
    "cargo build --release --bin flux\n" \
    "deploy/container/build-image.sh --binary target/release/flux --tag \"$REFERENCE\"\n"
when "container-image-published-without-attestation"
  container = jobs.fetch("publish-container-image")
  index = step_index(container) { |step| step.fetch("uses", "").start_with?("actions/attest-build-provenance@") }
  container.fetch("steps").delete_at(index)
when "container-image-attested-by-tag"
  # A mutable tag as the attestation subject: the statement stops describing these bytes the next
  # time anything moves the tag.
  container = jobs.fetch("publish-container-image")
  index = step_index(container) { |step| step.fetch("uses", "").start_with?("actions/attest-build-provenance@") }
  container.fetch("steps").fetch(index).fetch("with")["subject-digest"] = "${{ steps.image.outputs.reference }}"
when "container-image-published-before-the-release"
  # An image pushed for a release that then failed to publish is the one publication that cannot be
  # withdrawn.
  jobs.fetch("publish-container-image")["needs"] = %w[plan host attest]
else
  raise "unknown fixture #{fixture}"
end

File.write(dest, doc.to_yaml)
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

  tmp=$(mktemp -d "${TMPDIR:-/tmp}/flux-release-integrity.XXXXXX")
  trap 'rm -rf -- "$tmp"' EXIT

  # The unmutated re-serialization must still pass: every fixture below differs from the live
  # workflow by exactly one structural edit, not by YAML formatting.
  ruby -ryaml -e 'File.write(ARGV[1], YAML.safe_load(File.read(ARGV[0]), aliases: true).to_yaml)' \
    .github/workflows/release.yml "$tmp/good.yml"
  check_workflow_semantics "$tmp/good.yml"

  for fixture in \
    attestation-as-a-decoy-comment \
    bare-cargo-dist-build \
    no-pre-publication-asset-check \
    asset-check-after-the-handoff \
    attestation-does-not-wait-for-the-check \
    publication-does-not-wait-for-the-attestation \
    hosting-create-verb-in-plan \
    publishes-without-checking-the-builds \
    candidate-receipt-skips-when-nothing-built \
    candidate-bytes-consumed-before-verification \
    promotion-source-downloaded-by-pattern \
    release-token-escapes-the-publication-job \
    attestation-write-escapes-into-a-build-job \
    container-image-built-from-source \
    container-image-published-without-attestation \
    container-image-attested-by-tag \
    container-image-published-before-the-release
  do
    mutate_release "$fixture" "$tmp/bad.yml"
    if check_workflow_semantics "$tmp/bad.yml" >/dev/null 2>&1; then
      echo "self-test accepted the '$fixture' regression" >&2
      exit 1
    fi
  done

  scripts/verify-github-release.sh --self-test
  echo "PASS self-test: unauthenticated bootstrap and structural attestation/release-ordering regressions rejected"
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
