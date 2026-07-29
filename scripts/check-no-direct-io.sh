#!/usr/bin/env bash
#
# check-no-direct-io.sh — model-facing tool crates must not reach the filesystem, a process, a
# database or a socket directly; all real IO goes through `flux-system`, whose jail, symlink
# rejection and canonicalization are the confinement the tool policy assumes.
#
# Why this exists (C-194). `docs/architecture.md`'s "Invariants worth never breaking" states:
# *"All IO goes through `flux-system`; tools never touch `std::fs`/`std::process` directly."* Until
# now nothing checked it. The layering lint (`flux-codegate`) sees crate-dependency *direction*, not
# `std::fs` use inside an allowed edge; `validate_authority_contracts` checks a spec is internally
# coherent, not that `execute` is faithful to it. Neither could have caught C-192, where the
# `sqlite_query` tool opened a DB directly with `rusqlite::Connection::open*` and `VACUUM INTO` wrote
# a file at an arbitrary path — a workspace-jail escape flux-system would have refused. This lint
# turns the invariant into a gate: the next direct-IO call in a tool crate fails CI at authoring time.
#
# Scope (the model-facing tool crates named by C-194):
#   flux-tools, flux-web, flux-capabilities.
# Deliberately OUT of scope, and why:
#   - flux-eval       — a test/benchmark harness, not a model-facing tool; reading fixtures and
#                       writing result artifacts to disk is its whole purpose, and it never runs on
#                       model-controlled input the way a tool does. Linting it would be all noise.
#   - flux-plugin     — the plugin *host*: launching and supervising plugin subprocesses and
#                       brokering their guarded IO is intrinsically its job. Model-facing tool code
#                       does not live here; plugin-side IO is confined on the plugin side (the
#                       separate `plugins/` workspace) and through the protocol's egress audit.
#
# The rule. Outside `#[cfg(test)]`, a scoped crate's `src/` may not name any of:
#   std::fs::            tokio::fs::
#   std::process::Command   tokio::process::Command
#   Connection::open…    (rusqlite direct DB open — the C-192 primitive)
#   TcpStream::connect   UnixStream::connect
# A genuinely legitimate exception (a backend that owns its store, host-infra persistence, the
# already-guarded sqlite read path) is admitted ONLY by an explicit, greppable annotation carrying a
# reason — `// flux-allow-direct-io: <why>` — on the offending line or the line directly above it. An
# unannotated match, or a silent omission of a file from scope, is a failure. Occurrences inside line
# comments do not count (documentation may name the APIs it forbids).
#
#   scripts/check-no-direct-io.sh              # scan the scoped crates
#   scripts/check-no-direct-io.sh --self-test  # prove the check flags a direct open and honours the
#                                              # cfg(test), comment and allow-annotation exemptions
#
# Exit 0 clean, 1 an unannotated direct-IO call (a real failure).
#
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; }

# The crates in scope. Kept as a plain list so the scope is greppable and reviewable in one place.
SCOPED_CRATES="flux-tools flux-web flux-capabilities"

