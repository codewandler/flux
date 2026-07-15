# Harness hardening — guard the untrusted-input surface

## Why

A full-workspace code review (2026-07-15) found the codebase mature and well-gated, but surfaced a
coherent class of gaps: a prompt-injected model or hostile fetched content can reach
secret-exfiltration / SSRF primitives in a single unapproved call, and the interpreter plus several
network/tool paths enforce no resource bounds at the point of execution. This epic collects those
findings into individually-closeable stories so the harness fails closed against untrusted input.

## Context: what is already solid

The review confirmed strong baselines that these stories build on, not replace:

- **CI gates** — `fmt`, `clippy -D warnings`, the `flux-codegate` L0–L6 layering lint, and the
  raw-process-spawn scanner (spawns only via `flux-system::build_command`/`build_tokio_command`).
- **`cargo audit`** — no vulnerabilities (3 low advisories: `paste`/`ttf-parser` unmaintained,
  `lru` `IterMut` unsoundness — resolve on the next `cargo update`).
- **Verified-solid subsystems** — provider codecs (shared mapper, corpus-hardened streaming),
  the `flux-system` spawn seam (argv-only, symlink-chasing write-jail, env allowlist),
  `flux-policy`/`flux-secret` (default-deny, non-leaking `Debug`), **A2A multi-tenant isolation**
  (realm-checked everywhere; no cross-principal read found), `flux-pg` (`is_ident` schema guard),
  and the flux-lang optimizer/DAG-scheduler (soundness-reasoned, property-tested — no deadlock or
  ordering bug). The findings below are edge escapes and missing guardrails, not structural failures.

## Two root-cause themes

**1. Model-reachable exfiltration surface.** `network.fetch` is a default local grant, so a single
tool call — chosen by a prompt-injected model — can move a secret off-box. The primitives:
`http.request` resolving any `$secret` env var to any URL (and then *scrubbing it from the
transcript* so the operator never sees it leave); the egress guard vetting a resolved IP while
reqwest re-resolves at connect (DNS-rebinding → cloud metadata); `sqlite_query` reading any on-disk
DB outside the workspace jail at `Risk::Low`; plus credential-leak vectors (`OAuthToken` `Debug`,
inline-URL creds bypassing the cross-plugin gate and rendered to the model).

**2. No resource governor at the execution boundary.** Every safety cap lives in the analyzer pass;
`execute_flow`/`execute_plan` re-enforce none. So untrusted `.flux` / LLM-emitted plans reach
uncatchable stack-overflow aborts (unbounded recursion in the parsers, the `expr` evaluator, and
recursive composite ops), CPU pinning + OOM (`loop`/`each` with no step/time budget, unbounded
store/event/transcript growth), and host OOM in tool/network paths (fs read slurping before the size
cap, no provider HTTP timeout wedging every turn, unbounded A2A queues, plugin QuickJS/PG-auth/CDP
buffers).

## Finding inventory → story map

Severities are the review's synthesized judgement; the two Criticals were reproduced/verified
in-session. Detail (impact + fix) for each item lives in the linked story.

| Story | Sev | Finding(s) | Anchor |
|---|---|---|---|
| C-76 | Critical | `http.request` `$secret` → arbitrary env var to arbitrary URL, hidden by the redactor | `flux-web/src/http.rs:220` |
| C-77 | Critical | DNS-rebinding: guard vets IP, reqwest re-resolves at connect | `flux-system/src/net.rs:114` |
| C-78 | High | `sqlite_query` reads any on-disk DB outside the jail at `Risk::Low` | `flux-tools/src/extra.rs:264` |
| L-81 | High | Unbounded recursion (parsers, `expr`, composite calls) → SIGABRT | `flux-lang/src/expr.rs:510`, `runtime.rs:434`, `parser.rs:1102` |
| L-82 | High | No execution budget: `loop`/`each` busy-spin + unbounded store/event/transcript | `flux-lang/src/runtime.rs:2313` |
| C-79 | High | fs `read`/`grep`/`file_stat` slurp whole file before the size cap → OOM/hang | `flux-tools/src/lib.rs:544` |
| C-80 | High | Default provider HTTP client has no timeout → one stall wedges every turn | `flux-provider/src/lib.rs:343` |
| C-81 | High | One unknown/corrupt event row aborts the whole stream read (closed enum) | `flux-events/src/store/mod.rs:55` |
| C-82 | Medium | `OAuthToken`/`Refreshed` `Debug` leak; inline-URL creds bypass gate + rendered to model | `flux-credentials/src/lib.rs:82`, `flux-capabilities/src/endpoint/broker.rs:750`, `.../ops.rs:210` |
| C-83 | Medium | A2A DoS: unbounded turn queue, `id` echo amplification, un-swept push map, std-mutex across DB I/O | `flux-server/src/a2a.rs:838,1724,1373,364` |
| C-84 | Medium | Plugin/web DoS: QuickJS no mem/stack limit, unbounded PG-auth body, CDP frame/channel, `looks_like_html` UTF-8 panic | `flux-plugin/src/hooks.rs:73`, `.../pg.rs:313`, `flux-web/src/cdp.rs:117`, `.../fetch.rs:278` |
| C-85 | Medium | Tool-mutation guards: `git_checkout` pathspec data-loss, `edit` empty-`old_string` corruption | `flux-tools/src/lib.rs:2388,745` |
| C-86 | Medium | Config fails open: `deny_unknown_fields` missing on `[server]`/`[limits]`/`[workspace]`/`[permissions]` | `flux-config/src/lib.rs:392` |
| L-83 | Medium | `memo`/`once` cache-hit keyed on symbol name/label, not op+input provenance → stale/wrong value | `flux-lang/src/runtime.rs:2061,3061` |
| C-87 | Medium | Growth: `prune_empty` deletes durable-fact sessions, caller-id idempotency race, unbounded projections, evidence-log O(N) clone/turn, per-session maps never evicted | `flux-events/src/store/sqlite.rs:478`, `.../postgres.rs:578`, `flux-flow/src/engine.rs:395,133` |

Lower-severity hardening (own the fix inside the nearest story above): empty shared-secret bypass,
server error-text leakage, A2A client adopting a card endpoint unguarded, webhook first-`Authorization`
smuggling, plugin `PATH`/`LD_*` env override, token grant/refresh following redirects, Vault
`data_url` traversal, browser non-http(s) subrequest passthrough, OAuth state-less callback, and
substring-only redaction. See the review write-up for the full list.

Code-quality items (duplication, god-functions, error-stringify idiom) are tracked separately as
**C-88** (outside this epic — quality, not hardening).

## Approach

- Each story ships with a **failing-first test** that reproduces the gap (exfil attempt refused,
  deep-recursion input returns a bounded error instead of aborting, over-cap read rejected, etc.).
- Prefer **fail-closed** defaults and enforcement **at the execution/IO boundary**, not only in the
  analyzer — the recurring root cause is trusting an upstream check.
- No clean-cutover concerns here: these are additive guards. Keep the `flux-codegate` layering + the
  spawn seam intact.

## Priority

C-76 and C-77 are `ready` (P1/P2) — verified, single-call-reachable secret exfiltration. The rest are
`backlog` pending triage against the current async-live-datasource focus; C-78 and L-81 are the
strongest candidates to promote next.
