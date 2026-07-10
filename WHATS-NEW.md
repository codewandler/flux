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
