<!--
  WHATS-NEW.md — the CUSTOMER changelog. Audience: people who USE flux, not people who
  build it. Voice rules:
    - Plain language, feature-first. Say what the user can now do or what behaves
      differently — never how it is implemented.
    - NO story IDs, NO crate names, NO internal jargon (engineering detail lives in
      CHANGELOG.md).
    - Per release, use only the sections that apply: "### New", "### Improved",
      "### Fixed", "### Action needed" (breaking or attention-worthy changes).
  This file is embedded into the `flux` binary and shown by `flux changelog`.
  `scripts/cut-release.sh` rolls [Unreleased] into the release section on every cut.
-->

# What's new in flux

## [Unreleased]

### Fixed

- **Resuming an adaptive turn cannot restore an oversized integration catalog.** Flux rechecks the
  complete four-integration-family limit before exposing operation schemas, including for durable
  state created by an older runtime. Valid resumed turns are unchanged.

## [0.23.0] - 2026-07-14

### New

- **Install a plugin straight from a git repo.** `flux plugin install --git <url> --tag <v>` clones
  and builds a plugin from source — the way to run a private or third-party plugin that the signed
  pack channel can't serve. It shows the exact commit and asks for confirmation before building
  anything.
- **The Flux-Lang editor server now ships as a downloadable binary** (no Rust toolchain needed), and
  your editor can outline a `.flux` file and jump from a `$variable` or an operation call straight to
  where it's defined.

### Improved

- **Embedded apps can tune specialist cognition independently from the manager.** A host may select
  separate intent and exploration models, reasoning effort, output sizes, and call ceilings for
  delegated work while retaining the existing child defaults when it does not opt in. Spawn
  iteration, wall-clock, authorization, approval, and guarded-operation limits remain independent.
- **GitLab: safer destructive operations.** Deleting a branch, tag, release, file, CI/CD variable, or
  now a whole project is treated as the high-risk action it is, with an optional confirmation field to
  prevent fat-finger mistakes.
- **GitLab: scoped, previewable indexing and reliable project creation.** You can scope indexing to
  one project's issues and preview an estimate before a broad crawl, and creating a project inside a
  group namespace resolves reliably — refusing to guess when a name is ambiguous.
- **GitLab: readable files and side-effect-free reads.** Reading a file now hands you the decoded text
  alongside the raw content, and plain reads and lists no longer write to your index or print stderr
  chatter unless you ask them to.
- **The editor server updates large files incrementally and no longer eats your comments when
  formatting.**

### Fixed

- **Long adaptive turns keep one stable integration-family limit.** Later evidence may still surface
  another relevant integration, but repeated expansion cannot grow the active set past four and
  trigger a delayed oversized-catalog failure.
- **Gemini models on OpenRouter can use the same operation catalog as other providers.** Flux now
  translates compatible schema details before the request and stops locally with a precise
  operation/path error when a constraint cannot be represented. Your complete operation contract
  still governs validation, approvals, and execution.
- **Cancelling a served request now stops delegated work even when an integration opens a nested
  runtime.** The specialist remains linked to the real parent conversation for audit and live
  activity, while standalone one-shot flows keep their existing independent behavior.
- **Extreme outer-loop limits fail safely at startup.** `max_iterations` accepts values from 1 to
  1,000; larger CLI, config, SDK, or embedded-agent values are rejected before Flux builds the
  repeated control program.
- **Cancelling delegated work clears its live activity state.** A specialist that is still running
  when its parent turn is cancelled now reports a correlated failure completion, so chat surfaces
  do not leave that child shown as active.
- **Plugin calls no longer print secret-like fields.** Previewing or running a GitLab CI/CD variable
  write now masks the value (shown as `***`), and the GitLab auth check returns just your id, username,
  and name instead of your full profile.

### Action needed

- **Rust hosts that store `EngineLoopHost::set_turn` as a typed function pointer must accept or
  explicitly discard its returned activity reporter.** Ordinary calls used as statements keep compiling.
- **GitLab `changelog.add` now requires an explicit `branch`** — it no longer silently commits to your
  default branch, so add the branch you intend to write.
- **GitLab plain reads and lists no longer contribute to the datasource index by default** — pass
  `contribute: true` on `project.list`/`mr.list`/`issue.list` to restore the previous behavior.
- **GitLab `index.build` dropped the unimplemented `user_*`/`group_*` inputs** — remove them from any
  calls that set them (they were never honored).

## [0.22.0] - 2026-07-14

### Improved

- **Long delegated work can report what each specialist is doing.** Embedded chat surfaces can show
  a child agent's planning, safe progress notes, operation lifecycle, and success/failure as it
  happens, with stable child/call correlation. Private reasoning, child prose, operation arguments,
  and result content remain outside that live feed unless the embedding host explicitly projects a
  safe label.
- **Long evidence-gathering turns have room to finish, with visible controls.** Adaptive turns now
  allow up to 50 model calls by default, and the separate decision/batch loop allows 50 iterations.
  Use `--max-model-calls` and `--max-iterations` (or project config) to tune them independently.
  An authored `ai_segment` now receives exactly the `max_rounds` budget written in the flow instead
  of being silently reduced to 12.

### Action needed

