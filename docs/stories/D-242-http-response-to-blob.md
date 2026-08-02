---
id: D-242
title: "Stream plugin HTTP responses into the host blob store"
pillar: Core
status: done
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [flux-plugin]
note: "stored ARI recordings must not cross framed NDJSON as unbounded base64"
---

# Stream plugin HTTP responses into the host blob store

## Goal

Give binary API operations a host-owned response-to-blob path so large recordings never inflate
through the plugin protocol.

## Acceptance

- [x] A failing-first live HTTP test proves a response above the inline 16 MiB limit lands as a blob
      and returns only its opaque reference, size and digest.
- [x] The caller must declare both HTTP and blob capabilities; either missing grant refuses closed.
- [x] Streaming has an explicit maximum, timeout and cleanup on error; partial bytes never become a
      valid blob reference.
- [x] Existing `response_binary` behavior and cap remain byte-identical and test-covered.

## Progress

- 2026-08-02 failing first: `cargo test -p codewandler-flux-plugin
  host::tests::http_response_blob_streams_above_the_inline_limit_without_returning_bytes --
  --exact --nocapture` exited 101 because `http.do` ignored `response_blob` and returned no digest.
- 2026-08-02: `http.do` now requires both grants and explicit bounded byte/deadline inputs, retains a
  complete successful response in the existing blob store, and returns only `blob_ref`, `size` and
  `sha256`. Over-limit, timeout and non-2xx paths publish no partial reference; the host-kit exposes
  the endpoint-reference SDK method and a matching mock contract.
- 2026-08-02: `cargo test -p codewandler-flux-plugin` passed 140 tests with one ignored;
  `cargo test -p codewandler-flux-host-kit` passed 36 unit tests plus its four boundary tests;
  focused clippy with `-D warnings` and package-scoped formatting checks passed for both packages.
