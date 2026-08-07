---
id: C-712
title: "fleet logs scopes to the worker it was given, or says what it can scope to"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli]
note: "fleet logs <worker> ignores its target and returns wave-level lifecycle events, which reads as 'this worker produced nothing'"
---

# fleet logs scopes to the worker it was given, or says what it can scope to

## Goal

`flux fleet logs <worker>` accepts a worker name, ignores it, and returns wave-level lifecycle
events. It does not error and it does not say it ignored the argument, so the operator reads the
output as *this worker's* log — and since a worker's own operations are not in it, the natural
conclusion is that the worker did nothing.

That conclusion is wrong in the worst available way. Measured on `wave-602`: while `fleet logs`
showed only wave lifecycle, `wave-602-worker-1` had made **165 model calls** and written 946 lines,
and its full transcript — every plan, operation and result — was sitting in its session store the
whole time. A command that answers a different question than the one asked is worse than one that
refuses, because a refusal sends you looking and a wrong answer stops you looking.

The fix is small and is mostly about honesty. Either scope the output to the named worker, or refuse
the argument and name what the verb *can* scope to. What it must not keep doing is silently widen
the scope and let the output imply an answer it never computed.

## Acceptance

- [ ] `flux fleet logs <worker>` returns that worker's own recorded activity, or refuses with a message naming what it can scope to (a wave, a repository) and how to reach worker-level detail.
- [ ] A target the verb cannot scope to is an error, never a silently widened result. Returning wave-level events for a worker argument is specifically refused by a test.
- [ ] When output is scoped to a wave because that is what was asked, the output says so, so a reader can tell the difference between "this is the wave" and "this is the worker".
- [ ] `--output json` carries the resolved scope as data, so automation can assert what it received rather than inferring it from shape.
- [ ] Failing first: a test asks for a worker known to have produced events and asserts the result is not the wave-level lifecycle stream.

## Notes

Found while extracting worker transcripts during `wave-602`. Closely related to
[C-599](C-599-fleet-work-is-unobservable-while-it-runs.md), which gives the TUI a transcript view
over the worker's own store — that story makes the data reachable from the operator surface, this
one stops the CLI verb from misreporting it. They should land together or reference each other; the
CLI lying is not fixed by adding a second surface that tells the truth.
