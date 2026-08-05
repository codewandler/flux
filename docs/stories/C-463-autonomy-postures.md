---
id: C-463
title: "Name the autonomy postures — `auto_approve: bool` is doing the work of a first-class choice"
pillar: Core
status: done
priority: 3
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-cli, flux-runtime, flux-sdk, docs]
note: "⚠ owner-directed: no per-effect approval is a VALID model, not a degraded one — research, security hardening and long exploration are cases where interrupting the agent per effect is actively the wrong design. flux already ships that posture (C-410: unattended = fail-closed sandbox + auto-approve) and never names it, so it reads as safety switched off"
---

# Autonomy is a posture, not an absence of safety

## Goal

Make the autonomy posture an explicit, named choice — so running an agent without per-effect approval
reads as *"constrain by policy and isolation instead of by prompts"* rather than as *"safety off"*.

## Why this is a correction, not a feature request

Owner-directed, 2026-08-02, correcting my own framing. I had written that Anthropic's Managed Agents
lacking per-effect approval was *"coherent for their product and the opposite of flux's thesis."* That
is too narrow:

> *"it can be a valid model though — depends on the use-case — if you want for example high amount
> autonomy/freedom/exploration (research, security hardening, etc) this is totally fine"*

Correct, and it matters because **flux already ships that posture and does not name it**. C-410 raised
unattended CLI surfaces to a fail-closed `require` sandbox **with** auto-approve — *constrain harder,
prompt never*. That is a deliberate, safe configuration. But the only vocabulary for it is
`auto_approve: bool` and a `--yes` flag, which read as *turning something off*.

## ⚠ The framing that keeps "autonomy" from meaning "unsafe"

The envelope is **authorization → approval → guarded IO**. Of those three, **approval is the only stage
with a human in it.**

- Varying that stage is **choosing a posture**.
- Removing either of the other two is a **bug**.

An autonomous run is not an unguarded run: authorization still decides, guarded IO still executes,
evidence is still recorded. What changes is that the constraint budget moves from *human latency* to
*policy, sandboxing, budgets and destination scope* — all of which flux already has and which get
*more* important, not less, as the prompt goes away.

## The postures, as a starting set

| posture | approval | what constrains it | fits |
|---|---|---|---|
| **supervised** | per effect | a human at a terminal | daily driver, unfamiliar repo |
| **bounded autonomy** | none | policy + fail-closed sandbox + budgets | unattended CLI today (C-410) |
| **exploratory** | none, and interruption is the *harm* | hard isolation + wide-but-bounded grants + full evidence | research, security hardening, long exploration |
| **refusing** | denies everything | — | a served agent with nothing configured |

⚠ Today only the first, the second and an accidental fourth exist, and none is named.

## Acceptance

- [x] **Failing-first**: a test asserting a named posture selects its approver, sandbox posture and
      budget together as one coherent choice — failing at the merge base, where they are set
      independently.
- [x] The postures are **named and selectable**, not assembled from three unrelated flags. ⚠ The bug
      this prevents is the one C-444 describes from the SDK side: `auto_approve(true)` not implying
      confinement. A posture that sets approval without setting isolation is the same mistake with a
      nicer name.
- [x] ⚠ **Nothing in the docs or the CLI presents an autonomous posture as degraded.** No "unsafe
      mode", no warning styling on a legitimate choice. State what each posture relies on instead.
- [x] ⚠ **Each posture states what it does NOT protect against**, because that is the honest version of
      the above. Exploratory autonomy on a valuable repository is a real risk and the docs should say
      which one — not by discouraging it, but by naming the constraint the operator is now leaning on.
- [x] Authorization, guarded IO and evidence are **invariant across every posture**, asserted by a test.
      That assertion is what makes the whole idea safe to ship.
- [x] Existing `--yes` / `auto_approve` keep working and map onto a named posture. ⚠ No flag day.
- [x] Full gate green. *(Wave story: targeted checks only — see Progress. The integrator ran the
      full repository gate once on the combined tree; result recorded in Progress.)*

## Notes

- ⚠ Interacts directly with [C-453](C-453-a-remote-approval-channel.md), in flight: a remote approver is
  the *supervised* posture made reachable over a network, not a new default that other postures deviate
  from. C-453 has been told this.
- Interacts with [C-444](C-444-sdk-secure-defaults.md): the SDK is where posture-as-three-independent-
  flags does the most damage, because an embedder can set one and miss the others.
