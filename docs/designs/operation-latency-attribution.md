# Operation latency attribution

## Problem

The CLI starts an operation timer before dispatch enters the safety envelope. When approval is
required, human response time is consequently labeled as operation execution time. This corrupted
the tutorial E2E latency diagnosis: an instant write appeared to take roughly thirty seconds.

## Contract

The dispatcher is the authoritative measurement boundary. Its audit model is a correlated lifecycle
event sequence:

- `approval.requested` → `approval.approved` or `approval.denied` around the approver await;
- `tool.started` → `tool.ended` around `Tool::execute` only;
- `tool.cache_hit` when a deterministic read is served without executing the tool.

Every lifecycle event carries a monotonic dispatch id and microseconds elapsed from that dispatch's
entry. It contains no permission subjects, parameters, or result content. The ordinary evidence
flush makes the events durable without changing cassette cells; old logs and replay remain compatible.

For live surfaces, the dispatcher also projects the lifecycle into an `OperationTiming` convenience
value on the host outcome. CLI/TUI render `exec … + approval …`; callers that ignore the new sink
callback remain source compatible. This aggregate is not the audit source of truth — it is derived
presentation data. Denied calls have no execution duration, and pre-authorized calls retain the
direct path with monotonic-clock/event overhead only.

This split attributes tool latency only. Provider request phases (request start, first response,
thinking/tool/text deltas, completion, usage) are the next trace layer and must not be conflated with
operator wait.
