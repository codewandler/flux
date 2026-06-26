{
  "name": "improve",
  "params": [],
  "returns": { "named": "EvalReport" },
  "body": [
    {
      "kind": "bind",
      "name": "baseline",
      "value": {
        "kind": "call",
        "op": "eval_run",
        "args": [
          { "kind": "lit", "value": { "adapter": "local", "dir": "suites", "flux_bin": "target/debug/flux" } }
        ]
      }
    },
    {
      "kind": "bind",
      "name": "sessions",
      "value": { "kind": "call", "op": "eval_sessions", "args": [ { "kind": "var", "name": "baseline" } ] }
    },
    {
      "kind": "parallel",
      "branches": [
        {
          "name": "mined",
          "body": [
            { "kind": "call", "op": "painpoints_collect", "args": [ { "kind": "var", "name": "sessions" } ] }
          ]
        },
        {
          "name": "reviewed",
          "body": [
            {
              "kind": "call",
              "op": "task",
              "args": [
                { "kind": "lit", "value": "reviewer" },
                { "kind": "lit", "value": "Review these flux eval results for failure modes and missing capabilities. Eval report (per-case pass/fail + metrics):\n{{baseline}}\n\nReturn ONLY a JSON array of findings." }
              ]
            }
          ]
        }
      ]
    },
    {
      "kind": "bind",
      "name": "candidates",
      "value": {
        "kind": "call",
        "op": "improvements_aggregate",
        "args": [ { "kind": "var", "name": "mined" }, { "kind": "var", "name": "reviewed" } ]
      }
    },
    {
      "kind": "repeat",
      "max": 3,
      "until": { "kind": "call", "op": "candidates_empty", "args": [ { "kind": "var", "name": "candidates" } ] },
      "body": [
        {
          "kind": "bind",
          "name": "tasks",
          "value": {
            "kind": "call",
            "op": "task",
            "args": [
              { "kind": "lit", "value": "planner" },
              { "kind": "lit", "value": "Turn these improvement candidates into a JSON array of concrete, safe, verifiable engineering tasks for the flux codebase. Candidates:\n{{candidates}}\n\nReturn ONLY the JSON array of tasks." }
            ]
          }
        },
        {
          "kind": "bind",
          "name": "snapshot",
          "value": { "kind": "call", "op": "git_snapshot", "args": [] }
        },
        {
          "kind": "bind",
          "name": "implemented",
          "value": { "kind": "call", "op": "change_implement", "args": [ { "kind": "var", "name": "tasks" } ] }
        },
        {
          "kind": "bind",
          "name": "gate",
          "value": { "kind": "call", "op": "gate_check", "args": [] }
        },
        {
          "kind": "when",
          "cond": { "kind": "var", "name": "gate" },
          "then": [
            {
              "kind": "bind",
              "name": "candidate",
              "value": {
                "kind": "call",
                "op": "eval_run",
                "args": [
                  { "kind": "lit", "value": { "adapter": "local", "dir": "suites", "flux_bin": "target/debug/flux" } }
                ]
              }
            },
            {
              "kind": "when",
              "cond": {
                "kind": "call",
                "op": "score_compare",
                "args": [ { "kind": "var", "name": "baseline" }, { "kind": "var", "name": "candidate" } ]
              },
              "then": [
                { "kind": "call", "op": "git_stage", "args": [ { "kind": "lit", "value": ["."] } ] },
                { "kind": "call", "op": "git_commit", "args": [ { "kind": "lit", "value": "improve: adopt candidate (eval improved)" } ] },
                {
                  "kind": "bind",
                  "name": "score",
                  "value": { "kind": "call", "op": "eval_scalar", "args": [ { "kind": "var", "name": "candidate" } ] }
                },
                { "kind": "call", "op": "git_tag", "args": [ { "kind": "lit", "value": "improve-{{score}}" } ] },
                {
                  "kind": "bind",
                  "name": "baseline",
                  "value": { "kind": "call", "op": "eval_adopt", "args": [ { "kind": "var", "name": "candidate" } ] }
                }
              ],
              "otherwise": [
                { "kind": "call", "op": "git_revert", "args": [ { "kind": "var", "name": "snapshot" } ] }
              ]
            }
          ],
          "otherwise": [
            { "kind": "call", "op": "git_revert", "args": [ { "kind": "var", "name": "snapshot" } ] }
          ]
        },
        {
          "kind": "bind",
          "name": "candidates",
          "value": { "kind": "call", "op": "candidates_advance", "args": [ { "kind": "var", "name": "candidates" } ] }
        }
      ]
    },
    { "kind": "return", "value": { "kind": "var", "name": "baseline" } }
  ]
}
