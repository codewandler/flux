---
id: C-53
title: Separate approval wait from operation latency in traces and CLI timing
pillar: Core
status: done
note: "Session s_999 labeled ai.reason 26.1s and write 29.9s, but event correlation showed both spans included time waiting for y; headless write completed in milliseconds."
---

# Separate approval wait from operation latency in traces and CLI timing

## Goal

Make performance evidence truthful. A tool's displayed/runtime duration must not silently include
human approval delay; traces should expose approval wait and actual execution as distinct spans so
E2E gates do not diagnose a slow filesystem/model operation when the harness merely answered late.

## Acceptance

- [x] The dispatcher records whether approval was requested and its wait duration without exposing
      secret subjects or changing authorization decisions.
- [x] Run events/cassette compatibility is preserved through optional, version-tolerant fields or a
      dedicated observation.
- [x] CLI/TUI timing either pauses during approval or renders `approval … + execution …` explicitly.
- [x] Headless/fully pre-authorized calls retain their current low-overhead path.
- [x] Failing-first regression: a delayed approver around an instant tool reports the delay as
      approval wait and the tool itself as near-instant.
- [x] Correlated `approval.*` and `tool.*` lifecycle observations expose raw phase boundaries without
      changing authorization, approval, or cassette behavior.
