# Examples

Runnable `.flux` flows and programs demonstrating the language and runtime — native Flux-Lang
text and pre-compiled JSON `DraftAst` files (both forms run the same way). For a guided
introduction see the [language tour](https://codewandler.github.io/flux/docs/language/tour) and
the annotated [examples page](https://codewandler.github.io/flux/docs/language/examples).

Every file here is CI-gated: `crates/flux-eval/tests/examples_validate.rs` sweeps the whole
directory (no hand-picked list), parsing each file and lowering it against the full in-process op
registry — an unknown op, missing required param, or type conflict fails CI. Two documented
exceptions: program-form files (`channels-app.flux`, `zendesk.triage.flux`) get parse + structural
checks in that sweep (their external ops live outside flux-eval's in-process registry), and
`advanced-code-review.flux` is pinned at parse-only because it calls an out-of-process plugin op.
The Zendesk module additionally has an offline four-entrypoint execution test with static API/model
fixtures plus a CLI guard proving its operation set remains read-only.

## Run from this checkout

Run commands from the repository root. If `flux` is not already installed, build it and put the
development binary on this shell's path:

```bash
cargo build -p flux-cli
export PATH="$PWD/target/debug:$PATH"
```

Start with the examples that need no model, credentials, or external services:

```bash
flux flow run examples/data-transforms.flux
flux flow run examples/cognition-research.flux
flux flow run examples/eval-smoke.flux
```

The public Bitcoin example is also deterministic and credential-free, but it makes one guarded
network request and may ask for approval:

```bash
flux flow run examples/bitcoin-price.flux
```

Pass declared flow parameters with repeatable `--arg NAME=VALUE` flags or one `--inputs` JSON
object. For example, this model-backed fan-out runs three checked-in scout roles in parallel:

```bash
flux flow run examples/multi-perspective.flux \
  --arg 'query=How should Flux surface streaming errors?' \
  -m <model> --yes
```

Run the event-driven application separately; it stays active until Ctrl-C:

```bash
flux app run examples/channels-app.flux
```

## Run the Zendesk workflows

Build and register the first-party plugin, configure its endpoint and username, then store the API
token through Flux's hidden credential prompt:

```bash
(cd plugins && cargo build --release -p zendesk)
flux plugin add zendesk "$PWD/plugins/target/release/flux-plugin-zendesk"

export ZENDESK_URL="https://company.zendesk.com"
export ZENDESK_USER="agent@example.com/token"
flux auth set zendesk
```

**Not runnable today:** the backing `zendesk` plugin was removed before its first release and is to
be superseded by a flux-connectors interop layer, so every entrypoint below fails at its first call
until that lands. The flow is kept as the authored shape the replacement must satisfy, and its tests
run against stubbed operations so that shape stays enforced meanwhile.

Each named entrypoint is independently selectable. `setup` only verifies connectivity; the other
entrypoints use the configured model for bounded analysis and return gathered API evidence if that
model call fails:

```bash
flux run examples/zendesk.triage.flux --entry setup --yes

flux run examples/zendesk.triage.flux --entry triage \
  --arg 'query=type:ticket status:new' --yes

flux run examples/zendesk.triage.flux --entry brief \
  --arg ticket_id=12345 --yes

flux run examples/zendesk.triage.flux --entry eod \
  --inputs '{"query":"type:ticket updated>24hours"}' --yes
```

See the [Zendesk triage guide](../docs/zendesk-triage.md) for credential behavior, model data
exposure, and the write boundary the replacement integration inherits.

## Index

| File | What it shows | Run |
| --- | --- | --- |
| `advanced-code-review.flux` | Full AI code-review pipeline: parallel context gather with `fallback`, budgeted `ctx` pack, `retry`/`timeout`, cognition ops (`ai.extract`/`ai.rank`/`ai.judge`/`ai.reason`, `synth`), model-routed `route`, and native `confirm`, `saga`+undo with `once`-guarded steps, and `verify` | `flux flow run examples/advanced-code-review.flux --arg pr_branch=my-feature --arg base_branch=main --arg 'notify_channel=#code-review' -m <model> --yes` — needs a model and the `flux-plugin-slack` plugin (`slack.message.send`); CI validates it parse-only |
| `bitcoin-price.flux` | Deterministic BTC/USD spot-price lookup: one fixed Coinbase API request followed by bounded regex extraction; no model call | `flux flow run examples/bitcoin-price.flux` — needs network access, no model or API credentials |
| `channels-app.flux` | A whole app in native text: a cron `channel` (5s heartbeat) + a webhook `channel`, `trigger`s wiring bus events to `journey`s | `flux app run examples/channels-app.flux` — offline, pure ops only (Ctrl-C to stop) |
| `cognition-research.flux` | JSON `DraftAst`: the evidence loop shape — `need` → `grep` → `sort`/`top` → `ctx` pack → `repeat` until `gaps` close → `cite`d answer (several ops fed empty literal lists; it demos the loop structure) | `flux flow run examples/cognition-research.flux` — offline (deterministic evidence ops) |
| `data-transforms.flux` | Hermetic native Flux-Lang data shaping with `map`, `filter`, `flatten`, `sort`, and `top` | `flux flow run examples/data-transforms.flux` — offline, pure ops only |
| `eval-smoke.flux` | JSON `DraftAst`: offline eval slice — run the `mock` benchmark adapter, mine sessions for pain-points, aggregate improvement candidates | `flux flow run examples/eval-smoke.flux` — offline (drives the `-m mock` provider, no network) |
| `eval-synthetic.flux` | JSON `DraftAst`: one trial of the synthetic coding-riddle benchmark plus a markdown report | `flux flow run examples/eval-synthetic.flux` — needs credentials for the model named inside the file (an OpenRouter Anthropic model) |
| `god-review.flux` | One repo review through five parallel lenses (style/security/architecture/performance/clarity), one `task` sub-agent per lens, merged by a summarizer role | `flux flow run examples/god-review.flux -m <model> --yes` — needs a model; its `god-reviewer` role file (`.flux/agents/god-reviewer.md`) is gitignored, so a fresh clone must supply one |
| `improve-multi.flux` | JSON `DraftAst`: self-improvement loop over a combined terminal-bench + synthetic benchmark — baseline eval → reviewer/planner sub-agents → implement → protected-path guard + gate → re-eval → keep (commit+tag) or revert | `FLUX_EVAL_BINARY=target/x86_64-unknown-linux-musl/release/flux FLUX_TERMINAL_BENCH_REBUILD=1 flux flow run examples/improve-multi.flux` — needs a model, Docker + terminal-bench, and a musl release flux binary; makes real git commits/reverts in the repo |
| `improve-synthetic.flux` | JSON `DraftAst`: the same keep-or-revert improvement loop scored on the synthetic riddle suite only | `flux flow run examples/improve-synthetic.flux` — needs a model (named inside the file) and a debug flux binary; makes real git commits/reverts |
| `improve-tbench.flux` | JSON `DraftAst`: the same loop scored on a single terminal-bench task (`fibonacci-server`), candidates ranked by measured weight | `FLUX_EVAL_BINARY=target/x86_64-unknown-linux-musl/release/flux FLUX_TERMINAL_BENCH_REBUILD=1 flux flow run examples/improve-tbench.flux` — needs a model, Docker + terminal-bench, musl release binary; makes real git commits/reverts |
| `multi-perspective.flux` | Parallel 3-lens scout fan-out (technical/product/risk) via `task` + checked-in `.flux/agents/*-scout.md` role files, evidence merged and `synth`esized into a cited `Answer` | `flux flow run examples/multi-perspective.flux --arg 'query=How should Flux surface streaming errors?' -m <model> --yes` — needs a model |
| `release.flux` | Host-decided release planning: derives the SemVer bump mechanically and renders release metadata without letting a model choose the version | `flux flow run examples/release.flux --arg apply=false --yes` — safe planning mode; does not publish or push |
| `strict_review.flux` | The strict review protocol: read-only git/context gather → budgeted `ctx` → fixed fan-out to three restricted reviewer roles → deterministic `review.aggregate` (normalize, dedupe, rank) into a typed `ReviewReport` | `flux review --files <path>… -m <model>` — this very file is compiled into the binary (`include_str!` in `crates/flux-app/src/review.rs`) as the flow behind `flux review`; the header's `flux run … --input` line is stale (no such flag) |
| `zendesk.triage.flux` | Four named, read-only Zendesk support workflows: auth setup, queue triage, ticket+comment brief, and end-of-day summary; deterministic retry/parallel/timeout/fallback with AI confined to analysis | `flux run examples/zendesk.triage.flux --entry <setup\|triage\|brief\|eod> [--arg name=value] --yes` — needs `flux-plugin-zendesk`; all but `setup` need a configured model |

## Caveats

Examples marked as needing a model call sub-agents or cognition ops at run time, so a configured
provider (via `-m <model>` or the file's embedded model id) and its API credentials are required;
the offline ones (`channels-app`, `data-transforms`, `cognition-research`, `eval-smoke`) run with no
network at all.
The `improve-*` loops modify the repository they run in (snapshot, commit, tag, revert) — run them
only in a checkout you are willing to have committed to.
