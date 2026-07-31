---
id: D-214
title: "Re-point the Zendesk reference flow at the flux-connectors Tool pack"
pillar: Agent
status: blocked
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [flux-cli, docs, website]
note: "flux's half is landed — the pack already speaks this flow's exact op names, and both ends are now pinned by test. BLOCKED on the last bullet only: two flux-connectors gaps (no zendesk `authority`; `{subdomain}` unresolved) keep a live run impossible"
---

# Re-point the Zendesk reference flow at the flux-connectors Tool pack

## Goal

Make `examples/zendesk.triage.flux` runnable again by pointing its `zendesk.*` operations at the
flux-connectors Tool pack, and unblock the three stories withdrawn with the plugin.

When `flux-plugin-zendesk` was removed in 0.38, the flow was retained deliberately as *"the authored
shape the replacement has to satisfy"*, with the note that **the op names are the part expected to
change and the flow structure is not**. This is that change.

## Acceptance

- [x] A host can register connector operations with
      ~~`ClientBuilder::try_register_pack(connector_pack::pack(&["zendesk"]))`~~ →
      **corrected:** `pack` takes three arguments, not one —
      `pack(&["zendesk"], http, credentials)`. The credential port is required rather than
      `Option`, deliberately: a pack buildable without one would let a host install connectors that
      send every request unauthenticated. The four operations the flow calls resolve —
      `zendesk.test`, `zendesk.ticket.show`, `zendesk.ticket.search`,
      `zendesk.ticket.comment.list` — proven by flux-connectors'
      `crates/connector-pack/tests/projection.rs`, which installs the pack and looks them up by
      name. **It cannot be proven from this repository**: `connector-pack` depends on `flux-spec`
      and `flux-runtime`, so flux depending back on it would resolve two incompatible copies of the
      `Tool` trait. See Notes.
- [ ] ~~`examples/zendesk.triage.flux` loses its NOT-RUNNABLE header and runs all four
      entrypoints~~ → **BLOCKED, and this is the only open bullet.** Two flux-connectors gaps make
      a live run impossible, and neither is a missing credential:
      (1) `providers/zendesk.toml` declares no `authority`, so there is no
      `tenants/<tenant>/<authority>/<credential>` address to resolve and the pack answers
      `NoCredentialAddress` at `execute` — only 7 shipped connectors declare one (C-37);
      (2) `base_url` is `https://{subdomain}.zendesk.com` and the pack does not resolve config, so
      the built URL carries the placeholder verbatim (confirmed by that repo's `tests/request.rs`,
      which asserts exactly that URL). Both **refuse** rather than sending a broken request, so the
      failure is diagnosable. The header was therefore *rewritten to name these two gaps* rather than
      removed.
      **Correction, 2026-07-31 (audit):** this bullet first attributed (2) to C-86/C-68. That was
      wrong. The `[[config]]` binding is **already declared** (`providers/zendesk.toml`, `subdomain`
      → `endpoint.subdomain`) and C-86's relevant acceptance is `[x]`. The live chain is **C-87**,
      which publishes `[[config]]` into `catalog.json` — the pack's only input, which today carries no
      `config` key — followed by a pack that applies it at install, which **no story in either repo
      owns**. Counts also corrected: 7 of **41** providers declare an `authority` (not "2 of 19"), and
      **43 of 232** operations carry a templated host (not "27 of 105"); the pack's own module docs at
      `crates/connector-pack/src/lib.rs:98-108` are ~2× stale and were the source of the bad figures.
- [x] **The flow's structure is unchanged** — and so are the names. The premise that "the op names
      are the part expected to change" turned out **false**: the pack projects `zendesk-test` to
      `zendesk.test` and `zendesk-ticket-comment-list` to `zendesk.ticket.comment.list`, which is
      what this flow already called. The pack was authored to this shape, so there was nothing to
      re-point. Not one line of the flow's body changed.
- [x] **It stays read-only** — asserted by
      `zendesk_reference_exposes_four_read_only_entrypoints` (pre-existing) against the module's own
      call graph. **The claim needed narrowing, not just a test:** `pack(&["zendesk"])` registers
      all seven catalogue operations, so the three writes *are* in the host's registry — the
      plugin era's separate `flux plugin call` surface is gone. What holds is that no entrypoint
      reaches one; keeping them unreachable at all is the host's approval decision. The docs now say
      this instead of implying registry absence.
