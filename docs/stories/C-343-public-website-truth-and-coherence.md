---
id: C-343
title: "Make the public website describe the product that actually ships"
pillar: Core
status: backlog
note: "Reconcile runtime, channel, security, language, SDK, and setup claims; repair the public information architecture and add drift guards"
---

# Make the public website describe the product that actually ships

## Goal

Make the public website a coherent, runnable, and technically accurate guide to the current tree.
A reader should encounter one runtime mental model, security claims with the same boundaries as the
implementation, examples that use canonical syntax and real APIs, and navigation organized around
what they are trying to accomplish rather than the history of how Flux was built.

This is one documentation-coherence story, not a product-behavior story. When a page promises a
surface the product does not expose, document the honest boundary here; do not invent the missing
product behavior as part of the copy edit.

## Acceptance

- [ ] `public_runtime_story_matches_the_authored_loop_contract` fails first, then pins one canonical
      architecture description across the homepage, site metadata, `llms.txt` generator, Overview,
      Concepts, Infrastructure, saved-flow guidance, and related tutorial prose: models declare
      intent and make provider-native calls inside typed stages; authored Flux-Lang owns control
      flow; and the host freezes literal effects into action batches. Say precisely that the default
      adaptive loop does not ask the model to generate a per-turn Flux plan, while documenting
      `op.register` as the explicit analyzed, scoped, approval-gated seam where a model may propose
      one composite operation's source. Replace `architecture_v0.png`, whose
      `LLM / Planner -> typed Flux-Lang plan` and `plan -> match -> run_plan` path is no longer true.
- [ ] The Channels section matches the registered surface and runtime: `connector` is documented and
      linked from the sidebar; the inventory no longer says there are only six kinds; delivery is
      described as isolated, bounded concurrency (including its limit, backpressure, and shared-state
      caveat), not an ordered queue of one; and webhook input is described as parsed JSON rather than
      a verbatim body. `public_channel_inventory_covers_every_registered_kind` fails first and proves
      that the public inventory cannot silently omit another shipped kind.
- [ ] Plain-language and technical security pages state the same guarantee without overclaiming it.
      In particular, distinguish manifest-scoped host callbacks from raw syscalls by trusted native
      plugins; distinguish host-injected auth, raw `secret`/`credential` materialization into trusted
      plugin code, and host-terminated `conn.authenticate`; state that OS confinement is opt-in for
      interactive use; describe every deliberate sandbox exemption; and stop claiming that the host
      stores only secret locations or that every stored token reaches the wire as a host-injected
      header. Repair the orphaned `fs` row in the plugin-capability table.
- [ ] Configuration and installation guidance is executable and matches the shipped flags. The
      install path uses literal per-platform commands, actually puts `flux` on `PATH`, separates the
      convenience installer from attestation-verified manual extraction, and gives consistent update
      steps. `FLUX_ALLOW_ALL` is documented as filesystem-scope widening, never as `--yes`; the full
      native/plugin/fleet scope of `FLUX_ALLOW_PRIVATE_NET` is explicit; sandbox network defaults name
      the different interactive and unattended postures; and the troubleshooting state map includes
      credentials, endpoints, project-local state, and `--store` without saying that everything lives
      under `~/.flux`.
- [ ] The language guide teaches the formatter's canonical syntax in its primary examples: bare
      symbols, brace-free named arguments, named option headers, and duration units. Legacy spellings
      appear once as compatibility guidance rather than throughout the tour and reference. Invalid
      `//` comments and Rust raw strings in `ops.md` become valid Flux. The pure-data/control-flow
      pages agree about bind-before-`match` field access, `parse(..., as: "form")`, valid pure
      template leaves, symbol rebinding, and the distinct `once` versus `checkpoint` keys.
      `public_flux_examples_are_canonical_formatter_fixed_points` fails first and checks fragments as
      well as complete declarations.
- [ ] Generated language material is changed only at its source: update AST/prelude documentation and
      regenerate guarded website blocks where needed; do not hand-edit the generated node-kind or
      prelude tables. The Node reference says authors, SDK callers, and host tooling construct the AST,
      not a planner, and it no longer claims native bracket-path `jq` needs an `@json` escape.
- [ ] Agent and SDK guides describe APIs and durability that exist. The Realtime dependency block and
      Rust example compile against one released package set and do not promise an audio-input handle
      `run_voice_session` does not return. Agent Lab leads with an accurate replay-versus-check matrix,
      treats cost assertions as recorded-run cost, uses correct consumer feature commands, and states
      the at-least-once crash window before any “crash-proof” language. CLI/TUI recovery text separates
      durable accepted work from ephemeral composer/queue input, and `/quit` has one cancellation
      meaning. Datasource and work-board taxonomy, provider model compatibility, and local-first data
      egress wording are consistent across their pages. Project-context guidance says that discovery
      starts at the exact launch/workspace directory and does not imply an upward repository-root
      search the implementation does not perform.
