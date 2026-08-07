#!/usr/bin/env bash
#
# Restamp the shipped Kustomize profiles' image tag onto the version being cut, then prove no
# shipped manifest still names the version the release just left behind (C-696).
#
#   scripts/stamp-deployment-images.sh <old-version> <new-version> [root]
#
# Extracted from scripts/cut-release.sh rather than inlined there for one reason: the postcondition
# is the interesting half, and a postcondition that can only be exercised by running a real release
# cut is a postcondition nobody tests. `root` exists so scripts/test-release-candidate.sh can drive
# this over a fixture tree.
#
# Both `deploy/kubernetes/kustomization.yaml` and `deploy/agent/kustomization.yaml` pin the image
# tag in their `images:` block — one edit per profile moves the whole thing, and
# `crates/flux-cli/tests/deployment_artifacts.rs` checks that pin against
# `[workspace.package].version`. Before this script existed the cut bumped the manifest, the
# lockfile and both changelogs but never these two files, so every release shipped deployment
# profiles advertising the PREVIOUS version's image: the binary and the manifests disagreed the
# moment the tag was pushed.
#
# This script edits files. It does not stage, commit or push anything — the cut owns that, and owns
# restoring these files if its gate goes red.
set -euo pipefail

usage() {
  echo "usage: scripts/stamp-deployment-images.sh <old-version> <new-version> [root]" >&2
  exit 2
}

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage
OLD=$1
NEW=$2
ROOT=${3:-}
[ -n "$ROOT" ] || ROOT=$(git rev-parse --show-toplevel)
[ -d "$ROOT" ] || { echo "no such root: $ROOT" >&2; exit 2; }

for version in "$OLD" "$NEW"; do
  echo "$version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' \
    || { echo "not a plain X.Y.Z version: $version" >&2; exit 2; }
done

# The profiles that pin the released image. A new one belongs here and in `deployment_artifacts.rs`.
PROFILES=(deploy/kubernetes/kustomization.yaml deploy/agent/kustomization.yaml)

# `.` is a regex metacharacter, and `0.58.0` matching `0158X0` is exactly the kind of near-miss that
# makes a restamp look like it worked.
OLD_PATTERN=${OLD//./\\.}

stamped=0
for profile in "${PROFILES[@]}"; do
  path="$ROOT/$profile"
  [ -f "$path" ] || { echo "no such deployment profile: $profile" >&2; exit 1; }
  grep -qE "^[[:space:]]*newTag: $OLD_PATTERN\$" "$path" || {
    echo "!! $profile does not pin \`newTag: $OLD\`, so this cut cannot restamp it." >&2
    echo "!! Fix the profile — a cut that silently skips it ships the previous release's image." >&2
    exit 1
  }
  sed -i -E "s/^([[:space:]]*)newTag: $OLD_PATTERN\$/\1newTag: $NEW/" "$path"
  stamped=$((stamped + 1))
done

# The postcondition, over every shipped manifest rather than only the two files edited above: a
# Deployment that hard-coded `image: flux-system:<old>` beside the Kustomize pin would leave the
# release half-stamped, and nothing else in the cut would notice.
stale=()
while IFS= read -r manifest; do
  grep -qF -- "$OLD" "$manifest" && stale+=("${manifest#"$ROOT/"}")
done < <(find "$ROOT/deploy" -type f \( -name '*.yaml' -o -name '*.yml' \) | sort)

if [ "${#stale[@]}" -gt 0 ]; then
  echo "!! restamped to $NEW, but these shipped manifests still reference $OLD:" >&2
  printf '   %s\n' "${stale[@]}" >&2
  echo "!! A release whose manifests name the previous version is the defect this check exists for." >&2
  exit 1
fi

echo "   restamped $stamped deployment profile(s) newTag: $OLD -> $NEW"
