---
id: C-685
title: "A Kubernetes profile for the agent surface"
pillar: "Core"
status: done
epic: remote-agents
areas: [release, flux-server]
design: docs/designs/operating-a-deployed-host.md
note: "C-480 shipped deployment profiles for `flux system serve` (the substrate); the agent surface `flux app run --serve` has none, and its non-loopback bind has auth requirements the manifests must encode"
---

# A Kubernetes profile for the agent surface

## Goal

C-480 made the *substrate* deployable: an OCI image, a Kustomize base and a VM guest profile, all
for `flux system serve`. The *agent* surface — `flux app run --serve`, which exposes an agent over
HTTP/A2A with an agent card, `message/send`, `message/stream` and the sessions API — has no
profile at all, so running an agent inside a cluster is back to a BYO recipe. It also has stricter
requirements than the substrate daemon: a non-loopback agent listener must be authenticated (an
unauthenticated one is a release boundary), the approval posture must be chosen explicitly
(`--yes` or `--remote-approval`), and a program with channels may need inbound webhook routing.
Ship the profile that encodes all of that, reusing C-480's image rather than building a second.

## Acceptance

- [x] The same released OCI image runs the agent surface (entrypoint or command override only —
      no second image), with a Kustomize base declaring the bearer Secret, the listener Service,
      TCP probes, non-root/seccomp/read-only-rootfs and a default-deny NetworkPolicy with an
      explicit ingress allowance for the operator path.
- [x] The manifests refuse to express an unauthenticated non-loopback listener: the token is
      required, and a test pins that no manifest binds a public address without it.
- [x] The approval posture is explicit in the manifest with both options documented: `--yes` for
      policy-constrained autonomy, `--remote-approval` for a human in the loop — the latter noted
      as loopback/shared-token only until C-687 lands.
- [x] Session durability is declared: the store directory is a volume, and the runbook states what
      survives a restart and what does not.
- [x] Reaching the deployed agent is documented end to end from a workstation — `flux a2a <url>`
      with the bearer token — including how the channel endpoints of a program are exposed (or
      deliberately not) alongside the agent endpoint.


## Comments

- Review minors carried at integration: the --yes rationale said the sandbox floor constrains this agent in a profile that passes --no-sandbox (corrected in all three files). Still open: egress exclusions assume RFC1918 so a 100.64/10 or IPv6-ULA cluster keeps in-cluster 443 reachability; 'optional: true' is matched literally so 'optional: yes' would evade the lint; daemon_argv reads only the first container's command/args.
