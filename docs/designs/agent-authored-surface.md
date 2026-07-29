# The agent-authored surface — panes the model opens, config it can safely change

Story: [C-219](../stories/C-219-agent-authored-surface-epic.md) · Pillar: Core · Status: design ·
Epic slug: `agent-authored-surface`

## The ask

Let the harness's main agent shape the harness. Concretely: open containers on the TUI for results,
live info and sub-agent fleets; and change flux's own configuration, applying it to the session it
is already in. "So it would be completely free."

## Where the surface actually stands today

`flux-tui` is entirely **host-authored**. `render` (`crates/flux-tui/src/rendering.rs:122`) draws a
fixed six-row `Layout::vertical` — header, transcript, steering queue, slash/`@` menu, composer,
footer — and every overlay (help, `/usage`, the queue, the session picker) is a centred panel the
*surface* decides to open, through one shared `render_overlay_panel` (`rendering.rs:36`). The
agent's only expression on that surface is the transcript: `Entry::Assistant`, `Entry::Tool`,
`Entry::Notice` and friends (`lib.rs:318-347`). It cannot open a container, cannot hold a live
region across turns, and cannot change how it is presented.

Three findings make this epic much smaller than it sounds, and they are the reason it is worth doing
now rather than as a rewrite later.

**1. The tool→surface seam already exists, twice.** `ToolProgressSink` and `SpawnActivitySink`
(`crates/flux-runtime/src/lib.rs:188-262`) are the same shape: a synchronous, send-only trait defined
at **L2**, installed by an **L6** surface, reached by a tool through `ToolContext`, with redaction
applied *at the reporter* so a tool structurally cannot put unredacted bytes on a screen —
"[`ToolContext::progress_reporter`] … is the only way to reach one" (`lib.rs:242-246`). A pane
contract is a third instance of a twice-proven pattern. It needs no new architecture and creates no
layering violation: `flux-tui` is L6, tools are L2, and the dependency runs the legal direction.

**2. "The agent extends the harness" already ships one layer down.** `op.register`
(`crates/flux-tools/src/reflect.rs:459-503`) lets the model author a Flux-Lang composite at runtime
with `scope: turn | session | project | global`, installed through `CompositeRegistrar`, where "the
engine owns all state mutation; this tool just delegates through the audited dispatcher" and "every
inner call still runs through the same approval and guarded-IO envelope". The pane vocabulary copies
that shape rather than inventing one: same lifetime ladder, same host-owns-the-state posture.

**3. There is already a live data stream with nowhere to go.** A-79 shipped `SpawnActivitySink`
([live-sub-agent-activity.md](live-sub-agent-activity.md)): correlated, redacted, per-role sub-agent
activity carrying "role, child session and parent session correlation on every event", balanced
planning state, and tool lifecycle with timing. **`flux-tui` installs no sink for it.** The only
implementation in the tree is `flux-cli`'s `IgnoredSpawnActivity` (`crates/flux-cli/src/main.rs:114`),
which drops every event on the floor. The sub-agent pane is therefore not speculative UI — it is a
designed, tested, already-redacted stream that the daily-driver surface currently discards.

## The line this must not cross

A model that can draw a styled region inside a trusted terminal is a model that can imitate the
approval sheet. This is not a hypothetical: [C-163](../stories/C-163-plugin-commands-and-host-ui.md)
already names it for plugins — *"a plugin that can pop a dialog is a plugin that can phish the user
inside a trusted surface. Constrain the rendering (plugin name always shown, no styling control)
rather than relying on plugin good behavior."* The same sentence holds with "model" substituted, and
more forcefully, because the model is the thing the approval sheet exists to gate.

The config half has the same shape. A `config.set` that could write `[permissions] allow` or
`[sandbox] enabled` and hot-apply it is a direct envelope bypass wearing a config-file costume.
AGENTS.md is unambiguous: *there are no bypass paths. Don't add one.*

Both are closed **structurally**, not by policy — a denial can be argued with, an unrepresentable
value cannot:

- **Panes carry no model-supplied style, colour, geometry or ordering.** The wire type has `kind`
  and `data` and nothing else that reaches a `Style`. Trust chrome — the border, the mark, the
  placement — is surface-owned and therefore unforgeable, because the model has no field to write it
  into.
