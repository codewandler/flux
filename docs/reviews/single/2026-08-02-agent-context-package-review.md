---
title: Agent context-package review — role, authority, tooling, and repository-policy clarity
date: 2026-08-02
kind: internal-review
lens: context-architecture-and-instruction-clarity
method: >
  Desk review of the supplied system, orchestration, repository, environment, Git, and prior-response
  context. No workspace implementation claims were independently verified; the pass evaluates only
  the visible context's internal clarity, precedence, scope, completeness, and organization.
reviewer: agent
subject:
  repo: codewandler/flux
  scope: supplied agent context package and the prior context summary
verdict: >
  The package contains strong safety and engineering rules but delivers them as one dense, weakly
  typed body. Role, execution mode, authority, active tools, dynamic workspace evidence, and durable
  repository policy are insufficiently separated. The highest-leverage repair is a precedence-aware,
  task-filtered context envelope with explicit source types, freshness, activation conditions, and a
  single agent lifecycle.
triage:
  kind: single
  status: triaged
  owner_stories: [A-147]
  aggregated_into: null
---

# Agent context-package review — 2026-08-02

> The central packaging findings are addressed by
> [A-147](../../stories/A-147-layered-harness-context.md). The implementation separates
> the embedded harness protocol, optional profiles, authored instructions, repository policy, and
> workspace snapshots; adds typed provenance/manifests and `flux context show`; and reduces root
> `AGENTS.md` to a host-agnostic repository contract. Follow-on context selection can build on those
> types without reintroducing one caller-replaceable prompt string.

This is a review of the visible context package, not of hidden orchestration or omitted repository
files. Its findings cover confusing, conflicting, redundant, or poorly organized elements and give
concrete restructuring guidance.

## Headline

The context has good individual rules but no sufficiently explicit model for how they compose. A
reader must infer whether the agent is planning or executing, which tools exist, which statements are
policy versus data, how request-local constraints override defaults, and which specialized repository
rules apply to the task. Safety-critical requirements then compete with historical rationale and
subsystem runbooks in one flat prompt.

## Findings

### 1. Competing agent identities

The assistant is described as a generic API assistant, a staged planning agent, and a precise,
autonomous coding agent. Planning and execution have materially different responsibilities. Define
one primary role and a state machine such as inspect → propose/capture → approve → execute → verify →
report. If modes differ, expose one authoritative `mode: plan | execute | review` field.

### 2. Request-local tool constraints conflict with the default mandate

A request may prohibit tools and effects while the general workflow requires evidence gathering and
end-to-end implementation. Put current-turn constraints in an authoritative envelope such as
`mode`, `tool_policy`, `effects`, and `evidence_scope`, and state that it narrows the default workflow.

### 3. Tool inventory does not match operational instructions

The workflow references workspace, Git, Cargo, shell, environment, and task operations that may not
be visible in the selected tool set. Separate currently visible operations, activatable capability
families, and the operations exposed by each activation.

### 4. Capability signaling is underspecified

The context does not fully define persistence, approval, concrete tool mappings, family limits,
deactivation, or interaction with a request that forbids tools. Publish a capability lifecycle and a
family-to-operation map.

### 5. “Only operations selected” is ambiguous

That phrase may mean visible, authorized, or merely activatable operations. Use distinct fields for
`visible_tools`, `authorized_effects`, `activatable_capabilities`, and `prohibited_operations`.

### 6. Action-capture behavior lacks a protocol

The context says an action may be captured rather than executed, but does not define eligible calls,
result shape, or next transitions. Standardize results such as `executed | captured | denied | failed`
with action and batch identifiers.

### 7. Plan finalization has unclear timing and consequences

It is not explicit whether finalization prompts for approval, causes later automatic execution, may
include reads, or leads to another turn. Document the complete gather → propose → finalize → approve
→ execute → verify lifecycle.

### 8. Decision requests are too narrowly described

The decision operation is tied to newly discovered alternatives, while the wider policy also covers
user preference and destructive ambiguity present from the start. Use one rule: ask only when a
material user-owned choice cannot be resolved from evidence.

### 9. User-supplied intent contracts resemble trusted control metadata

Structured intent fields in user content look like orchestration state. Runtime contracts should
come from a trusted channel, or the system should define which user-supplied fields are enforceable
and how they are validated.

