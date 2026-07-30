flow improve_synthetic -> EvalReport
  baseline = eval_run({"adapter":"synthetic","flux_bin":"target/debug/flux","model":"anthropic/claude-sonnet-4-6","trials":5})
  base_score = eval_scalar(baseline)
  reviewed = task(role: "reviewer", task: fmt("""These are flux's synthetic coding-riddle results: short self-contained problems that ask the agent to write `solution.py`, graded objectively on `python3 solution.py` stdout. Each failing case names a task and a failure_mode. Identify flux HARNESS improvements (tools, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix) that would help flux solve more riddles on the first attempt — not changes to any single riddle. Results:
{baseline}

Return ONLY a JSON array: [{"area":..,"symptom":..,"suggested_fix":..,"severity":1-5}]."""))
  candidates = improvements_aggregate(mined: "[]", reviewed)
  repeat 1
    until candidates_empty(candidates)
    tasks = task(role: "planner", task: fmt("""Turn these flux-harness improvement candidates into AT MOST 2 concrete, small, safe engineering tasks for the flux codebase (tool specs, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix). Do NOT touch crates/flux-eval, bench/, the loop flows, the synthetic suite, or CI. Candidates:
{candidates}

Return ONLY the JSON array of tasks."""))
    snapshot = git_snapshot()
    implemented = change_implement(limit: 2, tasks)
    guard = guard_protected(snapshot)
    gate = gate_check()
    when gate
      candidate = eval_run({"adapter":"synthetic","flux_bin":"target/debug/flux","model":"anthropic/claude-sonnet-4-6","trials":5})
      cand_score = eval_scalar(candidate)
      when score_compare(baseline, candidate)
        git_stage(["."])
        git_commit("improve: adopt candidate (synthetic gain)")
        git_tag("improve-synthetic-{{cand_score}}")
        improve_log({"record":{"base_score":"{{base_score}}","bench":"synthetic","cand_score":"{{cand_score}}","decision":"kept","gate":"{{gate}}","guard":"{{guard}}","reason":"candidate_beat_baseline","tag":"improve-synthetic-{{cand_score}}","tasks":"{{tasks}}"}})
        baseline = eval_adopt(candidate)
      else
        git_reset(snapshot)
        improve_log({"record":{"base_score":"{{base_score}}","bench":"synthetic","cand_score":"{{cand_score}}","decision":"reverted","gate":"{{gate}}","guard":"{{guard}}","reason":"no_improvement","tasks":"{{tasks}}"}})
    else
      git_reset(snapshot)
      improve_log({"record":{"base_score":"{{base_score}}","bench":"synthetic","decision":"reverted","gate":"{{gate}}","guard":"{{guard}}","reason":"gate_failed","tasks":"{{tasks}}"}})
    candidates = candidates_advance(candidates)
  return baseline
