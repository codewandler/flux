# Session `s_251` post-mortem — `ctx`-pack eviction & endpoint-discovery alias resolution

**Epic.** Two compounding defects surfaced in stream `s_251` (`openai/gpt-5.5`, 2026-06-30): an
`endpoint.discover` "check db connectivity" turn that returned `{"candidates": []}`, and the
follow-up "analyze why it's broken" turn that **looped 7 iterations and was cancelled**. The two
defects are independent — either can fail on its own — but together they produced the death spiral.
This design captures the evidence and the fix shape for both child stories
([L-08](../stories/L-08-ctx-pack-eviction.md) and [D-33](../stories/D-33-endpoint-discovery-aliases.md)).

## Evidence base

All evidence is in `~/.flux/events.db` (stream `s_251`) and `~/.flux/flow.db` (values_store +
symbols). Reproduce with:

```sql
-- the 4 ctx_shrunk events (what the packer dropped each iteration)
SELECT payload FROM events WHERE stream='s_251' AND kind='run'
  AND json_extract(payload,'$.data.event')='ctx_shrunk' ORDER BY stream_seq;

-- the per-iteration plans (what the agent gathered, then lost)
SELECT data FROM values_store WHERE n IN (<plan output value ids>);
```

---

## Defect 1 — the `ctx` packer evicts the working set

### Symptom

The agent's turn-2 plans gathered real evidence each iteration (**28 `read`, 14 `grep`, 15
`kubernetes.endpoint.discover`, 7 `git_log` across the turn**) and bound it into named context packs
for the `ai.reason` step. Then `ctx_shrunk` fired **4 times** and each shrink dropped exactly the
code reads / discovery outputs / session evidence the flow had just collected, keeping only thin
`status`/`recent_commits` stubs. The `ai.reason` step then ran blind and returned *"No session logs,
discovery output, commit diffs, file contents, or line-numbered reads are included in the prompt."*
(seq 305). With no usable diagnosis, the loop's stop condition was never met → re-plan → re-gather →
re-shrink → re-starve. Seven iterations, then cancelled.

### Root cause — `build_ctx` in `crates/flux-lang/src/runtime.rs:3275`

```rust
let mut order: Vec<usize> = (0..members.len()).collect();
order.sort_by_key(|&i| std::cmp::Reverse(ranks[i]));   // visibility tier, stable
keep = vec![false; members.len()];
let mut running = 0usize;
for &i in &order {
    if running + sizes[i] <= b as usize { running += sizes[i]; keep[i] = true; }
    else { break; }                                       // ← hard break
}
```

Two properties:

1. **Greedy prefix-fill with a hard `break`.** The first member that overflows the char budget drops
   *itself and every subsequent member*, even members that would individually fit in the remaining
   budget.
2. **Priority is visibility tier only** (`vis_keep_rank`: Pinned 4 > Visible 3 > Hidden 2 > Expired 1
   > Private 0). Within a tier, declared order alone decides survival. Every bind the model makes is
   `Visibility::Visible`, so survival is effectively decided by declared order — which the model sets
   inconsistently.

### The data (shrink #4, `endpoint_discovery_analysis_pack_fresh`, budget 160,000)

Member byte sizes pulled from `symbols` → `values_store.bytes`:

| member | bytes | fate |
|---|---|---|
| `analysis_status_fresh` | 50 | **kept** |
| `analysis_recent_commits_fresh` | 917 | **kept** |
| `analysis_session_evidence_fresh` | **492,648** | dropped (triggers break) |
| `analysis_endpoint_ops_fresh` | 10,916 | dropped ← would fit in remaining ~159k |
| `analysis_k8s_db_discovery_fresh` | 24,517 | dropped ← would fit |
| `analysis_sql_endpoint_fresh` | 11,797 | dropped ← would fit |
| … 9 more members | … | dropped |

