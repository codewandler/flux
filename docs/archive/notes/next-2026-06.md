# next.md — Flux improvement backlog

Checkbox-based TODO list. Tick a box when implementation + test + clippy pass.
Ordered by agent-ergonomics impact. Each item carries enough detail to implement without further investigation.

---

## Tier 1 — Correctness bugs (silent, fix first)

- [x] **T1-1 `race` true concurrency** — DONE
  - `Node::Race` branches now run concurrently via `tokio::select!`; winning branch's buffer is replayed; remaining tasks cancelled. Timeout via `tokio::time::timeout`. ✓

- [x] **T1-2 `try` catch binding (`as`)** — DONE
  - `Node::Try` arm binds error string to `catch` symbol before executing handler. ✓

- [x] **T1-3 `confirm` deny error** — DONE
  - `Node::Confirm` arm returns `Error::ConfirmDenied` immediately on deny, never dispatches body. ✓

- [x] **T1-4 `throttle`/`debounce` name field** — DONE
  - `Node::Throttle` keys FlowStore bucket on `__throttle_bucket_{name}`. ✓

---

## Tier 2 — Ergonomics gaps (agents reach for `bash` unnecessarily)

- [x] **T2-1 `repeat` collect** — DONE
  - `Node::Repeat` arm accumulates per-iteration ValueIds; binds as `Value::List` after loop. ✓

- [x] **T2-2 `assert` typed error** — DONE
  - `AssertFailed(String)` variant in `flux-core/src/error.rs`. Raised in `runtime.rs`. `Node::Retry` arm skips retry on `Error::AssertFailed` and `Error::ConfirmDenied` (fatal errors). ✓

- [x] **T2-3 `{sym}` substitution unification** — DONE
  - Substitution logic extracted into `crates/flux-flow/src/interp.rs` (`pub(crate) fn interpolate`). `task`/`write`/`append` tool impls route through it. ✓

- [x] **T2-4 `parse` node** — DONE
  - `Node::Parse { value, as_type }` in `ast.rs`. Pure arm in `runtime.rs` coerces via `str::parse`/`serde_json::from_str`. Analyzer validates `as_type` ∈ {`f64`, `i64`, `bool`, `json`, `string`}. `DOCUMENTATION.md` updated. ✓

- [x] **T2-5 node `id` + structured errors** — DONE
  - Optional `id: Option<String>` field on `Node`. Analyzer auto-assigns `"node_0"`, `"node_1"` etc. Every `execute_call` error wrapped as `[{node_id}:{op}] {err}`. ✓

---

## Tier 3 — Missing ops / infrastructure (low risk, high payoff)

- [x] **T3-1 `grep` regex flag** — DONE
  - `grep` op uses `regex::Regex` by default; `literal: true` for substring. ✓

- [x] **T3-2 `temp_executor_with_approver` test helper** — DONE
  - `crates/flux-flow/src/testutil.rs` created (cfg(test)). `DenyApprover` + `AllowApprover` + `temp_executor_with_approver` available to all runtime tests. ✓

- [x] **T3-3 `cargo doc` surfaces DOCUMENTATION.md** — DONE
  - `#![doc = include_str!("../DOCUMENTATION.md")]` on `crates/flux-flow/src/lib.rs`. ✓

---

## Tier 4 — Large features

- [x] **T4-1 `goal` node** — DONE
  - Fields: `judge: "bash"|"assert"|"model"`, `max: u32`, `attempt: Vec<Node>`, `context: Vec<Node>`, `bind: Option<SymbolName>`. Runs attempt body, evaluates judge, feeds context back on failure, retries up to max. `DOCUMENTATION.md` updated. ✓

- [x] **T4-2 `watch` node** — DONE
  - Fields: `source: String`, `debounce_ms: u64`, `for_ms: u64`, `body: Vec<Node>`. Layered on loop+debounce. Analyzer rejects missing `for_ms`. `DOCUMENTATION.md` updated. ✓

- [x] **T4-3 `checkpoint` node** — DONE
  - `checkpoints` table in `crates/flux-flow/src/state.rs`. Fields: `name: String`, `body: Vec<Node>`. Snapshots symbol table before body; resume skips to next node after last committed checkpoint. `DOCUMENTATION.md` updated. ✓

- [x] **T4-4 flux-eval M2 metrics** — DONE
  - `Chunk::Usage` tokens captured into `RunResult`. `--output json` flag. Pain-point mining expanded: tool-not-found, bad-arg-schema, empty-result-rephrase, max-iter hit, read→edit churn, timeout. ✓

- [x] **T4-5 hot-reload `--dev` / `flux_restart` tool** — DONE
  - `flux_restart` built-in in `crates/flux-tools/src/restart.rs`. `Error::RestartTriggered(RestartContext)` in `flux-core`. `--dev` flag in `flux-cli`. Session continuity via SQLite WAL + synthetic `tool_result` before resume. ✓

---

## Attack order

1. ~~T1-4 + T2-1~~ — done ✓
2. ~~T1-2 + T1-3~~ — done ✓
3. ~~T3-1~~ — done ✓
4. ~~T2-2~~ — done ✓
5. ~~T3-2~~ — done ✓
6. ~~T1-1~~ — done ✓
7. ~~T3-3~~ — done ✓
8. ~~T2-4~~ — done ✓
9. ~~T2-5~~ — done ✓
10. ~~T2-3~~ — done ✓
11. ~~T4-4~~ — done ✓
12. ~~T4-1 → T4-2 → T4-3 → T4-5~~ — done ✓
