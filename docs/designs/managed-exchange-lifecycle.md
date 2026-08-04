# Managed Exchange lifecycle and onboarding capability boundary

**Status:** contract only; implementation queued · **Stories:** C-510 and C-509 · **Authority:**
flux-roadmap Decisions 0004 and 0007 at
`4511f44b4defcb6de92ab8fc1b56bd5b4356ca78`; canonical Exchange merge
`3b16bcb5b1c52984449118775125fe66da1686da` contains accepted X-134 contract head
`9dc414c76f231bd179358fd526019a16872a7be1`

This design records Flux's ownership boundary for a managed local Exchange. It defines how Flux
retains a verified executable and divides work between C-510 and C-509. It does not define Exchange
wire bytes. Exchange `docs/designs/local-release-v1.md`, X-126, X-128, X-129 and X-134 remain the
provider authority for schema names, fields, bounds, framing and conformance verdicts.

No production implementation or provider conformance is claimed here. Both Flux stories remain
`ready` with open acceptance, queued by the roadmap behind the connectors C-515 registry release,
X-134 implementation and X-126's post-X-134 release-v2 fixture inventory.

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

X-129 proves only the four unchanged Service Account HTTP v1 identities. The accepted X-134 contract
supersedes X-125's unpublished `exchange.connection-plan.v1` submission evidence; X-134
implementation must prove plan v2, local management, Service Account handoff and the changed
readiness inventory. A package version, local alias or the old six-field object cannot substitute
for any provider fixture.

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
Create and held-label create replay send `selection:null`; credential acquire/rotate send
`selection:BEGIN.label`. In the response every secret field has `set:null`, and aggregate plan state
ignores secret presence. `credential_revision` is `null` exactly when `selection:null` and is an
opaque nonzero 256-bit value encoded as exactly 64 lowercase hexadecimal characters for every
selected label, whether credentials are present or absent; the complete all-zero value refuses.
Credential acquire/rotate copies that exact revision into `BEGIN`. Flux validates and projects it
only as a value-free compare-and-swap token; it never interprets it as presence, count, generation,
time or a digest. The plan and target static revisions remain distinct from this credential CAS.

The final query payload, response bytes, bounds, target-selection/partition rules and framing remain
X-134-owned and must be consumed verbatim from its committed fixtures rather than redefined here.

## Provider fixtures are a future hard input

The only canonical provider corpus Flux may consume is X-126's future, post-X-134
`tests/fixtures/exchange-release-v2/` tree. Flux vendors those bytes with the provider commit and the
complete `fixture-set.json` inventory/digest and runs the same expected outcome for every case. A
missing, additional or changed byte, bound or verdict is a cross-repository contract failure.
X-128 and X-129 prove their respective existing provider bytes upstream; X-134 owns the accepted
contract and must prove its implemented bytes there. Their standalone pre-release evidence is not a
second Flux input. X-126 will aggregate the accepted X-134 contract identities and implemented cases
into the one consumable inventory after X-134 implementation.

The existing `tests/fixtures/exchange-release-v1/` corpus is obsolete six-field implementation
evidence. It and X-125's plan-v1 fixture must be rejected, not copied forward, normalized, partially
consumed or treated as a bootstrap subset. Until X-134 is implemented and X-126 regenerates the v2
inventory, Flux has no consumable provider release fixture and C-510/C-509 implementation remains
queued.

## Ownership is deliberately asymmetric

| C-510 owns exclusively | C-509 owns exclusively |
|---|---|
| channel/trust verification and rollback floors | plan projection, user selection and grant confirmation |
| compatible release selection and download/import | native value-free plan reads and grant management |
| bounded archive verification and atomic cache | strict connection-plan-v2 CLI projection |
| quarantine and reinstall | one non-secret helper request and one value-free terminal result |
| `VerifiedInstallGuard` and two typed guard-bound launches | FXSA receiving writer and owner-only token store |
| supervisor, lifecycle control, readiness and liveness | opaque runtime reference and token resolver |
| `start|status|stop`, lifecycle JSON/exits/diagnostics | grant preview/apply CAS, receipts and retries |
| process ownership and health | concrete invocation approval and replay suppression |

