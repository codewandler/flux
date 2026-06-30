# hello.flux — a tiny multi-agent app, the whole thing in native flux-lang.
# Run it with `flux app run crates/flux-app/examples/hello.flux` (uses only pure ops; no model needed).

agent greeter
  description "greets on startup and echoes user input"

channel cli

trigger on_start
  on "startup"
  run greet

trigger on_input
  on "user_input"
  run echo

journey greet
  agent greeter
  flow
    send({ "channel": "cli", "message": "Hello from flux-app!" })
    return "Hello from flux-app!"

journey echo
  agent greeter
  flow
    $reply = fmt("you said: {text}")
    send({ "channel": "cli", "message": $reply })
    return $reply