- **The agent-writable config allowlist is disjoint from `flux_config::PinnableKey::ALL`**
  (`crates/flux-config/src/lib.rs:451-505`), asserted by unit test. That list already enumerates
  every security-relevant key with a canonical spelling — `policy`, `sandbox.enabled`,
  `sandbox.require`, `sandbox.network`, `workspace.allow_all`, `private_net.web`, `tools.disable`.
  Attempting one is not a denied request; it is an unknown key.

## Shape

### The vocabulary

```
pane.open   { id, title, slot, kind, lifetime, data }
pane.update { id, data }             // replace or append, per kind
pane.close  { id }
pane.list   { }                      // what is open, for the model's own orientation
```

| Field | Values | Owned by |
|---|---|---|
| `slot` | `left \| right \| bottom \| overlay` | model **proposes**; surface resolves, demotes or suppresses |
| `kind` | `rows \| kv \| log \| progress \| tree \| markdown` | model chooses from a closed set |
| `lifetime` | `turn \| session \| project` | model; mirrors `op.register`'s scope ladder |
| `data` | kind-specific typed payload | model |
| geometry, colour, style, z-order, the trust mark | — | **surface, always** |

`markdown` reuses the transcript's existing `flux-markdown` (`ratatui` feature) path — no second
renderer. `tree` reuses `plan.rs`'s renderer. Nothing here introduces a rendering primitive that the
TUI does not already own, which is also why this fits under the standing `ratatui` hold at 0.29
(root `Cargo.toml:131-136` — `markdown-ratatui` requires `^0.29`; lifting is a separate concern and
this epic adds no widget dependency).

### Layering

| Layer | Crate | What lands there |
|---|---|---|
| **L0** | `flux-config` | `AGENT_WRITABLE_KEYS` + its disjointness-from-`PinnableKey` proof |
| **L2** contract | `flux-runtime` | `SurfaceSink` trait, `PaneCommand`/`PaneSpec`/`PaneData`, `SurfaceReporter` (redacts, then emits), `ToolContext::surface()` |
| **L2** ops | `flux-tools` (`surface.rs`, `config_keys.rs`) | `pane.*`, `config.get`, `config.set` |
| **L6** surface | `flux-tui` (`panes.rs`) | `ChatState.panes`, slot layout, trust chrome, the `SpawnActivitySink` install |

`ToolContext::surface()` returns `None` when no host installed a sink — exactly the posture of
`progress_reporter` (`crates/flux-runtime/src/lib.rs:1042`), whose doc comment already states the
rule: `None` when no host installed a sink. Headless `flux run`, `flux-server` and the SDK therefore
get a clear op failure, never a silent no-op that the model reads as success.

### Surfacing, not gating

The `pane.*` ops must **not** be a `ToolGroup`. Groups are signal-gated — `builtin_groups()`
(`crates/flux-tools/src/groups.rs:9-28`) surfaces a group when a `project.signal` matches, and there
is no project signal for "a human is watching a terminal". The right precedent is `[consult]`, whose
config comment states it exactly: *"`model`'s mere presence is what surfaces the `consult` op into
the catalog at all … within a session the surfacing decision is made once at assembly time and never
churns"* (`crates/flux-config/src/lib.rs:398-410`, the A-95 cache-stability lesson).

So: **the presence of a surface sink at assembly time surfaces the pane ops.** One decision, taken
once, never re-evaluated mid-session — which is what keeps the tool set stable and the prompt prefix
cacheable. A session with no sink never sees the ops at all, so the failure path exists for
correctness rather than as a routine occurrence.

### Rendering

`render` grows a horizontal split around the transcript row for `left`/`right`, and one extra
vertical constraint for `bottom`. `overlay` reuses `render_overlay_panel` (`rendering.rs:36`), the
shared chrome C-152 already consolidated.

Panes are **bounded and suppressible**. Below a minimum transcript width they are not drawn at all
rather than squeezing the conversation — the same posture `EMPTY_CARD_MIN_WIDTH` (`rendering.rs:66`)
established, and the same one C-102 took for the header/footer bars at narrow widths. Caps on pane
count, per-pane rows and total width fraction are surface constants, not model inputs.

The approval sheet continues to draw **last**, over its own `Clear`ed rect. A pane can never occlude
it, and a pane can never be mistaken for it, because a pane's border, mark and title style come from
the theme rather than from the payload. Under `Theme::MONO` (`theme.rs:120`) — where every colour
role resolves to `Color::Reset` — the agent-region mark must still be legible, so it is a glyph and
a modifier, not a tint. That is the same reasoning C-149 used for the transcript gutter rail
(`lib.rs:770-781`) and C-154 for the approval risk tiers.

