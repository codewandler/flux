---
title: "flux 0.42.0 — rooms, and the history you already have"
description: A Room port and a real XMPP backend let flux be one participant among several. Local harness transcripts become searchable, contained by construction. Plus per-child resource ceilings, and a memory-eating prune caught before it had a caller.
slug: flux-0-42-0-rooms-and-history
tags: [release, channels, datasource, runtime]
date: 2026-07-31
---

**0.42.0 is a minor release**, which pre-1.0 is how this project signals a breaking change. Two of
them: `VoiceTurnHandler::turn` now carries a `Speaker`, and `SubAgents` gained a public field. If you
implement a voice handler or build `SubAgents` with a struct literal, read the [action-needed
section](#action-needed) before upgrading.

The headline is that flux can now sit in a room with several people. The subtler half is that three
of the nine stories in this cut are **foundations whose consumers are not wired yet** — and saying so
plainly is more useful than a feature list that implies otherwise.

<!-- truncate -->

## Rooms — one participant among several

Every channel flux had was one-to-one or fire-and-forget. `schedule`, `webhook`, `slack` and `a2a`
wake a journey and return. The voice path was richer but assumed **exactly one caller**, because on a
phone line there is only one candidate.

A room breaks that assumption, and the break is not cosmetic: **every inbound event now carries an
occupant id**. Attribution is not a feature layered on top of a room, it is the precondition for one.
Without knowing who spoke, "answer only when addressed" cannot even be expressed.

Two stories landed together. The **`Room` port** (`join` / `occupants` / `say` / `whisper` / `leave`,
plus a `Joined`/`Left`/`Message`/`Ended` event stream) arrives as an ordinary channel kind, so a room
reaches an agent through the same machinery every other channel uses — and a room-sourced turn
dispatches through the ordinary executor and approver, which is asserted by a test rather than
assumed.

The **XMPP MUC backend** then makes it real against any standards-compliant server — prosody,
ejabberd, or a hosted JaaS tenant — with **no browser and no vendor SDK**.

### Why the dependency choice was a safety decision

The obvious move was an existing XMPP client crate. We rejected it, and the reason is worth stating
because it generalises: `tokio-xmpp` opens its own TCP socket and resolves its own DNS. That means its
egress **cannot** be routed through flux's network guard — every outbound request in flux passes one
guard that resolves hostnames and blocks private, loopback and link-local ranges unless the caller
holds a scoped grant. Using that crate would have meant either a bypass or a second guard, and the
invariant forbids both.

So we took `quick-xml` — a *parser*, not a client — and wrote the protocol here, about 200 lines of
element tree. It is the only new package in the lockfile, and no second TLS stack came with it.

Two details from a live spike are now regression tests, asserted against the raw frames the server
actually reads: every stanza is namespace-qualified (prosody answers `<unsupported-stanza-type/>` and
kills the stream otherwise), and the keepalive is an XMPP ping, never a whitespace frame — a bare
space gets the connection closed with `1007`.

### What is deliberately not finished

**The agent replies to every line said in the room.** The addressing rule is carried in settings and
not yet enforced; that is the next story. Under the in-process test double this was theoretical. With
a real backend it is a live cost, and you should treat a room as something you put flux into
deliberately, not something to leave running.

## The history you already have

If you use codex, claude-code or opencode, you have years of transcripts on disk. Their token-usage
parsers already walk right past the message text to reach the eight integers they came for.

This release extracts that text — normalising roles across three different vocabularies — and exposes
it as `search(query, harness)`.

**The containment is the story, not a caveat on it.** Because this is the change that exposes the
data, it is the change that has to contain it, and each property is structural rather than
maintained:

- **Off by default**, by construction — the default tool pack *literally is* the history-enabled one
  called with history disabled, so there are not two declarations that could drift apart.
- **Redacted and escaped at ingest**, not at render. The test asserts on the record sitting in the
  index, because redaction-at-render is the failure mode that looks identical from a passing test.
- **Per-harness permission**, so granting search over one harness is not granting it over all.
- The escaper is the one flux already uses for untrusted knowledge-base bodies, exported as a
  one-line delegation so the two callers cannot drift into two schemes.

Nothing in flux wires this up yet. That is deliberate: extraction shipped one release before this
one specifically so unredacted text could not reach a model-visible surface before redaction existed.

Two limits are documented rather than claimed solved: the over-fetch behind the harness filter is a
**heuristic, not a bound** — a harness holding many better-scoring hits can still cause a silent
under-return — and a host must pass **one** history handle to both ingest and registration, because
they take independent handles today.

## Resource ceilings that actually reach something

A configured `[limits]` table now binds for the `flux` binary and descends into sub-agents. It is
**per-child**: `max_concurrent_tool_calls = N` bounds *each* agent, so a process with k live children
may run up to N×(k+1).

That is weaker than it sounds and we are saying so rather than letting the name imply otherwise. A
shared ceiling was built first — and reproduced a real deadlock, where a parent holds a permit while
awaiting a child that queues on the same semaphore. Per-child is safe by construction: parent and
child hold different semaphores, so no ancestor can block a descendant.

Not covered: `flux app run` assembles its own environment and still ignores `[limits]` entirely.

## A prune that would have eaten your memory

`memory:*` streams carry no registry row, which is exactly what the ad-hoc stream prune targets. An
aged memory was indistinguishable from an aged scratch stream and would have been swept.

It was caught before it could bite — the prune has no caller anywhere in the tree — so this closes a
trap rather than fixing an incident. Whether memory should *ever* be prunable by age is now answered
in writing rather than left implicit: it should not be, and `forget` remains the deliberate path.

## Also in this release

- **Flux Glyph**, a compact indented opcode projection of a Flux program, for agents. The round-trip
  is proven as a property across every node kind in three nesting positions, plus hundreds of seeded
  random programs. `.flux` and the runtime are untouched.
- **`pane.open` / `update` / `close`** — an agent can open its own panes. Shipped **partial**: the ops
  are surfaced only when a host has a surface sink, and nothing installs one yet, so the vocabulary is
  currently inert.

## Action needed

- **If you implement a voice handler**, its `turn` method now takes a speaker alongside the text. If
  you only use flux's built-in phone/voice support, the single caller on a line is simply named now
  instead of anonymous, and nothing changes for you.
- **If you construct `SubAgents` with a struct literal**, add the new field or switch to
  `SubAgents::new`.

Full engineering detail, including the review findings behind each change, is in the
[CHANGELOG](https://github.com/codewandler/flux/blob/main/CHANGELOG.md).