C-510 supplies C-509 the already-owned endpoint and typed lifecycle status for consumption, plus two
in-process typed launch capabilities. C-509 never adds a lifecycle operation, status field, lifecycle
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

## C-510 exposes two typed guard-bound launch capabilities

C-510 may lend C-509 only two in-process operations bound to the already-selected
`VerifiedInstallGuard` and authenticated owned instance. The vendor-ceremony operation launches only
the provider's fixed helper mode of that exact verified Exchange executable. C-509 supplies one
bounded, canonical non-secret initiating frame. C-510 creates a one-way request pipe for exactly one
at-most-65,548-byte frame plus EOF and a distinct one-way terminal-result pipe for exactly one
at-most-65,548-byte value-free receipt or error frame plus EOF. The request is only connect `BEGIN`
`0x0001` or credential acquire/rotate `BEGIN` `0x0030`; the result is only connect `RECEIPT`
`0x0006`, credential `RECEIPT` `0x0032` or `ERROR` `0x7fff`.

The Service Account mint operation is a separate typed operation. It launches only
`flux-exchange local service-account-mint --id <id> --expires-at <canonical-decimal> --writer-fd 5`
on Unix, or the same fixed mode with `--writer-handle <canonical-decimal>` instead of `--writer-fd`
on Windows, and accepts only the provider-owned closed mint identity and expiry through that typed
API. The helper receives C-509's dedicated FXSA writer: Unix maps only that writer to FD 5; Windows
admits exactly that writer HANDLE through its closed inherited-HANDLE list. It never shares, aliases
or inherits the ceremony request FD 6/result FD 7 or their Windows handles.

Neither capability exposes any of the following:

- a filesystem path, executable handle, cache lookup or alternate binary;
- arbitrary argv, extra argv, a shell, a generic process builder or a caller-selected helper mode;
- a secret or secret-bearing buffer;
- an endpoint, tenant, address, working directory, environment value, raw FD/HANDLE aperture or
  caller-selected FXLM operation; or
- a lifecycle mutation, lifecycle result, status field or diagnostic extension.

C-510 constructs each closed process from the guard and derives its cwd and native endpoint from the
authenticated owned instance's OS-account native root. It never derives either from the install or
cache guard identity, caller input or inherited `HOME`, XDG or profile environment. Corrected X-134
fixes Unix ceremony execution to `flux-exchange local vendor-secret` with no
additional arguments, request-read FD 6, terminal-write FD 7, reserved FXSA FD 5 closed and every
other descriptor at or above 3 closed. On Windows it fixes the same mode to exactly
`flux-exchange local vendor-secret --request-handle <REQUEST> --response-handle <RESPONSE>` in that
order, with canonical nonzero decimal HANDLE values; `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` contains
exactly those two distinct handles and every unused handle is non-inheritable. On both platforms the
environment is cleared, standard streams are the platform null device and the working directory is
the provider root derived as above. Neither the caller nor a generic
process API can add argv, an inherited descriptor/HANDLE or another placement value. Direct
TTY/browser access follows the provider's closed helper ABI, not redirected Flux streams.

C-510 must finish writing the initiating frame and close the request pipe by the absolute deadline
five seconds from spawn. From request EOF it applies one absolute 335-second deadline to the terminal
result; child traffic does not reset either deadline. X-134 fits its helper-private five-second
pre-ceremony work and 300-second predecision plus 30-second postdecision budgets inside that outer
result interval. This relationship is a Flux consumption assertion, but helper connection count,
state and traffic remain provider-owned and invisible to Flux. Ceremony stdout and stderr are empty.
A complete receipt or application refusal written to the result pipe exits 0; exit 1 is reserved for
a capability, transport or result-write failure that prevents the contract. If no terminal output
crosses, Flux may only launch the helper again with the byte-identical initiating frame.

