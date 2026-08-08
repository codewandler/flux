---
title: What's new
description: "Customer-facing Flux release notes, generated from the changelog embedded in the CLI."
---

# What's new in flux

This is the same customer changelog embedded in the binary. From a terminal, use
`flux changelog` for the latest release, `flux changelog --all` for the full history, or
`flux changelog <version>` for one release.

<!-- BEGIN generated:whats-new -->
## [Unreleased]

## [0.59.2] - 2026-08-08

### New

- **`flux ops --explore` browses every operation flux can run.** Operations are the things flux actually
  does — read a file, commit, fetch a page, run a test. Until now there was no way to see the full list.
  `flux ops --explore` opens a search box: start typing and you get a ranked list on the left and, on the
  right, what the selected operation does, the parameters it takes, how risky it is, and links to its
  documentation. Tab cycles a category filter, `Ctrl-Y` copies the doc link, and `Esc` steps back out.

- **A datasource connects from the machine its endpoint is reachable through.** Naming the host an
  endpoint answers on told flux where to dial from, but a datasource reading that endpoint still
  opened its connection wherever the session happened to be. It now follows the endpoint: a
  host-bound datasource is refused up front if you are working from somewhere else, naming both the
  machine it needs and the machine you have, and it is refused before any connection is attempted
  rather than failing at dial time. Datasources whose endpoints name no host are unchanged.

- **Rescue the work an interrupted run left behind.** If the machine goes down mid-run, a worker's
  uncommitted changes used to be invisible — the run reported no work, and the repair it suggested
  was the command that deletes the directory holding it. `flux fleet doctor` now reports a working
  directory holding uncommitted changes, says how many files, and takes precedence over the advice
  that would have thrown them away. `flux fleet capture <wave>` commits what was left, onto that
  piece of work's own branch. And `flux fleet reopen <wave>` puts a run that stopped short of
  delivery back where it can continue, instead of leaving it stranded in a state no command could
  move.

### Fixed

- **A run is reported delivered only when the work is actually on the branch.** "Delivered" was
  written from intent: the run tagged what it had built and then said it had landed, without ever
  checking. A run that finished its own pipeline but has not landed now says so — and re-running the
  final step re-asks the question, so the answer catches up as soon as the work lands. Where landing
  requires a push, flux says that plainly instead of quietly claiming success.

- **An interrupted run stops holding its work hostage.** When the process supervising a run died,
  the items it had claimed stayed claimed, so the queue reported itself empty while the top of the
  list never moved and no command could free it. A run whose supervising process is provably gone
  now releases its claims on the next tick — carefully, so a live run is never released — and the
  same tick picks the work up again. Its working directories are kept, because they may hold the
  only copy of what was in progress.

- **Ready work is no longer skipped on a guess.** The scheduler could decide a piece of work was
  "already built" because some unrelated commit mentioned it, and skipping is the one decision that
  leaves no trace. It now checks the claim against the work itself before acting on it, and a check
  that fails is reported alongside the work being scheduled rather than silently swallowed. Anything
  skipped repeatedly is now reported, so a permanent skip cannot pass for an empty queue.

- **A flaky test no longer fails a release.** A test that checked how flux refuses a version
  mismatch reported an unreachable fixture as that refusal failing, which was the single largest
  source of failing builds here and once aborted a release twenty minutes after the same suite had
  passed.

## [0.59.1] - 2026-08-08

### New

- **Say which machine an endpoint is reachable from.** A service that only answers inside your
  cluster and one that answers from anywhere used to look identical once written down, so flux
  would happily try a cluster-internal address from your laptop and fail at dial time. An endpoint
  can now name the host it is reachable through — `host = "k8s-dev"` in its config, or
  `flux endpoint add --host k8s-dev` — and `list`, `show` and `resolve` all show it, so `resolve`
  now tells you *from where* as well as *as whom*. Endpoints that do not name a host keep working
  exactly as before, meaning reachable from wherever you happen to be. Two things get caught early
  instead of late: naming a host that does not exist is reported when the config loads, listing the
  hosts that do exist, and asking for a host-bound endpoint while working from somewhere else is
  refused up front, naming both sides, before any credential is unlocked.

## [0.59.0] - 2026-08-07

### Action needed

- **Upgrade `flux system serve` daemons together with their clients.** This release changes the
  remote execution protocol twice over — host metrics and guarded HTTP both cross the wire now —
  and the two ends check that they agree before doing anything. A daemon from an older release and
  a client from this one will refuse to pair at all, saying so plainly rather than degrading. If
  you run the serving daemon anywhere (a container, a pod, a VM guest, a build box reached over
  ssh), update it in the same step you update the machines that talk to it.

### New

- **Name the machines your work runs on.** A new `[[host]]` config section declares named execution
  hosts — your local machine, or a remote one served over the secure remote protocol — with the
  credential kept as a reference to where it lives, never pasted into config. Manage them with
  `flux host ls / show / add / rm`, and verify one end-to-end with `flux host probe`, which checks
  the host's identity without running anything on it. Declared hosts also become visible to the
  agent, which can list, inspect and probe them (never reading a credential value).

- **Run on a named host with `--host <name>`.** Instead of pasting a URL every time, select a
  declared host by name and the session executes its effects there; the audit trail records which
  host every action ran on. Selection is permission-gated: each host says who may use it
  (interactive sessions, unattended automation, both — or nobody, the default), and automation
  never silently inherits a permission granted only to interactive use. `--remote <url>` still
  works for one-off connections.

- **Web requests actually run on the host you select.** Fetching a page or calling an API now
  travels to the execution host you picked with `--host` and leaves *that* machine's network, with
  that machine's address — not the one your agent happens to be sitting on. The remote host applies
  its own egress guard to every hop, enforces the response size limit itself, and re-checks each
  `$secret` grant at every redirect it follows, so a `Location` pointing outside a grant's `to=`
  list is refused on the machine that would have followed it. If that host admits a request to a
  private or internal address, the admission appears in your session's audit trail naming the host
  it happened on. A credential your request carries is necessarily visible to the host that sends
  it — scope it with `NAME;to=<host>` so the grant, not the network, decides where it can travel.
  A host started without web support says so up front and refuses, rather than quietly running the
  request locally. Browser automation and site crawling still run locally, since they drive a local
  browser.

- **Confinement is now a host you can name.** `[[host]] backend = "sandboxed"` declares a host that
  runs guarded effects under the operating-system sandbox (bubblewrap on Linux, Seatbelt on macOS)
  — and it fails closed: on a machine with no usable confinement, selecting it refuses at startup
  and names the reason, instead of quietly running unconfined. A forged or stale "already confined"
  environment marker is refused the same way. `flux host probe` reports which backend a sandboxed
  host would use, and an autonomy posture that must not run unconfined selects it automatically for
  named local work. `--sandbox`/`--no-sandbox` behave exactly as before, with one deliberate limit:
  they never lower an explicitly selected sandboxed host.

- **Ask a named host how it is doing.** `flux host metrics <name>` reports a machine's own
  condition — CPU, load, memory, swap, disk, uptime, temperature and fan speed — as typed
  readings, with JSON output as the automation API and the same view available to the agent. A
  metric the machine cannot measure is reported as explicitly unavailable with a reason, never as
  zero, and readings that came from a remote machine are marked as reported by it rather than
  observed locally.

- **Choosing a confined host no longer costs you the web.** Fetches and HTTP requests made under
  a selected host now go out through the same reviewed, guarded egress path an ordinary run uses,
  with the same private-network audit trail — and a host that genuinely cannot make requests
  still refuses plainly instead of quietly sending them from somewhere else. Sub-agents now
  inherit exactly the host their parent was running on, and nothing else.

- **Deploy the remote execution system from shipped, published artifacts.** Flux now ships a
  container image that runs only the serving daemon as a non-root user with no secrets in any
  layer, a Kubernetes Kustomize base (one replica per workspace, persistent volume, TLS and bearer
  Secrets, a default-deny network policy), and a hardened VM/microVM guest unit with a cloud-init
  install contract. Upgrade, rollback and version-mismatch behavior are documented per profile.

  The image is published per release at `ghcr.io/codewandler/flux-system:<version>` — the released
  binary repacked rather than rebuilt, so the image and the release archive describe the same
  bytes — and both Kubernetes profiles point at it, restamped by the cut so the manifests you apply
  and the binary they run can never name different versions. Check where it came from before you
  run it:

  ```sh
  gh attestation verify oci://ghcr.io/codewandler/flux-system:<version> --repo codewandler/flux
  ```

  Building locally still works, for an air-gapped registry or a different base. Flux still never
  provisions your Docker hosts, clusters or VMs — the artifacts run where you already have
  somewhere to run them.

- **A VM or microVM guest is a host you can name.** Declare `backend = "microvm"` in a `[[host]]`
  entry pointing at a guest that runs the serving daemon, and `--host <name>` runs guarded effects
  inside it — same authenticated protocol, same handshake, same credential *reference* as a remote
  host. `flux host probe` reports the negotiated protocol version and the guest's own substrate
  identity, marked as reported by it. Flux still never creates, starts, stops or destroys a guest:
  the binding consumes an endpoint that already exists, and the VM/microVM deployment profile is
  how you make one. A binding declared before its guest exists is legal and says so — it lists as
  unwired and refuses selection naming the missing endpoint, instead of quietly falling back to
  your machine.

- **Deploy the agent itself, not just the machine it acts on.** Alongside the serving-daemon
  profiles, Flux now ships Kubernetes manifests that run the agent surface from the same released
  image — the HTTP/A2A endpoint you talk to with `flux a2a`, with its bearer token and model
  credential as Secret references, a persistent session volume, and network policy that denies by
  default in both directions. The manifests cannot express an unauthenticated public listener: a
  bind that reaches beyond loopback without a token fails the shipped checks, and the running
  daemon refuses it too.

- **A clearer answer when a remote machine will not start.** If an `ssh` host binding cannot bring
  up flux on the far machine, the message names the problem you actually have. A machine with no
  sandbox available says so, and points at the fix on *that* machine, instead of claiming flux is
  not installed there when it plainly is. "Not installed" and "installed but refused to start" are
  told apart by the far side's exit status rather than by how its shell happens to word things, so
  the distinction holds whatever shell that machine runs.

- **Use the dev box you already have.** Declare an `ssh` host binding and flux reaches a machine
  that has nothing on it but sshd: it opens a port-forward, makes sure flux is serving on the other
  side, and runs your work there over the same secure protocol a `remote` host uses — same
  authentication, same version check, same approval gates. ssh only gets you there; it never
  becomes the way your work runs, so the far machine keeps enforcing its own permissions. Nothing
  prompts: host-key checking is strict, a changed key is a refusal you can read rather than a
  question in an unattended run, and your key stays a reference to a file flux never opens. Every
  failure names the piece that is missing — no sshd, an unrecognised host key, a key that was
  declined, no flux binary over there — and none of them quietly falls back to your own machine.

- **Watch an agent that lives on a server, from your own terminal.**
  `flux tui --attach https://agent.internal:8787` points the full chat UI at an agent served by
  `flux app run --serve`: its turns stream into the ordinary transcript, your messages go into the
  live session, and reconnecting replays what happened while you were away, read from the agent's
  own history. The bearer token is always named by reference, never typed on the command line. The
  UI is honest about the seams — anything the protocol does not carry (tool call details, for now)
  is shown as unavailable with the reason, and the conversation lives in the server's session
  store, so it deliberately does not appear in your local `flux sessions`.

- **Web requests ride out rate limits.** When an API answers `429 Too Many Requests`, fetches and
  HTTP requests now wait and try again — honouring `Retry-After` in either of its forms, or backing
  off gently when the server names no delay — instead of handing the error straight to your
  program. Every wait stays inside the timeout you set, cancelling a turn ends it immediately, and
  the result says what happened: `rate-limited, retried 2 times over 3.4s`. On a remote host the
  waiting happens next to the service being called, not on your machine. A `503` is deliberately
  *not* retried: a 429 tells you the server received your request and declined to act on it, which
  is what makes retrying safe even for a POST, while a 503 makes no such promise.

- **Reach your own cluster by name.** A `[[host]]` binding can now name the private CA its endpoint
  uses (`ca_cert = "/etc/flux/ca.pem"`), so a Kubernetes pod, VM guest or container with a
  certificate from your own CA is reachable as `flux --host cluster …` rather than only through
  `--remote … --remote-ca`. The CA is read as one exactly-named file — naming it grants that file
  and nothing else in its directory — and if it is missing, unreadable or too large the binding
  refuses and tells you which file. It never quietly falls back to public trust, and there is no
  flag to skip the check.

- **See and switch the loop your agent runs, without leaving the terminal UI.** The loop driving
  the current agent is now always shown, a hotkey opens a selector listing the `*.flux` loops
  available to you, and choosing one raises a short overlay that visualizes the outer loop's
  structure and renders its description — so "what is this agent actually doing, and can I run it
  differently" is answerable in place. A loop authored while the UI is running appears without a
  restart.

- **`/restart` reloads flux without losing your conversation.** After upgrading, type `/restart` in the
  terminal UI and it relaunches on the new version with the same options and the same session — no quitting,
  no retyping the command you started with.

- **`/fleet:restart` applies fleet configuration changes without leaving the terminal UI.** Edit your fleet
  settings, type `/fleet:restart`, and the fleet stops and starts so the new configuration, loops and limits
  take effect. It waits rather than interrupting: if a worker is still running it tells you instead of
  restarting underneath it. `/fleet:refresh` re-reads fleet state when you want the current picture now.

- **When a fleet goes wrong, you can now ask it what happened.** Diagnosing a stuck run used to mean
  reading the fleet's internal state file by hand. Now each question has a command: `flux fleet doctor`
  reports whether the running system is actually healthy — not just whether its configuration is valid
  — and names things like a worker recorded as working with no process behind it. `flux fleet inspect
  gate <wave>` shows why a run went red by printing the gate's own output, newest first, so the verdict
  is the part you see. `flux board reconcile` tells you which items already have their work in the
  tree while their status still says otherwise, so nobody is sent to build something twice; it reports
  and never changes anything.

- **Pausing, repairing and pausing-for-upgrade are commands now, not procedures.** `flux fleet park
  <wave> --reason` pauses a run and records *why*, and `flux fleet unpark` resumes it — the pause and
  its reason show up in `flux fleet status`, so a paused run stays paused instead of being reconsidered
  every minute. `flux fleet repair` rebuilds working directories a run expects but disk no longer has.
  And `flux fleet quiesce` stops new work and confirms nothing is still running, so you can install a
  new version safely; `flux fleet resume` lifts it. Each replaces a hand-run sequence that was easy to
  get half-right.

## [0.58.0] - 2026-08-07

### New

