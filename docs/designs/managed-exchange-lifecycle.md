# Managed Exchange lifecycle and onboarding capability boundary

**Status:** contract only; implementation queued · **Stories:** C-510 and C-509 · **Authority:**
flux-roadmap Decisions 0004 and 0007 at
`ecd327ba8a4036889a91a943e952b3e54857e096`, with the canonical Exchange provider baseline
inspected at `9e84f77c1a6db60b967b0fb887198a14af26cd30`

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

## Plan discovery is native, value-free management

Flux reads the connection plan from the supervised Exchange instance's owner-authenticated native
management endpoint. It sends X-134's FXLM `PLAN_QUERY` request opcode `0x0007` and accepts only the
matching `PLAN_RESPONSE` opcode `0x0008` whose payload is the exact canonical
`exchange.connection-plan.v2` response. The query payload is exactly
`{"connector":Connector,"selection":Label|null}` with a required JSON `null` for no selection. For
the same resolved owner, connector, selection and state snapshot, the native payload and provider's
human-user HTTP body are byte-for-byte the same RFC 8785 UTF-8 bytes; Flux does not translate between
two plan shapes.

The native operating-system owner proof authenticates this read. It is not an HTTP identity, an
Exchange Service Account, a browser session or a capability that Flux may mint or borrow. Flux must
not use the Service Account catalogue/invoke client or browser authorization for plan discovery.
The final query payload, response bytes, bounds and framing remain X-134-owned and must be consumed
verbatim from its committed fixtures rather than redefined here.

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
| channel/trust verification and rollback floors | plan projection, user selection and grant confirmation |
| compatible release selection and download/import | native value-free plan reads and grant management |
| bounded archive verification and atomic cache | strict connection-plan-v2 CLI projection |
| quarantine and reinstall | one non-secret helper request and one value-free terminal result |
| `VerifiedInstallGuard` and exact process construction | FXSA receiving writer and owner-only token store |
| supervisor, lifecycle control, readiness and liveness | opaque runtime reference and token resolver |
| `start|status|stop`, lifecycle JSON/exits/diagnostics | grant preview/apply CAS, receipts and retries |
| process ownership and health | concrete invocation approval and replay suppression |

C-510 supplies C-509 the already-owned endpoint and typed lifecycle status for consumption, plus one
in-process helper-launch capability. C-509 never adds a lifecycle operation, status field, lifecycle
diagnostic or alternate process-discovery path. Conversely, C-510 never implements an FXLM opcode,
receives an FXSA token, parses a plan or participates in a grant/connection transaction.

Flux owns plan projection, user selection and grant proposal/confirmation. For a credential-bearing
connection transaction it owns only one canonical non-secret initiating FXLM frame and the
value-free terminal result; the verified Exchange helper owns the secret-bearing FXLM peer.

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
`VerifiedInstallGuard` for the owned instance. For vendor-secret onboarding it launches only the
provider's fixed helper mode of that exact verified Exchange executable. C-509 supplies one bounded,
canonical non-secret initiating frame to the typed launcher. C-510 creates a one-way request pipe
for exactly one at-most-65,548-byte frame plus EOF and a distinct one-way terminal-result pipe for
exactly one at-most-65,548-byte value-free receipt or error frame plus EOF. The request is only
connect `BEGIN` `0x0001` or credential acquire/rotate `BEGIN` `0x0030`; the result is only connect
`RECEIPT` `0x0006`, credential `RECEIPT` `0x0032` or `ERROR` `0x7fff`.

The capability exposes none of the following:

- a filesystem path, executable handle, cache lookup or alternate binary;
- arbitrary argv, extra argv, a shell, a generic process builder or a caller-selected helper mode;
- a secret or secret-bearing buffer;
- an endpoint, tenant, address, working directory, environment value, raw FD/HANDLE aperture or
  caller-selected FXLM operation; or
- a lifecycle mutation, lifecycle result, status field or diagnostic extension.

C-510 constructs the closed process from the guard and derives every native placement value itself.
The corrected X-134 candidate fixes Unix execution to `flux-exchange local vendor-secret` with no
additional arguments, request-read FD 6, terminal-write FD 7, reserved FXSA FD 5 closed and every
other descriptor at or above 3 closed. On Windows it fixes the same mode to exactly
`flux-exchange local vendor-secret --request-handle <REQUEST> --response-handle <RESPONSE>` in that
order, with canonical nonzero decimal HANDLE values; `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` contains
exactly those two distinct handles and every unused handle is non-inheritable. On both platforms the
environment is cleared, standard streams are the platform null device and the working directory is
the provider root derived from the guard rather than caller input. Neither the caller nor a generic
process API can add argv, an inherited descriptor/HANDLE or another placement value. Direct
TTY/browser access follows the provider's closed helper ABI, not redirected Flux streams.

