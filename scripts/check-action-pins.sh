#!/usr/bin/env bash
#
# check-action-pins.sh — every third-party GitHub Action must be pinned to an immutable commit SHA.
#
# Why this exists (C-187): a `uses:` that names a movable tag (`actions/checkout@v4`,
# `dtolnay/rust-toolchain@stable`) hands whoever controls that upstream tag the ability to change
# the code that runs in our workflows. Two of those workflows hold the crown jewels — `release.yml`
# has crates.io publish rights and `release-plugins.yml` signs the plugin index with
# `MINISIGN_SECRET_KEY`, whose public half is embedded in the flux binary (D-47). The per-artifact
# SHA-256 chain is only as trustworthy as that key, so an unpinned action in the same workflow is a
# direct path to the plugin trust model's root of trust.
#
# The rule: every `uses:` that names a third-party action must reference a full 40-char commit SHA
# and keep the human-readable version as a trailing comment (`uses: actions/checkout@<sha> # v6.1.0`).
# Local (`./`-path) actions live in this repo and are covered by the tree itself, so they are exempt.
#
#   scripts/check-action-pins.sh              # scan .github/workflows/*.yml
#   scripts/check-action-pins.sh --self-test  # prove the check rejects a tag and accepts a SHA pin
#
# Exit 0 clean, 1 an unpinned or comment-less `uses:` (a real failure).
#
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; }

# Verdict for a single `uses:` line. Exit 0 = acceptably pinned (or exempt), 1 = violation with a
# reason printed to stdout. Kept as a function so the self-test can exercise it directly, without a
# throwaway workflow file to scan.
pin_ok() {
  local line="$1" ref sha
  # The action reference is the token right after `uses:`.
  ref="$(printf '%s\n' "$line" | sed -nE 's/^[[:space:]-]*uses:[[:space:]]*([^[:space:]]+).*/\1/p')"
  # Not a `uses:` line we can parse — let the caller's grep decide what to feed us; treat as clean.
  [ -n "$ref" ] || return 0
  # Local path actions ship in this repo; there is no upstream tag to pin.
  case "$ref" in
    ./*|../*) return 0 ;;
  esac
  # Third-party action: the part after the last `@` must be a full commit SHA.
  sha="${ref##*@}"
  if ! printf '%s' "$sha" | grep -qE '^[0-9a-f]{40}$'; then
    echo "unpinned action (ref is not a 40-char commit SHA): $ref"
    return 1
  fi
  # The version the SHA stands for must survive as a trailing comment, or the pin is unreadable and
  # nobody will ever knowingly bump it.
  if ! printf '%s\n' "$line" | grep -qE '@[0-9a-f]{40}[[:space:]]+#[[:space:]]*\S'; then
    echo "pinned SHA is missing its '# <version>' trailing comment: $ref"
    return 1
  fi
  return 0
}

# --self-test: the failing-first proof. Four synthetic lines show the check rejects a movable tag
# and a comment-less SHA, accepts a SHA carrying its version comment, and exempts a local action —
# without needing an unpinned workflow in the real tree to demonstrate against.
if [ "${1:-}" = "--self-test" ]; then
  green='      - uses: actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803 # v6.1.0'
  tag='      - uses: actions/checkout@v4'
  no_comment='      - uses: actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803'
  local_action='      - uses: ./.github/actions/setup'

  pin_ok "$green" || { fail "self-test: a SHA pin with a version comment was rejected"; exit 1; }
  pin_ok "$local_action" || { fail "self-test: a local ./ action was flagged"; exit 1; }
  if pin_ok "$tag" >/dev/null; then fail "self-test: a movable @tag was accepted"; exit 1; fi
  if pin_ok "$no_comment" >/dev/null; then fail "self-test: a SHA pin without its version comment was accepted"; exit 1; fi

  printf '\033[32mPASS\033[0m self-test: tags rejected, SHA+comment pins accepted, local actions exempt\n'
  exit 0
fi

echo "== third-party action pins in .github/workflows =="
violations=0
checked=0
for wf in .github/workflows/*.yml; do
  [ -f "$wf" ] || continue
  # A `uses:` key at the start of an (indented) list item or step — never a `#` comment line.
  while IFS= read -r hit; do
    lineno="${hit%%:*}"
    line="${hit#*:}"
    checked=$((checked + 1))
    if reason="$(pin_ok "$line")"; then
      :
    else
      fail "$wf:$lineno: $reason"
      violations=$((violations + 1))
    fi
  done < <(grep -nE '^[[:space:]-]*uses:[[:space:]]' "$wf")
done

if [ "$violations" -gt 0 ]; then
  echo >&2
  echo "$violations unpinned or comment-less action reference(s) above." >&2
  echo "Pin each to a full commit SHA with the version as a trailing comment:" >&2
  echo "  uses: actions/checkout@<40-char-sha> # v6.1.0" >&2
  echo "Resolve a tag with: gh api repos/<owner>/<repo>/git/ref/tags/<tag> --jq .object.sha" >&2
  exit 1
fi

printf '\033[32mPASS\033[0m %s third-party action reference(s), all pinned to a commit SHA\n' "$checked"
