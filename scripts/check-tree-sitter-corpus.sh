#!/usr/bin/env bash
#
# check-tree-sitter-corpus.sh — the grammar revision `.helix/languages.toml` PINS parses every
# canonical `.flux` example with zero `ERROR` and zero `MISSING` nodes.
#
# Why this exists (C-334). `AGENTS.md` concedes it outright: editor-tooling mirrors are manual, and
# only ONE of the four was guarded. The asymmetry is the sharp part — the guarded mirror is the
# website's Prism grammar, and Prism is the only one of the four that CANNOT break parsing. It
# mis-colours. The three unguarded mirrors are exactly the three that put a red squiggle under
# working code in a real editor.
#
# That is not a hypothetical. Two separate failures, both found by hand:
#
#   1. C-301 — 0.39.0 made duration suffixes canonical (`delay: 500ms`), the tree-sitter grammar
#      still lexed `500` and choked on `ms`, and Helix/Neovim/Zed reported idiomatic Flux as a
#      syntax error for multiple releases.
#   2. The deeper one. The grammar repo already contained TWO landed improvements — L-96
#      named-option headers and `permissions` declarations — that reached NOBODY, because
#      `.helix/languages.toml`'s pinned rev never moved off `29cff6c`. The mirror work was done. It
#      landed nowhere, and nothing noticed.
#
# So the failure mode guarded here is not only "nobody mirrored the grammar change" but **"the pin
# does not reflect the mirror"**. This check therefore resolves the rev FROM THE PIN and parses with
# that exact revision's committed parser — never with whatever the grammar repo's `main` happens to
# be, which is precisely the state that looked fine while editors were broken.
#
# WHAT IT PARSES WITH. The revision's committed `src/parser.c` + `src/scanner.c`, NOT a parser
# regenerated from `grammar.js`. That is deliberate: those two C files are the artifact Helix
# (`hx --grammar build`) and nvim-treesitter (`files = { "src/parser.c", "src/scanner.c" }`)
# actually compile. Running `tree-sitter generate` first would test `grammar.js` instead, and would
# go green on a rev whose committed parser was never regenerated — a rev that is broken in every
# editor on earth.
#
# WHAT IT DOES **NOT** COVER — say it rather than leave a reader assuming four-way coverage:
#
#   * the TextMate grammar and the IntelliJ grammar in `codewandler/flux-editors`. Both are
#     highlight-only: they can mis-colour a construct, they cannot fail to parse it, so a stale one
#     is a cosmetic defect rather than a red squiggle. That is why they rank below this check — but
#     they are still unguarded, and syntax work still has to propagate to them by hand.
#   * the website Prism grammar. Already guarded, narrowly, by
#     `crates/flux-lang/tests/named_option_headers.rs` (canonical header-option labels only).
#   * the grammar's own highlight QUERIES. A query that stops matching costs colour, not parsing.
#   * whether the pin is the NEWEST grammar revision. This check answers "does the pinned rev parse
#     canonical Flux", not "is there a better rev". A pin left behind on a rev that still parses the
#     corpus passes here, correctly — the corpus is the contract.
#
# THE CORPUS is `examples/*.flux`, and deliberately no second one. Those files are already swept
# mechanically by two Rust tests — `crates/flux-lang/tests/cst_agreement.rs` (frozen AST SHA-256)
# and `crates/flux-eval/tests/examples_validate.rs` (a real `read_dir` sweep, so a NEW example is
# guarded the moment it is added). A private corpus for the grammar would drift from the one the
# compiler is tested against, and then the two would disagree with nothing to say which was right.
#
#   scripts/check-tree-sitter-corpus.sh              # parse the corpus with the pinned rev
#   scripts/check-tree-sitter-corpus.sh --rev <sha>  # ...with some other rev (history proofs)
#   scripts/check-tree-sitter-corpus.sh --keep       # leave the work tree behind for inspection
#   scripts/check-tree-sitter-corpus.sh --self-test  # prove the check catches ERROR and MISSING
#
# Exit 0 clean, 1 the pinned grammar cannot parse the corpus (a real failure), 2 the grammar or the
# tree-sitter CLI could not be obtained (a logged skip — this check needs the network, and a GitHub
# or npm outage must not turn main red). Same convention, same reasoning, as
# `scripts/check-release-tags.sh`; do not invent a second one.
#
# ⚠ It needs the network, so it is NOT a PR gate — `.github/workflows/tree-sitter-corpus.yml` runs it
# nightly and on demand, and on no push, PR or tag. That workflow's header records why, and records
# that the lane is RED at the pin it landed with: 7 of the 15 examples do not parse, on constructs
# the grammar never supported. The fix is upstream, not an allowlist here.
#
set -uo pipefail

fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; }

# Field separator for the internal `kind<TAB>row<TAB>col<TAB>...` records the functions below pass
# around. A literal tab in source is invisible and gets eaten by editors; name it once.
TAB="$(printf '\t')"

# --- pure text functions, driven directly by --self-test -----------------------------------------

# The grammar pin, read from the text of `.helix/languages.toml` ($1). Prints `url<TAB>rev`.
#
# It reads the ONE `source = { git = ..., rev = ... }` line, and fails when there is not exactly one:
# a second `[[grammar]]` block, or a reformatted pin this regex no longer matches, must stop the
# check rather than let it silently audit some other grammar — or, worse, fall back to a branch tip
# and report a green that says nothing about what editors install.
read_pin() {
  local text="$1" lines url rev
  lines="$(printf '%s\n' "$text" |
    grep -E '^[[:space:]]*source[[:space:]]*=[[:space:]]*\{[[:space:]]*git[[:space:]]*=')"
  if [ "$(printf '%s\n' "$lines" | grep -c .)" -ne 1 ]; then
    echo "expected exactly one pinned grammar source line, found $(printf '%s\n' "$lines" | grep -c .)"
    return 1
  fi
  url="$(printf '%s\n' "$lines" | sed -nE 's/.*git[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p')"
  rev="$(printf '%s\n' "$lines" | sed -nE 's/.*rev[[:space:]]*=[[:space:]]*"([0-9a-f]{40})".*/\1/p')"
  if [ -z "$url" ] || [ -z "$rev" ]; then
    echo "pin line has no git url and/or no 40-char rev: $lines"
    return 1
  fi
  printf '%s%s%s\n' "$url" "$TAB" "$rev"
}

# Every defect in a `tree-sitter parse` tree ($1), one `kind<TAB>row<TAB>col<TAB>endrow<TAB>endcol`
# record per node, rows and columns 0-based exactly as tree-sitter prints them. `detail` (the token
# a MISSING node names, e.g. `"}"`) is appended when the node carries one.
#
# Both kinds matter and they are different defects: an ERROR node is a span the parser could not fit
# into any rule, a MISSING node is a token the parser INVENTED to recover — which prints no error
# text at all and is the easier of the two to read straight past.
scan_tree() {
  printf '%s\n' "$1" | sed -nE \
    "s/^[[:space:]]*\((ERROR|MISSING)([^[]*)\[([0-9]+), ([0-9]+)\] - \[([0-9]+), ([0-9]+)\].*/\1${TAB}\3${TAB}\4${TAB}\5${TAB}\6${TAB}\2/p"
}

# One human-readable defect line: `path:line:col  KIND  <the construct>`, 1-based like every editor
# and every compiler. $1 path, $2 kind, $3..$6 the 0-based span, $7 the detail, $8 the source line.
#
# Naming the CONSTRUCT is the point — "advanced-code-review.flux has an error" sends a reader
# hunting, `zendesk.triage.flux:63:41 ERROR `ms`` says which spelling the pinned grammar rejects.
format_defect() {
  local path="$1" kind="$2" row="$3" col="$4" endrow="$5" endcol="$6" detail="$7" src="$8"
  local construct
  if [ "$row" = "$endrow" ] && [ "$endcol" -gt "$col" ]; then
    construct="$(printf '%s' "$src" | cut -c "$((col + 1))-$endcol")"
  else
    construct="$(printf '%s' "$src" | sed -E 's/^[[:space:]]+//')"
  fi
  # A MISSING node is zero-width, so it has no text of its own; its detail names the token the
  # parser had to invent. Prefer that over the surrounding line.
  detail="$(printf '%s' "$detail" | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"
  if [ "$kind" = "MISSING" ] && [ -n "$detail" ]; then
    construct="inserted $detail"
  fi
  printf '%s:%s:%s  %s  %s\n' "$path" "$((row + 1))" "$((col + 1))" "$kind" "$construct"
}

