# Security assurance — close the gap between the envelope and its proof

Story: [C-186](../stories/C-186-security-assurance-epic.md) · Pillar: Core · Status: design

Source: the external adversarial desk review of 2026-07-29,
[`reviews/2026-07-29-security-posture-desk-review.md`](../../reviews/2026-07-29-security-posture-desk-review.md),
verified claim-by-claim against the tree at `0.33.1`. Every story under this epic traces to a
**confirmed** finding with a `path:line`. Nothing here rests on the reviewer's prose alone.

## The gap

The review's useful signal is not its score (6/10) but its **spread**: security architecture rated
8/10 while security assurance rated 5/10. That is the whole problem in one line — the envelope is
well designed, and almost nothing outside the envelope proves it stays that way.

flux's central claim is strong: *the LLM is not the runtime; every operation traverses authorization
→ approval → guarded IO; there are no bypass paths.* A claim that strong is only worth what its
external checks are worth. Today the checks are: a good test suite written by the implementer, and
a layering lint. The review's point — correct — is that **tests written by the implementer are not a
substitute for adversarial review**, and that the supply chain delivering flux to a user is currently
the softest part of the system.

## What this epic is, and is not

**Is:** the confirmed, actionable findings — supply-chain hardening, standing vulnerability signal,
daemon resource limits, and two invariants that are enforced in one place but not by construction.

**Is not:** the score, the bus factor, or "get an external audit". Those are context. A code change
cannot fix them and pretending otherwise turns a review into a wish list.

## Ordering, and why it is not the review's ordering

The review leads with the sandbox default. This epic does not. The ranking here is
**risk × reachability ÷ cost**:

1. **[C-187] SHA-pin the GitHub Actions.** The only finding exploitable by a third party with *no
   flux bug and no operator mistake*. Movable tags (`dtolnay/rust-toolchain@stable`,
   `actions/checkout@v4`, …) mean whoever controls those upstream repos controls code running in
   workflows that hold `MINISIGN_SECRET_KEY` and crates.io publish rights. That key is what makes
   the plugin trust model mean anything: the per-artifact SHA-256 chain the review praised
   (`release-plugins.yml:166-181`) terminates in a signature whose key is reachable through an
   unpinned third-party action. Compromise there does not break one release — it invalidates the
   signing story retroactively.
2. **[C-188] Dependency advisory scanning.** The only finding whose truth value is currently
   *unknown*. Everything else is a known-shape risk that can be reasoned about; a RUSTSEC advisory
   in the transitive tree of 38 crates either exists right now or does not, and there is no way to
   tell. Adding it converts an unknown into a standing signal.
3. **[C-189] Server body limits and request timeouts.** A gap against flux's **own published
   scope**: `SECURITY.md` names "denial of service in the `--serve` daemon" as in scope, and the
   routers carry no limit. A gap against a self-declared promise is the strongest class of finding
   available.
4. **[C-190] Make the non-loopback auth invariant hold by construction.** The review mentions this
   as a passing caveat; it is ranked higher here because `AGENTS.md` says *"there are no bypass
   paths. Don't add one."* The refusal lives in `serve_on` (`flux-server/src/lib.rs:457`), so a
   caller mounting the router directly gets no guard. That reads as a bypass path that already
   exists.
5. **[C-191] Registry-wide `ToolSpec` invariant test.** The review's "classification trust" concern,
   made concrete and cheap.

## The shape of the answer

The four hardening stories share one property worth stating, because it is what separates this epic
from a checklist: **each replaces a promise with a mechanism.**

- Pinning replaces "we trust the action publisher" with a content hash.
- Advisory scanning replaces "we would have noticed" with a job that fails.
- Limits replace "nobody would send that" with a rejected request.
- Construction-time auth replaces "callers should use `serve_on`" with a type that cannot be built
  wrong.

C-191 is the same move applied inward. The policy engine is only as good as each operation's
self-declared metadata, and `ToolSpec::read_only()` (`flux-spec/src/lib.rs:281-295`) is a coherent
preset — `Effect::Read` + `Risk::Low` + `Idempotent`. The field itself is sound: `risk` carries no
`#[serde(default)]`, so a plugin-supplied spec *cannot* omit it. The gap is that nothing asserts the
**combination** stays coherent as ops evolve. An op that starts from `read_only()` and later gains a
mutating effect without upgrading its risk compiles, ships, and quietly clears a lower approval bar.
A registry-wide test turns a standing trust assumption into a gate.

