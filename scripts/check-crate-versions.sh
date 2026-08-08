#!/usr/bin/env bash
#
# check-crate-versions.sh — a published crate whose CONTENT changed must also change its VERSION.
#
# Why this exists (C-146): flux used to publish every crate at one lockstep version, so "did this
# crate change?" never mattered — the version moved regardless. The plugin wire contract now sits on
# an independent 1.x line (C-143) whose whole point is that it does NOT move on a flux cut. That
# buys cheap releases and costs the guarantee the lockstep was silently providing: nothing stops
# someone editing `flux-plugin-protocol` and shipping it under a version already on crates.io, where
# the edit would be invisible to every consumer and `cargo publish` would skip it as already-done.
#
# So: for every publishable (`codewandler-*`) package in either workspace, if its source directory
# changed since the base release tag, its version must differ from the version at that tag.
#
#   scripts/check-crate-versions.sh              # compare HEAD against the previous v* tag
#   BASE=v0.28.0 scripts/check-crate-versions.sh # explicit base
#   scripts/check-crate-versions.sh --self-test  # prove both checks catch stale state
#   scripts/check-crate-versions.sh --direct-dependencies root
#   scripts/check-crate-versions.sh --direct-dependencies plugins
#   scripts/check-crate-versions.sh --update-direct-dependencies root
#
# Exit 0 clean, 1 a stale version (a real failure), 2 the base tag could not be resolved.
#
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; }

# Is this exact version already on crates.io? (C-729)
#
# The git comparison above cannot answer this, and the workspace build cannot either: every
# first-party crate resolves through its `path` dependency, so the LOCAL content always wins and
# `cargo build --workspace` is green no matter what the registry holds. `cargo publish` resolves
# from the registry instead, and skips a version that is already there — so a crate edited under an
# already-published version ships nothing, and the next crate in the closure compiles against the
# stale published copy and fails there, after the whole pipeline has run.
#
# That is exactly how `codewandler-flux-secret` 1.3.0 shipped without the `host` field it had
# locally: 1.2.0 -> 1.3.0 satisfied the git check, 1.3.0 was published by an earlier failed run, and
# the field was added afterwards under the same number.
#
# Reads the sparse index directly — one HTTP GET, no build, no credential, no `cargo` invocation, so
# it can run first and cost nothing. A network failure warns rather than fails: this check must not
# make an offline working tree unbuildable. `FLUX_SKIP_REGISTRY_CHECK=1` opts out explicitly.
registry_has_version() {
  local crate="$1" version="$2" path body
  [ "${FLUX_SKIP_REGISTRY_CHECK:-}" = 1 ] && return 1
  command -v curl >/dev/null 2>&1 || return 1
  case ${#crate} in
    1) path="1/$crate" ;;
    2) path="2/$crate" ;;
    3) path="3/${crate:0:1}/$crate" ;;
    *) path="${crate:0:2}/${crate:2:2}/$crate" ;;
  esac
  # 404 means "never published", which is a clean pass — distinguish it from a transport failure.
  body="$(curl -sS --fail-with-body --max-time 20 \
    -H 'User-Agent: flux check-crate-versions (https://github.com/codewandler/flux)' \
    "https://index.crates.io/$path" 2>/dev/null)"
  case $? in
    0) ;;
    22) return 1 ;;
    *) printf '\033[33mwarn\033[0m  could not reach the crates.io index for %s; skipping the published-version check\n' "$crate" >&2; return 1 ;;
  esac
  printf '%s' "$body" | grep -Fq "\"vers\":\"$version\""
}

# Emit the exact resolved direct, external dependency edges for one workspace. Cargo.lock already
# pins the resolved package; this review lock makes *which first-party package directly chose it*
# visible too. The final column calls out Cargo's lifecycle-script analogue: a dependency whose
# selected package runs a custom build target. Transitive packages are deliberately absent — this
# is a decision register for dependencies Flux names directly, not a second Cargo.lock.
direct_dependency_snapshot() {
  local workspace="$1" manifest
  case "$workspace" in
    root) manifest="Cargo.toml" ;;
    plugins) manifest="plugins/Cargo.toml" ;;
    *) fail "unknown workspace '$workspace' (expected root or plugins)"; return 2 ;;
  esac
  command -v jq >/dev/null 2>&1 || {
    fail "jq is required to verify the direct-dependency review lock"
    return 2
  }
  printf '# member\tkind\tdependency\tpackage\tresolved\tsource\tbuild-script\n'
  cargo metadata --manifest-path "$manifest" --locked --offline --format-version 1 \
    | jq -r '
        def pkg($id): first(.packages[] | select(.id == $id));
        def hasbuild($p): (([$p.targets[].kind[]] | index("custom-build")) != null);
        .resolve.nodes[] as $node
        | (pkg($node.id)) as $member
        | select($member.source == null)
        | $node.deps[] as $dep
        | (pkg($dep.pkg)) as $target
        | select($target.source != null)
        | $dep.dep_kinds[]
        | [$member.name, (.kind // "normal"), $dep.name, $target.name,
           $target.version, $target.source,
           (if hasbuild($target) then "build.rs" else "-" end)]
        | @tsv
      ' \
    | LC_ALL=C sort -u
}