- **A run's time and token budget is now visible while the run happens.** When a budget is set, the
  terminal UI header shows how much of it has been spent against what was declared and keeps updating as
  spend accrues; plain `flux run` says when a line is crossed. The two kinds of line stay distinct: a
  target is guidance you can pass, a limit stops the run.
- **The board view groups work the way you read it.** Items appear as collapsed boxes grouped by status
  and ordered exactly as "what should I do next" orders them, so the top of the list is the answer. Wide
  boards stay responsive because only the rows on screen are built.

### Improved

- **`flux fleet status` stays small and says what to do next.** It used to grow without bound by copying
  whole turn receipts and event histories into itself — megabytes for one command. It now reports the
  shape of things within a fixed size, names the single most useful next command for the state it found,
  and says plainly when it left something out. Full detail is still one `flux fleet inspect` away.
- **A fleet worker is offered the toolchain its own repository has.** Previously the tools a worker could
  use were derived from wherever the coordinator was started, so a worker could be handed a toolchain that
  did not exist where it was working.

### Fixed

- **Turning off a tool or denying a permission now applies to helper agents too.** A `deny` rule or a
  disabled tool used to stop at the first agent and not reach the agents it delegated to, including
  nested ones. It now travels the whole way down.

## [0.57.0] - 2026-08-06

### New

- **A workspace can now carry its complete cross-repository program in `.flux/board.toml`.** Plain
  `flux board` selects the configured workspace, validates every repository story and dependency,
  and reports the active milestone, ordered work and configured waves. `flux fleet schedule` reads
  that same plan instead of keeping another schedule. Board configuration, Fleet configuration and
  live Fleet state remain separate files with separate owners.

- **The terminal UI can supervise the Fleet main coordinator and its Board.** Launch
  `flux tui --fleet` in a Fleet root, or use `--fleet=ROOT`, to resume the exact durable main
  conversation. `F2`, `/fleet`, or `/board` opens bounded native views of active workers, work,
  decisions, failures, planning documents, and exact progress statistics; wide terminals also keep
  an attention rail beside the chat. Requirements show accepted, delivered, and completed/failed
  acknowledgement across restart. Viewing is read-only, and deciding an open question requires a
  second explicit confirmation. Ordinary `flux tui` remains a labelled standalone chat, and the
  operations view cannot push, release, deploy, apply a Fleet candidate, or clean worktrees.

### Improved

- **The Fleet main agent is now a coordinator, not a general coding agent.** It can read and manage
  the configured Board and Fleet, list all durable workers, and delegate bounded read-only research.
  It no longer receives shell, file-editing, git-writing, web/plugin, pane, evaluation, or transient
  process-worker tools. Safe status and planning reads do not ask for approval, and Fleet status is
  compact even when old waves have accumulated. Free-form questions use the Fleet's authored loop
  with only the current request instead of growing an adaptive history budget.

- **Fleet workers now run the loop their task kind selected at admission.** Operator-authored loop
  profiles replace the general adaptive explorer for implementation, research, review, repair and
  decision work. Flux validates that loop against the worker's tools before starting and preserves
  its exact digest across messages, restart, resume and rework. Status and terminal receipts show
  the bounded loop identity, and dispatch responses include the worker ids the coordinator needs.

- **Fleet supervision opens immediately, even in a large workspace.** Board and Fleet views load in
  the background, so refreshing them no longer freezes typing or rendering. If a later refresh
  fails, the last working view stays visible and is clearly marked stale.

- **Board and Fleet terminology is documented from planning through release.** The guides explain
  epics, stories, milestones, program lanes, configured versus dispatched waves, workers, handoffs,
  review, gates and apply. Concepts now appears before Coding, and compact diagrams show the
  eligibility, worktree, rework and publication boundaries.

### Action needed

- **Fleet configurations must name the main coordinator loop.** Add
  `loop = ".flux/fleet/loops/main-coordinator.flux"` and
  `research_loop = ".flux/fleet/loops/research.flux"` under `[main]`, and keep both Flux-Lang files
  in the Fleet root. `flux tui --fleet` now refuses a missing or invalid binding rather than
  falling back to the general adaptive agent loop for either the coordinator or its research task.

- **Fleet worker templates must declare loop policy.** Add operator-authored `[loop_profiles.*]`
  entries, map each supported task kind under `[loop_policy]`, and set `task_kind` on each agent
  template. Flux refuses an unbound or capability-incompatible worker before creating its worktree
  or making a model request.

## [0.56.0] - 2026-08-05

### New

- **Flux now has one durable board-and-fleet workflow for AI-assisted development.** `flux board`
  manages a session checklist, one repository's Track stories, or a cross-repository workspace,
  including vision, roadmap, decisions, designs, exact progress statistics, and current/history
  reports. `flux fleet` schedules bounded local sub-agents into isolated worktrees, preserves their
  commits and test/review evidence across restart, and provides stable status, message, activity,
  log, worktree, and integration inspection. Claude, Codex, and scripts use the same clean JSON API;
  `flux board skill` and `flux fleet skill` print short installed-version guides. A green wave stays
  local until you explicitly run `flux fleet apply`, and neither command pushes, releases, deploys,
  or removes worktrees for you. The new **Coding / AI-assisted development** docs section explains
  the whole model and every backend.

- **You can now name how much autonomy an agent runs with, in one choice.** `--posture supervised`
  asks you before each effect. `--posture bounded-autonomy` never asks and instead constrains the
  run with policy, a fail-closed sandbox and budgets — the posture unattended runs already used.
  `--posture exploratory` suits research and long investigations where being interrupted is itself
  the problem, leaning on hard isolation and full evidence instead. `--posture refusing` denies
  everything. Each posture sets its approval, confinement and budget together, so you cannot end up
  with approvals switched off and confinement left open. The documentation states plainly what each
  one relies on and what it does not protect you against. `--yes` keeps working and means
  `bounded-autonomy`.

- **Streaming JSON output now identifies which call each tool event belongs to.** Tool call and tool
  result lines carry a new `dispatch` field holding the same identifier on both ends of one call, so
  a script consuming the stream can pair a result with its call directly instead of guessing by tool
  name. The field is additive; existing consumers are unaffected.

- **`flux review` now shows what it is doing while reviewers work.** By default it displays a live
  reviewer tree in a terminal and readable progress lines when redirected. Use
  `--progress tree|plain|off` to choose the display explicitly. Progress goes to stderr, so the final
  Markdown or JSON report on stdout stays clean for scripts.

- **Agents can now run a Flux program supplied directly in a `flow_run` call.** Use
  `inline_program` when a program does not need to be saved first; it supports the same input bindings
  and execution safeguards as named and file-based flows.
- **You can delegate Flux-Lang authoring to a purpose-built repository role.** The
  `flux-lang-writer` inspects the language contract, makes focused `.flux` changes, and validates
  syntax and analysis without running an effectful flow as a shortcut. Actual execution still uses
  Flux's normal authorization, approval, guarded IO, sandboxing, and redaction. A source-linked
  catalogue now makes every built-in and repository role discoverable.

- **The first-run Exchange command surface is explicit and safe while its dependencies land.**
  `flux exchange local start|status|stop` and `flux integration connect|grant|list|doctor` now have
  closed command shapes. Their current deterministic `unsupported` response is a temporary gate,
  not the final local-lifecycle or integration result contract. Until the compatible Exchange
  release and Flux lifecycle manager ship, the commands make no changes, setting values stay out of
  diagnostics, and credential or token arguments are not accepted. The final managed lifecycle and
  owner onboarding run only on the two Linux GNU targets; other Flux targets keep the commands but
  return a typed side-effect-free platform refusal and may still use a separately provisioned Linux
  Exchange through the authenticated runtime HTTP client.

- **Flux can now use operations granted to an Exchange Service Account.** Set the Exchange URL and
  Service Account token in the host environment. Flux refreshes the available operations between
  turns and sends one-shot calls to Exchange, while Exchange keeps credentials and deployment
  choices. If Exchange goes offline, core Flux tools keep working and external operations disappear.

- **Flux programs can read headers and other map keys that contain punctuation directly.** Use
  familiar quoted access such as `$response.headers["content-type"]`; append `?` for optional access.
  Quoted numeric keys remain object keys, while unquoted numeric brackets remain list indexes.

### Improved

- **Fleet workers keep the same tools and workspace authority for their whole assignment.** Flux
  validates each worker's declared capability bundles before it starts, then preserves that exact
  ceiling across messages, restarts, resume, and rework. Changing a template affects only newly
  admitted workers; it cannot silently give an existing worker more tools, paths, process access,
  or network authority. Status and receipts show a compact contract digest for verification.

- **Fleet workers now begin as fresh, assignment-scoped writers.** Each story worker sees its own
  exact story, pinned branch and worktree plus the configured writer instructions. It no longer
  inherits the main agent's conversation, workspace-wide goals, another worker's history or an
  unrelated assignment, and unrelated repository roots are not mounted into its process. Follow-up
  and rework still continue the same worker when explicitly addressed to it.

- **Releases no longer depend on repository settings that were never configured.** The release
  pipeline uses the existing release credential only in its isolated host-controlled mutation and
  GitHub Release steps. Model and build work still cannot see it, tags are pushed with the separate
  credential so their workflows run, and missing or unusable authority now fails before promotion
  changes repository state. No dedicated GitHub App, release Environment, ruleset or branch
  protection setup is required or claimed.

- **Remote execution now keeps compatible tools available instead of hiding their whole group.**
  Each tool declares whether it works on the selected execution system, belongs on the local control
  plane, or is native-only. Native-only tools stay hidden and are refused if called directly, while
  compatible tools in the same group remain usable; `/tools` explains why an unavailable tool was
  excluded.

- **Fenced code blocks are easier to recognize in every Markdown view.** Each row now has a `▎ `
  gutter in the terminal, exported output, and monochrome displays, including blank rows and code
  nested inside lists.

- **Tool cards now keep hostile terminal bytes inert and make diffs much easier to read.** Escape
  sequences and control bytes are stripped before live or historical tool output reaches the TUI;
  patch and Git diff output gets hunk/add/delete styling, long wrapped rows stay inside their card,
  and the CLI and TUI share one explicit truncation policy without changing `-v` full-output mode.

- **Release downloads now close as one exact, auditable set.** The release controller derives
  migration versions from complete change records and the customer-facing action-needed section,
  merges the deterministic cut through protected `main`, and verifies all 28 expected downloads,
  checksums, provenance, publication workflows and the latest-release pointer before cleaning up
  recovery evidence.

- **Flux can inspect independent evidence sources at the same time.** When a model requests several
  safe reads together, Flux overlaps them within the configured tool-concurrency limit and still
  returns their results in request order. Writes, approval-sensitive work, hooked calls, and tools
  without trustworthy read-only metadata remain ordered.

- **The security guide now tells you which secret guarantee you actually have.** It separates
  credentials kept outside Flux or a plugin from values materialized locally and protected by
  scopes and redaction. It also states plainly that unknown credentials pasted into prompts are not
  redacted and that raw prompts and answers are written to session history.

- **The model guide now explains how to use a provider Flux does not name.** Use an OpenRouter
  catalogue id for hosted models, either Ollama wire for local models, or implement the Rust provider
  interface in an embedded product. Flux keeps a small, fully maintained built-in set instead of
  promising a separate adapter for every vendor.

- **The integration guides now distinguish today's plugin compatibility path from its replacement.**
  The signed plugin pack still works today while adapters migrate. The embedded Exchange client is
  now the official path, with Exchange executing connector runtimes and no local fallback; adapter
  migrations and final plugin removal remain planned work.

- **A running REPL can now pick up a plugin's changed actions without restarting.** Run
  `/plugin-refresh <name>`; the current turn keeps the actions it started with, and the next turn
  adopts the complete refreshed set. Existing plugin grants and disabled-tool patterns still apply
  to newly advertised actions, and a rejected refresh changes nothing.

- **Approval screens now show what each confirm guard is about.** A confirm that wraps real work now
  shows the planned operations and their likely effects, instead of only a plain message. Unknown
  operations are shown explicitly, and invalid confirm risk labels are rejected before execution.

- **Maintainers can now cut a release by merging `main` into `release`.** The hosted flow writes the
  release notes, derives the version mechanically, prepares and verifies the exact build once, then
  publishes it. A failed build or publication stays visibly failed with its recovery candidate
  preserved; it cannot silently finish as a partial release. The expensive release gate now runs
  once on that exact candidate instead of being repeated during the cut.

- **The engineering presentation grew to thirteen chapters and became easier to share.** It now
  walks through the agent loop, session records, and model strategy (about twenty minutes end to
  end), prints as a complete handout, and supports touch swipe, a contents menu, and the browser
  Back button; keyboard shortcuts keep working after you click a control. Its Exchange chapter
  reflects what actually ships today, and the presentation is linked from the site's front page,
  footer, docs overview, and README instead of hiding behind the playground.

### Fixed

- **New Node, Vue, and React projects can use the Node tools before package files exist.** Requests
  that explicitly mention npm, package.json, JavaScript, TypeScript, Vue, Vuex, or React now expose
  the dedicated Node operations instead of leaving the agent with an unusable process route. The
  generic shell remains opt-in.

- **Workspace Board checks and cross-repository Fleet runs now operate on the repositories you
  configured.** Board validation includes every workspace member and understands namespaced
  dependencies between them. Fleet agents can read each configured checkout without broadening
  their write access, and one unusually large activity update can no longer corrupt the final
  completion record.

- **Long-term quota exhaustion now returns immediately instead of wasting retry time.** When a
  provider explicitly says that usage or credits are exhausted, Flux preserves the reset or limit
  message and returns control after the first response. Ordinary short-lived rate limits continue
  to retry.

- **The terminal UI no longer includes a dependency version with a known soundness defect.** Its
  rendering and input stack was updated together without changing the TUI's behavior.

- **A tool's result, timing and progress always land on the tool card that started it.** When
  several same-named reads or searches run at once, a result that arrived out of order could resolve
  whichever card was newest, leaving the real one spinning and the finished one showing another
  call's output. Each call now carries its own identity from start to finish, so the transcript,
  the fleet pane and replayed runs all pair correctly no matter how many calls overlap.

- **Building and installing Flux from one checkout no longer loses live compiler output to
  repository cleanup.** `task install` keeps its Cargo target reusable while it verifies and
  installs both `flux` and `flux-lsp`; `task clean` now refuses until active repository builds have
  finished. Existing absolute or relative `CARGO_TARGET_DIR` choices continue to work.

- **JaaS room joins no longer race each other or re-resolve their signalling host.** Two concurrent
  joins now create one live room session, and the XMPP WebSocket connects only to the address Flux's
  network guard already checked.

- **Formatting no longer moves a final top-level comment into the preceding flow.** Format-on-save
  and `fluxlang fmt` now keep a comment written at the left margin after the last declaration at the
  left margin, preserving which part of the program it documents.

- **Room-media diagnostics no longer include sidecar command arguments.** The sidecar executable
  remains visible for troubleshooting, while tokens and other host-specific arguments are redacted.