### Config self-modification, and why there is no re-exec

The write path already exists and is guarded end to end: `flux_config`'s pure round-tripping
serializers (`render_theme`, `render_allow_rules` — `lib.rs:1102-1140`) and
`flux_runtime::metadata`'s atomic persisters (`persist_user_theme`, `persist_allow_rules` —
`metadata.rs:145-177`), which write through a `System` confined to the trusted root and round-trip
every unrelated setting. `config.set` adds a third persister in that family, not a new IO path.

**"Reload" is not a restart.** It is: re-read config → apply the hot keys to the live `ChatState` /
`FlowEngine` → re-project the session through the existing `project_session` + `load_history` that
`/resume` already uses (`lib.rs:3954-3975`). This is a deliberate rejection of process re-exec. A
self-restart is a **new turn-termination path**, and AGENTS.md names that as the bug class that has
recurred three times: *"Session shape is always a valid provider history … treat any new termination
path as suspect. The mock provider does not catch it; only a live provider 400 does."* Buying "the
agent can restart itself" at the price of re-opening that class is a bad trade for a capability that
`/resume` already delivers without leaving the process.

Hot-appliable keys are presentation and behaviour only: `theme`, `effort`, model spec, verbosity,
`[consult]`. Anything on the allowlist that cannot be hot-applied is persisted and **reported as
"takes effect next session"** — never silently half-applied, which would leave the model believing a
setting is live when it is not.

## Alternatives considered

- **Raw layout control** (model supplies rects, constraints and styles). Rejected: it is the version
  where a model-drawn region can be pixel-identical to the approval sheet, and it makes every
  malformed frame the surface's problem. The typed vocabulary gives up almost nothing real — the
  model wants a container for content, not a rect calculator.
- **A `surface` `ToolGroup`.** Rejected above: groups are signal-gated and there is no signal for
  "a TUI is attached". Sink presence at assembly time is the correct and already-established seam.
- **Config edit + process re-exec + resume.** Rejected above: a new turn-termination path for a
  capability `/resume` already provides in-process.
- **Reusing `Entry` for panes** (a pane as a transcript entry). Rejected: panes are persistent and
  positioned, entries are append-only and scrolled. C-157's empty-state card already documents the
  hazard of anything that renders in the transcript area without being a transcript row
  (`rendering.rs:70-74`): no layout cache, no focus, no scroll bookkeeping. Panes stay outside
  `transcript_viewport` for the same reason.

## Risks & open questions

- **Screen budget.** Panes compete with the transcript for width on a surface whose whole design
  premise is density. The caps and the suppression threshold are the mitigation; if they end up
  fighting the user, the answer is a smaller default footprint, not a model-tunable one.
- **Update churn.** A model that calls `pane.update` every round adds tool calls to every turn.
  `pane.*` results should be terse and the ops cheap; watch whether the catalog addition itself
  moves cache behaviour (the A-95 lesson is why surfacing is assembly-time).
- **`lifetime: project`** implies persistence across sessions, which implies a on-disk pane store.
  Deliberately deferred: C-220 defines the field, C-221 implements `turn` and `session`, and
  `project` is rejected at the op boundary until a story claims it.
- **Open:** whether `pane.list` should report panes the *host* opened (the fleet pane) or only the
  model's own. Leaning toward all-of-them-labelled, so the model can see it does not need to
  duplicate what the surface already shows.

## Acceptance / done

The union of C-220…C-225. In one line: **the model can open a live pane on the TUI and change its
own presentation, and neither capability can be turned into a way to imitate the approval sheet or
widen its own permissions** — both proven by test rather than by argument.

## Stories

- [C-220](../stories/C-220-surface-sink-contract.md) — the `SurfaceSink` contract at L2
- [C-221](../stories/C-221-tui-pane-slots.md) — pane slots in the TUI
- [C-222](../stories/C-222-pane-trusted-chrome.md) — the trusted-chrome invariant
- [C-223](../stories/C-223-pane-ops.md) — the `pane.*` ops
- [C-224](../stories/C-224-subagent-fleet-pane.md) — the sub-agent fleet pane
- [C-225](../stories/C-225-agent-writable-config.md) — agent-writable config keys