The exact helper-mode spelling, Unix FD numbers, Windows argument spelling/order, frame bounds and
terminal-result grammar are consumption assertions owned by corrected X-134. Flux must copy the
committed provider names and bytes and fail conformance if they drift; it must not create a second
wire or launch contract while that amendment is still in flight. This preserves C-510's exclusive
discovery/verification ownership without moving onboarding semantics into the lifecycle layer.

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

C-509 strictly projects only plan-v2 targets the provider classifies as non-secret. After the user
selects the projected values, Flux constructs exactly one provider-canonical, non-secret initiating
FXLM frame containing the connector, label, plan revision and selected non-secret settings. The frame
is one complete connection proposal: Flux does not prewrite the non-secret settings in a separate
transaction and does not send any later management frame for that ceremony.

The verified Exchange helper keeps its owner-authenticated native plan-validation operation separate
from the secret-bearing ceremony peer: `PLAN_QUERY` terminates at `PLAN_RESPONSE|ERROR`, and only a
distinct connection carries the initiating `BEGIN` through its terminal result. The helper receives
and parses `NEED_SECRETS`, retains the transaction id and secret
ordinals, opens the direct TTY/browser vendor-input surface, sends `SECRET` and `COMMIT`, and owns the
provider transaction through its terminal state. It returns exactly one value-free receipt or error
to Flux over the distinct bounded result pipe. Intermediate transaction ids and secret ordinals,
provider bytes and every secret-bearing FXLM frame remain helper-private and never enter Flux.

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

C-509 speaks only the value-free `exchange.local-management.v1` plan and grant operations and
consumes X-134's closed receipts/errors verbatim. It does not speak or parse the secret-bearing
connection state machine. Connect retry may relaunch the helper only with the byte-identical
non-secret initiating frame and never prompts again after a committed receipt. Grant preview
consumes the complete connector-scoped candidate, revision/ETag and proposal digest;
compare-and-swap apply preserves unrelated connectors, inbound authority and all provider-owned
unmodified fields.

A pre-decision error follows only the provider's closed `never|refresh|operator` retry instruction.
An uncertain post-decision result uses `query_receipt` and retries only the same proposal; retry is
never an edit path. Approval is a separate Flux effect boundary: denial sends no Exchange invoke,
and uncertain send state is never automatically replayed for a non-idempotent or conditional write.
A high-risk or effectful invocation requires approval for its exact permission subject even after
the Exchange grant admits it.

## Corrected X-134 is an implementation gate

Roadmap Decision 0007 fixes the ownership split, native plan opcodes and closed helper capability,
but delegates every final byte and spelling to X-134. The corrected X-134 story/design candidate
inspected for this reconciliation agrees on the plan opcodes/payload, fixed helper mode, FD 6/FD 7,
closed Windows HANDLE list, 65,548-byte frame cap and terminal opcodes recorded above. No unresolved
content contradiction was observed in that candidate snapshot.

The provider is still freezing the two distinct owner-authenticated connections inside one bounded
pre-ceremony phase so its outer result deadline encloses the provider's 5 + 300 + 30 second budgets.
That connection lifecycle and timing remain helper-internal X-134 state: Flux supplies one initiating
frame and reads one terminal result and neither observes nor shares either peer.

It has not landed in the canonical Exchange baseline, so it is not yet a provider contract Flux may
implement. Before either C-510 or C-509 implementation resumes, one committed X-134
story/design/fixture set must agree on every helper spelling and platform ABI, opcode name/number,
exact query and response byte, bound, receipt and error name. Flux then consumes that single contract
verbatim and amends these assertions if the provider's final bytes differ. No Flux test fixture or
parser may make candidate values canonical on its own.

## Dependency and evidence boundary

The direct provider dependencies are X-126, X-128 and X-134 for C-510, and X-134 plus C-510 for
C-509. X-129 supplies the already-delivered four HTTP v1 identities; it does not supply plan,
management or handoff identities. X-127 remains an observable transitive publication gate through
X-126.

The order remains `connectors/C-515 -> exchange/X-134 -> exchange/X-126 -> flux/C-510 ->
flux/C-509`. The canonical future X-126 v2 fixture inventory is the gate between provider and
consumer work. This design and the amended stories reconcile the contract only; they do not advance
that dependency order, mark acceptance complete or authorize production implementation.
