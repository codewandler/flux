# GitLab plugin hardening (epic)

**Status:** 📋 planned 2026-07-08 (backlog filed; no code yet) · **Pillar:** cross-cutting (Agent /
Core) · **Epic slug:** `gitlab-plugin-hardening`

Tracks a batch of 47 findings (GL-001 … GL-047) from a second external beta pass that focused
specifically on the `gitlab` plugin's operation surface — dry-run vs. runtime validation, hidden
read defaults, index scoping, pagination/truncation semantics, destructive-op safety, and the plugin
security surface. This design is the *why, the evidence, and in what order*; per-finding acceptance
lives in the child stories (D-88 … D-95).

## Why

The `gitlab` plugin has outgrown its connectivity phase — its 64-op surface is broadly useful, but
its **dry-run validator trusts only the generated JSON schema while the runtime enforces more**, so
an operator (or an agent) gets a green `--dry-run` for inputs that fail immediately at runtime, and
several read/index ops silently broaden scope or truncate output without saying so. Closing the
schema↔runtime gap once (D-88) collapses ~14 findings at the source; the rest tighten defaults,
truncation honesty, destructive-op safety, index scoping, and the security surface.

## Context

An external tester (Codex, non-destructive: dry-run checks, read-only calls, source inspection, and
a single reversible create/delete lifecycle) exercised the shipped `gitlab` plugin end-to-end
against a private GitLab instance. No consumer/host specifics are recorded here per the repo's
external-facing rule — only the plugin behavior. The core plugin thesis held: the private-network
guard failed closed by default, host-mediated auth worked, project-scoped reads and search behaved,
and a plugin-native snippet create/delete round-tripped. `cargo test -p gitlab` (44 tests) and the
schema-contract test pass, so the schema behavior below is *contracted*, not an accidental build
break — which is exactly why the dry-run/runtime drift needs an explicit fix rather than a one-line
patch.

The findings cluster into six code themes plus two doc/parity themes. The recurring meta-observation
across every batch: **"the runtime handlers contain useful validation and compatibility behavior,
but the generated schemas and CLI dry-run path cannot express it — the most valuable step is a
shared validation layer used by both dry-run and runtime."** That is D-88, the keystone.

## The findings (embedded — the source pass lived outside this repo)

