---
id: D-235
title: "Argv alone does not reach the host audio server — the sandbox masks the socket"
pillar: Agent
status: ready
priority: 3
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels, flux-system]
note: "D-208 says host audio specifics ride in the sidecar's argv; that is necessary but NOT sufficient — bubblewrap_argv masks /run with a tmpfs, so the pulse socket is gone regardless of what argv says. An operator following the recipe gets a silent zero level"
---

# A recipe that is necessary but not sufficient

## Goal

Make the documented path to real room audio actually reach a real audio server, and say plainly what an
operator must grant — so following the instructions produces sound rather than a silent zero level.

## The finding

[D-208](D-208-room-media-sidecar.md) (`done`) states the seam at `:88-89` and `:108`:

> flux clears `DISPLAY`/`XDG_RUNTIME_DIR`/`PULSE_SERVER`; the seam therefore requires argv to carry
> anything the sidecar needs about the host audio server.
>
> Host specifics ride in the sidecar's argv, which flux passes through and never interprets.

That is the right seam and the env-clearing is correct. **But argv is not sufficient.** The sandbox
mounts a tmpfs over `/run` (`crates/flux-system/src/sandbox.rs:1194`, and the invariant is asserted at
`:2563`: *"host IPC directory must stay hidden behind the /run tmpfs"*). The PulseAudio / PipeWire
socket lives at `/run/user/<uid>/pulse`. So the socket is **masked**, and no argv value can name a path
that is not there.

An operator must *also* grant it explicitly — `[sandbox] writable = ["/run/user/<uid>/pulse"]`
(`crates/flux-config/src/lib.rs:338-342`, surfaced as `sandbox_writable()` at `:1023-1026`). Nothing in
D-208, and nothing in the sidecar's own documentation, says so.

⚠ **The failure mode is the bad kind: silent.** The sidecar starts, connects, publishes, and the level
probe reads zero. There is no error that names the masked socket, so the operator's evidence points at
the sidecar or at the room, not at the sandbox. This is exactly the case where "it is documented
elsewhere" is not a defence — the guarantee is fine, the *discoverability* is the defect.

## Acceptance

- [ ] The argv-only claim is corrected wherever it is stated: D-208's Notes, the sidecar's own docs, and
      any website page that describes bringing real audio to a room.
- [ ] The companion grant is documented as part of the same recipe, with a concrete example, and stated
      as **required**, not optional.
- [ ] ⚠ A failing-first test pinning the composition, not just the prose: with argv correct and the
      socket path **not** in `[sandbox] writable`, the sidecar's audio path fails in a way that names
      the masked socket rather than reporting a zero level. A diagnosable failure is the deliverable;
      silence is the bug.
- [ ] ⚠ **No env passthrough is added to `flux-system`.** The env-clearing at the seam is deliberate and
      correct; this story documents and diagnoses, it does not loosen the sandbox.
- [ ] The `/run` tmpfs mask and the `:2563` invariant are unchanged.

## Notes

- Verified 2026-08-02 against `main`: the tmpfs mask at `sandbox.rs:1194`, the invariant assertion at
  `:2563`, and the config key at `flux-config/src/lib.rs:338`.
- ⚠ Note the asymmetry the fix must respect: `writable` grants *write* access, and the pulse socket
  needs to be *reachable* — check whether the existing `writable` grant is the right mechanism for a
  socket at all, or whether a socket needs its own bind. If `writable` does not actually suffice, that
  is a bigger finding than the documentation gap and should be said out loud rather than papered over
  with a config line that does not work.
- Surfaced by [D-232](D-232-the-media-sidecar-harness.md)'s implementor while answering its Risk 1;
  its own answer contradicted D-208's story text, which is what prompted this. D-232 was instructed not
  to add env passthrough and did not.
- Related: [D-232](D-232-the-media-sidecar-harness.md) (the browser harness), and the meeting-rooms
  design.
- ⚠ Renumbered D-233 → D-235 immediately after filing: a parallel session took D-233 while this was
  being written. See also [D-234](D-234-mediasettings-debug-prints-argv.md), filed in that same wave and
  touching the same argv path.
