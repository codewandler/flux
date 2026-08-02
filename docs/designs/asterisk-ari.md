# Asterisk ARI from the official Swagger contract

**Status:** superseded 2026-08-02 by [D-249](../stories/D-249-remove-asterisk-plugin.md)

> This records the implementation that shipped in v0.51.1; it is not current ownership guidance.
> The owner correction is that ARI's spec-described REST surface belongs in `flux-connectors`, while
> Flux retains neither ARI nor the AMI plugin. Connector event/channel design is deferred.

## Superseded decision

Asterisk remains a native Flux plugin. It is a stateful, private-network, protocol-rich technology
adapter, so it does not become a generated provider in `flux-connectors`. The existing eight AMI
operations remain stable and the same `asterisk` plugin gains an `asterisk.ari.*` surface.

The source of truth is Asterisk's own Swagger description at release tag `22.10.1`: annotated tag
object `4f85d05889cf9fb9c9e2ae44cc3f4a825a74545a`, peeled source commit
`f0e408a7b0d829c85bf15fa4b487870a50cb3000`. The vendored set is `rest-api/resources.json` plus the
eleven documents it names under `rest-api/api-docs/`. A fresh measurement on 2026-08-02 found 76
paths, 109 operations, 85 models and 275 parameters. The eleven documents are byte-identical at the
then-current standard release `23.4.1`; the LTS tag is pinned because it is the more stable operator
contract. Upstream drift is reported separately rather than silently changing the shipped surface.

These are legacy Swagger documents (1.1, with `events.json` at 1.2), not OpenAPI 3. They are consumed
inside the hand-written plugin rather than widened into `connector-spec`'s OpenAPI front end.

## What complete means

The generated contract must account for every one of the 109 Swagger operations:

- 108 request/response REST operations are individually discoverable, with exact method, path,
  required parameters, primitive/list types, response model and source nickname.
- The `/events` WebSocket is reachable through a host-owned endpoint-reference WebSocket capability.
  The plugin never receives the endpoint URL or Basic-auth secret.
- The stored-recording binary response is transferred into the host blob store without returning a
  potentially large base64 value through the plugin protocol.
- All 85 declared models are translated into output JSON Schema, including inheritance and list
  responses. Unknown vendor fields stay lossless at execution.
- Existing AMI operation names and schemas do not move.

The coverage test reads the vendored files and compares their operation identities with the plugin
manifest in both directions. A route that exists only in the spec or only in the manifest is a red
test. A second test pins the source tag, commit and document hashes.

## Safety boundary

ARI is normally private/loopback. The plugin declares `ASTERISK_ARI_URL` with the host-side default
`http://localhost:8088/ari`, and callers still need the existing scoped private-network grant. HTTP
Basic authentication is injected by the host from `ASTERISK_ARI_USERNAME` and
`ASTERISK_ARI_PASSWORD`; neither value is returned to the plugin.

GET operations are read/low/idempotent. POST and PUT are writes unless the official operation is a
read-shaped probe; DELETE operations are destructive and declare the `delete` semantic effect.
Call origination, external-media creation, playback, recording and event publication are at least
high risk because they affect live calls or send media/events externally. Generated metadata carries
an explicit reviewed override table for these cases; HTTP method alone is not accepted as the final
risk decision.

The WebSocket capability is separate from request/response HTTP and deny-by-default in the plugin
manifest. It performs the same endpoint resolution, SSRF/private-network check, host allow-list and
auth injection as `http.do`. Connections are host-owned, byte/message bounded, deadline bounded and
closed on plugin/session teardown. No plugin opens a socket directly.

The binary HTTP-to-blob path is likewise host-owned and bounded by an operator-visible maximum. It
streams into the scratch blob store and returns only `{blob_ref, size, sha256}`. The existing inline
binary path retains its 16 MiB cap.

## Generation boundary

`plugins/asterisk/scripts/vendor-ari-specs.sh` fetches only the pinned public GitHub bytes, validates
the resource inventory, writes provenance and is deterministic under `--source-dir`. Vendored specs
are committed with their upstream license/attribution and are development inputs only. A deterministic
generator normalizes factual operation/model contracts into reviewed committed Rust; the plugin binary
does not `include_str!` or redistribute the upstream Swagger text. Generator parse errors refuse before
changing output, while duplicate or inconsistent generated registrations still reach `try_serve` as
startup errors rather than panics.

The generic executor accepts only schema-declared fields, percent-encodes path and query values with
the shared host-kit encoder, emits at most one declared JSON body, and rejects the WebSocket route on
the REST executor. Non-2xx responses preserve status and a capped body in the error. Void responses
return a stable status receipt; JSON responses preserve the vendor value.

## Delivery order

1. Vendor and inventory the exact official documents.
2. Add shared encoding and host-owned WebSocket/HTTP-to-blob primitives with adversarial tests.
3. Generate and execute all REST operation contracts resource by resource.
4. Bind the event WebSocket and stored-recording binary route.
5. Run the root and nested-workspace gates, update the live smoke, then publish a plugin-pack release.
