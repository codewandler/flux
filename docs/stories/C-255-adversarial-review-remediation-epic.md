---
id: C-255
title: "Adversarial review remediation — close every actionable finding from the three 2026-07-30 reviews (epic)"
pillar: Core
status: in-progress
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
note: "REVIEW EPIC — three independent passes rated 5.5/10, 6/10, and 7/10; all reject Flux as a standalone unattended boundary"
---

# Adversarial review remediation — close every actionable finding from the three 2026-07-30 reviews

## Goal

Close every actionable finding from the three independent reviews of `cb3bb057`, preserve a
one-to-one evidence trail, and make the next review verify durable gates instead of rediscovering
adapter-level exceptions to the safety envelope.

## Acceptance

- [x] C-256 and C-257 bind every reviewed fleet/plugin HTTP, OAuth, and TCP connection to the DNS
      answers admitted by the shared egress guard, including redirects.
- [x] C-258 makes `eval_run` host-select its executable and prevents provider credentials from being
      copied into a model-selected sandbox exemption.
- [x] C-259 content-authenticates release bootstrap tools and gives core artifacts a consumer-
      verifiable signature or provenance.
- [x] C-260 and C-261 bound REST SSE lifecycle and authenticated daemon resource use.
- [x] C-262 establishes a fail-closed unattended sandbox profile without claiming unsupported
      platforms are confined.
- [x] C-263 and C-264 strengthen structural and adversarial assurance over model-facing I/O.
- [x] C-265 makes the built-in strict-review protocol immutable, toolless, and subject to the
      fail-closed unattended sandbox profile even inside an untrusted checkout.
- [x] Existing C-218, C-226, C-233, and C-234 are complete; no duplicate replacement story is used
      to hide their original acceptance criteria.
- [x] Engineering and customer changelogs describe every shipped user-visible change.
- [ ] Three fresh independent reviews against the resulting exact working tree find no reproducible
      High-severity containment defect in the remediated paths.

## Progress

- 2026-07-30 — opened from the three dated artifacts under `docs/reviews/`. Finding-to-story
  traceability and ordering live in the design document.
- 2026-07-30 — all originally mapped child findings are implemented. The root workspace build, tests,
  all-target Clippy, format check, layering/direct-I/O gate, release-policy/action-pin checks, and
  adversarial smoke/preflight suite pass; the nested plugin workspace tests and format check pass.
  The first follow-up review found and drove closure of an eval-selector bypass, insufficient
  release-attestation binding, and overlapping-session accounting error.
- 2026-07-30 — the first closure pass found twelve reproducible High/Medium defects across guarded
  proxy use, Unix-socket grants, timeout finalization, usage accounting, Git text conversion,
  catalog/direct-I/O gates, strict-review authority, release verification, and assurance policy.
  C-265 owns the new containment defect; the other findings reopened their existing stories rather
  than hiding them in duplicates. All twelve are now fixed with regressions. The complete root and
  nested-plugin gates plus release, direct-I/O, adversarial, and action-pin policy checks are green.
  Three final rotated-scope reviews are in progress against that exact integrated tree.

## Notes

- Review ratings are evidence snapshots, not acceptance criteria; closure depends on reproducible
  behavior and gates.
- Bus factor and commissioning an external audit are intentionally recorded as residual governance
  risks, not fictional code stories.
