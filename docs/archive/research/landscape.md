# The coding-agent landscape — and where flux fits

A survey of the field of "coding harnesses" / AI coding agents, collected so flux can position
itself deliberately. **Part 1** is a neutral landscape (category schema + master comparison matrices,
with full per-tool profiles in the linked cluster files). **Part 2** is the synthesis: *what this
means for flux.*

- [profiles-cli.md](profiles-cli.md) — CLI / terminal-first agents (the cluster closest to flux)
- [profiles-ide.md](profiles-ide.md) — IDE-integrated agents + completion platforms
- [profiles-autonomous.md](profiles-autonomous.md) — autonomous / cloud SWE agents, app builders, benchmarks

---

## Methodology & observation date

- Data gathered via web search + the **GitHub API**, observed **~2026-06-25**. Star counts were
  re-grounded against the *authenticated* GitHub API (`gh api`), so the figures here are the API's
  `stargazers_count` at that date — not scraped from articles.
- "Stars" is a coarse popularity proxy (it rewards age, virality, and being open-source). For closed
  products the proxy is **funding / valuation / install counts** instead — not directly comparable.
- Every number is a **point-in-time snapshot** and drifts daily. Re-pull before citing externally.

### Caveats block (read before trusting any single number)

- **Forward-dated 2026 figures.** Funding rounds, acquisitions, and lifecycle events through mid-2026
  are included; many rest on secondary/SEO sources. The strongest anchors are TechCrunch, official
  vendor blogs/changelogs, and the live GitHub API.
- **Contested / uncertain items, flagged inline where they appear:**
  - A reported **SpaceX/xAI acquisition of Cursor (~$60B)** — *uncertain*, not treated as fact.
  - A reported **Cursor acquisition of Continue (2026)** — better corroborated but still secondary.
  - **SWE-bench Verified saturation** — the very-high 88–95% numbers are contested; prefer
    *agent+model* pairs and SWE-bench Pro.
- **Surprising-but-verified star counts.** Some counts look implausibly high; they were re-checked
  against the authenticated API and are real as of the observation date — notably
  `NousResearch/hermes-agent` **~202K**, `sst/opencode` (= `anomalyco/opencode`) **~178K**, and
  `earendil-works/pi` **~65K**.
- **Renamed/redirected repos.** Several canonical slugs changed; use the current one when citing:
  `badlogic/pi-mono → earendil-works/pi`, `sst/opencode → anomalyco/opencode`,
  `block/goose → aaif-goose/goose`, `All-Hands-AI/OpenHands → OpenHands/OpenHands`,
  `geekan/MetaGPT → FoundationAgents/MetaGPT`, `antinomyhq/forge → tailcallhq/forgecode`,
  `paul-gauthier/aider → Aider-AI/aider`.
- **Benchmarks belong to model+scaffold combos,** not to the agent alone.

### The two names the user asked to pin down

- **"pi" → `earendil-works/pi`** (~65K stars) — Mario Zechner's deliberately *minimal* terminal
  harness (four tools, sub-1k-token prompt), now stewarded by Earendil. Possible alternative if a
  different "pi" was meant: `can1357/oh-my-pi` (~14.5K). High confidence on the primary.
- **"hermes" → `NousResearch/hermes-agent`** (~202K stars) — Nous Research's **general** agent harness
  (model-agnostic, self-improving), *distinct from the Hermes models* and not coding-specific. High
  confidence. ("hermess" is a typo for hermes.)

---

## Category schema

Every profile and matrix uses these nine categories:

1. **Popularity & traction** — stars / installs / users (OSS); funding / valuation (closed).
2. **Vendor & backing** — company/community, funding, acquisitions.
3. **License & openness** — OSI-open (which) / source-available (e.g. FSL) / proprietary.
4. **Technology & architecture** — language; surface (CLI/TUI, VS Code extension, editor fork,
   JetBrains-native, cloud VM); client-server/daemon; **MCP** support.
5. **Model support** — vendor-locked vs model-agnostic vs BYO-key; local-model support.
6. **Features & capabilities** — agentic loop, multi-file edit, planning, subagents, checkpoints,
   memory, codebase indexing/RAG, git/PR, browser, web search, hooks/plugins.