- **Rust callers constructing `AgentConfig` with every field must add `max_iterations`.** Using the
  type's default keeps the new 50-iteration outer-loop behavior.
- **Rust callers constructing `SpawnRequest` with every field must add `activity: None`** or switch
  to `SpawnRequest::new`. Hosts that want live child progress can supply the optional reporter.

## [0.21.0] - 2026-07-13

### New

- **Adaptive turns have an explicit model-call budget and per-stage controls.** A logical turn uses
  at most 12 model calls by default, even across repairs and questions that pause for your reply.
  Use `--max-model-calls` or project config to change the total; intent and exploration can also
  inherit or override effort, output size, call count, and a model on the same provider.
- **Loop diagnostics show where model time and context go.** `--show-loop` now reports each model
  stage's round, total time, time to first response, operation count, and schema size. The audit trail
  retains those redacted measurements alongside approval wait and batch execution time; exact
  request bodies remain behind the explicit sensitive trace setting.

### Improved

- **Integration routing uses the aliases and capabilities integrations declare.** Requests can
  select a loaded integration from names such as “company chat” or a pasted service URL without
  loading every installed operation first. One clear match cannot be dropped by the router; several
  matches ask you to choose. Installed but unusable integrations are never offered.

### Fixed

- **Questions can pause and resume repeatedly without replaying completed work.** This includes a
  question discovered after an approved batch has already executed. Later questions render normally
  instead of leaking raw JSON, and separate sessions sharing one long-lived agent no longer inherit
  each other's surfaced integrations.

### Action needed

- **Rust integrations using exhaustive agent or provider-request struct literals need new fields.**
  Add the adaptive policy and host-only trace fields, or construct values with the provided defaults
  and request constructor. No trace metadata is sent to model providers.

## [0.20.1] - 2026-07-13

### Fixed

- **Requests for live facts reliably select an evidence operation.** Capability summaries no longer
  hide less common operations, and fresh read-only facts such as the current clock can be gathered
  directly without an unnecessary approval step.
- **Capabilities discovered during exploration remain available for the rest of the turn.** An
  agent can start with Slack, discover that it also needs web search, gather the live fact, and still
  return to Slack for the proposed action. Each expansion remains bounded by the configured tools,
  permissions, and active flow scope; unrelated later turns do not inherit it.
- **The bundled plugin pack builds against the new adaptive agent operation metadata.** Plugin
  authors can also set the same typed exploration-versus-action hint in custom integrations.

## [0.20.0] - 2026-07-13

### New

- **Every agent turn now runs through an authored adaptive loop.** Flux first identifies intent,
  narrows the live capability set, explores with each operation's exact schema, and captures proposed
  effects into one visible action batch. The batch receives explicit approval and then executes
  through the same guarded runtime; the model never generates executable Flux code.
- **You can define a different outer loop when the built-in one is not your application.** Select an
  explicit `.flux` loop from the CLI, config, an app agent, a role, or the SDK. Embedded applications
  can also register strongly typed Rust stages, while config-defined model stages declare their own
  input and output schemas.
- **Reasoning effort is now a real per-agent control.** Use `--think` and/or
  `--effort low|medium|high|xhigh|max`; the choice follows intent, exploration, final answers, compaction,
  cognition operations, and sub-agents unless a role deliberately overrides it.
- **Model calls can explain their request shape and latency.** Set `FLUX_MODEL_TRACE=1` for safe
  request-size, cache, retry, first-response, usage, and total-time records. A `full` mode can print
  the exact request body for sensitive local debugging.
- **Apps can declare their own hard capability boundary.** Put exact operation names in a top-level
  `permissions` block, then optionally narrow them per agent. An app or agent deny cannot be undone
  by local auto-approval or `--yes`.
- **Journeys can execute as an agent without becoming open-ended agent loops.** An owned journey now
  uses that agent's model, instructions, and datasource boundary while the steps remain the flow you
  authored. Invalid owners, sources, tools, and disallowed calls fail when the app loads.

### Improved

- **Agent turns now show what they are doing from the start.** The CLI and terminal UI display
  `routing intent…` and `exploring…` during the initial model consultations, then retain the accepted
  intent and selected capability families in the transcript. Verbose mode also shows the exact
  operations selected, and the final elapsed time now includes this initial work.
- **Plugin-heavy commands start much faster.** Installed integrations now verify, start, and exchange
  manifests concurrently with bounded fan-out while preserving stable operation ordering and the
  same safety checks. An 18-plugin local setup improved from about 2.23 seconds to 0.59 seconds for
  a warm offline startup.
- **Installed integrations no longer make unrelated agent turns carry their entire tool catalog.**
  Flux keeps plugin operations out of model-stage catalogs until your request names that integration,
  then makes only that integration available for the session. On a plugin-heavy installation this
  cut a measured tiny prompt from about 42k to 14k input tokens while keeping named integrations
  discoverable.
- **The beginner tutorial now demonstrates why deterministic flows matter.** It first lets the model
  decide whether to search a handbook, then refactors the same assistant so every answer must pass
  through scoped retrieval before the model writes it.

### Action needed

