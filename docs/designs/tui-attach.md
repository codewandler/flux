# Design — Attaching the TUI to an agent that lives on a host

**Status:** shipped (v1) · **Pillar:** Core · **Stories:** [C-686](../stories/C-686-the-tui-attaches-to-an-agent-that-lives-on-a-host.md) ·
builds on [operating-a-deployed-host.md](operating-a-deployed-host.md) and
[remote-agents.md](remote-agents.md)

## Why, and which "remote" this is

[remote-agents.md](remote-agents.md) opens with the distinction this design lives inside: *"remote
agent" means two different things.* This story is the **agent axis** — the whole agent (planning,
model calls, tools, session) lives on another machine and you attach a viewer to it. It is **not**
the substrate axis, which is what `--remote <url>` and `--host <name>` already select: a **local**
agent whose guarded effects land elsewhere.

Those are opposite postures, and confusing them is a safety problem, not a documentation one — with
`--remote` you approve on your machine; with an attached agent the approval stage is the remote's,
and your terminal is a window onto it. So the selection is a **third, mutually exclusive** one:

| you type | what moves | who approves | where the session lives |
|---|---|---|---|
| `flux tui` | nothing | you, locally | your `~/.flux/events.db` |
| `flux tui --remote <url>` / `--host <name>` | only where effects land | you, locally | your `~/.flux/events.db` |
| `flux tui --attach <url\|name>` | **the whole agent** | the remote's posture | **the remote's event store** |

`--attach` is refused in combination with `--remote` and `--host` at parse time. That refusal is the
mechanism: the confusable pair cannot be spelled together, so nobody discovers the difference by
watching an effect land in the wrong place.

## The seam: how remote turns reach the panes without pretending to be local events

The TUI renders from a `ChatState` mutated by a `UiEvent` stream (`crates/flux-tui/src/controller.rs`).
In ordinary mode that stream is produced by `ChannelSink`, an `AgentSink` the local `FlowEngine`
drives. Attach mode does **not** install a second `AgentSink`, and it never appends to the local
event store: a remote agent's turn is not a local session event and must not be fabricated into one.

Instead a small, protocol-neutral seam sits beside the local one:

```
served agent  ──A2A──▶  flux_a2a::attach::AttachedA2aAgent  ──AttachUpdate──▶  flux_tui::attach
                                                                                    │
                                                        (translated, never forged)  ▼
                                                                                 UiEvent
                                                                                    ▼
                                                                                 ChatState
```

- **`flux_tui::attach`** (`crates/flux-tui/src/attach.rs`) declares `AttachedAgent`, an
  object-safe async trait, plus `AttachUpdate` — a deliberately *small* vocabulary (`Text`,
  `State`, `Artifact`, `Notice`, `Ended`) that says only what the A2A protocol actually carries.
  It names no protocol type, so `flux-tui` gains no dependency and stays renderer-only.
- **`flux_a2a::attach`** (`crates/flux-a2a/src/attach.rs`) implements the protocol half against the
  shipped `flux a2a` client — the same `A2aClient`, the same bearer handling, the same origin lock.
  There is exactly one A2A client in the tree.
- **`flux-cli`** owns the ~1:1 translation between the two vocabularies and the flag wiring.

The split is forced by layering (`flux-tui` is L6, `flux-a2a` is L1, and L1 may not see L6) and it
is the right shape anyway: the surface renders, the protocol crate speaks the protocol.

**Why `AttachUpdate` is small.** flux's served `message/stream` emits **text deltas and lifecycle
status only** — `StreamSink` in `crates/flux-server/src/a2a.rs` implements `AgentSink::text_delta`
and nothing else. The remote agent's *tool calls and tool results do not cross the A2A wire at
all.* A vocabulary with a `ToolCall` variant would therefore be a promise the transport cannot
keep, and the tool pane would sit permanently empty with no explanation. So the attached header
states the limitation instead, and the gap is filed rather than faked (see *Gaps* below).

## What is authoritative for history, and what is local

**The remote is authoritative.** The attached session's history lives in the served agent's event
store and is read back over `tasks/get`, whose `Task.history` is projected from that store
(`a2a_history`, `crates/flux-server/src/a2a.rs`). On reattach the TUI replays *that*, not a local
mirror, so what the pane shows is what the remote actually recorded — including the turns that
happened while the operator was detached.

**Nothing local is authoritative, because nothing local is written.** In attach mode the TUI:

- creates **no** local session (`ChatState::attached` carries an empty `session_id`);
- appends **no** events to `~/.flux/events.db`;
- keeps its transcript **in memory only**, as a rendering of the remote's stream.

