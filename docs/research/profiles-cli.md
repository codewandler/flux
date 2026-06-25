# Profiles — CLI / terminal-first coding agents

Full profiles for the cluster closest to flux: agents whose primary surface is a terminal CLI/TUI
(not an editor extension). See [landscape.md](landscape.md) for methodology, the category schema,
the master comparison matrices, the **caveats block**, and the *"what this means for flux"*
synthesis. Star counts are GitHub-API values observed **~2026-06-25**; treat every figure as a
point-in-time snapshot.

Each profile follows the nine categories: **popularity · vendor · license · tech/architecture ·
model support · features · safety/permissions · extensibility · status/differentiator.**

---

## Claude Code — Anthropic

- **Popularity:** `anthropics/claude-code` ~**134.2K** stars. The reference agent of the current era;
  created Feb 2025.
- **Vendor:** Anthropic.
- **License:** **Proprietary** ("All rights reserved"; Commercial ToS). The public repo is mostly
  issues/docs/plugins — the agent ships as a minified npm package. Its unobfuscated TS source
  *leaked ~Mar 31 2026* (accidental source maps, mass-forked); the proprietary license still stands.
- **Tech / architecture:** TypeScript / Node CLI + TUI. Talks to Anthropic's API (routable via
  Bedrock / Vertex / Foundry). **MCP** support is first-class.
- **Model support:** **Claude-only** (vendor-locked). No BYO third-party models.
- **Features:** Agentic loop, multi-file edit, planning, **subagents** + agent teams, **hooks**,
  **skills**, `CLAUDE.md` project memory, web/IDE integrations, plugin marketplace, git integration.
- **Safety / permissions:** Permission model with approval prompts + a sandboxed bash tool. Default
  posture is permissive-with-prompts rather than default-deny.
- **Extensibility:** The richest in the field — subagents, hooks, skills, MCP servers, a plugin
  marketplace.
- **Status / differentiator:** The category leader on mindshare and extensibility; the bar everyone
  else is measured against. Vendor-locked and proprietary.

## OpenAI Codex CLI — OpenAI