- **Webhook and connector listeners now refuse overload consistently with the main server.** Body
  size, timeout, request-rate and concurrent-work limits apply before a delivery can start, and
  long-lived streaming responses remain full-duplex under backpressure.

- **Release automation now stops clearly when generated notes have the wrong shape.** Unexpected
  prose or missing fields are rejected before a changelog, commit, or tag can be changed; a standard
  JSON code block is normalized safely.

### Action needed

- **Bringing real room audio to a sandboxed agent requires granting the audio socket explicitly.**
  The sandbox hides the host's `/run` directory, so naming your audio server on the sidecar's command
  line is necessary but not sufficient — the socket itself has to be granted. Add the directory that
  holds it, for example `writable = ["/run/user/1000/pulse"]` under `[sandbox]`, using your own user
  id. Flux now refuses at startup if a configured writable path under `/run` does not exist, naming
  the path and the likely cause, rather than creating an empty directory that applies cleanly and
  silently reaches nothing. If you previously had such a path in your config, correct it — a wrong
  user id was indistinguishable from working audio until now.

- **Rust server integrations must use the guarded address-based serving APIs.** The helpers that
  accepted an already-bound native TCP listener have been removed. Single-agent serving now uses the
  execution system selected on its engine; multi-agent serving requires that execution system as a
  new argument. Use an address ending in port `0` when the operating system should choose the port.

## [0.55.0] - 2026-08-03

### New

- **Config files can now bound delegated agent fan-out.** Set `[limits] max_live_agents` to cap the
  root plus all live descendants in one tree. `1` (and `0`, which is normalized to `1`) disables
  delegation; combine it with `max_concurrent_tool_calls` to bound total simultaneous tool work.

### Improved

- **The context-management guide now explains why compaction uses a fixed history budget.** A model's
  nominal context window does not reveal how much room remains after instructions, skills and tool
  schemas. The 48,000-character default therefore stays consistent across providers; use
  `FLUX_COMPACT_CHARS` or a per-agent setting when a known workload needs a different cap.

### Fixed

- **A broken intent or exploration response now names the stage that produced it.** Flux stops the
  turn safely and shows a bounded, redacted excerpt of the unusable response instead of reporting a
  misleading Flux-Lang field-access error.

- **A typo in `FLUX_COMPACT_CHARS` is no longer silent on served agents.** An invalid value now
  prints the same warning-and-fallback outcome as the CLI. Valid values and `0` remain quiet, and a
  per-agent compaction setting still takes precedence without reading the environment override.

- **`/compact` now tells you what happened.** It reports a successful compaction only after the
  conversation was actually rewritten, with the before/after message counts. A context that stayed
  unchanged, an explicitly disabled setting, and a cancelled check now say so directly.

### Action needed

- **Rust integrations using either `TokenCounter` trait must remove that integration.** Flux never
  wired either extension point into production. Context slicing now exposes its actual policy
  directly: a deterministic estimate of roughly four characters per token. Calls to
  `slice_context` must also omit the former counter argument.

- **Rust integrations that call `FlowEngine::maybe_compact` must handle its typed outcome.** The
  method now distinguishes disabled, unchanged, cancelled, and compacted results instead of
  returning only `()` on success.

## [0.54.4] - 2026-08-03

### Fixed

- **The Plugins and Direction guides now tell the same migration story as the roadmap.** The signed
  plugin pack remains the supported compatibility path today, while official integrations move to
  connectors only after local and hosted parity is proven.

## [0.54.3] - 2026-08-03

### Improved

- **Every official integration now has one destination.** Rich integrations such as Docker,
  Kubernetes, SQL, and Prometheus will become connectors that run locally or through Exchange. The
  existing plugin pack remains available while parity and migration work is completed.

- **Flux, connectors, and Exchange now use one set of names.** “Agent” always means a model with an
  authored loop and bounded capabilities; an API token belongs to a Service Account. The Concepts
  guide also distinguishes Programs from installed Apps, event types from deliveries, and model
  providers from identity providers and connectors.

## [0.54.2] - 2026-08-03

## [0.54.1] - 2026-08-03

### Fixed

- **`flux docs` now contains the documentation that belongs to this release.** Its embedded site is
  rebuilt from the current sources and no longer changes merely because the build ran in GitHub's
  Pages environment.

## [0.54.0] - 2026-08-03

### New

- **Generated connector event streams can now run through Flux's guarded placement.** A connector's
  declared WebSocket binding uses the same destination controls whether it runs locally or on a
  selected remote system, keeps event names to the connector's declared closed set, closes cleanly
  on cancellation, and reconnects only for failures that may recover. Credentials and raw event
  payloads stay out of logs.

## [0.53.0] - 2026-08-03

### New

- **Present Flux to your engineering team from the docs.** The new 15-minute presentation explains
  why the model is not the runtime, walks through authorization, approval, and guarded IO, and puts
  connectors and Exchange into honest local and shared deployment topologies. It is linked from the
  Flux-Lang console, works as a keyboard-driven hosted deck, and reuses the existing guarded scratch
  example when opened through local `flux docs`.

- **See what today's agent work actually accomplished.** Run `flux insights` for a factual daily
  rollup of sessions, outcomes, time, model usage, operations, errors, approvals and touched
  subjects, followed by one grounded summary. Inside the REPL or TUI, `/insights [direction]`
  reports on the current session and can focus the summary on something like blockers. Flux
  computes every fact from the durable log; the model only narrates the bounded, redacted result.

- **Run a review shaped by the project instead of a fixed checklist.** The new
  `examples/review.flux` classifies the current project from its file structure and Git history,
  derives four project-specific review dimensions from that classification alone, runs four
  read-only reviewers in parallel, and combines their evidence into one prioritized verdict.

- **Agents can run checked-in Flux workflows by path.** The `flow_run` operation now accepts a
  workspace-relative file such as `examples/review.flux`, rereads it whenever it runs, and checks
  it against the operations currently available in the session. Its result identifies the exact
  file and flow that ran, plus the input names supplied.

- **Edit and run the examples inside your local docs.** Run `flux docs` to open the exact public
  documentation shipped with your installed version. Flux code blocks and `/console/` now share a
  syntax-aware editor with live diagnostics, completion, hover, formatting, graph checks, inputs,
  output, cancellation, and approval prompts. Declared cookbook examples run in disposable scratch
  projects, and both first-app tutorial versions stay alive across browser messages. A public
  `--bind` remains documentation-only; the hosted site remains an editor with no execution backend.

### Improved

- **Agent instructions have clear ownership.** Flux now keeps its runtime protocol inside the
  harness, adds coding behavior only for coding agents, and labels repository rules and workspace
  snapshots separately. A project's `AGENTS.md`, `.agents`, or `.claude` content can specialize a
  run without accidentally becoming Flux's default prompt or granting runtime authority.

### Action needed

- **Embedded-agent prompt configuration has been split.** Rust SDK users should replace
  `.system_prompt(text)` with `.instructions(text)` and select `AgentProfile::General` for a
  non-coding persona; `Role.prompt` is now `Role.instructions`. Program-agent settings use
  `instructions` and `instruction_files`; the old `system_prompt` names fail with a migration
  message instead of silently removing Flux's harness protocol.

### Fixed

- **Default web fetches no longer parse PDFs with an unsafe dependency.** PDF bodies are detected
  and kept opaque unless the optional safe PDF extractor is enabled. Ordinary HTML and HTTP fetches
  are unchanged.

- **Flux builds on Rust 1.87 again.** Several dependency updates had started requiring newer
  compilers even though Flux still promised 1.87. The affected database, archive, document,
  terminal, and channel dependencies are back on compatible lines; no toolchain upgrade is needed.

## [0.52.1] - 2026-08-02

### Improved

- **It is now clear what Docker, Kubernetes, and microVM support means.** A new execution-placement
  matrix separates managing existing infrastructure, choosing where one effect lands, placing a
  whole worker agent, and provisioning the isolation itself. The Docker plugin has a complete setup
  and safety guide, the Kubernetes guide calls out its runtime boundary, and one deployment guide
  gives the TLS, token, persistent-workspace, readiness, container, pod, and microVM contract for a
  remote execution system. It also says plainly which pieces are bring-your-own today.

- **Remote-effect evidence identifies its source.** Tool-call and result lifecycle records now say
  whether Flux observed the work locally or received a report from a remote execution system,
  without adding workspace paths or endpoint addresses to the audit trail.

## [0.52.0] - 2026-08-02

### Action needed

- **Asterisk is no longer included in Flux's plugin pack.** Its REST interface belongs in the
  generated connector catalogue instead; live event support will follow once connector channels
  have a settled contract. Remove any locally registered Asterisk plugin and install plugin pack
  0.2.0 for the corrected set. Existing non-Asterisk plugin binaries remain wire-compatible.

- **Rust plugin authors using the removed WebSocket or direct HTTP-to-blob helpers must update.**
  The protocol and host-kit libraries move to 2.0.0 because those public APIs are gone. Guarded
  request/response HTTP, raw connections, the ordinary blob store, and capped binary responses are
  unchanged.

## [0.51.1] - 2026-08-02

### New

- **Control Asterisk through its complete REST interface, including live events and recordings.**
  The Asterisk plugin now exposes every operation in the official ARI 22.10.1 description while
  preserving its existing AMI tools. Flux keeps the PBX address and credentials on the host side,
  applies your private-network grant, opens the event WebSocket itself, and streams stored recordings
  into bounded blob storage instead of carrying large base64 values through the plugin. Invalid call
  origination, external-media and snoop combinations are rejected before any request is sent.

## [0.51.0] - 2026-08-02

### Action needed

- **Embedders implementing the guarded system port need to update two exhaustive contracts.**
  Starting a managed background process is now asynchronous so a remote target can return a live
  handle, and guarded-operation failures now include `Unknown`: the far side accepted the work but
  its terminal result cannot be proved. Treat that state as reconciliation-required; never
  automatically retry a mutation under a new identity.

### New

- **Keep the agent and approval prompt here while its effects land on another machine.** Start a
  single-workspace daemon with `flux system serve`, then pass `--remote https://…` to `flux tui` or
  another agent command. The model, credentials, approvals, sessions, and evidence stay local; the
  remote workspace is canonical and is not synchronized with your local directory. The TUI keeps
  the endpoint and remote root visible, TLS and a bearer token are mandatory, and an interrupted
  accepted mutation is reported as unknown instead of being retried. Operations that cannot yet use
  the remote port are unavailable in this mode rather than quietly running on your laptop.

- **An agent can ask you a real question without pretending it is an approval.** In the local CLI
  and TUI, agents can now show yes/no choices, single or multiple selections, and small forms, then
  continue with a checked answer. You can cancel the question explicitly, and unusual data shapes
  fall back to validated JSON. Embedded applications can supply their own question UI, including
  reviewed audio workflows; unattended and served agents do not expose the operation unless their
  host installs a responder.

## [0.50.0] - 2026-08-02

### Action needed

- **Embedded agents with automatic or custom approval now default to confinement and finite runtime
  ceilings.** If you use an embedded agent without a human approval prompt, process launches now
  require an available OS sandbox, the sandbox network starts closed, and delegated work has a finite
  concurrency and retention budget. Custom approval callbacks receive the same conservative default
  because flux cannot tell whether they prompt a person or simply allow everything.
  If an outer container or VM already supplies isolation, state that explicitly with the SDK's sandbox
  override. Likewise, set explicit resource limits when the built-in ceiling is not right for your
  workload; explicit choices still win.

### New

- **A served agent can ask you before each effect.** Start the built-in HTTP/A2A agent with
  `--remote-approval` to receive pending operations at `/approvals` and allow or deny each one from
  your client. An unanswered request is denied, every decision is tied to exactly what you reviewed,
  and captured decisions cannot be replayed. The existing `--yes` mode remains the explicit choice
  for unattended work. Remote approval currently uses one shared operator token (or an open loopback
  listener); per-user principal mode is refused until it has a separate supervisor authorization
  model.

- **HTTP credentials can be limited to where—and for whom—they work.** An `http.request` secret
  allowlist entry can now name destination hosts, the person allowed to cause its use, and whether
  it may appear in a header or query parameter. The check happens before flux reads the value and is
  repeated for redirects. Existing bare-name entries still work as before and are explicitly
  unscoped. Configure entries with `[web] allowed_secrets`; the existing
  `FLUX_WEB_SECRET_ALLOW` environment form remains available and is now documented.

