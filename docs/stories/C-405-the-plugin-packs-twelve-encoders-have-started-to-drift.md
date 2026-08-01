---
id: C-405
title: "The plugin pack carries twelve private percent-encoders and one has already drifted"
pillar: Core
status: ready
priority: 13
epic: connector-platform
areas: [plugins, flux-plugin]
note: "found by C-313's census — drift observed, not predicted. `plugins/gitlab`'s `enc` omits `~` from the unreserved set, so it emits %7E where the other eleven emit `~`. The nested workspace structurally cannot delegate to flux-core, so the fix is a shared encoder in host-kit — a protocol-line change owing a pack release"
---

# The pack's twelve encoders, and the one that drifted

## Goal

Give the nested `plugins/` workspace **one** RFC 3986 percent-encoder, so the twelve private copies
stop drifting apart.

[C-313](C-313-url-encoder-consolidation-and-key-pinning.md) consolidated the root workspace onto
`flux_core::percent_encode_component` and then censused the rest of the tree. The root is now clean.
The nested workspace is not, and it **structurally cannot** delegate to `flux-core`:

- `plugins/` is excluded from the root workspace;
- nothing in it depends on `flux-core`;
- `host-kit` — the crate whose stated job is to re-export the vocabulary a plugin needs — exposes no
  encoder at all, only `join_url`.

So twelve plugins each carry their own. ⚠ **This is not a hypothetical maintenance worry; the drift
already happened**:

`plugins/gitlab/src/operations/mod.rs:20` — `pub(super) fn enc` omits `~` from its unreserved set,
so it emits `%7E` where the other eleven emit a literal `~`. RFC 3986 lists `~` as unreserved, so
percent-encoding it is legal but non-canonical — two plugins hitting the same vendor path with the
same input produce different URLs.

## Acceptance

- [ ] **Failing-first**: a test that shows two pack plugins encoding the same input differently
      today — the `~` case is the concrete one — failing at the merge base.
- [ ] `host-kit` exposes one percent-encoder, and all twelve copies delegate to it. No plugin keeps
      a private one.
- [ ] The encoder's unreserved set is **pinned by a test**, not just written down: `-`, `_`, `.`,
      `~` pass through, everything else is `%XX` uppercase. C-313's lesson applies directly — the
      root-workspace copy was byte-identical to `flux-core`'s and still nobody would have noticed if
      it had not been.
- [ ] Behaviour matches `flux_core::percent_encode_component` exactly, so the root and nested
      workspaces cannot disagree. State how that is enforced rather than merely intended — the two
      cannot share code, so something must compare them.
- [ ] ⚠ **The version decision is part of this story.** `host-kit` is on the independently-versioned
      protocol line; adding a public function is additive but still owes a bump, and a
      `plugins/` change owes a **plugin-pack release** separate from the flux release. Run
      `BASE=v0.45.0 bash scripts/check-crate-versions.sh` and get it to EXIT=0, and say in the report
      that a pack cut is owed.
- [ ] Gate green in **both** workspaces, including `cargo build --manifest-path plugins/Cargo.toml
      --workspace --locked`.

## Notes

- The `~` difference is legal-but-non-canonical, so nothing is *broken* today. The story is about the
  twelve-copy structure that produced it: one drifted silently, and nothing would catch the next.
- Whoever takes this should check whether `join_url` — host-kit's existing URL helper — is the right
  home, or whether a sibling `urlencode` module reads better beside it.
- Related unpinned surface, found by the same census and left for whoever builds the fixture:
  `expand_endpoint_template` in `crates/flux-plugin/src/host.rs` has **no test of any kind**, so its
  encoding is unpinned exactly as the query key was. C-313 recorded that a pin is feasible without
  unsafe env mutation (`resolve_config` reads env via `self.system().env(key)`, not `std::env`) but
  needs a fake `PluginSystem` that no test in that file has today.

## Progress

- Filed 2026-08-01 from C-313's census, which found the drift by enumerating rather than by hitting a
  bug.