`session_evidence_fresh` (493k chars — a full session evidence dump) is declared before the code
reads. `running = 967`; next member is 493k → `967 + 493k > 160k` → **break**. Every subsequent member,
including the 11k/24k/12k code reads that would trivially fit, is dropped.

The agent tried to escape by raising the budget each iteration (60k → 90k → 120k → 160k). It never
helped: the oversized evidence bind is declared early and the packer hard-breaks, so survival needs
the budget to exceed 493k, which it never did. One shrink (#3, 120k) kept 9 code reads because *that*
pack happened to declare the code reads **before** the giant evidence bind — so survival is random
w.r.t. value.

### Fix shape (story [L-08](../stories/L-08-ctx-pack-eviction.md))

Two independent changes to `build_ctx`, both small:

1. **Drop-and-continue instead of hard break.** When a member doesn't fit, skip it and keep packing
   the rest (classic greedy bin-fill). The single oversized member that doesn't fit is dropped; the
   11k/24k/12k members after it survive. This alone would have fixed `s_251` turn 2.
2. **A value-aware keep priority** so the highest-information members survive when budget is tight.
   Options on the table (story picks one, proven by a failing-first test):
   - Let the model `pin` evidence-rich binds (raise their visibility tier) so they outrank status
     stubs; document that pinned members are never dropped to make room for plainer ones.
   - Rank within a tier by a smallness-favoring or declared-recency heuristic, so a 493k member
     doesn't preempt a 12k code read.
   - Give `ctx` a `drop:` allowlist so the model can mark "this is the one to shed first."

### Safety/non-regression

- `CtxShrunk` events are the durable record of an intentional shrink; the change keeps emitting them
  with accurate `kept`/`dropped`.
- No op semantics change — consuming ops (`ai.reason`, etc.) still read the already-bounded member
  list. The interpreter stays op-agnostic.
- A failing-first test: a pack with one oversized early member + several small late members, budget
  tight, must keep the small late members (today it drops them).

---

## Defect 2 — endpoint discovery doesn't resolve cluster/namespace aliases

### Symptom

Turn 1: `check db connectivity to backend database in namespace=latest cluster=dev`.

The agent's first plan called `kubernetes.endpoint.discover("dev", true, 5, null, "postgres",
"backend")` — passing `"dev"` as the context arg. That failed at seq 20:

```
provider error: kubectl get namespaces --context dev failed (exit 1):
Error in configuration: context was not found for specified context: dev
```

The real kubeconfig contexts are full EKS ARNs (e.g.
`arn:aws:eks:<region>:<account>:cluster/dev-<region>`, `current=true`). The agent then
**manually** recovered: it called `kubernetes.cluster.list()`, eyeballed the list, and hardcoded the
dev ARN as a literal in its next plan (seq 38). Resolution happened, but as model-driven recovery —
the op itself does no alias resolution.

**Critical:** resolving `dev` → ARN was *not* the root cause of the empty result. After the agent
hardcoded the correct ARN, discovery against the correct dev cluster *still* returned
`{"candidates": []}` (v_11417, v_11430, v_11444). So the empty result is a separate downstream
issue (Defect 2b).

### Defect 2a — no alias resolution in the kubernetes provider

`plugins/kubernetes/src/main.rs:486`:

```rust
fn ctx_args(input: &Value) -> Vec<String> {
    match opt_str(input, "context") {
        Some(c) => vec!["--context".into(), c.into()],   // passed literally to kubectl
        None => Vec::new(),
    }
}
```

`cluster_list` (line 543) dumps `kubectl config view` contexts but nothing maps a short alias like
`"dev"` to the ARN context that `contains("dev")`. The model had to do it by hand.

### Defect 2b — the broker never relays structured `cluster`/`namespace`

The agent-facing `endpoint.discover` op (`crates/flux-capabilities/src/endpoint/ops.rs`) takes
`product` / `query` (free text) / `limit`. The broker
(`crates/flux-capabilities/src/endpoint/broker.rs:150`) sends providers only
`{product, query, limit}` — it does **not** parse `cluster=` / `namespace=` out of the free-text
`query` into structured fields. So through the broker:

- the literal-namespace path is unreachable (the provider's `namespace` field is never set by the
  broker), and
- the context-resolution path is unreachable (the provider's `context` field is never set by the
  broker).

### Defect 2c — `namespace=latest` is ambiguous

`resolve_namespaces` (`plugins/kubernetes/src/main.rs:734`): when `namespace` is unset, it falls to
`wants_latest(input)` (line 752), which is `true` whenever `query` contains the substring
`"latest"`. So `namespace=latest` in the user's query was **not** treated as "the namespace named
latest" — it triggered the `latest_namespace` heuristic (line 769): `kubectl get namespaces`, pick
the newest by `metadata.creationTimestamp`. The agent's own `ai.reason` (seq 153) spotted this:
*"latest is commonly a version/tag/channel, not a Kubernetes namespace."*

`discover_db_secrets` (line 949) then searched *that* newest namespace's Secrets for a host-like key
+ a password key + a crossplane/RDS-ish name, found none, and returned `{"candidates": []}`.

### Fix shape (story [D-33](../stories/D-33-endpoint-discovery-aliases.md))

Depends on the in-flight **positional-args → kwargs** work in the kubernetes plugin: the structured
form should carry `cluster` and `namespace` as **named fields**, not positional args. Then:

1. **Provider alias resolution.** In `endpoint_discover` / `cluster_test`, when `cluster` (or
   `context`) is set but isn't an exact kubeconfig context, resolve it against `cluster_list`'s
   output: match by case-insensitive substring (`"dev"` → the ARN containing `/dev-`), or by the
   short suffix after the last `/cluster/`. Reject ambiguous matches (>1 hit) with a clear error
   naming the candidates, not a silent empty result. A `cluster.list` op already exists; the provider
   can call it internally.
2. **Broker query parsing.** `endpoint.discover` gains optional structured `cluster` / `namespace`
   fields (alongside `query`). The broker parses `cluster=<x>` / `namespace=<y>` tokens out of the
   free-text `query` (when the caller used the NL form) into the structured fields, then forwards
   `{product, cluster, namespace, query, limit}` to providers. This makes both the literal-namespace
   path and the context-resolution path reachable through the broker.
3. **Disambiguate `latest`.** When `namespace == "latest"` (literal) is ambiguous with the
   "newest namespace" heuristic, prefer the literal: if a namespace literally named `latest` exists,
   use it; only fall back to the newest-namespace heuristic when no literal `latest` exists *and*
   the caller asked for "latest" without an `=`. Surface in the op description which interpretation
   applied. (Or: retire the substring heuristic in favor of an explicit `latest_namespace: true`
   field, so "latest" as a literal name and "newest namespace" are never conflated.)

### Safety/non-regression

- Discovery stays read-only and weak-ref-only (URLs + credential *references*, never values). No
  change to the references-only IO invariant.
- A failing-first test: `endpoint.discover` with `cluster="dev"` resolves to the dev ARN context and
  runs against it (today it passes `"dev"` literally to kubectl and fails); and
  `namespace="latest"` against a cluster that *has* a literal `latest` namespace uses it (today it
  reinterprets as newest-namespace).
- The model-driven manual recovery (list → match → hardcode) should become unnecessary.

---

## Why both defects compound (the spiral)

Defect 1 made the reasoning step blind; Defect 2 meant the *correct* answer (discovery returns `[]`
because the namespace being searched is wrong / the alias didn't resolve) was never firmly landed on
either — the agent kept re-gathering discovery evidence that the packer then dropped before
`ai.reason` saw it. Fixing Defect 1 stops the spiral; fixing Defect 2 makes turn 1's original
question actually answerable. Both are needed for the "check db connectivity" path to be trustworthy.