## The `Network`-without-`Read` posture (C-208)

C-191 gated `try_register_builtins`. C-208 extends the same invariants to the **production**
catalog — the registry `flux-cli` actually assembles (`flux-cli/src/execution.rs`), which also holds
the cognition pack, the eval/reflect/flows/render packs, `flux-web`, datasource + endpoint ops,
`TaskTool`, and config-authored model stages. Eleven operations violated the invariants there. Eight
of them share one shape: `Effect::Network` at `Risk::Low` with **no** `Effect::Read`.

That shape is not a single mistake with a single fix, because the two honest repairs mean opposite
things:

- **Add `Effect::Read`.** The declaration becomes `[Read, Network]`, which `is_consequence_bearing`
  accepts — and, by the correspondence with `flux-flow`'s `gather_safe`, also makes the op runnable
  during evidence gathering, *before* the approval gate.
- **Raise to `Risk::Medium`.** The declaration becomes coherent while staying out of the gather
  path, at the cost of an approval prompt on every call.

Loosening `is_consequence_bearing` to make the eight disappear was rejected outright. It is the
exact negation of `gather_safe`, and that correspondence is what makes the invariant mean anything;
weakening it would move the untruth from the catalog into the rule.

**The decision: split the eight by what the operation actually does.**

**Group A — add `Effect::Read`: `web.fetch`, `web.crawl`.** A read over the network *is* a read. The
old declaration was simply wrong: it named the carrier and omitted the direction, so it described an
unread egress — a POST — when the operation is a retrieval. Gather-safety is the correct consequence
here, not a concession granted to get the build green: these operations mutate nothing, and their
egress still passes through `flux_system::net::guard_url_scoped_pinned`, so the SSRF / private-range
envelope is exactly what it was. Nothing about the safety boundary changes; only the honesty of the
label does.

One caveat belongs on the record, because it is the one thing gather-safety now admits that it did
not before. Wired with a record sink — which is how `flux-cli` wires them — `web.fetch` and
`web.crawl` persist each fetched page as a durable `web.page` datasource record (C-58). That
contribution is disclosed, but as the **semantic** effect `write_db` plus a `datasource:web.page`
permission subject and a `flow.write_db` authority requirement, deliberately *not* as a host
`Effect::Write` (`flux-web/src/fetch.rs` pins that in a test: a datasource marker must not be read as
a filesystem write). So these two ops are now reachable during pre-approval evidence gathering, and
each such call writes a knowledge record. Authorization still runs — `flow.write_db` is required and
checked — so this is un-*approved*, not un-*authorized*, and the record is the agent's own index of a
page it just read. Recorded here so the next reader meets the trade-off rather than discovering it.

**Group B — raise to `Risk::Medium`, do *not* add `Read`: `consult`, `ai.extract`/`ai.rank`/
`ai.judge`/`ai.reason`/`ai.rewrite`/`synth`, every config-authored model stage, and the two adaptive
loop stages `detect_intent` and `explore`.** These invoke a model, and a model call **spends real
money**. `Risk::Low` is the tier every consumer reads as "nothing here worth a gate", and that
understates a billable call. Gather-safety would be actively wrong for them: it would let the
adaptive loop spend the operator's budget during exploration, before anyone approved anything.

The distinguishing property is **cost, not mutation** — state that plainly, because it is the part a
future reader will not reconstruct from the code. Both groups are non-mutating; both are "reads" in
the ordinary sense. Group A reads a public resource for free, Group B reads a metered one. The
invariants speak in terms of consequence, and a charge on the operator's account is a consequence
that outlives the call just as surely as a written file does. **Sort a future op by asking "does
calling this cost money?", not "does it write anything?".**

