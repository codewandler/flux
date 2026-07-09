# data-transforms.flux — hermetic pure data-shaping example for the L-46..L-52 epic.
#
# Run with: `flux flow run examples/data-transforms.flux`

flow data-transforms
  $issues = [{"id":1,"state":"opened","author":{"username":"ada"},"labels":["bug","ui"],"score":5,"title":"Fix login","body":"release v1.2.3 by ada@example.com"},{"id":2,"state":"closed","author":{"username":"ben"},"labels":["docs"],"score":2,"title":"Docs polish","body":"no version here"},{"id":3,"state":"opened","author":{"username":"ada"},"labels":["bug","backend"],"score":9,"title":"API timeout","body":"release v1.3.0 by ops@example.com"}]
  $authors = map({ items: $issues, path: "author.username" })
  $scores_x2 = map({ items: $issues, expr: "it.score * factor", vars: { factor: 2 } })
  $opened = filter({ items: $issues, where: "it.state == 'opened' && it.score > min", vars: { min: 3 } })
  $label_lists = map({ items: $issues, path: "labels" })
  $labels = flatten({ items: $label_lists })
  $tail_authors = skip({ items: $authors, n: 1 })
  $author_line = join({ items: $authors, sep: "," })
  $author_parts = split({ s: $author_line, sep: ",", trim: true })
  $total_score = sum({ items: $issues, path: "score" })
  $by_state = count_by({ items: $issues, path: "state" })
  $by_author = group_by({ items: $issues, path: "author.username" })
  $has_high = any({ items: $issues, where: "it.score > 8" })
  $opened_all_open = all({ items: $opened, where: "it.state == 'opened'" })
  $has_bug = has({ items: $labels, value: "bug" })
  $slim = pick({ items: $issues, keys: ["id", "state", "score"] })
  $public_first = omit({ items: $issues.0, keys: ["body"] })
  $merged = merge_obj({ objects: [{ "state": "unknown", "owner": "" }, { "state": "opened", "owner": "triage" }] })
  $owner = coalesce({ values: ["", null, $merged.owner], default: "unassigned" })
  $merged_keys = keys({ item: $merged })
  $merged_values = values({ item: $merged })
  $mentions_version = regex_match({ s: $issues.0.body, pattern: "v\\d+\\.\\d+\\.\\d+" })
  $bodies = map({ items: $issues, path: "body" })
  $body_text = join({ items: $bodies, sep: "\n" })
  $versions = regex_extract({ s: $body_text, pattern: "v(\\d+\\.\\d+\\.\\d+)", group: 1, all: true })
  return { authors: $authors, scores_x2: $scores_x2, opened: $opened, labels: $labels, tail_authors: $tail_authors, author_parts: $author_parts, total_score: $total_score, by_state: $by_state, by_author: $by_author, has_high: $has_high, opened_all_open: $opened_all_open, has_bug: $has_bug, slim: $slim, public_first: $public_first, owner: $owner, merged_keys: $merged_keys, merged_values: $merged_values, mentions_version: $mentions_version, versions: $versions }
