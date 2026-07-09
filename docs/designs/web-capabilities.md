# Design: Web capabilities — request · read · browse

**Status:** proposed · **Pillar:** Agent / Core · **Stories:**
[D-98](../stories/D-98-flux-web-plugin-and-http-request-op.md) ·
[D-120](../stories/D-120-web-fetch-readable-markdown.md) ·
[D-121](../stories/D-121-browser-plugin-cdp-foundation.md) ·
[D-122](../stories/D-122-browser-page-digest.md) ·
[D-123](../stories/D-123-browser-actions-delta.md) ·
[D-124](../stories/D-124-browser-egress-interception.md)

## Why

flux's web surface grew bottom-up and it shows. Today there are exactly two model-facing web ops:
`web_fetch` (native, raw `[status]\n<body>` text capped at 256 KiB, **no** condensation, and a
bespoke private-net path with no manifest to intersect against — the caveat recorded on
[D-96](../stories/D-96-allow-private-net-cli-override.md)) and `web_search` (native, Tavily) plus
the `websearch` plugin. Integration plugins do vendor-scoped HTTP through the host `http.do`
capability, but nothing exposes *generic* HTTP to the model. And there is no browser at all —
`flux-capabilities/src/browser.rs` names CDP automation as deferred in a comment.

The organizing idea of this epic: **working with the web is three fundamentally different
capabilities**, distinguished by what the model *sees*, what can go wrong, and how egress is
governed — and they should be three deliberately separate surfaces, not one op with modes:

| Tier | Capability | The model sees | Governance |
|---|---|---|---|
| **1 — request** | `http.request` — raw protocol access, any method/headers/body | status + headers + capped body (bytes) | plugin envelope: declared open-**public** egress, scoped private-net grant, secret-by-purpose auth injection, redaction |
| **2 — read** | `web.fetch` — fetch a page as a *document* | readable content as **condensed markdown**, never markup | public-https native tool; same condenser core everywhere |
| **3 — browse** | `browser.*` — operate a page as an *application* | an **interface digest**: condensed content + a resolved **action space** (stable element refs), deltas after actions | browser plugin sessions + per-request egress interception |

The rule of thumb the surface teaches the model: **APIs → tier 1, documents → tier 2,
applications → tier 3.** Tier 3 is deliberately **non-visual by default**: the model sees what a
screen reader sees (roles, names, states) plus condensed text — not screenshots, not HTML source,
and after the first observation it sees *deltas*, not the page again.

This epic subsumes [D-98](../stories/D-98-flux-web-plugin-and-http-request-op.md) (which bundled
tiers 1+2) and re-scopes it to tier 1.

## Approach

### Tier 1 — `http.request` (the `web` plugin) — D-98

A new `plugins/web` crate (binary `flux-plugin-web`) exposing `http.request`: method, URL, headers,
body, timeout → status, response headers (capped), body (capped, char-boundary safe — the
`web_fetch` `MAX_BYTES` precedent). Non-2xx is a *result*, not an op failure. All IO rides the host
`http.do` capability — **no `reqwest`/`std::net` in the plugin**, per the references-only invariant
(D-27).

The new mechanism this needs: an **open-public egress declaration**. Every existing plugin pins
`http_hosts` to vendor hosts; a generic web op can't enumerate hosts. The manifest gains
`http_hosts: ["*"]` meaning *any public host* — `ensure_http_host_allowed` learns that the wildcard
never admits a private/loopback/link-local target (the `flux_system::net` guard still runs on every
call); private hosts remain reachable **only** via the scoped private-net grant model
(D-20/D-95/D-96), audited via `PrivateNetAdmit`. So "arbitrary HTTP" is arbitrary over the public
internet by declaration, and over private ranges only by explicit operator grant — visible in the
manifest, enforced host-side.

Auth: prefer host-injected secret-by-purpose (the D-12 Basic/header/query injection) over raw
header values; raw values still work but are redactor-seeded (C-13) so they never land readable in
transcripts or `events.db` (C-22 lesson).

### Tier 2 — `web.fetch` readable markdown (native) — D-120

The everyday capability — "read this page" — stays **native and zero-install**, but stops returning
markup. A condenser core turns HTML into condensed markdown: parse (html5ever-family) → readable
extraction (drop nav/boilerplate/script, readability-style) → **`flux-markdown` AST** → the
existing markdown writer. Proposed home: a feature-gated `html` module in `flux-markdown` (L0, so
the plugin workspace can share it — the `flux-datasource` precedent), keeping "prefer one crate +
modules".

- Native `web_fetch` upgrades in place: `text/html` responses return condensed markdown (capped
  *after* condensation, so the budget buys content, not tags); non-HTML stays raw; `raw: true`
  escape hatch.
- A pure op `html_to_markdown` registers natively — composable in flux-lang
  (`http.request → html_to_markdown` covers "read a private-net page" without any special path).
- **The bespoke native private-net path dies** (clean cutover, no fallback flag): `web_fetch`
  becomes public-only; `effective_web_fetch_private_hosts` and the `web_fetch` special-casing in
  `[private_net]` / `--allow-private-net` are deleted. Private fetching = the web plugin under a
  grant. This completes what D-96 flagged and D-98 originally proposed.
- Fetched pages contribute `web.page` datasource records (the `websearch` → `web.result` pattern),
  so read content is groundable later.

### Tier 3 — browser use (the `browser` plugin) — D-121 → D-124

A new `plugins/browser` crate driving **headless Chromium over CDP**. Four stories, in dependency
order:

