---
id: D-235
title: "Argv alone does not reach the host audio server — the sandbox masks the socket"
pillar: Agent
status: done
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

- [x] The argv-only claim is corrected wherever it is stated: D-208's Notes, the sidecar's own docs, and
      any website page that describes bringing real audio to a room.
- [x] The companion grant is documented as part of the same recipe, with a concrete example, and stated
      as **required**, not optional.
- [x] ⚠ A failing-first test pinning the composition, not just the prose: with argv correct and the
      socket path **not** in `[sandbox] writable`, the sidecar's audio path fails in a way that names
      the masked socket rather than reporting a zero level. A diagnosable failure is the deliverable;
      silence is the bug.
- [x] ⚠ **No env passthrough is added to `flux-system`.** The env-clearing at the seam is deliberate and
      correct; this story documents and diagnoses, it does not loosen the sandbox.
- [x] The `/run` tmpfs mask and the `:2563` invariant are unchanged.

## Result

**`[sandbox] writable` is the right mechanism for a unix socket, and the asymmetry in the Notes
resolves in its favour** — but for a reason the key's name does not advertise, so it is now written
down rather than inferred. `writable` emits a read-write bwrap `--bind`, and read-write is not an
over-grant for a socket, it is the *minimum*: `connect(2)` on an `AF_UNIX` socket takes `MAY_WRITE`
on the socket inode, so a `--ro-bind` would leave the socket visible and unconnectable. In
bubblewrap there is no separate "reachable" grant to look for — reachability **is** the bind. The
second half is ordering: `--tmpfs /run` is emitted before the configured binds
(`sandbox.rs`), and bwrap applies mount ops in argv order while resolving bind *sources* in the
original namespace, so the grant punches the host directory back through the mask. Reversed, the
config line would be silently inert. Both properties are now pinned by
`a_configured_run_grant_is_bound_read_write_after_the_run_mask`.

**One genuine defect was found behind the documentation gap**, and it is the shape the Notes warned
about — a config line that looks applied and does not work. `prepare_writable_paths` *creates* a
configured writable path that does not exist. That is right for an output root and wrong for `/run`:
a mistyped uid (`/run/user/1001/pulse` on a uid-1000 host) had flux create an empty directory and
bind it over the mask, after which the sandboxed process finds a directory, finds no socket, and the
only evidence is a level probe reading zero. A `/run` path that does not exist is now refused at
startup by name, and nothing is created under `/run`.

The diagnostic half is `Ready::routing_error`: the sidecar's own explanation for
`owns_device_routing: false`, preserved through the handshake and quoted verbatim in
`publish_audio`'s refusal. When the sidecar says nothing, the refusal now names the `/run` mask and
the `[sandbox] writable` grant itself, so the operator is never left with only a zero level.

No environment entry was added or changed — `crates/flux-system/src/lib.rs` is untouched — and the
`sandbox.rs` diff contains **zero deletions**: the `/run` tmpfs mask and the "host IPC directory
must stay hidden behind the /run tmpfs" assertion are byte-identical, only shifted by added code.

## Notes

- Verified 2026-08-02 against `main`: the tmpfs mask at `sandbox.rs:1194`, the invariant assertion at
  `:2563`, and the config key at `flux-config/src/lib.rs:338`.
- ⚠ Note the asymmetry the fix must respect: `writable` grants *write* access, and the pulse socket
  needs to be *reachable* — check whether the existing `writable` grant is the right mechanism for a
  socket at all, or whether a socket needs its own bind. If `writable` does not actually suffice, that
  is a bigger finding than the documentation gap and should be said out loud rather than papered over
  with a config line that does not work.
## Progress

**2026-08-05** — Implemented. `Ready::routing_error` added to the media protocol and surfaced in
`SidecarMediaPeer::publish_audio`'s refusal; `/run` grants that name nothing are refused at startup
instead of created; the argv-only claim corrected in `sidecar.rs`'s module header, D-208's Notes and
"what no test can tell you", the design's preflight runbook (new required step 8),
`website/docs/security/os-sandbox.md` (new "Reaching a host socket on purpose"),
`website/docs/troubleshooting.md` (new silent-audio section) and `website/docs/reference/config.md`.
Failing-first evidence captured for both halves before implementing.

⚠ **The `sidecar.js` half of this story's sibling stories could not be done here.**
`crates/flux-channels/assets/room-media/sidecar.js` does not exist on `main` and has never existed
there; it lives only on the unmerged `impl/D-232` branch. The sidecar-side preflight that *produces*
`routing_error` is therefore D-232's to land. What is landed here is the flux-side half: the protocol
field, its preservation, the refusal text, the startup refusal, and the documentation. A sidecar that
does not send `routing_error` still gets a refusal naming the mask and the grant.

- Surfaced by [D-232](D-232-the-media-sidecar-harness.md)'s implementor while answering its Risk 1;
  its own answer contradicted D-208's story text, which is what prompted this. D-232 was instructed not
  to add env passthrough and did not.
- Related: [D-232](D-232-the-media-sidecar-harness.md) (the browser harness), and the meeting-rooms
  design.
- ⚠ Renumbered D-233 → D-235 immediately after filing: a parallel session took D-233 while this was
  being written. See also [D-234](D-234-mediasettings-debug-prints-argv.md), filed in that same wave and
  touching the same argv path.
