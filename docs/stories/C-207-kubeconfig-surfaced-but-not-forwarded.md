---
id: C-207
title: "KUBECONFIG decides the kubernetes group is surfaced, then is not forwarded to kubectl"
pillar: Core
status: ready
priority: 18
epic:
design:
note: "kubeconfig_reachable() reads KUBECONFIG from the host env to surface the endpoint group, but KUBECONFIG is not in SAFE_ENV — so a user with a non-default kubeconfig gets the ops offered and every one of them fails"
---

# `KUBECONFIG` decides the kubernetes group is surfaced, then is not forwarded to kubectl

## Goal
Two pieces of the same feature disagree about where the kubeconfig comes from:

- **Surfacing** — `crates/flux-runtime/src/lib.rs:1787-1791`, `kubeconfig_reachable()` returns true
  when `KUBECONFIG` is set and non-empty, **or** `~/.kube/config` exists. This is what decides
  whether the `endpoint` discovery group (and with it the kubernetes plugin's ops) is offered.
- **Execution** — every plugin subprocess is spawned through `System::apply_safe_env`
  (`crates/flux-system/src/lib.rs:1863-1889`), which calls `env_clear()` and re-adds a fixed
  `SAFE_ENV` allow-list. `KUBECONFIG` **is not on that list**.

So for a user whose kubeconfig lives anywhere other than `~/.kube/config`, flux reads `KUBECONFIG`,
concludes Kubernetes is reachable, offers ~24 `kubernetes.*` ops to the model — and then every one
of them runs a `kubectl` that falls back to `~/.kube/config` and fails. The failure is also
confusing rather than clear: it surfaces as a kubectl connection or context error, not as
"flux did not pass your kubeconfig through".

Make the two halves agree.

## Acceptance
- [ ] Failing-first: a test that sets `KUBECONFIG` to a non-default path and asserts the value
      reaches the spawned subprocess's environment, failing against the tree as it stands.
- [ ] The surfacing signal and the execution environment use the same source of truth — either
      `KUBECONFIG` is forwarded, or surfacing stops honouring it. See Notes: forwarding is the
      likely answer, but it is a real allow-list decision, not an obvious one.
- [ ] If forwarded, the `:::caution` currently in `website/docs/plugins/kubernetes.md` step 2
      (documenting the workaround: merge or symlink into `~/.kube/config`) is removed, since it
      stops being true.
- [ ] Whatever is decided is recorded in `SAFE_ENV`'s comment block, which already explains why each
      entry is on the list.

## Progress
- 2026-07-29 — found while writing the public kubernetes plugin guide under the
  [website-truth-and-identity](../designs/website-truth-and-identity.md) epic (C-203). Both halves
  verified against the tree at `0.33.1` before filing.

## Notes
- **`SAFE_ENV` is deny-by-default on purpose**, and that posture is right — `env_clear()` plus a
  named allow-list is what stops a plugin subprocess inheriting the host's secrets. Adding
  `KUBECONFIG` is a *path* to a config file, in the same category as `PATH` and `HOME` which are
  already forwarded, so it is a defensible addition rather than a hole. But note the kubeconfig file
  itself contains credentials, so forwarding the path means the subprocess can read them — which is
  exactly what `kubectl` needs, and no more than `~/.kube/config` already gives it today.
- The alternative (stop honouring `KUBECONFIG` when deciding to surface) is worse: it would hide the
  group from users whose setup is fine, to avoid mis-surfacing for users whose setup is unusual.
- Check whether other tools have the same shape before fixing just this one — anything whose
  *availability probe* reads an env var the *executor* then drops has this bug. `kubeconfig_reachable`
  is the one confirmed instance; a quick sweep of the other discovery probes in
  `crates/flux-runtime/src/lib.rs` would say whether this is a class or a one-off.