- [ ] Versioned and directional claims are dated and explicit. A2A pages say Flux implements the
      v0.3 contract rather than “the current spec” or universal peer compatibility, and link the
      versioned specification; the public Wasm page describes the portable model-free language core
      and CI parity that now exist while clearly naming the still-unbuilt host ABI, limits, and product
      surface. Remove internal story IDs from ordinary customer guidance; a Direction page may link a
      repository design record without making board bookkeeping part of the user explanation.
- [ ] The public information architecture is simplified after a task-oriented pass: collapse the two
      near-identical Overview navigation lists; keep the strong adaptive-turn -> authored-flow -> app
      tutorial progression; de-emphasize the on-hold Improvement loop instead of calling it co-equal
      on first-reader pages; group the twenty-page Agent section into discoverable subtopics; place
      setup, daily use, integrations, language, SDK, security, and reference in a defensible reading
      order; and remove or label duplicated guidance rather than maintaining two subtly different
      answers.
- [ ] Site plumbing supports the improved prose: “Edit this page” opens GitHub's edit surface rather
      than the tree viewer; Markdown link failures are treated consistently with route/anchor
      failures; sidebar refs do not duplicate pages in `llms.txt`/`llms-full.txt`; mutable counts and
      plugin versions are generated or replaced with stable wording; and the customer changelog is
      edited only through `WHATS-NEW.md` plus its guarded website regeneration, never directly in the
      generated mirror.
- [ ] Focused tests cover the high-risk drift seams above, every changed Rust/Flux example is compiled
      or parsed by a real consumer, and `npm run build`, `cargo test -p flux-cli --test
      website_contract`, the guarded language website-sync tests, and the external-link workflow all
      pass. Purely subjective spelling/style normalization is limited to touched prose and follows one
      recorded site convention; it must not create churn in historical release notes or dated blog
      posts.

## Progress

- 2026-08-01 — audited all 73 public documentation pages (~114k words), the homepage, sidebar,
  Docusaurus configuration, generated LLM-doc plugin, and current implementation seams in three
  parallel passes. `npm run build` is green: the defects below are semantic/editorial drift, not a
  currently broken route or anchor.

## Notes

### Confirmed runtime and first-reader contradictions

- `website/docs/intro.md:12-14`, `website/docs/infrastructure.md:8-23`,
  `website/src/pages/index.js:54-56`, `website/docusaurus.config.js:101-105`, and
  `website/plugins/llms-txt/index.js:105-109` say the model compiles a request into Flux-Lang.
  `website/docs/agent/agent-loop.md:16-18,56-60` and `docs/vision.md:10-14` correctly say that
  generated-plan path was removed. `website/static/img/architecture_v0.png` embeds the obsolete path
  in pixels, so prose alone cannot close this.
- `website/docs/intro.md:23-33` calls the Improvement loop a pillar while marking it on hold;
  `website/docs/infrastructure.md:53-54` nevertheless calls all three pillars co-equal. The Overview's
  `Start here` and `Related docs` lists (`:51-71`) then repeat nearly the same navigation.
- `website/docs/getting-started.md:14-57` downloads and extracts an unresolved `<target>` archive,
  never installs the binary, then immediately assumes `command -v flux` succeeds and tells that user
  to “rerun the installer.”

### Confirmed shipped-surface drift

- `website/docs/channels/overview.md:70-79` and
  `website/docs/channels/inventory.md:226-233` describe the retired serialized delivery gate. Current
  isolation and the 64-delivery bound live in `crates/flux-channels/src/deliver.rs:19-22` and
  `crates/flux-channels/src/lib.rs:25-38`.
- `website/docs/channels/inventory.md:8,17-24` omits the shipped `connector` adapter registered at
  `crates/flux-channels/src/adapters/mod.rs:50-57`, even though the current customer changelog
  advertises connector channels.
- `website/docs/direction/portable-wasm-runtime.md:8-14` says no Wasm build exists. The portable ABI
  exists at `crates/flux-lang/examples/portable/wasm_abi.rs`, with native/Wasm parity in
  `crates/flux-lang/tests/wasm_parity.rs`; the remaining boundary is smaller and should be named.
