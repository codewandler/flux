flow improve_tbench -> EvalReport
  baseline = eval_run({"adapter":"terminal-bench","agent_timeout_secs":180,"model":"anthropic/claude-sonnet-4-6","tasks":["fibonacci-server"],"trials":5})
  base_score = eval_scalar(baseline)
  reviewed = task(role: "reviewer", task: fmt("""These are flux's terminal-bench results. Each failing case names a task, a failure_mode, and (when partial) which sub-checks failed (failed_checks). Identify flux HARNESS improvements (tools, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix) that would help flux pass more. Results:
{baseline}

Return ONLY a JSON array: [{"area":..,"symptom":..,"suggested_fix":..,"severity":1-5}]."""))
  candidates = improvements_aggregate(mined: "[]", reviewed)
  repeat 1, until: candidates_empty(candidates)
    tasks = task(role: "planner", task: fmt("""Turn these flux-harness improvement candidates into AT MOST 2 concrete, small, safe engineering tasks for the flux codebase (tool specs, tool output/views, system prompt, a new tool, or an agent-loop efficiency fix). Do NOT touch crates/flux-eval, bench/, the loop flows, or CI. The candidates are RANKED by measured weight — take them in order and only skip one if it is infeasible or out of bounds; every task must name the candidate id it addresses in an `addresses` field. Candidates:
{candidates}

Return ONLY the JSON array of tasks."""))
    snapshot = git_snapshot()
    implemented = change_implement(limit: 2, tasks)
    implemented_count = implemented.implemented?
    when implemented_count
      guard = guard_protected(snapshot)
      gate = gate_check()
      when gate
        candidate = eval_run({"adapter":"terminal-bench","agent_timeout_secs":180,"model":"anthropic/claude-sonnet-4-6","tasks":["fibonacci-server"],"trials":5})
        cand_score = eval_scalar(candidate)
        when score_compare(baseline, candidate)
          git_stage(["."])
          git_commit("improve: adopt candidate (terminal-bench gain)")
          git_tag("improve-tbench-{{cand_score}}")
          improve_log({"record":{"base_score":"{{base_score}}","bench":"terminal-bench","cand_score":"{{cand_score}}","decision":"kept","gate":"{{gate}}","guard":"{{guard}}","reason":"candidate_beat_baseline","tag":"improve-tbench-{{cand_score}}","tasks":"{{tasks}}"}})
          baseline = eval_adopt(candidate)
        else
          git_reset(snapshot)
          improve_log({"record":{"base_score":"{{base_score}}","bench":"terminal-bench","cand_score":"{{cand_score}}","decision":"reverted","gate":"{{gate}}","guard":"{{guard}}","reason":"no_improvement","tasks":"{{tasks}}"}})
      else
        git_reset(snapshot)
        improve_log({"record":{"base_score":"{{base_score}}","bench":"terminal-bench","decision":"reverted","gate":"{{gate}}","guard":"{{guard}}","reason":"gate_failed","tasks":"{{tasks}}"}})
    else
      improve_log({"record":{"base_score":"{{base_score}}","bench":"terminal-bench","decision":"reverted","implemented":"{{implemented}}","reason":"no_implementation","tasks":"{{tasks}}"}})
    candidates = candidates_advance(candidates)
  return baseline
