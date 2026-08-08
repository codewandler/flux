#!/usr/bin/env bash
#
# check-release-authority.sh — C-354. Every release authority is explicit and non-composable.
#
# GitHub permissions are JOB-scoped while secrets can be STEP-scoped, so "least privilege" for a
# release pipeline is a statement about the workflow/job/step graph, not about which strings appear
# in a file. This check therefore PARSES the four release workflows (aliases resolved) and reasons
# over the parsed structure: workflow/job/step nesting, `on`, `if` expressions, inherited
# permissions, `needs`, `uses`, action inputs and `env`.
#
# It is deliberately not a grep. A grep cannot tell a `MINISIGN_SECRET_KEY` in a signing step from
# the same text hoisted into a job `env` where six unrelated steps can read it, and it cannot tell a
# tag-only publication job from the same job made reachable by `workflow_dispatch`. Both of those
# are the exact regressions this file exists to stop.
#
# The authority model it enforces:
#
#   * Every release workflow declares workflow-level `contents: read` and grants no other write.
#     Any additional write permission is declared on the one job that consumes it.
#   * Provider credentials are forbidden from all four release workflows. Release availability is
#     a repository property, not an Anthropic, OpenRouter or OpenAI account property.
#   * Each publication secret is bound to the explicit (workflow, job, step) consumers in AUTHORIZED
#     below. Anywhere else — workflow `env`, job `env`, another step, or a `run` interpolation — is
#     a violation, including in a step that would "obviously" be fine.
#   * `RELEASE_TOKEN` may appear only in the isolated core promotion, plugin tag-control and GitHub
#     Release steps. Signing and Cargo publication keep their own secrets. No job holds two release
#     authorities, and no job holds one beside unrelated GitHub write scope.
#   * Core promotion gives its job-scoped Actions token only Actions write. It dispatches and
#     observes exact-SHA gates; the step-scoped PAT alone moves git refs and pushes tags.
#   * No release workflow may depend on a GitHub App variable/key, token mint, or Environment.
#   * Publication jobs are reachable only from an exact version tag: their `if` must carry the
#     tag-derived conjunct, and the value it reads must not be derivable from a dispatch input.
#
#   scripts/check-release-authority.sh              # check .github/workflows
#   scripts/check-release-authority.sh --self-test  # prove each violation class is rejected
#   scripts/check-release-authority.sh <dir>        # check a fixture directory
#
# Exit 0 clean, 1 a policy violation (a real failure).
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

MODE=check
if [ "${1:-}" = "--self-test" ]; then
  MODE=self-test
  shift
fi
WORKFLOW_DIR=${1:-.github/workflows}

