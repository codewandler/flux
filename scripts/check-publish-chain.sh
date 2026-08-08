#!/usr/bin/env bash
#
# check-publish-chain.sh — a tag build that publishes nothing must be RED (C-719).
#
# v0.59.0 was tagged, pushed, and its `Release` workflow reported **success** while creating no
# GitHub Release at all: no binaries, no attestation, no container image, no announcement. Not one
# job failed. `plan`, `resolve-release-candidate` and `host` were green and every publish job
# skipped — and GitHub reports a run whose jobs all skipped as `success`. So "the release workflow
# passed" carried no information about whether a release existed. That is the worst failure mode an
# unattended fleet has: it burns a version number and reports success.
#
# THE MECHANISM. GitHub propagates `skipped` **transitively** down the `needs` graph. A job whose
# dependency *closure* contains a skipped job is itself skipped unless its own `if:` breaks the
# chain with `always()` — and breaking it at one hop is not enough, because the skip flows straight
# through an intermediate job that ran and succeeded. Skipping `build-local-artifacts` is correct on
# the promote-a-prepared-candidate path; it is exactly what promotion is for. `host` broke the chain
# with `always()` plus explicit result checks. `attest`, `publish-github-release` and
# `publish-container-image` each carried a bare `if: needs.plan.outputs.publishing == 'true'`, so
# the skip reached them *through a successful host* and took the whole publish chain with it.
#
# WHAT THIS SCRIPT IS. GitHub Actions YAML cannot be unit-tested by running it, so this models the
# one scheduling rule the publish chain depends on — transitive skip propagation, and `always()` as
# the only thing that stops it — and drives the COMMITTED `.github/workflows/release.yml` through
# it. The `if:` expressions are read from the file, never restated here, so the model tracks the
# workflow rather than a copy of it.
#
# Three kinds of assertion:
#
#   1. STRUCTURAL. Every publish-chain job breaks the skip chain with `always()`, and every job that
#      holds publication authority asserts each of its real upstreams actually succeeded — admitting
#      a transitively-skipped graph, never a failed or skipped dependency. `verify-published` exists,
#      is gated on nothing but "this run was publishing", and reads only job results.
#   2. EXECUTED. `verify-published`'s own step script is extracted from the workflow and RUN, with
#      `needs.publish-github-release.result` substituted. It must exit non-zero for `skipped`,
#      `failure` and `cancelled`, and zero for `success`. This is the literal form of the story:
#      a tag build that published nothing exits non-zero.
#   3. SIMULATED. 216 publishing runs — every combination of `host`, `build-local-artifacts` and
#      `build-global-artifacts` results crossed with every subset of the authority jobs failing —
#      are scheduled through the model, and each one must satisfy
#      `run concluded success  =>  publish-github-release succeeded`. Plus the liveness half:
#      the promote path (both build jobs skipped, `host` green) must still publish and still be
#      green, so the fix cannot be "force a rebuild" or "fail every promotion".
#
#      Rule 3 is the property; rules 1 and 2 are how today's graph achieves it, and for today's
#      graph rule 3 is implied by them — every fixture below trips a structural rule first, and
#      that is the intended shape of a proof. It is here because the publish chain is DERIVED from
#      the `needs` graph rather than listed: a publish job added later, or a rewiring the structural
#      rules were not written for, is scheduled through this sweep on the day it lands.
#
# THE MODEL IS VALIDATED IN BOTH DIRECTIONS, against real runs of this repository's own workflow:
#
#   - `--replay` against `.github/workflows/release.yml` at `e353d528^` — the file v0.59.0 was
#     tagged from — reproduces run 31196060862 exactly: `attest`, `publish-github-release`,
#     `publish-container-image` and `announce` all `skipped`, conclusion `success`, no Release.
#   - `--replay` against the workflow as committed predicts `success` for all five, and that is what
#     runs 31246987406 (v0.59.1) and 31251445072 (v0.59.2) did — both promote-path runs with
#     `build-local-artifacts` and `build-global-artifacts` skipped, both publishing the exact
#     28-asset inventory.
#
# So the sense in which this is "a model" is narrow: on the only two graphs this repository has
# actually run, it agrees with GitHub's scheduler job for job.
#
# WHAT IT DOES NOT CATCH. It is a model, not GitHub. It does not validate YAML against the Actions
# schema, does not evaluate `fromJson`, `startsWith` or any expression outside the tiny grammar the
# publish chain uses (it aborts rather than guessing when it meets one), and takes `plan`,
# `resolve-release-candidate`, `build-local-artifacts`, `build-global-artifacts` and `host` as
# inputs rather than deciding them — which is exactly the form the v0.59.0 evidence arrived in. It
# cannot tell you a step's script is wrong, only that the graph around it publishes or is red. And
# it says nothing about whether the published bytes are correct; that is
# `scripts/verify-github-release.sh`, and the source-policy shape is
# `scripts/check-release-integrity.sh`.
#
#   scripts/check-publish-chain.sh              # check the committed release.yml
#   scripts/check-publish-chain.sh --replay     # print how a promote-path tag run schedules
#   scripts/check-publish-chain.sh --self-test  # replay v0.59.0 and prove each regression is caught
#
# Exit 0 clean, 1 a real defect, 2 a usage error.
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