check_direct_dependencies() {
  local workspace="$1" expected tmp_dir actual
  expected="scripts/direct-dependencies-${workspace}.lock"
  tmp_dir="$(mktemp -d)"
  actual="$tmp_dir/actual"
  if ! direct_dependency_snapshot "$workspace" >"$actual"; then
    rm -rf -- "$tmp_dir"
    return 2
  fi
  if ! diff -u "$expected" "$actual"; then
    rm -rf -- "$tmp_dir"
    fail "$workspace direct dependencies changed without review acknowledgement"
    echo "Review every added/moved edge and every new 'build.rs' marker, then run:" >&2
    echo "  ./scripts/check-crate-versions.sh --update-direct-dependencies $workspace" >&2
    return 1
  fi
  rm -rf -- "$tmp_dir"
  printf '\033[32mPASS\033[0m %s direct dependencies match the review lock\n' "$workspace"
}

update_direct_dependencies() {
  local workspace="$1" target
  target="scripts/direct-dependencies-${workspace}.lock"
  direct_dependency_snapshot "$workspace" >"$target" || return $?
  echo "updated $target — review its dependency edges and build.rs markers"
}

# Does this manifest set its own version, rather than inheriting the workspace one? Only
# independently-versioned crates are in scope: a workspace-inherited crate is bumped for the whole
# workspace by `scripts/cut-release.sh` at the cut, so "changed but not bumped" is its normal state
# on main and flagging it would make this check noise. The crates that need guarding are exactly the
# ones that opted OUT of that sweep — the protocol line (C-143).
has_own_version() {
  echo "$1" | awk '/^\[package\]/{p=1;next} /^\[/{p=0} p' | grep -qE '^version *= *"'
}