# --- self-test ------------------------------------------------------------------------------------
#
# The failing-first proof, with no network. Two hazards it is written against:
#
#   * a self-test whose fixture is invented agrees with the parser this script contains, not with
#     tree-sitter. So the ERROR fixture below is REAL captured output — `tree-sitter parse` 0.24.7
#     run against rev 29cff6c (the rev flux pinned until 2026-07-31) on a duration literal, the
#     exact C-301 defect. It is not a guess at the shape.
#   * a self-test that only proves the script REPORTS things proves nothing about the failure it
#     exists to catch. So it also asserts the clean case reports nothing — a scanner that flags
#     every tree would "pass" the first half and be useless.
#
# The real end-to-end proof is stronger than either and is recorded in the story: this script fails
# against 29cff6c and passes against the current pin.
if [ "${1:-}" = "--self-test" ]; then
  # Rule 1 — the pin is read from the real file's shape, including its surrounding comments.
  pin_fixture='[[grammar]]
name = "flux"
# Immutable revision containing the tested parser, queries, and Helix installer.
source = { git = "https://github.com/codewandler/flux-tree-sitter", rev = "9ea98905ef9787c30319e69fb100327c47f8eaee" }'
  got="$(read_pin "$pin_fixture")" || { fail "self-test: read_pin rejected the real pin shape: $got"; exit 1; }
  [ "$got" = "https://github.com/codewandler/flux-tree-sitter${TAB}9ea98905ef9787c30319e69fb100327c47f8eaee" ] || {
    fail "self-test: read_pin returned '$got'"; exit 1; }

  # A pin with no rev must stop the check, not silently resolve to a branch tip — auditing `main`
  # instead of the pinned rev is the exact blind spot C-334 exists to close.
  if read_pin 'source = { git = "https://github.com/codewandler/flux-tree-sitter" }' >/dev/null; then
    fail "self-test: a revless pin was accepted"; exit 1; fi
  # Two grammar blocks: ambiguous, so it must fail rather than pick one.
  if read_pin 'source = { git = "https://example.com/a", rev = "0000000000000000000000000000000000000000" }
source = { git = "https://example.com/b", rev = "1111111111111111111111111111111111111111" }' >/dev/null; then
    fail "self-test: two pinned grammars were accepted"; exit 1; fi

  # Rule 2 — REAL captured output. `tree-sitter parse` 0.24.7, grammar rev 29cff6c, on
  # `delay: 500ms`: the pre-C-301 grammar lexes `500` and cannot place `ms`.
  error_fixture='(source_file [0, 0] - [3, 0]
  (flow_declaration [0, 0] - [3, 0]
    name: (identifier [0, 5] - [0, 8])
    (retry_clause [1, 2] - [2, 0]
      max: (number [1, 8] - [1, 9])
      delay: (number [1, 40] - [1, 43])
      (ERROR [1, 43] - [1, 45])))'
  got="$(scan_tree "$error_fixture")"
  [ "$(printf '%s\n' "$got" | grep -c .)" -eq 1 ] || {
    fail "self-test: scan_tree found $(printf '%s\n' "$got" | grep -c .) defect(s) in the captured ERROR tree, want 1"; exit 1; }
  IFS="$TAB" read -r k r c er ec d <<<"$got"
  [ "$k" = "ERROR" ] || { fail "self-test: scan_tree classified '$k', want ERROR"; exit 1; }
  line='  retry 3, backoff: exponential, delay: 500ms -> $identity'
  got="$(format_defect 'examples/zendesk.triage.flux' "$k" "$r" "$c" "$er" "$ec" "$d" "$line")"
  case "$got" in
    'examples/zendesk.triage.flux:2:44  ERROR  ms') ;;
    *) fail "self-test: the defect line must name the file, the 1-based position and the construct, got '$got'"; exit 1 ;;
  esac

  # Rule 3 — a MISSING node is a defect too, and it is zero-width, so the report has to name the
  # token the parser invented instead of slicing an empty span out of the source line.
  missing_fixture='(source_file [0, 0] - [1, 0]
  (object [0, 0] - [0, 8]
    (MISSING "}" [0, 8] - [0, 8])))'
  got="$(scan_tree "$missing_fixture")"
  [ "$(printf '%s\n' "$got" | grep -c .)" -eq 1 ] || {
    fail "self-test: a MISSING node was not reported as a defect"; exit 1; }
  IFS="$TAB" read -r k r c er ec d <<<"$got"
  [ "$k" = "MISSING" ] || { fail "self-test: scan_tree classified '$k', want MISSING"; exit 1; }
  got="$(format_defect 'examples/x.flux' "$k" "$r" "$c" "$er" "$ec" "$d" '{ "a": 1')"
  case "$got" in
    *'x.flux:1:9  MISSING  inserted "}"') ;;
    *) fail "self-test: the MISSING report must name the invented token, got '$got'"; exit 1 ;;
  esac

  # Rule 4 — the clean case. A scanner that flags every tree would have passed rules 2 and 3 and be
  # worthless; this is what makes them mean something. Real output, current pin, same corpus file.
  clean_fixture='(source_file [0, 0] - [3, 0]
  (flow_declaration [0, 0] - [3, 0]
    name: (identifier [0, 5] - [0, 8])
    (retry_clause [1, 2] - [2, 0]
      max: (number [1, 8] - [1, 9])
      delay: (number [1, 40] - [1, 45]))))'
  got="$(scan_tree "$clean_fixture")"
  [ -z "$got" ] || { fail "self-test: a clean parse tree reported defects: $got"; exit 1; }
  # `error` and `missing` appearing as ordinary identifiers in Flux source must not be mistaken for
  # defect nodes — `catch $error` is idiomatic, and a substring match would fail every such file.
  got="$(scan_tree '(source_file [0, 0] - [1, 0]
  (catch_clause [0, 0] - [1, 0]
    error: (variable [0, 6] - [0, 12])
    (identifier [0, 13] - [0, 20])))')"
  [ -z "$got" ] || { fail "self-test: an ordinary \$error variable was reported as a defect: $got"; exit 1; }

  printf '\033[32mPASS\033[0m self-test: the pin is read exactly, and ERROR/MISSING nodes are caught and named while clean trees are not\n'
  exit 0
