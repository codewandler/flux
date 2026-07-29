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
# reason — `// flux-allow-direct-io: <why>` — in the run of comment lines DIRECTLY ABOVE the call
# (which is how every real exception is written). An unannotated match, or a silent omission of a
# file from scope, is a failure.
#
# The scan is string/char/comment-aware (a small awk tokenizer). It matters for a *security* lint
# that it cannot be fooled in the unsafe direction — a brace inside a string literal must not throw
# off the `#[cfg(test)]` tracking, a `//` inside a string must not truncate the line, and the
# allow-marker must be honoured only when it is a real comment, never text inside a string or path.
#
#   scripts/check-no-direct-io.sh              # scan the scoped crates
#   scripts/check-no-direct-io.sh --self-test  # prove the check flags a direct open and honours the
#                                              # cfg(test), comment and allow-annotation exemptions,
#                                              # and resists the four known text-handling bypasses
#
# Exit 0 clean, 1 an unannotated direct-IO call (a real failure).
#
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; }

# The crates in scope. Kept as a plain list so the scope is greppable and reviewable in one place.
SCOPED_CRATES="flux-tools flux-web flux-capabilities"

# scan_file <path> — print "<lineno>\t<snippet>" for every unannotated, non-test direct-IO call in
# one Rust source file.
#
# The awk program is a character-level tokenizer. For each line it produces two derived strings:
#   code    — the line with every string/char literal and every comment blanked out, so brace
#             counting, `#[cfg(test)]` detection and the IO-pattern match run over *real code only*;
#   comment — the text of the comments on the line, so the allow-marker is recognised only inside an
#             actual comment.
# String (`"…"`, byte `b"…"`, raw `r#"…"#`), char (`'x'`, distinguished from a `'a` lifetime) and
# block-comment (`/* … */`) states persist across lines where the language allows it, so no literal
# or comment can leak a brace, a `//`, or the marker text into the code stream.
scan_file() {
  awk '
    function is_ident(ch) { return ch ~ /[A-Za-z0-9_]/ }
    BEGIN {
      depth = 0; in_test = 0; test_base = 0; pending = 0; allow = 0
      st = 0; hashes = 0; prevcode = ""          # st: 0 code 1 //line 2 /* */ 3 "str" 4 char 5 raw
      sq = sprintf("%c", 39); dq = sprintf("%c", 34); bs = sprintf("%c", 92)
    }
    {
      raw = $0
      n = length(raw)
      code = ""; comment = ""
      if (st == 1) st = 0                         # line comments never cross a newline
      if (st == 4) st = 0                         # an unterminated char literal is a misparse; reset
      i = 1
      while (i <= n) {
        c = substr(raw, i, 1)
        nc = (i < n) ? substr(raw, i + 1, 1) : ""
        if (st == 0) {
          if (c == "/" && nc == "/") { st = 1; i += 2; continue }
          if (c == "/" && nc == "*") { st = 2; i += 2; continue }
          if (c == dq)               { st = 3; i += 1; continue }
          if (c == "r" && !is_ident(prevcode) && (nc == dq || nc == "#")) {
            j = i + 1; h = 0
            while (j <= n && substr(raw, j, 1) == "#") { h++; j++ }
            if (j <= n && substr(raw, j, 1) == dq) { st = 5; hashes = h; i = j + 1; continue }
            code = code c; prevcode = c; i += 1; continue           # a bare identifier `r…`
          }
          if (c == sq) {
            if (nc == bs)                    { st = 4; i += 1; continue }   # char with escape
            if (substr(raw, i + 2, 1) == sq) { st = 4; i += 1; continue }   # simple char literal
            code = code c; prevcode = c; i += 1; continue                   # a lifetime tick
          }
          code = code c
          if (c ~ /[^[:space:]]/) prevcode = c
          i += 1; continue
        }
        if (st == 1) { comment = comment c; i += 1; continue }
        if (st == 2) {
          if (c == "*" && nc == "/") { st = 0; i += 2; continue }
          comment = comment c; i += 1; continue
        }
        if (st == 3) {
          if (c == bs) { i += 2; continue }
          if (c == dq) { st = 0; i += 1; continue }
          i += 1; continue
        }
        if (st == 4) {
          if (c == bs) { i += 2; continue }
          if (c == sq) { st = 0; i += 1; continue }
          i += 1; continue
        }
        if (st == 5) {                              # raw string: close on the quote + hashes count
          if (c == dq) {
            ok = 1
            for (k = 1; k <= hashes; k++) if (substr(raw, i + k, 1) != "#") { ok = 0; break }
            if (ok) { st = 0; i += 1 + hashes; continue }
          }
          i += 1; continue
        }
      }

      codetrim = code; gsub(/[[:space:]]/, "", codetrim)
      purecomment = (codetrim == "" && comment != "")
      blankline   = (codetrim == "" && comment == "")

      # --- #[cfg(test)] region tracking, over real code only ---
      if (code ~ /#\[cfg\(test\)\]/) pending = 1
      t = code; opens  = gsub(/\{/, "", t)
      t = code; closes = gsub(/\}/, "", t)
      if (pending) {
        if (opens > 0)                    { in_test = 1; test_base = depth; pending = 0 }
        else if (code ~ /;[[:space:]]*$/) { pending = 0 }   # bodyless test item, e.g. `use …;`
      }

      # --- allow-annotation block + violation, honouring only comment-borne markers above the call ---
      if (purecomment) {
        if (comment ~ /flux-allow-direct-io/) allow = 1
      } else if (blankline) {
        allow = 0
      } else {
        if (!in_test && code ~ /std::fs::|tokio::fs::|std::process::Command|tokio::process::Command|Connection::open|TcpStream::connect|UnixStream::connect/) {
          if (!allow) { snip = raw; sub(/^[[:space:]]+/, "", snip); printf "%d\t%s\n", NR, snip }
        }
        allow = 0                                    # a code line ends the annotation block
      }

      depth += opens - closes
      if (in_test && depth <= test_base) in_test = 0
    }
  ' "$1"
}

# --self-test: the failing-first proof. A synthetic fixture holds the calls that MUST be flagged
# (an unannotated open; and one instance of each of the four known text-handling bypasses) and the
# calls that MUST NOT (a real comment-above annotation, a cfg(test) call, a prose mention). It proves
# red on every real call and green on every exemption, without weakening any guard in shipped code.
if [ "${1:-}" = "--self-test" ]; then
  fixture="$(mktemp -t direct-io-selftest.XXXXXX.rs)"
  trap 'rm -f "$fixture"' EXIT
  cat > "$fixture" <<'RS'
// Prose that names std::fs::write and Connection::open but is a comment, not a call: MUST NOT flag.
fn baseline(path: &str) {
    // flux-allow-direct-io: fixture — a real comment-above annotation exempts the next call.
    let ok_annotated = std::fs::create_dir_all(path);   // GOODANNOT — must stay green
}

// Bug #2: a // inside a string literal must not truncate the code after it (same line).
fn bug2() {
    let u = "http://example.test"; let _ = std::fs::write(u, b"y"); // BUG2 — must flag
}

// Bug #3: a same-line marker must not exempt other IO on the line (marker only counts above a call).
fn bug3(p: &str) {
    let _ = std::fs::write(p, b"BUG3"); let _ok = Connection::open(p); // flux-allow-direct-io: nope
}

// Bug #4: the marker text inside a string/path must not exempt the call.
fn bug4() {
    let _ = std::fs::read("flux-allow-direct-io-BUG4.txt");            // BUG4 — must flag
}

// Bug #1: a net-imbalanced brace inside a string in a cfg(test) region must not disarm the file.
#[cfg(test)]
fn brace_in_test_string() {
    let _brace = "{{{ unbalanced BUG1CFGTEST";
    let _ = std::fs::remove_dir_all("GOODCFGTEST");     // in cfg(test): MUST NOT flag
}

// After the cfg(test) item above, prod code must still be scanned.
fn after_test() {
    let _ = std::fs::write("BUG1", b"x");               // BUG1 — must flag (the disarm is fixed)
}
RS

  out="$(scan_file "$fixture")"

  ok=1
  # Every one of these real calls must appear in the output.
  for needle in BUG1 BUG2 BUG3 BUG4; do
    if ! printf '%s' "$out" | grep -q "$needle"; then
      fail "self-test: expected a violation for $needle but it was not flagged"; ok=0
    fi
  done
  # None of these exemptions may appear.
  for needle in GOODANNOT GOODCFGTEST; do
    if printf '%s' "$out" | grep -q "$needle"; then
      fail "self-test: $needle was flagged but should be exempt"; ok=0
    fi
  done

  if [ "$ok" -ne 1 ]; then
    echo "self-test scan output was:" >&2
    printf '%s\n' "$out" >&2
    exit 1
  fi
  printf '\033[32mPASS\033[0m self-test: real opens flagged (incl. the 4 text-handling bypasses); cfg(test), comment and allow-annotation exemptions honoured\n'
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
  echo "genuine, contained exception — add a reasoned annotation on the line(s) directly above it:" >&2
  echo "  // flux-allow-direct-io: <why this is safe>" >&2
  echo "  let conn = Connection::open(&p)?;" >&2
  exit 1
fi

printf '\033[32mPASS\033[0m %s file(s) scanned, no unannotated direct IO in %s\n' "$scanned" "$SCOPED_CRATES"
