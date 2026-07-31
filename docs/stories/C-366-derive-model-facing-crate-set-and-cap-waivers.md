---
id: C-366
title: Derive the model-facing crate set from production registration, and cap direct-I/O waivers
pillar: Core
status: backlog
epic: structural-gate-blind-spots
design: docs/designs/structural-gate-blind-spots.md
note: "MODEL_FACING_OPERATION_CRATES is hand-written with no completeness test; the only backstop is scanned>50 against 110 files, so three of eight crates can be deleted and the gate stays green. 35 waivers, no cap, any non-empty text passes"
---

# Derive the model-facing crate set from production registration, and cap waivers

## Goal

Stop the direct-I/O gate's scope from being a list someone has to remember to update, and give its
escape hatch a budget.

## Acceptance

- [ ] A test binds `MODEL_FACING_OPERATION_CRATES` to the crates that actually contribute to the
      production catalog (or to `impl Tool` sites), and fails when a crate ships an op without being
      classified. Three crates ship `impl Tool` outside the list today.
- [ ] Deleting a crate from the classification reds the gate — the `scanned > 50` floor is replaced
      by a per-crate expectation.
- [ ] Direct-I/O waivers are capped and structured, following the pin census's `MAX_PIN_EXEMPTIONS`
      pattern; the current 35 are reviewed once as part of landing the cap.
- [ ] The cross-crate escape is addressed or recorded: FS/socket/HTTP/DB enforcement stops at a
      crate boundary, so a helper one crate lower is invisible. Only `Command` is enforced tree-wide.
- [ ] The pattern set's allow-list nature is documented — `ClientBuilder::new`, `Client::default`,
      `connect_timeout`, `TcpSocket`, `std::os::unix::fs::*` all fall outside it — and tied to the
      dependency graph rather than to remembered spellings.
- [ ] The two lexical eval-exemption guards named in C-351 are folded in: neither enumerates callers.

## Progress

- 2026-08-01 — mutations 3, 5, 6, 9, 10, 11 from the design doc's table.

## Notes

- C-263's acceptance deliberately chose "exhaustively checked classification" over "derived from
  registration". This story revisits that choice with evidence about how it drifts.