| ID | Finding | Sev | Story |
|---|---|---|---|
| GL-001 | `project.create` has no `project.delete` counterpart — no plugin-native repo lifecycle | Med | D-91 |
| GL-002 | Live private-GitLab access needs a documented scoped-egress test recipe | Low | D-95 |
| GL-003 | Endpoint-level private-net grants are not honored by direct `flux plugin call` | Low | D-95 |
| GL-004 | Conditional target reqs (`ref` OR `project`+`iid`) invisible to dry-run / required-input docs | Med | D-88 |
| GL-005 | Destructive ops not differentiated from ordinary writes (all "Medium", no confirm field) | Med | D-91 |
| GL-006 | File reads return base64 only — no decoded-text convenience field | Low | D-94 |
| GL-007 | Instance-global blob search fails without advanced/exact code search (undocumented) | Low | D-89 |
| GL-008 | Dry-run accepts unknown input fields without warning | Med | D-88 |
| GL-009 | `per_page` silently ignored — only `limit` is read | Med | D-89 |
| GL-010 | Non-positive `limit`/`max_bytes` expand to default / no-limit instead of zero | Low | D-89 |
| GL-011 | Enum-like values (`state`, `visibility`, `link_type`, `variable_type`) not dry-run validated | Med | D-88 |
| GL-012 | Nested mutation payloads (commit `actions`, snippet `files`, labels) mostly untyped in dry-run | Med | D-88 |
| GL-013 | `file.show max_bytes` truncates the base64 string, not decoded bytes → invalid base64 fragment | Med | D-90 |
| GL-014 | `compare.truncated` can be false while a per-file diff is truncated | Low | D-90 |
| GL-015 | Plain read/list calls contribute datasource records + print stderr noise (hidden side effects) | Low | D-94 |
| GL-016 | `gitlab.test` returns the full ~50-key user profile (email, 2FA, sign-in times) | Med | D-93 |
| GL-017 | `index.build {}` is a broad instance-wide crawl with no dry-run scope estimate | Med | D-92 |
| GL-018 | `project.list` defaults to membership-only despite "token can see" wording | Low | D-89 |
| GL-019 | List ops silently clamp high limits; no page/cursor controls or "capped" signal | Med | D-90 |
| GL-020 | Dry-run does not enforce non-empty arrays for commit/snippet creation | Med | D-88 |
| GL-021 | Update ops have inconsistent empty-update handling (mr guarded, issue/release not) | Med | D-88 |
| GL-022 | Archive `format` unvalidated, interpolated into URL/filename | Med | D-88 |
| GL-023 | `repository.archive` has no size cap or dry-run estimate | Med | D-90 |
| GL-024 | Numeric ids/iids accept zero/non-positive in dry-run | Low | D-88 |
| GL-025 | `mr.discussion.create` preview uses plugin `dry_run` field, not CLI `--dry-run` (two concepts) | Low | D-88 |
| GL-026 | Project-create namespace resolution searches only the first 20 groups | Low | D-92 |
| GL-027 | `mr.diff.lines search` is regex; invalid regex not caught by dry-run | Low | D-88 |
| GL-028 | Runtime aliases (`tag`/`name` → `tag_name`, `name` → `branch`) hidden from schemas, dry-run rejects | Low | D-88 |
| GL-029 | `snippet.delete` has no required id in the dry-run schema (accepts `{}`) | Med | D-88 |
| GL-030 | Blank required strings (`"   "`) pass dry-run but fail runtime preflight | Med | D-88 |
| GL-031 | Secret-like CI/pipeline variable `value` fields echoed by dry-run — no redaction metadata | Med | D-93 |
| GL-032 | `search.blobs` accepts both `project` and `group`, then silently prefers `project` | Low | D-89 |
| GL-033 | `job.list scope` accepts non-string entries that are silently ignored | Low | D-89 |
| GL-034 | `index.build` accepts unknown selectors, succeeds with `indexed:0` (typos look like empty success) | Low | D-89 |
| GL-035 | `max_*_bytes` caps are exceeded by the appended truncation marker | Low | D-90 |
| GL-036 | MR discussion line-anchor requirements (`path`, `new_line`/`old_line`) not dry-run validated | Med | D-88 |
| GL-037 | `changelog.add` commits to the default branch/`CHANGELOG.md` with only project + version | Med | D-91 |
| GL-038 | `mr.list`/`issue.list` default to `opened` only (differs from `index.build`'s `all`) | Low | D-89 |
| GL-039 | `index.build` exposes user/group inputs that are not implemented | Low | D-92 |
| GL-040 | Issue indexing cannot be project-scoped even when `project` is supplied | Med | D-92 |
| GL-041 | Group-scoped blob search accepts `ref` but ignores it | Low | D-89 |
| GL-042 | `mr.changes` applies `max_files` (per_page) before the `file` filter → target file can be missed | Med | D-90 |
| GL-043 | MR diff file resolution hard-coded to the first 200 changed files (diff.lines + discussion.create) | Med | D-90 |
| GL-044 | `mr.changes` has no top-level file-count truncation flag | Low | D-90 |
| GL-045 | `compare` returns all commits with no `max_commits` cap / truncation marker | Med | D-90 |
| GL-046 | Project-create namespace resolution can match an ambiguous group basename (case-insensitive) | Med | D-92 |
| GL-047 | `mr.diff.lines` line lookup targets only `new_line`; cannot address a deleted `old_line` | Low | D-90 |

## Story map (recommended order — keystone first)

1. **D-88 — Shared dry-run/runtime validation layer** (Core, host-kit). *The keystone.* Give
   `host-kit` a preflight hook both `--dry-run` and the runtime call, so schema-passing-but-runtime-
   failing inputs are caught locally: unknown fields (GL-008), enum values (GL-011, GL-022),
   non-empty arrays (GL-020), typed nested payloads (GL-012), positive ids (GL-024), blank strings
   (GL-030), conditional targets `ref OR project+iid` (GL-004, GL-029), regex validity (GL-027),
   empty-update guards (GL-021), MR line-anchor conditionals (GL-036), documented alias surface
   (GL-028), and one clear dry-run concept (GL-025).
2. **D-89 — Honest read defaults; no silent scope-broadening** (Agent, gitlab). Surface/validate
   defaults and honor accepted fields: `per_page` (GL-009), non-positive limits (GL-010), membership
   default (GL-018), `project`+`group` (GL-032), `job.list scope` types (GL-033), unknown index
   selectors (GL-034), `opened` default (GL-038), group blob-search `ref` (GL-041), and the
   advanced-search doc note (GL-007).
3. **D-90 — Pagination & truncation truth** (Agent, gitlab; extends D-38). Byte/file/commit caps and
   honest truncation flags: base64-safe file truncation (GL-013), compare top-level flag (GL-014),
   list capping signal + cursor (GL-019), archive size cap (GL-023), marker-inclusive byte caps
   (GL-035), filter-before-cap (GL-042), the 200-file diff-resolution cap (GL-043), `mr.changes`
   truncation flag (GL-044), `compare` commit cap (GL-045), deleted-line lookup (GL-047).
4. **D-91 — Destructive-op safety, confirm fields & `project.delete`** (Agent, gitlab). Finer
   risk/effect metadata + optional confirm fields for delete ops (GL-005), add `project.delete`
   (GL-001), require a safer explicit target for `changelog.add` writes (GL-037).
5. **D-92 — Index scoping correctness & scope estimate** (Agent, gitlab; extends D-38). Project-scope
   issue indexing (GL-040), a dry-run scope estimate for broad crawls (GL-017), drop or implement
   user/group index inputs (GL-039), and robust namespace resolution (GL-026, GL-046).
6. **D-93 — Plugin security surface** (Core, host-kit + gitlab). Redaction metadata for secret-like
   fields so dry-run/echo/audit mask them (GL-031); trim `gitlab.test` to a minimal identity
   (GL-016).
7. **D-94 — Output ergonomics & pure-read side-effects** (Agent, gitlab). Optional `decoded_content`
   for UTF-8 text files (GL-006); make direct read/list calls' datasource contribution + stderr
   noise opt-in/quiet (GL-015).
8. **D-95 — Direct-call private-net grant parity + QA egress docs** (Core; extends D-20). Wire
   endpoint-level grants into the direct `flux plugin call` path or document the `[private_net.plugins]`-
   only support (GL-003); add a short "testing a private GitLab safely" scoped-egress note (GL-002).

## Out of scope / already handled (do not re-file)

- The **v0.6.0 beta findings F-001 … F-016** are the separate [`beta-hardening`](beta-hardening.md)
  epic — 14/16 fixed; F-002 is a documented weak-model capability floor (A-64); F-008 is fully
  closed (`f59fcc7` + `64dbfc1`).
- **v0.9.0-retest N-002/N-003/N-004** are fixed on `main` (kubernetes `OperationSpec.group`;
  `install --dir` stale-descriptor prune; `read` line-number prefix is a model-facing view only).
- **N-001** (a version tag newer than the latest GitHub Release object) is release-ops, filed
  separately as **C-47** (release-publication reliability), not part of this plugin epic.
- Some pagination/index findings overlap the completed [`D-38`](../stories/D-38-gitlab-parity-ports.md)
  parity ports; D-90/D-92 extend that work rather than duplicate it.
