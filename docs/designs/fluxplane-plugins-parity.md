# Design: fluxplane-plugins parity (the integration-plugin epic)

**Status:** planned · **Pillar:** Agent · **Layer:** L4 (`flux-plugin`) + the `plugins/` workspace ·
**Owner:** Timo · **Stories:** [D-12](../stories/D-12-plugin-protocol-parity.md) ·
[D-13](../stories/D-13-plugin-skill-command.md) · [D-14](../stories/D-14-deepen-native-plugins.md) ·
[D-15](../stories/D-15-observability-ai-plugins.md) · [D-16](../stories/D-16-datastore-infra-plugins.md) ·
[D-17](../stories/D-17-telephony-plugins.md)

## Why

flux shipped **8** native plugins under [D-08](../stories/D-08-integration-plugin-pack.md) over the
[D-10](../stories/D-10-process-plugin-protocol.md) protocol. The source they were modelled on —
`~/projects/fluxplane/fluxplane-plugins` — ships **26 marketplace plugins**, and flux's 8 cover only a
*fraction* of their operations (gitlab 6/60+, slack 5/30, jira 3/~20, k8s 5/24). The goal of this epic is
**full native parity**: every *portable* fluxplane plugin rewritten as a native flux plugin at full op
coverage, plus a generated **plugin skill** so the catalog is self-documenting to the agent.

"Native" matters: flux deliberately does **not** wrap fluxplane's Go binaries or speak MCP — each plugin is a
Rust subprocess on flux's own `host-kit` over the `flux.plugin.v1` protocol, capability-gated and inside the
same safety envelope as the agent's own tools.

## Parity matrix (the 26 marketplace plugins → flux disposition)

| Disposition | fluxplane plugins | Story |
|---|---|---|
| **Native, shallow** — port-deepen to full op set | confluence, gitlab, jira, kubernetes, loki, prometheus, slack, websearch | **D-14** |
| **Missing — HTTP** (needs non-Bearer auth) | alertmanager, grafana, opsgenie, huggingface | **D-15** |
| **Missing — raw-conn / SDK** | sql, docker, aws | **D-16** |
| **Missing — telephony** | asterisk, homer | **D-17** |
| **Covered differently — NOT ported** | clock→`now`, system→`sys_info`, sleep→builtin, git→tool group, openai/ollama→providers, duckduckgo/tavily→folded into flux `websearch` | — |
| **Deliberate divergence — NOT ported** | vision/websearch *aggregators*, openapi *generator* | — |

### Why some plugins are not ported (so "parity" is well-defined)
- **clock / system / sleep / git** — flux already exposes these as **builtin ops / a tool group**
  (`now`, `sys_info`, `sleep`, the `git` group in `flux-tools`), not as plugins. A plugin would be redundant.
- **openai / ollama** — these are **providers** in flux (the model layer), not integration plugins. Their
  fluxplane "ops" (image/vision/model.list, generate/chat/embed) are provider surface, addressed there.
- **duckduckgo / tavily** — flux's `websearch` plugin already **folds both backends in** (Tavily primary, DDG
  fallback); fluxplane split them into provider plugins behind an aggregator. flux's single plugin is simpler.
- **vision / websearch aggregators** + **openapi generator** — these rely on fluxplane's provider-call and
  spec-driven-generation surfaces that flux intentionally omits (see below). flux RAG-indexes OpenAPI specs
  via D-07 instead of generating a tool-per-endpoint plugin (an explicit D-08 non-goal).

## Protocol gap → D-12

flux's `SystemHostCaps` (`crates/flux-plugin/src/lib.rs`) services `process.run`, `secret` (by purpose, env),
`endpoint`, and `http.do` (Bearer-by-purpose injection + SSRF guard). The missing plugins need three
**additive** host capabilities, designed in [plugin-protocol-parity.md](plugin-protocol-parity.md):

1. **Non-Bearer auth injection** — Basic/header/query by purpose. jira/confluence hand-roll
   `Authorization: Basic base64(email:token)` *inside the plugin* today; alertmanager/grafana/homer/opsgenie
   need basic / `config` / `GenieKey`. Unblocks D-15 and lets D-14 delete the base64 hand-rolling.
2. **Raw connection dialer** (`conn.*`) — a guarded tcp/unix socket dialer (flux-system has none today, only
   `guard_url`). sql/docker/asterisk reach backends over a socket, not HTTP. Gates D-16/D-17.
3. **Blob store** (`blob.*`) — a `blob_ref` indirection so file-upload ops don't inline base64. Gates the
   file-upload ops in D-14/D-15.

**Deliberately omitted** from flux's protocol (the D-10 "drop fluxplane's cruft" decision): provider/
capability-call (the aggregator mechanism), context providers, evidence observers, and dual protocol modes.
Parity is **operational** (the integrations an agent can drive), not a byte-for-byte protocol clone.

## Skill generation → D-13

fluxplane's `fluxplane-plugin skill` command *generates* a Claude-format `SKILL.md` + `references/<plugin>.md`
from installed-plugin manifests (that is exactly what produced `~/.claude/skills/fluxplane-plugin/`). flux
gets the analogue **`flux plugin skill`**, designed in [plugin-skill-generation.md](plugin-skill-generation.md):
it renders the discovered flux-plugin manifests into a trigger-activated `flux-plugins` skill so the agent
knows which `flux plugin call` ops exist, their inputs, and their auth — without hard-coding a catalog.

## Sequencing

```
D-12 (protocol: auth → conn → blob)         D-13 (flux plugin skill)   ← this session: D-13 + D-12 start
        │                                            │
        ├── D-14 (deepen the 8, drop base64) ────────┤  (skill refresh after each)
        ├── D-15 (alertmanager, grafana, opsgenie, huggingface)   [needs auth]
        ├── D-16 (sql, docker, aws)                              [needs conn + blob]
        └── D-17 (asterisk, homer)                               [needs conn]
```

D-13 is independent (no protocol dependency) and ships first. D-12's auth slice is the first protocol
deliverable; conn + blob follow. The plugin-port stories (D-14…D-17) consume D-12 and run in later sessions.

## Authoring pattern (for the port stories)

Each plugin is a `plugins/<name>/` crate in the nested workspace (excluded from the root gate), binary
`flux-plugin-<name>`, built on `host-kit`'s `PluginBuilder` + `read_op`/`write_op` + `MockHost` (reference:
`plugins/gitlab/src/main.rs` for HTTP, `plugins/kubernetes/src/main.rs` for CLI). Op shapes are copied (not
code) from `~/projects/fluxplane/fluxplane-plugins/<plugin>/manifest.go`. Each slice: full gate green in
`plugins/`, a smoke entry in `scripts/smoke-plugins.sh`, and `flux plugin skill refresh` to regenerate the
catalog.

## Non-goals
- Wrapping fluxplane's Go binaries, or any MCP bridge — plugins are native Rust.
- The omitted protocol surfaces (provider-call, context, evidence, dual modes).
- A plugin marketplace / `.dex`-style endpoint registry.
- Porting the builtin/provider-covered plugins (clock/system/sleep/git/openai/ollama/duckduckgo/tavily).
