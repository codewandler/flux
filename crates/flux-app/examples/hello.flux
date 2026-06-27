{
  "name": "hello-app",
  "agents": [
    { "name": "greeter", "description": "greets on startup and echoes user input" }
  ],
  "channels": [
    { "name": "cli", "kind": "cli" }
  ],
  "triggers": [
    { "name": "on_start", "on": "startup", "run": "greet" },
    { "name": "on_input", "on": "user_input", "run": "echo" }
  ],
  "journeys": [
    {
      "name": "greet",
      "agent": "greeter",
      "flow": {
        "name": "greet",
        "body": [
          {
            "kind": "call",
            "op": "send",
            "args": [{ "kind": "lit", "value": { "channel": "cli", "message": "Hello from flux-app!" } }]
          },
          { "kind": "return", "value": { "kind": "lit", "value": "Hello from flux-app!" } }
        ]
      }
    },
    {
      "name": "echo",
      "agent": "greeter",
      "flow": {
        "name": "echo",
        "body": [
          {
            "kind": "bind",
            "name": "reply",
            "value": { "kind": "fmt", "template": "you said: {text}" }
          },
          {
            "kind": "call",
            "op": "send",
            "args": [
              { "kind": "lit", "value": "cli" },
              { "kind": "var", "name": "reply" }
            ]
          },
          { "kind": "return", "value": { "kind": "var", "name": "reply" } }
        ]
      }
    }
  ]
}
