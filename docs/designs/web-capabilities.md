# Design: Web capabilities — request · read · browse

**Status:** implemented (2026-07-09) · **Pillar:** Agent / Core · **Stories:**
[D-98](../stories/D-98-flux-web-crate-and-http-request-op.md) ·
[D-120](../stories/D-120-web-fetch-readable-markdown.md) ·
[D-121](../stories/D-121-browser-cdp-foundation.md) ·
[D-122](../stories/D-122-browser-page-digest.md) ·
[D-123](../stories/D-123-browser-actions-delta.md) ·
[D-124](../stories/D-124-browser-egress-interception.md)

## Why

flux's web surface grew bottom-up and it shows. Today there are exactly two model-facing web ops:
`web_fetch` (native, raw `[status]\n<body>` text capped at 256 KiB, **no** condensation, and a
bespoke per-tool private-net path — the caveat recorded on
[D-96](../stories/D-96-allow-private-net-cli-override.md)) and `web_search` (native, Tavily) plus
the `websearch` plugin. Integration plugins do vendor-scoped HTTP through the host `http.do`
capability, but nothing exposes *generic* HTTP to the model. And there is no browser at all —
`flux-capabilities/src/browser.rs` names CDP automation as deferred in a comment.

The organizing idea of this epic: **working with the web is three fundamentally different
capabilities**, distinguished by what the model *sees* and what can go wrong — and they should be
three deliberately separate surfaces, not one op with modes:

| Tier | Capability | The model sees |
|---|---|---|
| **1 — request** | `http.request` — raw protocol access, any method/headers/body | status + headers + capped body (bytes) |
| **2 — read** | `web.fetch` — fetch a page as a *document* | readable content as **condensed markdown**, never markup |
| **3 — browse** | `browser.*` — operate a page as an *application* | an **interface digest**: condensed content + a resolved **action space** (stable element refs), deltas after actions |

The rule of thumb the surface teaches the model: **APIs → tier 1, documents → tier 2,
applications → tier 3.** Tier 3 is deliberately **non-visual by default**: the model sees what a
screen reader sees (roles, names, states) plus condensed text — not screenshots, not HTML source,
and after the first observation it sees *deltas*, not the page again.

**All three tiers are native — no plugins.** These are table-stakes capabilities of a daily-driver
agent; none of them should sit behind an install step (user call, 2026-07-09, revising the first
draft of this design). They live in **one new library crate, `crates/flux-web`** (package
`codewandler-flux-web`, the workspace convention), and all of them are governed by **one
family-wide scoped egress policy** instead of today's per-tool special case.

This epic subsumes the original D-98 (which proposed a `flux-web` *plugin*) and re-scopes it to
the native crate + tier 1.

## Approach

### The crate: `crates/flux-web`

One crate, module-per-concern (`http`, `fetch`, `condense`, `browser`, `cdp`, `digest`) — the
"prefer one crate + modules" rule. Mechanics, all verified against the current tree:

- **Layer L5**, beside `flux-capabilities` — add `"flux-web"` to the L5 arm of the layer match in
  `crates/flux-codegate/src/lib.rs` (~line 42). Its flux-deps are all ≤ L2: `flux-runtime` (the
  `Tool` trait, `flux-runtime/src/lib.rs:366`), `flux-spec` (ToolSpec/Effect/Risk/intents),
  `flux-system` (the net guard), `flux-markdown` (AST + writer), `flux-evidence` (`ToolGroup`),
  `flux-datasource` (record schema).
- **Registration follows the flux-eval precedent exactly**: `flux_web::register_web(&mut registry)`
  + `groups.push(flux_web::browser_group())` at the same four `flux-cli/src/main.rs` sites where
  `flux_eval::register_eval_ops` / `eval_group()` are wired today (~:2121/:2335/:7264/:7274). The
  existing `WebFetchTool` registration and `flux-capabilities/src/browser.rs` retire into it.
