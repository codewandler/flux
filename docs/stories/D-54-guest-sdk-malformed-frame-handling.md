---
id: D-54
title: "Guest SDK serve() must not silently skip malformed host frames"
pillar: Core
status: done
note: "god-review finding #4 survivor (the one validated kernel): `flux_plugin::serve` silently `continue`s on an unparseable frame from the host — the host then awaits a response that never comes; diagnose + bound instead of hiding"
---

# Guest SDK serve() must not silently skip malformed host frames

## Goal
The guest-side plugin SDK loop (`flux_plugin::serve`, `crates/flux-plugin/src/lib.rs:439`) drops
any line that fails to parse as a `Frame` with a bare `continue` — no stderr diagnostic, no
counter, no bound (`lib.rs:455-457`). Frames arrive only from the parent host process, so a
malformed frame means a host bug or stream corruption; skipping it silently strands the host
awaiting a response for a request id that will never be answered. Mirror the workspace's
stream-resilience philosophy (skip + count + surface a diagnostic; only persistent breakage is
fatal): emit a stderr diagnostic per malformed frame and exit the loop after a small bound of
consecutive malformed frames.

## Acceptance
- [x] Failing-first test: a malformed line followed by a valid request — today the malformed line
      vanishes without trace; after the fix the guest writes a one-line diagnostic to stderr and
      still answers the valid request (single malformed frames stay tolerated).
- [x] Failing-first test: N consecutive malformed frames (documented constant, e.g.
      `MAX_CONSECUTIVE_MALFORMED_FRAMES`) terminate the serve loop with a final stderr diagnostic —
      the host side then surfaces its existing "plugin closed the connection" error instead of
      hanging. A valid frame resets the counter.
- [x] `serve()` currently locks `std::io::stdin`/`stdout` directly — introduce a testable
      `serve_io(reader, writer, handler)` (or equivalent) seam that `serve()` delegates to, so the
      tests run in-process without spawning a binary. No behavior change for well-formed traffic;
      existing plugin protocol tests (`crates/flux-plugin/tests/host.rs`) stay green.
- [x] Diagnostics go to **stderr only** (stdout is the protocol channel) and never echo the raw
      malformed bytes beyond a bounded, char-boundary-safe prefix (frames can carry secrets in
      well-formed traffic; malformed lines get length + parse-error only, not content).

## Progress
- 2026-07-06 filed — from the god-review validation pass (`review.md`, finding #4). The host side
  is already hardened (8 MiB frame cap, hard errors, id-correlated demux — `lib.rs:1974`); this
  story closes the guest side.
- 2026-07-06 implemented (`crates/flux-plugin/src/lib.rs`):
  - Added `const MAX_CONSECUTIVE_MALFORMED_FRAMES: u32 = 5` (lib.rs:442).
  - `pub fn serve(handler)` (lib.rs:447-451) now just locks real stdin/stdout/stderr and delegates
    to a new private `fn serve_io<R: BufRead, W: Write, D: Write>(reader, writer, diag, handler)`
    (lib.rs:463-onward) — same loop body as before for well-formed traffic, but a line that fails
    `serde_json::from_str::<Frame>` now: increments a `consecutive_malformed` counter, writes one
    `writeln!(diag, …)` diagnostic naming only the line's byte length and the `serde_json::Error`
    (never the raw content — satisfies "length + parse-error only, not content"), and `continue`s;
    any frame that *does* parse resets the counter to 0. Hitting
    `MAX_CONSECUTIVE_MALFORMED_FRAMES` consecutive failures writes one final diagnostic naming the
    bound and `break`s the loop (so the guest process exits and the host's existing
    `read_frame`/"plugin closed the connection" hard-error path fires instead of an indefinite
    hang). Production `serve()` passes `std::io::stderr()` as the diagnostic sink — stdout stays the
    protocol channel exclusively, matching acceptance box 4.
  - Did not reuse `truncate_on_char_boundary`: per the acceptance's own resolution ("malformed
    lines get length + parse-error only, not content"), the diagnostic embeds zero raw bytes from
    the line, so there is no content to truncate. This is a strictly stronger safety stance than a
    bounded prefix.
  - New unit tests in `crates/flux-plugin/src/lib.rs`'s `mod tests` (in-process, no subprocess):
    `tests::serve_io_skips_single_malformed_frame_and_answers_next_request` and
    `tests::serve_io_terminates_after_consecutive_malformed_frames_but_valid_frame_resets_counter`.
    Both drive `serve_io` over an in-memory `std::io::Cursor` reader and `Vec<u8>`
    writer/diagnostic sinks. Confirmed failing-first by temporarily reverting `serve_io`'s body to
    the old bare-`continue` behavior (signature unchanged) and re-running: both new tests failed
    for the expected reason (0 diagnostics emitted; trailing request answered when it should have
    been unreachable), then the fix was restored (verified byte-identical to the pre-revert file).
  - Gate (package-scoped, all green):
    `cargo build -p flux-plugin`; `cargo test -p flux-plugin` (63 passed + 1 ignored unit, 5/5
    `tests/host.rs` integration tests green, confirming no regression to the existing plugin
    protocol suite); `cargo clippy -p flux-plugin --all-targets -- -D warnings` (clean);
    `cargo fmt --all` followed by `cargo fmt --all -- --check` (clean; diffed the fmt output
    against a pre-fmt copy of `lib.rs` — byte-identical, so fmt changed nothing in this file or any
    other concurrently-edited file in the tree).

## Notes
- Deliberately NOT adding size/timeout enforcement guest-side: the guest reads from its parent via
  `BufRead::read_line`; the trust boundary and resource guards live host-side (per the envelope
  invariant). This is a diagnosability fix, not a security boundary.
