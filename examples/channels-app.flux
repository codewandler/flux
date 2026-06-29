{
  "name": "channels-demo",
  "comment": "A background agent woken by events: a cron heartbeat + an inbound webhook. Run it with `flux app run examples/channels-app.flux` (Ctrl-C to stop). Each channel fires a bus event under its own name; the matching trigger runs a journey. These journeys use only pure ops, so no model/credentials are needed.",

  "channels": [
    { "name": "heartbeat", "kind": "schedule", "settings": { "schedule": "*/5 * * * * *" } },
    { "name": "ci",        "kind": "webhook",  "settings": { "addr": "127.0.0.1:8799", "path": "/ci" } }
  ],

  "triggers": [
    { "name": "on_boot", "on": "startup",   "run": "announce" },
    { "name": "on_beat", "on": "heartbeat", "run": "tick" },
    { "name": "on_ci",   "on": "ci",        "run": "ci_report" }
  ],

  "journeys": [
    {
      "name": "announce",
      "flow": { "name": "announce", "body": [
        { "kind": "call", "op": "send", "args": [
          { "kind": "lit", "value": { "channel": "cli", "message": "channels-demo up — heartbeat every 5s; POST JSON to http://127.0.0.1:8799/ci" } }
        ] },
        { "kind": "return", "value": { "kind": "lit", "value": "" } }
      ] }
    },
    {
      "name": "tick",
      "flow": { "name": "tick", "body": [
        { "kind": "bind", "name": "m", "value": { "kind": "fmt", "template": "heartbeat at {at}" } },
        { "kind": "call", "op": "send", "args": [ { "kind": "lit", "value": "cli" }, { "kind": "var", "name": "m" } ] },
        { "kind": "return", "value": { "kind": "var", "name": "m" } }
      ] }
    },
    {
      "name": "ci_report",
      "flow": { "name": "ci_report", "body": [
        { "kind": "bind", "name": "r", "value": { "kind": "fmt", "template": "received CI event: {status}" } },
        { "kind": "return", "value": { "kind": "var", "name": "r" } }
      ] }
    }
  ]
}