- **Path-only, not published**: only `flux-cli` (unpublished) consumes it, so like `flux-eval` it
  stays out of the crates.io closure — `Cargo.toml` `members` + a path-only
  `[workspace.dependencies]` alias, **no** entry in `scripts/publish-crates-io.sh`. If a published
  crate ever depends on it, it joins the closure then.

### One egress policy for the whole family

The D-96 caveat ("`web_fetch` has no manifest to intersect against") is answered by unification,
not by a manifest: a single **`web` scope** in `[private_net]` config governs every flux-web op.

- Default: **public internet only** — `flux_system::net::guard_url_scoped` runs on every fetch,
  request, and (per D-124) every browser subrequest; private/loopback/link-local/CGNAT ranges are
  refused.
- Private ranges by grant: `[private_net] web = true | ["host", …]` (the existing
  `PrivateNetGrant` shape, `flux-config/src/lib.rs:31`), or ephemerally `--allow-private-net`
  (D-96) which widens the `web` scope exactly as it widens plugin scopes. Every admit emits
  `PrivateNetAdmit` with `caller: "web:<op>"` (the existing event carries all needed fields,
  `flux-events/src/kind.rs:118`).
- The bespoke `web_fetch` path — `effective_web_fetch_private_hosts`
  (`flux-cli/src/main.rs:5497`) and the `[private_net] web_fetch` key — is **deleted** in D-120
  (clean cutover, no fallback key).

Auth for tier 1: secrets ride flux's existing reference model (`secret "ENV"` declarations /
`resolve_secrets`) and every resolved value is redactor-seeded (C-13), so a token in a header
never lands readable in transcripts or `events.db` (C-22 lesson).

### Tier 1 — `http.request` — D-98

A native op in `flux-web::http`: method, URL, headers, body, timeout → status, response headers
(capped), body (capped, char-boundary safe — the `web_fetch` `MAX_BYTES` precedent). Non-2xx is a
*result*, not an op failure. Dotted native op names are established (`proc.run`, `ai.extract`,
`endpoint.discover`). Honest metadata: `Effect::Network`, `NetworkFetch` intent, non-flat risk —
plan approval sees it (D-91 lesson). Ungated (`group: None`, always advertised) — it's one op, and
it's table-stakes.

### Tier 2 — `web.fetch` readable markdown — D-120

The everyday capability — "read this page" — stops returning markup. The condenser lives in
`flux-web::condense`: parse (html5ever family — the dep lands in flux-web, **not** in
flux-markdown) → readability-style extraction (drop nav/boilerplate/script) → **`flux-markdown`
AST** → the existing markdown writer.

- `web_fetch` moves into flux-web and upgrades in place: `text/html` responses return condensed
  markdown (capped *after* condensation, so the budget buys content, not tags); non-HTML stays
  raw; `raw: true` escape hatch.
- A pure op `html_to_markdown` (no egress) composes in flux-lang:
  `http.request → html_to_markdown` covers any exotic fetch-then-read shape.
- Fetched pages contribute `web.page` datasource records (the `websearch` → `web.result`
  pattern), so read content is groundable later.

### Tier 3 — browser use — D-121 → D-124

Native modules in flux-web driving **headless Chromium over CDP**. Four stories, in dependency
order:

