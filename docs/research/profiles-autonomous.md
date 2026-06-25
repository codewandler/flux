# Profiles — autonomous / cloud SWE agents, app builders & benchmarks

Full profiles for **autonomous "AI software engineer"** agents (issue → patch/PR, often in a
sandboxed VM), the **app-builder / "vibe-coding"** tools, and the **benchmarks** the field is ranked
on. See [landscape.md](landscape.md) for methodology, the category schema, the master matrices, and
the caveats block. Star counts are GitHub API values observed **~2026-06-25**.

> **Benchmark caveat up front:** every SWE-bench / Terminal-Bench number is a property of an
> *agent + a specific model + a scaffold*, not of the agent alone — the same agent swings tens of
> points with the backing model. And **SWE-bench Verified is saturating** (audits found flawed tasks;
> the very-high 88–95% figures are contested). Treat all scores as directional.

Categories: **popularity/funding · vendor · license · tech/architecture · model support · features ·
autonomy & safety · benchmark · status/differentiator.**

---

## OpenHands — All Hands AI (formerly OpenDevin)

- **Popularity:** `All-Hands-AI/OpenHands` (now `OpenHands/OpenHands`) ~**78.3K** stars — one of the
  most-starred AI repos on GitHub.
- **Vendor:** **All Hands AI**; raised an **$18.8M Series A (Nov 2025)**. Originated as community
  "OpenDevin."
- **License:** **MIT.** Python core.
- **Tech / architecture:** **Event-stream / pub-sub** design: a stateless Agent emits Actions → a
  Workspace (local process or **Docker container**) executes → returns Observations; LLM wrapped by
  LiteLLM. Default **CodeActAgent** runs bash + Python in a sandboxed Linux container. A composable
  **Software Agent SDK** was published (late 2025).
- **Model support:** Highly **model-agnostic / BYO** via LiteLLM.
- **Features:** High autonomy; Docker sandboxing; repo-wide edits; test execution; **multimodal
  browser use**; git/PR creation; planning; self-verification. Hosted "All Hands Online/Cloud" too.
- **Autonomy & safety:** Container-sandboxed by default — a real isolation story (unlike most local
  CLI agents).
- **Benchmark (SWE-bench Verified):** ~53% with standard models; **up to ~72% with Claude Sonnet 4.5 +
  extended thinking** — among the strongest *open-source* results.
- **Status / differentiator:** The leading open-source autonomous SWE agent; strong research lineage
  (ICLR 2025); generalist design (code + browser + shell). **The closest OSS analog to flux's
  ambition — but it leads with capability, not a non-bypassable safety envelope.**

## SWE-agent (+ mini-SWE-agent) — Princeton / Stanford

- **Popularity:** `SWE-agent/SWE-agent` ~**19.6K** stars; `SWE-agent/mini-swe-agent` ~**5.4K**.
- **Vendor:** Academic (Princeton + Stanford) — the group behind SWE-bench.
- **License:** **MIT.** Python.
- **Tech / architecture:** Single-agent loop around a custom **Agent-Computer Interface (ACI)** — the
  research insight that *agent-friendly tools matter as much as the model*. Runs in Docker.
  **mini-swe-agent** is a ~100-line reimplementation with no big configs.
- **Model support:** **Model-agnostic / BYO.**
- **Features:** GitHub issue → patch; repo navigation, file editing, test execution. Also adapted for
  offensive cybersecurity (EnIGMA) and competitive coding. More research scaffold than daily driver.
- **Autonomy & safety:** Docker-isolated.
- **Benchmark:** SWE-agent 1.0 + Claude 3.7 was **SoTA on SWE-bench Verified (Feb 2025)**;
  **mini-swe-agent scores ~65–74%** depending on model — remarkable for ~100 lines.
- **Status / differentiator:** The canonical reference scaffold; the ACI concept influenced the whole
  field. mini-swe-agent is a powerful argument that *scaffold simplicity + a strong model* is
  competitive — relevant to flux's "small, well-understood surface" principle.

## Devin — Cognition AI

- **Popularity (closed):** Metric is funding/valuation — **raised >$1B at ~$25B pre / ~$26B post
  (May 2026)**, up from ~$10.2B (Sept 2025). ARR ~$1M (Sept 2024) → ~$73M (Jun 2025), then jumped
  after acquiring **Windsurf**.
