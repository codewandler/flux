---
id: C-684
title: "A named host binding trusts a declared private CA"
pillar: "Core"
status: backlog
priority: 1
epic: first-class-hosts
areas: [flux-cli]
design: first-class-hosts
note: "C-677's review found it: --remote takes --remote-ca, a [[host]] binding takes nothing — so every self-signed pod, guest and container is unreachable by name"
---

# A named host binding trusts a declared private CA

## Goal

`flux run --remote <url>` accepts `--remote-ca <PEM>`, but a named `[[host]]` binding accepts no
equivalent: `resolve_named_host` and `CliHostProber` call `connect_remote_system` and
`probe_remote_system`, never the delivered `connect_remote_system_with_ca_pem`. So the named
binding — the entity decision 0018 built to replace the anonymous flag — is strictly *less*
capable than the flag it replaces, and every substrate whose certificate chains to an
operator-managed private CA is unreachable by name: a Kubernetes pod with a cluster-issued cert,
a VM guest from C-480's install contract (which produces exactly such a cert by default), a
container on a developer's own CA. The gap affects `remote`, `kubernetes` and `microvm` bindings
equally; it is not specific to any one kind. Give the binding a CA *reference* in configuration
and route every binding-resolution path through the CA-aware client.

## Acceptance

- [ ] A `[[host]]` entry declares an optional CA certificate location using the existing
      reference vocabulary (a path is a location, not a secret value); an unreadable or malformed
      CA fails closed at resolution naming the binding and the file, never falling back to the
      default trust store silently.
- [ ] `resolve_named_host`, `CliHostProber::probe` and the metrics read all route through the
      CA-aware client when the binding declares one, and the ordinary public-trust path is
      unchanged when it does not.
- [ ] `flux host probe` against a private-CA endpoint succeeds where it previously failed, and a
      certificate that does not chain to the declared CA is refused with the TLS failure named —
      never downgraded, never `--insecure`-style bypassed (no such flag is added).
- [ ] A test proves both faces against a loopback TLS server with a generated private CA, and
      C-480's VM/Kubernetes reference docs point at the new field where they describe their
      generated certificates.
