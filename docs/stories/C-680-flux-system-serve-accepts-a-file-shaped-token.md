---
id: C-680
title: "flux system serve accepts a file-shaped token"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-server]
design: first-class-hosts
note: "C-480 review: the k8s manifest carries the bearer token as a pod env var only because serve offers nothing but --token-env; file-shaped is the decision 0007/0008 direction"
---

# flux system serve accepts a file-shaped token

## Goal

C-480's Kubernetes profile mounts TLS material as a file-shaped Secret at mode 0400 but has to
deliver the bearer token as a pod environment variable, because `flux system serve` offers only
`--token-env` — no file form exists. The env var never reaches spawned children (`apply_safe_env`
clears it), so the review noted rather than faulted it — but decisions 0007 and 0008 already fix
the direction: secret material arrives as file-shaped references read by owner-authenticated
input, not as environment values. Give serve a `--token-file` (and the client the matching
credential-location scheme if one is missing), make the Kubernetes profile use it, and keep
`--token-env` as the documented fallback for environments without secret mounts.

## Acceptance

- [ ] `flux system serve --token-file <path>` reads the bearer token from a file (mode-checked
      like the existing store-directory discipline), refuses a world-readable file, and never
      logs the value; `--token-env` remains and the two are mutually exclusive.
- [ ] The C-480 Kubernetes deployment mounts the bearer Secret file-shaped and drops the env
      var; the deployment_artifacts test pins the new shape.
- [ ] Rotation is documented: the file is re-read on restart (or live, if the serving loop
      already watches config), and the runbook says which.
- [ ] The VM guest profile's install contract offers the same file-shaped form.