- **Vendor:** Cognition AI.
- **License:** **Proprietary** (closed SaaS).
- **Tech / architecture:** Cloud **sandboxed VM** running an Agent-Computer Interface — code editor +
  terminal + **sandboxed browser** + planning. **Devin 2.0** (Apr 2025) is an "agent-native cloud
  IDE." VPC option via AWS PrivateLink.
- **Model support:** **Locked / proprietary** Cognition stack; no BYO.
- **Features:** Multiple parallel agents, editable/approvable plans, runs tests, **submits PRs**,
  browser use, self-maintained wiki. Tickets via Slack/Linear/Jira/GitHub/MCP. Billed in **ACUs**.
- **Autonomy & safety:** High autonomy, human-in-the-loop via PR review.
- **Benchmark:** Launch (Mar 2024) was **13.86% end-to-end on SWE-bench** — a breakthrough at the time
  (prior baselines ~2%); now badly dated. *No current official SWE-bench Verified % verified — flag.*
- **Status / differentiator:** The agent that *defined* the autonomous-AI-software-engineer category
  and the most valuable company in the space; acquired Windsurf for an IDE surface.

## MetaGPT — FoundationAgents

- **Popularity:** `FoundationAgents/MetaGPT` (was `geekan/MetaGPT`) ~**69.0K** stars.
- **Vendor:** Open-source / research (FoundationAgents).
- **License:** **MIT.** Python.
- **Tech / architecture:** A **multi-agent "AI software company"** framework — coordinated roles
  (PM / architect / engineer / QA) following SOPs to build software from a one-line requirement.
- **Model support:** Multi-provider / BYO.
- **Features:** Role-based multi-agent collaboration; produces PRDs, designs, code.
- **Autonomy & safety:** Generates artifacts for human review.
- **Benchmark:** Known for the "software company" framing, not a SWE-bench headliner.
- **Status / differentiator:** The most-starred *multi-agent-framework* take on coding; influential
  conceptually (role decomposition).

## GPT-Pilot — Pythagora

- **Popularity:** `Pythagora-io/gpt-pilot` ~**33.7K** stars (active).
- **Vendor:** Pythagora-io (underpins the commercial Pythagora product / VS Code extension).
- **License:** **NOASSERTION** (non-standard — verify before commercial use; flag). Python.
- **Tech / architecture:** **Multi-agent / multi-role** — coordinated personas (PM, architect, dev,
  reviewer) build an app step-by-step with developer oversight; debugs as it goes.
- **Model support:** OpenAI/GPT-4-class (BYO).
- **Features:** Builds apps incrementally with human checkpoints; in-loop debugging.
- **Autonomy & safety:** Human checkpoints throughout.
- **Benchmark:** Not a SWE-bench headliner.
- **Status / differentiator:** Notable early multi-agent dev framework; now mostly the open core of the
  commercial Pythagora.

## GPT-Engineer — Anton Osika (**archived**)

- **Popularity:** `AntonOsika/gpt-engineer` ~**55.2K** stars but **`archived: true`** (confirmed via
  API; last push May 2025).
- **Vendor:** Anton Osika; evolved into the commercial **Lovable.dev** (the README calls it a
  precursor).
- **License:** **MIT** (archived). Python.
- **Tech / architecture:** CLI codegen — generate a whole app from a prompt, then iterate.
- **Model support:** OpenAI/GPT (BYO).
- **Features:** One-shot/iterative project generation; lower agentic sophistication than current SWE
  agents.
- **Status / differentiator:** Historically huge (viral in 2023); now superseded by Lovable. Mainly of
  historical importance.

## Devika — stitionai

- **Popularity:** `stitionai/devika` ~**19.5K** stars.
- **Vendor:** Community OSS.
- **License:** **MIT.** Python.
- **Tech / architecture:** The first OSS "agentic software engineer," an early Devin alternative;
  plans, researches the web, writes code.
- **Model support:** Multi-provider / BYO.
- **Features:** Plan → research → code loop.
- **Status / differentiator:** A landmark early "open Devin," now largely **stale** (last push 2025-09).

## Devon — entropy-research

- **Popularity:** `entropy-research/Devon` ~**3.4K** stars.
- **Vendor:** Community OSS.
- **License:** **AGPL-3.0.** Python.
- **Tech / architecture:** OSS pair-programmer / Devin alternative.
- **Model support:** Multi-provider / BYO.
- **Features:** Multi-file editing, agentic loop.
- **Status / differentiator:** Another early open Devin alternative; modest traction.

## Aide — CodeStory (**archived**)

