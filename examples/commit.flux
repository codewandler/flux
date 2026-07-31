# commit.flux — safely commit only explicitly named paths; never pushes.
#
# Run with:
#   flux flow run examples/commit.flux \
#     --inputs '{"paths":["crates/flux-flow/src/staged.rs"],"title":"feat(flow): enrich declare_intent schema","body":"- Add typed intent fields for planning.\n- Preserve compatibility with older declarations."}'
#
# The flow refuses a pre-populated index, stages only the supplied paths, and presents separate
# approval gates before staging and committing. The final git_commit action is also previewed by the
# safety envelope. It never calls git_push.

flow commit(paths: List<String>, title: String, body: String) -> String
goal "Create one focused commit from explicit paths without absorbing another session's work."

  assert $paths, "At least one workspace-relative path is required."

  $valid_title = regex_match({ s: $title, pattern: "^(feat|fix|refactor|perf|test|docs|chore|style)(\\([a-z0-9][a-z0-9,._-]*\\))?!?: [a-z0-9].+$" })
  assert $valid_title, "Title must be `type(scope): imperative description` (or `type(scope)!:` for breaking changes)."

  $valid_body = regex_match({ s: $body, pattern: "(?m)^- \\S.+" })
  assert $valid_body, "Commit body must contain at least one `- ` bullet explaining what changed and why."

  parallel
    branch $status_before
      git_status()
    branch $staged_before
      git_diff({ staged: true })
    branch $recent
      git_log({ limit: 5 })

  assert $staged_before == "no changes", "The index already contains staged changes. Refusing to absorb or alter another session's work."

  observe({ kind: "commit.proposal", data: { paths: $paths, title: $title, body: $body, status: $status_before, recent: $recent } })

  confirm "Stage only the explicitly selected paths shown in the commit proposal?", risk: medium
    git_stage({ paths: $paths })

  $staged = git_diff({ staged: true })
  assert $staged != "no changes", "The selected paths produced no staged changes."

  observe({ kind: "commit.staged_patch", data: { title: $title, body: $body, diff: $staged } })

  confirm "Commit the staged patch shown in the preceding observation?", risk: high
    $commit = git_commit({ message: $title, body: $body })

  $status_after = git_status()

  return fmt("{commit}\n\nRemaining working-tree changes were not staged or committed:\n{status_after}")
