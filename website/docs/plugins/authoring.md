---
title: Plugin authoring
---

# Plugin authoring

Plugins extend flux by exposing operations over a framed protocol. A plugin is not a bypass around the
runtime. It receives only the host capabilities declared in its manifest, and every operation is
projected as a policy-gated tool.

Core rules:

- The host does privileged IO.
- Plugin capabilities are deny-by-default and manifest-scoped.
- Secrets are requested through declared secret purposes.
- Process and network access use host callbacks, not ambient environment access.
- Tool effects, risk, and idempotency must be declared honestly.

The canonical in-repo authoring guide is:

https://github.com/codewandler/flux/blob/main/plugins/AUTHORING.md

This public page is intentionally short until the plugin surface is packaged for non-source users.
