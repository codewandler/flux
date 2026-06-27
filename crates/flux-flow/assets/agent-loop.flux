{
  "name": "agent-loop",
  "returns": "string",
  "body": [
    { "kind": "bind", "name": "answer",   "value": { "kind": "fmt", "template": "" } },
    { "kind": "bind", "name": "feedback", "value": { "kind": "fmt", "template": "" } },
    { "kind": "bind", "name": "done",     "value": { "kind": "fmt", "template": "" } },

    { "kind": "repeat", "max": 25, "until": { "kind": "var", "name": "done" }, "body": [
      { "kind": "bind", "name": "plan",
        "value": { "kind": "call", "op": "plan", "args": [ { "kind": "var", "name": "feedback" } ] } },

      { "kind": "bind", "name": "kind",
        "value": { "kind": "jq", "path": ".kind", "input": { "kind": "var", "name": "plan" } } },

      { "kind": "match", "subject": { "kind": "var", "name": "kind" },
        "cases": [
          { "value": { "kind": "lit", "value": "chat" }, "body": [
            { "kind": "bind", "name": "answer",
              "value": { "kind": "jq", "path": ".text", "input": { "kind": "var", "name": "plan" } } },
            { "kind": "bind", "name": "done", "value": { "kind": "fmt", "template": "true" } }
          ] },
          { "value": { "kind": "lit", "value": "error" }, "body": [
            { "kind": "bind", "name": "answer",
              "value": { "kind": "jq", "path": ".text", "input": { "kind": "var", "name": "plan" } } },
            { "kind": "bind", "name": "done", "value": { "kind": "fmt", "template": "true" } }
          ] }
        ],
        "default": [
          { "kind": "bind", "name": "ran",
            "value": { "kind": "call", "op": "run_plan", "args": [ { "kind": "var", "name": "plan" } ] } },
          { "kind": "bind", "name": "feedback",
            "value": { "kind": "jq", "path": ".transcript", "input": { "kind": "var", "name": "ran" } } },
          { "kind": "call", "op": "observe",
            "args": [ { "kind": "lit", "value": "turn.iteration" }, { "kind": "var", "name": "ran" } ] }
        ]
      }
    ] },

    { "kind": "return", "value": { "kind": "var", "name": "answer" } }
  ]
}
