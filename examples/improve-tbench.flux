{
  "name": "improve_tbench",
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
          { "kind": "lit", "value": {
            "adapter": "terminal-bench",
            "tasks": ["chess-best-move", "fibonacci-server"],
            "trials": 1,
            "dataset": "terminal-bench-core==0.1.1",
            "model": "anthropic/claude-sonnet-4-6",
            "flux_binary": "target/x86_64-unknown-linux-musl/release/flux",
            "rebuild": true
          } }
        ]
      }
    },
    {
      "kind": "bind",
      "name": "reviewed",
      "value": {
        "kind": "call",
        "op": "task",
        "args": [
          { "kind": "lit", "value": "reviewer" },
          { "kind": "lit", "value": "These are flux's terminal-bench results. Each failing case names a task and a failure_mode (e.g. agent_timeout = flux ran out of time, often a harness inefficiency/loop; a plain failure = wrong output). Identify flux HARNESS improvements (tools, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix) that would help flux pass more. Results:\n{{baseline}}\n\nReturn ONLY a JSON array: [{\"area\":..,\"symptom\":..,\"suggested_fix\":..,\"severity\":1-5}]." }
        ]
      }
    },
    {
      "kind": "bind",
      "name": "candidates",
      "value": {
        "kind": "call",
        "op": "improvements_aggregate",
        "args": [ { "kind": "lit", "value": "[]" }, { "kind": "var", "name": "reviewed" } ]
      }
    },
    {
      "kind": "repeat",
      "max": 1,
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
              { "kind": "lit", "value": "Turn these flux-harness improvement candidates into AT MOST 2 concrete, small, safe engineering tasks for the flux codebase (tool specs, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix). Do NOT touch crates/flux-eval, suites/, bench/, or CI. Candidates:\n{{candidates}}\n\nReturn ONLY the JSON array of tasks." }
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
          "value": { "kind": "call", "op": "change_implement", "args": [ { "kind": "var", "name": "tasks" }, { "kind": "lit", "value": 2 } ] }
        },
        {
          "kind": "bind",
          "name": "guard",
          "value": { "kind": "call", "op": "guard_protected", "args": [ { "kind": "var", "name": "snapshot" } ] }
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
                  { "kind": "lit", "value": {
                    "adapter": "terminal-bench",
                    "tasks": ["chess-best-move", "fibonacci-server"],
                    "trials": 1,
                    "dataset": "terminal-bench-core==0.1.1",
                    "model": "anthropic/claude-sonnet-4-6",
                    "flux_binary": "target/x86_64-unknown-linux-musl/release/flux",
                    "rebuild": true
                  } }
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
                { "kind": "call", "op": "git_commit", "args": [ { "kind": "lit", "value": "improve: adopt candidate (terminal-bench gain)" } ] },
                {
                  "kind": "bind",
                  "name": "score",
                  "value": { "kind": "call", "op": "eval_scalar", "args": [ { "kind": "var", "name": "candidate" } ] }
                },
                { "kind": "call", "op": "git_tag", "args": [ { "kind": "lit", "value": "improve-tbench-{{score}}" } ] },
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
          "kind": "call",
          "op": "improve_log",
          "args": [
            { "kind": "lit", "value": { "bench": "terminal-bench", "guard": "{{guard}}", "gate": "{{gate}}", "tasks": "{{tasks}}" } }
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