- **The old model-to-Flux planning commands and APIs have been removed.** `flux plan`, REPL `/plan`
  and `/run`, `--staged`, natural-language `FlowClient::compile`, and planner-corpus export no longer
  exist. Use the normal adaptive `flux run` path for conversational work, and author `.flux` files
  when you need fixed control flow. Existing authored flows and historical session reads remain.
- **A project file named `.flux/agent-loop.flux` no longer overrides the agent silently.** Select a
  custom loop explicitly with `--loop`, config, your app/role declaration, or the SDK.

- **Skills are now manual-only.** Installing a skill or mentioning one of its trigger words no longer
  changes an agent prompt. Pass repeatable `--skill <name>` in the CLI, or explicitly add skills to
  an embedded agent specification. This avoids accidental early activation and hidden context cost.
- **Rust code constructing Flux-Lang programs directly may need one small update.** `Program` and
  `AgentDecl` have a new optional permissions field; exhaustive struct literals should set it to
  `None` or finish with `..Default::default()`. Existing `.flux` files and serialized programs remain
  compatible.

### Fixed

- **Agents stop guessing file paths when a request does not name one.** They inventory the workspace
  first, and invalid wildcard or missing-path reads return a concrete recovery instruction. This
  keeps multi-file answers grounded without spending repeated model calls on path repair.

- **Answers grounded in several files can cite the right source reliably.** Read and search results
  keep a concise source label when they are fed into the next planning step, instead of arriving as
  indistinguishable blocks that invite the model to guess filenames.
- **Approval time is reported separately from tool execution.** A command that waits for your answer
  no longer appears to have spent those seconds reading a file, writing output, or calling a model;
  CLI and TUI timing show both phases explicitly.
- **The beginner tutorial now completes reliably and explains the architecture it demonstrates.**
  Context packs pass selected handbook text into AI reasoning, GPT-5 requests use the token-limit
  field its API accepts, and the lesson now starts with adaptive evidence gathering before turning
  the same requirement into a deterministic flow and owned app journey.
- **App datasources are now real per-agent boundaries.** An app agent is told which named knowledge
  sources it can use; searches are automatically scoped when there is one source, and attempts to
  query an undeclared source are rejected.
- **Ctrl-C now stops an interactive app promptly.** `flux app run` no longer hangs during shutdown
  just because it is waiting for the next terminal line.

## [0.19.2] - 2026-07-12

### New

- **Learn flux by building a local documentation assistant.** A new four-part beginner tutorial
  starts with a safely previewed agent task, turns the task into a reusable Flux-Lang flow, and then
  combines an agent, terminal channel, local Markdown knowledge, triggers, and a journey into a real
  model-backed app. It assumes only basic terminal skills and explains each safety boundary as it
  appears.

## [0.19.1] - 2026-07-12

### Improved

- **`web.crawl` can stop early at a content-size budget.** Pass `max_total_bytes` to bound a crawl by
  the total amount of readable content it gathers — not only by page count or depth. The crawl halts as
  soon as it reaches the budget and returns the pages it already collected, so you can cap how much a
  crawl pulls down without having to guess a page count up front.

## [0.19.0] - 2026-07-12

### Action needed

- **Two web tools were renamed for consistency.** `web_fetch` is now `web.fetch` and `web_search` is
  now `web.search`, so the whole web toolset shares one naming style (`http.request`, `web.fetch`,
  `web.search`, `web.crawl`, `browser.*`). Update any flow that calls `web_fetch(...)`/`web_search(...)`
  and any agent whose tool list grants `web_search` — the old names no longer work, and there is no
  alias. (This is unrelated to the old `[private_net] web_fetch` config setting, which was already
  replaced by the `[private_net] web` setting.)

## [0.18.0] - 2026-07-12

### New

- **Slack support bots run out of the box.** The Slack channel is now built into the standard `flux`
  binary — no special build needed. Point a program at your Slack app's tokens and it listens for
  mentions and replies in the thread. There's a new step-by-step [Slack channel setup
  guide](agent/slack-channel.md), and the bundled `support-bot.flux` example is now a complete,
  runnable Slack support agent that answers from a folder of docs.

### Improved

- **A program's docs folder is found next to the program, wherever you run it.** A datasource path like
  `./docs` now resolves relative to the program file itself, so `flux app run /path/to/app.flux` picks
  up the docs shipped beside it no matter which directory you launched from.

## [0.17.1] - 2026-07-12

## [0.17.0] - 2026-07-12

### New

- **Embedded agents can now run guided, resumable conversations.** An application can hand an
  embedded session an authored flow — a scripted interview or wizard that asks a question, waits for
  the person's answer, then asks the next — and drive it turn by turn. Each turn tells you whether
  the conversation is still waiting on an answer, and with a storage directory the wait is durable:
  the process can pause, exit, and a later run reopens the session by id and continues exactly where
  it left off. No model call is spent on the scripted questions, and every action the flow takes
  still passes through flux's safety and approval checks.
- **Embedded conversational agents can now delegate to sub-agents.** An application can give an
  embedded chat agent a set of named helper roles; when a turn decides to hand off a piece of work,
  it runs that helper as a child agent — through the same safety and approval checks — and the
  child's token usage shows up in the conversation's cost trace. Each child is bounded by a
  time limit (ten minutes by default, adjustable), and cancelling a streamed turn also stops a
  running child. This was already available for one-shot flows; it now works on the conversational
  agent too.