# ---------------------------------------------------------------------------
# The policy, over parsed structure.
# ---------------------------------------------------------------------------
check_dir() {
  ruby - "$1" <<'RUBY'
require 'yaml'

dir = ARGV.fetch(0)
violations = []
def note(list, message) = list << message

RELEASE_WORKFLOWS = %w[release.yml release-flow.yml release-plugins.yml crates-io.yml].freeze
PROVIDER_SECRETS = %w[ANTHROPIC_API_KEY OPENROUTER_API_KEY OPENAI_API_KEY].freeze
PUBLICATION_SECRETS = %w[RELEASE_TOKEN MINISIGN_SECRET_KEY CARGO_REGISTRY_TOKEN].freeze
LONG_LIVED = (PROVIDER_SECRETS + PUBLICATION_SECRETS).freeze
REMOVED_SETTINGS = %w[PROMOTION_APP_ID PROMOTION_APP_PRIVATE_KEY mint-promotion-token.sh].freeze
# GITHUB_TOKEN is minted per job and bounded by that job's `permissions:` block, which this check
# already constrains. It is not a long-lived credential and is not part of the placement table.
AMBIENT = %w[GITHUB_TOKEN].freeze

# Every authorized consumer of a long-lived credential: workflow -> job -> step name.
AUTHORIZED = {
  'release-flow.yml' => {
    'release-control' => {
      'Promote the merged cut to a public release' => %w[RELEASE_TOKEN],
    },
  },
  'release.yml' => {
    'publish-github-release' => {
      'Create GitHub Release' => %w[RELEASE_TOKEN],
    },
  },
  'release-plugins.yml' => {
    'release-control' => {
      'Create the absent plugin tag at canonical main' => %w[RELEASE_TOKEN],
    },
    'sign' => {
      'Sign the index (minisign)' => %w[MINISIGN_SECRET_KEY],
    },
    'publish-github-release' => {
      'Create the plugins-v release' => %w[RELEASE_TOKEN],
    },
    'publish-host-kit' => {
      'Publish codewandler-flux-host-kit to crates.io' => %w[CARGO_REGISTRY_TOKEN],
    },
  },
  'crates-io.yml' => {
    'publish' => {
      'Publish the closure to crates.io' => %w[CARGO_REGISTRY_TOKEN],
    },
  },
}.freeze

# A GitHub Release step publishes a Release. It never moves a ref, dispatches a run, or mints
# another credential — so those two consumers may not contain the verbs that would.
FORBIDDEN_IN_RELEASE_TOKEN_STEP = {
  'a ref push' => /git\s+push|git\s+update-ref/,
  'a ref API write' => %r{/git/refs|/git/tags|git/ref/},
  'a tag creation' => /git\s+tag\b|gh\s+api[^\n]*tags/,
  'a workflow dispatch' => /gh\s+workflow\s+run/,
  'a credential mint' => %r{access_tokens|/installation},
}.freeze

# Psych parses YAML 1.1, so a bare `on:` key arrives as the boolean `true`.
def triggers(doc) = doc['on'] || doc[true] || {}

def walk_strings(value, &blk)
  case value
  when Hash then value.each { |k, v| walk_strings(k, &blk); walk_strings(v, &blk) }
  when Array then value.each { |v| walk_strings(v, &blk) }
  when String then blk.call(value)
  end
end

def secret_names(value)
  found = []
  walk_strings(value) do |text|
    text.scan(/secrets\.([A-Za-z_][A-Za-z0-9_]*)/) { |m| found << m.fetch(0) }
  end
  found.uniq - AMBIENT
end

# Split an expression on top-level `&&`, ignoring operators inside parentheses or quotes.
def split_conjuncts(expression)
  parts = []
  buffer = +''
  depth = 0
  quote = nil
  index = 0
  while index < expression.length
    char = expression[index]
    if quote
      quote = nil if char == quote
      buffer << char
    elsif char == "'" || char == '"'
      quote = char
      buffer << char
    elsif char == '('
      depth += 1
      buffer << char
    elsif char == ')'
      depth -= 1
      buffer << char
    elsif depth.zero? && char == '&' && expression[index + 1] == '&'
      parts << buffer
      buffer = +''
      index += 1
    else
      buffer << char
    end
    index += 1
  end
  parts << buffer
  parts.map { |part| normalize_expression(part) }.reject(&:empty?)
end

def normalize_expression(text)
  body = text.to_s.strip
  body = body.sub(/\A\$\{\{/, '').sub(/\}\}\z/, '').strip if body.start_with?('${{')
  body = body.gsub(/\s+/, ' ').strip
  body = body[1..-2].strip while body.start_with?('(') && body.end_with?(')') && balanced?(body[1..-2])
  body
end

def balanced?(text)
  depth = 0
  text.each_char do |char|
    depth += 1 if char == '('
    depth -= 1 if char == ')'
    return false if depth.negative?
  end
  depth.zero?
end

def conjuncts(job)
  raw = job['if'].to_s.strip
  raw = raw.sub(/\A\$\{\{/, '').sub(/\}\}\z/, '') if raw.start_with?('${{')
  split_conjuncts(raw)
end

def effective_permissions(doc, job)
  granted = job.key?('permissions') ? job['permissions'] : doc['permissions']
  case granted
  when Hash then granted
  when 'write-all' then { 'all' => 'write' }
  when 'read-all', 'none' then {}
  else granted.is_a?(Hash) ? granted : {}
  end
end

def writes(permissions) = permissions.select { |_, value| value == 'write' }.keys.sort

documents = {}
Dir.children(dir).sort.each do |name|
  next unless name.end_with?('.yml', '.yaml')

  path = File.join(dir, name)
  begin
    documents[name] = YAML.safe_load(File.read(path), aliases: true)
  rescue StandardError => e
    note(violations, "#{name}: cannot be parsed as Actions YAML (#{e.class})")
  end
end

# --- 1. Inventory. The policy covers a closed set; a new release workflow needs a disposition. ---
RELEASE_WORKFLOWS.each do |name|
  note(violations, "#{name}: declared release workflow is missing from #{dir}") unless documents.key?(name)
end
documents.each do |name, doc|
  next if RELEASE_WORKFLOWS.include?(name)

  escaped = secret_names(doc) & (PUBLICATION_SECRETS + %w[PROMOTION_APP_PRIVATE_KEY])
  unless escaped.empty?
    note(violations,
         "#{name}: an undeclared workflow references release authority #{escaped.join(', ')}; " \
         'add it to RELEASE_WORKFLOWS with an explicit disposition or remove the reference')
  end
  if name.match?(/\A(release|publish|crates)/)
    note(violations, "#{name}: looks like a release workflow but has no disposition in this policy")
  end
end

documents.each do |name, doc|
  next unless RELEASE_WORKFLOWS.include?(name)
  next unless doc.is_a?(Hash)

  authorized_jobs = AUTHORIZED.fetch(name, {})

  rendered = doc.to_s
  REMOVED_SETTINGS.each do |setting|
    note(violations, "#{name}: removed release setting `#{setting}` was reintroduced") if rendered.include?(setting)
  end

  # --- 2. Workflow-level authority is read-only, and holds no credential. ---
  workflow_permissions = doc['permissions']
  unless workflow_permissions.is_a?(Hash) && workflow_permissions['contents'] == 'read'
    note(violations, "#{name}: workflow-level permissions must declare `contents: read`")
  end
  if workflow_permissions.is_a?(Hash)
    escalated = writes(workflow_permissions)
    unless escalated.empty?
      note(violations,
           "#{name}: workflow-level write permission #{escalated.join(', ')} is inherited by every " \
           'job; grant it on the single job that consumes it')
    end
  end
  workflow_secrets = secret_names(doc['env'])
  unless workflow_secrets.empty?
    note(violations, "#{name}: workflow-level env exposes #{workflow_secrets.join(', ')} to every job and step")
  end

  jobs = doc['jobs'] || {}
  jobs.each do |job_name, job|
    next unless job.is_a?(Hash)

    permissions = effective_permissions(doc, job)
    job_writes = writes(permissions)
    environment = job['environment']
    environment = environment['name'] if environment.is_a?(Hash)
    steps = job['steps'] || []

    if environment
      note(violations,
           "#{name}: job `#{job_name}` depends on removed GitHub Environment `#{environment}`")
    end

    # --- 3. Job-level env never holds a long-lived credential. ---
    leaked = secret_names(job['env']) & LONG_LIVED
    unless leaked.empty?
      note(violations,
           "#{name}: job `#{job_name}` env exposes #{leaked.join(', ')} to every step in the job")
    end
    # A derived token is a credential the moment it is minted; a job env or job output spreads it
    # exactly as a repository secret would.
    if secret_names(job['outputs']).any? || job['outputs'].to_s.include?('outputs.token')
      note(violations, "#{name}: job `#{job_name}` publishes a credential as a job output")
    end
    if job['env'].to_s.include?('outputs.token')
      note(violations, "#{name}: job `#{job_name}` env holds a minted installation token")
    end

    used = []
    steps.each_with_index do |step, index|
      next unless step.is_a?(Hash)

      label = step['name'] || step['uses'] || step['id'] || "step ##{index + 1}"
      # A credential must arrive through `env:` or an action input, where its scope is the step.
      # `${{ secrets.X }}` interpolated into a `run:` body is a different thing: Actions substitutes
      # it into the script text before execution, so it also lands in the command line.
      inline = secret_names(step['run']) & LONG_LIVED
      unless inline.empty?
        note(violations,
             "#{name}: job `#{job_name}` step `#{label}` interpolates #{inline.join(', ')} into its " \
             'run body; pass a credential through the step env instead')
      end

      scoped = (secret_names(step['env']) + secret_names(step['with']) + inline).uniq & LONG_LIVED
      next if scoped.empty?

      used.concat(scoped)
      allowed = authorized_jobs.dig(job_name, label) || []
      stray = scoped - allowed
      next if stray.empty?

      note(violations,
           "#{name}: job `#{job_name}` step `#{label}` references #{stray.join(', ')} outside its " \
           'authorized step')
    end

    authorized_jobs.fetch(job_name, {}).each do |label, expected|
      step = steps.find { |candidate| candidate.is_a?(Hash) && candidate['name'] == label }
      unless step
        note(violations, "#{name}: job `#{job_name}` is missing authorized step `#{label}`")
        next
      end
      scoped = (secret_names(step['env']) + secret_names(step['with'])).uniq & LONG_LIVED
      missing = expected - scoped
      unless missing.empty?
        note(violations,
             "#{name}: job `#{job_name}` step `#{label}` is missing #{missing.join(', ')}")
      end
    end

    used.uniq!
    publication = used & PUBLICATION_SECRETS
    provider = used & PROVIDER_SECRETS

    # --- 4. Authorities do not compose inside one job. ---
    if publication.length > 1
      note(violations,
           "#{name}: job `#{job_name}` combines publication authority #{publication.join(' + ')}; " \
           'signing, GitHub Release and Cargo publication are distinct jobs')
    end
    unless provider.empty?
      note(violations,
           "#{name}: job `#{job_name}` reintroduces forbidden provider credential " \
           "#{provider.join(', ')}; release workflows must be credential-free outside publication")
    end

    # --- 5. Each RELEASE_TOKEN consumer is bound to its one host-owned purpose. ---
    next unless publication.include?('RELEASE_TOKEN')

    steps.each_with_index do |step, index|
      next unless step.is_a?(Hash)

      label = step['name'] || step['uses'] || step['id'] || "step ##{index + 1}"
      next unless (secret_names(step['env']) + secret_names(step['with'])).include?('RELEASE_TOKEN')

      body = [step['run'], step['with']].map(&:to_s).join("\n")
      purpose = [name, job_name, label]
      if purpose == ['release-flow.yml', 'release-control', 'Promote the merged cut to a public release']
        unless body.strip == 'scripts/promote-release-flow.sh'
          note(violations, "#{name}: core promotion PAT step must run only scripts/promote-release-flow.sh")
        end
        next
      end
      if purpose == ['release-plugins.yml', 'release-control', 'Create the absent plugin tag at canonical main']
        unless body.strip == 'scripts/plugin-tag-control.sh'
          note(violations, "#{name}: plugin tag PAT step must run only scripts/plugin-tag-control.sh")
        end
        next
      end

      FORBIDDEN_IN_RELEASE_TOKEN_STEP.each do |what, pattern|
        next if pattern.source.empty?
        next unless body.match?(pattern)

        note(violations,
             "#{name}: job `#{job_name}` step `#{label}` uses RELEASE_TOKEN for #{what}; it may only " \
             'create or upload a GitHub Release')
      end
    end
  end
end

# --- 6. Trigger and reachability policy, per workflow. ---
def require_conjunct(violations, name, job_name, job, needle)
  return if conjuncts(job).include?(normalize_expression(needle))

  violations << "#{name}: job `#{job_name}` is not gated on `#{needle}`"
end

release = documents['release.yml']
if release.is_a?(Hash)
  jobs = release['jobs'] || {}
  plan = jobs['plan'] || {}
  publishing = plan.dig('outputs', 'publishing').to_s
  unless publishing.include?('refs/tags/')
    violations << 'release.yml: `plan.outputs.publishing` must be derived from the pushed tag ref'
  end
  if publishing.match?(/inputs\./)
    violations << 'release.yml: `plan.outputs.publishing` reads a dispatch input, so a forged input ' \
                  'can enter the publication jobs'
  end

  %w[plan resolve-release-candidate build-local-artifacts build-global-artifacts
     record-release-candidate host].each do |job_name|
    job = jobs[job_name]
    next unless job.is_a?(Hash)

    escalated = writes(effective_permissions(release, job))
    unless escalated.empty?
      violations << "release.yml: job `#{job_name}` holds write permission #{escalated.join(', ')}; " \
                    'plan, candidate resolution, builds, receipt recording and hosting stay read-only'
    end
    if job['environment']
      violations << "release.yml: job `#{job_name}` enters a protected environment it does not publish from"
    end
  end

  attest = jobs['attest']
  if attest.is_a?(Hash)
    granted = writes(effective_permissions(release, attest))
    unless granted == %w[attestations id-token]
      violations << 'release.yml: job `attest` must hold exactly `id-token: write` and ' \
                    "`attestations: write` (found #{granted.inspect})"
    end
    require_conjunct(violations, 'release.yml', 'attest', attest, "needs.plan.outputs.publishing == 'true'")
  else
    violations << 'release.yml: attestation must be a separate tag-triggered job named `attest`'
  end

  # C-696 — the registry is a third authority, and it is declared here rather than left undeclared
  # because it is the only job that can push a public object nobody can withdraw. It holds a
  # registry session and an attestation identity and NOTHING else: no Release credential, no
  # repository write, no long-lived secret. `packages: write` bounds the ambient GITHUB_TOKEN, which
  # is why no registry PAT appears anywhere in this workflow.
  container = jobs['publish-container-image']
  if container.is_a?(Hash)
    granted = writes(effective_permissions(release, container))
    unless granted == %w[attestations id-token packages]
      violations << 'release.yml: job `publish-container-image` must hold exactly ' \
                    '`attestations: write`, `id-token: write` and `packages: write` ' \
                    "(found #{granted.inspect})"
    end
    require_conjunct(violations, 'release.yml', 'publish-container-image', container,
                     "needs.plan.outputs.publishing == 'true'")
    unless Array(container['needs']).include?('publish-github-release')
      violations << 'release.yml: the container image must be published only after the GitHub ' \
                    'Release it packages; a pushed image cannot be withdrawn'
    end
  else
    violations << 'release.yml: container image publication must be its own job ' \
                  '`publish-container-image`'
  end

  publish = jobs['publish-github-release']
  if publish.is_a?(Hash)
    require_conjunct(violations, 'release.yml', 'publish-github-release', publish,
                     "needs.plan.outputs.publishing == 'true'")
    unless Array(publish['needs']).include?('attest')
      violations << 'release.yml: the GitHub Release job must come after the attestation job'
    end
  else
    violations << 'release.yml: GitHub Release publication must be its own job `publish-github-release`'
  end
end

flow = documents['release-flow.yml']
if flow.is_a?(Hash)
  jobs = flow['jobs'] || {}
  control = jobs['release-control']
  if control.is_a?(Hash)
    require_conjunct(violations, 'release-flow.yml', 'release-control', control,
                     "github.event_name == 'push'")
    permissions = effective_permissions(flow, control)
    unless permissions['actions'] == 'write' && permissions['contents'] == 'read' &&
           permissions['pull-requests'] != 'write'
      violations << 'release-flow.yml: `release-control` must grant Actions write while Contents ' \
                    'remains read-only and Pull requests write stays absent'
    end
  else
    violations << 'release-flow.yml: promotion must live in one narrow job named `release-control`'
  end
  cut = jobs['cut']
  if cut.is_a?(Hash)
    escalated = writes(effective_permissions(flow, cut))
    unless escalated.empty?
      violations << "release-flow.yml: job `cut` holds write permission #{escalated.join(', ')}; the " \
                    'deterministic plan and local cut path takes no GitHub write token'
    end
  end
end

plugins = documents['release-plugins.yml']
if plugins.is_a?(Hash)
  on = triggers(plugins)
  tags = on.dig('push', 'tags')
  unless tags == ['plugins-v[0-9]+.[0-9]+.[0-9]+']
    violations << 'release-plugins.yml: publication must trigger on an exact ' \
                  "`plugins-v[0-9]+.[0-9]+.[0-9]+` tag push (found #{tags.inspect})"
  end
  dispatch_inputs = on.dig('workflow_dispatch', 'inputs') || {}
  if dispatch_inputs.key?('publish')
    violations << 'release-plugins.yml: `workflow_dispatch` keeps a `publish` input; manual runs are ' \
                  'build/validation only'
  end
  run_trigger = on['workflow_run']
  if run_trigger.is_a?(Hash)
    unless Array(run_trigger['workflows']) == ['ci']
      violations << 'release-plugins.yml: the controller must observe exactly the required `ci` workflow'
    end
    unless Array(run_trigger['branches']) == ['main']
      violations << 'release-plugins.yml: the controller must observe only canonical `main`'
    end
  else
    violations << 'release-plugins.yml: automatic plugin tag creation must come from a `workflow_run` of `ci`'
  end

  jobs = plugins['jobs'] || {}
  control = jobs['release-control']
  if control.is_a?(Hash)
    [
      "github.event_name == 'workflow_run'",
      "github.event.workflow_run.name == 'ci'",
      "github.event.workflow_run.conclusion == 'success'",
      "github.event.workflow_run.head_branch == 'main'",
    ].each { |needle| require_conjunct(violations, 'release-plugins.yml', 'release-control', control, needle) }
  else
    violations << 'release-plugins.yml: plugin tag creation must live in a narrow `release-control` job'
  end

  %w[sign publish-github-release publish-host-kit].each do |job_name|
    job = jobs[job_name]
    unless job.is_a?(Hash)
      violations << "release-plugins.yml: publication job `#{job_name}` is missing"
      next
    end

    [
      "github.event_name == 'push'",
      "startsWith(github.ref, 'refs/tags/plugins-v')",
    ].each { |needle| require_conjunct(violations, 'release-plugins.yml', job_name, job, needle) }
  end

  %w[build assemble].each do |job_name|
    job = jobs[job_name]
    next unless job.is_a?(Hash)

    if job['environment']
      violations << "release-plugins.yml: job `#{job_name}` is a build/validation job and must not " \
                    'enter a protected environment'
    end
  end
end

crates = documents['crates-io.yml']
if crates.is_a?(Hash)
  on = triggers(crates)
  unless on.keys == ['push']
    violations << "crates-io.yml: crates.io publication accepts only a tag push (found #{on.keys.inspect})"
  end
  tags = on.dig('push', 'tags')
  unless tags == ['v[0-9]+.[0-9]+.[0-9]+']
    violations << 'crates-io.yml: publication must trigger on an exact `v[0-9]+.[0-9]+.[0-9]+` tag ' \
                  "push (found #{tags.inspect})"
  end
  jobs = crates['jobs'] || {}
  publish = jobs['publish']
  if publish.is_a?(Hash)
    unless Array(publish['needs']).include?('validate')
      violations << 'crates-io.yml: publication must follow the secret-free validation job'
    end
  else
    violations << 'crates-io.yml: crates.io publication must be its own `publish` job'
  end
  validate = jobs['validate']
  if validate.is_a?(Hash)
    leaked = secret_names(validate) & LONG_LIVED
    unless leaked.empty?
      violations << "crates-io.yml: job `validate` reads #{leaked.join(', ')}; validation is secret-free"
    end
  else
    violations << 'crates-io.yml: version validation and packaging must be a separate `validate` job'
  end
end

if violations.empty?
  puts "PASS release authority: #{RELEASE_WORKFLOWS.length} workflows, every credential occurrence bound to an explicit step"
  exit 0
end

warn 'release authority violations:'
violations.each { |line| warn "  - #{line}" }
exit 1
RUBY
}

