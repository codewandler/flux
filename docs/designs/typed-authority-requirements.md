# Typed authority requirements

**Status:** implemented (2026-07-14) · **Story:**
[C-62](../stories/C-62-typed-authority-requirements.md)

## Decision

Planning and dispatch authorize the same `AuthorityRequirement { action, resource }` values for a
concrete operation invocation. Effects remain disclosure and scheduling metadata; they are not
silently translated into a guessed resource. `Tool::authority_requirements` owns the exact contract,
with `authority_requirements_from_declaration` as the conservative adapter for ordinary tools.

The stable vocabulary is:

| Resource | Actions | Invocation identity |
| --- | --- | --- |
| pure transform | none | no requirement |
| workspace path | `workspace.read`, `workspace.write` | normalized permission subject/path |
| datasource | `datasource.read`, `datasource.write`, `flow.write_db` | datasource name or record scope |
| endpoint/host state | `host.read`, `host.write` | endpoint/config/operation identity |
| model provider | `model.invoke` | provider name, or any provider when not known until dispatch |
| HTTP/network | `network.fetch` | resolved URL/declared host, or any network destination |
| connection | `connection.dial` | declared connection target, or any target |
| process | `process.exec` | program/argv subject, or any process |
| browser | `browser.navigate` | network destination selected by the browser operation |
| secret | `secret.read` | declared secret key, or any secret |
| semantic mutation | `operation.mutate` | operation name |
| external send | `flow.send_external` plus `network.fetch` | operation plus destination authority |
| delete | `flow.delete` | operation name |
| money movement | `flow.money` | operation name |
| calendar mutation *(deprecated, C-184)* | `flow.calendar` — default-deny; declare `send_external` instead | operation name |

`write_file` is valid only with a concrete workspace-write requirement. `write_db` is valid only
with a `datasource:` subject and produces both datasource-write and semantic write requirements.
The non-authority semantic tags `pure`, `read`, and `human_visible` add no requirement; `model` and
`network` map to their typed provider/network requirements.

## Derivation and enforcement

1. The tool computes invocation-level permission subjects and typed requirements from the same
   input that will execute. Integration adapters replace manifest-wide placeholders with exact
   declared hosts, programs, connection targets, and datasource subjects.
2. Registry insertion validates the declaration against a representative empty invocation. A
   filesystem/process/browser/host effect without its matching access family, a write with no typed
   write resource, or an unknown semantic action fails registration.
3. Whole-plan risk preview stores these exact requirements on every planned operation. The
   dispatcher recomputes the same contract for the frozen call and evaluates every requirement
   against the mandatory authorization profile before permission rules and approval.
4. Requirements are conjunctive: satisfying one never authorizes another. Approval may narrow or
   confirm an allowed requirement but cannot override an authorization denial.

Generic `Effect::Read` is intentionally pure by itself. It no longer means `workspace.read`, because
the same effect describes in-memory transforms, datasource queries, endpoint reads, and provider
operations. Likewise, `Effect::Write` without a typed destination fails closed instead of acquiring
a wildcard workspace grant.

## Plugin compatibility

Older plugin manifests deserialize without an explicit access list, but the host derives a
conservative contract from the manifest capabilities and operation effects. Declared HTTP hosts,
programs, secret keys, connection targets, and datasources narrow the resource identity; a missing
or inconsistent authority declaration is rejected before the operation is advertised. This changes
authorization semantics, not the framed plugin wire format.

## Proof obligations

- Policy tests independently deny filesystem writes, datasource writes, money movement, deletion,
  and external sends even under auto-approval.
- Plan/dispatch parity tests compare the exact requirement vectors.
- Sink-backed `web.fetch`/`web.crawl` require datasource-write authority in addition to network
  authority; sink-free calls remain network-only.
- Catalog tests validate every built-in and projected plugin declaration, so a new access family or
  semantic tag must extend this document and the shared adapter deliberately.