WORKFLOW=.github/workflows/release.yml

# `check` enforces every rule; `replay` prints the scheduled table for a promote-path tag run so a
# human — and the self-test below — can read what the graph actually does.
run_model() {
  ruby -ryaml -rtmpdir - "$1" "$2" <<'RUBY'
mode = ARGV.fetch(0)
path = ARGV.fetch(1)

doc = YAML.safe_load(File.read(path), aliases: true)
abort "publish chain: #{path} is not a mapping" unless doc.is_a?(Hash)
JOBS = doc.fetch("jobs")

def die(message)
  abort "publish chain: #{message}"
end

# Jobs whose result the model takes as an INPUT. They are the ones whose `if:` expressions depend on
# the dist manifest, the candidate lookup and the matrix — outside this model's grammar, and already
# guarded elsewhere. Taking them as inputs is not a gap: the v0.59.0 evidence IS a table of their
# conclusions, so feeding that table in is replaying the incident rather than approximating it.
UPSTREAM = %w[plan resolve-release-candidate build-local-artifacts build-global-artifacts host].freeze

# The chain jobs that hold publication authority. Each must assert its real upstreams succeeded.
AUTHORITY = %w[attest publish-github-release publish-container-image].freeze

# The backstop: the job that exists so a publish-nothing tag run cannot be green.
BACKSTOP = "verify-published"

# A publishing (tag) run's `plan` outputs.
PUBLISHING_OUTPUTS = {
  "plan" => { "publishing" => "true", "preparing" => "false", "tag" => "v9.9.9" }
}.freeze

# `if:` is parsed into `always()` plus a conjunction of comparisons, and NOTHING else is tolerated.
# A condition this grammar cannot represent aborts the check rather than being guessed at, because a
# model that quietly mis-evaluates a publish gate is worse than no model: it would report green over
# the exact defect it exists to find.
def parse_condition(name, raw)
  die("job `#{name}` has no `if:`, so nothing decides whether it runs") if raw.nil?
  text = raw.to_s.strip.delete_prefix("${{").delete_suffix("}}").strip
  if text.include?("||")
    die("job `#{name}`'s `if:` uses `||`. This model evaluates conjunctions only and will not " \
        "guess at a disjunctive publish gate; express the gate as a conjunction, or teach the " \
        "grammar in scripts/check-publish-chain.sh what the new form means.")
  end
  always = false
  atoms = []
  text.split("&&").each do |piece|
    atom = piece.strip
    case atom
    when "always()"
      always = true
    when /\Aneeds\.([A-Za-z0-9_-]+)\.result\s*(==|!=)\s*'([^']*)'\z/
      atoms << { kind: :result, job: Regexp.last_match(1),
                 op: Regexp.last_match(2), value: Regexp.last_match(3) }
    when /\Aneeds\.([A-Za-z0-9_-]+)\.outputs\.([A-Za-z0-9_-]+)\s*(==|!=)\s*'([^']*)'\z/
      atoms << { kind: :output, job: Regexp.last_match(1), key: Regexp.last_match(2),
                 op: Regexp.last_match(3), value: Regexp.last_match(4) }
    else
      die("job `#{name}`'s `if:` contains `#{atom}`, which this model cannot evaluate. A publish " \
          "gate has to stay readable by the check that proves it cannot silently skip.")
    end
  end
  { always: always, atoms: atoms }