- **Popularity:** `openai/codex` ~**93.4K** stars; ~840 releases in ~14 months. **~5M weekly users**
  (OpenAI's own figure, Jun 2026 — one of the only officially published large numbers in the field).
- **Vendor:** OpenAI.
- **License:** **Apache-2.0** — genuinely open.
- **Tech / architecture:** Rewritten from TypeScript to **Rust (~96%)** in mid-2025; single binary.
  **MCP** support; `AGENTS.md` project conventions.
- **Model support:** OpenAI by default, but **supports local / BYO** (Ollama, LM Studio, any
  OpenAI-compatible endpoint).
- **Features:** Agentic loop, approval modes, web search, image input, `/review`, cloud tasks
  (background agents), git/PR.
- **Safety / permissions:** **Best-in-class OS-level sandboxing** — Linux **Landlock + seccomp**,
  macOS **Seatbelt** — plus explicit approval modes. The closest mainstream peer to flux on
  isolation *intent* (though it does not pair this with a default-deny authorization policy).
- **Extensibility:** MCP; configurable approval/sandbox policies.
- **Status / differentiator:** The open, Rust, sandbox-first counterweight to Claude Code; strong
  benchmark presence (see Terminal-Bench in [profiles-autonomous.md](profiles-autonomous.md)).

## Gemini CLI — Google

- **Popularity:** `google-gemini/gemini-cli` ~**105.5K** stars; launched Jun 2025.
- **Vendor:** Google.
- **License:** **Apache-2.0** today — but see status.
- **Tech / architecture:** TypeScript / Node CLI. **MCP**, `GEMINI.md`, checkpointing, GitHub
  Actions integration, Trusted-Folders permission model, Google Search grounding.
- **Model support:** **Gemini-only** (vendor-locked).
- **Features:** Agentic loop, multi-file edit, search grounding, checkpoints, GitHub Actions.
- **Safety / permissions:** Trusted-Folders permission gating; approval prompts.
- **Extensibility:** MCP; the upstream that **Qwen Code** forked.
- **Status / differentiator:** ⚠️ **Uncertain/transitioning** — Google is reported to be moving it to
  a closed-source Go "Antigravity CLI" and, as of ~Jun 18 2026, to have stopped serving the
  consumer/free tier (enterprise retains access). *Verify current status before citing.*

## OpenCode — SST / Anomaly

- **Popularity:** `sst/opencode` (= `anomalyco/opencode`) ~**178.2K** stars — the most-starred
  open-source terminal agent.
- **Vendor:** SST / Anomaly (Dax Raad).
- **License:** **MIT.**
- **Tech / architecture:** **TypeScript, client/server** — a persistent server holds sessions; a
  polished TUI, a Tauri desktop app, and IDE clients attach to it. **MCP**, **LSP** integration.
- **Model support:** **Model-agnostic — ~75+ providers, mid-session switching.** Among the most
  provider-flexible tools anywhere.
- **Features:** Build/plan agents, **session sharing via links**, plugin SDK, multi-file edit.
- **Safety / permissions:** Permissive sandboxing by default (approval prompts), not default-deny.
- **Extensibility:** Plugin SDK + MCP + LSP.
- **Status / differentiator:** **Lineage worth knowing:** the *original* OpenCode was a Go project by
  Kujtim Hoxha (`opencode-ai/opencode`, now archived). After a naming dispute, **Charm kept the Go
  code and rebranded it Crush**, while **SST kept the "OpenCode" name and rewrote it in TypeScript**.

## Goose — Block → Linux Foundation

- **Popularity:** `block/goose` (= `aaif-goose/goose`) ~**50.1K** stars.
- **Vendor:** Originally **Block** (Square/CashApp); **governance moved to the Agentic AI Foundation
  under the Linux Foundation** — now vendor-neutral.
- **License:** **Apache-2.0.**
- **Tech / architecture:** **Rust** single binary, local-first. **General-purpose** agent (not
  coding-only). **MCP**-centric (70+ MCP "extensions").
- **Model support:** **Model-agnostic — 50+ providers**, incl. local / air-gapped.
- **Features:** **Recipes** (portable YAML workflows), subagents, broad MCP toolset.
- **Safety / permissions:** Sandbox mode + an "adversary reviewer" pattern; approval gating.
- **Extensibility:** MCP extensions + Recipes.
- **Status / differentiator:** The vendor-neutral, foundation-governed, Rust local-first agent;
  Recipes are a distinctive portable-workflow primitive.

## Crush — Charm

- **Popularity:** `charmbracelet/crush` ~**25.7K** stars.
- **Vendor:** **Charm** (makers of Bubble Tea / Glow / Gum).
- **License:** **FSL-1.1-MIT — source-available, not OSI-open** (auto-converts to MIT after 2 years).
- **Tech / architecture:** **Go**, best-in-class TUI, the widest OS matrix (incl. Android/BSD).
  Built-in **LSP**; **MCP** over stdio / HTTP / SSE.
- **Model support:** Fully **BYO-key, model-agnostic**; mid-session model switching.
- **Features:** Agent Skills; multi-file edit; per-tool permissions.
- **Safety / permissions:** Per-tool permission prompts + a `--yolo` bypass.
- **Extensibility:** MCP + Agent Skills; subagents emerging, not yet first-class.
- **Status / differentiator:** The "aesthetics + DX matter" pick; the branded continuation of the
  original Go OpenCode codebase.

## Qwen Code — Alibaba (Qwen)

- **Popularity:** `QwenLM/qwen-code` ~**25.5K** stars.
- **Vendor:** Alibaba's Qwen team.
- **License:** **Apache-2.0.**
- **Tech / architecture:** Began as an Apache-2.0 **fork of Gemini CLI** (v0.8.2) and diverged into
  an independent multi-platform framework: daemon mode, desktop app, JetBrains/Zed plugins,
  TS/Python/Java SDKs, IM bots. **MCP**, **LSP**.
- **Model support:** **Model-agnostic** (OpenAI / Anthropic / Gemini / Qwen + local Ollama / vLLM,
  runtime switching, "Agent Arena"); tuned for Qwen3-Coder.
- **Features:** **SubAgents / Agent Teams**, **Plan Mode**, Computer Use, git worktrees, multi-file edit.
- **Safety / permissions:** Approval prompts inherited/extended from the Gemini CLI base.
- **Extensibility:** MCP, SDKs in three languages, IDE plugins.
- **Status / differentiator:** A Gemini-CLI fork that outgrew its origin into a broad agent platform;
  strongest when paired with Qwen3-Coder.

## Amp — Sourcegraph → Amp Inc.

- **Popularity:** **Closed** — no public source repo (only a small examples repo). Metric is funding:
  Sourcegraph raised ~$245M (a16z-led Series D, ~$2.6B valuation).
- **Vendor:** Built at **Sourcegraph**; in Dec 2025 **Amp and Sourcegraph split into two independent
  companies** (Amp founders left to form Amp Inc.).
- **License:** **Proprietary / closed.**
- **Tech / architecture:** Multi-surface — **CLI + VS Code + web + mobile, synced threads**. **MCP**;
  a TS plugin system.
- **Model support:** **Curated multi-model, no BYO key** — "smart / deep / rush" modes (smart ≈ Claude
  Opus, also GPT-5.x) plus a separate **"Oracle"** reasoning model.
- **Features:** First-class **subagents**, MCP, multi-file edit, "Deep/Oracle" extended reasoning.
- **Safety / permissions:** Hosted approval model; no local sandbox story published.
- **Extensibility:** MCP + TS plugins.
- **Status / differentiator:** Unusual **ad-supported free tier that trains on your code**, or
  pay-as-you-go. The successor to Sourcegraph's Cody (which went enterprise-only — see
  [profiles-ide.md](profiles-ide.md)).

