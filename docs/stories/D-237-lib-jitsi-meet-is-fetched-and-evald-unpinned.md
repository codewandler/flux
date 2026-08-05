---
id: D-237
title: "The sidecar's default fetches lib-jitsi-meet from the network and `eval`s it with no integrity check"
pillar: Agent
status: blocked
priority: 2
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "sidecar.js defaults to https://8x8.vc/libs/lib-jitsi-meet.min.js, fetched and (0,eval)'d with no SRI, hash or pin — and the network URL rather than the vendored path is the default. Unreachable today because only `join` triggers it"
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
- [ ] A failing-first test: the sidecar **refuses to execute** library bytes that do not match the
      recorded hash / integrity attribute. It must fail at the merge base, and it must assert the
      refusal, not merely that a hash was computed.
- [ ] The default is the safe path. If a network fetch remains possible at all, it is opt-in and its
      integrity requirement is not optional.
- [ ] A version is pinned explicitly, so "it worked yesterday" and "it works today" mean the same bytes.
- [ ] ⚠ The failure mode when integrity does not match is **diagnosable**: it names the mismatch and the
      expected source, rather than surfacing as an opaque page error. An operator hitting this needs to
      know it was an integrity refusal and not a broken room.

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

## Progress

- 2026-08-05 — dispatched in wave `flux-wave-20260805-0829` as `ready`; raised to `blocked` without
  an implementation, because the acceptance surface is not on `main`.

  Every clause of this contract is about `crates/flux-channels/assets/room-media/sidecar.js` — the
  `:63` network default, the `(0, eval)` at `:216-223`, the refusal test, the version pin. That file
  has never existed on `main` and is absent from the wave's source commit `dc07e60e`; it exists only
  on the unmerged `impl/D-232`. There is no sidecar to make refuse anything.

  **Ground truth for whoever records the decision.** `impl/D-232`'s tip (`6d1053b1`) has already
  chosen, and the reasoning is worth preserving: a **local, version-and-integrity-pinned bundle
  only**, because fetching from inside the spawned Node process would bypass flux-system's guarded
  egress and DNS pinning *even if the bytes were hash-checked*. That is exactly the sandbox
  interaction this story's Notes said to check rather than assume, and it argues against
  fetch-with-SRI on grounds the contract did not anticipate. Pins used there: 8x8 release `6869`,
  SHA-256 `09f03ed9d03f4c7dc4691d9e8781f9872ca89660c07a59dad5c292c83f89a0e1`. That commit is honest
  that the state is interim — the bytes are operator-supplied rather than repository-vendored, so
  neither accepted end state is fully reached.

  No decision was recorded on the wave branch on purpose: recording it in the design doc while the
  enforcing code lives on another branch is precisely the "documented elsewhere is not a defence"
  failure this family of stories exists to attack, and it would conflict with `impl/D-232`'s own
  design-doc edit.

  **The D-237-before-D-236 ordering mandate is satisfied vacuously**: neither landed, so no page
  context ever held unverified library bytes together with a live credential. Blocked behind D-232,
  which is blocked on a human confirming audible tone in a live room.
