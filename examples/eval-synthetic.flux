flow eval_synthetic -> EvalReport
  report = eval_run({"adapter":"synthetic","model":"openrouter/anthropic/claude-sonnet-4.6","trials":1})
  md = eval_report_md(report)
  return report
