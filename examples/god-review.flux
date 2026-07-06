# god-review.flux — one repo review through five parallel lenses, one sub-agent per lens
# (role `god-reviewer`, .flux/agents/god-reviewer.md — read-only tools, fast model), merged by
# the built-in summarizer role (inherits the driver model) into a single prioritized report.
#
#   flux flow run examples/god-review.flux -m openrouter/deepseek/deepseek-v4-pro --yes
flow god-code-review -> String
  parallel
    branch $style
      $style = task({role: "god-reviewer", task: "STYLE lens: review the flux workspace (crates/*/src) for naming, formatting, and idiom consistency — places where the code diverges from the surrounding conventions. Sample broadly before judging."})
    branch $security
      $security = task({role: "god-reviewer", task: "SECURITY lens: audit the flux workspace for weaknesses in the safety envelope (authorization -> approval -> guarded IO). Focus on crates/flux-runtime, crates/flux-system, crates/flux-plugin: bypass paths, unchecked inputs at trust boundaries, secret handling."})
    branch $arch
      $arch = task({role: "god-reviewer", task: "ARCHITECTURE lens: review the flux workspace for coupling and layering problems. The layering rule: a crate may depend only on its own layer or lower (see AGENTS.md table). Look for leaky abstractions, cyclic knowledge, misplaced responsibilities."})
    branch $perf
      $perf = task({role: "god-reviewer", task: "PERFORMANCE lens: review the flux workspace for avoidable allocations, quadratic scans, and hot-path waste — especially crates/flux-flow (the turn loop), crates/flux-events (the event store), crates/flux-lang (parse/format)."})
    branch $clarity
      $clarity = task({role: "god-reviewer", task: "CLARITY lens: review the flux workspace for readability — functions doing too much, misleading names, comments that restate code, missing why-comments at genuinely surprising spots."})
  $report = task({role: "summarizer", task: """
Merge these five code-review lens reports into ONE prioritized review of the flux repository.

Rules: de-duplicate overlapping findings; keep each finding's file:line; downgrade anything that
merely restates a design invariant or a documented trade-off; rank by severity then confidence;
end with a one-paragraph overall verdict. Output markdown with sections: Verdict, High, Medium,
Low. Attribute each finding to its lens(es).

## Style lens
{style}

## Security lens
{security}

## Architecture lens
{arch}

## Performance lens
{perf}

## Clarity lens
{clarity}
"""})
  return $report
