---
title: Configuration
---

# Configuration

flux is local-first and opt-in. Configuration controls providers, permissions, shell access, skills,
plugins, and app/runtime behavior.

Important public defaults:

- Generic shell execution is opt-in.
- Destructive effects require approval.
- Plugin capabilities are deny-by-default.
- Non-loopback server binds require authentication.
- Secrets should be referenced, not embedded as literals.

The exact config keys are still evolving with the CLI and SDK surfaces. Use `flux --help`, command-level
help, and the repository docs for the current implementation details.