- **Embedded agents get finer control over tools and long conversations.** An application can now
  group tools so they're only offered to the model when they're relevant (revealed by a signal it
  provides), and can set when a long conversation gets automatically summarized to stay within the
  model's context window. These controls already governed the `flux` CLI; they're now available when
  embedding an agent too.
- **Flow runs now report what their AI steps cost.** When a flow uses model-backed steps (extract,
  rank, judge, reason, synthesize, rewrite), the result now includes the total token usage of those
  calls — so an application can measure and bill the spend of a flow. A flow with no AI steps reports
  no usage.
- **Embedded conversations expose their history, cost, and metrics.** An embedded session can now be
  asked for its turn-by-turn summaries, its full run trace, its per-model cost in dollars, and its
  efficiency metrics — the same data the `flux` CLI reports — so an application can show or bill a
  conversation's usage. An optional pricing add-on loads your `~/.flux/pricing.toml` rates for the
  cost calculation.
- **Embedding an agent is now one line to a provider.** An optional providers add-on lets an
  application turn a model name like `claude/sonnet` or `ollama/qwen3` into a ready-to-use provider —
  resolving credentials (including Claude/Codex subscriptions and AWS Bedrock) exactly the way the
  `flux` CLI does — instead of wiring each backend by hand. The default library stays lean and pulls
  no provider code unless you ask for it.
- **Embedded agents can use installed plugins.** An optional plugins add-on lets an application load
  an installed integration plugin (GitLab, Slack, …) so its operations become tools the embedded
  agent can call — running through the same permission and approval checks, and limited to what each
  plugin's manifest declares. As always, only install plugins you trust.
- **Embedded voice agents can be script-driven.** An embedded session can now run a voice call
  driven by an authored flow: the agent speaks first, asks each scripted question, waits for the
  caller's spoken answer, and hangs up when the script finishes — with the model handling only speech
  in and out. This complements the existing model-led voice mode, where the model leads and calls
  tools.
- **Embedded agents can replay a past session.** An application can now hermetically re-run a
  recorded conversation — every tool result served from the recording, so nothing happens for real
  and the model is never called — and inspect what it did. The same time-machine replay the `flux`
  CLI offers, now available when embedding.
- **Embedded agents can explore "what if" branches.** An application can fork a recorded session at
  a chosen point, swap in a different value or an alternate plan, run the branch, and get a
  precise diff of what changed — all without altering the original session. Great for debugging
  ("what if that lookup had returned X?") and A/B exploration.
- **Flow runs can be watched step by step.** An application running a flow can now stream each step
  as it happens — every operation's call and its result — instead of only getting the final answer,
  either by supplying a handler or consuming a live event stream.

### Action needed

- **Embedding flux? Two result types gained a field.** The result an embedded *turn* returns now
  carries a `suspended` flag (whether the conversation is waiting on an answer); the result a *flow*
  run returns now carries a `usage` total. Both are marked non-exhaustive. If your code built either
  result by hand or matched all of its fields, add a `..` catch-all — a one-line change. Nothing
  changes for code that only reads the results.

## [0.16.1] - 2026-07-11

### New

- **The web toolkit is now available on crates.io.** The crate behind the agent's web capabilities —
  fetching pages as clean markdown, extracting text from PDFs, crawling a small site, and driving a
  headless browser — is now published as `codewandler-flux-web`, so you can pull the same web tools
  into your own SDK or plugin projects instead of only using them through the CLI.

## [0.16.0] - 2026-07-11

### New

- **Embedded agents can now remember.** Point an SDK client at a storage directory and its
  conversations survive restarts: reopen a session by id and keep talking — including
  conversations waiting on a human answer, which pick up exactly where they paused. The same
  directory works with `flux sessions`, `flux replay`, and `flux fork`.
- **Embedded agents take custom tools and approval policies.** An application can hand its own
  functions to an embedded conversational agent as callable tools, restrict the agent to an
  explicit tool list, and plug in its own per-call approval policy (for example, a risk-based
  confirmation gate) — everything still runs through flux's safety envelope.
- **Embedded turns can stream.** An application can now watch a turn unfold live — the assistant's
  text, its plan, and each tool call and result as they happen — either by handing in its own
  listener or by consuming a turn as an event stream, and it can cancel a turn mid-flight without
  corrupting the conversation.
- **The agent can crawl a small site.** Point it at a starting page and it follows links within the
  same site to read a whole section in one step — bounded by the page and depth limits you set —
  instead of fetching one URL at a time. It stays on the same site and won't wander off-domain, read
  robots files, or run JavaScript.
- **Fetching a PDF now returns its text.** When the agent fetches a link that turns out to be a PDF,
  it reads the document's text instead of a wall of raw bytes, so linked PDFs become usable content.
  Even a PDF served without the right file type is detected and read.

### Improved

- **A clearer guide to embedding flows.** The public SDK documentation now shows which entry point
  to use for a conversational agent, a stored or model-compiled flow, Rust-authored flow builders,
  custom language tooling, and advanced durable/replay/voice hosts. The full direct-flow lifecycle,
  policy controls, extension points, result shape, and suspension boundary are documented together.