## Aider — independent (Paul Gauthier)

- **Popularity:** `Aider-AI/aider` ~**46.7K** stars (org renamed from `paul-gauthier/aider`). A top-3
  terminal agent by daily active devs.
- **Vendor:** Independent OSS; community-driven, no major VC.
- **License:** **Apache-2.0.**
- **Tech / architecture:** **Python.** Single-agent loop operating directly on the local git repo
  (no Docker sandbox by default). **tree-sitter "repo map"** for whole-repo context; edits applied as
  diffs / whole-file; **auto-commits each change to git.**
- **Model support:** Strongly **model-agnostic / BYO** via LiteLLM (Claude, GPT, DeepSeek, Gemini,
  local). Supports a two-model **"architect / editor"** split.
- **Features:** Pair-programming-first (human-in-the-loop) but scriptable/autonomous; repo-wide edits,
  `--auto-test` lint/test-fix loop, voice input, image/URL context.
- **Safety / permissions:** Runs with your shell's permissions; sandboxing is whatever the shell
  provides. Git history is the safety net (every change is a reviewable commit).
- **Extensibility:** Provider breadth via LiteLLM; no plugin system per se.
- **Status / differentiator:** The de-facto terminal pair-programmer; its **polyglot leaderboard** is
  an industry reference (see [profiles-autonomous.md](profiles-autonomous.md)). Philosophy: "natural
  language → reviewable git commits."

## pi — Mario Zechner / Earendil

- **Popularity:** `earendil-works/pi` ~**65.4K** stars (the old `badlogic/pi-mono` slug now redirects
  here — confirmed via the API). npm: `@mariozechner/pi-coding-agent`; domain pi.dev.
- **Vendor:** Created by **Mario Zechner** (GitHub `badlogic`, libGDX creator). **Since Apr 2026
  stewarded by Earendil**, with Zechner retaining decision authority; core stays MIT.
- **License:** **MIT** (Earendil has signaled some future add-ons may be Fair Source).
- **Tech / architecture:** **TypeScript**, deliberately **minimal** — **four tools (read, write,
  edit, bash)**, a system prompt under ~1,000 tokens. Three packages: `pi-ai` (unified multi-provider
  LLM API), `pi-agent-core` (agent loop + tool calling), `pi-coding-agent` (CLI). **Four runtime
  modes:** interactive TUI, print/JSON, RPC, embeddable SDK.