### 10. Structured and natural-language requests duplicate authority

If the contract and prose diverge, precedence is unclear. Normalize them into one task object or
explicitly validate and report inconsistencies.

### 11. “Supplied context only” has fuzzy source boundaries

It is unclear whether this includes assistant derivations, injected Git metadata, embedded files, and
all instruction layers. Declare an allowed source set and whether prior assistant summaries count as
evidence or only derived material.

### 12. The required evidence checklist has no representation

The workflow requires a checklist for multipart work but gives no schema or visibility rule. Either
keep it as an internal implementation detail or define fields for requirement, source, evidence, and
completion.

### 13. Citation rules do not fit context-only reviews

Exact tool-result identifiers cannot cite injected messages without stable IDs. Define citations by
source class: `path:line` for files, result IDs for tools, and named anchors for injected policy or
metadata.

### 14. Line citations are impossible for unnumbered injected files

Embedded policy, environment, and Git sections lack stable line numbers. Supply numbered snapshots or
permit path-only and section-anchor citations.

### 15. The path-knowledge rule omits trusted injected metadata

Paths appear in developer-supplied repository context as well as user text and tool output. Treat a
path as known when explicitly present in any trusted context source, while still verifying existence
before mutation.

### 16. Mandatory inventory may duplicate an injected inventory

The rule to inventory when no path is known does not say whether a recent trusted repository snapshot
satisfies it. Attach freshness metadata and require a new inventory only when the supplied one is
absent or stale for the task.

### 17. Evidence-acquisition rules are repetitive

Inspect-first, search-first, read-authoritative-source, batch calls, and use `read_many` are one
workflow scattered across the context. Consolidate them into a short ordered evidence protocol.

### 18. Platform protocol and repository policy are mixed

Capability activation and approval are platform concerns; Cargo gates and crate layering are
repository concerns. Separate runtime protocol, request contract, repository engineering policy, and
workspace state.

### 19. Too much repository policy is injected for unrelated tasks

Provider, plugin, server, grammar, release, and golden details compete with a simple review request.
Use relevance-based context loading and attach specialized policy only when the task activates it.

### 20. The root policy document serves too many purposes

It is onboarding, workflow, architecture, security policy, testing guide, authoring guide, release
runbook, and language-maintenance guide at once. Keep the root document as a concise mandatory index
and move specialized procedures to focused documents.

### 21. The universal “start here” workflow does not fit every task kind

Git status, story selection, and implementation gates suit code changes but not explanations,
reviews, or tool-free work. Branch the workflow by `review`, `investigation`, `change`, and `release`.

### 22. Autonomous backlog selection can surprise the user

Taking the top ready story whenever no task is named is inappropriate for many informational
requests. Require an explicit request for autonomous backlog execution.

### 23. Story creation is overgeneralized

“New or unscoped work” could include trivial documentation and typo fixes. Define thresholds and
exceptions for behavior changes, small maintenance, and urgent work.

### 24. Failing-first evidence is not operationalized

The policy requires a failing-first test but does not say how to record it or handle a dirty tree
where behavior is partly implemented. Define acceptable evidence and prohibit resetting user changes
to manufacture a baseline.

### 25. Dirty-worktree ownership is unresolved

All changes are assumed user-owned unless made by the agent, but no provenance identifies prior agent
work or unknown hunks. Supply a session change ledger and allow only targeted additive edits to
unknown modified files after diff inspection.

### 26. Git status is truncated

Separate `(+N more)` summaries prevent a reliable complete path count and may overlap. Supply complete
porcelain output or a stable artifact; do not infer totals from independently truncated views.

### 27. Git whitespace is not preserved reliably

Porcelain columns use meaningful spaces, but rendered lists may normalize them. Present raw Git state
in a literal lossless block and identify its originating command.

### 28. “Unstaged changes” may not describe the supplied diff summary precisely

The context does not clearly separate staged, unstaged, and untracked state. Report each category and
its statistics independently.

### 29. Dynamic context has no freshness marker

Branch, HEAD, working tree, and environment can change immediately. Attach `captured_at`, HEAD, and a
snapshot identifier, and state whether mutation requires refresh.

### 30. Repository summary duplicates embedded policy

Stack, architecture, tests, and conventions appear in multiple places. Keep the repository summary
factual and compact; make the policy document the authoritative source.

