---
id: C-455
title: "`docs/ecosystem.md` says flux-exchange \"binds no port, holds no credential, and answers no request\" — three of those shipped"
pillar: Core
status: ready
priority: 6
design: docs/designs/docs-completeness.md
epic: docs-completeness
areas: [docs]
note: "found while comparing localities. The paragraph was true at flux-exchange v0.4.0 and is false at v0.11.0 — sign-in, the credential store and `invoke` all exist. Only channels, `subscribe`, stored workflows and execution records remain accurate"
---

# A staleness that reads as a capability claim in reverse

## Goal

Correct `docs/ecosystem.md`'s description of flux-exchange to what v0.11.0 actually does.

## The finding

`docs/ecosystem.md:127-131` says `cargo run` in flux-exchange *"prints which runtimes each deployment
shape would serve, and exits. It binds no port, holds no credential, and answers no request. Sign-in, the
credential store, `invoke`, `subscribe`, channels, stored workflows and execution records are all
described above and none of them are built."*

At v0.11.0: sign-in is complete (OIDC, back-channel code redemption, id-token signature verified against
the provider's keys), the credential store exists with per-tenant connections that can be created,
listed, rotated and deleted, and **`invoke` runs** — gated by a grant since X-13.

⚠ Only **channels**, **`subscribe`**, **stored workflows** and **execution records** remain unbuilt.

## Acceptance

- [ ] The paragraph reflects v0.11.0, and names the version it was checked against **with a date**, since
      it will go stale again.
- [ ] ⚠ **Check the neighbours in the same pass.** `docs/ecosystem.md` is the derived summary of
      `docs/designs/ecosystem.md`; that design says *"Where they disagree, this one is the argument and
      that one is the summary — **fix both**."* A one-sided fix reproduces the drift.
- [ ] The still-accurate half stays explicit. ⚠ A correction that quietly drops *"none of them are built"*
      without stating which four remain unbuilt trades an understatement for an overstatement, which is
      the worse error for a page about who owns what.
- [ ] ⚠ flux-exchange's own docs carry the same drift in the other direction — its `README.md:11` says
      "v0.9.0" while `Cargo.toml` and `CHANGELOG.md` say 0.11.0, and its `docs/roadmap.md` still says
      v0.4.0 and *"it still executes nothing"*. Out of scope for this repo; record it so someone raises
      it there.

## Notes

- Small, and worth doing precisely because it is the kind of gap [C-442](C-442-peer-docs-gap-audit.md)
  classifies as *covered but wrong* — the most expensive kind, since a reader trusts it.

## Progress
- Filed 2026-08-02.
