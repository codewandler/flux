# Design: The secret the agent never sees — flux's redaction against Anthropic's substitution

**Status:** proposed · **Pillar:** Core · **Stories:** [C-458](../stories/C-458-substitute-at-egress.md) · [C-459](../stories/C-459-scope-a-secret.md) · [C-460](../stories/C-460-rotation-revocation-audit.md) · [C-461](../stories/C-461-what-our-secret-model-is.md)

## Why

Anthropic's [Vaults](https://platform.claude.com/docs/en/managed-agents/vaults) solve the same problem
flux solves, with a **different strategy**, and the difference explains a class of bug flux keeps
finding.

### Two strategies, named

| | **containment** (flux's local path) | **prevention** (Vaults) |
|---|---|---|
| the secret is | resolved to plaintext in process | never in the agent's reach |
| what the agent holds | the real value | an **opaque placeholder** |
| what keeps it out of output | the `Redactor`, scrubbing after the fact | there is nothing to scrub |
| failure mode | a missed scrub leaks | a missed substitution **fails to authenticate** — loud, not silent |

Vaults' `environment_variable` credential is *"stored in the sandbox as an opaque placeholder. When the
agent initiates an outbound request, the opaque placeholder is substituted with the real secret **at
egress**. The agent never sees the secret value."*

⚠ **That failure-mode row is the whole argument.** Containment fails *silently and in the wrong
direction*: a scrub that misses publishes a credential. Prevention fails *loudly and safely*: a
substitution that misses sends a placeholder, the third party rejects it, and you get an auth error.
flux's three known secret bugs are all containment failures —
[C-339](../stories/C-339-redaction-falls-back-to-the-unredacted-value.md) (redaction fails **open**),
[C-432](../stories/C-432-browser-credentials-never-come-from-the-prompt.md) (the `Redactor` cannot redact
what it was never told), [D-234](../stories/D-234-mediasettings-debug-prints-argv.md) (a `Debug` impl
prints a resolved secret). **None of them is expressible in a substitution model.**

### ⚠ flux already built the stronger model — in one place

This is the finding that reframes the epic. On the **connector** path flux does exactly what Vaults
does, and asserts it as an invariant: [C-312](../stories/C-312-connector-credential-boundary.md) —
*"flux holds exactly ONE secret on this path — the deployment session bearer. A response carrying
credential-shaped material is refused, not merely redacted."* The vendor credential never enters the
flux process at all.

On the **local** path it does the opposite. `crates/flux-app/src/secrets.rs:35-41` — `resolve_in` reads
`std::env::var(&name)` and substitutes the **plaintext into the config value**, then seeds the
`Redactor`. From that moment the secret is in process memory, in the resolved config, and in whatever
formats it.

**So flux has both strategies and applies the strong one only to connectors.** That is the gap, stated
without needing anything from Anthropic.

### What Vaults has that flux has nothing equivalent to

- ⚠ **A secret scoped to where it may go.** `networking.allowed_hosts` — *"controls which outbound hosts
  the secret can be substituted for… prevents your key from ever being shared with unauthorized hosts."*
  flux guards egress **per caller**, never per secret: once `resolve_in` has run, a flux secret can
  travel anywhere the egress guard already permits. *(C-459 closed this on the `http.request` path; the
  `resolve_in` sentence still stands for program-declared secrets.)*
- ⚠ **A secret scoped to where in the request it lands.** `injection_location` — header, body, or both,
  with the reasoning: *"Request payloads are often assembled from content the agent is working with, so
  the request body is the broader exposure surface."* *(C-459: flux has no body-injection path at all,
  so it scopes header vs query instead — see below.)*
- **Rotation and revocation reaching a running session**: *"credentials are re-resolved periodically…
  rotation, archival, or deletion propagates to running sessions without a restart."*
- **Archive versus delete**: *"Secrets are purged; records are retained for auditing."*
- **Per-end-user scoping**: a vault is *"the collection of credentials associated with an end user"*,
  referenced per session — so a credential is bound to a principal.

### ⚠ And what Vaults honestly does not do — which flux should not overclaim either

Their docs state these; a flux comparison that omits them would be dishonest:

- **Workspace-scoped**: *"anyone with an API key for the same workspace can reference them."*
- **Outbound only**: *"If a client uses the stored secret to fetch a session token… the returned token
  arrives in the sandbox **unredacted**."*
- **It breaks some clients**: substitution happens at egress, so anything validating the credential's
  format at startup, or computing a signature from it (AWS SigV4), gets a placeholder and fails.
- **Not available on self-hosted sandboxes** — which matters directly for
  [C-457](../stories/C-457-flux-as-an-environment-worker.md): if flux were an environment worker, env-var
  credentials would not be there, and flux's own secret handling would have to serve.
- *"The agent can do anything the key allows"* — scoping the key itself remains the operator's job.

## Approach