### 31. Effective precedence is not explicit

The reader must combine platform hierarchy with runtime protocol, repository policy, structured
request fields, and prose. Publish an order: platform safety → orchestration protocol → request-local
constraints → repository policy → defaults.

### 32. Untrusted tool output and authoritative repository policy are not distinguished cleanly

A policy file read through a tool is technically tool output, while arbitrary source files may contain
prompt injection. Designate trusted policy paths explicitly; treat other embedded instructions as
data.

### 33. Re-reading injected policy is ambiguous

The root policy is already supplied but the workflow says to read it before changing anything. Mark
injected files with path, hash, freshness, and `already_read` status.

### 34. Nested policy discovery is not consolidated

A nested language policy is mentioned without a general rule. Define nearest-ancestor policy scope
and require relevant nested files before editing within their subtree.

### 35. Output requirements pull in different directions

A general brevity preference conflicts with comprehensive audit requests. Make style task-sensitive:
short for completion reports, comprehensive for audits, and plain text where the CLI does not render
Markdown.

### 36. Two verbosity controls create meta-level noise

A numeric oververbosity target and repository-specific brevity guidance overlap. Resolve them into one
effective response profile after considering the request.

### 37. Evidence-integrity rules are repeated excessively

“Never invent,” inspect before relying, search then read, verify manifests, and do not assume commands
all express one principle. Consolidate them and cross-reference the single rule.

### 38. “Provider rounds” is undefined implementation jargon

The actionable requirement is simply to batch independent tool calls. Use that wording directly.

### 39. Parallelization instructions do not define one mechanism

Native parallel calls, a parallel wrapper, and `read_many` overlap. Specify the preferred method and
prohibit concurrent mutations to overlapping files.

### 40. Parallel-call failure semantics are absent

Atomicity, ordering, partial success, capture, and approval behavior are unspecified. Document them
and recommend parallel execution primarily for independent reads.

### 41. The four-family capability limit may block normal coding work

Read, write, Git, process, and network can exceed the cap. Define family composition, support release,
or provide a standard workspace-coding bundle.

### 42. Two unrelated operations are both called `bash`

The host coding-agent shell and Flux’s product-level built-in shell operation are distinct. Name them
separately so enabling one cannot be mistaken for enabling the other.

### 43. Host action guards and product runtime guards are conflated

Both use approval and safety-envelope language. Call them the host agent action guard and the Flux
runtime safety envelope.

### 44. The single guarded process path has ambiguous scope

The production requirement through `flux-system` could be misread as governing host Cargo execution.
State explicitly that it applies to code in the product, not the external development runner.

### 45. Filesystem and network rules have the same scope ambiguity

Absolute I/O language should appear under “production code architecture requirements,” with host tool
behavior documented separately.

### 46. Architecture is restated in several sections

Authorization, approval, guarded I/O, layering, and tool dispatch recur. Establish one architecture
baseline, then list task-specific implications.

### 47. The authoritative layer map is referenced but not supplied

Dependency work needs the map in `flux-codegate`. Inject it for relevant tasks or require reading it
as an explicit task-triggered step.

### 48. Historical lessons crowd baseline policy

Counts of prior stories, old release failures, and recurring incidents explain rationale but obscure
actionable rules. Keep imperatives in the root policy and link to history.

### 49. The pin-census rule is too dense

Present it as trigger, required adjacent comment, mutation-sensitive test condition, and verification
command rather than one narrative paragraph.

### 50. Gate ordering around formatting is ambiguous

Formatting occurs after build/test/clippy and can mutate files after validation; “then commit” also
conflicts with no-commit policy. Format first, then verify, and say to include formatting in the diff
without committing unless requested.

### 51. Full-workspace gates lack a fallback policy

The commands may be expensive or environmentally blocked. Define targeted development checks,
affected-crate checks, a preferred full gate, and explicit reporting when the full gate cannot run.

### 52. Formatting is both mutation and verification

Distinguish `cargo fmt` from `cargo fmt --check`, and define when each is permitted.

### 53. Whole-workspace formatting is risky in a dirty tree

It can rewrite unrelated user-owned files. Prefer formatting agent-touched files and global
check-only validation when unrelated modifications exist.

### 54. Sandbox checks are globally prominent despite a narrow trigger

Move no-backend and explicit-backend commands into a trigger matrix for changed process spawn or
serving surfaces.