end

# The transitive `needs` closure — the whole point of the story. `attest` needed only `plan` and
# `host`, both green, and was skipped anyway because `build-local-artifacts` two hops up was
# skipped. Direct dependencies are not the unit of skip propagation; the closure is.
def closure(name, seen = {})
  Array(JOBS.fetch(name, {})["needs"]).each_with_object([]) do |dependency, acc|
    next if seen[dependency]

    seen[dependency] = true
    acc << dependency
    acc.concat(closure(dependency, seen))
  end
end

# The publish chain is DERIVED, not listed: every job with `host` in its needs closure is downstream
# of the asset set and therefore inherits its skips. Deriving it means a publish job added later —
# a second registry, a package index, another announcement — is held to the same rules and enters
# the simulation without anyone remembering to add it to a list here. A list is exactly the kind of
# thing that goes stale one release before it matters.
def topological(names)
  ordered = []
  pending = names.dup
  until pending.empty?
    ready = pending.select { |name| (Array(JOBS.fetch(name)["needs"]) & pending).empty? }
    die("the publish chain has a `needs` cycle among #{pending.sort.join(', ')}") if ready.empty?
    ordered.concat(ready.sort)
    pending -= ready
  end
  ordered
end

CHAIN = topological(JOBS.keys.select { |name| name != "host" && closure(name).include?("host") }).freeze

CONDITIONS = CHAIN.each_with_object({}) do |name, acc|
  job = JOBS.fetch(name)
  condition = parse_condition(name, job["if"])
  declared = Array(job["needs"])
  condition[:atoms].each do |atom|
    next if declared.include?(atom[:job])

    die("job `#{name}`'s `if:` reads `needs.#{atom[:job]}` but does not declare it in `needs:`. " \
        "Actions resolves that to the empty string, so the gate silently reads as false.")
  end
  acc[name] = condition
end.freeze

# Resolve the `${{ … }}` forms the backstop's env may use against the simulated state.
def interpolate(value, results, outputs)
  value.to_s.gsub(/\$\{\{\s*(.+?)\s*\}\}/) do
    expression = Regexp.last_match(1)
    case expression
    when /\Aneeds\.([A-Za-z0-9_-]+)\.result\z/
      results.fetch(Regexp.last_match(1), "")
    when /\Aneeds\.([A-Za-z0-9_-]+)\.outputs\.([A-Za-z0-9_-]+)\z/
      outputs.fetch(Regexp.last_match(1), {}).fetch(Regexp.last_match(2), "")
    else
      die("`#{BACKSTOP}` reads `#{expression}`, which this model cannot resolve. The backstop must " \
          "decide from job results alone — see why in the `uses:` refusal below.")
    end
  end
end

# Actually RUN the backstop's steps. This is the assertion the story asks for in its literal form:
# a tag build whose publish chain produced nothing exits non-zero.
#
# Executing workflow script is only safe because the backstop is structurally confined to reading
# job results — no `uses:`, so no checkout, no credential, no third-party action — and that
# confinement is itself checked below. It is also what makes the backstop trustworthy at runtime:
# a job that has to check out the repository can fail for reasons that have nothing to do with
# whether a Release exists, and the last word on a release must not have its own weather.
def run_backstop(results, outputs)
  job = JOBS.fetch(BACKSTOP)
  ok = true
  Dir.mktmpdir("flux-publish-chain") do |dir|
    env = { "GITHUB_STEP_SUMMARY" => File.join(dir, "summary.md"), "GITHUB_ACTIONS" => "true" }
    File.write(env.fetch("GITHUB_STEP_SUMMARY"), "")
    job.fetch("env", {}).each { |key, value| env[key] = interpolate(value, results, outputs) }
    job.fetch("steps").each_with_index do |step, index|
      script = step["run"]
      next unless script

      step_env = env.dup
      step.fetch("env", {}).each { |key, value| step_env[key] = interpolate(value, results, outputs) }
      file = File.join(dir, "step-#{index}.sh")
      File.write(file, script)
      ok = system(step_env, "bash", "-e", file, out: File::NULL, err: File::NULL)
      break unless ok
    end
  end
  ok