**D-121 — foundation.** A minimal hand-rolled CDP client (`flux-web::cdp`) over
`--remote-debugging-pipe` (JSON-RPC on stdio fds 3/4): no WebSocket dep, no control-channel
network socket, no debug-port squatting — the hand-rolled-SigV4/SCRAM tradition, typed for only
the domains we use (Target, Page, DOM, Accessibility, Runtime, Input, Fetch). flux spawns Chrome
as a direct child (`Effect::Process` disclosed on `browser.open`); binary discovery
`FLUX_BROWSER_BIN` → config → PATH candidates (`chromium`, `google-chrome`, …) → an actionable
error. **No auto-download** (supply-chain stance). Ephemeral isolated profile per session; session
registry in-process behind `Arc<Mutex<…>>` — the established stateful-native-tool shape
(`EndpointBroker`'s plugin registry, `SqliteBackend`, `ReadTracker`) — with an idle TTL.

**Surfacing:** the ~5 browser ops are **evidence-gated** as a `browser` `ToolGroup` whose signal
is *a Chromium binary is discoverable* (a `detect_signals` probe; overridable, and
`FLUX_SURFACE_ALL` still forces). Ungated ops land in every system prompt (~7 ops ≈ a 7–8% catalog
bump), and advertising a browser that isn't installed only misleads the planner — this keeps the
catalog honest on machines without Chrome. The tier-1/2 ops stay ungated.

**D-122 — the page digest (the heart of "seeing in a smart way").** `browser.snapshot` builds a
digest from the **accessibility tree** (what a screen reader sees) joined with DOM node identity:

```
https://shop.example/checkout · "Checkout — Example Shop"
## content
<condensed readable text, byte-budgeted>
## actions
e3   link     "Edit cart"
e7   textbox  "Email" (value: "")
e9   checkbox "Subscribe to newsletter" (unchecked)
e12  button   "Place order" (disabled)
```

Interactive elements are filtered by AX role (+ a DOM heuristic fallback for div-soup apps with
unlabeled clickables), each assigned a **stable ref** `e<N>` mapped session-side to a CDP
`backendNodeId` — refs survive re-observation while the node lives. Both sections are
byte-budgeted with omission markers and `len <= cap` pinned by test (the A-24 lesson). Output
ordering is deterministic (replay/diff friendly). Content condensation reuses `flux-web::condense`.

**D-123 — actions with delta re-observe.** `browser.act` — click / type / select / press / scroll /
goto / back by ref, with bounded auto-wait after navigation-triggering acts. Each act returns a
**delta digest**: navigation/title change, focus, dialogs, console errors since last observe, and
added/removed/changed action refs — *not* the full page again (`full: true` on demand). This is
the "without ingesting tons of markup over and over" requirement made structural. Acts carry
honest effect/risk/intent metadata so the plan-approval envelope sees browsing side effects.

**D-124 — per-request egress interception (required for epic-done).** `Fetch.enable` interception
runs **every** request — navigation, subresource, redirect hop, JS-initiated — through
`guard_url_scoped` with the session's `web`-scope `PrivateNetAllow`; violations fail the request
and surface in the digest; admitted private hosts audit `PrivateNetAdmit` (`caller:
"web:browser"`). A navigation-only check cannot govern a browser (redirect-to-private and JS
`fetch()` are the classic SSRF escapes), which is why D-121's coarse gate is explicitly temporary.
No off switch — this *is* the policy (no-fallbacks rule).

### Cross-cutting

- **Determinism / Time Machine:** all of this is ordinary ops through normal dispatch, so outputs
  ride the C-43 cassette — a browsing run replays hermetically and forks/diffs like any other run.
- **The `websearch` plugin and native `web_search` are untouched** (non-goal); a later story may
  fold search into flux-web for symmetry, but nothing here depends on it.
- **Vision:** screenshots are a **non-goal** for v1 — `ToolResult` is text-only (the flux-render
  constraint), and the non-visual digest is the product thesis; revisit only if a multimodal
  content-model seam lands.

## Alternatives considered

- **Plugins for tier 1/3 (the first draft of this design, and the original D-98)** — rejected
  (user call): these are table-stakes capabilities of the agent itself; none should require
  `flux plugin install` before the agent can read a docs page or drive a form. Native also
  removes real friction the plugin shape carried: no host-cap indirection for a long-lived CDP
  session, no manifest-wildcard mechanism (`http_hosts: ["*"]`) to invent — the family-wide
  scoped `web` grant replaces what the manifest declaration would have bought.
- **Condenser as a feature-gated `html` module in flux-markdown** (first draft) — rejected:
  parser deps belong in flux-web; flux-markdown stays a pure markdown engine that flux-web
  consumes for AST + writing.
- **One `web.op` with a `mode` param** — rejected: the three tiers differ in risk, governance,
  and output contract; separate surfaces teach the model which tool fits.
- **chromiumoxide / headless_chrome / playwright bindings** — rejected for v1: heavy generated
  protocol crates + their own tokio/WS stacks + (some) browser auto-download; we use a handful of
  stable CDP domains and flux's tradition is a minimal hand-rolled client on a pipe transport.
- **Screenshots + vision as the observe channel** — rejected as default: token-expensive,
  provider-coupled, non-deterministic, and `ToolResult` is text-only today. The AX-tree digest is
  cheaper, replayable, and closer to the action space anyway.
- **Raw-DOM serialization for the digest** — rejected: markup-shaped, huge, and unstable; the AX
  tree is the "smart seeing" primitive, with DOM heuristics only as fallback for unlabeled
  interactives.

## Risks & open questions

- **Main-gate dep weight** — html5ever-family (and later the CDP client's small deps) now land in
  the root workspace via flux-web. Measure build impact in D-120; the deps are pure Rust and
  modest, but the gate check is explicit acceptance criteria.
- **AX-tree quality on div-soup sites** — the DOM-heuristic fallback (D-122) is best-effort;
  acceptance uses real-world fixture pages, not only well-formed ones.
- **Ref stability across mutations** — SPA re-renders can kill backendNodeIds; the delta digest
  must mark dead refs honestly rather than silently renumbering.
- **Chrome availability** — no auto-download means "no browser installed" is a real state; the
  evidence-gated `browser` group keeps those ops out of the catalog entirely in that state, and
  `browser.open` fails with actionable guidance when forced.
- **CDP drift** — pinned to stable domains; the hermetic scripted-CDP test harness (D-121) is the
  regression net.
- **Dialog handling** (JS alert/confirm/prompt, file pickers) — v1 auto-surfaces dialogs in the
  delta and provides an act to answer them; file upload/download is out of scope.

## Non-goals

Screenshots/vision (above); unbounded/cross-host crawling or spidering; persistent profiles or
cookie stores across runs; credential autofill / browser-driven login automation; CAPTCHA
circumvention; multi-tab orchestration beyond one page per session; replacing/moving `websearch`.

**Update (D-160, D-161):** two capabilities the original design listed as non-goals were later
shipped in a deliberately bounded form:

- **`web.crawl` (D-160)** — a **bounded, same-host** breadth-first crawl over the same egress
  envelope as `web_fetch` (every hop guarded by `guard_url_scoped` + `send_guarded`). It stays
  within the original non-goal's spirit: `max_pages` (≤ 50) and `max_depth` (≤ 5) caps, same-host
  only, and still **no robots.txt/sitemaps, no cross-host crawl, no JS rendering** (that remains the
  tier-3 `browser.*` path). The general "unbounded/cross-host spidering" non-goal stands.
- **PDF text extraction in `web_fetch` (D-161)** — a PDF response (declared `application/pdf` or
  `%PDF` magic-byte sniff) is returned as extracted text instead of a raw byte dump, via a pure-Rust
  extractor with a panic-safe raw fallback. Datasource *file* ingestion of PDFs remains deferred
  (D-50); this covers only the web-fetch path.

## Acceptance / done

Union of D-98 + D-120…D-124, plus the epic demo: against a live JS-rendered site, a model
completes a realistic task ("find X and submit the form") **without ever receiving HTML source** —
first observation is one bounded digest, every subsequent observation is a delta — while a blocked
private-net subresource shows up as a policy refusal in the digest and `PrivateNetAdmit`/deny
events audit correctly. `web_fetch` returns markdown for HTML pages; the whole family answers to
the one `[private_net] web` scope (the per-tool special case is gone from `flux-cli`/`flux-config`);
on a machine without Chromium the browser ops don't appear in the catalog at all.