- ⚠ Do not let this become a permission-preset generator. Four named postures whose contents are
  argued is worth more than an extensible scheme nobody configures correctly.
- The exploratory posture is also the argument for [C-397](C-397-container-process-backend.md) and
  [C-399](C-399-remote-guarded-io-backend.md): if the prompt is gone, isolation is what is left, and
  "run it somewhere disposable" stops being a nicety.

## Progress

- Filed 2026-08-02, owner-directed, correcting the framing in C-453's dispatch.
- **2026-08-05 — implemented.** `flux_runtime::AutonomyPosture` (`crates/flux-runtime/src/posture.rs`)
  is the named choice: one value answers *who approves* ([`ApprovalStance`]), *how confined*
  ([`SandboxFloor`], a tightest-wins floor) and *how much* (`ResourceLimits`). Four postures, no
  extensible scheme — `ALL` is a fixed array and there is no constructor for a fifth.

  **Failing-first** (`crates/flux-runtime/tests/autonomy_posture.rs`, at the merge base):

  ```text
  error[E0432]: unresolved imports `flux_runtime::ApprovalStance`, `flux_runtime::AutonomyPosture`
    --> crates/flux-runtime/tests/autonomy_posture.rs:21:21
     |
  21 |     ApprovalChoice, ApprovalStance, Approver, AutonomyPosture, Executor, PermissionManager, Tool,
     |                     ^^^^^^^^^^^^^^            ^^^^^^^^^^^^^^^ no `AutonomyPosture` in the root
     |                     |
     |                     no `ApprovalStance` in the root
  ```

  That *is* the story's claim: at the merge base approval, confinement and budget were set
  independently, so there was no value to name and the coherence assertion could not be written.

  **The invariance suite is the important one.** For all four postures the same op is refused by
  authorization even where the approver allows everything, the same workspace escape is refused by
  the guarded `System`, and the same `tool_call` evidence is recorded. The posture type exposes
  nothing that selects a substrate, widens a grant set, or touches the evidence log.

  **Surfaces.** CLI `--posture <name>` on `AgentFlags` (`run`/`tui`/`fork`/`record`/`app run`);
  SDK `ClientBuilder::posture(..)` / `FlowClientBuilder::posture(..)`. `--yes` and
  `auto_approve(true)` resolve to `bounded-autonomy` — no flag day, and pinned by tests on both
  sides. C-453's `ServedApprovalPosture` now *maps onto* the vocabulary rather than paralleling it:
  `Unattended` is `bounded-autonomy`, `Remote` is `supervised` with the network as its channel,
  exactly as this story's note asked. Surfaces with no terminal (`flux app run <program>`,
  `flux record`) refuse an explicit `--posture supervised` instead of downgrading it, and default to
  `refusing`, which is what they always installed.

  **Honesty, both halves.** Every posture carries `relies_on()` and `does_not_protect_against()`, and
  a test refuses the words "unsafe", "insecure", "dangerous", "safety off" anywhere in a posture's
  own prose. `bounded-autonomy` names the working tree as its blast radius; `exploratory` names
  exfiltration and says to point it at a disposable checkout; `refusing` names the startup plugin
  spawns and pre-authorised ops that never reach the approval stage at all. Public statement:
  [Safety & approvals](../../website/docs/agent/safety.md) — plus the `--yes` framing corrected in
  `getting-started.md`, `troubleshooting.md`, `usage.md`, `cli.md` and `security/os-sandbox.md`.

  **The one deliberate non-uniformity.** Only an *explicitly named* posture contributes a sandbox
  floor in `apply_sandbox_env`. `--yes` keeps contributing what it always did through
  `unattended_sandbox_surface`, because that classifier's exemptions (notably `flux tui --yes`, where
  an operator is watching the whole run) are decisions about *surfaces*, not postures — inferring a
  floor from the older spelling would confine them for the first time. Recorded at
  `AgentFlags::named_posture` and pinned by
  `a_named_posture_carries_its_confinement_into_the_sandbox_env`.

- 2026-08-05 — closed. The wave's single full repository gate ran green on the combined tree at
  `b075fd09`: `cargo test --workspace` 225 suites / 4473 tests, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo fmt --all -- --check` and `cargo test -p flux-codegate
  --all-targets` 51/51, all exit 0. Because this wave changed sandbox posture, the two conditional
  suites named in AGENTS.md also ran green: `FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test
  --workspace` and `FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend`.
  Shipped to `origin/main` as `b075fd09`.
