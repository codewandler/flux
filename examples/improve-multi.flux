flow improve_multi -> EvalReport
  baseline = eval_run({"adapter":"multi","members":[{"adapter":"terminal-bench","agent_timeout_secs":180,"tasks":["chess-best-move","fibonacci-server"]},{"adapter":"synthetic"}],"model":"anthropic/claude-sonnet-4-6","trials":3})
  base_score = eval_scalar(baseline)
  reviewed = task({"role":{"kind":"lit","value":"reviewer"},"task":{"kind":"lit","value":"""These are flux's combined benchmark results (terminal-bench + synthetic riddles). Each failing case names a task, a failure_mode, and (when partial) which sub-checks failed (failed_checks). Identify flux HARNESS improvements (tools, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix) that would help flux pass more. Results:
{{baseline}}

Return ONLY a JSON array: [{"area":..,"symptom":..,"suggested_fix":..,"severity":1-5}]."""}})
  candidates = improvements_aggregate({"mined":{"kind":"lit","value":"[]"},"reviewed":{"kind":"var","name":"reviewed"}})
  repeat 1
    until candidates_empty(candidates)
    tasks = task({"role":{"kind":"lit","value":"planner"},"task":{"kind":"lit","value":"""Turn these flux-harness improvement candidates into AT MOST 2 concrete, small, safe engineering tasks for the flux codebase (tool specs, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix). Do NOT touch crates/flux-eval, bench/, the loop flows, or CI. Candidates:
{{candidates}}

Return ONLY the JSON array of tasks."""}})
    snapshot = git_snapshot()
    implemented = change_implement({"limit":{"kind":"lit","value":2},"tasks":{"kind":"var","name":"tasks"}})
    guard = guard_protected(snapshot)
    gate = gate_check()
    when gate
      candidate = eval_run({"adapter":"multi","members":[{"adapter":"terminal-bench","agent_timeout_secs":180,"tasks":["chess-best-move","fibonacci-server"]},{"adapter":"synthetic"}],"model":"anthropic/claude-sonnet-4-6","trials":3})
      cand_score = eval_scalar(candidate)
      when score_compare_multi({"baseline":{"kind":"var","name":"baseline"},"candidate":{"kind":"var","name":"candidate"}})
        git_stage(["."])
        git_commit("improve: adopt candidate (multi-eval gain)")
        git_tag("improve-multi-{{cand_score}}")
        improve_log({"record":{"base_score":"{{base_score}}","bench":"multi","cand_score":"{{cand_score}}","decision":"kept","gate":"{{gate}}","guard":"{{guard}}","reason":"candidate_beat_baseline","tag":"improve-multi-{{cand_score}}","tasks":"{{tasks}}"}})
        baseline = eval_adopt(candidate)
      else
        git_reset(snapshot)
        improve_log({"record":{"base_score":"{{base_score}}","bench":"multi","cand_score":"{{cand_score}}","decision":"reverted","gate":"{{gate}}","guard":"{{guard}}","reason":"no_improvement","tasks":"{{tasks}}"}})
    else
      git_reset(snapshot)
      improve_log({"record":{"base_score":"{{base_score}}","bench":"multi","decision":"reverted","gate":"{{gate}}","guard":"{{guard}}","reason":"gate_failed","tasks":"{{tasks}}"}})
    candidates = candidates_advance(candidates)
  return baseline
