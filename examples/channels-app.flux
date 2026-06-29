# channels-app.flux — a background agent woken by events, the whole app in native flux-lang.
#
# Run it with `flux app run examples/channels-app.flux` (Ctrl-C to stop). A cron heartbeat fires every
# 5s and an inbound webhook accepts CI events; each channel fires a bus event under its own name and the
# matching trigger runs a journey. These journeys use only pure ops, so no model/credentials are needed.

channel heartbeat
  kind "schedule"
  schedule "*/5 * * * * *"

channel ci
  kind "webhook"
  addr "127.0.0.1:8799"
  path "/ci"

trigger on_boot
  on "startup"
  run announce

trigger on_beat
  on "heartbeat"
  run tick

trigger on_ci
  on "ci"
  run ci_report

journey announce
  flow
    send({ "channel": "cli", "message": "channels-demo up — heartbeat every 5s; POST JSON to http://127.0.0.1:8799/ci" })
    return ""

journey tick
  flow
    $m = fmt("heartbeat at {at}")
    send("cli", $m)
    return $m

journey ci_report
  flow
    $r = fmt("received CI event: {status}")
    return $r
