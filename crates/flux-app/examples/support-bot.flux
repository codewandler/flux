# support-bot.flux — a Slack support agent, the whole app in native flux-lang: an agent, the Slack
# channel it is reached on, the docs datasource it answers from, and the journey that runs per message.
#
# Secrets are environment-variable *references* (`secret "NAME"`), never inline plaintext — the host
# resolves them at load. Running it needs SLACK_BOT_TOKEN / SLACK_APP_TOKEN in the environment and a
# `./docs` directory to index: `flux app run crates/flux-app/examples/support-bot.flux`.

agent assistant
  model "claude-sonnet-4-6"
  tools [search, send]
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
  run answer
  agent assistant

journey answer
  agent assistant
  flow
    $hits = search($text)
    return $hits