- **Model support:** Very broad via `pi-ai` — Anthropic, OpenAI, Google, Azure, Bedrock, Mistral,
  Groq, Cerebras, xAI, Hugging Face, OpenRouter, Ollama.
- **Features:** Extensions, skills, prompt templates, themes — all shippable as "pi packages"
  (npm/git). Minimalism *is* the feature: "what you leave out matters more."
- **Safety / permissions:** Lean; approval around bash. Not a default-deny policy model.
- **Extensibility:** The whole point — a small core with a package ecosystem; embeddable SDK.
- **Status / differentiator:** **One of the two names the user asked to pin down.** A reaction to
  Claude Code's growing complexity; the philosophical opposite of the "kitchen-sink" agents.
  *(Possible alternative if the user meant something else: `can1357/oh-my-pi` ~14.5K stars, a separate
  terminal agent with hash-anchored edits, LSP, subagents.)*

## hermes — Nous Research

- **Popularity:** `NousResearch/hermes-agent` ~**202.1K** stars — *verified via API; genuinely one of
  the most-starred agent repos.* Created Jul 2025, very actively maintained.
- **Vendor:** **Nous Research** (the open-model lab behind the **Hermes** fine-tuned LLMs).
- **License:** **MIT.** Python (82%) + TypeScript.
- **Tech / architecture:** A model-agnostic **general agent harness** — *not coding-specific*, though
  it has strong dev capabilities. Full TUI; **multiple execution backends** (local, Docker, SSH,
  Singularity, Modal, Daytona); messaging gateways (Telegram/Discord/Slack/WhatsApp/Signal).
- **Model support:** Nous Portal, OpenRouter (200+), NovitaAI, NVIDIA NIM, z.ai/GLM, Kimi/Moonshot,
  MiniMax, Hugging Face, OpenAI, custom endpoints. **Distinct from the Hermes *models*** — the agent
  can drive any LLM.
- **Features:** **Self-improving loop** — creates skills from experience, refines them, persists
  knowledge, searches its own past conversations, models the user across sessions; cron scheduler;
  subagent spawning; batch trajectory generation for model training.
- **Safety / permissions:** Sandboxed execution backends; no flux-style default-deny policy.
- **Extensibility:** Skills, subagents, messaging gateways, pluggable backends.
- **Status / differentiator:** **The second name the user asked to pin down.** Important nuance for the
  comparison: it's a *general-purpose, self-improving* assistant harness that happens to code well —
  not a coding-specific tool. ("hermess" is a typo for hermes; no separate tool found.)

## Kimi CLI — Moonshot AI

- **Popularity:** `MoonshotAI/kimi-cli` ~**9.1K** stars.
- **Vendor:** Moonshot AI.
- **License:** **Apache-2.0.** Python.
- **Tech / architecture:** Terminal agent powered by **Kimi K2.x** (256K context). Notably can also be
  used as a *backend model* inside Claude Code / Roo.
- **Model support:** Kimi-centric, but usable as an OpenAI-compatible backend elsewhere.
- **Features:** Agentic loop, repo analysis, long-context tasks.
- **Safety / permissions:** Standard approval prompts.
- **Extensibility:** Slots into other harnesses as a provider.
- **Status / differentiator:** The Moonshot/Kimi-native CLI; strongest for very-long-context work.

## Forge (ForgeCode) — Antinomy / Tailcall

- **Popularity:** `antinomyhq/forge` (= `tailcallhq/forgecode`) ~**7.4K** stars; npm `@antinomyhq/forge`.
- **Vendor:** Antinomy / Tailcall (commercial ForgeCode.dev offering).
- **License:** **Apache-2.0.**
- **Tech / architecture:** **Rust (~94%).** Interactive terminal UI with a persistent conversational
  loop; **MCP** for external tools.
- **Model support:** Very broad — "Claude, GPT, o-series, Grok, DeepSeek, Gemini and 300+ models",
  **BYO key.**
- **Features:** Terminal pair-programming/agent loop, multi-step work, configurable agent personas
  (`awesome-forge-agents`).