# scan_file <path> — print "<lineno>\t<snippet>" for every unannotated, non-test direct-IO call in
# one Rust source file. The awk program tracks `#[cfg(test)]`-attributed item bodies by brace depth
# so a match inside a test module (or a test-only fn) is exempt, strips line comments before matching
# so prose that names these APIs never trips, and honours a `flux-allow-direct-io` marker on the
# match line or the line directly above it.
scan_file() {
  awk '
    BEGIN { depth = 0; in_test = 0; test_base = 0; pending = 0; allow = 0 }
    {
      raw = $0
      code = raw
      sub(/\/\/.*/, "", code)            # drop line comments for matching + brace counting

      is_comment = (raw ~ /^[[:space:]]*\/\//)
      is_blank   = (raw ~ /^[[:space:]]*$/)
      has_marker = (raw ~ /flux-allow-direct-io/)

      # A cfg(test) attribute arms the next item body as a test region.
      if (code ~ /#\[cfg\(test\)\]/) pending = 1

      t = code; opens  = gsub(/\{/, "", t)
      t = code; closes = gsub(/\}/, "", t)

      if (pending && opens > 0) {
        in_test = 1; test_base = depth; pending = 0
      } else if (pending && code ~ /;[[:space:]]*$/) {
        # a bodyless test item (e.g. `#[cfg(test)] use ...;`) — skip just this line
        pending = 0; depth += opens - closes; next
      }

      # An allow-annotation may sit on the offending line or anywhere in the contiguous run of
      # comment lines directly above it. Accumulate the marker across that comment block; a blank
      # line or a plain code line ends the block and clears it.
      if (is_comment) {
        if (has_marker) allow = 1
      } else if (is_blank) {
        allow = 0
      } else {
        if (!in_test && code ~ /std::fs::|tokio::fs::|std::process::Command|tokio::process::Command|Connection::open|TcpStream::connect|UnixStream::connect/) {
          if (!(has_marker || allow)) {
            snip = raw; sub(/^[[:space:]]+/, "", snip)
            printf "%d\t%s\n", NR, snip
          }
        }
        allow = 0                        # a code line ends the annotation block
      }

      depth += opens - closes
      if (in_test && depth <= test_base) in_test = 0
    }
  ' "$1"
}

# --self-test: the failing-first proof. A synthetic fixture file holds one unannotated direct open
# (must be flagged), plus a comment mention, an allow-annotated call, a same-line-annotated call and
# a call inside `#[cfg(test)] mod tests` (none of which may be flagged). Proves red on the bad line
# and green on every exemption, without weakening any guard in shipped code.
if [ "${1:-}" = "--self-test" ]; then
  fixture="$(mktemp -t direct-io-selftest.XXXXXX.rs)"
  trap 'rm -f "$fixture"' EXIT
  cat > "$fixture" <<'RS'
// This comment names std::fs::write and Connection::open but is prose, not a call.
fn persist(path: &str) {
    let bad = std::fs::write(path, b"x");          // MUST be flagged (line 4)
    // flux-allow-direct-io: fixture — allowed via annotation on the line above
    let ok1 = std::fs::create_dir_all(path);
    let ok2 = Connection::open(path); // flux-allow-direct-io: fixture — same-line annotation
}
#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        let _ = std::fs::remove_dir_all("/tmp/x");  // in cfg(test): MUST NOT be flagged
        let _ = Connection::open(":memory:");
    }
}
RS

  out="$(scan_file "$fixture")"
  count="$(printf '%s' "$out" | grep -c . || true)"

  if [ "$count" -ne 1 ]; then
    fail "self-test: expected exactly 1 violation, got $count:"
    printf '%s\n' "$out" >&2
    exit 1
  fi
  if ! printf '%s' "$out" | grep -q 'std::fs::write'; then
    fail "self-test: the one flagged line was not the unannotated std::fs::write:"
    printf '%s\n' "$out" >&2
    exit 1
  fi
  printf '\033[32mPASS\033[0m self-test: unannotated direct open flagged; cfg(test), comment and allow-annotation exemptions honoured\n'
  exit 0
fi

echo "== direct-IO in model-facing tool crates ($SCOPED_CRATES) =="
violations=0
scanned=0
for crate in $SCOPED_CRATES; do
  crate_dir="crates/$crate/src"
  [ -d "$crate_dir" ] || { fail "scope crate missing: $crate_dir"; violations=$((violations + 1)); continue; }
  while IFS= read -r f; do
    scanned=$((scanned + 1))
    while IFS=$'\t' read -r lineno snip; do
      [ -n "$lineno" ] || continue
      fail "$f:$lineno: direct IO without a flux-allow-direct-io annotation: $snip"
      violations=$((violations + 1))
    done < <(scan_file "$f")
  done < <(find "$crate_dir" -name '*.rs' -type f | sort)
done

if [ "$violations" -gt 0 ]; then
  echo >&2
  echo "$violations direct-IO call(s) above bypass flux-system's jail." >&2
  echo "Route the IO through flux-system (ctx.system() / flux_system::…), or — if the call is a" >&2
  echo "genuine, contained exception — annotate the line with its reason:" >&2
  echo "  let conn = Connection::open(&p)?; // flux-allow-direct-io: <why this is safe>" >&2
  exit 1
fi

printf '\033[32mPASS\033[0m %s file(s) scanned, no unannotated direct IO in %s\n' "$scanned" "$SCOPED_CRATES"
