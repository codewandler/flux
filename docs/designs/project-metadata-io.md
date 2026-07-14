# Project metadata IO boundary

**Status:** implemented (2026-07-14) · **Stories:**
[C-61](../stories/C-61-confine-project-metadata-io.md),
[A-85](../stories/A-85-fail-closed-role-metadata.md)

## Decision

Repository-derived metadata is untrusted workspace input. Automatic context, project config,
project skills, and project roles must resolve beneath the exact repository root and reject a
symlink or path traversal that escapes it. Tool workspace widening such as `--add-dir` or
`--allow-all-paths` does not widen automatic prompt metadata.

User-global configuration, credentials, roles, and skills are a separate trusted control plane:
they are selected by the operator from well-known home-directory roots or an explicit absolute
path. Those roots may use ordinary host filesystem IO, but repository-derived configuration cannot
turn a relative project path into such a trusted root.

## Boundaries

| Source | Trust and API | Required behavior |
| --- | --- | --- |
| `AGENTS.md`, `CLAUDE.md`, `.flux/context.md` | project; `ProjectFiles` over a repository-only `Workspace` | missing is harmless; unreadable or escaping paths are errors and never enter the prompt |
| `.flux/config.toml` | project; `flux_runtime::metadata` over a repository-only `System` | reads refuse escapes; pure parsing preserves unrelated settings; `System::write_file_atomic` writes a create-new sibling, syncs, identity-checks, then atomically renames |
| `.flux/skills`, `.claude/skills`, relative configured skill roots | project; guarded one-level discovery in `flux_runtime::metadata` | every directory and file is independently resolved; an escape is an error; guarded bytes are injected into the pure skill parser and cannot be retargeted during later activation |
| `.flux/agents/*.md` | project; `RoleRegistry::try_load_project(System, dir)` | escaping/unreadable files and malformed metadata are path-aware errors |
| `~/.flux/config.toml`, credential stores | trusted user control plane | explicit host-owned load; never inferred from a repository-relative path |
| `~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills` and explicit absolute extras | trusted user control plane | the operator-selected directory is an explicit `System` root; links beneath it remain part of that trusted boundary |
| user-global role directories | trusted user control plane; `RoleRegistry::try_load` | strict parse/read errors; first configured directory wins across roots |
| in-memory config/skills/roles | programmatic control plane | pure parsing/registration, no filesystem discovery |

Parsing remains in the L0 owners (`flux-config`, `flux-skill`, `flux-agent`/`flux-markdown`). The L2
`flux_runtime::metadata` assembler and guarded system own project path identity and IO. The config
crate accepts injected user/project documents and returns merged or serialized values; the skill
crate accepts injected markdown. APIs make provenance explicit rather than accepting one ambiguous
path list that could mix repository and operator authority. In particular, an absolute skill path
from project config remains project-controlled and is rejected if it leaves the workspace; only a
user-global config entry or explicit absolute CLI argument may select a trusted external skill root.

Malformed role frontmatter is never replaced with default metadata. In particular, an invalid
`tools` value cannot become `None`, because `None` means the intentionally omitted field that
inherits the parent tool ceiling. Strict discovery attaches the source path to parse and read
errors; a valid omission continues to inherit, while `tools: []` explicitly grants no tools.

## Structural enforcement

`flux-codegate` resolves the real package graph and scans both Cargo workspaces for new raw
repository-path reads. There is no file-wide L0 owner exemption: production project reads must go
through the guarded seam, with only narrow function-level exceptions for explicit user input.
Symlink fixtures cover every automatic project source and the config write destination, including
retarget attempts; precedence and atomic-merge tests preserve the existing user experience.

The resolver is platform-neutral. Unix CI creates file and parent-directory symlinks for the
adversarial fixtures. On Windows the same component walk and canonical-ancestor checks apply to
symlinks and junction/reparse-point targets; creating those fixtures can require privileges, so the
portable missing/path-traversal tests run everywhere and the privileged link matrix is explicitly
Unix-gated.