end

# Schedule the publish chain over `given` upstream results. `forced_failures` names chain jobs that
# run and then fail, so "the graph is fine but a publication broke" is covered too.
def simulate(given, outputs, forced_failures)
  results = given.dup
  CHAIN.each do |name|
    condition = CONDITIONS.fetch(name)
    unless condition[:always]
      inherited = closure(name).any? do |dependency|
        %w[skipped failure cancelled].include?(results.fetch(dependency, "skipped"))
      end
      if inherited
        results[name] = "skipped"
        next
      end
    end
    satisfied = condition[:atoms].all? do |atom|
      actual = case atom[:kind]
               when :result then results.fetch(atom[:job], "")
               else outputs.fetch(atom[:job], {}).fetch(atom[:key], "")
               end
      atom[:op] == "==" ? actual == atom[:value] : actual != atom[:value]
    end
    unless satisfied
      results[name] = "skipped"
      next
    end
    results[name] =
      if forced_failures.include?(name)
        "failure"
      elsif name == BACKSTOP
        run_backstop(results, outputs) ? "success" : "failure"
      else
        "success"
      end
  end
  results
end

# GitHub concludes a run `failure` if any job failed, and `success` when every job either succeeded
# or skipped. The second half is the entire defect.
def conclusion(results) = results.value?("failure") ? "failure" : "success"

# The promote path: a prepared candidate exists, so both build jobs legitimately skip.
PROMOTE_PATH = {
  "plan" => "success",
  "resolve-release-candidate" => "success",
  "build-local-artifacts" => "skipped",
  "build-global-artifacts" => "skipped",
  "host" => "success"
}.freeze

if mode == "replay"
  results = simulate(PROMOTE_PATH, PUBLISHING_OUTPUTS, [])
  # Declaration order, so the printed table reads like the incident table in the story.
  reported = JOBS.keys & (UPSTREAM + CHAIN)
  (reported + ([BACKSTOP] - reported)).each { |name| puts "#{name}\t#{results.fetch(name, 'absent')}" }
  puts "conclusion\t#{conclusion(results)}"
  exit 0
end

die("unknown mode #{mode}") unless mode == "check"

# ---------------------------------------------------------------------------
# 1. Structural: the chain is broken at every hop, and every hop still means something.
# ---------------------------------------------------------------------------
(AUTHORITY + [BACKSTOP]).each do |name|
  next if CHAIN.include?(name)

  die("`#{name}` is not a publishing job downstream of `host` in this workflow. Either it was " \
      "deleted — and the publish chain lost the guarantee it carried — or it was renamed, in which " \
      "case rename it in scripts/check-publish-chain.sh too so the rule follows it.")
end

CHAIN.each do |name|
  next if CONDITIONS.fetch(name)[:always]

  die("`#{name}` does not break the skip chain with `always()`. GitHub propagates `skipped` " \
      "transitively, so a legitimately-skipped `build-local-artifacts` will skip this job through " \
      "a green `host` — and a run whose publish jobs all skipped concludes `success`. That is " \
      "v0.59.0: a tag, no Release, no failing job.")
end

AUTHORITY.each do |name|
  condition = CONDITIONS.fetch(name)
  Array(JOBS.fetch(name)["needs"]).each do |dependency|
    next if dependency == "plan" # asserted through `outputs.publishing` instead

    asserted = condition[:atoms].any? do |atom|
      atom[:kind] == :result && atom[:job] == dependency && atom[:op] == "==" && atom[:value] == "success"
    end
    next if asserted

    die("`#{name}` uses `always()` but never asserts that `#{dependency}` succeeded. `always()` " \
        "alone does not admit a transitively-skipped graph — it admits a BROKEN one, and this job " \
        "would publish on top of a failed upstream.")
  end
  publishing = condition[:atoms].any? do |atom|
    atom[:kind] == :output && atom[:job] == "plan" && atom[:key] == "publishing" &&
      atom[:op] == "==" && atom[:value] == "true"
  end
  next if publishing

  die("`#{name}` does not restrict itself to a publishing run. With `always()` and no " \
      "`needs.plan.outputs.publishing == 'true'`, a pull-request run would enter the publish chain.")
