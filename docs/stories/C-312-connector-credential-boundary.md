---
id: C-312
title: "The credential boundary — prove a vendor credential never enters flux"
pillar: Core
status: ready
priority: 9
epic: connector-platform
areas: [flux-plugin, flux-secret]
note: "the connectors seam's central invariant, asserted rather than assumed: flux holds exactly ONE secret on this path — the deployment session bearer. A response carrying credential-shaped material is refused, not merely redacted"
---

# The credential boundary — prove a vendor credential never enters flux

## Goal

Make "flux never holds a vendor credential" a property the tree enforces, not a claim the design
makes. On the connectors seam the deployment resolves and injects the vendor credential; flux holds
exactly one secret — the deployment's own session bearer — and that asymmetry is the entire safety
argument for the seam.

An invariant nobody tests is an invariant that decays at the next refactor. This story is the test.

## Acceptance

- [x] **Failing-first test**: a platform-sourced operation whose response carries credential-shaped
      material is **refused**, not merely redacted. Redaction hides a leak from the model; refusal
      says the boundary was crossed. State which shapes are recognised and why that set, not a
      different one.
      → `a_platform_sourced_response_carrying_a_vendor_credential_is_refused`
      (`crates/flux-plugin/tests/credential_boundary.rs`), driving the new `platform_plugin`
      fixture. The refusal lives at `credential_boundary::refuse_response`, called from
      `PluginTool::execute` on the raw response. **The recognised set is exactly what
      `flux_secret::Redactor` recognises** — registered values, PEM private-key blocks, URL
      authority passwords, and token shapes (vendor prefix, or an opaque value an assignment name
      declares a secret) — reached through the new `Redactor::credential_shape`, which is
      `redact`'s own verdict rather than a second recogniser. **Why that set:** it is the set flux
      already commits to hiding everywhere else, so a boundary that refused a *different* set would
      either let a shape through to be caught only at display, or refuse something the redactor
      considers ordinary text. One implementation, two views, incapable of drifting.
- [x] Platform-sourced ops carry an empty `secret_purposes` — the deployment resolves credentials, so
      flux must not be asked to. A manifest that declares `secret_purposes` on a platform-sourced op
      is refused at load, with the test naming it.
      → `validate_manifest_operations` (`flux-plugin-protocol`), so the refusal covers the load
      **and** every refresh, which re-runs the same function. Pinned by
      `a_platform_sourced_op_declaring_secret_purposes_is_refused_at_load`. It is not merely
      redundant: a non-empty `secret_purposes` is what gives the projected op `AccessKind::Secret`
      and a `secret.read` authority requirement.
- [x] The activation / auth-initiation path returns **a URL for a human** and never a token. Prove
      the negative: a response containing token material where an authorize URL was expected is
      refused.
      → `an_activation_response_carrying_token_material_is_refused`, with a positive control in the
      same test so the refusals are not a closed path. Three leak shapes: a token beside the URL, an
      implicit-flow `#access_token=`, and — the one no value heuristic can see — an authorization
      `code` on the way out, refused because an authorize URL is the *request* half of an OAuth
      exchange.
- [x] **Responses are treated as out-of-jail input**, because they are: injection-shaped,
      secret-bearing, and authored by whatever the deployment talked to. Redact and escape **at
      ingest**, not at display, and **reuse C-215's machinery** rather than growing a second
      redaction path — C-215 established exactly this posture for harness transcripts, and its own
      review found the ingest bound it asserted was not the bound its code had. Do not repeat that.
      → The check runs the moment `call_with_host` returns, **before** the op's own `redact_fields`
      masking (which a hostile manifest could otherwise declare its way past), before
      stringification, and before the executor's result redaction. C-215's machinery is reused at
      the only layer where it is reachable: `flux-plugin` is L4 and C-215's `contain` is L5, so the
      shared primitives are used directly — `Redactor::redact`'s own pipeline via
      `credential_shape`, and `flux_secret::{names_a_secret, is_opaque_material}` now public so the
      contextual rule reaches a JSON name/value pair it already has instead of one recovered by
      tokenizing around `=`. **Escaping is deliberately not applied here** — a plugin response is
      not written into a `<knowledge-base>` block; it is stringified into a tool result, which the
      provider frames itself. See the module header for the residual gaps, stated rather than
      implied.
