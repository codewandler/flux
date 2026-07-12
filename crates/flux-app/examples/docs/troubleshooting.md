# Troubleshooting

## The program exits with "channel `slack` … built with --no-default-features"

The Slack adapter is compiled in by default. You only see this if you built the binary with
`--no-default-features`. Rebuild without that flag, or re-enable the `slack` feature.

## The bot starts but answers "I don't have anything on that"

The docs index is probably empty. Check that your `datasource` path points at a folder that actually
contains markdown files, and that you are pointing at the right folder — relative paths resolve
against the program file's directory. Files larger than 200 KB are skipped.

## The program fails at startup naming an environment variable

A `secret "NAME"` reference could not be resolved because that environment variable is unset. Export
the variable (for example `export SLACK_BOT_TOKEN=xoxb-…`) before running. The error names the missing
variable, never its value.

## Slack never delivers messages to the bot

Confirm the Slack app has Socket Mode enabled, an app-level token with `connections:write`, the
`app_mentions:read` and `chat:write` bot scopes, and that it is subscribed to the `app_mention` event.
See the Slack channel setup guide for the full walkthrough.