end

backstop = JOBS.fetch(BACKSTOP)
%w[plan publish-github-release].each do |dependency|
  next if Array(backstop["needs"]).include?(dependency)

  die("`#{BACKSTOP}` does not declare `#{dependency}` in `needs:`, so it cannot read its result")
end
extra = CONDITIONS.fetch(BACKSTOP)[:atoms].reject do |atom|
  atom[:kind] == :output && atom[:job] == "plan" && atom[:key] == "publishing"
end
unless extra.empty?
  die("`#{BACKSTOP}` gates itself on #{extra.map { |a| a[:job] }.uniq.join(', ')} as well as on " \
      "`publishing`. The backstop's only condition may be that this run was supposed to publish — " \
      "any other gate skips it in precisely the case it exists to catch.")
end
if backstop.fetch("steps").any? { |step| step.key?("uses") }
  die("`#{BACKSTOP}` uses an action. The last word on whether a release exists must decide from " \
      "job results alone: a checkout, a credential or a third-party action gives the backstop its " \
      "own ways to fail, and its own ways to be skipped.")
end
if backstop.fetch("steps").any? { |step| step.key?("if") }
  die("a step in `#{BACKSTOP}` is conditional. The backstop is one unconditional assertion; a " \
      "step-level `if:` is the same defect as the job-level one, one level down.")
end

# ---------------------------------------------------------------------------
# 2. Executed: the backstop's real script, on the results that matter.
# ---------------------------------------------------------------------------
%w[skipped failure cancelled].each do |bad|
  next unless run_backstop({ "publish-github-release" => bad }, PUBLISHING_OUTPUTS)

  die("`#{BACKSTOP}` exits 0 when `publish-github-release` is `#{bad}`. A tag build that published " \
      "nothing must exit non-zero; this is the assertion the whole job exists to make.")
end
unless run_backstop({ "publish-github-release" => "success" }, PUBLISHING_OUTPUTS)
  die("`#{BACKSTOP}` fails even when the Release was published. A backstop that fails every run is " \
      "not a backstop — it is an outage, and it will be deleted within a week.")
end

# ---------------------------------------------------------------------------
# 3. Simulated: over every publishing run this model can express,
#    `run concluded success` implies `publish-github-release succeeded`.
# ---------------------------------------------------------------------------
states = %w[success skipped failure]
subsets = (0...(1 << AUTHORITY.length)).map do |mask|
  AUTHORITY.each_with_index.select { |_, index| mask[index] == 1 }.map(&:first)
end
states.each do |host_result|
  states.each do |local_result|
    states.each do |global_result|
      subsets.each do |forced|
        given = {
          "plan" => "success",
          "resolve-release-candidate" => "success",
          "build-local-artifacts" => local_result,
          "build-global-artifacts" => global_result,
          "host" => host_result
        }
        results = simulate(given, PUBLISHING_OUTPUTS, forced)
        next unless conclusion(results) == "success"
        next if results["publish-github-release"] == "success"

        die("a publishing run concludes `success` without publishing. host=#{host_result}, " \
            "build-local-artifacts=#{local_result}, build-global-artifacts=#{global_result}, " \
            "failing=#{forced.empty? ? 'none' : forced.join('+')} leaves " \
            "publish-github-release=#{results['publish-github-release']} and the run green. " \
            "That is a burnt version number reported as a release.")
      end
    end
  end
end

# The liveness half. "Fail every tag run" also satisfies the rule above, and would be a worse bug
# than the one being fixed.
promote = simulate(PROMOTE_PATH, PUBLISHING_OUTPUTS, [])
(AUTHORITY + [BACKSTOP]).each do |name|
  next if promote[name] == "success"

  die("promoting a prepared candidate leaves `#{name}` at `#{promote[name]}`. Skipping the build " \
      "jobs is what promotion IS; the fix for the skip chain must not stop the promote path from " \
      "publishing.")