- **What happens when the conversation gets long, finally written down.** A new
  [context management](https://flux.codewandler.dev/docs/agent/context-management) page answers the
  question you have been asking: what fills up, what flux does about it when it does, what it keeps,
  what it summarizes away, and what that means for the rest of the session. It also says plainly what
  the size limit actually counts and how to turn it off, and points you at the two neighbouring
  features — context packs and project context — that answer different questions.

## [0.49.0] - 2026-08-02

### Action needed

- **An agent in a shared room now answers only when it is addressed.** Until now an agent sitting in
  a room replied to every line said in it, including conversation between other people. It now
  answers when it is mentioned, whispered to, or greeted by a wake phrase.
  ⚠ **A room that relied on the agent answering everything will go quiet** until you set
  `address_rule "always"` on that channel. The agent stays silent in the room when it declines, by
  design — but flux prints one line to your terminal per distinct reason, naming the rule and the
  name it is listening for, so silence is never a mystery.
  Two things come with it: the agent keeps an attributed record of what it overheard, so an answer
  can refer to something said before it was addressed, and two agents in one room now converge
  instead of replying to each other until they hit a limit.

### New

- **Every way to run flux, on one page, with what each one costs.** A new
  [topologies](https://flux.codewandler.dev/docs/topologies) page lays out nine ways to run flux —
  entirely on your machine, OS-sandboxed, with containerized operations, with the work happening on
  another machine, as a served agent with a thin client, embedded in your own program, as portable
  wasm, on a hosted exchange, and over plain `ssh` — and answers the two questions people actually
  have about each: **where are my files**, and **where does the approval prompt appear**.
  Every row says honestly whether it ships today, ships partly, or is still proposed, so you can
  tell what you can run this afternoon from what is on the way.

- **An agent can join a meeting and hear it.** A room channel can now carry audio and video: a
  separate process owns the media while flux drives it, so text and presence keep working exactly as
  before if you do not enable it.
  ⚠ **The browser side is not shipped yet, so there is no audio in a real call in this release.** The
  seam and the protocol are in place; what is missing is the piece that actually joins the call.

- **Reaching a host by UDP or ping, under the same permission you already grant.** Network
  operations that need datagrams or a raw socket — a DNS probe, an ICMP ping — now work, and pass
  through exactly the same host permission check as everything else, so nothing reaches the network
  on a route you did not allow. If the machine lacks the privilege for a raw socket, flux says so
  before touching the network rather than failing halfway.

### Fixed

- **A room message no longer runs work as if you had typed it.** When a message in a shared room
  started a piece of work, that work ran with your own permissions — so a stranger's message could
  drive operations recorded under your name. It now runs as the person who spoke, with a stranger
  treated as untrusted, and each run records who it ran as. Questions the work pauses to ask carry
  the same identity when they resume.

## [0.48.0] - 2026-08-01

### Action needed

- **A plugin that starts asking for more than it did will now refuse to load.** flux records what a
  plugin declared the first time it runs, and treats that as the grant you agreed to. If a later
  version asks for more — a new command it may run, a new host it may reach, a new secret it may read
  — flux refuses it and tells you exactly which of those grew. This closes a gap where a plugin
  update could quietly widen its own permissions.
  To accept the new set, remove the `[grant]` table from that plugin's descriptor file (the path is
  in the message) and let the next run record it afresh. That keeps your rollback target, which
  uninstalling would discard. `flux plugin status` shows the recorded grant next to what the plugin
  currently asks for.
  ⚠ Two consequences worth knowing: the comparison is literal, so even a cosmetic change to a
  plugin's declared endpoints counts as asking for more; and if flux cannot write to its plugin
  store, loading now fails rather than continuing unrecorded.

### Action needed

- **Plugins now run sandboxed when flux is working unattended, and some will need a grant.**
  `flux plugin call`, `flux app run <program>` and `flux run <program.flux>` now start plugins inside
  the OS sandbox — the same confinement an auto-approved turn already applied. Two things change for
  a plugin that needs them:
  - **Network is closed.** A plugin that calls a vendor API (gitlab, aws, …) will fail to reach it
    until you allow it: set `[sandbox] network = true`, export `FLUX_SANDBOX_NET=1`, or pass
    `--no-sandbox`.
  - **Writes outside your workspace are refused.** A plugin that keeps state in `~/.config/<vendor>`
    will fail. The workspace, the temp directory and toolchain caches stay writable.

  flux now prints one line saying it confined the run and what it narrowed, so a failing plugin is
  not mistaken for a vendor outage. On a machine with no sandbox backend available at all, these
  commands refuse to start rather than running unconfined — that is deliberate, and `--no-sandbox`
  is the deliberate way out.

### New

- **A flux agent can join a Brave Talk room.** Declare `backend = "jaas"` on a room channel and the
  agent acquires its own guest token, joins, and keeps the token fresh across the session — no setup
  beyond the room you already use. ⚠ This shipped in **0.47.0** and was missed from that release's
  notes; it is recorded here so it is not lost. Own-tenant mode, where flux signs its own token from
  a JaaS API key, is not included yet.

## [0.47.1] - 2026-08-01

## [0.47.0] - 2026-08-01

### Action needed

- **A `flux app` program using the old positional call form will now refuse to start.** App
  journeys are checked before they run, the same way `flux flow run` already checks a flow. That
  catches a mistake before anything happens rather than halfway through — but it also means a
  journey written in the long-deprecated positional style, like `send("cli", $reply)`, now fails at
  startup instead of at the moment it runs. The error names the operation, what you passed, and the
  parameter names it accepts. The fix is the named form: `send(channel: "cli", message: $reply)`.
  Nothing that ships with flux used the old form, so this only affects your own app programs.

### Fixed

- **The terminal tells you when a pane update was dropped.** Under a burst heavy enough to fill the
  display queue, commands from the agent used to be discarded with no sign — so a pane the agent
  believed it had opened simply was not there. You now get one notice saying how many were dropped
  in that frame, instead of silence.

## [0.46.0] - 2026-08-01

### Improved

- **Approval prompts now tell you which vendor a connector operation actually reaches.** When a
  connector platform makes the call on flux's behalf, flux itself only ever talks to the platform —
  so until now the approval could not tell you where the request really went. It now shows the
  vendor, checked against the connector's own declared host list and re-checked whenever the
  connector reloads. A connector cannot quietly change or drop that answer after you have approved
  it. This is disclosure, not a guarantee: a connector that names one destination and contacts
  another is beyond what flux can see, so it does not replace trusting the deployment you started.
  ⚠ If you run a **narrowly scoped network policy**, a connector operation that declares no
  destination will now be refused where it previously ran. That is deliberate — it fails closed —
  but you may need to widen the grant or ask the connector author to declare its destination.

### Fixed

- **Deeply nested code no longer crashes the tools that read it.** A Flux program with very deeply
  nested blocks used to take the whole process down — the editor tooling and the formatter included —
  instead of reporting an error. It now returns an ordinary "nesting too deep" message and keeps
  going, and your source text is preserved exactly as written even in the part it refused. Nothing
  you would plausibly write is affected: the limit sits about twenty-five times deeper than the most
  deeply nested example that ships with flux.

### Improved

- **A connector deployment can no longer hand flux one of your vendor credentials.** When a
  connector platform answers an operation, flux now checks the response for credential material
  before anything else looks at it — and if it finds any, it throws the whole response away instead
  of masking it and passing it on. The same check covers error responses, and a sign-in response must
  be an ordinary authorization link you could click yourself, with no secret embedded in it. A
  connector cannot quietly drop this protection once it is running. This is a floor rather than a
  guarantee: a platform determined to smuggle a credential past it — by encoding it, or splitting it
  across fields — still can, so it does not replace trusting the deployment you started.

## [0.45.0] - 2026-08-01

### Improved

- **The documentation is more accurate and easier to navigate.** The website now explains the
  authored agent loop, approvals, channels, plugins, credentials, recovery, SDK, and Flux-Lang using
  the behavior and syntax that ship today. Installation steps and examples are runnable, limitations
  are stated where they matter, and the sidebar is organized around common tasks. Automated checks
  now catch stale channel lists, conflicting runtime explanations, noncanonical Flux examples,
  missing pages, and broken documentation links before they are published.

- **The terminal interface is easier to read and navigate in long sessions.** Selection remains visible
  without color, narrow panes hide nonessential previews before they crowd out the conversation, and
  the composer, scrollbar, queued-message count, turn separators, and cancelled actions all have clear
  visual cues. You can jump between failed actions with Ctrl-G, and a new empty session tells you when
  older sessions are available to resume.

### Fixed

- **Security: a webhook channel now checks who is calling before it reads what they sent.** If you
  expose a webhook, flux authenticates the request first and only then decodes the body — and it
  checks the signature against the exact bytes the sender signed, not a reformatted copy. Previously
  a webhook channel had no way to verify a signature at all.

- **Security: a credential that is only digits is now hidden in saved diagnostics too.** Several
  places that scan saved data for registered credentials skipped plain numbers, and two of them also
  skipped the *names* of fields — both write to storage that persists. All of them now look
  everywhere. Ordinary numbers are untouched.

- **A redaction that cannot be completed no longer hands back the original.** If hiding a credential
  left a recorded request unreadable, the fallback returned the *unredacted* value. It now refuses
  and stores a redaction marker instead.

- **The limits you configure are now observed end to end when you run an app**, and a test that
  claimed to check per-helper limits now says what it actually checks.

### Action needed

- **A webhook that listens beyond your own machine must now state its verification.** If it has a
  token and no verification, add the line `verify "none"` to keep today's behaviour. flux refuses to
  start otherwise, names the channel, and prints the exact line to add — rather than opening a port
  whose protection you might have assumed. Webhooks that only listen on your own machine are
  unaffected.

## [0.44.0] - 2026-07-31

## [0.43.0] - 2026-07-31

### New

- **flux can now take deliveries from a connector you have installed, by naming it.** Point a channel
  at an installed connector and the binding you want, and flux reads that connector's own description
  of itself and sets the channel up from it — you do not restate the vendor's details by hand.
  The part worth knowing is what happens when something in that description is wrong or has been
  edited since it was published: flux refuses to start the channel and tells you which rule failed
  and where, instead of starting a channel that looks healthy and quietly does nothing. That was a
  real failure mode — a description that narrowed which events it wanted would previously have opened
  a port, accepted deliveries, and never acted on any of them.
  Two limits today. Connectors that sign their deliveries are not usable yet, and flux refuses them
  outright rather than accepting the delivery unverified — so you cannot end up believing a channel
  checks signatures when it does not. And if you expose the channel beyond your own machine, it must
  have a token; an empty one is now rejected when you set it up, because an empty token would have let
  anyone through.

- **Web requests can now take query parameters as a proper list, instead of you gluing them onto the
  address yourself.** Give the request a set of named values and flux assembles the address for you,
  escaping each value correctly. This matters for more than tidiness: previously, a value that
  happened to contain a character like `&` or `=` — a search phrase, a customer's name, anything the
  agent picked up along the way — could silently turn into *extra* parameters and change what the
  request actually asked for. That can no longer happen.
  Values behave the way you would expect: leaving one empty means "don't send it", while `false` and
  `0` are sent as real values, and naming the same parameter twice is reported as a mistake rather
  than one quietly overwriting the other. A stored credential can be used as a query value and stays
  hidden in logs and in anything the model can see, exactly as it does in a request header.

- **Panes the agent opens now actually appear in the terminal UI.** When a flow asks flux to open a
  side pane — to show a file, a diff, a result alongside the conversation — that pane reaches your
  screen. Previously the request was accepted and then went nowhere, and the only symptom was
  silence: no error, no pane, nothing to suggest anything had gone wrong.

- **You can pick up a plugin's updated set of actions without restarting flux.** Run
  `flux plugin refresh <name>` and flux re-reads what that plugin offers and updates what your agent
  can call. Every check that runs when a plugin is first installed runs again here — this is not a
  shortcut around them.
  The part worth knowing is what it will *not* do. A refresh can only ever narrow what a plugin is
  allowed to reach, never widen it: if the updated description asks for anything beyond what you
  granted at install, flux refuses the whole refresh and leaves the plugin exactly as it was. It also
  refuses if the update would take over the name of an action belonging to a different plugin. In
  both cases nothing is half-applied — you get the old state back, intact, and a message saying which
  rule stopped it.
  One limit today: a session that is already running keeps the set of actions it started with. The
  refresh takes effect for the next one.

- **A web request now hands back its answer in parts you can pick from, instead of one block of
  text.** You get the status, the response headers and the body separately, so a flow can reach
  straight into the body for the piece it wants — an id, a status field, an item from a list. Before
  this, everything arrived glued into a single blob and asking for a field inside it simply gave you
  nothing back, with no error to tell you the ask had not worked.
  Bodies that are not JSON are handled the way you would hope: an HTML error page, an empty reply or
  a cut-off response still come back with the status and headers intact, and the body as plain text.
  A `404` is an answer, not a crash. What a person sees in the log is unchanged, and credentials that
  an API echoes back to you — in a header or in the body — are still hidden.

### Fixed

- **Security: a credential made only of digits is now hidden everywhere, including where it was
  slipping through.** flux hides credentials you have registered wherever they appear. But a
  credential that is only digits — an account id, a numeric key — cannot be recognised by any of the
  clues flux uses for the others, so registering it is its *only* protection. Four places that scan
  saved data for credentials were skipping over plain numbers, and two of them were also skipping the
  *names* of fields — and both of those write to storage that persists. All four now look everywhere.
  Ordinary numbers are untouched: ports, timeouts, counts and ids are only affected if you registered
  that exact value as a secret.
  **You may need to act:** if a web response contains a registered credential as a number, that one
  field now comes back as the text `[redacted]` instead of a number. Only that field changes, and its
  real value was never usable to you anyway.

- **Flux files stop showing false syntax errors in Helix, Neovim and Zed.** If you write a duration
  the normal way — `500ms`, `10s`, `1m` — your editor was marking it as a mistake, even though it is
  correct and is the spelling flux itself recommends. Writing the same value as a plain number
  (`60000`) looked fine, which made it seem like the shorter form was wrong. It was not; the editor
  support was.
  To pick this up you need to update the Flux grammar your editor uses — re-run
  `scripts/install-helix.sh` from the flux-tree-sitter repository, or re-install the parser in
  Neovim/Zed. Doing so also gets you two earlier improvements that had been finished but never
  reached anyone.

- **Security: an agent-to-agent channel with a blank token opened a door to the whole machine.** If
  you set up an `a2a` channel and its token resolved to nothing — most easily by pointing it at an
  environment variable that was exported empty — flux would open that channel **to the network** and
  accept every request, including requests carrying no credentials at all. Because flux approves the
  agent's actions automatically, that amounted to handing control of the machine to anyone who could
  reach the port. flux was supposed to refuse to open an unauthenticated channel to the network; a
  blank token slipped past that check because it did not *look* unauthenticated.
  It is now refused in three independent places: the channel will not start, the port will not open,
  and no request is accepted. **You may need to act:** an `a2a` channel with a blank token now fails
  to start with a message naming the channel, instead of running exposed.

- **Security: several kinds of credential that flux used to let through are now hidden.** flux hides
  secrets from logs, from what the model can see, and from saved transcripts. A measurement found six
  kinds it was missing — among them AWS secret keys, passwords embedded in database connection
  addresses, Stripe live keys, and private keys pasted in full. All six are now caught.
  Two of those matter more than the rest because of *where* they show up: Stripe live keys and private
  keys turn up exactly where an agent writes production configuration.
  flux deliberately still errs on the side of showing too much rather than too little. Blanking out
  anything that merely looks secret would eat your own configuration, commit hashes and screenshots —
  so the new rules only fire on strong evidence. What is still not caught is written down rather than
  glossed over.
  **One change you may notice:** if you declare a secret whose value is too short to protect, flux now
  refuses to run instead of quietly continuing without protecting it. A promise it cannot keep is now
  an error you can see.

- **Security: a webhook channel whose token was empty let everyone in — including callers who sent
  no credentials at all.** If you set a token on a webhook channel and the value came out empty —
  most easily by pointing it at an environment variable that was exported empty or never given a
  value — the channel accepted every request that reached it, whether or not it carried any
  credential. It looked authenticated and was not.
  flux now refuses to start such a channel and tells you which channel and what is wrong, rather
  than opening the port. It refuses even when the channel only listens on your own machine, because
  otherwise the mistake stays invisible until the day you expose it. A token that is only spaces
  counts as empty.
  **You may need to act:** if a webhook channel of yours currently has an empty or blank token, it
  will now fail to start instead of running. That is deliberate — it was an open door — but it is a
  startup failure, so set a real token before you upgrade.

- **The limits you configure now actually apply when you run an app, and to the helpers it spawns.**
  If you had set ceilings on how much work flux may do at once, two paths quietly ignored them:
  running an app, and the reviewer helpers that an app's strict review starts. Those helpers ran with
  no ceiling at all. Both now respect your settings, as do `flux review` and `flux record`. Each
  helper gets its own budget rather than competing for a shared one, so a busy helper cannot starve
  the others.
  One deliberate exception: replaying a recorded test still ignores your limits. A replay is meant to
  give the same answer on every machine, and letting local settings affect it would make the same
  test pass on one computer and fail on another.

### Action needed

- **If a flow of yours uses the result of a web request as text, it needs updating.** The result is
  now made of parts rather than one block, so somewhere you were using the whole answer as a string,
  use `.body` for the content, `.status` for the code and `.headers` for the headers.
  One rough edge to know about: most header names contain a hyphen — `content-type`, `x-request-id` —
  and you cannot yet write `.headers.content-type`. Until that is fixed, reach those with
  `pick({items: $resp.headers, keys: ["content-type"]})`.

## [0.42.0] - 2026-07-31

### New

- **flux can now join a real chat room, with no browser and no vendor software.** Point it at any
  standards-compliant chat server — including a hosted meeting service — and it joins as a visible
  participant: it sees who is present, notices people arriving and leaving, can speak to the room, and
  can send a private message to one person. Everything it hears is attributed, so it knows who said
  what.
  Two things to know before you use it. **It replies to every message in the room**, because the rule
  for "only answer when you are spoken to" is not in place yet — so put it in a room deliberately
  rather than leaving it running in one. And it handles text and presence only; audio and video are not
  part of this.
- **Search your own past sessions from other coding tools.** If you use Codex, Claude Code or
  OpenCode, their transcripts are already on your disk. flux can now search them — the actual
  conversation text, not just the token counts those tools record.
  **It is off unless you turn it on, and it is contained before it is searchable.** Sensitive values
  are removed and the text is neutralised *when it is indexed*, not when it is displayed, so nothing
  is sitting in the index waiting to leak. You grant access per tool, not all at once. Nothing in flux
  switches this on for you yet.
- **A compact notation for showing a flux program to a model.** Programs can be rendered in a short
  indented form that costs far fewer tokens than the full source, and converted back exactly. Your
  `.flux` files and how they run are completely unchanged — this is an extra way of *displaying* a
  program, not a new way of writing one.
- **An agent can open its own panes in the terminal.** The building blocks for an agent to open,
  update and close a pane are in place. Nothing installs them yet, so you will not see a change in
  this release.

### Improved

- **Resource limits now apply to the actual `flux` command, and to the sub-agents it delegates to.**
  Previously a configured limit only bound if you embedded flux as a library, so the setting quietly
  did nothing for people running the binary — and delegated work was unbounded either way.
  Note what the concurrency limit means: it applies **per agent**, not to the whole process. If you set
  it to 4 and flux delegates to 3 sub-agents, you may see up to 16 tool calls running at once. We tried
  the whole-process version first; it can deadlock, because a parent waiting on a child can be holding
  the very slot the child needs.
  Not yet covered: `flux app run` still ignores these settings.

### Fixed

- **Old memories could have been deleted by routine cleanup.** Cross-session memories were
  indistinguishable from short-lived scratch data to the cleanup pass, so an untouched memory could
  have been removed for being old. Memories are now excluded, and "no activity in months" is treated as
  the knowledge having settled rather than as it being disposable. **Nothing was actually lost** — the
  cleanup had no caller yet, so this closes a trap before it could bite anyone.

- **Rooms: flux can now sit in a conversation with several people at once.** Until now every way of
  talking to flux was one-to-one — a phone call with a single caller, a webhook, a chat message that
  wakes it and gets a reply. A new `room` channel type lets flux be *one participant among several*: it
  can see who is present, watch people join and leave, speak to the room, and send a private message to
  one person. Crucially, every message it receives now says **who** said it, which is the thing that was
  missing before.
  This is the groundwork rather than the finished feature. There is no real room backend yet — only an
  in-process test double — so you cannot point it at a live meeting today; that comes next. And for now
  the agent replies to **every** message in a room rather than only when addressed, so the "answer only
  when spoken to" rule is not in place yet either.

### Action needed

- **If you have written your own voice handler, its `turn` method now takes a speaker.** Anything
  implementing the voice turn interface needs to accept a speaker argument alongside the text. If you
  only use flux's built-in phone/voice support, nothing changes — the single caller on a line is simply
  named now instead of anonymous.

## [0.41.1] - 2026-07-31

### Fixed

- **The Zendesk example's setup instructions no longer describe software that does not exist.** The
  guide and the examples README told you to build and install a `zendesk` plugin, then store a token
  through the credential prompt. That plugin was withdrawn before it was ever released, so those steps
  could not work. The same four workflows are now served by the connectors library instead — under the
  same operation names, so the example itself is unchanged — and the documentation explains how a host
  makes them available. It also states plainly what is still missing before the workflows can reach a
  real Zendesk account: the Zendesk connector has nowhere to look a stored credential up, and its
  account-specific address (`https://your-company.zendesk.com`) is not yet filled in from
  configuration. Both stop with an explanation rather than sending a broken request, and neither is
  something you can work around by supplying a token.
- **A clearer statement of what "read-only" means for that example.** Making the Zendesk operations
  available brings the three writing operations along with the four reading ones. None of the four
  example workflows can reach a write — that is now checked automatically — but keeping writes out of
  reach entirely is a matter of what you approve, and the guide no longer suggests otherwise.

### New

- **Your flows can now call APIs that want a form-encoded body — including OAuth2 token endpoints.**
  Writing `parse($fields, as: "form")` turns a record into the `key=value&key=value` body those APIs
  require, with every value escaped properly. Previously the only body a flow could build was JSON, and
  hand-assembling a form body with text formatting silently corrupted any value containing `&` or `=`.
  A field you leave empty is simply not sent, and a nested field is refused with an explanation rather
  than guessed at — because every service spells nested form fields differently, and a wrong guess is
  accepted and quietly ignored.

## [0.41.0] - 2026-07-31

### New

- **You can see what your sub-agents are doing, in the terminal, while they work.** Delegated workers now
  appear in their own panel: what each one is, whether it is working or has gone quiet, and how long it
  has been going. What a worker is *saying* or *thinking* is never shown. The panel belongs to flux, not
  to the agent — an agent cannot close it, redraw it, or put anything that looks like it on screen.

- **If you embed flux, you can now cap how much a run uses, not just how much it spends.** Alongside the
  existing token and iteration budgets, you can set a ceiling on how many tools run at once, and a
  ceiling on how much tool output a long session keeps in memory. Going over is refused with a message
  that names the setting — never a silent truncation and never a hang.
  Two honest limits: there is no whole-process memory cap, because a library cannot refuse an allocation
  someone else makes, and these ceilings do not yet apply to the `flux` command line or descend into
  sub-agents.

- **A long-running session can be told to stop growing its audit log without losing any of it.** When the
  limit is reached, the oldest *contents* are set aside behind a note saying what was there and where to
  find it — the record of what happened, in what order, stays complete. Nothing is quietly deleted.

### Improved

- **Workflow control lines read like calls.** `confirm "Open issue?", risk: medium`,
  `retry 3, backoff: exponential, delay: 500ms`, `loop for 10s, every: 1s, until: done`. **Everything you
  have already written still works** — this is a new spelling, not a replacement, and the formatter emits
  the new one. Editor highlighting is updated everywhere: the website, VS Code, IntelliJ, and the
  tree-sitter grammar Helix, Neovim and Zed use.

- **An agent's panel can no longer be made to look like the approval prompt.** The prompt that asks you to
  allow or deny an action is drawn only by flux, and an agent's own panel now cannot reproduce it — not
  with colours, not with terminal codes, and not by drawing the box out of ordinary line characters.
  Worth knowing what this does **not** promise: plain text can always resemble a frame if it tries. What
  it guarantees is that no accurate copy is possible, and that the `◆ agent` mark on a panel is never
  something an agent can put there itself. **If you are being asked to approve something, look for the
  absence of that mark.**
  One cost: an agent panel can no longer draw diagrams, tables or progress bars out of line characters
  in free text. It can still ask flux to draw all three properly.

- **`flux eval` now points you at flux-bench**, which is the supported way to benchmark a coding-agent
  harness. `flux eval` itself is unchanged and still works.

### Fixed

- **A confined flux can no longer be fooled into thinking it is already confined.** The marker that tells
  a child process "you are inside a sandbox already" could previously be set by the code doing the
  spawning, even when no sandbox existed — so a child could skip its own sandboxing believing someone
  else had done it. It can now be neither forged nor cleared.

- **A busy machine no longer fails the build for no reason.** Two kinds of test read state from your home
  directory, so running several things at once could fail a test that had nothing to do with the change
  being tested — and the failure looked like a compile error.

## [0.40.0] - 2026-07-30

### Improved

- **A confined flux now confines what it starts.** When you run flux with OS sandboxing on, the
  processes it launches — including other flux processes, such as fleet workers — now inherit that
  confinement instead of quietly running unconfined. Previously a child was told *"you are already
  confined"* without being told **with what**, so it declined to confine its own descendants even
  when you had demanded sandboxing. The setting only ever travels downward: a child can be stricter
  than its parent, never looser, and a parent that is not sandboxing imposes nothing.

### Fixed

- **A "what if" simulation can no longer leave anything behind.** Asking flux what *would* have
  happened sometimes created the counterfactual session before it decided it could not answer, so a
  refused simulation could leave a stray session in your history. A refusal now leaves nothing at
  all, and that is guaranteed by construction rather than by the order the checks happen to run in.

- **Sandboxing is now proven on every build, in both directions.** Flux's continuous integration
  exercises both the case where the operating system offers no sandbox and the case where a real one
  is present. Until now the confined path was tested on no machine at all, so a regression in it
  could reach a release unnoticed.

### New

- **Flux's built-in operations and language nodes now have one machine-readable catalogue.** Run
  `flux catalog core --format json` to export the current foundational tool contracts, language-node
  schemas, and stable specification links without running a tool or contacting the network. The
  export distinguishes things you can call from language structure and future protocol support:
  HTTP is available, while DNS, TCP, UDP, and ICMP are clearly marked planned and non-callable.

- **A coordinating workflow can now start its own workers, so it can work several tasks at once.**
  Previously one worker served one task at a time, which capped a coordinated run at a single item
  regardless of how much work was waiting. A workflow can now start a worker for a task, check whether it
  is still alive, and stop it — each worker in its own private checkout. Depth and count are bounded by
  default (one generation, sixteen workers) and a worker cannot raise its own limit.
  **Two things you may need to set.** A coordinator that starts workers needs sandbox network access
  enabled, because a network-isolated worker cannot be reached — flux refuses to start one rather than
  hand you a worker that looks alive and is not. And dispatching to a worker needs private-network access
  allowed, because the worker answers on a local address and flux does not trust local addresses
  implicitly. Both refusals say what to set.

- **A coordinating workflow can give each task its own private checkout.** Previously a workflow could
  move itself into an isolated checkout, but only one at a time — so it could not hand several workers a
  workspace each in the same run. Now it can create one per task, off a clean starting point, without
  disturbing its own working directory. Each is refused up front if the starting point is dirty or the
  name is taken, rather than half-created. **Removing them afterwards is your workflow's job**: they are
  kept on purpose because they hold work that has not been merged yet, and nothing cleans them up for
  you — so a long-running coordinator should delete the ones it is finished with, or it will fill the disk.

### Action needed

- **If you embed flux and re-run a recorded turn, one function's arguments have changed.** Re-running
  a pinned turn now takes a resolved selection rather than a session name and turn number, so the
  refusals that used to happen part-way through are settled before anything is created. If you build
  against the flux libraries you will get a compile error pointing at the call; if you only run the
  `flux` command line, nothing changes for you.

## [0.39.0] - 2026-07-30

### Improved

- **Workflow source is shorter and easier to scan.** Locals no longer need `$` on every use, named
  operation inputs no longer need an extra object wrapper, standalone calls use the same `op(...)`
  spelling as bound calls, list indexes use `[0]`, and durations can use `ms`, `s`, or `m`. Existing
  source spellings remain accepted, and both normal and extra-compact formatting are valid source.

### Action needed

- **A `.flux` file must now contain Flux-Lang text.** Files containing only a JSON workflow AST are
  no longer detected by their first character. Convert them to native source; JSON AST values remain
  supported through explicit APIs and JSON tooling.

## [0.38.0] - 2026-07-30

### New

- **You can now run one named workflow out of a file that defines several.** `flux run <file>
  --entry <name>` picks a single top-level workflow by name, with the same strict input checking and
  safety path a single-workflow file already gets. One checked-in file can hold a related family of
  workflows instead of needing one file each.

- **Servers can now enforce per-caller admission and completed-usage limits.** REST, webhook, and
  agent-to-agent requests share limits for request rate and simultaneous work. Completed provider
  calls and priced spend act as circuit breakers: new work is rejected after a threshold is
  observed, while already-running work can overshoot only within the simultaneous-work bound.
  Long-lived streams keep their slot until they really finish. Single-process limits are built in;
  multi-server deployments can apply the same policy at their shared proxy or control plane.

- **Release downloads can be verified against GitHub's signed provenance.** The installation guide
  now starts with a version-pinned download and an attestation check bound to the tag's exact commit.
  The release verifier rejects unknown assets and checks every supported executable asset. Release
  jobs also verify the exact tooling they run instead of executing an installer directly from the
  network.

- **Release notes can be drafted by a model without letting it decide the release.** A flux program can
  now read a project's commit history, have a model write both the engineering and the customer-facing
  changelog prose, and run the release — while the version number itself is worked out from the commit
  titles, not from anything the model says. A model asking for a different version cannot change it. If
  the run is interrupted or a check fails, nothing is left half-written and no version is tagged.
  Publishing still requires a person to start it.

- **You can see what delegated helpers are doing while they work.** When flux hands work to helper
  agents, a status line now shows each one's role, how long since it last did anything, and whether it
  is working or stuck. Previously a delegated run was silent until it finished, so a wedged helper
  looked exactly like a busy one. Their inputs and outputs are never shown, only that activity
  happened — so a helper handling secrets cannot leak them onto your screen. This currently covers
  helpers running on your own machine; remote workers stay silent for now.

- **Work boards can now hand an item to a different worker, and record what a run produced.** If the
  worker holding a task dies, you can reassign the task to someone else — previously nothing could move
  it, because only the current holder was allowed to claim it. And a finished piece of work can now
  attach evidence to its item, such as a commit or a pull request link, so you can see what a run
  actually produced instead of just that it finished.

### Fixed

- **Server timeouts now stop work cleanly before returning.** Buffered REST, webhook, and blocking
  agent-to-agent requests cancel and finish their owning turn before a timeout response is sent.
  Provider call limits also count calls that report no token usage and keep overlapping callers'
  usage assigned to the correct caller.

- **Built-in code review stays read-only even in an untrusted project.** Project role files can no
  longer replace Flux's embedded reviewers with tool-using agents, and `flux review` now requires the
  same fail-closed operating-system confinement as other internally approved unattended commands.

- **Approved network destinations stay the destinations actually contacted.** Remote helper calls
  and plugin HTTP, sign-in refresh, and TCP callbacks now connect to the exact addresses that passed
  Flux's private-network check. A hostname cannot pass the check with one address and silently
  resolve to a private address a moment later; redirects and ambient proxy settings cannot bypass
  the boundary. Native web requests follow the same direct-connection rule. A plugin socket grant
  for one directory can no longer escape it through a wildcard and `..` path.

- **Closing a streaming HTTP response now stops its work.** A disconnected client no longer leaves
  model or tool work running in the background, and a slow reader cannot accumulate an unlimited
  event queue. Cancelled streams still leave a session that can be resumed safely.

- **Machine consumers can distinguish a failed turn from a successful answer.** A provider failure
  before execution now returns a failure exit status and a structured stream error instead of being
  wrapped as ordinary answer text.

- **Evaluation runs no longer accept model-chosen executable, benchmark-driver, dataset, import, or
  rebuild controls—or a bundle of unrelated provider secrets.** The operator owns those selectors,
  benchmark children follow the normal sandbox rules, and each provider receives only the
  credentials it needs.

- **A retried task no longer looks like it is still running on the machine that dropped it.** When a
  task failed and went back to the queue, it kept pointing at the run that had died, so status could
  report progress on a process that no longer existed. A retried task now forgets the dead run while
  staying assigned to whoever owned it.
- **Repeatedly blocking a task no longer resets its retry budget.** A task that cycled between blocked
  and ready could do so indefinitely, because only outright failures counted against its retry
  allowance. Unblocking now counts too, so a task that keeps stalling is eventually surfaced instead of
  looping forever.

### Action needed

- **Unattended and server operation now requires a working OS sandbox by default.** Auto-approved
  commands and serving modes refuse to start on a host where Flux cannot establish confinement, and
  sandboxed child processes start without network access. If you deliberately provide equivalent
  isolation with a container or VM, pass `--no-sandbox` or set exactly `FLUX_SANDBOX=off`; Flux will
  print a prominent unconfined warning. Interactive local use keeps its previous behavior.

## [0.37.0] - 2026-07-30

### New

- Flows can now create a branch, merge it, and undo a merge. Together with the existing stage, commit
  and diff steps, a flow can integrate work end to end: branch, merge with a real merge commit, and —
  if something turns out to be wrong — undo that merge. Undoing always *adds* a commit that reverses
  the change; it never rewrites or discards history, so the record of what happened stays intact.

- Work boards can now be read as structured data, not just as text. A flow can loop over board items
  and branch on their state, filter to the items that are ready *and* not blocked by unfinished
  dependencies in a single call, and read back the notes it left on an item. This is what lets a flow
  coordinate work across a board instead of only reporting on it.

### Fixed

- Starting two flux processes at the same time against a brand-new event store no longer risks one of
  them failing. Previously, if nothing had created the store yet — a fresh machine, a fresh CI job, or
  a server and a command starting together — the two could collide while setting it up and one would
  error out. They now take turns.

- Pulling a value out of text and feeding it straight into another step now works. Extracting a
  single match (or taking the first, last, or first-non-empty value) used to hand back the text
  wrapped in quote characters, so the next step received something like `"1.2.3"` instead of `1.2.3`
  and typically failed on it.

### Action needed

- **If a flow of yours calls `git_revert`, rename that call to `git_reset`.** The step that discarded
  your changes and returned the checkout to a snapshot was named `git_revert`, which described the
  wrong thing — it resets. It is now called `git_reset`, and the name `git_revert` belongs to a new
  step that undoes a commit by adding a reversing commit instead. There is no alias, so the old name
  will not be recognised. If you miss one it fails safely rather than destructively: the new
  `git_revert` refuses a snapshot argument instead of resetting anything.

- If you have a flow that worked around the quoting above — for example by stripping quote characters
  from an extracted value before using it — remove that workaround, or it will now strip real
  characters. Flows that simply passed the value along need no change and start working.

- Forking a session that can't be forked no longer leaves a stray empty session behind. Previously
  the new session was created before flux checked whether the original could be forked at all, so a
  refused fork still cluttered your session list. It now checks first and creates nothing on refusal.

## [0.36.0] - 2026-07-29

### New

- **Hand work to other flux agents, and pick it back up after a restart.** A program can now declare
  a **work board** — a list of tasks with real states (ready, claimed, in progress, blocked, review,
  done, failed) — and hand items out to remote flux workers without waiting for them. Boards now use
  a first-class `board` declaration with explicit scope, profile, backend kind, and root; the former
  datasource-shaped spelling has been retired.

  That gives your program operations to list, read, create, claim, move, comment on and dispatch
  board items. The `markdown` board backend keeps one file per item on disk, so **the board survives the process
  that wrote it** — which is the point. When work is handed to a worker, the worker's address and the
  task handle are written back onto the item itself, so a coordinator that is restarted can read the
  board and find every run that was in flight, then poll or cancel it. There is no second place where
  run state lives and no state file to reconcile: restart, re-read the board, carry on.

  The `memory` backend is also available for a single run and for tests, but it cannot outlive its
  process, so anything relying on recovery wants `markdown`.

  Handing out work is gated like any other outbound request: a worker on a private or loopback
  address is refused unless you allow it, each dispatch is approved against **that specific worker's
  address** rather than a blanket permission, and a worker that cannot be named is always sent to
  approval rather than matching an existing grant.

### Action needed

- If you have written your own work-board backend against the library, it must now also record which
  worker is running an item. A backend that quietly skipped this would look healthy until a restart
  recovered nothing, so it is required rather than optional.

### Fixed

- **Busy apps no longer start unlimited work at once.** The previous release made app event handling
  concurrent but left it uncapped, so a burst of incoming events — a webhook storm, say — could start
  a piece of work for every one of them. There is now a limit on how much runs at a time (64 by
  default, or set `FLUX_MAX_INFLIGHT_DELIVERIES`). Work over the limit **waits rather than being
  dropped**, so everything still runs; a slow background job still cannot starve incoming events; and
  work that is queued behind the limit is now reported differently from work that is merely slow, so
  "busy" and "stuck" no longer look the same.

## [0.35.0] - 2026-07-29

### New

- **See what a permission change would have done, before you adopt it.** `flux policy simulate
  proposed.toml` replays your recorded history against a proposed set of permissions and shows what
  it would newly block, newly allow, and leave alone — so changing what flux is allowed to do stops
  being a guess. Add `--json` to feed it to other tooling. It only reads: nothing is recorded and no
  model is called. Where your history does not contain enough detail to re-decide an action, it says
  so explicitly instead of guessing, because a confident wrong answer would be worse than an honest
  gap.

- **flux can now stage just its own changes in a file you are also editing.** Previously, if the
  agent and you had both edited the same file, staging was all-or-nothing: it either swept your
  unfinished edits into its commit or gave the job back to you. It can now list a file's individual
  changes and stage only the ones it made, leaving yours untouched in your working copy — the
  equivalent of picking changes by hand, without the prompting. If the file moves underneath a
  selection, it refuses and asks for a fresh look rather than staging the wrong lines, and it will
  not quietly stage a whole-file deletion when you asked for one change.

- **Hand work to a flux agent running somewhere else.** flux can now delegate a task to a remote
  flux worker over the network instead of running it in-process, and the everyday way you delegate
  work is unchanged — the same delegation step now simply works against a remote worker. For work you
  want to start and check on later rather than wait for, there are three new operations: send the
  work and get a task id back, ask how it is going, and cancel it. Cancelling genuinely stops the
  remote run; previously a remote run could only be walked away from. Remote worker addresses are
  checked against your network rules before any request is made, and the worker you are dispatching
  to is named on the approval prompt rather than a blanket wildcard.

### Improved

- **Operations that call a model are now labelled as costing something.** Fetching a web page and
  asking a model a question were both labelled "low risk", which made a billable call look as
  harmless as reading a file — and that label is what flux shows you in the summary you approve. Ops
  that spend money (second opinions, the built-in AI helpers, intent detection) are now labelled
  medium, and genuine page fetches are correctly marked as reads. This adds no new approval prompts;
  it makes the description you are shown before approving match what will actually happen.

- **Page fetches that also save the page now wait to be shown to you first.** When flux is set up to
  keep what it reads — fetched and crawled pages are saved as searchable knowledge you can ground
  later answers on — those operations do more than read, so they no longer run during the quick
  look-around flux does before showing you a plan. They appear in the plan instead. There is no new
  prompt and nothing you can no longer do; an affected turn may simply take one extra step. Page
  fetching without that saving behaviour is unchanged.

- **flux now tells you when an integration understates what its operations do.** Every operation
  declares how risky it is and whether it only reads — and that declaration is what decides whether
  you get asked before it runs. Nothing checked those declarations against each other, so an
  operation that quietly gained the ability to change something could keep a "harmless" label and
  skip the approval it deserved. A dozen built-in operations had drifted that way and are corrected,
  and the check now runs on every build so it cannot drift again. Integrations you install are
  checked when they load: they still work, but flux names how many of their operations understate
  themselves so you can weigh that when approving.

### Action needed

- **A slow job no longer blocks everything else in an app.** Until now, an app handled one incoming
  event at a time: if a scheduled job ran for a minute, every webhook, chat message and API call
  that arrived during that minute waited for it. They now run alongside each other, so a long job
  and a busy inbox no longer compete. Two things to know if you rely on the old behaviour: work that
  touches the same conversation can now interleave rather than queue, and there is currently no
  cap on how many events run at once, so a large burst starts a large amount of work.

- **If you embed flux in your own Rust program, two conversation-writing methods are gone.** The
  event store used to let you append a conversation message directly, with nothing checking that the
  result was a conversation a model would accept. Those two shortcuts have been removed rather than
  deprecated — leaving them next to the checked path is how the broken sessions kept happening. Use
  the session-log handle instead; the underlying append is still available if you genuinely need to
  write an unusual shape.

### Fixed

- **Forking a session no longer hands you a copy that fails immediately.** If you forked a session
  whose last action was still waiting on a result, the fork copied that half-finished state into the
  new session, and the new session then failed on its very first message. Forking now refuses up
  front and tells you why, instead of giving you a session that was already broken.

- **Project guidance files in subfolders no longer load.** `.flux/context.d/` is documented as a flat
  folder — the whole point being that one look tells you everything that can reach the agent. It was
  in fact being read recursively, so a file tucked in a subfolder quietly loaded too. It now behaves
  as documented. If you had guidance nested in a subfolder and want to keep it, move the file up into
  `.flux/context.d/` itself.

- **Kubernetes operations now work when your kubeconfig lives somewhere other than the default
  location.** If you point `KUBECONFIG` at a custom path, flux used to notice it, offer you the full
  set of Kubernetes operations — and then fail every one of them, because the value never reached
  `kubectl`. It now does, so the operations you are offered are the operations that work. You no
  longer need the old workaround of merging or symlinking your config into `~/.kube/config`.

## [0.34.0] - 2026-07-29

### Action needed


- **Querying a SQLite database now accepts only read statements.** The check that was supposed to
  keep these queries read-only listed the statements to reject, and missed some — including one that
  could write a file anywhere on disk, outside every path guard. It now works the other way round:
  a query must begin with `SELECT`, `WITH`, `PRAGMA` or `EXPLAIN`, and anything else is refused.
  Comments and whitespace in front of the statement no longer disguise it. If you have a flow that
  relied on another statement type, it will now be refused rather than silently allowed.
- **If you embed flux in your own Rust program, the HTTP server is built differently.** The two
  functions that build the server now take the address you intend to serve on and can fail. This is
  what makes the next item hold for every caller, not just the usual one — pass the address you bind
  to and handle the result.

### Improved


- **An unauthenticated server can no longer be exposed beyond your own machine.** flux already
  refused to start an unauthenticated server on a public address, but the refusal lived in one entry
  point, so a program that assembled the server a different way inherited no protection. The refusal
  now happens when the server is built, so there is no way to end up with one.
- **The hosted agent bounds request size and request time.** A request body larger than the limit is
  rejected outright, and a request that takes too long to produce a response is cut off rather than
  holding the connection open. Streaming responses are deliberately exempt — a long-lived stream is
  not a stuck request. Both bounds are generous by default and can be tuned per deployment.
- **flux checks its own dependencies for published security advisories.** Every build, and once a
  week regardless of changes, so an advisory disclosed against code that has not changed is still
  caught. The third-party build steps flux relies on are now pinned to exact versions that cannot be
  moved under it.
- **The documentation site covers the parts that were missing.** New references for the HTTP API
  (creating sessions, streaming a turn, webhooks, usage) and for the terminal UI's keys and
  commands, setup guides for the Kubernetes and SQL integrations, a real plugin-authoring guide, and
  a corrected Flux-Lang syntax reference — it previously stated that text could not span multiple
  lines, which has not been true for some time.
- **A running command shows what it is doing.** A shell command that takes a while used to display
  nothing but a spinner, so a command working steadily looked identical to one that had hung. Its
  output now appears beneath the command as it arrives, and is replaced by the usual summary once
  the command finishes. Commands that print nothing look exactly as they did before, and the same
  scrubbing that keeps secrets out of the final output also covers the live view. Work handed to a
  sub-agent still shows no live output.

### Fixed


- **A credential at the start of a diff line is no longer shown in full.** Where a line began with
  `+`, `-`, `*` or `#`, the marker ran into the credential that followed it and the redactor did not
  recognise it — so the key appeared unmasked anywhere a diff was displayed, including exported runs.
- **A long conversation no longer breaks when it is condensed.** Once a session grew past the point
  where flux summarizes earlier messages, it could store a conversation the model then refuses to
  accept. Sessions that reached that size could fail to continue.
- **Sessions started by an app, a flow, or by voice no longer begin invalidly.** These start without
  anything typed by you, and the stored conversation began on the agent's reply — a shape providers
  reject. Such a turn now opens with a short note of what triggered it, and its answer stays visible
  to the next turn.
- **An interrupted turn no longer wedges its session.** A turn that died partway through left the
  conversation waiting forever for an answer that never came. The next turn now closes it and
  carries on. Nothing already recorded is altered.
- **Asking for the latest download always gives you the newest version.** A released version could
  end up with no downloadable release attached to it, which left the "latest" download quietly
  serving an older build. The missing release has been restored, and a check now runs continuously
  so that neither a missing download nor a "latest" pointer drifting behind can go unnoticed again.

## [0.33.0] - 2026-07-29

### New

- **Share a run as a single file.** `flux export <run> -o run.html` turns a recorded session into
  one self-contained web page — the plan, every operation and its result, diffs, cost and a
  timeline, with sub-agent work nested where it belongs. No server and no viewer app: open it in a
  browser, attach it to a bug report, or link it from a pull request. Secrets are scrubbed before
  anything reaches the page, and exporting never changes the run.
- **Three new looks.** `/theme` now offers `dracula`, `nord`, and — if you need maximum
  legibility — `high-contrast`, alongside the existing dark, light, and mono. Your choice is
  remembered, and `NO_COLOR` is still respected everywhere.

### Improved

- **The conversation is easier to scan.** Each entry in the transcript now carries a subtle
  colored rail on its left edge — your messages, the agent's replies, and tool activity each read
  differently at a glance, so finding "where my turn started" in a long session no longer means
  reading.
- **Approvals show how risky the action is.** The approval sheet's frame and title now say
  whether the pending action only reads, writes, or destroys — a destructive action is
  unmistakably marked, including single-action approvals that previously looked identical to
  harmless ones.
- **Tool cards tell you they can expand.** A card with more detail behind it shows a `▸` marker
  (`▾` while expanded), so the detail view is discoverable without reading the help screen.
- **A fresh session greets you.** Opening flux on an empty conversation now shows a small card
  with your model, your project folder, and the three things worth knowing (`/help`, `/`
  commands, `@` to reference files) instead of a blank screen.
- **The queue, session, and help panels share one look.** Same header, same counters, same
  feel — and each panel now sizes exactly to its content.
- **Ctrl-C won't end your session by accident.** Quitting now takes two presses in quick
  succession, announced in the footer; a single stray Ctrl-C no longer closes flux. Interrupting
  a running turn and clearing a typed draft work exactly as before.
- **Long-lived servers keep their session database tidy.** A served agent now compacts the
  database's write-ahead log periodically, so disk use stays bounded on busy shared stores —
  with zero impact on running work.

## [0.32.0] - 2026-07-28

### New

- **Pipe flux into anything.** `flux run --stream-json` turns a run into one JSON object per line
  — what started, what was planned, every operation and its result, approvals, cost — ready for
  `jq`, CI logs, or another program, with secrets scrubbed from every line. Add
  `--stream-json-input` and the same pipe drives flux: send follow-up messages, or steer a turn
  that's still running. (Preview — the line format may still change.)
- **Your agent can now say "wake me in ten minutes."** With `[wakeup] enabled` in config, an
  agent that kicked off something slow can schedule its own follow-up: the reminder survives
  restarts, fires as a normal turn in the same session the next time it opens, and counts toward
  that session's usage. Scheduling one requires approval like any host-level action, there's a
  per-session cap and a maximum horizon, and `flux wakeups list | cancel` shows and clears what's
  pending.
- **`flux doctor` tells you what's wrong with an install.** One command checks your credentials
  (including expiring logins), plugin integrity, the sandbox, the session database's health, risky
  network config, disabled tools, and whether you're behind the latest release — each problem
  comes with a one-line hint for fixing it. `--json` for scripts; the exit code fails only on real
  failures, never on warnings.

- **Grade your agent's answers, not just its actions.** The SDK test-kit can now assert that a
  text output satisfies a written criterion, judged by a model you name explicitly. The judge's
  verdict is recorded on first run and replayed from disk after that — so `cargo test` stays
  offline and free — and any change to the output or the criterion demands a fresh grade instead
  of silently passing against a stale one.
- **Send your runs to Grafana.** Builds with the `otel` feature can export a recorded run as
  OpenTelemetry traces and metrics — the turn, its plans, and every operation as spans with real
  latency, retry, and cost numbers, plus token/spend/error-rate metrics — to any OTLP collector.
  Secrets are scrubbed from everything exported, and exporting never changes the run itself.
- **Organizations can now pin a security baseline.** A managed config file
  (`/etc/flux/config.toml`, or wherever `FLUX_MANAGED_CONFIG` points) loads ahead of user and
  project config. Settings there are defaults users may change — unless the operator *pins* them,
  in which case any attempt to relax them is refused with a clear message naming the pinned key.
  Making things stricter locally always stays allowed, and `flux doctor` shows which layer each
  security setting came from. Honest scope: this is an operator control backed by file
  permissions, not protection against someone who owns the machine.

### Improved

- **The pickers got faster to drive.** Type into the session picker to filter it — matching is the
  same fuzzy style as path completion, so a few characters land on the right session, and each row
  now shows how old the session is (`2h ago`). Slash-command matching uses the same fuzzy ranking
  (`/thm` finds `/theme`), and long lists say ` 3/14 ` so you know there's more below the window.

## [0.31.1] - 2026-07-28

### New

- **Find a past session without scrolling for it.** `flux sessions` can now search: `--query`
  matches what was said and done in a session, `--file` finds the sessions that touched a given
  path, and `--since`/`--until` narrow by date — so resuming, replaying, or forking past work no
  longer starts with scrolling a newest-first list for the right id. Secrets stay protected:
  redacted values never appear in results and cannot be used as search terms.
- **Turn tools off per repo.** A new `[tools]` section in `.flux/config.toml` takes a plain
  blocklist — exact names or `family.*` patterns like `disable = ["browser.*", "web.*"]` — for
  ops a repo simply never uses. Disabled ops disappear from what the model sees and are refused
  if anything tries to call them anyway. A pattern that matches nothing warns at startup instead
  of silently doing nothing, and `/tools` in the REPL marks what's off. This trims prompt size
  and attack surface; your authorization policy remains the security control.
- **Ask another model for a second opinion.** Configure `[consult] model` and the agent gains a
  `consult` op: it can put a hard sub-question to a different (typically stronger) model and get
  back advice — never actions. The consulted model has no tools, no file or network access, and
  its answer is treated as untrusted content; calls are capped per turn and show up in your usage
  and cost figures like any other model call.

### Fixed

- **Integration plugins now follow you into a worktree.** After entering an isolated worktree,
  flux's own tools worked there — but operations run by integration plugins (kubectl, gitlab, …)
  quietly kept running in the original checkout. Plugin operations now run in the same directory
  as everything else, switching when you enter and switching back when you leave.

### Improved

- **Long codex sessions got cheaper and lighter on the wire.** flux now keeps one live connection
  to the ChatGPT backend per session and sends only what's new in the conversation each round,
  instead of reconnecting and resending the whole history every time. Repeat rounds now reliably
  hit the server's prompt cache where they used to miss it almost entirely, and the same
  connection routing means the benefit no longer depends on luck. No setup needed; set
  `FLUX_CODEX_WS=off` if you ever need the old plain-HTTP behaviour.

## [0.31.0] - 2026-07-28

### New

- **The chat UI now shows where your time went.** Each model reply carries a badge with how long
  the model took and how quickly it started answering; while a call runs, the footer shows that
  wait live next to the turn total; and a finished turn splits its clock into model time versus
  everything else — so a slow model, a slow tool, and a paused connection stop looking identical.
  When a provider hiccups and flux retries behind the scenes, the footer now says so (`↻ retry 2/6 ·
  waiting 4s`) instead of leaving you staring at a spinner, and the transcript records how many
  retries a reply needed.
- **Approving a plan now shows you the plan.** The approval sheet used to summarize a multi-step
  plan as a count (“3 op(s)”); it now lists each operation with what it will touch, so you can see
  exactly what you are authorizing before you say yes.

### Improved

- **Claude models through OpenRouter got much cheaper.** They were being billed at full price on
  every request because prompt caching never applied to them — the reuse discount other providers
  got simply was not being asked for. It is now, and OpenRouter models also return their tool calls
  cleanly instead of occasionally emitting them as stray text.

### Action needed

- **`openrouter-anthropic` is now just `openrouter`.** If you use a model spec, config entry, or
  role that starts with `openrouter-anthropic/`, drop the `-anthropic`: `openrouter-anthropic/z-ai/glm-4.6`
  becomes `openrouter/z-ai/glm-4.6`. The model name itself does not change. There was never a real
  difference between the two — but because `openrouter/anthropic/…` is the spelling people reach
  for, the one that cached properly was the one nobody used. Now there is only one, and it is the
  good one. A spec still using the old name fails immediately and tells you the new spelling; past
  usage and cost history are unaffected.

## [0.30.1] - 2026-07-28

### New

- **Editing `.flux` files got a lot sharper.** The language server used to advertise more than it
  really did; this round makes each feature work the way you would expect.
  - **Completion knows where your cursor is.** It used to offer everything it could think of at
    once — every operation, every keyword, and every `$variable` it could find anywhere in the file,
    including ones from other flows and ones that only appear inside quoted text. Now typing `$`
    offers the variables actually in scope at that spot, typing `@` offers annotations, the start of
    a line offers keywords and operations, and inside a call you get operations, variables and
    types. Nothing is suggested while you are inside a comment or a string. Operation suggestions
    come with their signature and fill in the parameter placeholders for you.
  - **Hover reads the code, not the letters under the pointer.** Hovering the word `read` inside a
    comment no longer pops up an operation card, and hovering a `$variable` now tells you where it
    was bound and what it belongs to — before, it told you nothing at all.
  - **Find references and rename are new.** Rename understands scope: two flows that each use `$x`
    stay independent, and renaming an inner variable leaves the outer one alone. If your cursor is
    not on something renameable, the editor tells you up front instead of offering a broken rename.
  - **Formatting works on more files and preserves more of them.** A file with several `flow` or
    `op` declarations used to be skipped entirely; it now formats and keeps your declarations in the
    order you wrote them. A flow with comments used to only get its indentation fixed; it now gets
    fully tidied with every comment kept where it belongs. You can also format just a selection. If
    formatting would ever change what your program means, no edit is made.
  - **Your own operations are recognised.** Calling a composite operation you defined under
    `.flux/flows` or `.flux/ops` is no longer flagged as an unknown operation. Problems that would
    actually stop a flow running are now marked as errors rather than warnings, so the real ones
    stand out.
  - **Large files stay responsive.** The parsed file is reused across requests instead of being
    re-read for each one — on a 2,000-line flow, a keystroke followed by completion and hover does a
    third of the work it used to.

- **Connect to MySQL and MariaDB databases.** Pointing the SQL integration at a MySQL or MariaDB
  server used to fail with "not yet supported" — only PostgreSQL worked. Now all of it works the
  same way it does against Postgres: list databases and tables, inspect a table's columns, primary
  key and foreign keys, list its indexes, and run your own read-only queries. Everything stays
  read-only, and the connection password is still never handed to the integration itself.
  Two things worth knowing. On MySQL and MariaDB a schema *is* a database — there is no level in
  between — so listing databases gives you just the databases, where on Postgres it also lists the
  schemas inside the one you are connected to. And servers configured for MySQL 8's newer
  `caching_sha2_password` login are not supported yet; you will get a message naming it, and can
  either use a server-standard password login or connect to MariaDB, which does not default to it.
  SQLite is still unsupported — it is a file on disk rather than something to connect to.

- **Project guidance that only shows up when it applies.** Conventions files like `AGENTS.md` are
  read in full every session, so in a big repository you either keep them short and lose the
  subsystem detail, or keep them complete and pay for all of it on every message. You can now put a
  Markdown file in `.flux/context.d/` and give it a `globs:` list — it reaches the agent only when
  the files you are actually working on match. Parser conventions when you're editing the parser,
  UI conventions when you're editing the UI, neither when you're doing something else. A fragment
  with no `globs:` always loads, so it doubles as a way to split a long conventions file into
  readable pieces.

## [0.30.0] - 2026-07-28

### Improved

- **Long conversations got substantially cheaper and faster.** flux now reuses the cached
  conversation itself, not just the system prompt, so a long turn stops re-paying full price for the
  whole transcript on every round. On a long-transcript turn that took the share served from cache
  from 47% to 71% and the equivalent cost from about $0.11 to about $0.04; short turns are
  unaffected. The reusable part of the prompt also survives an hour now instead of five minutes, so
  stepping away for a coffee no longer cold-starts your next message.
- **Codex sessions stopped throwing their cache away.** The `codex` provider now talks HTTP by
  default: it was opening a fresh connection per request and resending the whole conversation, which
  landed on an arbitrary server with nothing cached — about 3% reuse, against roughly 50% on the new
  default and up to 97% once warm, with no speed penalty.
- **See the caching work, in the moment.** `/usage` in the TUI shows this turn's cache hit rate, the
  read/write/fresh split, and a per-round bar list that makes a mid-turn cache reset visible as it
  happens, plus session totals.
- **The live token counters stopped under-reporting.** The TUI header and the CLI turn summary were
  showing only the last round of a multi-round turn, and blended cache reads with cache writes.
  They now add up the whole turn and report reads and writes separately. `flux usage` was always
  correct and is unchanged.

## [0.29.0] - 2026-07-28

### Improved

- **Plugins keep working across flux upgrades, and now say so when they don't.** The protocol a
  plugin speaks to flux is versioned separately from flux itself, so upgrading flux no longer means
  reinstalling the plugin pack — a plugin built today keeps working against much later versions.
  Every release runs the previously published plugin binaries against the new flux to prove it. If a
  plugin genuinely does speak a different protocol, flux now tells you which version each side
  speaks instead of failing with an unreadable parsing error.

### Action needed

- **If you build your own flux plugins**, the shared plugin SDK has moved to its own version line
  and starts at `1.0`: change your dependency on `codewandler-flux-host-kit` from `"0.28"` to `"1"`.
  It will stay on `1.x` across flux releases and only move when the plugin protocol itself changes.

## [0.28.0] - 2026-07-28

### New

- **You can talk to the agent while it's working.** In the TUI, type while a turn is running and
  press Enter: your guidance is picked up at the agent's next planning step and folded into the
  work in progress — no need to interrupt and start over. Queued messages show above the composer
  and stay editable (open them with `/queue`) until the agent picks them up; the transcript notes
  the moment each one is delivered, and anything the agent didn't get to simply becomes your next
  turn. The plain REPL still waits for the turn to finish.

- **Your Claude Code commands and skills now work in flux.** Drop markdown command files in
  `.flux/commands` or `.claude/commands` (in the project or your home directory) and run them as
  `/name arguments…` in the REPL and TUI — `$ARGUMENTS` and `$1`–`$9` placeholders are filled in,
  and each command's description appears in `/help` and the slash menu. Skills load from both
  worlds too, including multi-file skills nested in subdirectories, and a skill can now point the
  agent at its own bundled reference files instead of everything loading up front. Anything in a
  skill's frontmatter that flux doesn't support warns loudly at load time instead of silently
  disappearing.

- **Optionally let the agent load skills itself.** Activating skills stays a manual choice by
  default — nothing changes unless you opt in. With `--skills-model-invoked` (or the matching
  config setting), the agent sees a compact catalog of skill names and descriptions and can pull
  one in mid-task; a loaded skill stays active for the rest of the session, and individual skills
  can opt out. Commands and skills explicitly marked agent-triggerable can also be invoked by the
  agent on its own — but only when policy allows it and the file opts in.

- **The agent can now do risky work in an isolated git worktree — and merge it back when it's
  done.** Ask the agent to enter a worktree and it moves itself (and only itself — other agents
  and your own shell are untouched) into a temporary copy of your repository on a scratch branch.
  Your checkout stays exactly as you left it. When the work is committed, leaving the worktree
  first proves the merge is conflict-free, then merges it into `main`, cleans up the scratch
  branch, and returns the agent to your project — and if anything goes wrong, your work and your
  `main` branch are always left intact. Both steps ask for your approval like every other
  high-risk operation.

- **Integration plugins that drive a CLI can no longer run more of it than they need.** A plugin's
  permission to run a program is now spelled out down to the subcommand — the Kubernetes
  integration, for example, is allowed `kubectl get`, `kubectl logs`, or `kubectl scale`, but
  `kubectl delete` simply does not exist for it, no matter what the cluster credentials would
  permit. Each individual operation is further limited to exactly the commands it needs, approval
  prompts show that narrowed permission (so you approve `kubectl get`, not all of kubectl), and
  the AWS integration is now structurally read-only. Existing plugins keep working unchanged.

- **Fetch the current Bitcoin price with a deterministic Flux-Lang example.** Run
  `flux flow run examples/bitcoin-price.flux` to request the BTC/USD spot price from Coinbase and
  extract it without a model or API credentials.

### Improved

- **The terminal UI got a major polish pass.** Pick a theme with `/theme` (`dark`, `light`,
  `mono`) and it sticks across sessions; press Ctrl-T to release the mouse so your terminal's
  native text selection and copy work; Ctrl-R searches your prompt history as you type; Ctrl-F
  searches the transcript with highlighted matches; F1 (or `/help`) opens a real help overlay.
  Approval prompts are safer and clearer: only explicit keys act — a stray keypress no longer
  counts as a denial — and what you're approving is shown as readable text. Running tools show a
  live spinner with elapsed time, a scrollbar appears when you scroll back through the
  transcript, and narrow terminals now drop the least important status details first instead of
  losing them all at once.
- **Working with what's on screen got easier.** Shift-↑/↓ walk a cursor through the transcript:
  Enter expands just the entry you're looking at, and `y` copies its full text to your clipboard —
  including over SSH. Typing `@` in the composer opens fuzzy file-path completion for your
  project. Denying an approval can now carry a reason (`d`), which the agent reads and adapts to
  instead of guessing why you said no. Edits and writes render as real diffs — hunk headers, line
  numbers, word-level highlighting — both in the tool card and in the approval prompt itself. The
  header shows small badges for the modes that are actually on (`auto-ok`, `shell`, `gather`,
  `effort:<level>`), and streamed answers are formatted as markdown as they arrive rather than
  only at the end.
- **flux opens with a boot splash, and waiting looks alive.** Interactive starts play a short
  animated FLUX splash (any key skips it; it never appears for piped output, `NO_COLOR`, small
  terminals, or with `FLUX_NO_SPLASH=1`), and on terminals with full color support the
  model-wait spinner becomes an animated effect bar that changes with every model round.
- **The project vision now matches what flux actually ships.** The vision document now calls out
  replay, fork, run diff, offline agent tests, editor tooling, multi-user deployment foundations,
  and the current on-hold status of the self-improvement work more clearly.

### Fixed

- **A saved flow or op that needs an integration you don't have no longer blocks the agent — or
  its sub-agents — from starting.** A definition in `~/.flux/flows` or `.flux/flows` that calls
  operations that aren't available (for example, a plugin you've since uninstalled, or ops a
  delegated sub-agent was never given) used to abort startup or make every sub-agent spawn fail.
  Now it's simply left out of that agent's catalog until its operations are available again — the
  exclusion is recorded in the session's audit trail, and registering a *new* op with unknown
  operations still fails immediately.