- [x] Its provider-free coverage keeps passing — `crates/flux-eval/tests/zendesk_triage.rs` lowers
      the checked-in module and executes all four entrypoints against static fixtures.
- [x] **Failing-first test:** `zendesk_reference_calls_exactly_the_connector_pack_read_operations`
      pins the flow's `zendesk.*` set **exactly**, replacing a prefix check that admitted anything.
      Proven by renaming `zendesk.test` to `zendesk.tickets.list` in the real example: the new test
      failed, and `zendesk_reference_exposes_four_read_only_entrypoints` **passed** — so the gap it
      closes was real, not hypothetical. A name the pack does not project now reds flux's own gate,
      in the repository where the edit happens.
- [x] `D-200`, `D-201`, `D-202` move off `blocked`, and `D-199`'s dependency note is rewritten to
      what genuinely remains.
- [x] Both workspace gates are green. No smoke leg is claimed: with no credential *address* there is
      nothing to skip honestly, and reporting a skip would imply a credential is all that is
      missing.

## Notes

- The counterpart work is `flux-connectors` **C-113 – C-117**
  (`docs/designs/connector-tool-pack.md` in that repo). C-114, C-115 and C-116 are **done** there;
  C-117 (pack codegen + the drift gate) is still `ready` and is not a blocker for this story.
- **Where each half of the proof lives, and why it is not a choice.** `connector-pack` depends on
  `flux-spec` and `flux-runtime`. If flux took a dependency back on `connector-pack`, cargo would
  resolve *its* flux dependencies to the published crates while this workspace uses path crates —
  two distinct `dyn Tool` traits, so registration would not even typecheck. There is no cycle to
  break and no feature flag that helps: the "the four operations resolve" half is only provable in
  flux-connectors, and it is (`tests/projection.rs`). flux's half is the one that must fail *here*
  when someone edits the flow *here* — hence a call-graph pin over the module rather than a live
  registration test. Both ends now name each other in comments so neither can be deleted as
  redundant.
- The write-safety substance of D-201 **survived the migration** and did not need re-deriving:
  `zendesk-ticket-update` in the connector catalogue requires `updated_stamp` and carries
  `safe_update` as a constant `true` the caller cannot supply or drop, declares `conditional`
  idempotency, and declares its `{ ticket, audit }` response — `audit.events` being the only place a
  flat body Zendesk accepted, ignored and answered `200` to looks different from a real update.
  Comments default to internal notes; tag addition is additive.
- **The safety property to check when reviewing the pack, not to assume:** each generated Tool
  delegates to `HttpRequestTool::execute` directly, which **bypasses `Executor::dispatch`**. That
  means the inner call never consults `http.request`'s own `permission_subjects`
  (`crates/flux-web/src/http.rs:118`) or its `NetworkFetch` intent (`:126`). The connector Tools are
  required to mirror both. If they do not, installing a connector is a hole through this host's
  network policy — verify it on the pack rather than trusting the claim.
  **Verified, 2026-07-31, against the code and not the claim:** `Operation` implements both
  (`crates/connector-pack/src/tool.rs` — `permission_subjects` returns the built request URL, or the
  operation's declared hosts when the request cannot be built, so the call most likely to be
  malformed is still gated; `intents` raises one `NetworkFetch`/`ReadTarget` per subject). The
  subject is deliberately the **unauthenticated** URL, so a query-placed credential does not land in
  an approval prompt, a policy rule, or the evidence log — `permission_subjects` cannot fail and so
  cannot consult a redactor either. `tests/network_gate.rs` holds this over `catalog::operations()`
  — every shipped operation, not a sample. **No hole found.**
- Nothing here re-introduces a typed vendor plugin. The withdrawal decision stands; this is the
  generic layer that replaces it.

## Progress

- 2026-07-31 — flux's half landed. Nothing in the flow's body changed, because the pack already
  speaks its exact operation names; what landed is the pin that keeps it so
  (`zendesk_reference_calls_exactly_the_connector_pack_read_operations`, failing-first against the
  real example), plus four stale documentation blocks corrected — the example header,
  `docs/zendesk-triage.md` (which still documented `flux plugin add`/`flux plugin call` against a
  removed binary), `examples/README.md` (which still told a reader to `cargo build -p zendesk`), and
  the website examples page. The read-only claim was **narrowed** where it was overstated: the pack
  registers all seven operations, so registry-absence was never the guarantee. Stays `blocked` on the
  live-run bullet only, behind two named flux-connectors gaps.
