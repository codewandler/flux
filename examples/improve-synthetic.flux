{
  "name": "improve_synthetic",
  "returns": { "named": "EvalReport" },
  "params": [],
  "body": [
    {
      "kind": "bind",
      "name": "baseline",
      "value": {
        "kind": "call",
        "op": "eval_run",
        "args": [
          { "kind": "lit", "value": {
            "adapter": "synthetic",
            "trials": 5,
            "model": "anthropic/claude-sonnet-4-6",
            "flux_bin": "target/debug/flux"
          } }
        ]
      }
    },
    {
      "kind": "bind",
      "name": "base_score",
      "value": { "kind": "call", "op": "eval_scalar", "args": [ { "kind": "var", "name": "baseline" } ] }
    },
    {
      "kind": "bind",
      "name": "reviewed",
      "value": {
        "kind": "call",
        "op": "task",
        "args": [
          { "kind": "lit", "value": "reviewer" },
          { "kind": "lit", "value": "These are flux's synthetic coding-riddle results: short self-contained problems that ask the agent to write `solution.py`, graded objectively on `python3 solution.py` stdout. Each failing case names a task and a failure_mode. Identify flux HARNESS improvements (tools, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix) that would help flux solve more riddles on the first attempt — not changes to any single riddle. Results:\n{{baseline}}\n\nReturn ONLY a JSON array: [{\"area\":..,\"symptom\":..,\"suggested_fix\":..,\"severity\":1-5}]." }
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
              { "kind": "lit", "value": "Turn these flux-harness improvement candidates into AT MOST 2 concrete, small, safe engineering tasks for the flux codebase (tool specs, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix). Do NOT touch crates/flux-eval, bench/, the loop flows, the synthetic suite, or CI. Candidates:\n{{candidates}}\n\nReturn ONLY the JSON array of tasks." }
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
                    "adapter": "synthetic",
                    "trials": 5,
                    "model": "anthropic/claude-sonnet-4-6",
                    "flux_bin": "target/debug/flux"
                  } }
                ]
              }
            },
            {
              "kind": "bind",
              "name": "cand_score",
              "value": { "kind": "call", "op": "eval_scalar", "args": [ { "kind": "var", "name": "candidate" } ] }
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
                { "kind": "call", "op": "git_commit", "args": [ { "kind": "lit", "value": "improve: adopt candidate (synthetic gain)" } ] },
                { "kind": "call", "op": "git_tag", "args": [ { "kind": "lit", "value": "improve-synthetic-{{cand_score}}" } ] },
                {
                  "kind": "call",
                  "op": "improve_log",
                  "args": [
                    { "kind": "lit", "value": { "bench": "synthetic", "decision": "kept", "reason": "candidate_beat_baseline", "base_score": "{{base_score}}", "cand_score": "{{cand_score}}", "tag": "improve-synthetic-{{cand_score}}", "guard": "{{guard}}", "gate": "{{gate}}", "tasks": "{{tasks}}" } }
                  ]
                },
                {
                  "kind": "bind",
                  "name": "baseline",
                  "value": { "kind": "call", "op": "eval_adopt", "args": [ { "kind": "var", "name": "candidate" } ] }
                }
              ],
              "otherwise": [
                { "kind": "call", "op": "git_revert", "args": [ { "kind": "var", "name": "snapshot" } ] },
                {
                  "kind": "call",
                  "op": "improve_log",
                  "args": [
                    { "kind": "lit", "value": { "bench": "synthetic", "decision": "reverted", "reason": "no_improvement", "base_score": "{{base_score}}", "cand_score": "{{cand_score}}", "guard": "{{guard}}", "gate": "{{gate}}", "tasks": "{{tasks}}" } }
                  ]
                }
              ]
            }
          ],
          "otherwise": [
            { "kind": "call", "op": "git_revert", "args": [ { "kind": "var", "name": "snapshot" } ] },
            {
              "kind": "call",
              "op": "improve_log",
              "args": [
                { "kind": "lit", "value": { "bench": "synthetic", "decision": "reverted", "reason": "gate_failed", "base_score": "{{base_score}}", "guard": "{{guard}}", "gate": "{{gate}}", "tasks": "{{tasks}}" } }
              ]
            }
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
