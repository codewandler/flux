{
  "name": "eval_synthetic",
  "params": [],
  "returns": { "named": "EvalReport" },
  "body": [
    {
      "kind": "bind",
      "name": "report",
      "value": {
        "kind": "call",
        "op": "eval_run",
        "args": [
          { "kind": "lit", "value": {
            "adapter": "synthetic",
            "trials": 1,
            "model": "openrouter-anthropic/anthropic/claude-sonnet-4.6"
          } }
        ]
      }
    },
    {
      "kind": "bind",
      "name": "md",
      "value": { "kind": "call", "op": "eval_report_md", "args": [ { "kind": "var", "name": "report" } ] }
    },
    { "kind": "return", "value": { "kind": "var", "name": "report" } }
  ]
}