- **Safety / permissions:** Less prominent than OpenHands/Codex; standard prompts.
- **Extensibility:** MCP + personas.
- **Status / differentiator:** Rust, very wide provider breadth; positioned as a fast terminal agent.

## gptme — independent (Erik Bjäreholt)

- **Popularity:** `gptme/gptme` ~**4.3K** stars; one of the earliest agent CLIs (spring 2023).
- **Vendor:** Independent OSS (gptme.org).
- **License:** **MIT.** Python.
- **Tech / architecture:** Local-first terminal agent shipping shell, Python, web/browse, and vision
  tools; designed to also run as a **persistent, headless autonomous agent** (CI, tmux, SSH).
- **Model support:** Provider-agnostic / BYO — Anthropic, OpenAI, Google, xAI, DeepSeek, OpenRouter,
  or **fully local via llama.cpp.**
- **Features:** Writes code, runs the terminal, browses, vision input; build-your-own persistent agent.
- **Safety / permissions:** Minimal — runs with your shell's permissions (less isolated than
  Docker/VM agents).
- **Extensibility:** Tool set + persistent-agent scaffolding.
- **Status / differentiator:** Pitched as an open alternative to Claude Code / Codex / Cursor / Warp;
  emphasis on local + persistent autonomy.

## Codebuff — CodebuffAI

- **Popularity:** `CodebuffAI/codebuff` ~**6.6K** stars.
- **Vendor:** CodebuffAI.
- **License:** **Apache-2.0.** TypeScript.
- **Tech / architecture:** Open-source **multi-agent** CLI.
- **Model support:** Multi-provider.
- **Features:** Multi-agent orchestration from the terminal; markets itself as beating Claude Code on
  some benchmarks (vendor claim — flag).
- **Safety / permissions:** Standard prompts.
- **Extensibility:** Agent definitions.
- **Status / differentiator:** A smaller multi-agent challenger; benchmark claims are self-reported.

## Plandex — independent (plandex-ai)

- **Popularity:** `plandex-ai/plandex` ~**15.5K** stars.
- **Vendor:** Independent OSS; the hosted **Plandex Cloud wound down starting Oct 2025** (OSS lives on,
  self-hostable).
- **License:** **MIT.**
- **Tech / architecture:** **Go** (single-binary self-host). Built for **large, multi-file, multi-step
  tasks** — up to ~2M tokens of context (~100k/file), indexes 20M+ token dirs via **tree-sitter
  project maps** (30+ languages).
- **Model support:** **Model-agnostic / BYO** — mix Anthropic/OpenAI/Google/open models, with context
  caching across them.
- **Features:** Planning-first (plan → execute many steps), **cumulative diff-review sandbox** that
  isolates AI changes until approved, controlled command execution with rollback.
- **Safety / permissions:** The change-isolation sandbox + human-reviewable diffs are its safety model.
- **Extensibility:** Provider breadth; no plugin marketplace.
- **Status / differentiator:** Best-in-class **large-project / long-task** focus; Go gives easy
  single-binary self-hosting.

## Warp (Oz) — Warp

- **Popularity:** **Closed** (the Warp terminal app). Metric is product traction, not stars.
- **Vendor:** Warp.
- **License:** **Proprietary.**
- **Tech / architecture:** Warp pivoted from "AI terminal" to an **Agentic Development Environment**;
  the **"Oz" platform launched ~Feb 10 2026.** Can orchestrate Oz, **Claude Code, Codex, and Gemini
  CLI** locally or in the cloud.
- **Model support:** Multi-model; orchestrates other agents.
- **Features:** Multi-agent orchestration, cloud or local execution, deep terminal integration.
- **Safety / permissions:** Hosted approval model.
- **Extensibility:** Acts as a conductor over other CLIs.
- **Status / differentiator:** Notable as an *orchestrator of other harnesses* rather than a single
  agent — a "meta" surface.

## Auggie (Augment CLI) — Augment Code

- **Popularity:** **Closed engine**, distributed as npm `@augmentcode/auggie` (the IDE extensions have
  ~765K VS Code / ~675K JetBrains installs).