7. **Safety, permissions & sandboxing** — approval modes, sandbox tech, default-deny vs default-allow,
   secret handling. *(flux's key axis.)*
8. **Extensibility** — plugins, MCP, skills, hooks, SDK, marketplace.
9. **Status & differentiator** — active / archived / acquired / wound-down + the one-line hook.

---

# Part 1 — The landscape

## Matrix A — CLI / terminal-first agents (flux's cluster)

| Tool | Repo (canonical) | Stars | License | Lang | Models | MCP | Sandbox / safety | Status |
|---|---|---|---|---|---|---|---|---|
| **Claude Code** | `anthropics/claude-code` | ~134K | Proprietary | TS | Claude-only | ✅ | bash sandbox + prompts | Active, leader |
| **Codex CLI** | `openai/codex` | ~93K | Apache-2.0 | Rust | OpenAI + local/BYO | ✅ | **OS-level (Landlock/seccomp/Seatbelt)** | Active |
| **Gemini CLI** | `google-gemini/gemini-cli` | ~106K | Apache-2.0 | TS | Gemini-only | ✅ | trusted-folders + prompts | ⚠️ going closed (Go) |
| **OpenCode** | `anomalyco/opencode` | ~178K | MIT | TS | ~75+ providers | ✅ | prompts (permissive) | Active |
| **Goose** | `aaif-goose/goose` | ~50K | Apache-2.0 | Rust | 50+ providers | ✅ | sandbox mode | Active (LF-governed) |
| **Crush** | `charmbracelet/crush` | ~26K | FSL-1.1-MIT* | Go | BYO key | ✅ | per-tool + `--yolo` | Active |
| **Qwen Code** | `QwenLM/qwen-code` | ~26K | Apache-2.0 | TS | agnostic + local | ✅ | prompts | Active |
| **Amp** | closed (`ampcode.com`) | n/a | Proprietary | TS | curated, no BYO | ✅ | hosted | Active |
| **Aider** | `Aider-AI/aider` | ~47K | Apache-2.0 | Python | agnostic/BYO | ❌ | git history as net | Active |
| **pi** | `earendil-works/pi` | ~65K | MIT | TS | broad/BYO | ➖ | lean prompts | Active |
| **hermes** | `NousResearch/hermes-agent` | ~202K | MIT | Python | agnostic (200+) | ➖ | sandboxed backends | Active (general) |
| **Kimi CLI** | `MoonshotAI/kimi-cli` | ~9K | Apache-2.0 | Python | Kimi-centric | ➖ | prompts | Active |
| **Forge** | `tailcallhq/forgecode` | ~7K | Apache-2.0 | Rust | 300+ /BYO | ✅ | prompts | Active |
| **gptme** | `gptme/gptme` | ~4K | MIT | Python | agnostic + local | ➖ | shell perms | Active |
| **Codebuff** | `CodebuffAI/codebuff` | ~7K | Apache-2.0 | TS | multi | ➖ | prompts | Active |
| **Plandex** | `plandex-ai/plandex` | ~16K | MIT | Go | agnostic/BYO | ➖ | **diff-review sandbox** | Active (cloud wound down) |
| **Warp (Oz)** | closed | n/a | Proprietary | — | multi (orchestrates) | ➖ | hosted | Active |
| **Auggie** | closed (npm CLI) | n/a | Proprietary | — | hosted | ✅ (is an MCP server) | Active (→ Cosmos) |
| **RA.Aid** | `ai-christianson/RA.Aid` | ~2K | Apache-2.0 | Python | multi/BYO | ➖ | prompts | Active |
| **iFlow CLI** | `iflow-ai/iflow-cli` | ~5K | unclear* | Shell | multi | ➖ | prompts | Active |
| **Amazon Q CLI** | `aws/amazon-q-developer-cli` | ~2K | Apache-2.0 | Rust | Bedrock | ✅ | AWS-managed | ⚠️ broader EOS |
| **Open Interpreter** | `openinterpreter/open-interpreter` | ~64K | Apache-2.0 | Rust | open-model-first | ➖ | local exec + prompts | Active |
| **flux** *(this project)* | `codewandler/flux` | — | MIT/Apache-2.0 | **Rust** | wire×credential (5, agnostic) | ❌ *(gap)* | **guarded-IO + default-deny policy + forced approval** | Active (0.2.1) |

\* FSL = Functional Source License (source-available, converts to MIT after 2y). "unclear" = license
not clearly stated. ✅ = supported · ❌ = not supported · ➖ = partial/unverified.

## Matrix B — IDE-integrated agents & completion platforms

| Tool | Repo / metric | Stars/installs | License | Surface | Models | MCP | Status |
|---|---|---|---|---|---|---|---|
| **Cursor** | `cursor/cursor` (docs) | ~33K / 1M+ DAU | Proprietary | VS Code **fork** | router + in-house | ✅ | Active; SpaceX deal *uncertain* |
| **Windsurf** | closed | ~3.86M ext | Proprietary | VS Code **fork** | SWE-1.x + 3rd-party | ✅ | Owned by Cognition |
| **Cline** | `cline/cline` | ~64K / 4.4M | Apache-2.0 | VS Code ext + SDK/CLI | 30+ BYO + local | ✅ | Active (biggest OSS) |
| **Roo Code** | `RooCodeInc/Roo-Code` | ~24K (archived) | Apache-2.0 | VS Code ext | broad/BYO | ✅ | ⚠️ **shut down** |
| **Kilo Code** | `Kilo-Org/kilocode` | ~24K / 1.25M | MIT | VS Code ext + JB/CLI | 500+ /local | ✅ | Active, fast-growing |
| **Continue** | `continuedev/continue` | ~34K / 3.45M | Apache-2.0 | VS Code + JB + CLI | full BYO + local | ✅ | ⚠️ read-only (acquired?) |
| **GitHub Copilot** | closed | ~74–76M ext | Proprietary | many IDEs + cloud | multi + BYO | ✅ | Active (largest base) |
| **Augment** | closed | ~765K / 675K | Proprietary | VS Code + JB + CLI | hosted multi | ✅ | ⚠️ IDE sunsetting → Cosmos |
| **Junie** | closed | ~26.4M JB | Proprietary | **JetBrains-native** + CLI | per-task + local | ➖ | GA (Jun 2026) |
| **JetBrains AI** | closed (Mellum OSS) | ~167M JB | Proprietary | JetBrains plugin | broadest + local | ✅(beta) | Active |
| **Amazon Q Dev** | `aws/amazon-q-vscode` | ~1.77M | Proprietary (ext Apache) | many IDEs + CLI | Bedrock | ✅ | ⚠️ EOS → Kiro |
| **Zed AI** | `zed-industries/zed` | ~86K | GPL/AGPL/Apache | **native Rust editor** | BYO + local | ✅ | Active (1.0) |
| **Tabby** | `TabbyML/tabby` | ~34K | Apache-2.0* | **self-host server** | self-host/local | ➖ | Active |
| **Refact** | `smallcloudai/refact` | ~4K (archived) | BSD-3 | local daemon + plugins | 20+ + local | ✅ | ⚠️ repo transition |
| **Trae** | `bytedance/trae-agent` (CLI) | ~12K | Proprietary IDE | VS Code **fork** | multi (no keys) | ➖ | Active ⚠️ telemetry |
| **Void** | `voideditor/void` | ~29K (archived) | Apache-2.0 | VS Code **fork** | full BYO + local | ➖ | ⚠️ paused |
| **Tabnine** | `codota/TabNine` (legacy) | ~11K | Proprietary | multi-IDE plugin | own + 3rd-party | ✅ | Active (air-gapped) |
| **Qodo** | `qodo-ai/pr-agent` | ~12K | Mixed | plugin + CLI + PR bot | BYO | ✅ | Active (test/review) |
| **Cody** | (repo pulled) | — | Enterprise-only | multi-IDE | multi | ➖ | ⚠️ individual wound down |

\* some enterprise features are source-available/commercial.

## Matrix C — Autonomous / cloud SWE agents

| Tool | Repo / metric | Stars/funding | License | Lang | Isolation | SWE-bench Verified | Status |
|---|---|---|---|---|---|---|---|
| **OpenHands** | `OpenHands/OpenHands` | ~78K | MIT | Python | **Docker** | ~53%; up to ~72% | Active (top OSS) |
| **SWE-agent** | `SWE-agent/SWE-agent` | ~20K | MIT | Python | Docker | SoTA Feb'25 | Active (research) |
| **mini-SWE-agent** | `SWE-agent/mini-swe-agent` | ~5K | MIT | Python | Docker | ~65–74% | Active |
| **Devin** | closed | ~$26B val | Proprietary | — | **cloud VM** | 13.86% (2024, dated) | Active (category-definer) |
| **MetaGPT** | `FoundationAgents/MetaGPT` | ~69K | MIT | Python | — | n/a | Active (multi-agent) |
| **GPT-Pilot** | `Pythagora-io/gpt-pilot` | ~34K | NOASSERTION | Python | — | n/a | Active |
| **GPT-Engineer** | `AntonOsika/gpt-engineer` | ~55K (archived) | MIT | Python | — | n/a | ⚠️ archived → Lovable |
| **Devika** | `stitionai/devika` | ~20K | MIT | Python | — | n/a | Stale |
| **Devon** | `entropy-research/Devon` | ~3K | AGPL-3.0 | Python | — | n/a | Modest |
| **Aide** | `codestoryai/aide` | ~2K (archived) | AGPL-3.0 | TS | — | n/a | ⚠️ archived |
| **Suna** | `kortix-ai/suna` | ~20K | NOASSERTION | TS | — | n/a | Active (generalist) |
| **Agent Zero** | `agent0ai/agent-zero` | ~18K | NOASSERTION | Python | — | n/a | Active (framework) |
| **smol developer** | `smol-ai/developer` | ~12K | MIT | Python | — | n/a | Historical |
| **Factory / Droid** | `Factory-AI/factory` (docs) | ~1K / $50M | Proprietary | — | cloud | Terminal-Bench claim | Active |
| **Cosine / Genie** | closed | — | Proprietary | — | cloud | vendor-claimed | Active |
| **Codex cloud** | closed | — | Proprietary | — | **cloud sandbox** | TB 2.1 #1 ~83% | Active |
| **Replit Agent** | closed | ~$9B val | Proprietary | — | cloud | — | Active (app builder) |

App builders (adjacent): **Lovable** (~$6.6B), **v0/Vercel** (~$9.3B), **Bolt.new**
(`stackblitz/bolt.new` ~16K, MIT), **LlamaCoder** (~7K). See [profiles-autonomous.md](profiles-autonomous.md).

## Matrix D — the safety axis (flux's differentiator, head-to-head)

The single most important comparison for flux. Most agents gate with *approval prompts* and rely on
the user's shell for isolation; a few add real OS/container isolation; **flux is the only one that
pairs OS-confined guarded IO with a default-deny *authorization policy* (grants over subjects ×
resources × actions) and forced approval on destructive ops.**

| Tool | Isolation mechanism | Permission model | Default posture | Secret redaction | Auditable trail |
|---|---|---|---|---|---|
| Claude Code | bash sandbox | approval prompts + allow-rules | allow-with-prompts | partial | session log |
| Codex CLI | **OS-level (Landlock/seccomp/Seatbelt)** | approval modes | allow-with-prompts | partial | log |
| OpenHands | **Docker container** | approval | allow-in-container | — | event stream |
| Devin | **cloud VM** | plan approval + PR review | autonomous-in-VM | hosted | wiki/log |
| OpenCode / Crush / Goose | varies (sandbox mode) | per-tool prompts / `--yolo` | allow-with-prompts | — | log |
| Aider / pi / gptme | shell permissions | prompt before run | allow-with-prompts | — | git / log |
| **flux** | **guarded-IO choke point** (workspace-confined, symlink/escape-rejecting, **argv-only**, SSRF-guarded fetch) | **default-deny policy** + permission rules + **forced approval on destructive** | **deny-by-default (usable local default)** | **enforced `Redactor`** | **event-sourced evidence** |

Takeaway: **Codex** matches flux on OS-level isolation *intent*; **OpenHands/Devin** match on
container/VM isolation; but *none* combine isolation with a **default-deny capability policy** + forced
destructive-op approval + enforced secret redaction + event-sourced evidence as a single
non-bypassable envelope. That combination is flux's whitespace.

---

# Part 2 — What this means for flux

flux's stated identity (per [../vision.md](../vision.md)) is **non-bypassable safety, provider
neutrality, local-first/no-telemetry, and quality over quantity.** Read against the field:

## Where flux is genuinely differentiated

- **Safety by construction is unique.** As Matrix D shows, flux is the only entrant pairing a single
  guarded-IO choke point with a **default-deny authorization policy** and forced approval on
  destructive operations. The capable agents lead with capability and bolt on prompts; the isolated
  ones (Codex/OpenHands/Devin) sandbox the *process* but don't impose a default-deny *capability*
  policy over subjects × resources × actions. "The autonomous agent you can actually trust to run" is
  open positioning.
- **Local-first / no-telemetry is increasingly rare.** Cursor/Windsurf/Amp/Trae are cloud-coupled;
  **Amp's free tier trains on your code**; **Trae** drew telemetry concerns. flux's "no phone-home,
  secrets stay on the box" is a real differentiator for regulated/privacy-sensitive users — a lane
  otherwise held by Tabby/Tabnine (completion) and self-hosted Refact.
- **Provider neutrality with credential reuse.** The *wire codec × credential* model is clean, and
  reusing existing **Claude / Codex subscription OAuth** (`flux auth login claude`) is a neat trick
  few harnesses offer. (Breadth is lower than OpenCode's ~75 / Goose's ~50 providers, but the
  architecture scales — see gaps.)
- **Auditable, event-sourced sessions + evidence.** Few tools emphasize a durable "explain what the
  agent did and why it was allowed" trail; it pairs naturally with the safety story for enterprise.
- **Engineering discipline as a feature.** A small, strictly-layered surface enforced by a layering
  lint, a permanently green gate, and "every change ships with a test" is the opposite of the long
  tail of **archived/abandoned** repos in Matrix C (gpt-engineer, devika, aide, void, refact…). This
  is a credibility/marketing asset, not just an internal nicety.

## Where flux is behind / table-stakes gaps

- **MCP is the conspicuous gap.** The Model Context Protocol is **near-universal** — Claude Code,
  Codex, Gemini CLI, OpenCode, Goose, Crush, Qwen Code, Amp, Forge, Cline, Copilot, JetBrains, Zed,
  and more all speak it. flux instead exposes a **bespoke subprocess-plugin protocol (framed NDJSON) +
  JS hooks**. flux's model is arguably *more* capability-secure (manifest-scoped, deny-by-default host
  capabilities), but it's an island: it can't consume the large and growing MCP server ecosystem, and
  other agents can't consume flux. This is the highest-leverage gap.
