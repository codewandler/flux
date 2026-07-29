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