That test is what moved `detect_intent` into Group B. Its name and description read like local
signal resolution, but `LoopHost::detect_intent` runs `flux-flow`'s `detect_intent_stage`, which
makes a provider call and records model usage — it is billable, so the cost test lands it with the
other model calls. `explore` is the same shape and reached the same place. Raising either is cheap
in practice: `Risk::Medium` does not add an approval prompt (`RiskApprover` gates *writes* at or
above `Risk::High`, and `Executor::dispatch` forces approval only for `Risk::Destructive`), and the
op cache already excluded them. What `Medium` actually buys is exclusion from the gather path and an
honest tier in `PlanRisk::summary`.

The remaining violators were decided on their own merits, each corrected in place — **the allowlist
`flux_spec::coherence::EXEMPT` gained no new entries, which is its documented goal state**:

- `improve_log` — `[Write, Filesystem]` at `Risk::Low`, C-191's title case verbatim → `Medium`.
- `grade` — `[Read, Process]` at `Risk::Low` + `Idempotent`, on an op that runs a *caller-supplied*
  command → `Medium` + `Conditional`. Not in the story's table; the sharpest of the set.
- `gate_check` — `Idempotent` on an op whose answer tracks a moving worktree → `Conditional`.
  `Idempotent` is what licenses the op cache to replay instead of executing, and a stale "green" is
  the worst possible answer from a gate.
- `endpoint.import` — `[LocalSystem]` at `Risk::Low` + `Idempotent`, the one endpoint op that
  *writes* host state outside the jail → `Medium` + `Conditional`.
- `browser.snapshot`, `browser.close` — `Low` while `browser.open`/`goto`/`act` were already
  `Medium` → `Medium`. Being the one tier below their own family was drift, not a decision.

One further correction came out of the same pass: `CognitionOp::spec` re-declared `Risk::Low` *after*
lowering `OpKind::opspec()`, silently overriding the typed contract's own tier. Two declarations of
one field, one shadowing the other, is the drift shape this epic exists to catch, so the trailing
override is gone and the tier is declared once.

### What the gate does *not* reach

Two registration seams stay open, deliberately:

- **`flux_sdk::FlowClient::try_register_op` / `try_register_pack`.** An embedder's own operations,
  authored outside this repo, arriving through a generic seam with no compile-time list to walk.
  This is the same call that C-191 declined to gate inside `ToolRegistry::try_register_from`, for
  the same reason: registration is not a trust boundary in this runtime, and first-party tests
  deliberately register incoherent specs to prove the *downstream* gates still hold. Refusing them
  at registration would delete defence-in-depth tests rather than add defence. The instrument that
  does reach third-party metadata is the plugin loader's `op_coherence_warnings`, and hardening that
  from warn to refuse needs its own deprecation window.
- **The sub-agent `child_base` registry** (`flux-cli/src/execution.rs`). Not open at all, in fact —
  it is exactly `try_register_builtins`, a strict subset of the catalog, so it is covered twice
  over. The C-208 gate asserts that directly rather than assuming it, because "the child registry is
  a subset" is a property that a future edit could quietly falsify.

## Explicitly deferred: the sandbox default

The review's headline finding — sandbox `Off` by default, network open — is **not** a story here,
deliberately.

It is confirmed (`flux-system/src/sandbox.rs:39,:50,:64`; test at `:1151` pins it) and it is real.
But it is a product decision with a compatibility cost, not a bug, and flipping it now would make
things *worse* in one specific way: `on` currently degrades to unconfined when no backend is
available (`:463`). A default of `on` that silently runs unconfined manufactures false assurance —
strictly worse than today's `off`, which is at least honest about what it is.

The correct sequence:

1. Make `on` **report its resolved posture loudly** rather than degrading silently — the user should
   never believe they are sandboxed when they are not.
2. *Then* revisit the default, with its own design doc covering the Windows gap (no backend exists;
   only Bubblewrap and Seatbelt are implemented) and the migration story.

Step 1 is a prerequisite and belongs in its own story when this epic's four land. Recording the
reasoning here so the deferral is a decision with a trail, not an omission.

## Acceptance for the epic

The epic is done when a re-run of the adversarial review against a later version can mark findings
1–4 and the classification-trust concern as **closed with evidence**, and the deferral above has
either become a story or been consciously dropped. Re-run it with the
[`adversarial-review`](../../.agents/skills/adversarial-review/SKILL.md) skill and diff against the
2026-07-29 baseline.