- **No IDE / editor surface.** Explicitly out of current scope (vision lists CLI/TUI/SDK/HTTP), and
  that's a defensible focus — but it cedes the largest *distribution* channel (VS Code/JetBrains
  installs dwarf CLI adoption).
- **No benchmark presence.** flux appears on no SWE-bench / Terminal-Bench / Aider-polyglot board. For
  a safety-first agent, a credible number would prove safety doesn't mean incapable.
- **Roadmap items that are table stakes elsewhere:** `web_search` (still "Next"; `web_fetch` exists),
  a real **browser tool** (deferred behind the `flux-browser` seam), and **cloud/background agents**
  (Codex, Cursor, Devin, Factory all ship these).
- **No ecosystem/marketplace.** Plugins exist but there's no discovery/marketplace (vs Claude Code's
  marketplace, Cline/Kilo's MCP marketplaces, Continue's Hub).
- **Provider count is small** (5) relative to the model-agnostic leaders — fine architecturally, but a
  visible number users compare on.

## Opportunities (from the gaps)

1. **MCP compatibility — do it without surrendering the envelope.** Add an MCP *client* (and optionally
   expose flux's tools as an MCP *server*), but route every MCP tool call through the same
   `Executor::dispatch` → policy → approval → guarded-IO chain. That unlocks the whole MCP ecosystem
   while keeping flux's distinctive guarantee — a story no one else can tell ("MCP, but every server is
   policy-gated and sandboxed"). This is the single highest-leverage move.
2. **Make Terminal-Bench the proving ground.** It's the CLI-relevant benchmark (Codex/Claude Code lead
   it). A respectable score *with the safety envelope on* is a uniquely credible claim.
