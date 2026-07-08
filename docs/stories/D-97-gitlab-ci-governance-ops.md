---
id: D-97
title: GitLab CI job-token allowlist, protected tags & deploy tokens
pillar: Agent
status: done
priority:
epic:
design:
note: "15 net-new gitlab ops — job_token scope+allowlist+groups_allowlist, protected_tag, deploy_token; High risk + optional confirm_* guards on destructive/credential ops; parity precedent D-38 (NOT the gitlab-plugin-hardening epic, which hardens existing ops); DONE, UNCOMMITTED"
---

# GitLab CI job-token allowlist, protected tags & deploy tokens

## Goal
Give the `gitlab` plugin the CI-governance surface it was missing: manage a project's CI/CD job-token
scope + allowlist (the piece that lets one project's CI use its `CI_JOB_TOKEN` to clone/access
another), protected tags, and project deploy tokens.

## Acceptance
- [x] Job-token scope: `gitlab.ci.job_token.scope.{show,set}` (PATCH replies 204 → synthesized
      confirmation).
- [x] Job-token allowlist: `gitlab.ci.job_token.allowlist.{list,add,remove}` and the
      `groups_allowlist.{list,add,remove}` counterpart.
- [x] Protected tags: `gitlab.repository.protected_tag.{list,show,protect,unprotect}`
      (`create_access_level` defaults to 40 = maintainer).
- [x] Deploy tokens: `gitlab.deploy_token.{list,create,revoke}`; `create` surfaces the one-time
      `token` (the deliverable) and is High risk.
- [x] Destructive removes/unprotect/revoke and `deploy_token.create` carry `Risk::High`; the
      destructive ops accept an optional `confirm_*` field enforced on mismatch.
- [x] Tests: MockHost lifecycles per group + `destructive_confirm_guard_blocks_mismatch`; a matching
      `schema_contract` entry per op (op count 64 → 79). `cargo test -p gitlab` green (50 tests),
      clippy `-D warnings` + fmt clean.

## Progress
- **Done, uncommitted.** All 15 ops in `plugins/gitlab/src/main.rs` (input structs +
  `manifest_builder` registration + handlers reusing `gl_get/post/put/delete` + `enc`/`req_project`/
  `body_from`/`flex_i64`), plus the two confirm-guard helpers. Tests + contract entries added; the
  `manifest_declares_ops` count assertion bumped to 79. CHANGELOG `[Unreleased]`.

## Notes
- Net-new API surface, not a hardening fix — the parity precedent is
  [D-38](D-38-gitlab-parity-ports.md); this is deliberately **not** filed under the
  `gitlab-plugin-hardening` epic (that epic tightens the *existing* op surface).
- Cross-refs: D-91 (destructive-op confirm-field convention — keep consistent if it lands
  plugin-wide) and D-93 (secret-field redaction — the deploy-token output `token` is sensitive; not
  marked `host_only` because the operator must call it directly).
- No new manifest capabilities: reuses `gitlab.endpoint` + the `personal_token` secret.
- Public op docs live in the (concurrently-authored) `website/docs/plugins/gitlab.md`; adding the new
  ops there is left to that in-flight work to avoid a collision.
