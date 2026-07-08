---
id: C-45
title: Reconcile `--yes` destructive-op behavior with the safety docs
pillar: Core
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-003 (beta rec #1): public safety docs say destructive ops always re-confirm even with --yes; README/source show --yes installs a headless allow-all approver INCLUDING destructive ops — one of these is wrong; decide the intended contract, then align"
---

# Reconcile `--yes` destructive-op behavior with the safety docs

## Goal
The beta test (F-003) found a direct contradiction on a **safety** claim: the public safety docs
state destructive operations always re-confirm even under `--yes`, while the README, internal docs,
and the source show `--yes` installs a headless **allow-all** approver that also auto-approves
destructive ops. A safety promise that the runtime does not keep is worse than no promise. Decide
the intended contract and make docs + code agree.

## Why (evidence)
- Beta F-003: "Public safety docs say destructive operations always re-confirm even with `--yes`,
  while README/internal docs/source show `--yes` installs a headless allow-all approver, including
  destructive operations."
- Prior docs work already documented the *observed* behavior (troubleshooting: "`--yes` uses
  AllowApprover") — so today the safety page and the troubleshooting page contradict each other.

## Decision to make (part of this story)
Pick one and apply it everywhere:
- **(A) Docs are wrong** — `--yes` really is allow-all incl. destructive. Fix the safety docs to say
  so plainly, with a loud caveat, and keep the code.
- **(B) Behavior is wrong** — destructive ops *should* re-confirm even under `--yes`. Then `--yes`
  becomes "approve non-destructive automatically; still gate destructive," and the allow-all
  behavior moves behind a distinct, explicit flag (e.g. `--yes-i-really-mean-it` / an env opt-in).

Recommendation: lean (A) for CLI ergonomics **but** verify no doc anywhere promises the
re-confirm-always guarantee that (A) breaks; if the guarantee is load-bearing for a consumer
surface, choose (B). Capture the rationale in Progress.

## Acceptance
- [ ] A single documented contract for `--yes` × destructive ops, consistent across: the public
      safety docs, `website/docs/**`, README, `docs/**`, and the CLI `--yes` help text.
- [ ] If (B): a failing-first test that a destructive op under `--yes` is still gated (re-confirm or
      refuse), and the allow-all path requires the new explicit opt-in.
- [ ] If (A): a test asserting the allow-all approver approves a destructive op under `--yes`
      (locks the documented behavior), plus the docs no longer claim the re-confirm guarantee.
- [ ] `grep` sweep: no remaining doc says destructive ops "always re-confirm" / "even with `--yes`"
      unless that is the chosen contract.

## Progress
- 2026-07-08 **Decision: (A) docs were wrong, code is right.** `--yes` installs `AllowApprover`
  (`flux-runtime` `lib.rs:690`) which approves destructive ops too; the destructive gate at
  `lib.rs:1247-1290` still *fires* (the intent is escalated + recorded `KIND_DESTRUCTIVE`) — it is
  answered `Allow`, not bypassed. Verified the "re-confirm even under `--yes`" guarantee lived **only**
  in prose and was already contradicted by `troubleshooting.md`/`usage.md`/README/clap help. Chose (A)
  because: (1) it matches the runtime + the majority of docs; (2) the product already offers the
  "auto-approve routine, gate destructive" behavior via interactive + allow-rules (the gate re-fires
  past an allow-rule) and via `RiskApprover` (D-58) — so `--yes` is deliberately the full-allow
  surface; (3) option (B) would break the documented headless contract (unattended CI would start
  blocking). The **program/app** surface shares the same contract (`flux-app` `app.rs:877`:
  `auto_approve` ⇒ `AllowApprover`, else `DenyApprover`), so programs.md was fixed too.
- Docs aligned to the allow-all contract (preserving the true "gate always re-fires" invariant):
  `website/docs/agent/safety.md` (destructive bullet + unattended-runs note), `getting-started.md`,
  `reference/config.md`, `agent/programs.md`. `troubleshooting.md`/`usage.md`/README/clap help were
  already correct — left as-is.
- Behavior-lock test `allow_approver_auto_approves_a_destructive_op_but_still_escalates_it`
  (`crates/flux-runtime/src/lib.rs`) asserts a destructive op under `AllowApprover` runs **and** is
  still escalated/recorded (allow-all is an approval, not a bypass) — green.
- `grep` sweep: no user doc claims destructive ops "re-confirm even with `--yes`"; remaining
  `re-confirm` mentions are the correct allow-all descriptions.

## Notes
- Beta rec order #1 ("resolve immediately").
- Approver types live behind the CLI; `AllowApprover` is the headless allow-all one. Ground the
  decision by reading the actual approver wiring in `crates/flux-cli` before choosing.
- Epic: [beta-hardening](../designs/beta-hardening.md).