- **Plugin operations can now declare their financial, destructive, or external-facing nature.** A
  plugin's operation manifest can tag an operation as moving money, deleting data, or sending
  something externally — flux surfaces that distinction through its operation catalog and per-call
  flow annotations automatically, without extra tagging in an authored flow.
- **More control over embeddings-powered knowledge search.** A deployment can now configure the
  embeddings service explicitly — API key, endpoint, and model — instead of only through environment
  variables, point different knowledge bases at different embedding models, and account for the
  tokens that embedding consumes.

### Fixed

- Voice sessions driven by an authored flow no longer risk the model talking over the flow's own
  spoken reply — server-side voice detection can now be configured so the flow's response speaks
  alone.

### Action needed

- **If you embed the SDK and read a client's session id:** the accessor now returns a result you
  unwrap, because a client's default session is created the first time it's used rather than up
  front — add error handling to that one call. The change is what lets a client start up without
  leaving an empty session behind and still resume the right conversation.

## [0.15.1] - 2026-07-11

## [0.15.0] - 2026-07-11

### New

- **Author a conversation as a flow.** A flow can now drive a session directly: it runs to its next
  prompt, says that prompt to the user, and resumes exactly where it left off on their reply — the
  model isn't consulted at all for the scripted skeleton, so those turns are instant, free, and
  perfectly repeatable. Where you *do* want model judgment, the flow delegates a bounded segment
  with `ai_segment(goal, tools, max_rounds, until?)`: you state the goal, which tools it may touch,
  how many rounds it gets, and optionally which value being filled ends it early — then control
  returns to the flow. Everything a segment does still passes through the same authorization and
  approval checks as any other run.
- **Flow-driven voice calls.** The same works over the realtime voice channel: the flow's authored
  prompts are spoken aloud to the caller, the caller's reply resumes the flow, and when the flow
  completes the call ends (or hands off) cleanly — classic-IVR determinism, with model cognition
  only where the flow explicitly asks for it.
- **Ask a flow where its risk lives.** Analysis can now report, per step of a flow, which effects it
  has (network, money, writes, …), its risk level, and whether it's safe to retry — so a visual
  editor or reviewer can pin down exactly which call moves money instead of only knowing that
  something in the flow does.

### Improved

- **A clearer front door for flux.** The project page now leads with what makes flux different—the
  model proposes a typed plan and the runtime controls execution—and uses a restrained new identity
  built around that explicit boundary instead of generic AI imagery.

### Fixed

- **An expired codex login now tells you what to do.** When a stored codex (or claude) sign-in
  expires, flux used to fail with a cryptic decode error; it now surfaces the provider's actual
  reason and points you to `flux auth login codex` to sign in again.

### Action needed

- **If you implement the voice turn-handler in your own integration:** `turn` now returns a reply
  value that signals whether the call continues or completes — return the "continue" variant with
  your text to keep the previous behavior. (The new "speak first" and "call ended" hooks have
  defaults; existing sinks keep compiling.)

## [0.14.9] - 2026-07-11

### New

- **Opt-in OS-level process sandboxing.** flux can now confine shell commands and plugin
  subprocesses at their spawn boundary inside an OS sandbox: bubblewrap on Linux, Seatbelt on
  macOS. It's an extra, opt-in layer *underneath* flux's existing safety checks, not a replacement
  for them: even a plugin that tries to bypass those checks with a raw system call is now confined
  in what it can read, write, and reach on the network. Off by default. Turn it on with
  `flux --sandbox`, a `[sandbox]` block in `.flux/config.toml`, or `FLUX_SANDBOX=on`; add
  `require = true` (or `FLUX_SANDBOX=require`) to have flux refuse to run rather than continue
  unconfined when no sandbox is available. A new "OS process sandboxing" security guide on the
  docs site covers what it protects, what it doesn't yet (stated plainly, not oversold), and how
  it composes with the existing plugin security model. macOS support is implemented and tested in
  isolation but still awaiting verification on real Apple hardware; Windows support isn't built
  yet — enabling the sandbox there prints a warning (or, under
  `require`, refuses to start) rather than silently doing nothing.

### Fixed

- **Sandboxing now fails closed and preserves supported workflows.** A malformed config stops
  startup before any plugin can run; backend checks cannot hang on forked children and work on
  NixOS/Guix PATH layouts; DNS no longer re-exposes host IPC sockets; writable `/` is rejected and
  missing configured output directories are created; linked Git worktrees can update their index
  and objects; and local eval hosts retain provider access when their descendants use network-off
  confinement.

### Improved

- GitLab results now tell you exactly what was cut, and nothing arrives broken. File previews
  capped by size stay valid (no more undecodable base64 fragments), size caps are exact — the
  returned text never exceeds the limit you set, marker included — and comparisons flag
  truncation at the top level whenever files, diffs, or commits were cut (commits now have their
  own cap too). Asking a merge request for one specific file finds it even in huge MRs (file
  lookups previously stopped at the first 200 changed files), review comments can anchor on
  deleted lines, listings accept an explicit page number to walk beyond a capped first page, and
  repository archive downloads refuse to exceed a size limit (50 MB unless you raise it). Arrives
  with the next plugin pack update.

