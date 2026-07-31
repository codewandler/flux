# Agent change recovery and provenance — 2026-08-01

## Context

Five ledger claims concern what the harness can *know and undo* about its own changes. Validated
against the tree on 2026-08-01:

- **GIT-01 — reproduced.** The model-facing family has 15 git ops and no mixed-reset equivalent.
  `git_revert` appends an inverse commit; the only `git_reset` in the repo is `flux-eval`'s
  `Risk::Destructive` `reset --hard` + `clean -fd` round-restore, which destroys the patch. The
  seam a safe uncommit needs already exists (`TreePrecondition`, pinned by C-249's
  `git_tree_policy.rs`) — but that scan selects ops by matching `--abort`/`--hard`/`-fd`, so a
  `--mixed` op would not be selected and must be added to the pinned set explicitly.
- **GIT-02 — partially-reproduced.** Path-level attribution does exist durably: the pre-dispatch
  `tool_call` observation carries `{tool, subjects, caller, caller_kind}`. It is insufficient for
  the stated use — it records attempts rather than successes, has no blob or hunk identity, and the
  model-facing reader reads the in-process log only, so nothing survives a resume. Hunk ids are
  already deterministic content hashes; nothing writes an ownership record keyed on them.
- **GIT-03 — reproduced by construction.** `gather_safe` allows only `Read | Filesystem | Network`
  effects; every git op declares `Effect::Process`, so all four read-only observers are refused the
  evidence phase regardless of arguments. This is deliberate and documented in `flux-spec`. Note the
  review's own suggested fix ("classify by effect, not family") is already how it works — the
  coarseness is that `Effect::Process` conflates "spawns a subprocess" with "acts".
- **HAR-04 — reproduced in the product, historical-fixed in the library.** C-214/C-215/C-216 shipped
  a redacted, permission-scoped, off-by-default harness-history datasource. No shipped assembly
  enables it, no config key exists, and `HarnessKind::Flux` has no adapter — it returns an empty
  result indistinguishable from "nothing matched" while the honest `unsupported` signal never
  reaches the model.
- **HAR-06 — design-decision on read-back, reproduced on wording.** The write-only contract is
  intentional and owned by C-306. But H's stated condition was that claims say *accepted* rather
  than imply visibility, and the three results say `"pane 'x' open"` / `"updated"` / `"closed"` —
  while C-324 proves a dropped command still returns ok.
- **LANG-01 — not-reproduced as a language gap.** The collapsed form (`each` over a frame table with
  a nested `loop for …, every: …`) parses, lowers and *executes* correctly with the item bound
  through the nested loop; this was run during validation. The verbosity is a property of the one
  checked-in demo flow and the docs, not of Flux-Lang. Per H's explicit constraint, no
  `pane.sequence` op is proposed.

## Finding-to-story traceability

| Residual | Story |
| --- | --- |
| No history-preserving uncommit | C-383 |
| Write receipts are pre-dispatch and content-blind | C-384 |
| Staging cannot target only receipt-owned changes | C-385 |
| Read-only git observers cannot reach the evidence phase | C-386 |
| The harness-history datasource is unreachable in every shipped assembly | C-387 |
| flux-native history has no adapter, no compaction projection, and a silent empty result | C-388 |
| Pane results claim visibility the contract cannot support | C-389 |
| The one shipped timed-pane example teaches the verbose form | C-390 |

## Decisions

- **An undo that cannot preserve the patch is not a recovery path.** `git_uncommit` fails closed on
  every ambiguity — pushed HEAD, merge commit, root commit, index content not from HEAD — rather
  than guessing.
- **Ownership is proven by receipts, never by conversational memory.** A staging operation scoped to
  "this session" refuses when a path's hunks are not fully covered; it never widens to the file.
- **C-386 touches the pre-approval execution envelope and is not an ergonomics story.** Two mutually
  exclusive options are on the table: narrow the widening to ops holding the I1 fixed-argv exemption
  (which breaks C-191's stated correspondence and must move `flux-spec` in the same commit), or keep
  capture and return a *typed* "evidence unavailable until approval" state so exact-state claims
  become impossible. Pick deliberately; do not drift into it.
- **An unsupported query returns an unsupported result.** An empty list that means "no adapter" is
  the same class of laundering as a successful turn that means "the provider failed".
- **Wording is a contract surface.** While the pane channel is send-only, results state acceptance.
  This is independent of C-306's read-back decision and does not wait on it.
