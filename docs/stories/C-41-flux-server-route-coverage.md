---
id: C-41
title: flux-server integration coverage — A2A routes, SSE framing, TTL pruning end to end
pillar: Core
status: done
design:
epic:
note: 9 inline tests for the highest-blast-radius surface; core auth invariants ARE pinned (bearer-required, loopback-bind guard) — this deepens route/stream coverage, it does not close a known hole
---

# flux-server integration coverage — A2A routes, SSE framing, TTL pruning end to end

## Goal
flux-server is the surface where a regression means an open RCE listener, and it carries 9 inline
tests (8 in lib.rs, 1 in a2a.rs). The auth core is already pinned
(`auth_required_when_token_configured`, `no_token_configured_is_pass_through`,
`unauthenticated_bind_is_loopback_only`) — what's thin is the behavior *behind* auth: the A2A
JSON-RPC routes, SSE event framing, and session lifecycle through the HTTP surface. Add a
`tests/` integration suite exercising the router end to end (mock provider, no network beyond
loopback/axum test harness).

## Acceptance
- [x] `crates/flux-server/tests/` exists and covers: `tasks/send` (completed shape),
      `tasks/sendSubscribe` (SSE framing: working events then final), the discovery card
      (auth-exempt), and a malformed-JSON-RPC request (error shape, not a 500/panic).
- [x] C-18 TTL pruning exercised through the HTTP surface (expired session swept at next mint;
      the C-29 queued-session retention holds).
- [x] No production code changes expected; if a test surfaces a defect, it gets its own
      failing-first fix (possibly its own story if non-trivial).