### 55. Conditional collateral requirements need one matrix

New crates, tools, spawns, syntax, user-visible changes, plugin wire changes, and goldens each trigger
extra work. Consolidate these “if X, also Y” rules.

### 56. Changelog rules are repeated with different scopes

Define a decision table for user-visible behavior, internal architecture, test-only changes,
refactors, and documentation corrections.

### 57. The customer-changelog mirror workflow is incomplete

The context requires regenerating a tracked website mirror but omits its path and command. Supply both
or point to a required runbook.

### 58. Release policy is loaded without a release task

Detailed versioning and plugin-release mechanics should be task-triggered rather than always active.

### 59. The dirty files imply an active feature but no task record is supplied

Broad modifications span many subsystems, while no active story or plan is identified. Inject the
active story, plan, related files, and resumed-session status.

### 60. Modified story files have unknown acceptance state

Story paths appear in Git state but their Goal, Acceptance, and status are absent. Supply relevant
story contents or label the files as background only.

### 61. Recent commits are not tied to the task

A list of commit subjects adds noise unless history, release scope, or regression origin matters.
Normally provide only branch and HEAD.

### 62. Release guidance references a last tag that is not supplied

For release work, include the latest tag, commits since tag, unreleased changelog, and current
versions rather than relying on release commit messages.

### 63. The top-level inventory lacks types

Files, source roots, nested workspaces, generated directories, and ignored outputs appear together.
Classify them in a structured inventory.

### 64. Generated output is given equal prominence

`target/` appears beside authored source roots despite being disposable. Exclude it or label it as
ignored build output.

### 65. Plugin policy is scattered

Authoring, trust, workspace commands, protocol versioning, and release compatibility belong in one
plugin guide loaded for plugin tasks.

### 66. Plugin trust and capability restriction appear contradictory

Explain that manifest restrictions constrain host callbacks but are not a sandbox for malicious local
machine code; plugin binaries must still be trusted.

### 67. The plugin environment claim is too absolute

Environment clearing prevents inheritance of arbitrary host variables, not every possible route to a
secret. State the narrower guarantee.

### 68. Safety invariants mix properties and implementation details

For each invariant, list the security property, required boundary, enforcement test, and design
reference separately.

### 69. Session-shape terminology is unexplained

Define valid provider-history states, tool-use/result pairing, role ordering, and the canonical
validator or tests.

### 70. Caller-identity immutability lacks exact references

Pair the invariant with authoritative source and test locations when that subsystem is active.

### 71. Built-in-tool requirements are one overloaded paragraph

Convert them into an implementation, metadata, registration, group, documentation, and verification
checklist.

### 72. “Public operation” is undefined

Name the metadata field or predicate that causes an operation to enter the public catalog.

### 73. Documentation table schema is buried

Put required row shape and risk-tier consistency next to the two catalog paths, with a minimal example.

### 74. Error policy is not layer-aware

The default result type, binary `anyhow`, and wire-string exception may conflict with low-layer
dependencies. Publish a layer-aware policy and boundary exceptions.

### 75. The `unwrap()` prohibition has unclear breadth

“No unwrap on fallible I/O” can imply unwrap is acceptable elsewhere. State the intended production
panic policy and accepted invariant-proving exceptions.

### 76. Cancellation requirements lack trigger criteria

Define long-running work as model turns, network waits, subprocesses, orchestration loops, and server
requests, or otherwise enumerate the relevant paths.

### 77. Golden-generation history is too detailed for root startup context

Keep the concise invariant at root and move exact variable semantics and historical failure modes to
language-local documentation.

### 78. Expected-failing regeneration can be mistaken for a failed task

Describe two explicit phases: regeneration must return non-zero with `REGENERATED`; a subsequent
normal verification run must return zero.

### 79. External syntax mirrors lack a feasible local workflow

The context mandates updates in other repositories despite workspace confinement. Define local work,
external follow-up, and how owed changes are tracked.

### 80. External grammar greps may be impossible

Require them only when the checkout is available; otherwise require a clearly reported follow-up.

### 81. No-commit policy is repeated

Put irreversible source-control rules in one prominent block and reference it elsewhere.

### 82. “Then commit” directly conflicts with commit prohibition

Replace the formatting comment with “include the formatting result in the proposed changes.”