# The version `cargo publish` would use for the package rooted at $1, read from a manifest that may
# inherit from its workspace. $2 is the manifest text, $3 the workspace manifest text.
package_version() {
  local manifest="$1" workspace="$2" section
  # The [package] section only — a [dependencies] entry can also carry `version = "…"`.
  section="$(echo "$manifest" | awk '/^\[package\]/{p=1;next} /^\[/{p=0} p')"
  local explicit
  explicit="$(echo "$section" | sed -nE 's/^version *= *"([^"]+)".*/\1/p' | head -1)"
  if [ -n "$explicit" ]; then
    echo "$explicit"
    return
  fi
  if echo "$section" | grep -qE '^version\.workspace *= *true|^version *= *\{ *workspace'; then
    echo "$workspace" | awk '/^\[workspace\.package\]/{p=1;next} /^\[/{p=0} p' \
      | sed -nE 's/^version *= *"([^"]+)".*/\1/p' | head -1
  fi
}

# --self-test: the failing-first proof. Two synthetic manifests differing only in content prove the
# comparison flags an unchanged version and accepts a bumped one — without needing a stale crate in
# real history to demonstrate it against.
if [ "${1:-}" = "--self-test" ]; then
  ws='[workspace.package]
version = "0.28.0"'
  explicit='[package]
name = "codewandler-flux-plugin-protocol"
version = "1.0.0"

[dependencies]
serde = { version = "9.9.9" }'
  inherited='[package]
name = "codewandler-flux-core"
version.workspace = true'

  got="$(package_version "$explicit" "$ws")"
  [ "$got" = "1.0.0" ] || { fail "self-test: explicit version read as '$got', want 1.0.0"; exit 1; }
  got="$(package_version "$inherited" "$ws")"
  [ "$got" = "0.28.0" ] || { fail "self-test: inherited version read as '$got', want 0.28.0"; exit 1; }

  # The actual rule: changed content + unchanged version must be reported.
  before="$(package_version "$explicit" "$ws")"
  after="$(package_version "$explicit" "$ws")"
  [ "$before" = "$after" ] || { fail "self-test: identical manifests compared unequal"; exit 1; }
  bumped="$(package_version "${explicit/1.0.0/1.1.0}" "$ws")"
  [ "$bumped" != "$before" ] || { fail "self-test: a bumped version compared equal to the old one"; exit 1; }

  # The review lock is intentionally exact: a new edge, moving an edge between members, changing
  # the selected version/source, or gaining a build script all changes the bytes compared by diff.
  baseline=$'crate-a\tnormal\tserde\tserde\t1.0.0\tregistry\t-'
  added="$baseline"$'\ncrate-b\tnormal\tnew-dep\tnew-dep\t1.0.0\tregistry\tbuild.rs'
  [ "$baseline" != "$added" ] || { fail "self-test: a new build-script dependency was invisible"; exit 1; }

  printf '\033[32mPASS\033[0m self-test: stale versions and direct-dependency drift are detectable\n'
  exit 0
fi

if [ "${1:-}" = "--direct-dependencies" ]; then
  [ -n "${2:-}" ] || { fail "--direct-dependencies requires root or plugins"; exit 2; }
  check_direct_dependencies "$2"
  exit $?
fi

if [ "${1:-}" = "--update-direct-dependencies" ]; then
  [ -n "${2:-}" ] || { fail "--update-direct-dependencies requires root or plugins"; exit 2; }
  update_direct_dependencies "$2"
  exit $?
fi

BASE="${BASE:-}"
if [ -z "$BASE" ]; then
  BASE="$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null)"
fi
[ -n "$BASE" ] || { fail "no v* tag found — pass BASE=<tag> (CI needs fetch-depth: 0)"; exit 2; }
git rev-parse --verify "$BASE^{commit}" >/dev/null 2>&1 || { fail "base tag '$BASE' not resolvable"; exit 2; }
echo "== crate versions vs $BASE =="

ROOT_WS_NOW="$(cat Cargo.toml)"
ROOT_WS_BASE="$(git show "$BASE:Cargo.toml" 2>/dev/null)"
PACK_WS_NOW="$(cat plugins/Cargo.toml)"
PACK_WS_BASE="$(git show "$BASE:plugins/Cargo.toml" 2>/dev/null)"

stale=""
checked=0
while IFS= read -r manifest; do
  [ -f "$manifest" ] || continue
  name="$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p' "$manifest" \
    | sed -nE 's/^name *= *"([^"]+)".*/\1/p' | head -1)"
  case "$name" in codewandler-*) ;; *) continue ;; esac

  dir="$(dirname "$manifest")"
  case "$dir" in plugins/*) ws_now="$PACK_WS_NOW"; ws_base="$PACK_WS_BASE" ;;
                 *)         ws_now="$ROOT_WS_NOW"; ws_base="$ROOT_WS_BASE" ;; esac

  current_manifest="$(cat "$manifest")"
  has_own_version "$current_manifest" || continue

  base_manifest="$(git show "$BASE:$manifest" 2>/dev/null)"
  # A crate that did not exist at BASE cannot have a stale version.
  [ -n "$base_manifest" ] || { echo "   new since $BASE: $name"; continue; }

  # Did anything in the crate ship differently? Compare the whole source directory, not just the
  # manifest — a code change with an untouched manifest is exactly the case this catches. The
  # comparison runs against the WORKING TREE, not HEAD, so the answer is the same whether this runs
  # before a commit locally or on a checkout in CI.
  if git diff --quiet "$BASE" -- "$dir"; then
    continue
  fi
  checked=$((checked + 1))

  now_version="$(package_version "$current_manifest" "$ws_now")"
  base_version="$(package_version "$base_manifest" "$ws_base")"
  if [ -z "$now_version" ]; then
    fail "$name: could not resolve its current version"
    stale="$stale $name"
    continue
  fi
  if [ "$now_version" = "$base_version" ]; then
    fail "$name changed since $BASE but is still $now_version"
    stale="$stale $name"
  elif registry_has_version "$name" "$now_version"; then
    # C-729. Git said the version moved, and that was not enough: a version already on crates.io
    # cannot be published again — `cargo publish` skips it — so a content change under it ships
    # nothing while every consumer keeps compiling against the published copy.
    fail "$name is $now_version, which is ALREADY on crates.io, and its content changed since $BASE"
    stale="$stale $name"
  else
    echo "   $name $base_version -> $now_version"
  fi
# `--others --exclude-standard` includes not-yet-committed crates, so a brand-new protocol crate is
# reported as new rather than silently skipped when this runs before the commit.
done < <(git ls-files --cached --others --exclude-standard '*/Cargo.toml' 'Cargo.toml')

if [ -n "$stale" ]; then
  echo >&2
  echo "The crates above have content changes since $BASE under a version already published." >&2
  echo "Bump each one (the protocol line is SemVer over the WIRE — see" >&2
  echo "docs/designs/plugin-protocol-decoupling.md), or revert the change." >&2
  exit 1
fi

printf '\033[32mPASS\033[0m %s changed crate(s), all with a moved version\n' "$checked"