**D-121 — foundation.** A minimal hand-rolled CDP client over `--remote-debugging-pipe` (JSON-RPC
on stdio fds 3/4): no WebSocket dep, no control-channel network socket, no debug-port squatting —
the hand-rolled-SigV4/SCRAM tradition, typed for only the domains we use (Target, Page, DOM,
Accessibility, Runtime, Input, Fetch). The plugin spawns Chrome as its own child (a
manifest-declared process capability, so the operator sees it; host-mediated spawn can't cheaply
hand the pipe pair back). Binary discovery: `FLUX_BROWSER_BIN` → config → PATH candidates
(`chromium`, `google-chrome`, …) → an actionable error. **No auto-download** (supply-chain stance).
Ephemeral isolated profile per session; `browser.open` / `browser.goto` / `browser.close`; session
state lives in the plugin process with an idle TTL. Until D-124, navigation URLs are guarded
coarsely via `flux_system::net` (public-only; documented as *insufficient* — see D-124).

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
ordering is deterministic (replay/diff friendly).

**D-123 — actions with delta re-observe.** `browser.act` — click / type / select / press / scroll /
goto / back by ref, with bounded auto-wait after navigation-triggering acts. Each act returns a
**delta digest**: navigation/title change, focus, dialogs, console errors since last observe, and
added/removed/changed action refs — *not* the full page again (`full: true` on demand). This is
the "without ingesting tons of markup over and over" requirement made structural. Acts carry
honest effect/risk/intent metadata (`Effect::Network`, Risk Medium, `NetworkFetch` intents) so the
plan-approval envelope sees browsing side effects (D-91 lesson: no flat dishonest risk).

**D-124 — per-request egress interception (required for epic-done).** `Fetch.enable` interception
runs **every** request — navigation, subresource, redirect hop, JS-initiated — through
`guard_url_scoped` with the session's `PrivateNetAllow` scope; violations fail the request and
surface in the digest; admitted private hosts audit `PrivateNetAdmit`. A navigation-only check
cannot govern a browser (redirect-to-private and JS `fetch()` are the classic SSRF escapes), which
is why D-121's coarse gate is explicitly temporary. No off switch — this *is* the policy
(no-fallbacks rule).

### Cross-cutting

- **Determinism / Time Machine:** all of this is ordinary ops through normal dispatch, so outputs
  ride the C-43 cassette — a browsing run replays hermetically and forks/diffs like any other run.
  Deterministic digest ordering makes `flux diff` on browsing runs legible.
- **Surfacing:** native `web_fetch`/`web_search`/`html_to_markdown` stay ungated (group `None`,
  as today); plugin ops surface by installation, as all plugin ops do. No new evidence group —
  web work isn't workspace-evidenced.
- **Vision:** screenshots are a **non-goal** for v1 — `ToolResult` is text-only (the flux-render
  constraint), and the non-visual digest is the product thesis; revisit only if a multimodal
  content-model seam lands.

## Alternatives considered

- **One plugin with modes / one `web.op` with a `mode` param** — rejected: the three tiers differ
  in risk, governance, and output contract; separate surfaces teach the model which tool fits.
- **chromiumoxide / headless_chrome / playwright bindings** — rejected for v1: heavy generated
  protocol crates + their own tokio/WS stacks + (some) browser auto-download; we use a handful of
  stable CDP domains and flux's tradition is a minimal hand-rolled client on a pipe transport.
- **Screenshots + vision as the observe channel** — rejected as default: token-expensive,
  provider-coupled, non-deterministic, and `ToolResult` is text-only today. The AX-tree digest is
  cheaper, replayable, and closer to the action space anyway.
- **Moving web fetching wholly into the plugin (original D-98)** — revised: the *reader* stays
  native so a fresh install can read docs pages with zero setup; the security payload of D-98 (kill
  the bespoke native private-net path) is preserved by making native `web_fetch` public-only.
- **Raw-DOM serialization for the digest** — rejected: markup-shaped, huge, and unstable; the AX
  tree is the "smart seeing" primitive, with DOM heuristics only as fallback for unlabeled
  interactives.

## Risks & open questions

- **HTML-parser dep weight in the main gate** (html5ever family via the `flux-markdown` `html`
  feature). Measure build impact in D-120; fallback: condenser lives only in the plugins workspace
  and native `web_fetch` stays raw (re-scope, explicitly ugly).
- **AX-tree quality on div-soup sites** — the DOM-heuristic fallback (D-122) is best-effort;
  acceptance uses real-world fixture pages, not only well-formed ones.
- **Ref stability across mutations** — SPA re-renders can kill backendNodeIds; the delta digest
  must mark dead refs honestly rather than silently renumbering.
- **Chrome availability** — no auto-download means "no browser installed" is a real state;
  `browser.open` must fail with actionable guidance, and the epic demo documents setup.
- **CDP drift** — pinned to stable domains; the hermetic scripted-CDP test harness (D-121) is the
  regression net.
- **Dialog handling** (JS alert/confirm/prompt, file pickers) — v1 auto-surfaces dialogs in the
  delta and provides an act to answer them; file upload/download is out of scope.

## Non-goals

Screenshots/vision (above); crawling/spidering; persistent profiles or cookie stores across runs;
credential autofill / browser-driven login automation (API auth is the plugin-oauth surface);
CAPTCHA circumvention; multi-tab orchestration beyond one page per session; replacing `websearch`.

## Acceptance / done

Union of D-98…D-124, plus the epic demo: against a live JS-rendered site, a model completes a
realistic task ("find X and submit the form") **without ever receiving HTML source** — first
observation is one bounded digest, every subsequent observation is a delta — while a blocked
private-net subresource shows up as a policy refusal in the digest and `PrivateNetAdmit`/deny
events audit correctly. `web_fetch` returns markdown for HTML pages and its private-net
special-casing is gone from `flux-cli`/`flux-config`.
