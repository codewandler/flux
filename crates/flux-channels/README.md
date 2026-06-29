# flux-channels

Event-trigger **channels** for a flux-app program: a *channel* is a long-running event source — a cron
schedule, an inbound webhook, or a Slack mention — that **wakes the program on an external event**.

Channels are declared in the `.flux` program as ordinary `ChannelDecl`s and run by the app runner:

```bash
flux app run path/to/program.flux        # starts the program's channels; Ctrl-C to stop
# (flux run path/to/program.flux is an alias)
```

A channel fires a bus event **under its own name**; a trigger routes it to a journey:

```json
{
  "channels": [
    { "name": "nightly", "kind": "schedule", "settings": { "schedule": "0 9 * * *" } }
  ],
  "triggers": [
    { "name": "t", "on": "nightly", "run": "summary" }
  ],
  "journeys": [ { "name": "summary", "agent": "reporter", "flow": { "name": "summary", "body": [ /* … */ ] } } ]
}
```

The event payload is seeded into the journey's flow store, so the flow reads it with `{field}`.

## Channel kinds

| `kind` | `settings` | Fires |
|--------|-----------|-------|
| `schedule` / `cron` | `{ "schedule": "0 9 * * *" }` (5-field crontab **or** 6/7-field seconds-first, e.g. `"* * * * * *"`) **or** `{ "on": "startup" }` | `{ at, name }` on each tick / once at boot |
| `webhook` / `http` | `{ "addr": "127.0.0.1:8799", "path": "/hook", "async": false, "token": "secret:env/HOOK_TOKEN" }` | the POSTed JSON body |
| `slack` (feature `slack`) | `{ "bot_token": "secret:env/SLACK_BOT_TOKEN", "app_token": "secret:env/SLACK_APP_TOKEN", "allow_users": [], "allow_channels": [] }` | `{ text, user, channel, thread, conversation }` |

Notes:
- **schedule** is UTC, fire-and-forget. A 5-field crontab is normalized to the `cron` crate's seconds-first form.
- **webhook** runs an axum server per channel; the response carries the triggered journeys' results, or
  `202 Accepted` when `async = true`. A **non-loopback `addr` requires a `token`** (the host auto-approves
  tools, so an open listener is a remote-trigger surface).
- **slack** is feature-gated: `cargo build -p flux-cli --features slack`. It uses socket mode; posts each
  journey's result back to the originating thread; `allow_users`/`allow_channels` (empty = allow all) gate access.
- Tokens accept a literal or a `secret:env/KEY` (or `env:KEY`) reference resolved from the environment.

See [`examples/channels-app.flux`](../../examples/channels-app.flux) and the
[design](../../docs/designs/event-trigger-channels.md).