### 83. Branch and worktree prohibitions are repeated

Consolidate stay-on-branch, no-worktree, and no-history-rewrite rules into one source-control policy.

### 84. Destructive-operation categories are incomplete

Classify always-prohibited Git operations, approval-required deletion, safe agent-created cleanup, and
ordinary targeted edits.

### 85. Disposable output has no cleanup authority

State whether ignored build outputs may be removed without asking and whether that is considered
destructive.

### 86. The one-product-binary rule lacks category boundaries

Explain how examples, fixtures, benchmarks, auxiliary bins, protocol binaries, and dev tools are
classified and identify any enforcing test.

### 87. Pre-1.0 versioning is not labeled as project policy

Clarify that the minor-as-breaking rule is Flux’s compatibility convention rather than a universal
Cargo rule.

### 88. Plugin release terminology lacks a glossary

Define protocol line, independent `1.x`, pack release, affected crates, and how an owed release is
recorded.

### 89. Website integration policy is fragmented

Operation docs, customer changelog mirrors, Prism, site workflows, and contract tests need one guide
covering source, generated output, commands, and checks.

### 90. Environment context is neither minimal nor reproducible

It provides OS and path but omits toolchain and capability versions while embedding a special `bwrap`
fact elsewhere. Supply a normalized environment capability snapshot for verification tasks.

### 91. Command-availability policy is inconsistent

Some shell tools require `command -v`, while Cargo is assumed. Declare environment-manifest tools as
available and require discovery only for undeclared commands.

### 92. Persistent-server instructions are globally loaded

Backgrounding, retries, and port checks should be a conditional runbook used only when starting a
service is the requested deliverable.

### 93. Watch-process and server rules need an explicit distinction

Permit unattended servers only for requested deliverables with lifecycle and cleanup plans; continue
to prohibit generic watchers.

### 94. Defensive-security policy is too terse to guide edge cases

Keep the fuller security-use policy at the platform layer rather than adding a one-line repository
restatement with no taxonomy.

### 95. Proactivity and “do not surprise” have no scope examples

Define allowed follow-through (tests, touched-file formatting, required mirrors), ask-first expansion
(dependency upgrades, broad refactors), and unrelated prohibited cleanup.

### 96. “Smallest change” can conflict with mandatory collateral

Define the smallest complete change as implementation plus every policy-required test, document,
story, and generated artifact.

### 97. End-to-end completion conflicts with approval pauses

Model completion states explicitly: completed, awaiting approval, blocked by decision, and blocked by
environment.

### 98. Blocked-state reporting has no template

Standardize `status`, `changes`, `verification`, `not_run`, `blocker`, and `next_action`.

### 99. One final-answer template does not fit every task

Use separate completion formats for coding, review, investigation, decision, and approval-plan tasks.

### 100. Plain-text guidance can be simpler

Say directly: use plain text, short section labels, numbered lists, and backticks; avoid decorative
Markdown.

### 101. The prior summary did not match the review deliverable

It primarily restated facts instead of identifying ambiguity, conflict, redundancy, and structural
repairs. Treat structured deliverable fields as acceptance criteria and check each before responding.

### 102. The prior summary made a questionable path-count inference

Truncated status and diff-stat views may overlap, so deriving “at least 68 changed paths” risks double
counting. Use “dozens” unless complete status evidence supports a number.

### 103. The prior summary blurred evidence and interpretation

Terms such as “heavily modified” are reasonable conclusions, not raw facts. Label supplied facts,
inferences, and unresolved uncertainties separately.

### 104. The prior summary missed the central mode/tool contradiction

The staged-planner role, autonomous-coder mandate, unavailable workspace operations, and request-local
no-tool mode should have been the first findings.

### 105. Flux-specific terms lack a glossary

Action batch, effect mode, capability family, provider round, permission subject, semantic effect,
pin census, wire seam, protocol line, golden mode, and projection are used without one concise
reference. Add a platform glossary and load subsystem glossaries only when relevant.

### 106. Normative strength is inconsistent

Classify statements as REQUIRED, PROHIBITED, CONDITIONAL, RECOMMENDED, or BACKGROUND rather than mixing
“must,” “never,” “prefer,” history, and description.

### 107. Absolute rules hide exceptions

Present each rule with scope and exceptions directly beside it, especially dispatch, I/O, error type,
binaries, gates, and external grammar requirements.

