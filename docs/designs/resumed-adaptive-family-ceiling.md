# Resumed adaptive capability-family ceiling

**Status:** shipped · **Story:** [A-84](../stories/A-84-validate-resumed-adaptive-family-ceiling.md)
· **Pillar:** Agent

## Problem

A-83 rejects a fifth capability family when `signal_capabilities` tries to append it. Adaptive
state is deliberately serialized across decisions and process resumes, however, and the state-entry
path treated the stored declaration as trusted. A state produced before A-83 (or otherwise decoded
at the durable boundary) can therefore already name five distinct small families. If their union
stays below the separate operation and schema-character budgets, exploration expands and exposes
that oversized catalog.

The family ceiling must be an invariant of the durable value, not only of the operation that
normally mutates it.

## Decision

Validate the deduplicated declaration family set at `selected_specs`, the narrowest shared boundary
used before any adaptive family union becomes provider-facing operation definitions. This covers
initial declarations, routing decisions, capability signals, and deserialized/resumed state without
duplicating resume-only policy.

The validation runs before family members or their schemas are traversed. More than four distinct
names fails locally. Repeated serialized names are idempotent and do not consume additional slots;
the existing operation-name map continues to deduplicate their members.

A-83's earlier signal-specific union check stays in place. It gives that control operation a precise
diagnostic and guarantees rejection before durable mutation, while the shared expansion guard is
the defense-in-depth state-entry invariant.

## Compatibility and safety

Valid states containing zero through four distinct live families are unchanged. Existing live-
registry, authored-tool, permission, approval, 64-operation, and 128,000-schema-character checks
remain independent and fail closed as before.

Published `v0.23.0` predates this invariant, so the immutable correction ships as `v0.23.1`.