The exact helper-mode spelling, Unix FD numbers, Windows argument spelling/order, frame bounds and
terminal-result grammar are consumption assertions owned by corrected X-134. Flux must copy the
committed provider names and bytes and fail conformance if they drift; it must not create a second
wire or launch contract alongside the accepted X-134 contract. This preserves C-510's exclusive
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

Every connect and credential acquire/rotate uses the ceremony, including a settings-only,
zero-secret create. For that case Exchange prepares an empty provider batch, sends `NEED_SECRETS`
with `secrets:[]`, the helper performs no prompt and sends no `SECRET`, then sends `COMMIT` and
returns the ordinary terminal result. Flux cannot learn whether prompting occurred. Create and
held-label create replay validate the plan with `selection:null`; credential acquire/rotate use
`selection:BEGIN.label` and carry the selected plan's exact opaque nonzero 256-bit
`credential_revision`, encoded as exactly 64 lowercase hexadecimal characters, in `BEGIN`. The
complete all-zero value refuses before ceremony.

C-509 consumes X-134's exact closed target-selection and partition fixtures. Create contains
`connection.name`, every required routable target and exactly the optional targets selected by their
plan target; acquire/rotate contain the complete credential partition and no name, setting or
authority target. Every target occurs exactly once in plan order and remains in its provider-declared
connection-name, settings, authority or credential partition. Flux cannot omit, invent, reorder,
duplicate or move a target across partitions. Static plan/target revisions and the opaque credential
CAS are never conflated.

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

C-509 speaks only the value-free `exchange.local-management.v1` plan, grant and receipt-query
operations and consumes X-134's closed receipts/errors verbatim. It does not speak or parse the
secret-bearing connection state machine. When a receipt id crossed the boundary, Flux may use
X-134's direct native value-free connect, credential, grant or Service Account receipt query. When
no receipt id crossed, Flux must not manufacture one or substitute a query: for a helper-mediated
proposal it may only relaunch the helper with the byte-identical initiating frame. Grant preview
consumes the complete connector-scoped candidate, revision/ETag and proposal digest;
compare-and-swap apply preserves unrelated connectors, inbound authority and all provider-owned
unmodified fields.

A pre-decision error follows only the provider's closed `never|refresh|operator` retry instruction.
An uncertain post-decision result retries only the same proposal; retry is never an edit path.
Approval is a separate Flux effect boundary: denial sends no Exchange invoke,
and uncertain send state is never automatically replayed for a non-idempotent or conditional write.
A high-risk or effectful invocation requires approval for its exact permission subject even after
the Exchange grant admits it.

## Provider authority is an implementation gate

Flux-roadmap Decision 0007 at `4511f44b4defcb6de92ab8fc1b56bd5b4356ca78` controls the capability
boundary and consumption requirements recorded here. Canonical Exchange merge
`3b16bcb5b1c52984449118775125fe66da1686da` contains accepted X-134 contract head
`9dc414c76f231bd179358fd526019a16872a7be1`, which is the provider authority for the exact bytes,
names, target-selection/partition rules, platform ABI and timeout relationships recorded here. This
acceptance does not satisfy the connectors C-515 registry-release gate, claim X-134 implementation
or provide the future X-126 release-v2 fixture inventory. Before C-510 or C-509 implementation
resumes, Flux must consume the implemented provider contract verbatim; it may not preserve a stale
alternative spelling, timing or wire format. No Flux test fixture or parser may create a second
authority.

## Dependency and evidence boundary

The direct provider dependencies are X-126, X-128 and X-134 for C-510, and X-134 plus C-510 for
C-509. X-129 supplies the already-delivered four HTTP v1 identities; it does not supply plan,
management or handoff identities. X-127 remains an observable transitive publication gate through
X-126.

The order remains `connectors/C-515 -> exchange/X-134 -> exchange/X-126 -> flux/C-510 ->
flux/C-509`. The canonical future X-126 v2 fixture inventory is the gate between provider and
consumer work. This design and the amended stories reconcile the contract only; they do not advance
that dependency order, mark acceptance complete or authorize production implementation.
