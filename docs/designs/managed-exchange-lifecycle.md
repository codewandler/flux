# Managed Exchange lifecycle identity and filesystem safety

This design records the Flux-owned safety boundary for C-510. The provider wire authority is
Exchange `docs/designs/local-release-v1.md` and X-126/X-127/X-128/X-129 at commit
`4ade23df62ce9fa8de39e9083ca5e0c98502d838`. Flux consumes those canonical types and fixture bytes;
this document deliberately defines no competing trust, channel, manifest, compatibility, archive or
readiness shape.

## Identity is one closed chain

An accepted install is identified by the complete authenticated chain, never by its cache pathname,
package version, tag alone or executable name:

- the authenticated bootstrap policy identity and the accepted trust document identity;
- the accepted stable-channel identity and the selected channel entry;
- the selected provider release/process and target identity, the canonical manifest digest and the
  complete six-value provider protocol identity;
- the exact manifest-declared member inventory: every normalized provider path, member kind,
  declared byte count and digest, including the executable member and digest; and
- verifier-observed file type, byte count, digest, owner/security state and executable-mode state for
  every member.

All fields above are validated provider values or verifier observations. None is accepted as a
caller-supplied observation. The installed audit record binds the whole chain and exact inventory.
A cache lookup is a hit only when its record and a fresh descriptor-anchored walk reproduce that
identity exactly: a missing, additional, renamed, type-changed, size-changed, digest-changed,
permission-changed or executable-mode-changed member refuses the hit. A cache directory name is only
an index into this check and conveys no trust.

The process identity extends that same immutable installed identity with the already-open child
process handle and X-128's provider-validated process-start and readiness identities. Channel
refresh, expiry or a newly selected release cannot rewrite the identity of a live process. PID,
pathname, listener address and package version never establish ownership.

## Verification owns its observations and limits

Hard security ceilings are compiled policy derived from the authenticated bootstrap and the exact
provider schema version. CLI flags, environment, project configuration, model input and import
metadata cannot raise or replace them.

The verifier reads each metadata, signature, archive and member stream itself. It hashes and counts
the actual bytes while reading under its fixed bound, refuses before an allocation or arithmetic
operation can exceed that bound, and compares the final observed count and digest with the
authenticated declaration only after end-of-stream. It also counts archive members and total
expanded bytes itself. Early EOF, trailing bytes, decompression beyond any bound, count overflow or
any declared/observed disagreement refuses. A transport, importer or caller may hand the verifier a
byte source and authenticated expectations; it may not hand in an "observed" digest, size, member
count or caller-selected ceiling.

Archive verification and extraction are one operation. A member becomes eligible for staging only
after its provider-owned name and kind have been admitted, and its bytes remain under the per-member,
total-expanded and member-count limits during extraction. The resulting exact observed inventory is
what the installed audit record retains.

## Anchored filesystem operations

Every lifecycle root is opened once as an owner-only directory capability. All traversal, creation,
revalidation, publication and quarantine operations are relative to retained directory descriptors
or handles. Each path component is opened without following links and is checked after opening.
Absolute lookup, ambient-current-directory traversal and a validate-by-path/use-by-path sequence are
forbidden.

Archives may contain only provider-declared regular files. Symlinks, hardlink entries, reparse-like
objects, devices, sockets, FIFOs and any other unsupported kind refuse. Staging and cache-hit walks
also refuse a regular file whose link state does not prove it is exclusively contained in the
install. Refusal never repairs metadata: Flux does not `chmod`, relink, truncate, overwrite or unlink
the rejected inode. In particular, a planted hardlink is quarantined only by moving the enclosing
install directory; its externally reachable inode and metadata are not touched.

Staging, live installs and quarantine are children of one pre-opened owner-only cache root and must
be proved to reside on one filesystem/volume. A newly created staging directory starts with final
owner-only security. Files are created exclusively through the staging capability, with no-follow
semantics and final security at creation; existing unsafe objects are never narrowed or repaired.
The per-release lock is acquired before staging or cache revalidation and remains held across the
transaction.

Before publication Flux flushes every staged file and required directory metadata. Publication is a
single same-filesystem atomic rename from a fully verified staging directory into the live namespace;
the parent directory is then made durable wherever the platform contract requires it. Replacement
preserves the old verified install until the new candidate is complete and uses an atomic directory
or pointer transaction whose crash result is wholly old or wholly new, never a mixture.

A failed staging candidate is removed through its retained parent capability only after an anchored
walk proves that cleanup cannot mutate a rejected shared inode. Otherwise it, like a visible install
that fails revalidation, is atomically renamed as a whole into its bounded quarantine slot without
member mutation. Source and destination parent-directory updates are made durable as required.
Quarantine is outside the executable namespace: lifecycle code never opens an execution handle from
it and never changes member modes in an attempt to make rejected bytes safe. If the platform cannot
prove anchored no-follow traversal, exclusive containment, same-filesystem atomic rename, required
durability or owner-only security, that operation and target fail closed.

## A verified install is a retained capability

Successful verification returns a non-path capability, conceptually a `VerifiedInstallGuard`. It
retains the per-release guard/lock, opened installation directory, opened executable and the complete
installed identity. It exposes identity and process construction, not a bare executable path.

Process creation consumes or borrows this guard. The platform implementation must either execute
from the retained executable handle or provide an equivalent guard that prevents replacement and
proves the executable used by the OS is the still-open verified object. Immediately reopening a
validated pathname is not equivalent. The guard stays alive until process creation returns an open
child handle and the child identity has been captured; the supervisor retains it with the owned
child for the child's lifetime. Readiness can add the provider process-start identity, but cannot
retroactively repair a pathname race or substitute for the retained installation identity.

An implementation without a native, tested equivalent for this handle-to-process binding is
unsupported and refuses to start. Cross-platform gaps are never filled by silently falling back to
path execution.

## Provider-owned seams that remain blocked

The following are deliberately not implementable from locally invented examples:

- **X-126:** canonical trust/channel/manifest/compatibility documents and signatures; exact asset,
  member, path and archive rules; delegated trust material; fixture inventory and verdicts; signed
  release artifacts; and the first public five-target release. These inputs close selection,
  archive/cache/install conformance and the clean-machine proof.
- **X-127:** native owner-only persistence/restart evidence that determines whether a target may
  appear in X-126's signed platform set. Flux must not infer platform support from compilation or
  describe Unix modes as Windows security behavior.
- **X-128:** the exact supervised launch ABI, readiness bytes, liveness behavior, native process-start
  identity and positive/adversarial fixtures. These close process ownership and readiness matching.
- **X-129:** provider tests binding the four advertised HTTP protocol identities to their actual wire
  routes and types. Flux treats those identities as supported only with that provider evidence.

Until those seams exist, Flux does not fabricate schemas, fixture bytes, archive roots or member
names, platform mappings, Windows behavior, readiness records, tagged-union integer mappings, release
identities or a substitute signed release. Provider fixture changes require the corresponding
supported schema/protocol identity and byte-identical conformance update; they are not normalized or
reinterpreted locally.