- GitLab reads now do exactly what you asked. Listing operations accept `per_page` as a synonym of
  `limit` instead of quietly ignoring it, and a zero or negative limit is rejected up front rather
  than silently returning the default page. Code search refuses an ambiguous project+group
  combination (and a branch filter on group searches, which GitLab doesn't support) instead of
  quietly picking one, and its description now says when instance-wide search needs GitLab's
  advanced code search. A typo in an index-build selector is an error instead of a successful
  "indexed 0 items", a job-status filter rejects unknown statuses instead of skipping them, and the
  merge-request/issue/project listings state plainly that they default to open items and
  your own memberships. Arrives with the next plugin pack update.

## [0.14.8] - 2026-07-10

### New

- **A dry run now tells you the truth.** `flux plugin call … --dry-run` used to check an
  integration call only against its declared input shape, so a "valid" preview could still fail
  the moment you ran it for real. Plugins now run the exact same validation in both places: the
  dry run reports every problem the live call would reject (a bad option value, an empty file
  list, a zero id, a missing target, an invalid search pattern, an update with nothing to change)
  plus warnings for input fields the operation doesn't recognize — and the live call refuses the
  same inputs with the same messages before anything leaves your machine. Arrives for GitLab with
  the next plugin pack update; other integrations pick it up as they rebuild.

### Fixed

- Two Slack plugin fixes, arriving with the next plugin pack update: reading messages from a
  channel whose history contains special characters (an em-dash, umlauts, emoji) no longer crashes
  the plugin — until you update, pass `text_format: "mrkdwn"` as a workaround — and file uploads
  now complete instead of failing against Slack's upload service.

### Improved

- Self-hosted platforms can authenticate flux's Vault-backed integration credential store with a
  Kubernetes service account instead of mounting a long-lived Vault token. Vault sessions renew and
  recover from projected-token rotation automatically; local and CLI credential storage is unchanged.

- The docs now include a step-by-step Slack setup guide — install the plugin, provide bot/user
  tokens (from your shell or stored once with `flux auth set`), verify with one command, and call
  your first operation — and the credentials guide explains how stored integration tokens work.

## [0.14.7] - 2026-07-10

### New

- **Store an integration token once, use it in every session.** `flux auth set <plugin>
  [<purpose>]` (e.g. `flux auth set slack bot_token`) prompts for the token — hidden, or pipe it
  in from a secret manager — and stores it securely, so integrations work in any later session
  without exporting the token in your shell environment. `--clear` removes it, and
  `flux plugin status <plugin>` shows what's configured without ever printing a secret.

### Fixed

- **Slack operations work again.** Requests to an integration whose API base URL includes a path
  (Slack's `https://slack.com/api`) dropped that path when composing the request URL, so every
  Slack operation failed with a 404. Fixed for all integrations — no plugin update needed.

## [0.14.6] - 2026-07-10

### New

- **Wire a known service once and reuse it everywhere.** `flux endpoint add <name> --url <url>`
  (with optional `--product`, `--protocol`, `--credential-ref <location>`, and repeatable
  `--label key=value`) records a service endpoint — a Postgres database, say — so the agent can
  reach it by name in this session and every later one, with no Kubernetes discovery in the picture.
  The URL must be credential-free; the credential is stored as a *location* (an `env/…`,
  `kubernetes/…`, or `plugin/…` reference), never a value. Prefer config-as-code? Declare the same
  endpoints with `[[endpoint.static]]` blocks in `.flux/config.toml`. Endpoints wired either way now
  resolve at connect time exactly like cluster-discovered ones — previously a statically-registered
  endpoint was recorded but never actually bound.

### Improved

- **The public documentation now tracks the product release.** New guides cover endpoints, saved
  flows, skills and roles, and flux's Improvement loop; the CLI, configuration, web operations,
  plugin security, and Rust SDK pages now match the shipped interfaces. The website also exposes
  this customer changelog, and automated checks catch stale commands, config examples, Flux code,
  operation names, plugin lists, and package instructions before publication.

## [0.14.5] - 2026-07-10

### Improved

- The terminal UI is now a dense daily driver: the transcript is borderless, the multiline input is
  separated only by a quiet background, pasted text stays intact, and long sessions remain smooth.
  Follow-ups typed during a turn form a visible editable queue instead of replacing one another.
  You can switch models, review and run plans, toggle shell access, inspect tools/evidence, compact
  context, create sessions, and pick or resume old sessions without leaving the UI. Resumed sessions
  reconstruct their messages, plans, tool activity, notices, and usage without running anything
  again—even for older sessions without detailed tool recordings—and the terminal is restored
  cleanly after exit or failure. Editing a queued follow-up keeps its place, pruning cannot remove
  the session you are using, and switching to the mock model remains fully offline.

## [0.14.4] - 2026-07-10

### Fixed

- The GitLab plugin's CI job-token allowlist commands (`gitlab.ci.job_token.allowlist.add`/`.remove`
  and the `groups_allowlist` counterparts) no longer fail with a `400 "id is invalid"` error when the
  project is given as a `namespace/path` rather than a numeric id.

## [0.14.2] - 2026-07-10

### Improved

- Helix highlighting setup and updates now use one safe, repeatable installer command. The editor
  guide also explains how to verify the semantic role under the cursor, why a successful health
  check does not prove which revision is active, and when a colour comes from the theme rather than
  the Flux grammar.

## [0.14.1] - 2026-07-10

### New

- Saved flows are now directly scriptable from the terminal. `flux flow list` shows project and
  global flows (plus reusable operations), and `flux flow run <name>` runs one without needing its
  path. Pass typed values with `--inputs` or repeatable `--arg key=value`; flux catches missing,
  unknown, or mistyped inputs before the flow starts. If you want natural-language input, opt in with
  `--map-inputs "deploy three replicas to dev"` — deterministic values still win, and fully supplied
  flows never call a model or require provider credentials.

### Fixed

- `flux app run --serve <address> --yes` (no program) is back to working as documented — a v0.14.0
  change broke this common form, silently ignoring the address you gave and trying to serve on the
  default one instead.

## [0.14.0] - 2026-07-10

### Improved

- Flux-Lang editor diagnostics now recognize the built-in model, knowledge-search, and web-reading
  operations available in the CLI. Files containing multiple flows or custom operations get live
  diagnostics, completion, and hover for every declaration without mixing up their source locations.
- Every flag the CLI accepts now does what its help says. Commands reject flags they can't honor
  (instead of silently ignoring them), contradictory combinations are caught up front with a clear
  usage error, and mistyped values — an unknown completion shell, a zero token limit, a malformed
  budget variable — fail loudly instead of being quietly dropped.
- `flux tui -v` now shows tool output in full, piped output (`flux usage > report.txt`) stays free
  of color codes, and abandoning a `flux auth login codex` browser flow times out cleanly instead
  of hanging forever.

### Fixed

- Setting the private-network variable to an "off" value (for example `FLUX_ALLOW_PRIVATE_NET=0`)
  no longer opens private-network access — only explicit "on" values count, and flux now warns
  whenever that access is active, naming what enabled it.
- A prompt that happens to start with an existing file name (like `flux run Cargo.toml explain
  this`) runs as a normal question again instead of being misread as a program file, and extra
  words after a program path are rejected instead of silently thrown away.
- `flux plugin status` no longer reports a working plugin as `missing` when its program is found
  via PATH, and replaying a recorded session stays fully offline even with a cloud-credential
  model configured.

### Action needed

- `flux review` now only accepts the flags it actually uses; scripts passing `--yes` or session
  flags to it will get a usage error (remove those flags — review has always been read-only and
  auto-approved). `flux app run --serve` and `flux plugin install --dir` now take their optional
  value attached with `=` (`--serve=0.0.0.0:8787`, `--dir=path`).

## [0.13.3] - 2026-07-10

### Improved

- The public docs now include a visual Infrastructure guide showing how requests become typed plans,
  how every real effect crosses the safety envelope, and how the project fits together from core
  contracts to user-facing surfaces.
- Network redirects are checked at every hop, credentials do not cross to another origin, large
  responses and command output are bounded while streaming, and timed-out commands are cleaned up.
- Plugin callbacks and filesystem reads now recover safely from cancellation and cannot follow a
  symlink outside the granted directory. Evaluation runs can bound trial concurrency while keeping
  results reproducible.

## [0.13.2] - 2026-07-09

### New

- `flux render` turns a `.flux` file into a polished, syntax-highlighted SVG image from the
  command line — either the highlighted source (the default) or the plan tree the file compiles
  to (`--view tree`). Print it to stdout or write it with `-o out.svg`. Great for READMEs, docs,
  and slides: it's the same renderer agents already use through the `flow_render` tool.

### Improved

- If you registered service endpoints (for example a Postgres database), the endpoint operations
  now show up in every session — previously they only appeared when a Kubernetes config was
  present. A broken endpoints file is reported at startup with a fix-it hint instead of being
  silently ignored.
- Old plugin registrations whose program has been deleted no longer spam one warning per plugin
  at the start of every command. You get a single line naming the affected plugins and how to fix
  or remove them, and plugins whose recorded checksum no longer matches the binary on disk are
  now refused at session start too, not just on direct plugin calls.

## [0.13.0] - 2026-07-09

### New

- The agent can now answer "what knowledge do I have?" A new read-only `sources` operation lists
  every knowledge source in the index — its name, the kinds of records it holds, and how many —
  so the agent no longer has to guess source names before searching or listing records.

## [0.12.0] - 2026-07-09

### New

- Agents can now use a real web browser — the non-visual way. flux drives headless Chrome and lets
  the agent "see" a page the way a screen reader does: a short digest of the readable content plus a
  list of the things it can act on (buttons, links, form fields), each with a stable handle. It clicks,
  types, and navigates by those handles and gets back just *what changed* — so a multi-step task on a
  live, JavaScript-heavy site costs a trickle of tokens instead of re-reading the whole page every
  turn. It never ingests raw HTML or screenshots. The browser tools only appear when a Chromium/Chrome
  browser is actually installed (set `FLUX_BROWSER_BIN` or `browser_bin` to point at one), and every
  request the page makes is held to the same egress rules as the rest of flux.
- "Read this page" now returns a clean, readable document instead of a wall of HTML. Fetching a web
  page strips the navigation, scripts, ads, and boilerplate and hands back condensed markdown — so
  the agent spends its budget on the actual content, and pages it reads become searchable afterwards.
  Ask for `raw: true` if you ever want the untouched source. There's also a pure "HTML → markdown"
  step you can run on HTML you already have.
- Agents can now make arbitrary HTTP requests. A new `http.request` capability lets a plan call
  any HTTP(S) API — pick the method, set headers and a body, and get back the status, response
  headers, and body. Non-2xx responses come back as normal results (a 404 is just a 404), and
  header values can reference secrets so tokens are never shown. Reach for it when talking to an
  API; keep using "read this page" (web fetch) when you just want a page's content. Private and
  loopback addresses stay blocked unless you grant the new `web` egress scope in config (or pass
  `--allow-private-net`).
- `flux changelog` — see what changed in your version of flux, right from the terminal.
  Shows your installed version's highlights by default; `flux changelog --all` shows the
  full history, `flux changelog <version>` a specific release.
- Flux plans now run faster: independent read-only steps (file reads, searches, status
  checks) automatically run in parallel when it is provably safe — results, ordering,
  and approval behavior are exactly as before, just sooner.
- Repeated identical reads within a single turn are answered from a result cache instead
  of re-running the work. Anything that writes invalidates it, and every new turn starts
  fresh. Turn it off with `FLUX_OP_CACHE=off` if you ever need to.
- When a plan needs a small correction, the model can now send just the fix instead of
  re-writing the whole plan — repairs get faster and cheaper, with the same safety checks
  on the final plan.
- A `fable` model shorthand: `-m fable` (or `-m claude/fable` on a Claude subscription) runs
  Claude Fable 5, Anthropic's most capable model. And plain `-m claude` now works — it's
  shorthand for the subscription's default model.
- A new docs page covers running flux on a Claude Code / Claude Max subscription: which
  models you get, how they're billed, and the exact request guarantees flux gives each one.

### Improved

- Editor diagnostics got precise: warnings from the flux language server now point at the
  exact statement in your `.flux` file (unknown operations, unbound `$variables`, wrong
  argument counts) instead of just naming the problem.
- The language now accepts more real-world text: kebab-case flow names, dotted operation
  names, blank lines and comments between a header and its body, single-quoted strings in
  conditions, and scientific-notation numbers.

### Fixed

- Claude Haiku works again everywhere it's offered (`-m claude/haiku`, `anthropic/haiku`,
  `aws/haiku`, and via OpenRouter). Runs used to fail with an "adaptive thinking is not
  supported" error; flux now tailors each request to what the chosen model actually
  accepts, so every documented model just works — including future ones on day one.