end
unless conclusion(promote) == "success"
  die("the promote path concludes `#{conclusion(promote)}` even though it published")
end

# ...and the promote path must still be a promotion. A "fix" that made `build-local-artifacts` run
# unconditionally would satisfy everything above by rebuilding on the tag — throwing away the
# build-once guarantee that the candidate's immutable artifacts are the ones published.
builder = JOBS["build-local-artifacts"] || die("`build-local-artifacts` is gone; the tag run has " \
                                              "no build path left to skip on the promote path")
unless builder.fetch("if", "").to_s.include?("needs.resolve-release-candidate.outputs.run-id == ''")
  die("`build-local-artifacts` no longer skips when a prepared candidate exists. The tag run must " \
      "promote the candidate's immutable artifacts, not rebuild them.")
end
RUBY
}

# Structural fixtures. Each reverts one load-bearing part of the fix by editing the PARSED workflow
# and re-serializing it — the shape a well-meaning refactor actually takes, and the same technique
# `scripts/check-release-integrity.sh` uses for its own invariants.
mutate_release() {
  ruby -ryaml - "$WORKFLOW" "$2" "$1" <<'RUBY'
source, dest, fixture = ARGV
doc = YAML.safe_load(File.read(source), aliases: true)
jobs = doc.fetch("jobs")

def unbreak(job)
  job["if"] = job.fetch("if").sub(/always\(\)\s*&&\s*/, "")
end

def drop_atom(job, needle)
  job["if"] = job.fetch("if").sub(/\s*&&\s*#{Regexp.escape(needle)}/, "")
end

case fixture
when "attest-loses-always"
  unbreak(jobs.fetch("attest"))
when "publish-github-release-loses-always"
  unbreak(jobs.fetch("publish-github-release"))
when "publish-container-image-loses-always"
  unbreak(jobs.fetch("publish-container-image"))
when "announce-loses-always"
  unbreak(jobs.fetch("announce"))
when "the-v0590-graph"
  # The workflow exactly as it stood when v0.59.0 was tagged: three bare `publishing == 'true'`
  # gates below a `host` that broke the chain correctly, and no backstop at all.
  #
  # This is a synthetic reconstruction rather than the file out of history, because CI checks out
  # at depth 1 and cannot read a parent commit. It is not a guess: `--replay` against the real
  # `.github/workflows/release.yml` at `e353d528^` prints a byte-identical table to the one the
  # self-test asserts below — and that table is the one recorded in the story from the actual run.
  %w[attest publish-github-release publish-container-image].each do |name|
    job = jobs.fetch(name)
    job["if"] = "${{ needs.plan.outputs.publishing == 'true' }}"
  end
  jobs.delete("verify-published")
when "verify-published-deleted"
  jobs.delete("verify-published")
when "verify-published-gated-on-the-result-it-checks"
  # The most plausible regression of all: it looks like tightening, and it disables the job in
  # exactly the case it exists for.
  jobs.fetch("verify-published")["if"] =
    "${{ always() && needs.plan.outputs.publishing == 'true' && " \
    "needs.publish-github-release.result == 'success' }}"
when "verify-published-accepts-a-skipped-publish"
  jobs.fetch("verify-published").fetch("steps").each do |step|
    next unless step.key?("run")

    step["run"] = "echo \"publish-github-release was $RELEASE_RESULT\"\n"
  end
when "verify-published-assertion-made-conditional"
  jobs.fetch("verify-published").fetch("steps").each do |step|
    next unless step.key?("run")

    step["if"] = "${{ needs.publish-github-release.result != 'success' }}"
  end
when "verify-published-needs-a-checkout"
  # A backstop that checks out the repository can fail — and be skipped — for reasons unrelated to
  # whether a Release exists.
  jobs.fetch("verify-published").fetch("steps").unshift(
    { "uses" => "actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803" }
  )
when "attest-admits-a-failed-host"
  drop_atom(jobs.fetch("attest"), "needs.host.result == 'success'")
when "publish-github-release-admits-a-failed-attest"
  drop_atom(jobs.fetch("publish-github-release"), "needs.attest.result == 'success'")
when "container-image-admits-an-unpublished-release"
  drop_atom(jobs.fetch("publish-container-image"), "needs.publish-github-release.result == 'success'")
when "promote-path-forces-a-rebuild"
  # Rebuilding on the tag would satisfy every skip rule and quietly discard build-once: the bytes
  # published would no longer be the bytes the candidate gate approved.
  job = jobs.fetch("build-local-artifacts")
  job["if"] = job.fetch("if").sub(
    "(needs.plan.outputs.publishing == 'true' && needs.resolve-release-candidate.outputs.run-id == '')",
    "needs.plan.outputs.publishing == 'true'"
  )
else
  raise "unknown fixture #{fixture}"
end

File.write(dest, doc.to_yaml)
RUBY
}

case "${1:-}" in
  "")
    run_model check "$WORKFLOW"
    echo "PASS publish chain: a publishing run cannot conclude success without a published Release"
    exit 0
    ;;
  --replay)
    run_model replay "${2:-$WORKFLOW}"
    exit 0
    ;;
  --self-test) ;;
  -h|--help)
    sed -n '2,77p' "$0" >&2
    exit 0
    ;;
  *)
    echo "usage: scripts/check-publish-chain.sh [--replay [workflow]] [--self-test]" >&2
    exit 2
    ;;
