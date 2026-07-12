---
id: C-53
title: Separate approval wait from operation latency in traces and CLI timing
pillar: Core
status: backlog
note: "Session s_999 labeled ai.reason 26.1s and write 29.9s, but event correlation showed both spans included time waiting for y; headless write completed in milliseconds."
---

# Separate approval wait from operation latency in traces and CLI timing

## Goal

Make performance evidence truthful. A tool's displayed/runtime duration must not silently include
human approval delay; traces should expose approval wait and actual execution as distinct spans so
E2E gates do not diagnose a slow filesystem/model operation when the harness merely answered late.

## Acceptance

- [ ] The dispatcher records whether approval was requested and its wait duration without exposing
      secret subjects or changing authorization decisions.
- [ ] Run events/cassette compatibility is preserved through optional, version-tolerant fields or a
      dedicated observation.
- [ ] CLI/TUI timing either pauses during approval or renders `approval … + execution …` explicitly.
- [ ] Headless/fully pre-authorized calls retain their current low-overhead path.
- [ ] Failing-first regression: a delayed approver around an instant tool reports the delay as
      approval wait and the tool itself as near-instant.
- [ ] The tutorial E2E runner reacts to approval prompts promptly and gates provider/runtime time
      separately from operator wait.

