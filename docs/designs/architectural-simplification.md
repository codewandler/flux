# Architectural simplification — fewer paths, smaller modules, honest boundaries

**Status:** proposed 2026-07-31. Epic tracker:
[C-337](../stories/C-337-architectural-simplification-epic.md).

## Context

The repository's high-level architecture is not the simplification target. The L0–L6 map is explicit
and enforced (`crates/flux-codegate/src/lib.rs:15-83`), and `ExecutionEnvironment` documents the
single guarded-executor intent (`crates/flux-runtime/src/lib.rs:2405-2412`). The review found
complexity accumulating *inside* those boundaries: parallel construction doors, compatibility APIs
past their stated removal release, subsystem-sized files, and a workspace whose crate count has grown
again since the completed consolidation.

This epic reduces that local complexity while preserving the two invariants that matter:

1. every real effect still goes through authorization → approval → guarded IO; and
2. a crate still depends only on its own layer or lower.

## Evidence

### Execution assembly can still drift by surface

`ExecutionEnvironment::new` takes five mandatory envelope inputs, then optional invariants are added
through chained methods (`crates/flux-runtime/src/lib.rs:2445-2468`). The CLI's shared helper takes
nine arguments and suppresses `clippy::too_many_arguments`
(`crates/flux-cli/src/execution.rs:993-1021`). Direct constructors remain in agent, app, CLI, SDK,
and runtime test paths. Resource limits are especially sensitive because clones must share one
semaphore; rebuilding them silently forks the configured ceiling
(`crates/flux-cli/src/execution.rs:981-990`).

The simplification is a typed assembly object, not another parallel convenience constructor. Mandatory
security inputs remain constructor-required; surface-wide options are named fields or fluent methods;
and production surfaces converge on that one path. Wiring tests must observe the values that are easy
to omit, especially resource limits, workspace context, disabled operations, redaction, and spawner
state.

### Compatibility debt has outlived its own deadline

`AgentExecutorConfig` and `AgentSpec::assemble` are deprecated since 0.24.0 and say they were planned
for removal in 0.26 (`crates/flux-agent/src/lib.rs:121-150`, `:316-342`). Their bridge,
`ExecutionEnvironment::from_context`, remains solely for deprecated assembly shims
(`crates/flux-runtime/src/lib.rs:2471-2497`), and the SDK still re-exports the old config type
(`crates/flux-sdk/src/lib.rs:66-67`). `flux-app` also retains current-directory-based compatibility
assembly explicitly planned for the next minor cleanup (`crates/flux-app/src/app.rs:626-655`).

Role parsing has strict production doors, but the public lenient parser and loader still turn malformed
metadata into inherited/default authority (`crates/flux-agent/src/role.rs:126-142`, `:274-278`). These
are removed together in a planned pre-1.0 minor release, with migration notes and downstream checks,
rather than one shim at a time.

### Several files are subsystem-sized

Measured on 2026-07-31:

| File | Lines |
| --- | ---: |
| `crates/flux-lang/src/runtime.rs` | 9,789 |
| `crates/flux-tui/src/lib.rs` | 8,568 |
| `crates/flux-runtime/src/lib.rs` | 7,444 |
| `crates/flux-plugin/src/host.rs` | 6,470 |
| `crates/flux-flow/src/staged.rs` | 5,313 |
| `crates/flux-flow/src/engine.rs` | 5,020 |
| `crates/flux-orchestrate/src/lib.rs` | 4,513 |
| `crates/flux-codegate/src/lib.rs` | 3,014 |

The remedy is internal modules with stable public re-exports, not more crates. `flux-runtime` leads
because it is both large and safety-central. Candidate modules are `surface`, `spawn`, `turn`,
`workspace`, `tool`, `registry`, `authorization`, `environment`, and `executor`. `flux-codegate`
follows: its layer checker ends at line 83 and syntax scanners begin immediately at line 85, making
`layering`, `syntax/process`, `syntax/io`, `syntax/sandbox_posture`, and workspace-test support natural
boundaries. The other files split incrementally by their existing conceptual seams.

### The crate graph has grown since consolidation