- [x] A test asserts no vendor credential appears in the session log, the evidence log, or a tool
      result, for a full activate → refresh → dispatch journey against a fixture.
      → `no_vendor_credential_survives_an_activate_refresh_dispatch_journey`, driving the fixture
      through `Executor::dispatch` in a **hostile** mode at every step. Scope stated in the test:
      the `ToolResult` (both faces) and the shared `EvidenceLog` are asserted directly; the provider
      session log is assembled a layer out in `flux-agent` from exactly these `ToolResult`s, and
      `flux-plugin` (L4) has no dependency on it, so pinning the result is what pins the log entry.
- [x] Full gate green in both workspaces.

## Progress
- Filed 2026-07-31 from the approved connectors-seam plan.
- 2026-08-01 — landed. Decisions made, and what was rejected:
  - **Per-operation `platform: PlatformSourcing` on the wire, not a manifest-wide flag.** A
    connector-platform plugin legitimately serves local ops beside its platform-sourced ones
    (`whoami`, the catalog itself), whose only credential is the deployment session bearer flux does
    hold; a manifest-wide flag would have to lie about one group. Rejected alternatives: encoding it
    in an existing field (a group name, the description) to dodge a wire change — that is exactly
    the kind of thing a security review should attack; and an operator-side declaration in the
    descriptor — the manifest is where every other capability-shaped fact about a plugin lives, and
    a self-declaration here can only ever *add* restrictions, so it cannot escalate.
  - **Refusal keyed on the redactor's verdict, not on a purpose-built matcher.** Rejected: an
    entropy score (catches more, and `flux-secret` already rejected it for the same reason), and a
    hand-written vendor-token regex list beside `SECRET_PREFIXES` (a second list to drift).
  - **A refresh may not shed the declaration** (`op_scope_weakenings`), ranked by
    `PlatformSourcing::strictness` so tightening stays free. Without it the cleanest escape was to
    answer `manifest` once with the declaration to load, once without it to get the credential
    through — the same class as C-310's capability widening.
  - **The failure path is an ingest surface too.** A plugin's `err` frame carrying a vendor 401 body
    would otherwise have gone straight into a tool result; `credential_boundary::scrub_error`
    replaces such a message wholesale rather than redacting it.
  - **`flux plugin call` carries the boundary too**, wired to the same declaration. Its redactor is
    fresh (a one-shot process has no session), so only shape-based material is caught there — the
    weaker half, recorded at the call site rather than papered over.
  - **`codewandler-flux-plugin-protocol` 1.1.1 → 1.2.0**: additive, serde-defaulted, and
    `skip_serializing_if` keeps every existing manifest byte-identical on the wire. A plugin-pack
    release is owed for the host-kit half.

## Notes
- The one secret flux *does* hold on this path is the deployment session bearer, and it is stored like
  any other credential. `flux auth login connectors` already supplies it —
  `crates/flux-cli/src/auth_cmd.rs:112-121` falls through to `login_plugin` for any non-builtin name.
- The confused-deputy question, answered honestly: `../flux-connectors/docs/designs/connectors-proxy.md`
  names it — *"a credential-injecting proxy is, by construction, a confused-deputy machine: its entire
  job is to add authority a caller does not have."* That design was **superseded** by
  `connectors-app.md`, which carves out the narrow defensible case: one operator, their own
  credentials, a process they started, loopback-bound. This story's tests are what keep flux's half
  inside that carve-out.
- Sibling stories: [C-310](C-310-plugin-catalog-refresh.md),
  [C-311](C-311-vendor-host-disclosure-at-approval.md). All three touch
  `crates/flux-plugin/` — run them in separate waves.
