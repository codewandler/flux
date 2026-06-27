{
  "name": "loop-poc",
  "returns": "string",
  "body": [
    {
      "kind": "bind",
      "name": "p",
      "value": {
        "kind": "call",
        "op": "plan",
        "args": [{ "kind": "lit", "value": "say hello to the world" }]
      }
    },
    {
      "kind": "bind",
      "name": "out",
      "value": {
        "kind": "call",
        "op": "run_plan",
        "args": [{ "kind": "var", "name": "p" }]
      }
    },
    { "kind": "return", "value": { "kind": "var", "name": "out" } }
  ]
}
