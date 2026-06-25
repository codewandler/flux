# Security policy

flux is a safety-focused agent harness: its central promise is that every tool call passes through a
non-bypassable authorization → approval → guarded-IO chain. A vulnerability that lets the agent (or a
plugin, hook, or sub-agent) escape the workspace, run an unapproved destructive operation, leak a
secret, or otherwise bypass that envelope is treated as a serious security issue.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately via GitHub's **["Report a vulnerability"](https://github.com/codewandler/flux/security/advisories/new)**
(Security → Advisories) so it can be triaged before disclosure.

When reporting, please include:

- the affected version (`flux --version`) or commit,
- a description of the issue and its impact (what envelope guarantee it breaks),
- steps to reproduce, ideally a minimal proof of concept.

We aim to acknowledge reports promptly, work with you on a fix and disclosure timeline, and credit
reporters who wish to be credited.

## Scope

In scope: anything that bypasses the safety envelope (workspace escape, unapproved destructive
operations, secret exfiltration, SSRF past the egress guard, plugin/host-capability escalation,
sub-agent privilege escalation) or that causes remote code execution / denial of service in the
`--serve` daemon or the plugin/hook subsystems.

The subscription provider paths (`claude`, `codex`) reuse undocumented endpoints and are opt-in; issues
specific to those are accepted but lower priority.

## Supported versions

Security fixes target the latest released `0.x` minor. Until a `1.0`, older minors are not maintained —
please upgrade to the latest release.
