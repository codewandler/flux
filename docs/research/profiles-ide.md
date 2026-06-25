# Profiles — IDE-integrated agents & completion platforms

Full profiles for editor-based tools: VS Code extensions, VS Code *forks*, JetBrains-native plugins,
and self-hosted completion platforms. These are a different surface from flux (CLI/TUI/SDK/HTTP), but
they define the feature expectations and the competitive frame. See [landscape.md](landscape.md) for
methodology, the category schema, the master matrices, and the caveats block. Star counts are GitHub
API values observed **~2026-06-25**.

> **Two contested 2026 claims to carry carefully** (also flagged in landscape.md): a reported
> **SpaceX/xAI acquisition of Cursor** (~$60B) is *uncertain* and not treated as fact here; a reported
> **Cursor acquisition of Continue** is better corroborated but still secondary-sourced. Verify
> against primary press before relying on either.

Categories: **popularity · vendor · license · tech/architecture · model support · features ·
safety/permissions · extensibility · status/differentiator.**

---

## Cursor — Anysphere

- **Popularity:** Closed core; `cursor/cursor` (docs/issues only) ~**33.0K** stars. Reported **1M+
  daily active users**, 1M+ paying customers; ARR reportedly ~$100M (Jan 2025) → **$500M (Jun 2025,
  TechCrunch — best-sourced)** → higher figures in late 2025/2026 (weaker sourcing).
- **Vendor:** **Anysphere, Inc.** (SF, founded 2022). Funding: Series C $900M @ **$9.9B** (Jun 2025) →
  **Series D $2.3B @ $29.3B** (Nov 2025). *(SpaceX/xAI ~$60B acquisition: reported, **uncertain**.)*
- **License:** **Proprietary**; a **fork of VS Code** (VS Code core is MIT).
- **Tech / architecture:** Standalone **VS Code fork** (not an extension) — deep editor-internals
  access (shadow workspaces, native diffs, terminal interception). MCP landed late 2025.
