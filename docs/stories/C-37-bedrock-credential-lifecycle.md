---
id: C-37
title: Bedrock credential lifecycle — expiry re-resolution + lazy chain constructor
pillar: Core
status: done
epic: aws-bedrock-provider
design: docs/designs/subscription-providers-and-cost.md
note: creds carry expiration (IRSA/Pod-Identity ISO, SSO epoch-millis), apply() re-resolves through the stored resolver inside a 5-min window, region pinned at construction and coerced onto every resolve, bedrock_with_chain is the sync lazy chain constructor
---

# Bedrock credential lifecycle — expiry re-resolution + lazy chain constructor

## Goal
Close the seam C-09 left open: `BedrockCredential` resolves its creds **once at construction and
never again** (`resolver` is `#[allow(dead_code)]`), so a long-running process on temporary
credentials (SSO, IRSA, EKS Pod Identity — all STS sessions that expire after hours) goes dark at
expiry until restart. The L1 401-force-refresh path doesn't help: Bedrock returns **403** for
expired AWS creds and `BedrockCredential` has no `token_source()`. Make the stored resolver live —
re-resolve on expiry — and add a **sync, lazy** chain constructor so sync call sites (sub-agent
provider factories, downstream servers) can build a chain-backed provider without async plumbing
or the `materialize_chain_into_env` snapshot (which freezes temporary creds in env and defeats
refresh by chain-step-1).

## Acceptance
- [x] `BedrockCreds` carries `expiration: Option<DateTime<Utc>>`, populated by every source that
      reports it: IRSA (STS XML `<Expiration>`), EKS Pod Identity (JSON `Expiration`), SSO
      (`roleCredentials.expiration` epoch-millis); `None` for static env. Unparseable stamps
      degrade to `None` (today's never-refresh behavior), never an error.
- [x] `BedrockCredential::apply()` re-resolves via the stored resolver when creds are absent or
      within a 5-minute expiry window, and caches the result; fresh creds are **not** re-resolved.
      Failing-first: counting-resolver tests — fresh creds → 1 resolve across two applies
      (`fresh_creds_resolve_once_across_applies`), near-expiry creds → a re-resolve per apply
      (`near_expiry_creds_re_resolve_on_apply`). The `#[allow(dead_code)]` on `resolver` is gone.
- [x] **`bedrock_with_chain(model_id)` — sync + lazy**: constructs without resolving (counting
      resolver sees 0 calls at construction via the equivalent lazy `BedrockCredential`); the
      first `apply()` resolves the default chain. The request region is **pinned at construction**
      from `AWS_REGION`/`AWS_DEFAULT_REGION` (default `us-east-1`) so `endpoint()` works before
      first resolution, and every resolved cred is coerced to the pinned region so the URL host
      and the SigV4 scope always agree (`lazy_endpoint_uses_pinned_region_before_resolve`).
- [x] Gate green: `cargo build/test -p flux-providers`, `clippy -D warnings`, `fmt`,
      `cargo test -p flux-codegate`.

## Progress
- Identified while wiring the babelforce ai-agent-platform deployment onto Bedrock via EKS Pod
  Identity: the injected session creds rotate (~hours), so resolve-once means a dead provider
  after the first expiry in an always-on server.
- Landed: `expiration` capture in all three temporary-cred sources; expiry-aware re-resolution in
  `apply()` (resolve outside the creds `Mutex` — a concurrent double-resolve is benign, the chain
  sources are idempotent); region pinned at construction (eager constructors pin from the first
  resolved creds, the lazy constructor from env) and coerced onto every resolved cred;
  `bedrock_with_chain` as the sync lazy constructor over `AwsChainResolver`.
