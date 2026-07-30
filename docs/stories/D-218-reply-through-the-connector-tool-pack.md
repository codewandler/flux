---
id: D-218
title: "The binding's reply is the connector Tool pack's operation — delete the hand-built `chat.postMessage`"
pillar: Agent
status: backlog
epic: connector-channels
note: "adapters/slack.rs:150-154 constructs a chat.postMessage from channel/text/thread_ts — the three body params of an operation flux-connectors compiles, declared in slack.toml as `operation = \"slack-chat-post-message\"` plus a two-entry bind map. This story is where that Rust stops existing"
---

# The binding's reply is the connector Tool pack's operation — delete the hand-built `chat.postMessage`

## Goal

Wire the connector channel's reply to the operation the binding names, dispatched as the **Tool the
connector pack registered** — so the vendor request is built once, by the generated code, and not a
second time by hand inside a channel adapter.

## Context — verified against this tree

- The hand-built request: `SlackApiChatPostMessageRequest::new(SlackChannelId(channel),
  SlackMessageContent::new().with_text(reply)).with_thread_ts(thread)` —
  `crates/flux-channels/src/adapters/slack.rs:150-154`.
- The declaration that replaces it, already published upstream:
  `../flux-connectors/providers/slack.toml:448-454` — `operation = "slack-chat-post-message"`,
  `result = "text"`, and `bind = { channel = "channel", thread_ts = "thread" }`.
- The `result` field exists because a reply's *text* is the one value no path into the inbound event can
  reach. flux already makes that split in code: `slack.rs:143-148` joins the `JourneyRun` results,
  skipping empties, and passes them as the text; `:149` sends nothing when the join is empty.
- The Tool that answers is the one D-214's pack registers. Its request construction, credential handling,
  redaction and **mirrored network gate** are the pack's concern
  (`crates/flux-web/src/http.rs:118`, `:126` — the `permission_subjects` and `NetworkFetch` that a direct
  `HttpRequestTool::execute` would otherwise lose).

## Acceptance

- [ ] The connector channel fills the reply's parameters from three sources and no others: the binding's
      `bind` map (payload symbol → parameter), the binding's `result` (the joined journey output), and
      nothing else. A parameter named by both `bind` and `result` is already refused upstream; assert
      flux does not silently prefer one.
- [ ] **The reply opens no HTTP client.** It hands one `(tool name, params)` pair to
      `Deliverer::call_operation` (D-217). Test `connector_reply_makes_no_direct_request`: with no HTTP
      port bound and a recording tool registered, the reply still reaches the tool.
- [ ] **Failing-first test `slack_binding_replies_through_the_registered_operation`**: an inbound
      `app_mention` fixture, a journey returning text, and an assertion that the pack's
      `chat.postMessage` Tool received `{ channel, thread_ts, text }` — failing against any
      implementation that constructs the request itself.
- [ ] Empty output sends nothing, matching `slack.rs:149`. A binding with no `result` and all required
      parameters bound is a legal fire-and-forget acknowledgement.
- [ ] **`crates/flux-channels/src/adapters/slack.rs` loses its payload map, its reply, its allow-list
      gate and its bot token.** What may remain is the connection loop, which D-220 moves. The diff is
      the acceptance: `build_payload`, `conversation_id`, `allowed` and the `chat_post_message` call are
      deleted here, not moved.
- [ ] The delivered payload for the Slack binding is byte-comparable with what `build_payload`
      (`slack.rs:172-180`) produced for the same event, **except** where the design records a known gap.
      Name each difference in Progress rather than adjusting the test to match.
- [ ] Both workspace gates green.

## Progress

- (not started)

## Notes

- Parent: **D-215**. Depends on **D-216** (the arm), **D-217** (the dispatch seam), and on
  flux-connectors **C-115/C-117** for the pack that registers the operation. Design:
  `../flux-connectors/docs/designs/connector-channel-seam.md`, sections "Composing with the Tool pack"
  and "Can `adapters/slack.rs` be deleted? An honest accounting".
- **The known gap to expect in the payload comparison.** `slack.rs:167-169` computes the conversation id
  as *thread ts, else channel*; the binding declares `conversation = "event.thread_ts"`
  (`../flux-connectors/providers/slack.toml:446`) and a payload map has no `coalesce`. So a
  **non-threaded** message loses its conversation id and runs in a one-shot session rather than the
  channel's persistent one (`crates/flux-app/src/app.rs:1563-1567`). Recorded upstream rather than
  papered over with an invented spelling — do not invent one here either.
- **The other known gap: Slack's `message` event stays unusable.** `slack.rs:109` drops any message
  carrying a `subtype` or a `bot_id`, because replying to our own post recurses. The upstream schema
  declares those fields for documentation but cannot express *absence* as a matching condition, so the
  guard is not reproducible from the binding. Ship `app_mention` only, and say so — a `message` binding
  wired without the guard is an infinite loop against a live workspace, not a test failure.
- The naming projection from an operation id (`slack-chat-post-message`) to a registered tool name is the
  pack's, not this story's — flux-connectors' C-114 owns it. Read it rather than assuming a rule.