esac

tmp=$(mktemp -d "${TMPDIR:-/tmp}/flux-publish-chain.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT

# The unmutated re-serialization must pass: every fixture below differs from the live workflow by
# exactly one structural edit, not by YAML formatting.
ruby -ryaml -e 'File.write(ARGV[1], YAML.safe_load(File.read(ARGV[0]), aliases: true).to_yaml)' \
  "$WORKFLOW" "$tmp/good.yml"
run_model check "$tmp/good.yml" >/dev/null

# The failing-first evidence, and the proof that the model is faithful rather than merely strict:
# the v0.59.0 workflow, scheduled over the v0.59.0 upstream table, must reproduce the v0.59.0 run —
# every publish job skipped, and the run green.
mutate_release the-v0590-graph "$tmp/v0590.yml"
replay=$(run_model replay "$tmp/v0590.yml")
expected='plan	success
resolve-release-candidate	success
build-local-artifacts	skipped
build-global-artifacts	skipped
host	success
attest	skipped
publish-github-release	skipped
publish-container-image	skipped
announce	skipped
verify-published	absent
conclusion	success'
if [ "$replay" != "$expected" ]; then
  echo "self-test: the model does not reproduce the v0.59.0 run it exists to forbid. Got:" >&2
  printf '%s\n' "$replay" >&2
  exit 1
fi

# ...and the same upstream table against the workflow as committed must publish instead.
replay=$(run_model replay "$WORKFLOW")
expected='plan	success
resolve-release-candidate	success
build-local-artifacts	skipped
build-global-artifacts	skipped
host	success
attest	success
publish-github-release	success
publish-container-image	success
announce	success
verify-published	success
conclusion	success'
if [ "$replay" != "$expected" ]; then
  echo "self-test: promoting a prepared candidate no longer publishes. Got:" >&2
  printf '%s\n' "$replay" >&2
  exit 1
fi

for fixture in \
  attest-loses-always \
  publish-github-release-loses-always \
  publish-container-image-loses-always \
  announce-loses-always \
  the-v0590-graph \
  verify-published-deleted \
  verify-published-gated-on-the-result-it-checks \
  verify-published-accepts-a-skipped-publish \
  verify-published-assertion-made-conditional \
  verify-published-needs-a-checkout \
  attest-admits-a-failed-host \
  publish-github-release-admits-a-failed-attest \
  container-image-admits-an-unpublished-release \
  promote-path-forces-a-rebuild
do
  mutate_release "$fixture" "$tmp/bad.yml"
  if run_model check "$tmp/bad.yml" >/dev/null 2>&1; then
    echo "self-test accepted the '$fixture' regression" >&2
    exit 1
  fi
done

echo "PASS self-test: v0.59.0 replays green-and-empty, and every way back into it is rejected"
