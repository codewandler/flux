---
id: D-98
title: flux-web plugin + http.request op (re-home native web_fetch)
pillar: Core
status: backlog
priority:
epic:
design:
note: "future: a flux-web plugin (download a URL → markdown) + a plain http.request op, both under normal plugin host-caps (declared hosts, private-net grant, redaction); once they exist, retire web_fetch's special native private-net path (cf. D-96)"
---

# flux-web plugin + http.request op (re-home native web_fetch)

## Goal
Move web fetching out of the native tool surface and into the plugin model, so web egress is subject
to the same declared-hosts / private-net-grant / redaction envelope every other integration uses —
rather than `web_fetch` being a special native case with its own private-net handling.

## Acceptance
- [ ] A `flux-web` plugin that downloads a URL and converts it to markdown, using the host `http`
      capability (no `reqwest`/`std::net` in the plugin).
- [ ] A plain `http.request` op (arbitrary method/headers/body) gated by the standard plugin
      host-caps: manifest-declared hosts, the scoped private-net grant, and secret redaction.
- [ ] Once both exist, `web_fetch`'s bespoke native private-net path is retired (it currently has no
      manifest safeguard — see the caveat on `--allow-private-net` in [D-96](D-96-allow-private-net-cli-override.md)).

## Progress
- Not started (captured from the D-96 discussion).

## Notes
- Motivation: `--allow-private-net` (D-96) fully opens `web_fetch` to private ranges for a run because
  the native tool has no manifest declaration to intersect against; a plugin-hosted fetch would be
  gated the same as any other plugin.
