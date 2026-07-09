# Examples

Runnable `.flux` flows and programs demonstrating the language and runtime — native Flux-Lang
text and pre-compiled JSON `DraftAst` files (both forms run the same way). For a guided
introduction see the [language tour](https://codewandler.github.io/flux/docs/language/tour) and
the annotated [examples page](https://codewandler.github.io/flux/docs/language/examples).

Every file here is CI-gated: `crates/flux-eval/tests/examples_validate.rs` sweeps the whole
directory (no hand-picked list), parsing each file and lowering it against the full in-process op
registry — an unknown op, missing required param, or type conflict fails CI. Two documented
exceptions: program-form files (`channels-app.flux`) get parse + structural trigger checks
(their orchestration ops live in the app runtime), and `advanced-code-review.flux` is pinned at
parse-only because it calls an out-of-process plugin op.

## Index

| File | What it shows | Run |
| --- | --- | --- |
| `advanced-code-review.flux` | Full AI code-review pipeline: parallel context gather with `fallback`, budgeted `ctx` pack, `retry`/`timeout`, cognition ops (`ai.extract`/`ai.rank`/`ai.judge`/`ai.reason`, `synth`), model-routed `route`, and native `confirm`, `saga`+undo with `once`-guarded steps, and `verify` | `flux flow run examples/advanced-code-review.flux -m <model>` — needs a model **and** the `flux-plugin-slack` plugin (`slack.message.send`); CI validates it parse-only. Its header's `flux run … --param` line is stale: the CLI has no flag to supply its declared flow parameters |
| `channels-app.flux` | A whole app in native text: a cron `channel` (5s heartbeat) + a webhook `channel`, `trigger`s wiring bus events to `journey`s | `flux app run examples/channels-app.flux` — offline, pure ops only (Ctrl-C to stop) |
| `cognition-research.flux` | JSON `DraftAst`: the evidence loop shape — `need` → `grep` → `sort`/`top` → `ctx` pack → `repeat` until `gaps` close → `cite`d answer (several ops fed empty literal lists; it demos the loop structure) | `flux flow run examples/cognition-research.flux` — offline (deterministic evidence ops) |
| `eval-smoke.flux` | JSON `DraftAst`: offline eval slice — run the `mock` benchmark adapter, mine sessions for pain-points, aggregate improvement candidates | `flux flow run examples/eval-smoke.flux` — offline (drives the `-m mock` provider, no network) |
| `eval-synthetic.flux` | JSON `DraftAst`: one trial of the synthetic coding-riddle benchmark plus a markdown report | `flux flow run examples/eval-synthetic.flux` — needs credentials for the model named inside the file (an OpenRouter Anthropic model) |
| `god-review.flux` | One repo review through five parallel lenses (style/security/architecture/performance/clarity), one `task` sub-agent per lens, merged by a summarizer role | `flux flow run examples/god-review.flux -m <model> --yes` — needs a model; its `god-reviewer` role file (`.flux/agents/god-reviewer.md`) is gitignored, so a fresh clone must supply one |
| `improve-multi.flux` | JSON `DraftAst`: self-improvement loop over a combined terminal-bench + synthetic benchmark — baseline eval → reviewer/planner sub-agents → implement → protected-path guard + gate → re-eval → keep (commit+tag) or revert | `flux flow run examples/improve-multi.flux` — needs a model, Docker + terminal-bench, and a musl release flux binary; makes real git commits/reverts in the repo |
| `improve-synthetic.flux` | JSON `DraftAst`: the same keep-or-revert improvement loop scored on the synthetic riddle suite only | `flux flow run examples/improve-synthetic.flux` — needs a model (named inside the file) and a debug flux binary; makes real git commits/reverts |
| `improve-tbench.flux` | JSON `DraftAst`: the same loop scored on a single terminal-bench task (`fibonacci-server`), candidates ranked by measured weight | `flux flow run examples/improve-tbench.flux` — needs a model, Docker + terminal-bench, musl release binary; makes real git commits/reverts |
| `multi-perspective.flux` | Parallel 3-lens scout fan-out (technical/product/risk) via `task` + checked-in `.flux/agents/*-scout.md` role files, evidence merged and `synth`esized into a cited `Answer` | `flux flow run examples/multi-perspective.flux -m <model>` — needs a model; the header's `--input` flag does not exist in the current CLI, so the `query` parameter cannot be supplied from the command line |
| `strict_review.flux` | The strict review protocol: read-only git/context gather → budgeted `ctx` → fixed fan-out to three restricted reviewer roles → deterministic `review.aggregate` (normalize, dedupe, rank) into a typed `ReviewReport` | `flux review --files <path>… -m <model>` — this very file is compiled into the binary (`include_str!` in `crates/flux-app/src/review.rs`) as the flow behind `flux review`; the header's `flux run … --input` line is stale (no such flag) |

## Caveats

Examples marked as needing a model call sub-agents or cognition ops at run time, so a configured
provider (via `-m <model>` or the file's embedded model id) and its API credentials are required;
the offline ones (`channels-app`, `cognition-research`, `eval-smoke`) run with no network at all.
The `improve-*` loops modify the repository they run in (snapshot, commit, tag, revert) — run them
only in a checkout you are willing to have committed to.