# ---------------------------------------------------------------------------
# Structural fixtures. Each mutates the PARSED workflow and re-serializes it, so no fixture can
# pass by accident of text layout — and none of them can be defeated by reformatting the YAML.
# ---------------------------------------------------------------------------
mutate() {
  ruby - "$WORKFLOW_DIR" "$2" "$1" <<'RUBY'
require 'yaml'
require 'fileutils'

source, dest, fixture = ARGV
FileUtils.mkdir_p(dest)
%w[release.yml release-flow.yml release-plugins.yml crates-io.yml].each do |name|
  FileUtils.cp(File.join(source, name), File.join(dest, name))
end

def load(dest, name) = YAML.safe_load(File.read(File.join(dest, name)), aliases: true)
def store(dest, name, doc) = File.write(File.join(dest, name), doc.to_yaml)

def step(doc, job, label)
  found = doc.fetch('jobs').fetch(job).fetch('steps').find { |s| s['name'] == label }
  raise "fixture cannot find step #{label} in #{job}" unless found

  found
end

case fixture
when 'workflow-secret-scope'
  doc = load(dest, 'release-plugins.yml')
  (doc['env'] ||= {})['MINISIGN_SECRET_KEY'] = '${{ secrets.MINISIGN_SECRET_KEY }}'
  store(dest, 'release-plugins.yml', doc)
when 'job-secret-scope'
  doc = load(dest, 'crates-io.yml')
  (doc['jobs']['publish']['env'] ||= {})['CARGO_REGISTRY_TOKEN'] = '${{ secrets.CARGO_REGISTRY_TOKEN }}'
  store(dest, 'crates-io.yml', doc)
when 'inherited-write-permission'
  doc = load(dest, 'release.yml')
  doc['permissions']['contents'] = 'write'
  store(dest, 'release.yml', doc)
when 'provider-credential-reintroduced'
  doc = load(dest, 'release-flow.yml')
  flow = step(doc, 'cut', 'Run the credential-free release flow')
  (flow['env'] ||= {})['ANTHROPIC_API_KEY'] = '${{ secrets.ANTHROPIC_API_KEY }}'
  store(dest, 'release-flow.yml', doc)
when 'reintroduced-environment'
  doc = load(dest, 'release-flow.yml')
  doc['jobs']['release-control']['environment'] = 'release-control'
  store(dest, 'release-flow.yml', doc)
when 'release-token-in-cut-step'
  doc = load(dest, 'release-flow.yml')
  flow = step(doc, 'cut', 'Run the credential-free release flow')
  (flow['env'] ||= {})['RELEASE_TOKEN'] = '${{ secrets.RELEASE_TOKEN }}'
  store(dest, 'release-flow.yml', doc)
when 'controller-pr-write'
  doc = load(dest, 'release-flow.yml')
  doc['jobs']['release-control']['permissions']['pull-requests'] = 'write'
  store(dest, 'release-flow.yml', doc)
when 'app-token-publication'
  doc = load(dest, 'release.yml')
  create = step(doc, 'publish-github-release', 'Create GitHub Release')
  create['env']['PROMOTION_APP_PRIVATE_KEY'] = '${{ secrets.PROMOTION_APP_PRIVATE_KEY }}'
  store(dest, 'release.yml', doc)
when 'reintroduced-app-variable'
  doc = load(dest, 'release-plugins.yml')
  control = step(doc, 'release-control', 'Create the absent plugin tag at canonical main')
  control['env']['PROMOTION_APP_ID'] = '${{ vars.PROMOTION_APP_ID }}'
  store(dest, 'release-plugins.yml', doc)
when 'missing-plugin-pat'
  doc = load(dest, 'release-plugins.yml')
  control = step(doc, 'release-control', 'Create the absent plugin tag at canonical main')
  control['env'].delete('RELEASE_TOKEN')
  store(dest, 'release-plugins.yml', doc)
when 'github-token-plugin-tag'
  doc = load(dest, 'release-plugins.yml')
  control = step(doc, 'release-control', 'Create the absent plugin tag at canonical main')
  control['env'].delete('RELEASE_TOKEN')
  control['run'] = 'git push origin HEAD:refs/tags/plugins-v1.2.3'
  store(dest, 'release-plugins.yml', doc)
when 'combined-authority'
  doc = load(dest, 'release-plugins.yml')
  publish = step(doc, 'publish-host-kit', 'Publish codewandler-flux-host-kit to crates.io')
  publish['env']['MINISIGN_SECRET_KEY'] = '${{ secrets.MINISIGN_SECRET_KEY }}'
  publish['env']['RELEASE_TOKEN'] = '${{ secrets.RELEASE_TOKEN }}'
  store(dest, 'release-plugins.yml', doc)
when 'secret-outside-authorized-step'
  doc = load(dest, 'release.yml')
  verify = step(doc, 'publish-github-release', 'Verify GitHub Release')
  (verify['env'] ||= {})['GH_TOKEN'] = '${{ secrets.RELEASE_TOKEN }}'
  store(dest, 'release.yml', doc)
when 'release-token-moves-a-ref'
  doc = load(dest, 'release.yml')
  create = step(doc, 'publish-github-release', 'Create GitHub Release')
  create['run'] = "#{create['run']}\ngit push origin HEAD:main\n"
  store(dest, 'release.yml', doc)
when 'plugin-branch-publication'
  doc = load(dest, 'release-plugins.yml')
  sign = doc['jobs']['sign']
  sign['if'] = sign['if'].to_s.sub(/\s*&&\s*startsWith\(github\.ref, 'refs\/tags\/plugins-v'\)/, '')
  store(dest, 'release-plugins.yml', doc)
when 'manual-plugin-tag-creation'
  doc = load(dest, 'release-plugins.yml')
  control = doc['jobs']['release-control']
  control['if'] = control['if'].to_s.sub("github.event_name == 'workflow_run'",
                                         "(github.event_name == 'workflow_run' || github.event_name == 'workflow_dispatch')")
  store(dest, 'release-plugins.yml', doc)
when 'plugin-controller-ignores-ci-result'
  doc = load(dest, 'release-plugins.yml')
  control = doc['jobs']['release-control']
  control['if'] = control['if'].to_s.sub(/\s*&&\s*github\.event\.workflow_run\.conclusion == 'success'/, '')
  store(dest, 'release-plugins.yml', doc)
when 'plugin-dispatch-input-publish'
  doc = load(dest, 'release-plugins.yml')
  on = doc['on'] || doc[true]
  dispatch = on['workflow_dispatch'] || {}
  (dispatch['inputs'] ||= {})['publish'] =
    { 'description' => 'publish', 'required' => false, 'type' => 'boolean', 'default' => false }
  on['workflow_dispatch'] = dispatch
  store(dest, 'release-plugins.yml', doc)
when 'tag-publication-from-dispatch'
  doc = load(dest, 'release.yml')
  # The regression is not "someone deleted the gate": it is a plausible-looking gate that admits a
  # manual candidate dispatch alongside the tag.
  doc['jobs']['attest']['if'] = "${{ needs.plan.result == 'success' }}"
  store(dest, 'release.yml', doc)
when 'publication-from-a-forged-input'
  doc = load(dest, 'release.yml')
  doc['jobs']['plan']['outputs']['publishing'] =
    "${{ startsWith(github.ref, 'refs/tags/') || inputs.publish == 'true' }}"
  store(dest, 'release.yml', doc)
when 'wrong-workflow-tag'
  doc = load(dest, 'crates-io.yml')
  on = doc['on'] || doc[true]
  on['push']['tags'] = ['v[0-9]+.[0-9]+.[0-9]+', 'plugins-v[0-9]+.[0-9]+.[0-9]+']
  store(dest, 'crates-io.yml', doc)
when 'manual-crates-publication'
  doc = load(dest, 'crates-io.yml')
  on = doc['on'] || doc[true]
  on['workflow_dispatch'] = nil
  store(dest, 'crates-io.yml', doc)
when 'attestation-write-escalation'
  doc = load(dest, 'release.yml')
  doc['jobs']['attest']['permissions']['contents'] = 'write'
  store(dest, 'release.yml', doc)
when 'container-registry-authority-escalation'
  doc = load(dest, 'release.yml')
  doc['jobs']['publish-container-image']['permissions']['contents'] = 'write'
  store(dest, 'release.yml', doc)
when 'container-publication-from-dispatch'
  doc = load(dest, 'release.yml')
  doc['jobs']['publish-container-image']['if'] = "${{ needs.plan.result == 'success' }}"
  store(dest, 'release.yml', doc)
when 'undeclared-release-workflow'
  File.write(File.join(dest, 'release-extra.yml'), {
    'name' => 'release-extra',
    'on' => { 'push' => { 'tags' => ['v*'] } },
    'permissions' => { 'contents' => 'read' },
    'jobs' => {
      'publish' => {
        'runs-on' => 'ubuntu-latest',
        'steps' => [{ 'name' => 'ship', 'env' => { 'GH_TOKEN' => '${{ secrets.RELEASE_TOKEN }}' },
                      'run' => 'gh release create x' }],
      },
    },
  }.to_yaml)
else
  raise "unknown fixture #{fixture}"
end
RUBY
}

