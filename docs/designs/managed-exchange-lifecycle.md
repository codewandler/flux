# Managed Exchange lifecycle and onboarding capability boundary

**Status:** contract only; implementation queued · **Stories:** C-510 and C-509 · **Authority:**
flux-roadmap Decisions 0004 and 0007 at `e78185f`, with the Exchange provider contract inspected at
`cba95b1157f4f062811cdcc3d309737e97fb4224`

This design records Flux's ownership boundary for a managed local Exchange. It defines how Flux
retains a verified executable and divides work between C-510 and C-509. It does not define Exchange
wire bytes. Exchange `docs/designs/local-release-v1.md`, X-126, X-128, X-129 and X-134 remain the
provider authority for schema names, fields, bounds, framing and conformance verdicts.

No production implementation or provider conformance is claimed here. Both Flux stories remain
`ready` with open acceptance, queued by the roadmap behind X-134 and X-126.

## One publishable protocol inventory

Flux accepts exactly the first-public-release eight-field protocol object:

| Field | Required identity | Provider owner |
|---|---|---|
| `exchange_api` | `exchange.api.v1` | X-129 HTTP v1 fixture |
| `effective_catalogue_response` | `exchange.effective-catalogue-response.v1` | X-129 HTTP v1 fixture |
| `invoke_request` | `exchange.invoke-request.v1` | X-129 HTTP v1 fixture |
| `invoke_response` | `exchange.invoke-response.v1` | X-129 HTTP v1 fixture |
| `connection_plan` | `exchange.connection-plan.v2` | X-134 plan fixture |
| `local_management` | `exchange.local-management.v1` | X-134 FXLM fixture |
| `service_account_handoff` | `exchange.service-account-handoff.v1` | X-134 FXSA fixture |
| `supervisor` | `exchange.supervisor-ready.v2` | X-128 ABI, amended by X-134 inventory |

Release trust alone remains `exchange.release-trust.v1`. The channel, manifest, compatibility and
readiness schemas are respectively `exchange.release-channel.v2`,
`exchange.release-manifest.v2`, `exchange.compatibility.v2` and
`exchange.supervisor-ready.v2`. Channel, manifest, compatibility and readiness must carry the same
exact eight identities.

X-129 proves only the four unchanged Service Account HTTP v1 identities. X-134 supersedes X-125's
unpublished `exchange.connection-plan.v1` submission evidence and proves plan v2, local management,
Service Account handoff and the changed readiness inventory. A package version, local alias or the
old six-field object cannot substitute for any provider fixture.

## Provider fixtures are a future hard input

The only canonical provider corpus Flux may consume is X-126's future, post-X-134
`tests/fixtures/exchange-release-v2/` tree. Flux vendors those bytes with the provider commit and the
complete `fixture-set.json` inventory/digest and runs the same expected outcome for every case. A
missing, additional or changed byte, bound or verdict is a cross-repository contract failure.
X-128, X-129 and X-134 own and prove their respective provider bytes upstream; their standalone
pre-release evidence is not a second Flux input. X-126 aggregates the final candidate identities and
cases into the one consumable inventory after X-134.

The existing `tests/fixtures/exchange-release-v1/` corpus is obsolete six-field implementation
evidence. It and X-125's plan-v1 fixture must be rejected, not copied forward, normalized, partially
consumed or treated as a bootstrap subset. Until X-134 lands and X-126 regenerates the v2 inventory,
Flux has no consumable provider release fixture and C-510/C-509 implementation remains queued.

## Ownership is deliberately asymmetric

| C-510 owns exclusively | C-509 owns exclusively |
|---|---|
| channel/trust verification and rollback floors | helper orchestration through C-510's capability |
| compatible release selection and download/import | owner-authenticated FXLM management |
| bounded archive verification and atomic cache | strict connection-plan-v2 CLI projection |
| quarantine and reinstall | vendor-input helper ceremony without vendor values |
| `VerifiedInstallGuard` and exact process construction | FXSA receiving writer and owner-only token store |
| supervisor, lifecycle control, readiness and liveness | opaque runtime reference and token resolver |
| `start|status|stop`, lifecycle JSON/exits/diagnostics | grant preview/apply CAS, receipts and retries |
| process ownership and health | concrete invocation approval and replay suppression |

C-510 supplies C-509 the already-owned endpoint and typed lifecycle status for consumption, plus one
in-process helper-launch capability. C-509 never adds a lifecycle operation, status field, lifecycle
diagnostic or alternate process-discovery path. Conversely, C-510 never implements an FXLM opcode,
receives an FXSA token, parses a plan or participates in a grant/connection transaction.

## A verified install is a retained capability

An accepted install is identified by the complete authenticated chain, never by a cache pathname,
package version, tag alone or executable name:

- authenticated bootstrap policy and accepted trust identity;
- accepted stable channel and selected entry;
- selected release, target, manifest digest and exact eight-protocol inventory;
- the manifest-declared normalized member inventory; and
- verifier-observed file type, byte count, digest, owner/security and executable-mode state.

The cache record binds that complete chain. A fresh descriptor/handle-anchored walk must reproduce it
on every cache hit. Missing, added, renamed, type-changed, size-changed, digest-changed,
permission-changed or executable-mode-changed content refuses; a directory name conveys no trust.