3. **Lead the "safe autonomy" niche.** None of the high-autonomy agents combine autonomy with a
   default-deny capability policy. Pair flux's `/pd` / `/goal` / `/loop` autopilot with the envelope
   and market the combination directly.
4. **`flux-sdk` as the "build a *safe* agent" library.** OpenHands and pi ship SDKs, but neither leads
   with a non-bypassable safety envelope. "Embed an agent that *cannot* escape the workspace or leak
   secrets" is a differentiated SDK pitch for the agent-platform builders.
5. **Own local-first/private + auditable for regulated buyers.** Combine no-telemetry, on-box secrets,
   guarded egress (SSRF-guarded), and event-sourced evidence into one compliance-flavored narrative —
   a lane the cloud-coupled leaders structurally can't enter.

## One-paragraph positioning

> Most coding agents optimize for capability and reach (Claude Code, Cursor, Copilot) or for
> autonomy at scale (Devin, OpenHands, Codex cloud); a few optimize for openness and provider breadth
> (OpenCode, Goose, Aider). **flux's whitespace is the intersection of capability and trust:** a
> provider-neutral, local-first coding agent whose every tool call — built-in, plugin, or sub-agent —
> passes through one non-bypassable, default-deny, fully-audited safety envelope. The clearest path to
> making that legible to the field is **MCP-compat (gated through the envelope)** plus a **credible
> Terminal-Bench result with safety on.**

---

*Sources: per-cluster profiles link to primary sources (GitHub repos, vendor blogs, TechCrunch,
benchmark leaderboards). Raw research was gathered 2026-06-25 across four parallel streams; star
counts re-grounded against the authenticated GitHub API the same day.*
