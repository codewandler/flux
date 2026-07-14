# Manual skill activation

## Decision

Skill discovery and skill activation are separate. Flux may scan frontmatter from project, configured,
and user directories to resolve names, but it never injects a body because words in a user message
matched `name`, `description`, or `triggers`.

The CLI activates a skill only through repeatable `--skill <name>`. `--skill-dir` and `[skills].dirs`
add discovery locations; they do not activate their contents. An unknown requested name fails before
provider construction. The SDK follows the same principle: `AgentSpec.skills` is the explicit active
set, and an empty set stays empty. Calling `AgentSpec::try_with_default_skills` is itself an explicit,
deliberately broad opt-in.

Every enabled skill is injected on each turn of that agent. This keeps the prompt stable and makes
the behavioral policy inspectable from the agent definition/CLI invocation. Metadata triggers remain
parseable for compatibility and a future evaluated intent router, but are inert in the production
prompt path.

## Why now

A live C-54 request trace showed two false positives on one trivial prompt: the 19.4 KB deprecated
Flux-Lang mirror, then the unrelated self-improvement skill on the word `agent`. Tightening keyword
heuristics would still let unproven prompt mutations happen implicitly. Manual activation is the
safe baseline; semantic routing can return only after measured quality/latency comparisons.

With the same plugin-heavy workspace, the false-activation case reported 28,449 input tokens. The
manual-only live probe injected no skill and reported 23,283 input tokens, a reduction of 5,166
tokens (18%). With no `--skill`, the CLI also returns before scanning any skill directory.