Successful verification returns a non-path capability, conceptually a `VerifiedInstallGuard`. It
retains the release lock, opened install directory, opened executable and complete installed
identity. Process construction must execute from the retained executable object, or a tested native
equivalent that prevents replacement and proves the OS executed the same verified object. Reopening
a validated path is not equivalent. The guard remains live until creation returns an open child
handle and the child identity is captured; the supervisor retains it with the child for the child's
lifetime.

The process identity extends the installed identity with that open child handle and the
provider-validated process-start/readiness identities. Channel refresh, expiry or selection of a
newer release cannot rewrite a live process's identity. PID, pathname, listener address and package
version never establish ownership.

## C-510 exposes one narrow helper-launch capability

C-510 may lend C-509 only an in-process capability bound to the already-selected
`VerifiedInstallGuard` for the owned instance. It launches only the provider's closed vendor-input
and Service Account mint helper modes of that exact verified Exchange executable.

The capability exposes none of the following:

- a filesystem path, executable handle, cache lookup or alternate binary;
- arbitrary argv, a shell, a generic process builder or a caller-selected helper mode;
- a secret or secret-bearing buffer;
- an FXLM management operation, endpoint override or tenant/credential address; or
- a lifecycle mutation, lifecycle result, status field or diagnostic extension.

C-509 supplies only provider-authorized non-secret helper inputs and any exact native capability the
closed mode requires. C-510 constructs the closed argv and process from the guard. This preserves
C-510's exclusive discovery/verification ownership without moving onboarding semantics into the
lifecycle layer.

## Readiness ABI stays stable while its inventory advances

X-128's capability ABI does not change. Unix uses readiness write FD 3 and liveness read FD 4;
Windows uses the two existing readiness/liveness HANDLE arguments admitted through the closed handle
list. Their directions, one-shot readiness framing, process-start identities and payload-free
liveness behavior remain exactly the provider's X-128 contract.

Only the readiness JSON schema identity and protocol inventory advance from the unpublished
six-field `exchange.supervisor-ready.v1` evidence to `exchange.supervisor-ready.v2` with all eight
fields. Flux commits ownership only after that exact provider record matches the verified install,
open child handle, selected channel/manifest, compatibility output and compiled policy.

Four transports never share a stream or operation space:

- C-510 lifecycle control authenticates only `start|status|stop` ownership operations;
- readiness is the one-shot X-128 record;
- liveness is the payload-free X-128 owner-death capability; and
- X-134 FXLM management plus the one-shot FXSA handoff are C-509 onboarding transports.

In particular, FXLM has no lifecycle opcode; FXSA carries no management or lifecycle record; and
neither can reuse readiness, liveness, child output or C-510's authenticated control channel.

## Vendor values terminate inside Exchange

C-509 strictly projects only plan-v2 targets the provider classifies as non-secret. Flux may send
connector, label, exact plan revision, ordered non-secret targets/settings and other provider-
declared non-secret transaction metadata. The verified Exchange helper/server opens the direct
TTY/browser vendor-input surface and owns the provider transaction and every vendor value.

Flux never reads, proxies, parses, orders, inherits, logs, renders, stores or serializes a vendor
value. There is no vendor-value route through argv, environment, JSON, generic `--field`, stdin,
Flux prompt, lifecycle state or diagnostics. This invariant concerns Flux software/dataflow; it does
not claim isolation from a hostile same-user debugger.

The only Exchange runtime credential that crosses into Flux is the newly minted Service Account
token. It crosses exactly one `exchange.service-account-handoff.v1` FXSA frame from the verified
Exchange helper/server directly into C-509's dedicated receiving writer. The parent CLI and
supervisor never read that pipe. The writer validates one frame plus EOF and reports
`credential_stored` only after atomically committing an opaque token under a new runtime reference
in its owner-only store.

The runtime client holds the reference, not token bytes. A host-owned resolver reads the stored
token only while constructing the sensitive Authorization header and bounds its lifetime. It does
not place the token in the shared redactor, ordinary configuration, logs, events, session state,
model-visible state, argv or environment. Unsafe, unavailable, locked or corrupt storage refuses
without repair or plaintext fallback. Management methods remain structurally absent from this
Service Account client.

## C-509 owns management outcomes and approvals

C-509 speaks the owner-authenticated `exchange.local-management.v1` state machines and consumes
X-134's closed receipts/errors verbatim. Connect replay uses the identical non-secret proposal and
never prompts again after a committed receipt. Grant preview consumes the complete connector-scoped
candidate, revision/ETag and proposal digest; compare-and-swap apply preserves unrelated connectors,
inbound authority and all provider-owned unmodified fields.

A pre-decision error follows only the provider's closed `never|refresh|operator` retry instruction.
An uncertain post-decision result uses `query_receipt` and retries only the same proposal; retry is
never an edit path. Approval is a separate Flux effect boundary: denial sends no Exchange invoke,
and uncertain send state is never automatically replayed for a non-idempotent or conditional write.
A high-risk or effectful invocation requires approval for its exact permission subject even after
the Exchange grant admits it.

## Dependency and evidence boundary

The direct provider dependencies are X-126, X-128 and X-134 for C-510, and X-134 plus C-510 for
C-509. X-129 supplies the already-delivered four HTTP v1 identities; it does not supply plan,
management or handoff identities. X-127 remains an observable transitive publication gate through
X-126.

The order remains `connectors/C-515 -> exchange/X-134 -> exchange/X-126 -> flux/C-510 ->
flux/C-509`. The canonical future X-126 v2 fixture inventory is the gate between provider and
consumer work. This design and the amended stories reconcile the contract only; they do not advance
that dependency order, mark acceptance complete or authorize production implementation.
