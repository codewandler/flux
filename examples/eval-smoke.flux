{
  "name": "eval_smoke",
  "params": [],
  "body": [
    {
      "kind": "bind",
      "name": "baseline",
      "value": { "kind": "call", "op": "eval_run", "args": [ { "kind": "lit", "value": "mock" } ] }
    },
    {
      "kind": "bind",
      "name": "sessions",
      "value": { "kind": "call", "op": "eval_sessions", "args": [ { "kind": "var", "name": "baseline" } ] }
    },
    {
      "kind": "bind",
      "name": "mined",
      "value": { "kind": "call", "op": "painpoints_collect", "args": [ { "kind": "var", "name": "sessions" } ] }
    },
    {
      "kind": "bind",
      "name": "candidates",
      "value": {
        "kind": "call",
        "op": "improvements_aggregate",
        "args": [ { "kind": "var", "name": "mined" }, { "kind": "lit", "value": "[]" } ]
      }
    },
    { "kind": "return", "value": { "kind": "var", "name": "candidates" } }
  ]
}