The completed consolidation deliberately merged only same-layer siblings and protected published and
L0 contract boundaries (`docs/designs/crate-consolidation.md:21-26`, `:69-78`). It landed at 31 crates
(`:65-67`); the current workspace lists 37 members (`Cargo.toml:3-42`). Growth is not itself a defect,
but it warrants a fresh ownership/consumer audit under the same rules.

`flux-audio` is the clearest audit prompt: it is an L0 workspace member (`Cargo.toml:40`,
`crates/flux-codegate/src/lib.rs:24-36`), describes itself as an optional leaf
(`crates/flux-audio/src/lib.rs:13-16`), and current workspace manifest/import searches find no consumer.
The audit must choose explicitly: consume it from a voice/room path, justify its independent optional
boundary, or remove/fold it. It must not assume that an unconsumed leaf is dead, nor merge published or
deliberate L0 contracts just to lower a number.

### `AgentSpec` is becoming a public configuration bag

`AgentSpec` currently exposes model, prompt, skills, tools, permissions, model limits, reasoning,
loop policy, tool groups, ambient signals, compaction, workspace, context, and skill disclosure as
public fields (`crates/flux-agent/src/lib.rs:163-215`). Each feature adds another top-level field and
more struct-update construction across downstream crates.

This needs a migration design before code. Candidate groups are `ModelSettings`, `LoopSettings`,
`ToolSettings`, and `ContextSettings`, with construction through the existing `AgentSpec` composition
root. Because this changes a published surface, implementation waits for a deliberate minor release;
the epic must not create a second builder that leaves both shapes alive indefinitely.

### The roadmap is carrying its archive inline

`docs/roadmap.md` is 1,638 lines. The active epic log starts at line 80, but delivered epic narratives
occupy most of the file. Move completed narratives under `docs/archive/roadmap/`, preserve links, and
keep the canonical roadmap focused on current status, active work, and pending decisions. This is an
independent docs-only tail, not a prerequisite for code simplification.

## Workstreams and order

| Order | Workstream | Intended result |
| ---: | --- | --- |
| 1 | Typed execution-environment assembly | One mandatory safety-input path; named optional invariants; wiring tests observe omissions |
| 2 | Compatibility cleanup | Remove expired agent/runtime/app/role shims in one planned minor release |
| 3 | Safety-central module split | Split `flux-runtime` and `flux-codegate` internally with stable re-exports |
| 4 | Remaining large-file splits | Incremental, behavior-preserving modules in lang, TUI, plugin host, flow, and orchestration |
| 5 | Crate ownership audit | Record consumer/publish rationale for all 37 members; act only within existing layer rules |
| 6 | `AgentSpec` migration design | Group settings and close public construction during a breaking window |
| 7 | Roadmap archive | Move delivered history out of the active roadmap without breaking links |

Workstreams 1 and 2 are ordered because the canonical assembly path must be clear before its old doors
are deleted. Workstreams 3 and 4 are mechanical and may proceed file-by-file after tests establish
public-surface parity. The crate audit may run in parallel, but any merge/removal becomes its own story.
The `AgentSpec` change is deliberately last among code changes because it is broad published-API churn,
not an urgent safety fix.

## Story decomposition rule

Before implementation, file one bounded child per independently reviewable outcome. Do not make a
single mega-refactor story spanning several subsystem files. Each child must state:

- whether it is behavioral, public-API, or a mechanical move;
- the failing-first or behavior-lock test that proves the boundary;
- public re-exports and downstream consumers that must remain stable;
- the scoped gate, followed by the full workspace gate before completion; and
- whether the pre-1.0 SemVer rule requires a minor release.

## Non-goals

- Changing the L0–L6 architecture or weakening `flux-codegate`.
- Adding a new crate to make a large file look smaller.
- Combining distinct published contracts merely to reduce workspace-member count.
- Replacing the authorization → approval → guarded-IO envelope.
- Mixing behavioral changes into module-move stories.
- Keeping both old and new `AgentSpec` construction shapes indefinitely.

## Verification

Documentation filing is complete when the tracker, this design, and the roadmap cross-link to one
another; story frontmatter parses under the board convention; all paths and line-count claims are
re-checked against the tree; and `git diff --check` reports no whitespace errors. Implementation
children inherit the standing workspace gate from `AGENTS.md`.
