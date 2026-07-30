---
title: Streaming & cancellation
description: "Observe a turn as it happens — text, plan, tool calls and results — with a consumer sink or an owned event stream, and cancel mid-flight."
---

# Streaming & cancellation

`Session::send` runs a turn to completion and hands back a collected `TurnOutput`. When you want to
show progress — the assistant's text as it arrives, each tool call and its result, plan headlines —
a [`Session`](./sessions.md) gives you two streaming shapes over the same engine, plus cancellation.

## Bring your own sink

Implement `AgentSink` and pass it to `send_with`. It receives the turn's events as they happen —
text and thinking deltas, plan progress, tool calls, **and tool results** — while `send_with` still
returns the collected `TurnOutput`:

```rust
use flux_sdk::{AgentSink, CancellationToken};

struct Printer;
impl AgentSink for Printer {
    fn text_delta(&mut self, t: &str) { print!("{t}"); }
    fn tool_call(&mut self, name: &str, _input: &serde_json::Value) {
        println!("\n[calling {name}]");
    }
    fn tool_result(&mut self, name: &str, _result: &flux_sdk::tools::ToolResult) {
        println!("[{name} done]");
    }
}

let out = session.send_with("Summarize the repo", &mut Printer, &CancellationToken::new()).await?;
```

## An owned event stream

`Session::stream` returns a `TurnStream` — a `futures::Stream` of owned `AgentEvent`s you can consume
with a loop, no trait to implement. The turn runs on a spawned task, so events arrive as they happen
whether or not you are polling:

```rust
use flux_sdk::AgentEvent;

let mut stream = session.stream("Summarize the repo");
while let Some(event) = stream.next().await {
    match event {
        AgentEvent::TextDelta(t) => print!("{t}"),
        AgentEvent::ToolCall { name, .. } => println!("\n[calling {name}]"),
        AgentEvent::ToolResult { name, .. } => println!("[{name} done]"),
        _ => {}
    }
}
let out = stream.finish().await?; // the same TurnOutput a plain `send` would return
```

`AgentEvent` mirrors `AgentSink` closely, but not exhaustively: the sink's `tool_timing` callback has
no event variant, so per-operation timing is available to an embedded sink and not to a stream
consumer. Every other sink method has a variant.

## Cancelling a turn

Both shapes cancel: the token for `send_with`, or `stream.cancel()`. Cancellation drops the
in-flight op and persists exactly one closing assistant message, so the session log stays a valid
`user → assistant` alternation and resumes cleanly.

```rust
let mut stream = session.stream("Run the long analysis");
// … decide to stop …
stream.cancel();
let _ = stream.finish().await;
```

See [`examples/streaming.rs`](https://github.com/codewandler/flux/tree/main/crates/flux-sdk/examples)
for a runnable, no-API-key version.

## Related docs

- [Sessions & persistence](./sessions.md) — the `Session` handle and durable conversations.
- [SDK overview](./overview.md) — the front doors and provider setup.
- [Safety & approvals](../agent/safety.md) — the envelope every streamed op still passes through.
