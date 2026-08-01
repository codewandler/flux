# Design: Explore, then freeze — ad-hoc browser testing that becomes a deterministic script

**Status:** proposed · **Pillar:** Core · **Stories:** [C-430](../stories/C-430-distil-an-exploration-into-a-flow.md) · [C-431](../stories/C-431-durable-locators.md) · [C-432](../stories/C-432-browser-credentials-never-come-from-the-prompt.md) · [C-433](../stories/C-433-a-frozen-script-asserts.md) · [C-434](../stories/C-434-the-e2e-recipe.md)

## Why

Ask an agent: *"go to site X, log in, then test the happy path of module U."* It explores — misreads a
label, backtracks, finds the right button, succeeds. That exploration is expensive and
non-reproducible, and today it is thrown away. Ask the same thing tomorrow and it explores again.

The valuable artifact is not the answer; it is **the path that worked**. This epic makes flux able to
say: *keep that, drop the trial and error, and give me a script I can run in CI.*

It is the thesis in its most legible form. **The model explores; the runtime keeps the result.** An
agent whose contract is its transcript can re-narrate what it did; it has no artifact to hand you.

### Most of the machinery already exists — one thing does not

Grounding, from the code rather than from ambition:

- **Browser control is real and already safe.** `crates/flux-web/src/browser.rs` drives headless
  Chromium over an in-repo CDP client with `browser.open · goto · act · snapshot · close`. Chrome is
  spawned through the guarded `System::spawn_debug_pipe` seam (argv-only, env-cleared, fd-3/4 pipe);
  **every subrequest routes through the family-wide `web` egress guard via CDP `Fetch` interception**;
  the ops are evidence-gated behind a Chromium-discoverable signal so they never mislead a planner on a
  machine with no browser. Below the `ops` boundary it is transport-agnostic, so tests drive a scripted
  fake over an in-memory duplex — **no Chrome in CI**.
- **The page model is semantic, not pixel.** The digest (`digest.rs`) is built purely from
  `Accessibility.getFullAXTree` joined to DOM identity — *"what a screen reader sees — roles, names,
  states"* — and its output ordering is document order, which its own header notes is
  *"replay/`flux diff` friendly"*. It was built with replay in mind.
- **Runs already carry their own source.** Since L-38 every accepted plan records parseable
  `plan_source`, redacted at record time, and `flux export` re-renders it through the same redactor.
- **Missing:** nothing turns a *recorded session* into a *saved flow*. `flux flow` has `list` and
  `run`; there is no `save`. That absence is this epic.

### ⚠ The problem that decides whether any of this works

`e<N>` refs are **not stable across runs**, and a naive recorder that emits them produces scripts that
break on the next deploy — the exact failure that made a generation of record-replay e2e tools
disposable.

`RefMap` (`crates/flux-web/src/digest.rs:53-72`) keys on `backendDOMNodeId` and assigns
`self.next += 1` in **first-encounter order within one live session**. So `e17` is genuinely stable
*while the agent is exploring* — which is what it was designed for — and **means nothing in a fresh
session**, where numbering restarts and depends on the navigation path taken.

The fix is available in the same data: every ref already carries an AX **role** and **name**
(`digest.rs:180`). Freezing must **re-anchor** `e17` into `role=button, name="Sign in"` — a locator
that survives a DOM refactor because it is what a screen reader (and a user) would use to find the
control. That re-anchoring is the difference between this epic and a macro recorder, and it is why
[C-431](../stories/C-431-durable-locators.md) is not an optimization.

### ⚠ The prompt in the request is a credential leak, and the epic must not teach it

The motivating phrasing — *"log in as X with password Y"* — puts a live password into the prompt. From
there it reaches the model context and the durable event log. flux redacts secrets from model-visible
output and never lets them off the machine, but the `Redactor` redacts **values it has been told
about**; a password typed into a prompt was never registered, so **nothing redacts it**.

A convincing recipe that teaches users to paste production passwords into prompts would do more harm
than the epic does good. [C-432](../stories/C-432-browser-credentials-never-come-from-the-prompt.md)
exists to make the safe path the *only* documented one.

## Approach