- A mistyped model spec (like `claude/` with nothing after the slash) now fails instantly
  with a hint about what to write, instead of a confusing error from the API.

### Action needed

- If you granted `web_fetch` access to private/internal hosts in `config.toml` under
  `[private_net] web_fetch = …`, rename that key to `web`. The web tools now share one
  `[private_net] web` grant; an old `web_fetch` entry is ignored (public-only by default).
- flux's default model is now **Claude Sonnet 5** (the current Sonnet): the `sonnet`
  shorthand — and any run without `-m` — picks it up automatically. It's stronger at
  coding and currently cheaper. To stay on the previous model, set
  `model = "anthropic/claude-sonnet-4-6"` in `.flux/config.toml` or pass it via `-m`.

## [0.11.6] - 2026-07-09

### New

- The agent can discover and run your saved flows: put reusable `.flux` files in
  `.flux/flows` (project) or `~/.flux/flows` (global) and they become listable and
  runnable mid-conversation, with your inputs passed in safely.

### Improved

- Plans are now requested from the model in a leaner format by default — noticeably lower
  token usage and cost per turn, with the same quality.

### Fixed

- Reading a shared directory like `@global_ops` directly (not a file inside it) now works;
  previously global reusable operations could silently fail to load.

## [0.11.5] - 2026-07-09