### Action needed

- **Codex now defaults to GPT-5.6.** Asking for `codex` without naming a model gets `gpt-5.6-sol`,
  the model the ChatGPT subscription currently serves, and cost reporting knows its prices. If you
  want the previous model, name it explicitly (`-m codex/gpt-5.5`) — any model you spell out is
  still sent as-is.
- **If you embed flux as a library, five APIs changed.** The guarded system on a tool context is
  now reached through a method instead of a field, approval choices gained a "deny with reason"
  variant, two terminal-UI types were renamed/extended, one composite-validation helper was
  removed in favour of the pruning path, and the skills crate dropped never-shipped entry points.
  The CLI, your config, and your `.flux` files are unaffected. See the CHANGELOG for the exact
  signatures.

## [0.27.0] - 2026-07-28

### New

- **Test your agent offline, for free — record a run once, replay it forever.**
  `flux record my-scenario "the thing your agent does"` runs the task once and saves everything
  about it — what the agent planned, what its tools answered, what the model said — into a folder
  you commit alongside your code. From then on, `flux test` re-runs the real agent against that
  recording: no API key, no network, no cost, no flaky results. If your agent starts doing
  something different, `flux test` fails and shows you exactly what changed — and whether the
  *reasoning* changed or the *world* did. Secrets are stripped before anything is written, so the
  recording is safe to check in. Building on flux as a library? The same thing lives in
  `cargo test`, where you can assert things like "this agent never runs a shell command" as a real,
  offline regression test. `FLUX_GOLDEN=update` re-records when a change was intentional.