### 108. Prompt density obscures safety-critical rules

Security invariants should occupy a short, separate high-priority section rather than compete with
release history and documentation mechanics.

### 109. No policy activation mechanism exists

Tag specialized rules with predicates such as `modifies_process_spawn`, `adds_public_tool`,
`changes_flux_syntax`, or `performs_release`, and inject only activated modules.

### 110. Context sections lack stable identifiers

Assign IDs to platform, request, repository, and safety sections so citations can be exact without
quoting whole paragraphs.

### 111. Data and instructions are not machine-distinguished

Annotate every block with `kind`, `trust`, and `freshness`, distinguishing policy, request, workspace
snapshot, source document, and untrusted data.

### 112. Static and dynamic context are mixed

Version static policy by hash and timestamp dynamic workspace state separately.

### 113. The embedded root policy has no provenance

Include content hash, read time, and whether it reflects HEAD, working tree, or a cached snapshot.

### 114. Working directory and repository roots are assumed identical

Provide `cwd`, `workspace_root`, `git_root`, and `cargo_workspace_root` separately.

### 115. Workspace confinement lacks symlink semantics

Define treatment of resolved paths, out-of-root symlinks, nested repositories, and external path
dependencies.

### 116. Host and product network policies are only partially separated

Document request-local network authority, host egress behavior, and Flux product URL guarding in
different sections.

### 117. Redaction behavior lacks a diagnostic workflow

Explain what output may be redacted, prohibit reconstruction, and require reporting when redaction
prevents verification.

### 118. Anthropomorphic test language reduces precision

Replace “reds the gate,” “test dies,” and similar phrases with explicit non-zero, failure, and
validation outcomes.

### 119. Story and incident IDs lack a taxonomy

Explain C-, A-, and L-prefixes or omit historical IDs from mandatory policy where they add no
navigational value.

### 120. Trusted paths are not clearly distinguished from verified paths

Paths named in policy should be known candidates, but existence should still be checked before
mutation. State this two-step rule explicitly.

## Recommended context structure

### 1. Effective request contract

```text
task_kind: review | investigation | change | release
deliverable:
evidence_scope:
tool_policy:
effect_mode:
response_profile:
```

### 2. Authority and precedence

```text
1. platform safety
2. runtime/orchestration protocol
3. request-local constraints
4. repository policy
5. defaults and recommendations
```

### 3. Agent mode and lifecycle

State one active mode and its permitted transitions. For changes, use inspect → propose/capture →
approve → execute → verify → report. For a context review, state that action capture and mutation do
not apply.

### 4. Typed source bundle

Every source should carry:

```text
id:
kind: policy | request | workspace_snapshot | source_document | derived_summary
trust: authoritative | informational | untrusted
captured_at:
content_hash:
```

### 5. Dynamic workspace snapshot

```text
cwd:
workspace_root:
git_root:
cargo_workspace_root:
branch:
head:
staged:
unstaged:
untracked:
baseline_failures:
```

### 6. Active task state

```text
story:
plan:
goal:
acceptance:
completed:
remaining:
blockers:
open_decisions:
change_ownership:
```

### 7. Mandatory baseline policy

Keep only source-control safety, evidence integrity, the product’s core safety boundary, and the
minimum verification expectation in the always-loaded policy.

### 8. Activated policy modules

Load specialized modules only when predicates match the task:

```text
modifies_process_spawn -> sandbox checks
adds_public_tool -> registration, metadata, and catalog checks
changes_flux_syntax -> golden and editor-mirror workflow
modifies_plugin_protocol -> compatibility and pack-release workflow
performs_release -> versioning and changelog runbook
```

### 9. Verification matrix

Distinguish formatting mutations, targeted checks, affected-crate checks, full gates, special posture
checks, and commands not run because of environment or scope.

### 10. Completion state

```text
status: completed | awaiting_approval | blocked_decision | blocked_environment
changes:
verification:
not_run:
blocker:
next_action:
```

## Conclusion

The context should stop behaving as a flat anthology of every rule a Flux maintainer might ever
need. It should be a typed, precedence-aware, task-filtered package. Explicit mode, authority,
tool/capability state, source trust, freshness, and activation predicates would remove most of the
ambiguity while making the genuinely important safety rules easier—not harder—to follow.
