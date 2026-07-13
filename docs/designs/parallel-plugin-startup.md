# Parallel plugin startup

## Problem

An agent command loaded every installed plugin before the first model request. Eighteen independent
descriptor verification, process startup, and manifest exchanges consumed roughly 2.2 seconds on a
warm mock run. The first concurrency attempt used `buffer_unordered`, but real plugin futures perform
synchronous verification/spawn work before their first pending await. A future could therefore run
most or all of its handshake during one poll, leaving the other futures effectively serial.

## Contract

Each plugin load runs in its own Tokio task behind a bounded semaphore. This lets the multi-threaded
runtime overlap synchronous prefixes as well as async pipe exchanges without creating unbounded
process fan-out. Plugin processes still start only through `System`, descriptor hashes are still
verified, and the manifest still defines deny-by-default host capabilities.

Completion order is deliberately ignored. Successful results are sorted by plugin name before tool,
group, and endpoint-broker registration, preserving a stable model catalog and prompt-cache prefix.

## Verification

A regression future blocks before its first yield and proves at least two loads overlap; this is the
shape the earlier test missed. On 2026-07-13, three warm normal-HOME mock runs improved from
2.222–2.246 seconds to 0.585–0.592 seconds. In a live Codex trace, time outside the provider fell
from roughly 2.53 seconds to 0.75 seconds. The provider itself remains separately attributable via
`FLUX_MODEL_TRACE`.