- **Ask "what if?" about a run that already happened.** Re-run a recorded session with exactly one
  thing changed — a different model, a different prompt, a different answer from one of your tools,
  or a stricter permission policy — while everything else stays frozen exactly as it was. The
  result is a straight answer about what that one change caused, not two noisy live samples you have
  to squint at. Swapping a tool's answer costs nothing at all, because the model is never called.
  Run the same question across a whole batch of past sessions to see how many of them would change.
  Crucially, flux tells you the truth about its own limits: if the change made the agent go
  somewhere the recording can't explain, it says so and points at exactly where, instead of
  presenting a confident, incomplete answer.
- **A crashed run finishes itself.** If flux is killed mid-task — an out-of-memory kill, a
  redeploy, a closed laptop — the next run on that conversation picks the task up from exactly where
  it stopped. It does not ask the model again (so there is nothing to pay for a second time), and
  anything already done is not done twice. flux is honest about the one unavoidable edge: an
  operation interrupted *while it was happening* runs again, because the alternative would be
  silently dropping work. Turn it off with `FLUX_AUTO_RESURRECT=0`. `flux sessions` marks any
  conversation that was interrupted.
- **Set reasoning effort mid-session with `/effort`.** In the REPL or TUI, run `/effort` to see the
  current reasoning effort, or `/effort high` (also `low`, `medium`, `xhigh`, `max`, or `off`) to
  change it. The new level takes effect from your next message. Models that don't support effort
  control simply ignore it.