- **Model support:** Multi-provider router (Claude, GPT, Gemini, DeepSeek) **+ in-house models**:
  **Fusion** (Tab autocomplete) and the **Composer** agentic family. BYO via OpenAI-compatible
  endpoints (routes through Cursor's backend; **Tab is locked to Fusion**).
- **Features:** Tab autocomplete, inline edit, chat, **Composer/Agent** multi-file mode, automatic
  codebase indexing/RAG, checkpoints, planning, **parallel agents via git worktrees** (Cursor 2.0),
  **Cloud Agents** in isolated VMs, **BugBot** PR review.
- **Safety / permissions:** Approval modes; cloud agents run in isolated VMs.
- **Extensibility:** MCP; the VS Code extension ecosystem (mostly).
- **Status / differentiator:** Revenue/valuation leader; deepest VS-Code-fork integration; cheap
  proprietary models. Acquired **Supermaven** (Nov 2024) and reportedly **Continue** (2026).

## Windsurf — Codeium → Cognition ("Devin Desktop")

- **Popularity:** Closed; the legacy VS Code extension `Codeium.codeium` ~3.86M installs. At
  acquisition (Jul 2025): **$82M ARR, 350+ enterprise customers.**
- **Vendor — the verified 2025 saga:** **OpenAI's ~$3B acquisition collapsed** (~Jul 11 2025).
  **Google paid $2.4B for a license + acquihire** (hired CEO Varun Mohan + key R&D; did *not* buy the
  company). **Cognition (makers of Devin) acquired the remaining company** (~Jul 14 2025).
- **License:** **Proprietary**; VS Code fork.
- **Tech / architecture:** Standalone **VS Code fork** bundling the **Cascade** agent; also JetBrains
  plugins + the legacy extension.
- **Model support:** Proprietary **SWE-1 / 1.5 / 1.6** models + third-party (Claude, GPT). Local/BYO
  more limited than Cursor.
- **Features:** **Cascade** agentic system (multi-step planning, multi-file edits, terminal exec,
  "Windsurf Tab" autocomplete), MCP, checkpoints, codebase RAG.
- **Safety / permissions:** Approval/autonomy modes.
- **Extensibility:** MCP.
- **Status / differentiator:** Pioneered the smooth "flow"/Cascade UX. Now a **Cognition** product
  fused with **Devin** (reportedly rebranding toward "Devin Desktop" in 2026 — 2026 details less
  certain). Strategically: an IDE front-end for autonomous Devin cloud agents.

## Cline — Cline (formerly "Claude Dev")

- **Popularity:** `cline/cline` ~**63.9K** stars; VS Code (`saoudrizwan.claude-dev`) ~4.44M installs;
  Open VSX ~4.72M. The largest of the Cline-lineage family.
- **Vendor:** Cline (company), founded 2024 by Saoud Rizwan. ~$32M Series A (Emergence Capital).
- **License:** **Apache-2.0.**
- **Tech / architecture:** VS Code extension (~95% TypeScript). 2026: open-sourced the **Cline SDK**
  (Apache-2.0 agent runtime) + a **headless CLI** for CI/CD.
- **Model support:** **BYO key, 30+ providers** + a first-party "Cline Provider": Anthropic, OpenAI,
  Gemini, Bedrock, Azure, OpenRouter, DeepSeek, Qwen, xAI, Mistral, **local (Ollama, LM Studio)**, any
  OpenAI-compatible endpoint.
- **Features:** Agentic loop with **Plan/Act dual modes**, multi-file edits, **MCP + MCP
  Marketplace**, checkpoints, terminal exec, custom modes via `.clinerules/`, browser automation.
- **Safety / permissions:** Per-step approval / auto-approve.
- **Extensibility:** MCP + Marketplace + custom rules; an SDK and headless CLI.
- **Status / differentiator:** Origin of the whole fork family; biggest by stars/installs; the SDK +
  headless CLI reposition it as embeddable infrastructure.

## Roo Code — Roo Code, Inc. (**discontinued**)

- **Popularity (frozen):** `RooCodeInc/Roo-Code` ~**24.3K** stars, **`archived: true`** (confirmed via
  API); VS Code ~1.77M installs (residual).
- **Vendor:** Roo Code, Inc.; pivoted to **"Roomote"** (a Slack-first cloud agent).
- **License:** **Apache-2.0.**
- **Tech / architecture:** VS Code extension (TypeScript), forked from Cline.
- **Model support:** Broad provider list inherited/extended from Cline; OpenRouter; local models.
- **Features (its historical contribution):** Pioneered **custom modes/personas** (Architect, Code,
  Debug, Ask, Orchestrator) — the direct ancestor of Kilo's mode system.
- **Safety / permissions:** Auto-approve / per-step.
- **Extensibility:** MCP; modes.
- **Status / differentiator:** ⚠️ **Discontinued** (sunset Apr 2026, repo archived ~May 2026). The team
  concluded the IDE-extension category was the wrong bet; Cline credited Roo as the most impactful
  fork. Numbers are frozen.

## Kilo Code — Kilo

- **Popularity:** `Kilo-Org/kilocode` ~**24.5K** stars (now narrowly ahead of frozen Roo); VS Code
  ~1.25M / Open VSX ~2.76M installs.
- **Vendor:** Kilo (SF); ~$8M seed (~Dec 2025), co-founded by Scott Breitenother and **Sid Sijbrandij
  (GitLab co-founder)**. Claims 6.1T+ tokens/month.
- **License:** **MIT** (the only one of the Cline trio not Apache-2.0).
- **Tech / architecture:** VS Code extension (TypeScript + substantial **Kotlin** for **JetBrains**
  support, in RC); also a **CLI**; expanding to mobile/Slack.
- **Model support:** **500+ models** at zero markup, 30+ providers; first-party **Kilo Gateway**; **$20
  free credits** on signup (most generous on-ramp); local (Ollama/LM Studio).
- **Features:** Agentic loop; modes inherited from Roo (agents auto-spawn subagents); MCP +
  Marketplace; **checkpoints ("snapshots")**; signature **Memory Bank** (persistent architectural
  context); built-in code review. *(Codebase indexing reportedly being rebuilt in 2026 — flag.)*
- **Safety / permissions:** Approval modes.
- **Extensibility:** MCP + Marketplace + Memory Bank + multi-IDE.
- **Status / differentiator:** "Best of both" — Cline's core + Roo's modes + Memory Bank, multi-IDE,
  zero-markup gateway. The most actively developed Roo-lineage tool.

## Continue — Continue Dev

- **Popularity:** `continuedev/continue` ~**34.4K** stars; VS Code ~3.45M installs.
- **Vendor:** Continue Dev, Inc. (YC; founded 2023). Funding ~$5.1M (a claimed "$65M Series A" is
  **unverified** — flag). **Reportedly acquired by Cursor June 2026** — repo went read-only after a
  final v2.0.0.
- **License:** **Apache-2.0.**
- **Tech / architecture:** VS Code + JetBrains extensions + a **CLI (`cn`)**. TypeScript.
  Config-as-code (`config.yaml`) + **Continue Hub** ("Docker Hub for AI assistants" — shareable
  modular blocks).
- **Model support:** Fully **BYO / model-agnostic**; strongest local story (**Ollama** auto-detect).
- **Features:** Autocomplete, chat, edit (multi-file), **agent mode**; **MCP** (first-class blocks);
  codebase indexing/RAG; context providers (Jira, Confluence, docs); terminal exec.
- **Safety / permissions:** Approval modes.
- **Extensibility:** MCP + the Hub sharing model + config-as-code.
- **Status / differentiator:** The most credible *fully open-source, model-agnostic* assistant with
  the best local/offline story. ⚠️ **Development effectively stopped post-acquisition** (read-only repo).

## GitHub Copilot — GitHub / Microsoft

- **Popularity:** `GitHub.copilot` ~**73.9M** installs; `GitHub.copilot-chat` ~**75.9M** — among the
  most-installed VS Code extensions ever. ~**4.7M paid subscribers** (Jan 2026); ~90% of Fortune 100.
- **Vendor:** GitHub / Microsoft.
- **License:** **Proprietary** (Free / Pro / Pro+ / Business / Enterprise).
- **Tech / architecture:** VS Code, Visual Studio, JetBrains, Eclipse, Xcode, github.com, CLI. **MCP**
  GA across several clients.
- **Model support:** Multi-provider picker — Claude (Sonnet/Opus 4.x), OpenAI (GPT-5/4.x/o-series),
  Gemini 2.5. **BYO-key** for Free/Pro/Pro+.
- **Features:** **Agent mode** (in-IDE autonomous multi-step: plans, multi-file edits, runs
  terminal/tests, self-corrects, iterates). **Copilot coding agent** — a *separate autonomous cloud
  agent on github.com* that opens a PR end-to-end in ephemeral Actions environments. MCP (GitHub +
  Playwright on by default); custom instructions; inline autocomplete + chat.
- **Safety / permissions:** Org/enterprise policy controls; approval in agent mode.
- **Extensibility:** MCP; custom instructions; the whole GitHub ecosystem.
- **Status / differentiator:** Deepest enterprise/multi-IDE reach; by far the largest install base; a
  unique **two-tier model** — a local IDE agent + an autonomous cloud PR agent fused with GitHub.

## Augment Code — Augment Computing

- **Popularity:** VS Code ~765K installs; JetBrains ~675K. No public core repo.
- **Vendor:** Augment Computing (Palo Alto, 2022). **$227M Series B @ ~$977M** post-money (Apr 2024);
  backers incl. Eric Schmidt's Innovation Endeavors, Lightspeed, Sutter Hill.
- **License:** **Proprietary**, enterprise-oriented.
- **Tech / architecture:** VS Code extension + JetBrains plugin + **Auggie CLI** (see
  [profiles-cli.md](profiles-cli.md)).
- **Model support:** Multi-model hosted (model choice abstracted), not primarily BYO.
- **Features:** Signature **Context Engine** — large-context codebase RAG (markets "400,000+ files");
  agent mode; Smart Apply; **Context Engine MCP**.
- **Safety / permissions:** Enterprise gating.
- **Extensibility:** MCP + ACP.
- **Status / differentiator:** Best-in-class large/legacy-codebase context. ⚠️ **Sunsetting IDE
  completions (Mar 2026) and moving upmarket to "Cosmos," a cloud multi-agent platform** (public
  preview May 2026).

## Junie — JetBrains

- **Popularity:** JetBrains Marketplace ~**26.4M** downloads (auto-update-inflated). Topped SWE-Rebench
  at GA (~61.6% resolved).
- **Vendor:** JetBrains.
- **License:** **Proprietary.**
- **Tech / architecture:** **JetBrains-IDE-native** (not a VS Code extension) — built into
  IntelliJ/PyCharm/WebStorm/GoLand/etc. 2026: added a **Junie CLI** (any terminal/CI). Left beta
  June 2026.
- **Model support:** **Model-agnostic by design — Junie picks the model per task** (Anthropic/OpenAI/
  Google frontier models); BYO key + local runtimes (Ollama, LM Studio, LiteLLM).
- **Features:** Agentic loop (plans, multi-file edits, runs tests/terminal, fixes failures),
  autonomous debugging, plan mode, async tasks, PR reviews; deep IDE-native code intelligence.
- **Safety / permissions:** **Default** (asks before bash/file ops) vs **Brave Mode** (no prompting).
- **Extensibility:** IDE-native; CLI for CI.
- **Status / differentiator:** The tightest native IDE integration of any tool here; per-task model
  selection. Pricing is credit-per-task (a common complaint).

## JetBrains AI Assistant — JetBrains

- **Popularity:** JetBrains Marketplace ~**166.9M** downloads (auto-update-inflated; freemium).
- **Vendor:** JetBrains.
- **License:** **Proprietary plugin — but its Mellum completion model is open-sourced** (open weights
  on Hugging Face).
- **Tech / architecture:** JetBrains plugin across all IntelliJ-platform IDEs; cloud + optional
  local/offline.
- **Model support (broadest here):** Anthropic/OpenAI/Google cloud; **BYOK**; **local/offline**
  (Ollama, LM Studio, any OpenAI-compatible server; completion via Qwen2.5-Coder, DeepSeek-Coder,
  Codestral, or Mellum).
- **Features:** Completion + **Next Edit Suggestions**; AI Chat with an integrated **Claude Agent**
  (built on Anthropic's Agent SDK, Sept 2025); **MCP** (beta); RAG codebase context.
- **Safety / permissions:** IDE-managed.
- **Extensibility:** MCP (beta).
- **Status / differentiator:** The most provider-flexible tool in this file; open-sourcing Mellum is
  unusual. Functional split: AI Assistant = completion/chat/light agent; **Junie** = heavyweight
  autonomous agent.

## Amazon Q Developer — AWS

- **Popularity:** VS Code (`AmazonWebServices.amazon-q-vscode`) ~1.77M installs; extension repo
  `aws/amazon-q-vscode` Apache-2.0 (~29 stars).
- **Vendor:** AWS / Amazon (evolved from CodeWhisperer).
- **License:** Service **proprietary**; VS Code extension **Apache-2.0**; models via Bedrock.
- **Tech / architecture:** Plugins for VS Code, JetBrains, Visual Studio, Eclipse + a **CLI** (see
  [profiles-cli.md](profiles-cli.md)) + Console/Slack/Teams.
- **Model support:** **Amazon Bedrock**, prominently Claude. Managed by AWS, not BYO.
- **Features:** Inline suggestions; **agentic IDE experience** (reads/writes files, runs shell,
  multi-step); automated code reviews; unit-test gen; security scanning; native tools + MCP; `/dev`
  and `/transform` agents (Java upgrades, .NET porting).
- **Safety / permissions:** AWS-managed.
- **Extensibility:** MCP.
- **Status / differentiator:** ⚠️ **End-of-support announced** — new signups blocked ~May 15 2026; IDE
  plugin EOS ~Apr 30 2027; users steered to **Kiro** (AWS's spec-driven agentic IDE). First-party Q
  experiences continue.

## Zed AI — Zed Industries

- **Popularity:** `zed-industries/zed` ~**85.9K** stars (one of the most-starred Rust projects). Hit
  1.0 in April 2026.
- **Vendor:** Zed Industries (Atom/Tree-sitter alumni); ~$32M from Sequoia.
- **License:** Editor **GPL**; server **AGPL**; GPUI framework **Apache-2.0** (genuinely OSS copyleft).
- **Tech / architecture:** **From-scratch native editor in Rust** (NOT a VS Code fork),
  GPU-accelerated via its own GPUI framework.
- **Model support:** BYO key (Anthropic, OpenAI, OpenAI-compatible) + Zed-hosted + OpenRouter; **strong
  local support** (Ollama, LM Studio, llama.cpp).
- **Features:** **Agent Panel** with live tool-call visibility; **Agentic Editing** with per-hunk
  accept/reject; **parallel/multi-threaded agents** across worktrees; inline NL editing; **Terminal
  Threads** (run Claude Code or Amp as a sidebar agent, ~May 2026); **MCP** context servers; edit
  prediction via Zed's "Zeta" model.
- **Safety / permissions:** Per-hunk change review; approval.
- **Extensibility:** MCP; can host external agents (Claude Code, Amp) inside the editor.
- **Status / differentiator:** Native Rust performance; parallel agents; uniquely **hosts other
  agents** inside the editor; fully open-source.

## Tabby — TabbyML

- **Popularity:** `TabbyML/tabby` ~**33.6K** stars; 130+ contributors.
- **Vendor:** TabbyML, Inc. (venture-backed); positions as the leading self-hosted Copilot alternative.
- **License:** Predominantly **Apache-2.0** (some enterprise features source-available/commercial —
  verify per feature). **Rust.**
- **Tech / architecture:** **Self-contained server in Rust** — no external DB/cloud needed; runs on
  local hardware (CUDA / Apple Metal). IDE plugins (VS Code, JetBrains, Vim/Neovim) connect to it.
- **Model support:** Self-hosted/local open models (StarCoder, CodeLlama, Qwen-Coder, DeepSeek-Coder);
  can also call external APIs.
- **Features:** Code completion (original focus), **Answer Engine** (in-IDE Q&A), inline chat,
  **Context Providers** (RAG/codebase indexing). More completion/chat than full autonomous agent.
- **Safety / permissions:** Self-hosted; data stays on your infra.
- **Extensibility:** Context Providers; MCP limited/emerging.
- **Status / differentiator:** Fully **self-hosted, no cloud/DB**; strong fit for
  privacy-conscious/regulated/air-gapped teams; runs on modest consumer GPUs.

## Refact — Refact.ai

- **Popularity:** `smallcloudai/refact` ~**3.6K** stars but **`archived: true`** (confirmed via API);
  active dev moved to a maintainer fork. Refact Cloud retired.
- **Vendor:** Small Magellanic Cloud AI; now driven through the maintainer fork.
- **License:** **BSD-3-Clause.** Rust core.
- **Tech / architecture:** **Local-first daemon** (state under `.refact/`, no mandatory cloud); thin VS
  Code/JetBrains clients; self-hostable inference.
- **Model support:** 20+ provider families (Anthropic, OpenAI, Groq, DeepSeek, Gemini) **+ local
  (Ollama, LM Studio)**; BYO or self-host.
- **Features:** Full **autonomous agent** — file editing, shell/tool exec, **browser automation**,
  **MCP**, worktree-isolated multi-agent task "cards"; unlimited-context autocomplete + chat; RAG.
- **Safety / permissions:** Local-first; self-hostable.
- **Extensibility:** MCP + agent delegation.
- **Status / differentiator:** Among the most agentic of self-hostable OSS tools; BSD-licensed,
  local-first. ⚠️ Recent maintainership/repo transition (archived upstream + retired cloud) — verify
  the active repo before adopting.

## Trae — ByteDance

- **Popularity:** AI-native **VS Code fork** (Jan 2025); IDE is **proprietary/closed**. Only
  `bytedance/trae-agent` (a research harness CLI, MIT) is public, ~**11.7K** stars.
- **Vendor:** ByteDance.
- **License:** **Proprietary** IDE (the agent CLI is MIT).
- **Tech / architecture:** VS Code fork; completion + chat + **SOLO Mode** (autonomous full-stack
  scaffolding — a standalone app since ~Mar 2026).
- **Model support:** GPT-4o, Claude Sonnet 4, Gemini 2.5 Pro, DeepSeek R1 (no API keys; free tier).
- **Features:** Completion, chat, SOLO autonomous mode.
- **Safety / permissions:** Hosted.
- **Extensibility:** Limited.
- **Status / differentiator:** A major non-Western entrant with an aggressive free tier. ⚠️ **Privacy
  flag:** 2025 researchers reported extensive telemetry even when disabled, with multi-year retention
  — not suitable for proprietary/regulated code.

## Void — Void (**paused**)

- **Popularity:** `voideditor/void` ~**28.8K** stars but **`archived: true`** (confirmed via API).
- **Vendor:** Void (YC-backed), positioned as an open-source Cursor alternative.
- **License:** **Apache-2.0.** TypeScript **VS Code fork.**
- **Tech / architecture:** VS Code fork with built-in agent + BYO models.
- **Model support:** Fully BYO (incl. local).
- **Features:** Agentic editing, autocomplete, chat — an open Cursor analog.
- **Safety / permissions:** Local/BYO.
- **Extensibility:** VS Code ecosystem.
- **Status / differentiator:** ⚠️ **Development paused / repo archived** (early 2026); features still
  work but no new releases.

## Tabnine — Tabnine

- **Popularity:** Proprietary multi-IDE plugin; the public `codota/TabNine` (~**10.8K** stars) is a
  legacy client only.
- **Vendor:** Tabnine.
- **License:** **Proprietary.**
- **Tech / architecture:** Multi-IDE plugin; SaaS / VPC / on-prem / **air-gapped** deployment; local
  models.
- **Model support:** Own + third-party; deployment flexibility is the differentiator.
- **Features:** Completion, chat; 2026 agentic push — AI agents, **MCP**, Jira, a "Context Engine."
- **Safety / permissions:** Strong compliance / privacy posture (its main selling point).
- **Extensibility:** MCP; enterprise integrations.
- **Status / differentiator:** Privacy-first enterprise incumbent; differentiator is **deployment
  flexibility and compliance**, not raw capability.

## Qodo (formerly CodiumAI) — Qodo

- **Popularity:** Mix of OSS + proprietary. Open: **PR-Agent** (`qodo-ai/pr-agent`, now redirecting to
  `The-PR-Agent/pr-agent`, ~**11.8K** stars) and qodo-cover.
- **Vendor:** Qodo.
- **License:** **Mixed** (PR-Agent Apache-2.0; products proprietary).
- **Tech / architecture:** Qodo Gen (IDE plugin), Qodo Merge (PR review), Qodo Command (CLI). MCP-aware.
- **Model support:** BYO.
- **Features:** Strength is **test generation + code review** rather than a general editor.
- **Safety / permissions:** Standard.
- **Extensibility:** MCP.
- **Status / differentiator:** Quality/test/review-focused rather than a general coding agent.

## Cody — Sourcegraph (**individual product wound down**)

- **Popularity:** Was a major IDE assistant; the public OSS repo was pulled.
- **Vendor:** Sourcegraph.
- **License:** Was Apache-2.0 (client); now enterprise-only.
- **Tech / architecture:** Multi-IDE assistant built on Sourcegraph's code graph for context.
- **Model support:** Multi-model.
- **Features:** Chat, completion, code-graph-grounded context.
- **Safety / permissions:** Enterprise.
- **Extensibility:** —
- **Status / differentiator:** ⚠️ **Cody Free/Pro discontinued (Jul 2025)**; Sourcegraph pushed users
  to **Amp** (see [profiles-cli.md](profiles-cli.md)); **Cody Enterprise still supported**. Listed for
  completeness — not a current individual option.

---

### Discontinued / acquired, for the record

- **Supermaven** — acquired by Anysphere (Cursor) Nov 2024; standalone product wound down late 2025.
  Dead as a standalone.
- **PearAI** — a minor Continue-based VS Code fork; repo moved/stale.
- **avante.nvim** (`yetone/avante.nvim`, ~**18.0K** stars, Apache-2.0, Lua) — "use Neovim like Cursor";
  brings agentic editing into Neovim. The notable *editor-integrated* entry outside VS Code/JetBrains.
