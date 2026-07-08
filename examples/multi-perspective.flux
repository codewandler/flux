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
# After the join, each scout's `.evidence` array is pulled out via field-access sugar, the three
# claim lists are concatenated with `merge`, and `synth` turns the combined claims into a single
# cited prelude `Answer` (`status`/`summary`/`evidence`/`gaps`/`risks`) — demonstrating that fan-out
# orchestration, role-file sub-agents, and the cognition ops compose in the language itself, no host
# code required.
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

  # Scout results are raw model output — read `.evidence` with the `?` opt-out so a scout that
  # returns no `evidence` key degrades to an empty list instead of hard-erroring the whole turn
  # after all three sub-agents were already paid for (L-53 strict field access).
  $claims_tech = $technical.evidence?
  $claims_prod = $product.evidence?
  $claims_risk = $risk.evidence?
  $all_claims = merge({ lists: [$claims_tech, $claims_prod, $claims_risk] })

  $answer = synth({ claims: $all_claims, cite: true, format: "detailed" })
  observe({ kind: "synthesis", data: $answer })
  return $answer
