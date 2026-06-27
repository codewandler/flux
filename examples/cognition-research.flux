{
  "name": "cognition-research",
  "returns": { "named": "Answer" },
  "body": [
    {
      "kind": "bind",
      "name": "need",
      "ty": { "named": "Need" },
      "value": {
        "kind": "call",
        "op": "need",
        "args": [
          {
            "kind": "lit",
            "value": {
              "ask": "what changed in enterprise pricing since 2026-01-01?",
              "require": ["date", "plan", "price", "source"]
            }
          }
        ]
      }
    },
    {
      "kind": "bind",
      "name": "src",
      "value": {
        "kind": "call",
        "op": "grep",
        "args": [{ "kind": "lit", "value": "enterprise pricing" }]
      }
    },
    {
      "kind": "bind",
      "name": "ranked",
      "value": {
        "kind": "call",
        "op": "sort",
        "args": [{ "kind": "lit", "value": { "items": [], "by": "confidence", "order": "desc" } }]
      }
    },
    {
      "kind": "bind",
      "name": "claims",
      "value": {
        "kind": "call",
        "op": "top",
        "args": [{ "kind": "lit", "value": { "items": [], "n": 8 } }]
      }
    },
    {
      "kind": "ctx",
      "name": "pack",
      "purpose": "the evidence backing the pricing answer",
      "include": ["src", "claims"],
      "budget": 6000
    },
    {
      "kind": "bind",
      "name": "open",
      "value": {
        "kind": "call",
        "op": "gaps",
        "args": [{ "kind": "lit", "value": { "claims": [], "need": {} } }]
      }
    },
    {
      "kind": "repeat",
      "max": 2,
      "until": { "kind": "var", "name": "open" },
      "body": [
        {
          "kind": "bind",
          "name": "more",
          "value": {
            "kind": "call",
            "op": "grep",
            "args": [{ "kind": "lit", "value": "pricing change" }]
          }
        },
        { "kind": "ctx_append", "ctx": "pack", "add": ["more"] },
        {
          "kind": "bind",
          "name": "open",
          "value": {
            "kind": "call",
            "op": "gaps",
            "args": [{ "kind": "lit", "value": { "claims": [], "need": {} } }]
          }
        }
      ]
    },
    {
      "kind": "bind",
      "name": "cited",
      "value": {
        "kind": "call",
        "op": "cite",
        "args": [{ "kind": "lit", "value": { "claims": [] } }]
      }
    },
    { "kind": "return", "value": { "kind": "var", "name": "cited" } }
  ]
}