- **A plain-language security page.** The docs now open the Security section with "Security in
  plain terms" — what flux does to protect you, your files, and your passwords, written for people
  who use flux rather than build it. The deeper, developer-facing pages are unchanged and now link
  to it from the Security overview.
- **`flux render` can save the highlighted image as a PNG.** `flux render my.flux -o out.png`
  produces a PNG with the same syntax colours as the SVG, and text renders identically on every
  machine — the font is built in, so there is nothing to install. Any other output extension
  keeps producing SVG.

### Fixed

- **Short numeric answers stay visible.** A response consisting only of a number and period, such
  as `2.`, could disappear because it looked like an empty numbered list. It now displays normally
  in both the terminal and TUI.
- **Approval prompts no longer vanish behind the spinner.** Running without `--yes` in the plain
  terminal, the confirmation prompt was being overwritten by the progress spinner within a fraction
  of a second — the run looked stuck even though pressing `y` quietly approved it. The prompt now
  stays on screen until you answer, and it tells you what you are approving: the operations, the
  files and commands they touch, and a warning when something destructive is in the batch.
- **Agents start again when a command-line integration is installed.** Integrations that work by
  running a tool you already have — the Kubernetes one drives `kubectl`, the AWS one drives `aws` —
  were rejected at startup for reaching a cluster or cloud account through that command, and a single
  rejected operation stopped the whole session before the agent ran. Such operations are now accepted
  and remain restricted to the program the integration declares, so approval prompts and audit records
  still name exactly what runs.

