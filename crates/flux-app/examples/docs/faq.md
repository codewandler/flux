# Frequently asked questions

## What models can an agent use?

Name an explicit model id on the agent, for example `model "claude-sonnet-5"`. The agent needs
provider credentials in the environment (for example `ANTHROPIC_API_KEY`) to run.

## How does the agent answer from my docs?

Declare a `datasource` of `kind "markdown"` pointing at a folder, and grant the agent the `search`
tool. The host indexes every `.md`, `.txt`, `.rst`, `.adoc`, and `.mdx` file under that folder (up to
1000 files, 200 KB each). At runtime the agent calls `search` to retrieve the passages it answers from.

## Where is the docs folder resolved from?

A relative datasource path such as `path "./docs"` resolves against the program file's own directory —
not the directory you launched from. So `flux app run /any/where/support-bot.flux` finds the `./docs`
shipped beside the program, from any working directory.

## Does the agent reply in the same Slack thread?

Yes. The Slack channel posts the agent's answer back into the thread the message came from, and keeps
one conversation session per thread, so follow-up questions have context.

## How do I restrict who can use the bot?

Add `allow_users [...]` and/or `allow_channels [...]` to the `channel slack` declaration. Empty lists
(the default) allow everyone.