## Progress
- **Done (2026-07-07).** Added `crates/flux-server/tests/` — a real integration suite driving the
  production axum `Router` end to end (`tower::ServiceExt::oneshot`), built only against
  `flux-server`'s **public** API (`router`/`CardInfo`), plus a shared `tests/support/mod.rs` fixture
  module (mock `Provider` impls + a duplicated `test_engine` builder, since the crate's own
  private test helpers aren't visible from an external integration-test crate). No production code
  changed; no `Cargo.toml` edit was needed (`tower`/`flux-provider`/`flux-system`/`flux-tools` were
  already in the dep tree). 9 new tests (10 counting the intentionally-`#[ignore]`d one below),
  alongside the pre-existing 9 inline tests — all green.
  - **`tests/a2a_message_send.rs`** — `message/send` completes synchronously with `status.state ==
    "completed"`, the request's `contextId` echoed, and the reply text riding the completed
    status message; plus a missing-params `-32602` negative case.
  - **`tests/a2a_message_stream.rs`** — `message/stream` SSE framing, asserted on parsed
    `data:`-frame JSON structure (not substring matching): `[working-ack (no message),
    working (turn's answer), completed (final: true, no message)]`, task/context id continuity
    across every frame. Documented a real architecture finding along the way: the flux-flow engine
    compiles a whole turn before touching the sink (`FlowEngine::run_turn_cancellable` calls
    `sink.text_delta` exactly **once**, with the turn's final answer — `crates/flux-flow/src/engine.rs:298`),
    so even a provider streaming multiple raw `TextDelta` chunks surfaces as a single SSE "working"
    frame, not one frame per chunk. The test's `MultiDeltaProvider` fixture and assertions reflect
    this real shape rather than an assumption.
  - **`tests/discovery_card_auth_exempt.rs`** — both discovery aliases (`/.well-known/agent.json`,
    `/.well-known/agent-card.json`) plus `/health` are reachable with **no** `Authorization` header
    even when a token is configured (card shape asserted: `name`, `capabilities.streaming`, `url`
    ending `/a2a`); `/a2a` and `/sessions/:id` 401 with no/wrong token and succeed (200 / 404, never
    401) with the correct one — proving the 401s are the auth gate specifically.
  - **`tests/malformed_json_rpc.rs`** — an unrecognized method and a wrong `jsonrpc` version both
    yield clean JSON-RPC error envelopes (`-32601`, `-32600`, HTTP 200 — never a 500/panic). A
    syntactically-garbage body is pinned to its **actual** current behavior (HTTP 400, plain text,
    not JSON-RPC-shaped) in a passing test — see the Notes defect below — plus a second,
    intentionally-`#[ignore]`d failing-first test asserting the *ideal* spec-conforming shape
    (`-32700 Parse error` envelope), ready for a follow-up to un-ignore once fixed.
  - **`tests/a2a_ttl_pruning.rs`** (C-18) — TTL pruning driven purely through `POST /a2a` and the
    production `[server] a2a_session_ttl_secs` config knob (the crate's own `A2aTtl`/
    `router_with_ttl` test seam is `pub(crate)`, invisible here): a session minted, aged past a 1s
    TTL for real, then swept by the *next* mint's lazy sweep (verified via `engine.events.info`
    going `Err`), while the just-minted second session survives its own sweep. A second test pins
    `a2a_session_ttl_secs = 0` disabling pruning through the same HTTP path. Since the production
    TTL knob is read from the **process** cwd at router-build time, this is the one test file that
    manipulates `std::env::set_current_dir` (behind a `static Mutex` + an RAII restore guard, since
    cwd is global process state) — isolated to its own test binary/process so it can't race other
    `flux-server` test files.
  - **C-29 (queued-session-survives-a-concurrent-sweep) reachability, honestly**: this property is
    **not** independently reachable through black-box HTTP against a single router instance. `send`/
    `subscribe` acquire the single-turn `turn_gate` and hold it for the mint *and* the entire turn
    (`crates/flux-server/src/a2a.rs`'s `create_a2a_session` doc comment), so within one `Router` a
    second request's mint can only ever begin strictly after the first request's turn has fully
    finished — there's no window where two mints (and so two sweeps) are actually concurrent.
    Reproducing the hazard needs calling the mint function directly, ahead of/independent from the
    gate — exactly what the crate's own `a2a::tests::queued_session_survives_concurrent_sweep_while_gate_held`
    already does with white-box access to `create_a2a_session`/`TurnGate` (both `pub(crate)`,
    invisible to this suite). That inline test (still green, reran as part of this story's gate)
    remains the regression pin for C-29; nothing here weakens or duplicates it. This is exactly the
    "say so rather than force it" case the story anticipated.
- **Gate:** `cargo test -p flux-server` — 18 passed, 1 intentionally ignored, 0 failed (9 pre-existing
  inline + 9 new integration, across 5 new test binaries). `cargo clippy -p flux-server --all-targets
  -- -D warnings` — clean. `cargo fmt -p flux-server -- --check` — clean. No `cargo build
  --workspace`/`cargo test --workspace` run (out of this story's package-scoped gate, and other
  agents were concurrently editing unrelated crates in this tree).

## Notes
- Honest framing from the 2026-07-07 survey: this is coverage depth on a high-blast-radius
  surface, not a known-hole fix. Some of these paths are exercised indirectly by
  scripts/smoke-live.sh step 5 — hermetic in-crate coverage removes the dependency on the live
  gate for them.
- **Defect surfaced, not fixed (follow-up candidate):** a syntactically-invalid `/a2a` request body
  never reaches `a2a_handler` — axum's `Json<JsonRpcRequest>` extractor
  (`crates/flux-server/src/a2a.rs:207`) rejects it first, so the response is axum's generic `400
  Bad Request` (plain text) rather than a JSON-RPC-shaped `{"jsonrpc":"2.0","error":{"code":-32700,
  ...}}` envelope every other error path in the same handler produces. Not a 500, not a panic (the
  story's literal Acceptance bar), but a real gap against a strict JSON-RPC 2.0 server contract.
  Closing it needs a custom extractor or a rejection-mapping layer — more than the "small, obvious"
  fix bar for this coverage-only story, so it's left as: a passing test
  (`garbage_body_never_500s_or_panics`) pinning today's actual (safe) behavior, plus an
  `#[ignore]`d failing-first test (`garbage_body_yields_a_json_rpc_parse_error_envelope`, both in
  `crates/flux-server/tests/malformed_json_rpc.rs`) pinning the ideal shape for whoever picks this
  up.