- **Vendor:** Augment Code (~$252M raised; ~$977M post-money, Apr 2024).
- **License:** **Proprietary** (the CLI wrapper is distributed openly; the engine is closed).
- **Tech / architecture:** Terminal agent that **runs as an MCP server** and supports **ACP** (Agent
  Client Protocol). Built around Augment's large-context **Context Engine**.
- **Model support:** Multi-model hosted (model choice abstracted), not primarily BYO.
- **Features:** Whole-repo context retrieval, agent mode; exposes its Context Engine over MCP to other
  agents.
- **Safety / permissions:** Enterprise-oriented; hosted gating.
- **Extensibility:** MCP + ACP.
- **Status / differentiator:** Best-in-class large/legacy-codebase context. **Augment is sunsetting its
  IDE completion features and moving upmarket to "Cosmos," a cloud multi-agent platform** — see
  [profiles-ide.md](profiles-ide.md).

## RA.Aid — independent (ai-christianson)

- **Popularity:** `ai-christianson/RA.Aid` ~**2.2K** stars.
- **Vendor:** Independent OSS.
- **License:** **Apache-2.0.** Python.
- **Tech / architecture:** A "research-and-implement" autonomous loop ("develop software
  autonomously"); combines a research phase with an implementation phase.
- **Model support:** Multi-provider / BYO.
- **Features:** Autonomous research → plan → implement; repo-wide work.
- **Safety / permissions:** Standard prompts.
- **Extensibility:** Configurable.
- **Status / differentiator:** A smaller but interesting research-then-build loop design.

## iFlow CLI — iFlow AI

- **Popularity:** `iflow-ai/iflow-cli` ~**5.1K** stars.
- **Vendor:** iFlow AI (Chinese ecosystem).
- **License:** Not clearly stated (flag). Shell-based.
- **Tech / architecture:** Terminal agent for repo analysis + workflow automation.
- **Model support:** Multi-provider (China-region models prominent).
- **Features:** Repo analysis, automation workflows.
- **Safety / permissions:** Standard prompts.
- **Extensibility:** Configurable.
- **Status / differentiator:** A notable non-Western terminal agent; lighter documentation in English.

## Amazon Q Developer CLI — AWS

- **Popularity:** `aws/amazon-q-developer-cli` ~**2.0K** stars (the CLI client is OSS; the service is
  AWS-backed).
- **Vendor:** AWS / Amazon.
- **License:** **Apache-2.0** (client). **Rust.**
- **Tech / architecture:** Agentic terminal chat; native tools + **MCP**; talks to Amazon Bedrock
  (prominently Claude).
- **Model support:** Bedrock-managed (Claude family), not BYO third-party.
- **Features:** Agentic shell tasks, code actions; tied into the broader Q Developer suite.
- **Safety / permissions:** AWS-managed.
- **Extensibility:** MCP.
- **Status / differentiator:** ⚠️ The broader **Amazon Q Developer is under end-of-support** (new
  signups blocked ~May 2026; IDE plugins EOS ~Apr 2027), with users steered to **Kiro** — see
  [profiles-ide.md](profiles-ide.md).

## Open Interpreter — Open Interpreter

- **Popularity:** `openinterpreter/open-interpreter` ~**64.1K** stars.
- **Vendor:** Open Interpreter.
- **License:** **Apache-2.0.**
- **Tech / architecture:** **Rewrote in Rust** (API confirms `language: Rust`) and rebranded as "a
  lightweight coding agent for **open models** (DeepSeek, Kimi, Qwen)." Runs code locally in a
  natural-language REPL.
- **Model support:** Open-model-first; also hosted providers.
- **Features:** Local code execution (Python/shell/etc.) driven by natural language; data/file tasks.
- **Safety / permissions:** Runs locally; approval before execution. Historically "runs code on your
  machine" — isolation is the user's responsibility.
- **Extensibility:** Profiles / config.
- **Status / differentiator:** A very-high-star generalist "let an LLM run code locally" tool; the
  2026 Rust rewrite repositioned it toward open models.