- **Popularity:** `codestoryai/aide` ~**2.2K** stars but **`archived: true`** (confirmed via API).
- **Vendor:** CodeStory.
- **License:** **AGPL-3.0.** TypeScript **VS Code fork.**
- **Tech / architecture:** "Open-source AI-native IDE" (a VS Code fork).
- **Model support:** Multi-provider.
- **Features:** Agentic editing in-editor.
- **Status / differentiator:** ⚠️ Archived — CodeStory pivoted to a hosted agent ("Sidecar/SOTA").

## Suna — Kortix

- **Popularity:** `kortix-ai/suna` ~**19.9K** stars.
- **Vendor:** Kortix.
- **License:** **NOASSERTION** (verify; flag). TypeScript.
- **Tech / architecture:** A generalist agent platform ("company AI command center") with strong dev
  use, not coding-only.
- **Model support:** Multi-provider.
- **Features:** Browsing, file/data tasks, tool use, dev tasks.
- **Status / differentiator:** A broad agent platform that overlaps the coding-agent space.

## Agent Zero — agent0ai

- **Popularity:** `agent0ai/agent-zero` (was `frdel/agent-zero`) ~**18.2K** stars.
- **Vendor:** Community OSS.
- **License:** **NOASSERTION** (verify; flag). Python.
- **Tech / architecture:** A general-purpose, **self-extending** agent framework popular for coding
  tasks — writes its own tools as it goes.
- **Model support:** Multi-provider / BYO.
- **Features:** Hierarchical subordinate agents, tool creation, persistent memory.
- **Status / differentiator:** A "build-your-own-agent" framework rather than a packaged coding tool.

## smol developer — smol-ai

- **Popularity:** `smol-ai/developer` ~**12.2K** stars.
- **Vendor:** smol-ai.
- **License:** **MIT.** Python.
- **Tech / architecture:** An early "embed a dev agent in your app" library — "your own personal junior
  developer."
- **Model support:** OpenAI/GPT.
- **Features:** Whole-codebase scaffolding from a prompt.
- **Status / differentiator:** Historical (inactive since 2024); influential in the "library, not app"
  framing.

## Factory / Droid — Factory.ai

- **Popularity (closed):** `Factory-AI/factory` (docs/marketing) ~**1.0K** stars. **$50M Series B
  (Sep 2025).**
- **Vendor:** Factory.ai.
- **License:** **Proprietary.**
- **Tech / architecture:** Agent-native across **CLI, web, Slack/Teams, Linear/Jira, mobile**; agents
  ("Droids") pick up tickets and ship.
- **Model support:** Multi-model.
- **Features:** End-to-end ticket → PR; multi-surface; long-running agents.
- **Benchmark:** Reported **#1 on Terminal-Bench at ~58.75%** at one point, ahead of Claude Code/Codex
  (vendor-reported; the leaderboard has since moved — see below).
- **Status / differentiator:** A well-funded "agent-native dev org" play; strong benchmark marketing.

## Cosine / Genie — Cosine

- **Popularity (closed):** No meaningful public repo.
- **Vendor:** Cosine.
- **License:** **Proprietary.**
- **Tech / architecture:** Autonomous SWE powered by a proprietary "Genie" model trained on human
  engineer **reasoning traces**; picks up GitHub/Jira/Linear tickets and opens PRs.
- **Model support:** Proprietary Genie model.
- **Features:** Ticket → PR autonomy.
- **Benchmark:** Has claimed strong SWE-bench Verified numbers (vendor-reported — flag).
- **Status / differentiator:** Distinctive training approach (human reasoning traces).

## OpenAI Codex (cloud) — OpenAI

- **Popularity (closed platform):** Part of the Codex product (the CLI is in
  [profiles-cli.md](profiles-cli.md)).
- **Vendor:** OpenAI. **Proprietary**, OpenAI-locked.
- **Tech / architecture:** Cloud preview (May 2025) ran on **codex-1** (an o3 variant RL-trained on
  real SWE tasks, trained to run tests until they pass); later **GPT-5-Codex / GPT-5.5-Codex**. Tasks
  run **in parallel, each in its own cloud sandbox**; a GA **subagent/manager model** coordinates
  workers; **background computer-use**. A desktop **Codex app** (Feb 2026) orchestrates agents. One
  reported run: 25 hours, ~13M tokens, ~30k LOC.
- **Features:** Cloud-VM sandbox, parallel/background autonomy, test-until-pass, git/PR, multi-agent
  orchestration, computer use.