if [ "$MODE" = "self-test" ]; then
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/flux-release-authority.XXXXXX")
  trap 'rm -rf -- "$tmp"' EXIT

  check_dir "$WORKFLOW_DIR" >/dev/null || {
    echo "self-test: the live workflows already violate the authority policy" >&2
    exit 1
  }

  # One fixture per violation class named by C-354/C-559 Acceptance. Each is a structural edit to the
  # parsed workflow — the same edit a well-meaning refactor makes by hand.
  fixtures="
workflow-secret-scope
job-secret-scope
inherited-write-permission
provider-credential-reintroduced
reintroduced-environment
release-token-in-cut-step
controller-pr-write
app-token-publication
reintroduced-app-variable
missing-plugin-pat
github-token-plugin-tag
combined-authority
secret-outside-authorized-step
release-token-moves-a-ref
plugin-branch-publication
manual-plugin-tag-creation
plugin-controller-ignores-ci-result
plugin-dispatch-input-publish
tag-publication-from-dispatch
publication-from-a-forged-input
wrong-workflow-tag
manual-crates-publication
attestation-write-escalation
container-registry-authority-escalation
container-publication-from-dispatch
undeclared-release-workflow
"
  count=0
  for fixture in $fixtures; do
    rm -rf -- "$tmp/case"
    mutate "$fixture" "$tmp/case"
    if check_dir "$tmp/case" >/dev/null 2>&1; then
      echo "FAIL self-test: the policy accepted the '$fixture' regression" >&2
      exit 1
    fi
    count=$((count + 1))
  done

  printf 'PASS self-test: %s structural authority regressions rejected\n' "$count"
  exit 0
fi

check_dir "$WORKFLOW_DIR"