Consequently a remote agent's session never appears in `flux sessions` and can never be `flux
replay`ed. That is deliberate and is the story's fifth acceptance item: a session listed locally
must be one whose evidence chain is local. Tagging a synthetic local session `agent_id =
"attached"` was considered and rejected — it would put a row in `flux sessions` whose `replay`
could only ever be a lie, because the plan, the dispatches and the evidence are all on the other
machine. The honest answer is absence plus a documented export path (the remote's own
`flux sessions`/`replay`, or `tasks/get` over the wire).

Slash commands that would silently act on the *local* engine while the operator believes they are
steering the remote (`/model`, `/compact`, `/new`, `/clear`, `/evidence`) are refused in attach
mode with a line that names which machine owns the thing they asked for.

## Steering, and what the protocol supports

| affordance | how | when it is unavailable |
|---|---|---|
| send a message into the live session | `message/stream` (or `message/send` when the card declares no streaming) keyed to one stable `contextId` | never — this is the floor |
| cancel a running turn | `tasks/cancel` on the live task id | the *embeddable* dispatch classifies `tasks/cancel` unsupported; Ctrl-C then says so and the remote turn continues |
| replay history | `tasks/get` → `Task.history` | before the first turn of a fresh attach (no task id exists yet) |
| reattach a dropped stream | `tasks/resubscribe` on the live task id | a task that is neither live nor retained |
| answer an approval | `GET /approvals` + `POST /approvals/{id}` echoing `fingerprint` | see below |

Every one of these is probed at connect time and rendered as a capability line. An affordance that
the served agent does not offer is shown **disabled with its reason**, never left inert.

## Approvals, and the honest limit

The served side already has the whole mechanism (C-453): `--remote-approval` parks each guarded
effect at `GET /approvals` and takes one decision at `POST /approvals/{id}`, bound to the effect by
an echoed `fingerprint`. Attach mode reuses the TUI's **existing** approval sheet: a polled pending
request becomes the same `UiEvent::Approval` a local effect raises, and the operator's `y`/`n`/`d`
answer is POSTed back with the fingerprint verbatim. There is no second approval concept and no new
authorization model.

The posture is probed, and each answer is rendered as itself:

- `200` — the remote parks effects and this credential may answer them. The header says so, **and
  says that answers are attributed to the deployment's shared operator token**, because that is the
  only posture the served side supports.
- `501` — the remote asks nobody (a headless approver constrained by policy, sandbox and budgets).
  This is a *statement of posture*, not an error, and is rendered as one.
- `401`/`403` — the remote parks effects but this credential cannot answer them.
- anything else — unknown, with the transport's own words.

**No approvals are invented.** Principal-authenticated approval is refused by the server at router
construction (one deployment-wide queue would let one tenant answer another's effects), and closing
that is **C-687**, not this story. Until then, multi-operator supervision of one attached agent is
capped, and the TUI says that in the same line where it says approvals are answerable.

## Credentials

`--attach-token-env <ENV>` names the **environment variable**, defaulting to `FLUX_A2A_TOKEN` — the
same credential `flux a2a` uses. The value is never a command-line argument, never printed, and
never part of the header label. A named binding (`[[endpoint.static]]` with `protocol = "a2a"`)
carries a `credential_ref` location instead, resolved exactly like a `[[host]]` binding's: the
`env` scheme resolves, and any other scheme is refused by name rather than silently ignored.

## Gaps found and not papered over

Implementing against the shipped served surface surfaced three real holes. None is fixed here — the
story's scope is the client, and bolting server routes on as a side effect would land unreviewed
protocol surface:

1. **Tool activity does not cross the A2A wire.** `message/stream` carries text deltas only, so an
   attached operator watches prose appear with no idea which files were read or which commands
   ran — precisely the visibility the TUI exists for. The `AgentSink` already has `tool_call` /
   `tool_result`; the served `StreamSink` simply does not implement them. Candidate story: emit
   tool activity as `TaskArtifactUpdateEvent` or `data` parts on the A2A stream.
2. **A `contextId` cannot be resolved to its task id without running a turn.** Task id *is* the
   session id server-side, and `find_correlated` maps a context to a session — but no read-only
   route exposes that mapping, so a fresh process attaching to an existing conversation cannot
   replay its history until it has sent something. Candidate story: a `contextId → task` lookup, or
   history on `GET /sessions/{id}`.
3. **`GET /sessions/{id}` carries no history** — only `{id, model, created_at_ms}` — so the REST
   sessions API cannot answer the reattach question either.

## Alternatives considered

- **Overload `flux tui --remote` to mean both axes.** Rejected outright: the flag ships today
  meaning the opposite thing, and an operator who mis-reads it approves on the wrong machine.
- **Mirror the remote session into the local event store** so `flux sessions`/`replay` work.
  Rejected: a replay needs the plan, the dispatches and the evidence, all of which are remote. A
  local row that replays into nothing is worse than no row.
- **Write a second, TUI-owned A2A client.** Rejected — `flux a2a`'s client already handles
  discovery, bearer auth, the origin lock and SSE decoding. Attach widens it (`tasks/resubscribe`,
  a generalized RPC stream) rather than forking it.
- **Add a `[[agent]]` config table for named served agents.** Rejected for v1:
  `[[endpoint.static]]` is already a named url + `credential_ref` binding and adding
  `protocol = "a2a"` costs no new vocabulary. `[[host]]` was rejected specifically because it is
  the *substrate* axis — reusing it would re-fuse the two things this design exists to separate.
