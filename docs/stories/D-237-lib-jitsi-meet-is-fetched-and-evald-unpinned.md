---
id: D-237
title: "The sidecar's default fetches lib-jitsi-meet from the network and `eval`s it with no integrity check"
pillar: Agent
status: in-progress
priority: 2
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "the unsafe network execution path is removed: URL schemes refuse before IO and explicit local bytes need the release digest. Still open: operator-supplied local bytes are not a repository-vendored artifact, so the story's vendor-or-fetch-with-SRI end-state decision is not complete"
---

# Remote code, evaluated, unpinned, by default

## Goal

Decide and implement how the sidecar obtains `lib-jitsi-meet` — vendored and hash-pinned, or fetched
with subresource integrity — so that joining a room cannot execute code nobody verified.

## The finding

`crates/flux-channels/assets/room-media/sidecar.js:63` defaults the library location to
`https://8x8.vc/libs/lib-jitsi-meet.min.js`. It is fetched and evaluated via `(0, eval)` at `:216-223`.
There is **no SRI, no hash, no version pin**, and the **network URL is the default** — the vendored path
is the opt-in, which is the wrong way round.

So the shipped default for joining a room is: fetch whatever bytes that host serves today, and execute
them in the page that holds the room token and the audio track. A compromised or substituted asset at
that URL runs with the sidecar's full page context. Nothing in the tree would notice: there is no
recorded hash to compare against.

⚠ **Not currently reachable, which is why this is a story and not an incident.** Only the `join` path
triggers the fetch, and per
[D-232](D-232-the-media-sidecar-harness.md) there is still no audio in a real call — the live-room half
is `#[ignore]`d and unshipped. This must be settled **before** a real join ships, not after.

## Acceptance

- [ ] A decision, recorded with reasoning: vendor-and-pin, or fetch-with-SRI. ⚠ Pick one; do not
      implement both and leave the weaker one as the default, which is the current failure.
      → **OPEN.** The interim implementation accepts only operator-supplied local bytes with a
      version/digest pin. That removes the unsafe runtime fetch, but it is not a repository-vendored
      artifact and therefore does not yet satisfy either accepted end state.
- [x] A failing-first test: the sidecar **refuses to execute** library bytes that do not match the
      recorded hash / integrity attribute. It must fail at the merge base, and it must assert the
      refusal, not merely that a hash was computed.
- [x] The default is the safe path. If a network fetch remains possible at all, it is opt-in and its
      integrity requirement is not optional.
- [x] A version is pinned explicitly, so "it worked yesterday" and "it works today" mean the same bytes.
- [x] ⚠ The failure mode when integrity does not match is **diagnosable**: it names the mismatch and the
      expected source, rather than surfacing as an opaque page error. An operator hitting this needs to
      know it was an integrity refusal and not a broken room.

## Interim result

Interim posture: **local, version-and-integrity-pinned bundle only**. The tenant's browser bundle is the
compatibility surface, but fetching it inside the spawned Node process would bypass flux-system's
guarded-egress and DNS-pinning boundary even if the bytes were hash-checked. The no-argument path has
no source; URL schemes are refused before IO; an explicit local `--jitsi` uses the shipped pins for
8x8 release `6869` plus SHA-256
`09f03ed9d03f4c7dc4691d9e8781f9872ca89660c07a59dad5c292c83f89a0e1`; the local bytes must match the
digest before any CDP evaluation. The mismatch test asserts the evaluator was never called, a
second test refuses the former URL before execution, and a guarded operator fetch re-verified the
1,089,184-byte release artifact on 2026-08-02. The final vendor-or-SRI artifact decision remains open;
this hardening can be checkpointed without calling D-237 done.

## Notes

- ⚠ Whatever is chosen interacts with the sandbox: a vendored asset needs to be reachable inside the
  confinement, and a network fetch needs egress that passes the guard. Check both rather than assuming
  the chosen path is reachable — see [D-235](D-235-argv-alone-does-not-reach-the-audio-server.md) for the
  same class of mistake made about the audio socket.
- ⚠ Interacts with [D-236](D-236-room-token-cannot-reach-the-sidecar.md): the page that evaluates these
  bytes is the page that will hold a real room token once D-236 lands. Unverified code plus a live
  credential in one context is materially worse than either alone, so D-237 should land **before** or
  **with** D-236, not after.
- Surfaced by D-232's review, 2026-08-02. Pre-existing from D-208's sidecar asset rather than introduced
  by D-232.
- Related: [D-232](D-232-the-media-sidecar-harness.md), [D-208](D-208-room-media-sidecar.md).
