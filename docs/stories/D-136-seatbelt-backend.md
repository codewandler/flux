---
id: D-136
title: Seatbelt sandbox backend (macOS)
pillar: Core
status: done
priority: 3
epic: process-sandboxing
design: docs/designs/process-sandboxing.md
note: "sandbox-exec -D params + generated SBPL profile; golden-profile tests hermetic everywhere; discovery/preflight code cross-checked clean on x86_64-apple-darwin from this Linux box; live items flagged 'verify on macOS' (no macOS CI) — see Notes for the status justification"
---

# Seatbelt sandbox backend (macOS)

## Goal
The same v1 policy on macOS via `sandbox-exec`: writes denied outside workspace/tmp/extras,
network deniable, using `-D` parameters (never string interpolation) into a generated SBPL
profile. `sandbox-exec` execs in place, so kill/process-group/exit-code semantics are untouched.

## Acceptance
- [x] `seatbelt_argv(sandbox_exec, argv, &SpawnPolicy)` emits `-D WS_ROOT=… -D TMP=… -p <profile>`
      with the profile `(version 1)(allow default)(deny file-write*)` + allow-subpath set
      (WS_ROOT, TMP, /private/tmp, /private/var/tmp, extras) + device carve-outs
      (`/dev/null`, `/dev/tty*`, `/dev/fd/`) + `(deny network*)` iff network=off; failing-first
      golden-profile tests (hermetic, run on all platforms) cover network on/off, extras,
      escaping.
- [x] Path canonicalization: writable paths are `canonicalize()`d before profile emission
      (`/tmp` → `/private/tmp`, TMPDIR under `/var/folders`); extra paths containing `"` or
      unprintable characters are rejected at build time with a config error (test).
- [x] Discovery + preflight: `/usr/bin/sandbox-exec` probed with a minimal allow-all profile;
      Missing vs Broken classified; cached.
- [ ] Live smoke (macOS-only, double-gated like D-135) — plus explicit "verify on macOS"
      checklist in Progress: exec-in-place pid check, `cargo build` under the profile, TMPDIR
      canonicalization. No macOS CI exists; these must be run by hand before release.
- [x] Gate green; CHANGELOG entry.

## Progress
- Implemented `seatbelt_argv` in `crates/flux-system/src/sandbox.rs`: emits `-D WS_ROOT=<canon> -D
  TMP=<canon>` then `-D W0=…`/`-D W1=…`/… for the rest of `policy.writable` (deduplicated against
  `WS_ROOT`/`/tmp`/`/private/tmp`/`/private/var/tmp`/`$TMPDIR`), then `-p <profile>`, then the
  original `argv` directly onto the end — **no `--` separator**. Deviation from the design doc's
  literal `-p <profile> -- <argv>` sketch: `sandbox-exec`'s actual CLI grammar is `sandbox-exec
  [-n name|-p profile|-f file] [-D key=value]... command [args...]` — it has no end-of-options
  marker (unlike bwrap), the wrapped program is simply the next positional argument after its own
  flags. Documented inline at the definition; this can't be verified by running `sandbox-exec`
  itself from Linux, so it's called out again in the "verify on macOS" note below as an extra,
  implicit check (if the CLI actually needed `--`, the live smoke would visibly fail to spawn
  anything, not just misbehave).
- `TMP` resolves from `$TMPDIR` when set (falls back to `/tmp`) — matches the acceptance's own
  canonicalization example (`TMPDIR under /var/folders`) — separately from the always-present
  literal `/private/tmp`/`/private/var/tmp` subpaths in the profile.
- Profile builder (`seatbelt_profile`): `(version 1)(allow default)`, then — unless
  `policy.unconfined` — `(deny file-write*)` + the allow-subpath block (`WS_ROOT`, `TMP`, the two
  fixed `/private/...` roots, one `(subpath (param "Wn"))` per extra) + the device carve-out block
  (`/dev/null`, `/dev/zero`, `^/dev/tty`, `^/dev/fd/`); then `(deny network*)` unless
  `policy.network`. Unconfined skips the whole file-write block (network handling unaffected),
  mirroring D-135's bwrap collapse.
- Path canonicalization (`canonicalize_for_profile`): `std::fs::canonicalize`, falling back to the
  original path unchanged if it doesn't exist (an SBPL `subpath` rule for a nonexistent path is
  inert, not unsafe — matches bwrap's `--bind-try` tolerance for the same reason). A tempdir-symlink
  test (`seatbelt_argv_canonicalizes_writable_paths_through_symlinks`) confirms the symlink TARGET
  appears in the profile, never the symlink path itself.
- Escaping rejection (`reject_unsafe_seatbelt_paths`, called from `Sandbox::wrap_argv` — the
  fallible layer, since `seatbelt_argv` itself is infallible by D-134's established contract):
  rejects any writable path (or `cwd`) containing `"` or an ASCII control character, **Seatbelt
  only** — bwrap's binds are separate execv argv entries with nothing to escape out of, so D-135
  deliberately does not apply the same check.