## [0.26.0] - 2026-07-15

### New

- **Live systems of record can expose a consistent `<domain>.list` and `<domain>.get` operation
  pair.** Flux generates each entity's visible filter schema and renders rows and continuation
  cursors consistently, while the backend retains ownership of its async fetches.

### Hardened

- **The web and HTTP tools can no longer be tricked into leaking your secrets.** A prompt-injected
  agent can't read an arbitrary environment variable into a request header — only operator-allowlisted
  names resolve — and every outbound request is pinned to the address the egress guard vetted, closing
  a DNS-rebinding path to cloud-metadata credentials.
- **The SQLite tool is confined to your workspace and `~/.flux`.** It can no longer be steered into
  reading browser cookie stores or credential databases elsewhere on disk.
- **Untrusted programs and plans can't exhaust the host.** The language interpreter now bounds
  recursion depth (no crash from deeply nested input) and caps runaway loops and oversized iterations;
  file reads check size before loading (no out-of-memory on a giant file, no hang on a named pipe); and
  the browser, plugin, and agent-to-agent surfaces cap their buffers and queues.
- **Credentials stay out of logs and listings.** OAuth tokens redact themselves in debug output, and
  inline URL credentials are gated and hidden from endpoint listings.
- **Mistyped security or budget settings now fail loudly.** A typo in a `[server]` or `[limits]` key is
  a clear error instead of a silently ignored — and therefore disabled — control.
- **Safer file and branch edits.** `git_checkout` can no longer discard uncommitted work through a
  path-like branch name, and an empty search string in `edit` is refused instead of corrupting the file.

### Improved

- **The Rust SDK now includes a runnable live-system reference integration.** It demonstrates
  multiple entities, typed filters, opaque paging cursors, record lookup, conditional operation
  discovery, and authorization denial before a backend is contacted.
- **Embedded agents can attach a live system of record with one fallible builder call.** Its
  operations appear only when that configured domain is surfaced, and both plan preview and
  execution require the exact datasource and external resources declared by the backend.
- **Live datasource queries now validate each entity's declared filters before contacting the
  backend.** Page sizes use safe backend defaults and ceilings, while continuation cursors remain
  opaque and unchanged.
- **Prepared release binaries can now be promoted by a matching version tag without rebuilding all
  five platforms.** Promotion is tied to the exact source commit and retains the existing public
  asset verification; missing candidates fall back visibly to the full build.
- **Slack channel, user, history, and thread reads now validate their complete input before making
  a request and advertise their stable result envelope.** Slack's evolving object fields, cursors,
  and response metadata remain available unchanged.
- **GitLab project, merge-request, and issue reads now validate their complete input before making
  a request and advertise their stable result fields.** Existing raw GitLab results, including new
  vendor fields and explicit nulls, remain unchanged.

### Fixed

- **Rust SDK installation no longer stops at the runtime configuration dependency.** The config
  contract now ships as part of the dependency-ordered crates.io release.

## [0.25.0] - 2026-07-14

### New

- **Plugin authors can bind an operation directly to typed input and output.** Flux generates both
  schemas, reports the exact bad field when decoding fails, and uses the same normalized input for a
  preview and the real call. Guest-only builds also avoid pulling in the host's HTTP, credential,
  scripting, signing, and installer stacks.

### Improved

- **Web search now has one canonical `web.search` operation.** It uses Tavily when a host
  credential is configured and falls back to DuckDuckGo without a key, while keeping credentials
  out of model-visible inputs and results.
- **Embedded applications now preserve their configured workspace and integration environment when
  agents are created lazily**, so eager and lazy runs use the same root, authorization, redaction,
  specialist, plugin, and endpoint setup.

### Fixed

- **Concurrent, resumed, and cancelled agent work is now isolated cleanly.** Fresh and resumed turns
  share the same lifecycle, one agent instance cannot overlap its own mutable turn state, cancelled
  specialists are reaped, and concurrent app deliveries cannot steal or duplicate cascade results.
- **Private A2A notification webhooks can no longer redirect credentials or change destination
  after approval.** Flux rechecks DNS on every delivery, rejects redirects, and requires an exact
  private-host grant instead of a blanket local-network switch.
- **Project instructions, roles, skills, and config can no longer escape the workspace through a
  symlink or absolute path.** Trusted user-global configuration remains supported separately.
- **Bedrock setup is now lazy and refreshable.** Creating a provider no longer resolves or exports
  AWS credentials, works safely inside an existing async runtime, and refreshes temporary SSO or
  workload credentials near expiry.
- **Duplicate operation names now stop with a source-labelled error** instead of silently replacing
  the earlier handler in a built-in or plugin catalog.
- **Token usage reported before a failed or cancelled cognition call is no longer lost.** Billing,
  usage totals, and cost projections retain it once without hiding the original failure.
- **Malformed, unreadable, or duplicate specialist roles now fail with their source path** instead
  of being skipped or accidentally inheriting the parent's operations.
- **Automatic approval no longer bypasses authorization.** Database writes, deletes, payments,
  external sends, and exact filesystem, network, process, datasource, secret, and provider resources
  are checked against the caller's grants before any approval decision can execute them.

### Action needed

- **Embedded multi-principal hosts must pass caller identity into each agent turn.** Replace
  `Executor::set_identity` or `IdentityCell::set` with a `TurnIdentity` passed to
  `FlowEngine::run_turn_as` / `run_turn_cancellable_as` (or the authored-flow equivalents). This
  prevents one request from changing another in-flight turn's authorization identity.
- **If you send A2A notifications to a private host, list each hostname in
  `FLUX_A2A_PUSH_PRIVATE_HOSTS`.** `FLUX_A2A_PUSH_ALLOW_LOCAL=1` now grants only `localhost`,
  `127.0.0.1`, and `::1`.
- **`web.search` no longer accepts an `api_key` input.** Configure `TAVILY_API_KEY` on the host, or
  rely on the keyless DuckDuckGo fallback, and ensure the first-party web-search integration is
  installed.
- **Fix invalid or duplicate role files before starting an agent.** Unknown frontmatter keys and
  malformed `tools`, effort, or loop settings are now rejected rather than ignored.

## [0.24.1] - 2026-07-14

### Improved

- **Embedded chat and voice apps can keep presentation and progress operations available while Flux
  narrows a turn to the relevant integration.** Those host-owned operations remain governed by the
  same permissions, approvals, and operation limits as the rest of the active catalog.

## [0.24.0] - 2026-07-14

### Improved

- **Reading or crawling web pages now clearly shows when a page will also be saved to your searchable
  knowledge index**, so approval prompts and the audit trail disclose that storage instead of treating
  it as a plain network read.

### Action needed

- **The Slack `slack.channel.mark-read` operation is now `slack.channel.mark_read`** (underscore),
  matching every other operation name.
- **The dev-only `flux_reload` now rebuilds and asks you to restart** (exit and re-run, resuming your
  session) instead of replacing the running process.

## [0.23.1] - 2026-07-14

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
<!-- END generated:whats-new -->
