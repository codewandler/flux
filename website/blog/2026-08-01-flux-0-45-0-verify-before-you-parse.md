---
title: "flux 0.45.0 — verify before you parse"
description: A webhook now authenticates before it decodes, over the exact bytes the sender signed. Four hand-rolled redaction walkers become one that cannot narrow. A redaction that fails no longer hands back the original. Plus the editor grammar finally parses the language flux ships.
slug: flux-0-45-0-verify-before-you-parse
tags: [release, security, channels, flux-lang]
date: 2026-08-01
---

**0.45.0 is a minor release**, which pre-1.0 is how this project signals a breaking change. One of
them: a webhook that listens beyond your own machine must now state what it does about verification.
If that is you, read the [action-needed
section](/blog/flux-0-45-0-verify-before-you-parse#action-needed) before upgrading — it is one line.

This post covers **0.43.0 through 0.45.0**. The theme running through all three is not a feature. It
is that several things which *looked* verified turned out not to be, and the interesting part is
consistently **how they were found** rather than what the patch was.

<!-- truncate -->

## Verify before you parse

A `channel webhook` had no signature verification path at all. It now captures the raw body and
authenticates the caller **before** decoding anything.

The ordering is the whole point. A signature is over bytes. Anything that parses and re-serializes
first has verified a *normalisation* of the message, not the message. That distinction is easy to
assert and hard to prove, so the test fixture is a body deliberately built to be destroyed by a JSON
round trip:

```
ORIGINAL     = {"b": 1,\n  "a": 2,   "a": 3 }
ROUNDTRIPPED = {"a":3,"b":1}
```

Keys reorder, the duplicate key collapses, whitespace is erased. Feeding the verifier a
re-serialized body reds exactly one test out of sixty-four — which is also the measure of how little
a canonical-JSON fixture would have proved.

Nine guards ship with it, each verified by deleting it and watching a named test change colour. The
two refusal sites are deliberately independent: removing the load-time guard reds two integration
tests while all sixty-four unit tests stay green, and removing the request-time guard does the exact
reverse. Neither half silently carries the other — the same doctrine the connector and bearer-token
fixes established earlier.

The signature *schemes* themselves are not here yet. A declared-but-unperformable verification is a
**load error before a port is bound**, so nothing degrades quietly to unverified while you wait.

## One walk, so it cannot narrow again

flux hides registered credentials from logs, from models, and from saved transcripts. An earlier
release closed a hole where the walker that scans saved JSON skipped plain **numbers** — which
matters because an all-digit credential is invisible to every heuristic flux has, making registration
its only protection.

Fixing it turned up three more copies of the same traversal. Two of them fed **durable stores**, and
those two also skipped object *keys*.

Four independent hand-rolled walks of one shape, each free to narrow on its own. That duplication was
not a tidiness problem — it was the mechanism by which the defect could exist at all, because there
was no single place where "total" was defined. There is now one walk, and its `match` is exhaustive
with no catch-all, so a new JSON node kind fails the **build** rather than being silently skipped.

A related fix in the same area is worth stating plainly because the direction was wrong rather than
the code: when text-level redaction corrupted a recorded request badly enough that it no longer
parsed, the fallback returned the **unredacted** original. It failed open, and silently. It now
refuses.

## The editor grammar parses the language

flux ships a tree-sitter grammar that Helix, Neovim and Zed use. It could not parse **7 of 15** of
flux's own canonical examples — 166 error nodes across six constructs it never supported, including
bare-identifier binds, `ctx` blocks and `+=`.

Why nobody noticed is the useful part. The grammar repository had its own three-file example corpus,
and its CI parsed *that* one. At the identical revision that corpus was 100% clean while flux's was
47% broken. **A second corpus did not merely permit the drift — it certified it.**

That is now closed at both ends: the grammar parses all fifteen, its CI parses *flux's* corpus with
no allowlist, and a check on this side verifies that the exact revision flux pins can parse the
canonical examples. Moving that pin, it turns out, was the step that had been quietly missing — two
earlier grammar improvements had landed and reached nobody because the pin never moved.

Editors also stopped rendering ordinary identifiers as punctuation, which in the language server was
worse than a colour problem: those tokens were emitted **not at all**, so an editor lost the
identifier entirely.

## Also in this release

- **Connector deliveries** — point a channel at an installed connector and flux reads that
  connector's own description of itself. Every rule in it is re-checked as a load error before a port
  is bound, so a description edited after publication cannot start a channel that quietly does
  nothing.
- **`http.request` returns a record** — `{status, headers, body}` instead of one flat string, so a
  flow can select a field. Note the action-needed item from 0.43.0 if you consume it as text.
- **Panes actually appear.** The `pane.*` vocabulary shipped inert in 0.42.0; the surface sink is now
  installed, and both remaining links of the delivery chain are pinned by tests that fail
  independently.
- **Plugin catalog refresh** without a restart, where a refresh can only ever *narrow* what a plugin
  may reach — a refreshed manifest asking for more is refused outright.
- **Configured `[limits]`** are now observed end to end on the app-journey path, proven by breaking
  links in the middle of the chain rather than at its ends.

## Action needed

- **A webhook that listens beyond your own machine must state its verification.** If it has a token
  and no verification, add `verify "none"` to keep today's behaviour. flux refuses to start
  otherwise, names the channel, and prints the exact line to add — rather than opening a port whose
  protection you might have assumed. Webhooks bound to loopback are unaffected.

## A note on how these were found

Nearly every item above came from a review or an implementor's audit rather than from a roadmap —
and in several cases the *story describing the bug was itself wrong*. One claimed a truncation
mechanism that provably does not exist. One was blocked on a dependency constraint that had lifted
weeks earlier. One named the wrong root cause, the wrong fault model and the wrong colour.

Each was corrected by the person implementing it, with evidence, before the fix shipped. That is
worth more than any single entry on this list.

Full engineering detail, including the review findings behind each change, is in the
[CHANGELOG](https://github.com/codewandler/flux/blob/main/CHANGELOG.md).
