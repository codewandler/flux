---
title: Providers and models
---

# Providers and models

flux keeps provider transport separate from the agent runtime. A provider is the model-facing wire
codec plus credential source. The runtime remains responsible for executing plans and gating IO.

Model names are routed by provider and model, for example:

```bash
flux run -m openai/gpt-4.1 "summarize this repository"
flux run -m anthropic/claude-sonnet-4 "fix the failing test"
```

Exact provider names and supported models can change as providers evolve. Prefer the CLI's current
help output and the repository's model documentation for the live matrix.
