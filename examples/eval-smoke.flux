flow eval_smoke
  baseline = eval_run("mock")
  sessions = eval_sessions(baseline)
  mined = painpoints_collect(sessions)
  candidates = improvements_aggregate({"mined":{"kind":"var","name":"mined"},"reviewed":{"kind":"lit","value":"[]"}})
  return candidates
