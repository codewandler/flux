# multi-perspective.flux — parallel 3-lens scout fan-out, merged and synthesized to a cited Answer
# (docs/stories/L-37-multi-perspective-example.md).
#
# One query, three independent lenses, run concurrently via `parallel` + `branch $name` arms: a
# TECHNICAL scout (architecture/implementation/feasibility), a PRODUCT scout (user value/UX/scope),
# and a RISK scout (failure modes/security/operational risk). Each lens is a `task` call resolved
# through its `.flux/agents/*-scout.md` role file — a real sub-agent, not a prompt fragment. The
# branch's LAST statement is what binds to the branch name, so each arm's `task(...)` bind comes
# after its `observe` (a per-branch trailing observe would clobber the scout result — see the story's
# Notes for the grounding).
#
# After the join, the scout outputs are projected with pure transforms: `map` plucks each
# `.evidence` array, `filter` drops missing/empty evidence, and `flatten` concatenates the remaining
# claim lists. `synth` then turns the combined claims into a single cited prelude `Answer`
# (`status`/`summary`/`evidence`/`gaps`/`risks`) — demonstrating that fan-out orchestration,
# role-file sub-agents, and the cognition ops compose in the language itself, no host code required.
#
# Run with: `flux flow run examples/multi-perspective.flux --input '{"query": "How should flux surface streaming errors?"}'`

flow multi-perspective(query: String) -> Answer
  parallel
    branch $technical
      observe({ kind: "lens-start", data: { perspective: "technical" } })
      $technical = task({ role: "tech-scout", task: $query })
    branch $product
      observe({ kind: "lens-start", data: { perspective: "product" } })
      $product = task({ role: "product-scout", task: $query })
    branch $risk
      observe({ kind: "lens-start", data: { perspective: "risk" } })
      $risk = task({ role: "risk-scout", task: $query })

  observe({ kind: "lens-end", data: { perspectives: ["technical", "product", "risk"] } })

  # Scout results are raw model output — `map(path: "evidence")` uses lenient dotted paths, so a
  # scout that returns no `evidence` key yields `null`; `filter(where: "it")` drops null/empty lists
  # before `flatten` concatenates the claim arrays.
  $scouts = [$technical, $product, $risk]
  $claim_lists = map({ items: $scouts, path: "evidence" })
  $present_claim_lists = filter({ items: $claim_lists, where: "it" })
  $all_claims = flatten({ items: $present_claim_lists })

  $answer = synth({ claims: $all_claims, cite: true, format: "detailed" })
  observe({ kind: "synthesis", data: $answer })
  return $answer
