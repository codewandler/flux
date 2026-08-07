# Design — Operating a host you deployed

**Status:** planning · **Pillar:** Core · **Stories:** C-674, C-676, C-684 (substrate) · C-685,
C-686, C-687 (agent surface) · builds on the shipped work in
[remote-agents.md](remote-agents.md) and [first-class-hosts.md](first-class-hosts.md)

## Why

The motivating scenario is one cluster and one laptop: a `flux-host` deployment in a local k3s,
and an operator who wants to use it — run processes there, read and write files there, reach the
network from there, and eventually have an agent *live* there that they watch from their own
terminal. Everything needed to answer that is already in the tree; what is missing is the last
mile in two places, and a clear statement of which of the two "remote" things a given step is.

## The two axes, because "remote host" names both

| | **Remote substrate** | **Remote agent** |
|---|---|---|
| what moves | only where guarded effects land | the whole agent: planning, model calls, session |
| what stays local | the runtime, model, credentials, approvals, session store | a thin client |
| far side runs | `flux system serve` | `flux app run --serve` |
| you select it with | `--host <name>` / `--remote <url>` | `flux a2a <url>` (or the TUI, after C-686) |
| the property it preserves | you approve here, it happens there | someone else's machine owns the whole loop |

Neither is telnet, and neither is a channel. **Channels** (cron/webhook/Slack) belong to a
*program*: `flux app run` serves the channels its program declares, wherever that program runs. A
webhook channel in a pod is an HTTP endpoint in that pod; reaching it is ingress, not a flux
transport.

## What already stands

The substrate axis is further along than it looks. The remote protocol's bounded vocabulary
already carries **twenty** operations — process (`run`, `run_stdin`, `spawn`), network (`dial`,
`bind_tcp`, `bind_udp`), env, host identity/read/metrics, and the full workspace file surface
(`read_bytes`, `write_bytes`, `append`, `read_capped`, `file_size`, `path_exists`, `is_dir`,
`file_mtime`, `list_dir`, `walk_files`). So "FS + network too" is not future work: it is the
delivered v1 contract, gated the same way as everything else — authenticated TLS, exact-version
negotiation, per-request delivery ledger, and the guarded port's fail-closed defaults on the far
side.

The agent axis is likewise largely shipped: agent-card discovery, `POST /a2a` JSON-RPC with
`message/send` and `message/stream`, and a sessions API, with `flux a2a <url>` as the client.

## The k3s walkthrough, as it works today

**Substrate.** Deploy C-480's Kustomize base (namespace, PVC, TLS Secret, bearer Secret,
ClusterIP, default-deny NetworkPolicy) running `flux system serve`. Reach it from the workstation
by whatever ingress the cluster already has — a port-forward is the zero-config one — then declare
a `[[host]]` binding at that endpoint and select it with `--host`. Every guarded effect lands in
the pod; the model call, the approval prompt and the session stay on the laptop.

Two frictions are real and filed: the pod's certificate is normally cluster-issued, and a named
binding has no way to trust a private CA (**C-684**); and pointing a binding at a *pod* rather
than a URL is what the `kubernetes` backend kind is for, still unwired (**C-676**). HTTP effects
do not cross the wire yet, so `http.request`/`web.fetch` under such a selection refuse honestly
until the versioned wire change (**C-674**).

**Agent.** Run `flux app run --serve` in the cluster and talk to it with `flux a2a`. A
non-loopback agent listener must be authenticated — that is a release boundary, not a
recommendation — and the approval posture must be chosen explicitly. There is no deployment
profile for this surface yet (**C-685**), the experience is a line REPL rather than the TUI
(**C-686**), and multi-operator approval is capped by the missing supervisor authorization model
(**C-687**), which is also what blocks the fleet's multi-operator supervision.

## The plan

1. **Make the deployed substrate reachable by name** — C-684 (private CA), then C-676 (a
   `kubernetes` binding that resolves a pod endpoint instead of a hand-held URL). After these,
   `flux --host flux-host` is the whole story for a cluster substrate.
2. **Close the effect surface** — C-674 puts HTTP on the wire, after which the twenty operations
   plus HTTP cover everything a coding agent does remotely.
3. **Make the agent surface deployable** — C-685, reusing the released image rather than a second
   one, with the authentication and posture requirements encoded in the manifests.
4. **Make it watchable** — C-686 attaches the TUI to an agent that lives on a host, which is the
   step that turns "an agent runs in my cluster" into something an operator can supervise.
5. **Make it shareable** — C-687 defines who may answer an approval, which is the precondition for
   more than one human operating the same deployed agent, and for the fleet coordinator's own
   decisions.

## Boundaries this plan keeps

Flux does not provision infrastructure: no story here creates a cluster, a node, a container or a
guest. Bindings consume endpoints that already exist, and the deployment artifacts document how an
operator makes one exist. A remote artifact is still verified before acceptance, a credential is
still a reference rather than a value, and publication remains a human word.
