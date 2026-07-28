#!/usr/bin/env bash
#
# check-plugin-compat.sh — run a PREVIOUSLY RELEASED plugin binary against the host built from this
# tree, and assert they still speak the same wire protocol.
#
# This is the test that backs the whole decoupling (C-145). flux and the plugin pack no longer share
# a version line: plugins depend on `codewandler-flux-plugin-protocol` 1.x and are rebuilt only when
# the wire changes. Every other test in the repo builds host and guest from the same commit, so
# none of them can catch "today's host stopped understanding yesterday's binary" — only this one
# points a real, already-shipped artifact at the current host.
#
# What it does:
#   1. resolve the latest `plugins-v*` GitHub release (or $PACK_TAG),
#   2. download + extract the linux-x86_64 pack,
#   3. build this tree's `flux`,
#   4. `flux plugin install` the old binaries, then read a manifest and call one read-shaped op
#      through the host.
#
# Exit codes: 0 pass, 1 incompatibility (a real failure), 2 could not obtain the pack (reported as
# a skip — the release genuinely isn't there). An incompatibility NEVER exits 2.
#
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

REPO="${REPO:-codewandler/flux}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

note() { printf '  %s\n' "$1"; }
fail() { printf '\033[31mFAIL\033[0m %s\n' "$1" >&2; exit 1; }
skip() { printf '\033[33mSKIP\033[0m %s\n' "$1" >&2; exit 2; }

command -v gh >/dev/null 2>&1 || skip "gh CLI not available — cannot resolve the plugin pack release"

TAG="${PACK_TAG:-}"
if [ -z "$TAG" ]; then
  TAG="$(gh release list --repo "$REPO" --limit 50 --json tagName \
    --jq '[.[] | select(.tagName | startswith("plugins-v"))] | .[0].tagName' 2>/dev/null)"
fi
[ -n "$TAG" ] && [ "$TAG" != "null" ] || skip "no plugins-v* release found in $REPO"
note "pack release: $TAG"

# The pack ships ONE archive PER PLUGIN per platform. Two are enough to prove the wire: pulling all
# 21 would only repeat the same protocol exchange. This job runs on linux-x86_64.
PLUGINS_UNDER_TEST="${PLUGINS_UNDER_TEST:-websearch gitlab}"
mkdir -p "$WORK/pack"
got=0
for plugin in $PLUGINS_UNDER_TEST; do
  pattern="flux-plugin-$plugin-*-x86_64-unknown-linux-gnu.tar.xz"
  if ! gh release download "$TAG" --repo "$REPO" --pattern "$pattern" --dir "$WORK" 2>/dev/null; then
    note "no archive for $plugin on $TAG — skipping that one"
    continue
  fi
  for archive in "$WORK"/flux-plugin-"$plugin"-*.tar.xz; do
    [ -f "$archive" ] || continue
    tar -xf "$archive" -C "$WORK/pack" || fail "could not extract $(basename "$archive")"
    got=$((got + 1))
  done
done
[ "$got" -gt 0 ] || skip "no linux-x86_64 plugin archives downloadable from $TAG"

mapfile -t BINARIES < <(find "$WORK/pack" -type f -name 'flux-plugin-*' -perm -u+x | sort)
[ "${#BINARIES[@]}" -gt 0 ] || skip "no flux-plugin-* binaries inside the $TAG archives"
note "binaries: ${#BINARIES[@]}"

# `flux plugin install` wants a directory of binaries; flatten whatever layout the archives used.
mkdir -p "$WORK/bin"
for binary in "${BINARIES[@]}"; do
  cp "$binary" "$WORK/bin/"
done

echo "== building this tree's flux =="
cargo build -p flux-cli --bin flux >/dev/null 2>&1 || fail "could not build flux from this tree"
FLUX="$PWD/target/debug/flux"

# Install into a throwaway home so the developer's real ~/.flux is untouched.
#
# ⚠ It must be HOME, not FLUX_HOME. The plugin descriptor directory is resolved as
# `$HOME/.flux/plugins` (`flux_cli::execution::plugins_dir`) and does NOT consult FLUX_HOME — an
# earlier version of this script exported FLUX_HOME, which was silently ignored, so running it
# locally REWROTE the developer's real `~/.flux/plugins/{gitlab,websearch}.toml` to point at this
# script's `mktemp -d` directory. The moment the script exited and the trap deleted that directory,
# both plugins were broken and had to be reinstalled from the pack. CI never noticed, because a
# throwaway runner has nothing to clobber.
#
# HOME is overridden per-command rather than exported, because `gh` and `cargo` above need the real
# one (auth, registry cache, toolchain).
SANDBOX_HOME="$WORK/home"
mkdir -p "$SANDBOX_HOME/.flux"
flux_sandboxed() { HOME="$SANDBOX_HOME" "$FLUX" "$@"; }
PACK_DIR="$WORK/bin"

echo "== old plugin binaries vs this host =="
# `--dir=` is the local-scan mode: register already-built binaries with no pack-index
# signature check. That is what we want — the artifacts are the released ones, and the
# question under test is the wire, not the distribution channel.
install_out="$(flux_sandboxed plugin install "--dir=$PACK_DIR" 2>&1)"
install_status=$?
if [ "$install_status" -ne 0 ]; then
  echo "$install_out" >&2
  fail "this host refused to install plugin binaries from $TAG"
fi
note "installed"

# `plugin list` reads each plugin's MANIFEST over the wire: spawn, frame exchange, protocol-marker
# check, manifest deserialization. An incompatible wire fails right here, which is the point.
list_out="$(flux_sandboxed plugin list 2>&1)"
list_status=$?
if [ "$list_status" -ne 0 ]; then
  echo "$list_out" >&2
  fail "this host could not read manifests from $TAG plugin binaries"
fi

if echo "$list_out" | grep -qiE "protocol .* this host speaks|speaks protocol"; then
  echo "$list_out" >&2
  fail "protocol mismatch between $TAG plugins and this host"
fi
note "manifests read over the wire"

# Prove an operation round-trips, not just discovery. `sources` is read-shaped, needs no third-party
# credential, and every host-kit plugin answers it.
ops_out="$(flux_sandboxed plugin call websearch sources '{}' 2>&1)"
if echo "$ops_out" | grep -qiE "speaks protocol|unsupported protocol|invalid frame"; then
  echo "$ops_out" >&2
  fail "an operation call hit a protocol error against $TAG binaries"
fi
note "operation round-trip clean"

printf '\033[32mPASS\033[0m plugin pack %s speaks this host'"'"'s protocol\n' "$TAG"