- `website/docs/agent/a2a.md:15-21` and `agent/a2a-conformance.md:8-10,52` present v0.3 behavior as the
  current universal contract. A2A v1.0 is now the current stable specification; v0.3 is a previous
  version ([official migration summary](https://a2a-protocol.org/latest/whats-new-v1/)), and the public
  compatibility statement needs an explicit version boundary.
- `website/docs/agent/realtime.md:47-69` uses incomplete/wrong published dependencies and promises a
  caller-pushed audio path that the described `FlowClient::run_voice_session` API does not expose.
- `website/docs/agent/cli.md:121-131`, `agent/tui.md:177-183`, and
  `sdk/agent-lab.md:8-28,303-309` overstate crash recovery despite the documented at-least-once window
  in `crates/flux-flow/src/resurrect.rs` and `crates/flux-sdk/src/session.rs:323-338`.

### Confirmed security and configuration contradictions

- `website/docs/security/plain-terms.md:31-35` says Flux stores only a secret location and that plugins
  cannot snoop or use the network themselves. `security/overview.md:62-71`,
  `security/credentials.md:46-65`, and `security/plugin-sandbox.md:24-30` correctly document plaintext
  token storage and trusted, unsandboxed native plugin code.
- `security/plugin-sandbox.md:61-88` claims two raw-secret exceptions, describes them
  inconsistently, and omits the declared `secret` callback used by shipped plugins.
  `security/credentials.md:144-147` likewise overgeneralizes host-injected headers.
- `website/docs/reference/config.md:462` conflates `FLUX_ALLOW_ALL` with auto-approval; source confines
  it to filesystem scope. `:464` understates `FLUX_ALLOW_PRIVATE_NET`, which also feeds plugin and
  fleet private-network grants.
- `website/docs/security/credentials.md:68-81` presents Vault as if setting its environment switches
  the stock CLI/server credential store. Current code exposes an injected embedder/store path instead;
  the page must name that boundary.
- `website/docs/security/os-sandbox.md:137-162` announces one exemption and then documents three.
  `website/docs/security/plugin-sandbox.md:53` leaves the `fs` capability row outside its table.

### Confirmed language, SDK, and wording drift

- The language tour says bare symbols and brace-free inputs are canonical
  (`website/docs/language/tour.md:8-14`), while its own main examples and the reliability, control-flow,
  concurrency, examples, and Node-reference pages predominantly teach the legacy forms. The formatter
  and `crates/flux-lang/tests/named_option_headers.rs` are the canonical oracle.
- `website/docs/language/ops.md:160-195` contains `//` comments and Rust raw strings even though Flux
  supports `#` comments and normal/triple-quoted strings. Existing website tests parse only complete
  declarations, so these fragments escape validation.
- `website/docs/language/node-reference.md:8-12` says planners emit the AST and claims bracket-path
  `jq` needs `@json`; the language overview, execution model, pure-data guide, AST docs, and formatter
  say otherwise.
- `website/docs/language/pure-data.md:78-82` says a field expression may be a `match` subject while the
  analyzer and control-flow guide require binding it first. Its pure-leaf list also omits supported
  `parse`, and the guide omits `parse(..., as: "form")`.
- `website/docs/language/durability.md:18-21,108-121` describes `once` and `checkpoint` as sharing a
  key. Runtime keys `once` by session plus label/body hash; checkpoint uses session plus flow key.
- `website/docs/sdk/agent-lab.md` conflates hermetic replay of recorded plans with `check` re-driving
  the current loop, mislabels replay's recorded usage as near-zero cost, and tells consumers to pass a
  dependency feature as if it were their package feature.
- `website/docs/agent/datasources.md:8-22,201,243-250` and `agent/fleet.md:15-20` disagree over whether
  a work board is a datasource, include “declares one the same way,” and count the taxonomy
  inconsistently.
- `website/docs/agent/tui.md:141,177-183` gives `/quit` two opposite meanings and treats unsent
  composer/queue state as durable. `website/docs/troubleshooting.md:50-64` says “Everything” is under
  `~/.flux` while omitting credentials, endpoints, project-local state, and alternate stores.
- `website/docs/agent/project-context.md:8-18,30-34` says repository/project root, but context
  discovery starts at the process workspace directory and does not walk upward to find a parent
  repository.
- `website/docs/concepts.md:62-65` says secrets stay local without distinguishing local credential
  storage from prompts/context intentionally sent to a configured remote model.

### Site mechanics and scope boundaries

- `website/docusaurus.config.js:52` points “Edit this page” at `/tree/main/website/`, not GitHub's edit
  route. The same config throws on broken routes/anchors but only warns on broken Markdown links.
- The LLM-doc generator includes sidebar refs without de-duplication, so the Security-category ref to
  `agent/safety` emits that page twice.
- Preserve the tutorial's adaptive-turn -> authored-flow -> app progression, the Agent-loop ownership
  table, the safety page's native-plugin caveat, and the HTTP tenancy/limits material. They are clear
  and already tell the right story.
- Exclude `website/docs/whats-new.md` history and dated blog posts from blanket wording cleanup.
  `website/docs/whats-new.md` is generated from `WHATS-NEW.md`; the Node/prelude tables are generated
  from Flux-Lang source docs. Fix those sources and use their guarded regeneration commands.