fi

# --- the real check ---------------------------------------------------------------------------------

cd "$(git rev-parse --show-toplevel)" || { fail "not inside a git checkout"; exit 2; }

REV_OVERRIDE=""
KEEP=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --rev)
      [ "$#" -ge 2 ] || { fail "--rev needs an argument"; exit 2; }
      REV_OVERRIDE="$2"
      shift 2
      ;;
    --keep) KEEP=1; shift ;;
    -h|--help) sed -n '2,64p' "$0" >&2; exit 0 ;;
    *) fail "unknown argument: $1"; exit 2 ;;
  esac
done

PIN_FILE=".helix/languages.toml"
[ -f "$PIN_FILE" ] || { fail "$PIN_FILE is missing — nothing pins the grammar"; exit 1; }

pin="$(read_pin "$(cat "$PIN_FILE")")" || { fail "$PIN_FILE: $pin"; exit 1; }
IFS="$TAB" read -r GRAMMAR_URL PINNED_REV <<<"$pin"
REV="${REV_OVERRIDE:-$PINNED_REV}"

# The corpus. Empty is a failure, not a pass: a glob that matches nothing would otherwise report a
# confident green while parsing zero files.
corpus=(examples/*.flux)
[ -e "${corpus[0]}" ] || { fail "no examples/*.flux to parse — the corpus this check exists for is empty"; exit 1; }

command -v git >/dev/null 2>&1 || { printf 'skip: git is not installed\n' >&2; exit 2; }
command -v node >/dev/null 2>&1 || { printf 'skip: node is not installed\n' >&2; exit 2; }
command -v npm >/dev/null 2>&1 || { printf 'skip: npm is not installed\n' >&2; exit 2; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/flux-ts-corpus.XXXXXX")" || {
  printf 'skip: could not create a work directory\n' >&2; exit 2; }
cleanup() { [ -n "$KEEP" ] || rm -rf "$WORK"; }
trap cleanup EXIT

GRAMMAR="$WORK/grammar"

# The compiled-parser cache MUST be private to this run. tree-sitter caches by grammar NAME, so a
# shared cache serves whatever `flux.so` some other checkout compiled last — the check would then
# report on a grammar revision it never fetched, and a stale green would look exactly like a real
# one. Same class of trap as sharing CARGO_TARGET_DIR between checkouts.
export TREE_SITTER_LIBDIR="$WORK/ts-lib"
mkdir -p "$TREE_SITTER_LIBDIR"

printf 'grammar: %s\n' "$GRAMMAR_URL"
if [ -n "$REV_OVERRIDE" ]; then
  printf 'rev:     %s  (--rev override; %s is the pin in %s)\n' "$REV" "$PINNED_REV" "$PIN_FILE"
else
  printf 'rev:     %s  (pinned in %s)\n' "$REV" "$PIN_FILE"
fi

# Fetch exactly the pinned revision — depth 1, no branches, no tags. Fetching by SHA is the whole
# point: `main` is not what editors install.
mkdir -p "$GRAMMAR"
git init -q "$GRAMMAR" || { printf 'skip: could not init a work checkout\n' >&2; exit 2; }
git -C "$GRAMMAR" remote add origin "$GRAMMAR_URL" || { printf 'skip: could not configure the remote\n' >&2; exit 2; }
if ! git -C "$GRAMMAR" fetch -q --depth 1 origin "$REV"; then
  printf 'skip: could not fetch %s from %s\n' "$REV" "$GRAMMAR_URL" >&2
  exit 2
fi
git -C "$GRAMMAR" checkout -q FETCH_HEAD || { printf 'skip: could not check out %s\n' "$REV" >&2; exit 2; }

for required in src/parser.c grammar.js package.json; do
  [ -f "$GRAMMAR/$required" ] || {
    fail "rev $REV has no $required — this is not a tree-sitter grammar checkout"; exit 1; }
done

# The tree-sitter CLI, at the version THAT REVISION declares. `--ignore-scripts` keeps npm from
# building the repo's optional Node binding (node-gyp, a C++ toolchain, and nothing here uses it);
# the CLI's own downloader is then run explicitly, from inside its package directory because it
# writes the binary relative to the working directory.
if [ -n "${TREE_SITTER_BIN:-}" ]; then
  TS="$TREE_SITTER_BIN"
  [ -x "$TS" ] || { printf 'skip: TREE_SITTER_BIN=%s is not executable\n' "$TS" >&2; exit 2; }
  printf 'cli:     %s  (TREE_SITTER_BIN override, not the version rev %s declares)\n' "$TS" "$REV"
else
  if ! (cd "$GRAMMAR" && npm install --ignore-scripts --no-audit --no-fund --loglevel=error) >"$WORK/npm.log" 2>&1; then
    printf 'skip: npm install failed in the grammar checkout\n' >&2
    tail -20 "$WORK/npm.log" >&2
    exit 2
  fi
  TS="$GRAMMAR/node_modules/tree-sitter-cli/tree-sitter"
  [ -f "$GRAMMAR/node_modules/tree-sitter-cli/install.js" ] || {
    printf 'skip: rev %s does not declare tree-sitter-cli as a dependency\n' "$REV" >&2; exit 2; }
  if ! (cd "$GRAMMAR/node_modules/tree-sitter-cli" && node install.js) >"$WORK/cli.log" 2>&1; then
    printf 'skip: could not download the tree-sitter CLI\n' >&2
    tail -20 "$WORK/cli.log" >&2
    exit 2
  fi
  [ -x "$TS" ] || { printf 'skip: the tree-sitter CLI was not downloaded\n' >&2; cat "$WORK/cli.log" >&2; exit 2; }
  printf 'cli:     %s\n' "$("$TS" --version 2>/dev/null || echo unknown)"
fi

# `tree-sitter parse` resolves the grammar from the working directory's `src/`, and compiles it on
# first use — so the compiler has to exist. Prove that up front rather than letting a missing `cc`
# surface as an unreadable parse failure that looks like a grammar defect.
command -v cc >/dev/null 2>&1 || { printf 'skip: no C compiler to build the grammar with\n' >&2; exit 2; }

status=0
parsed=0
CORPUS_ROOT="$PWD"

for file in "${corpus[@]}"; do
  out="$(cd "$GRAMMAR" && "$TS" parse "$CORPUS_ROOT/$file" 2>&1)"
  # An unparseable tree (the CLI failed to load or build the grammar) is not a corpus defect —
  # nothing here may report a language failure it did not actually observe.
  case "$out" in
    *'(source_file '*) ;;
    *)
      printf 'skip: the pinned grammar could not be loaded for %s\n' "$file" >&2
      printf '%s\n' "$out" >&2
      exit 2
      ;;
  esac
  parsed=$((parsed + 1))

  defects="$(scan_tree "$out")"
  [ -n "$defects" ] || continue
  status=1
  while IFS="$TAB" read -r kind row col endrow endcol detail; do
    [ -n "$kind" ] || continue
    src="$(sed -n "$((row + 1))p" "$file")"
    format_defect "$file" "$kind" "$row" "$col" "$endrow" "$endcol" "$detail" "$src" >&2
  done <<<"$defects"
done

if [ "$status" -ne 0 ]; then
  fail "grammar rev $REV cannot parse the canonical corpus — every construct above is a red squiggle in Helix, Neovim and Zed."
  printf 'Fix the grammar in %s, then MOVE THE PIN in %s. Landing the fix\n' "$GRAMMAR_URL" "$PIN_FILE" >&2
  printf 'without moving the pin reaches nobody — that is the C-334 failure mode, twice observed.\n' >&2
  exit 1
fi

printf '\033[32mPASS\033[0m grammar rev %s parses all %d canonical example(s) with no ERROR or MISSING nodes\n' \
  "$REV" "$parsed"
exit 0