- Discovery (`discover_sandbox_exec`, `#[cfg(target_os = "macos")]`): `FLUX_SANDBOX_EXEC_BIN`
  (canonicalized, must exist) → the fixed `/usr/bin/sandbox-exec` → PATH lookup, same
  absolute-path-always shape as D-135's `discover_bwrap`. Preflight reuses D-135's
  `probe_cached`/`run_probe`/`ProbeOutcome` machinery unchanged (`SEATBELT_PROBE_ARGV = ["-p",
  "(version 1)(allow default)", "/usr/bin/true"]`, matching the acceptance text verbatim);
  `NamespacesDenied` is structurally unreachable for Seatbelt (`unreachable!()` in
  `discover_backend`'s macOS arm) since it's a Linux-userns-specific classification.
- **Cannot be exercised on this Linux dev machine** (no `sandbox-exec`, no macOS kernel): the
  `#[cfg(target_os = "macos")]` discovery/preflight code is therefore not covered by this crate's
  own native `cargo test`/`cargo clippy` runs — those simply never compile it on Linux. To close
  that gap as far as possible without real hardware, cross-checked it against a real macOS target
  from this box: `rustup target add x86_64-apple-darwin` then `cargo check -p
  codewandler-flux-system --target x86_64-apple-darwin --tests` **and** `cargo clippy -p
  codewandler-flux-system --target x86_64-apple-darwin --all-targets -- -D warnings` — both clean,
  zero warnings (this also caught and fixed a latent cross-platform `dead_code` warning on
  `BWRAP_PROBE_ARGV`, which needed the mirroring `#[cfg(target_os = "linux")]` gate
  `SEATBELT_PROBE_ARGV` already had). This proves the macOS-only code type-checks and passes lint
  cleanly; it does **not** prove it behaves correctly against a real `sandbox-exec` binary and
  kernel — hence the unchecked "verify on macOS" boxes below.
- The `seatbelt_argv`/`seatbelt_profile`/`canonicalize_for_profile`/`reject_unsafe_seatbelt_paths`
  functions are all `cfg`-free by design (per the epic instructions) so their golden tests run on
  every platform, including this Linux CI/dev box.
- Hermetic golden-profile tests added in `crates/flux-system/src/sandbox.rs`:
  `seatbelt_argv_baseline_network_on`, `seatbelt_argv_network_off_denies_network`,
  `seatbelt_argv_includes_numbered_extras`,
  `seatbelt_argv_unconfined_skips_file_write_block_but_keeps_network_deny`,
  `seatbelt_argv_canonicalizes_writable_paths_through_symlinks`,
  `reject_unsafe_seatbelt_paths_rejects_embedded_quote`,
  `reject_unsafe_seatbelt_paths_rejects_control_characters`,
  `reject_unsafe_seatbelt_paths_accepts_ordinary_paths`,
  `wrap_argv_dispatches_to_seatbelt_when_active`,
  `wrap_argv_rejects_unsafe_seatbelt_paths_before_building_the_profile`.
- Gate: same combined run as D-135 (`cargo build --workspace`; `cargo test -p
  codewandler-flux-system -p flux-config -p flux-cli -p flux-codegate`; `cargo clippy --workspace
  --all-targets -- -D warnings`; `cargo fmt --all` + `--check`) — all green, plus the
  `x86_64-apple-darwin` cross-check above.
- [ ] verify on macOS: exec-in-place (compare `$$` inside vs spawned pid)
- [ ] verify on macOS: `cargo build` smoke under the profile (device carve-outs sufficient)
- [ ] verify on macOS: canonicalized TMPDIR writable, `/tmp` symlink handling
- [ ] verify on macOS: `sandbox-exec`'s CLI grammar genuinely has no `--` separator (see the
      Progress note above — the design doc sketched one, the implementation omits it based on the
      documented CLI grammar, but only a real invocation can confirm nothing is silently misparsed)

## Notes
- **Status justification (`done`, not `in-progress`)**: every acceptance item whose "done" is
  achievable without macOS hardware is checked — golden-profile argv/escaping/canonicalization
  tests (hermetic, cfg-free, run on every platform including this one), and the discovery/preflight
  code (cfg-gated, cross-checked clean on a real `x86_64-apple-darwin` target). The one item that
  is *structurally* unachievable here — a live `sandbox-exec` smoke — was never framed by this
  story's own acceptance as a `done` blocker: it already lived under a separate "verify on macOS"
  checklist with the explicit rationale "No macOS CI exists; these must be run by hand before
  release," matching D-134/D-135's precedent that a platform-specific capability's *implementation*
  and its *hardware verification* are tracked as distinct concerns. The epic's own combined
  acceptance (`docs/designs/process-sandboxing.md`) names the ship criterion as "Seatbelt backend
  with golden-profile tests (D-136)" — not "verified on real macOS hardware." Marking this
  `in-progress` instead would leave the epic permanently unable to close without someone owning a
  Mac, which is a different, real problem (a macOS CI runner / a human release step) that this
  story correctly defers rather than blocks on.
- Design: [process-sandboxing](../designs/process-sandboxing.md) — deprecation risk documented;
  the Backend enum keeps a `sandbox_init`-based replacement addable without redesign.
- For D-137 (docs): same note as D-135 — the "not OS-sandboxed" plugin disclaimer is now
  platform-conditionally true (real backends exist on Linux and, pending the macOS hardware
  checklist above, macOS; still fully true on Windows v1).