### New

- Editor support grew: syntax highlighting for `.flux` files in Helix, Neovim, and Zed via
  a dedicated grammar, alongside the flux language server (completion, hover, diagnostics,
  formatting). See the "Editor support" docs page for setup.

## [0.11.4] - 2026-07-09

### New

- Every flux language feature now has a native, readable spelling — durable steps,
  approvals, rate limits, sagas with rollback, try/catch, and more no longer need JSON
  escape blocks. Your existing files keep working unchanged.
- A language server for `.flux` files (`flux-lsp`): error squiggles as you type,
  completion for operations and variables, hover documentation, and formatting.

## [0.9 – 0.11.3] - 2026-07

### Highlights

- flux became installable everywhere: prebuilt binaries for Linux/macOS/Windows and the
  full library stack published to crates.io.
- Time Machine: `flux replay`, `flux fork`, and `flux diff` — deterministically re-run,
  branch, and compare past sessions.
- Postgres storage option for teams that want sessions and events in a shared database.
- Signed plugin packs (`flux plugin install gitlab|slack|kubernetes|…`) with verified
  downloads, plus OAuth login for plugins that need it (`flux auth login <plugin>`).
- Sub-agents, A2A server support, budgets/usage reporting with real pricing, and a large
  set of built-in data-transform operations.

---

Engineering-level detail for every release lives in
[CHANGELOG.md](https://github.com/codewandler/flux/blob/main/CHANGELOG.md).