Five stories. C-431 and C-432 are not polish on C-430 — they are the two ways this feature fails.

### C-430 — distil a recorded exploration into a flow

The core verb: from a session, emit authored Flux-Lang containing the **path that worked**, with the
backtracking dropped. `plan_source` and the run trace are the raw material; the work is selecting the
successful subsequence and emitting readable flux rather than a transcript dump.

⚠ Dropping the failures is the point *and* the risk: a script with no record of why it is shaped as it
is invites someone to "simplify" it back into the thing that did not work. The distilled flow should
carry a provenance line to the session it came from.

### C-431 — durable locators

Re-anchor `e<N>` to role+name at freeze time, and decide what happens when a locator is ambiguous or
missing — the two cases that decide whether frozen scripts are trusted. A script that silently clicks
the wrong "Submit" is worse than one that fails.

### C-432 — credentials never come from the prompt

Browser credentials resolve from the secret store, registered with the `Redactor` before the run, and
the frozen script **references** rather than embeds them. The docs teach only this path.

### C-433 — a frozen script asserts

"Test the happy path" means checking something. A distilled sequence of clicks proves only that the
clicks did not error. What the agent *verified* has to survive freezing, or the output is automation
rather than a test — and a green suite of scripts that assert nothing is a liability, because it is
trusted.

### C-434 — the worked recipe

End to end on a real target: explore, freeze, re-run in CI. Belongs to the
[flux-recipes](flux-recipes.md) family and is this epic's proof; filed here so it is not orphaned if
that epic re-sequences.

## Alternatives considered

- **Emit Playwright/Selenium instead of flux.** Real appeal: existing runners and ecosystem. Rejected
  as the primary output — it leaves flux's envelope, so the egress guard, approval and redaction stop
  applying, and the artifact can no longer be replayed or diffed. Worth revisiting as an *export*
  once the native path works.
- **Record CDP commands verbatim.** Simplest, and the fastest way to a demo. Rejected: it produces
  exactly the brittle, unreadable, ref-keyed script this design exists to avoid.
- **Re-explore every run, cache nothing.** Honest and simple, but it is the status quo, and it makes
  every CI run cost a model and vary.
- **Pixel/coordinate capture.** Rejected outright: the browser stack is deliberately non-visual, and
  coordinates are the least durable locator available.

## Risks & open questions

- ⚠ **A frozen script that passes for the wrong reason.** The worst outcome of the whole epic: a green
  test that no longer exercises what it claims. C-433 is the mitigation; ambiguity handling in C-431 is
  the other half.
- ⚠ **Credentials.** See above. This is the one that must not be got wrong in the *documentation*, not
  just in the code.
- **What is a "happy path" is a judgement the model makes.** The distiller inherits it. If the agent
  succeeded by accident — clicked something that happened to work — the frozen script enshrines the
  accident.
- **Open:** where a distilled flow is written, and under what verb (`flux flow save <session>`?
  `flux distil`?). The CLI has an explicit-subcommands rule; this needs a name that says what it does.
- **Open:** whether the distiller is itself model-driven. Selecting the successful subsequence is
  partly mechanical (which ops errored) and partly semantic (which succeeded but were dead ends). A
  model-driven distiller is more capable and makes the *freezing* non-deterministic — acceptable, since
  the frozen artifact is then reviewed and committed, but it should be a stated choice.
- **Open:** re-freezing. When the UI changes and the script breaks, the desirable flow is "explore
  again, diff against the old script" — that is `flux diff`'s territory, and may be nearly free.
- Chrome is a heavy, version-drifting dependency. It is already handled (discovery + evidence gate),
  but a *frozen script* pins behaviour in a way an exploratory run does not.

## Acceptance / done

- An agent explores a site, and one command turns that session into a readable, committable flux flow
  containing only the path that worked.
- The frozen flow re-runs without a model, in CI, against a scripted CDP fake and against real Chrome.
- Locators survive a DOM change that renumbers refs; an ambiguous locator fails loudly rather than
  clicking the wrong control.
- No documented path puts a password in a prompt.
- The frozen script asserts what the exploration verified, and fails when that stops being true.
