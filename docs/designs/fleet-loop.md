# Design record: host-enforced fleet loop

**Status:** superseded in product shape by
[native-board-fleet-cli.md](native-board-fleet-cli.md) and flux-roadmap Decision 0010 ·
**Epic:** [C-239](../stories/C-239-fleet-loop-epic.md)

The original fleet-loop design established the invariants that remain binding:

- the model reasons while the host enforces irreversible ordering;
- one writer and isolated worktree owns one story;
- ledger paths are fenced and write-set overlap serializes;
- behavior carries failing-first and targeted-pass evidence;
- review reads the recorded diff, two reworks return to the same session and the third parks;
- accepted commits integrate in dependency order; and
- one configured full gate runs on the final combined repository tree.

Its former product boundary is superseded. The loop is no longer only a reference
`coordinator.flux`, and a native Track replacement is no longer a non-goal. Decision 0010 makes the
durable coordinator and Track-compatible planning surface supported `flux fleet`/`flux board` CLI
products. A green candidate is not automatically published: it remains a local `fleet/<wave>` branch
until explicit `flux fleet apply` revalidates and merges it, and neither operation pushes.

Delivered foundations remain inputs: structured board query and evidence (C-236/C-240), git
integration verbs (C-238), isolation (C-241), process-worker lifecycle (C-243) and observability
(C-246). Current remaining work is C-244 typed local handoff, C-245 same-session rework, C-242 gated
integration/apply, A-117 durable supervisor/CLI and C-551 scriptless roadmap parity. The complete
current model, CLI contract, persistence and story order live in
[native-board-fleet-cli.md](native-board-fleet-cli.md).

Direct Claude/Codex process workers, remote A2A code workers, container runtimes and automatic
publication remain later work. Claude and Codex are supported callers of the CLI in V1.