- **Benchmark:** **Terminal-Bench 2.1 (Jun 2026): Codex CLI + GPT-5.5 is #1 at ~83.4%** (see below).
- **Status / differentiator:** The autonomous, parallel, cloud counterpart to the Codex CLI.

## Cursor background agents — Anysphere

- **Popularity (closed):** Part of Cursor (see [profiles-ide.md](profiles-ide.md)).
- **Vendor:** Anysphere. **Proprietary.**
- **Tech / architecture:** Long-running autonomous agents working in **cloud environments** while you
  keep coding; spawn-and-review. Cursor's published long-horizon-autonomy work reportedly *inspired
  OpenAI Codex*.
- **Status / differentiator:** IDE-anchored autonomous agents rather than a standalone CLI — a boundary
  case included for completeness.

---

## App builders / "vibe coding" (adjacent category)

These generate and host whole apps from natural language. Adjacent to flux (different audience), but
part of the broader "coding agent" landscape and where a lot of the *revenue* is.

- **Replit Agent (Agent 3)** — Replit. Proprietary. **$400M raise, ~$9B valuation (2026, tripled in
  6 months).** Full build-deploy-host loop in-browser.
- **Lovable** — Lovable. Proprietary. **~$6.6B valuation, ~$400M ARR (Feb 2026)** — the revenue leader
  among app builders; descends from GPT-Engineer.
- **v0** — Vercel. Proprietary. **4M+ users; Vercel raised $300M @ ~$9.3B (2026).** UI/app generation.
- **Bolt.new** — StackBlitz. The OSS core `stackblitz/bolt.new` ~**16.4K** stars (MIT); the commercial
  product runs full-stack generation in-browser via WebContainers.
- **LlamaCoder** — `Nutlope/llamacoder` ~**7.0K** stars (MIT) — an OSS "Claude Artifacts"-style app
  builder (Together AI).
- **Melty** — `meltylabs/melty` ~**5.4K** stars (MIT) — a "chat-first code editor"; now largely
  dormant (team pivoted).

---

## Benchmarks

How the field is ranked. flux currently appears on **none** of these — see the opportunity note in
[landscape.md](landscape.md).

### SWE-bench (full / Verified / Lite / Pro)
- **What:** Real GitHub issues + repos; the agent must produce a patch that resolves the issue and
  passes hidden tests. `SWE-bench/SWE-bench` ~**5.2K** stars; from the Princeton/Stanford group.
- **Verified:** A 500-task human-curated subset, the most-cited variant. ⚠️ **Saturating** — 2026
  audits found flawed/underspecified tasks; at high scores the signal degrades. OpenAI reportedly
  stopped publishing Verified and points to **SWE-bench Pro**.
- **Lite:** Smaller/cheaper subset for fast iteration.
- **Pro:** The harder successor positioned as more reliable post-saturation; 2026 leaders reported in
  the ~69% range (e.g. a tracker citing Claude Opus leading "active" at ~69%).
- **Who leads (mid-2026):** Leaderboards report **model+scaffold** combos in the high-80s–90s% on
  Verified — *contested / saturating, treat skeptically.* Among open agents the credible data points
  are **OpenHands ~72%** (Sonnet 4.5 + extended thinking) and **mini-swe-agent ~65–74%**. Devin's
  historic **13.86%** is the category's origin marker, not a current figure.

### Aider polyglot leaderboard
- **What:** Aider's own benchmark — 225 hard Exercism exercises across C++/Go/Java/JS/Python/Rust;
  measures **code-editing accuracy + correct edit formatting**, not autonomous issue resolution. An
  industry reference for "can the model edit code reliably."
- **Top (~Jun 2026):** **GPT-5 ~88.0%**, Gemini 2.5 Pro ~82%, o3 ~81%.

### Terminal-Bench
- **What:** Agents performing complex end-to-end **terminal** tasks (tbench.ai;
  `harbor-framework/terminal-bench`). The most relevant benchmark for a CLI agent like flux.
- **Top (Terminal-Bench 2.1, Jun 2026):** **Codex CLI + GPT-5.5 #1 at ~83.4%**, **Claude Code + Opus
  4.8 #2 at ~78.9%**, Gemini CLI + Gemini 3.1 Pro ~70.7%. (Earlier, Factory/Droid had claimed #1 on an
  earlier version — leaderboards move fast.) A harder "Terminal-Bench Hard" variant also exists.

### SWE-Rebench
- A continuously-refreshed SWE benchmark; **Junie** topped it at GA (~61.6% resolved). Used to counter
  SWE-bench contamination/saturation concerns.
