---
id: A-57
title: A2A push notifications — pushNotificationConfig methods + webhook delivery
pillar: Agent
status: backlog
epic: a2a-conformance
design: docs/designs/a2a-stateful-task-model.md
note: "Tier-3, last/optional slice: per-task webhook config + delivery; flips capabilities.pushNotifications"
---

# A2A push notifications

## Goal
Let a client register a webhook for a task and have flux POST the task's status/artifact updates to
that URL as a background run advances — implementing `tasks/pushNotificationConfig/{set,get,list,delete}`
and a delivery layer, and flipping `capabilities.pushNotifications` to `true`.

## Why (evidence)
- The four `pushNotificationConfig/*` methods have no server code (`-32004`, A-50);
  `capabilities.pushNotifications` is advertised `false` (honest).
- The delivery payloads already exist: `TaskStatusUpdateEvent` / `TaskArtifactUpdateEvent` shapers
  (A-52). Presupposes the non-blocking execution + task projection from A-54.

## Acceptance
- [ ] `tasks/pushNotificationConfig/{set,get,list,delete}` store/read/remove a per-task webhook config,
      realm-scoped; unknown/other-realm task id → `-32001`.
- [ ] A delivery layer POSTs `TaskStatusUpdateEvent` / `TaskArtifactUpdateEvent` frames to the
      registered URL as the background run advances (best-effort, with a documented retry/failure
      policy).
- [ ] `capabilities.pushNotifications` is emitted `true` once enabled; `-32003
      PushNotificationNotSupported` becomes the "configured off / not enabled" answer.
- [ ] Failing-first tests: a `set` then a task run delivers ≥1 update to a test webhook; `get`/`list`
      reflect the config; `delete` stops delivery; the card capability flips.

## Notes
- Last and optional Tier-3 slice; ship A-54..A-56 first. Webhook egress needs a documented
  security/SSRF posture (allowed schemes/hosts). Epic:
  [a2a-conformance](../designs/a2a-conformance.md).