⚠ **Substitution is strictly better where it applies, and where it applies is narrower in flux than in
Managed Agents.** flux's `secret "NAME"` feeds XMPP MUC passwords, SIP registrar credentials and plugin
argv — not only HTTP request headers. Substituting at HTTP egress covers part of flux's secret usage and
not all of it. So:

- **adopt prevention where the shape fits** (C-458),
- **keep redaction as the backstop everywhere** — it stops being the only defence, which is the point,
- **and take the scoping wins regardless** (C-459), because they apply to every secret whatever the
  transport.

### C-458 — substitute at egress

A placeholder in the agent's reach; the real value spliced in at the guarded send.

⚠ **The feasibility crux, verified**: flux has one egress **guard** but not one egress **sender**.
`crates/flux-web/src/egress.rs` is the model-facing web family's single guarded, address-pinned sender —
the natural splice point — but `flux-providers`, `flux-a2a`, `flux-channels`' JaaS tokens and
`flux-auth` each build their own `reqwest` client. Model-facing egress is the only part that needs this,
which makes the scope tractable; it is still a prerequisite, not a detail.

### C-459 — scope a secret · **shipped**

Two axes flux had nothing for: **which destinations** a secret may be sent to, and **which principal**
may cause it to be used. The second was newly available — C-408 and C-415 established per-speaker
`TurnIdentity`, so a secret bound to a principal became expressible where it was not before.

`flux_system::secret_scope` is the mechanism, deliberately co-located with the egress guard rather than
with `flux-secret`, because the security content of a destination scope is *which address was vetted*,
not *which string was typed*. `guard_url_scoped_for_secret` alone mints one private-field result
carrying the guard's URL, exact pins, and `Destination` token; an empty pin set produces no token, so
there is no public constructor that can assert an arbitrary pair was vetted — the bypass that would
otherwise make the whole feature decorative. A grant is written as
`NAME;to=<host>;by=<principal>;in=header|query`; every declared axis is default-deny, a bare `NAME`
stays valid and unscoped, and a malformed entry refuses rather than degrading into an absent one.

⚠ **The enforcement point is `http.request`, and that is narrower than the goal.** It is flux's only
model-facing path where a named secret meets a guarded, address-pinned send. `flux-app`'s `resolve_in`
and `flux-channels`' adapter resolution still substitute plaintext with no destination in view, so a
`to=` scope there would have nothing to match. Widening the reach is not a matter of applying this
mechanism harder — it needs C-458's egress seam to exist first, which reverses the epic's original
"C-459 has the broader reach" reading: the *mechanism* does, the *coverage* does not, yet.

**Injection location, decided.** Vaults' header/body split does not transfer, because flux resolves a
`{"$secret": …}` marker only in `headers` and in the `query` record — the body has no marker resolution,
so their broader exposure surface does not exist here. The axis that does exist is header versus query,
for the same underlying reason: a query-placed credential lands in a URL, which proxies, gateways and
access logs keep. That is what `in=` scopes.

**`Sensitivity` is not the carrier.** It is an ordinal level; a scope is a set of permitted destinations
and principals, and "may go to `api.github.com`" has no rank. It is also still read by nothing, and it
was left in place rather than removed because `codewandler-flux-secret` is a published protocol-line
crate — retiring a public type there is a version decision C-461 should make, not a side effect of this
story.

### C-460 — rotation, revocation, and an audit record

Today a rotated secret means a restart, and nothing records which secret was used where.

### C-461 — say what flux's model is

⚠ The comparison itself, written down — including that flux's connector path already prevents rather
than contains, which is a stronger story than flux currently tells.

## Alternatives considered

- **Adopt substitution everywhere and drop the `Redactor`.** Rejected: flux's secrets reach non-HTTP
  transports where there is no egress splice point, and redaction is the only defence there. Prevention
  is an addition, not a replacement.
- **Do nothing; the `Redactor` is fine.** Rejected on evidence: three shipped containment bugs, one of
  which failed *open*.
- **Copy the Vaults API surface.** Rejected: vaults are a hosted multi-tenant product with a workspace
  trust boundary flux does not have. Take the *ideas* — placeholder, destination scoping, injection
  location — not the resource model.

## Risks & open questions

- ⚠ **A partial substitution model is a false sense of safety.** If some secrets are placeholders and
  some are plaintext, an operator cannot tell which. Whatever ships must make the distinction visible.
- ⚠ **Breaking clients.** Anthropic documents SigV4 and format-validating clients failing. flux would
  inherit that, and the failure is confusing unless anticipated.
- **Open:** whether the placeholder is per-secret-per-session (unforgeable) or a stable token. A stable
  placeholder that leaks tells an attacker exactly what string to send.
- **Open:** does substitution belong in `flux-system`'s guarded send, or one layer up in the web family?
  The former reaches more callers; the latter is where the pinned client already lives.

## Acceptance / done

- flux states its secret model plainly, including that the connector path already prevents and the local
  path contains.
- At least one class of secret is never materialized in the agent's reach.
- A secret can be scoped to destinations and to a principal.
- A rotated or revoked secret reaches a running session, and there is a record of which secret was used
  where.
