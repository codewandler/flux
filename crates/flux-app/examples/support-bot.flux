# support-bot.flux — a Slack support agent, the whole app in native flux-lang: an agent, the Slack
# channel it is reached on, the docs datasource it answers from, and the trigger that wakes it per
# message. On a Slack mention the model reads the message, calls `search` over the indexed docs, and
# its answer is posted back into the thread.
#
# Running it needs, in the environment:
#   SLACK_BOT_TOKEN / SLACK_APP_TOKEN  — a Slack app in Socket Mode (see the Slack channel setup guide:
#                                        website/docs/agent/slack-channel.md)
#   ANTHROPIC_API_KEY                  — credentials for the agent's model
# then, from ANY directory (the `./docs` corpus below resolves next to THIS file, not your cwd):
#   flux app run crates/flux-app/examples/support-bot.flux
#
# Secrets are environment-variable *references* (`secret "NAME"`), never inline plaintext — the host
# resolves them at load and redacts the values from all logs.

agent assistant
  model "claude-sonnet-5"
  tools [search]
  datasources [docs]
  description "answers support questions from the docs"

channel slack
  bot_token secret "SLACK_BOT_TOKEN"
  app_token secret "SLACK_APP_TOKEN"

datasource docs
  kind "markdown"
  path "./docs"

trigger on_message
  on "slack"
  agent assistant
