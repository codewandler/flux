#!/usr/bin/env bash
# Build the release-versioned `flux system serve` OCI image.
#
# Two binary sources, and the difference is the whole provenance story:
#
#   --release [VERSION]   Repack the published flux-cli-x86_64-unknown-linux-gnu archive for that
#                         release. The bytes in the image are the bytes GitHub attested, so
#                         `gh attestation verify` on the archive covers the binary in the layer.
#                         This is the supported way to build a publishable image.
#   --binary PATH         Use a Linux binary you already have. For development and for the
#                         container integration test; it carries no release provenance.
#
# This script never compiles Rust — it repacks — so it does not take a build-ownership lease.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TARGET=x86_64-unknown-linux-gnu
BASE_IMAGE=debian:trixie-slim
SOURCE=
BINARY=
VERSION=
TAG=
REPO=codewandler/flux

usage() {
  cat >&2 <<'USAGE'
usage: deploy/container/build-image.sh (--release [VERSION] | --binary PATH) [options]

  --release [VERSION]   Repack the published release archive (default: the workspace version).
  --binary PATH         Use an existing Linux `flux` binary.
  --version VERSION     Version stamped into the image label and the default tag.
  --tag REF             Image reference to build (default: flux-system:<version>).
  --base-image REF      Base image (default: debian:trixie-slim).
  --target TRIPLE       Release target to repack (default: x86_64-unknown-linux-gnu).
USAGE
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --release)
      SOURCE=release
      case "${2:-}" in --*|"") ;; *) VERSION=$2; shift ;; esac
      ;;
    --binary) SOURCE=binary; BINARY=${2:?--binary needs a path}; shift ;;
    --version) VERSION=${2:?--version needs a value}; shift ;;
    --tag) TAG=${2:?--tag needs a value}; shift ;;
    --base-image) BASE_IMAGE=${2:?--base-image needs a value}; shift ;;
    --target) TARGET=${2:?--target needs a value}; shift ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
  shift
done

[ -n "$SOURCE" ] || usage

# The single source of truth every other release entry point reads the same way.
if [ -z "$VERSION" ]; then
  VERSION=$(grep -m1 '^version = ' "$ROOT/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')
fi
[ -n "$VERSION" ] || { echo "could not read [workspace.package].version" >&2; exit 1; }
: "${TAG:=flux-system:$VERSION}"

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

case "$SOURCE" in
  binary)
    [ -f "$BINARY" ] || { echo "no such binary: $BINARY" >&2; exit 1; }
    cp "$BINARY" "$STAGE/flux"
    ;;
  release)
    command -v gh >/dev/null || { echo "--release needs the gh CLI" >&2; exit 1; }
    ARCHIVE="flux-cli-$TARGET.tar.xz"
    # The pinned download path, not a raw URL: gh resolves the tag and the asset together.
    gh release download "v$VERSION" --repo "$REPO" --dir "$STAGE" \
      --pattern "$ARCHIVE" --pattern "$ARCHIVE.sha256"
    ( cd "$STAGE" && sha256sum --check --status "$ARCHIVE.sha256" ) \
      || { echo "$ARCHIVE failed its published checksum" >&2; exit 1; }
    tar -xJf "$STAGE/$ARCHIVE" -C "$STAGE"
    cp "$STAGE/flux-cli-$TARGET/flux" "$STAGE/flux"
    ;;
esac

chmod 0755 "$STAGE/flux"
cp "$ROOT/deploy/container/Dockerfile" "$STAGE/Dockerfile"

docker build \
  --build-arg "BASE_IMAGE=$BASE_IMAGE" \
  --build-arg "FLUX_VERSION=$VERSION" \
  --tag "$TAG" \
  "$STAGE" >&2

# The identity to record alongside the release. An image digest exists only once the image is
# pushed; until then the local image id is what pins these exact bytes.
IMAGE_ID=$(docker image inspect --format '{{.Id}}' "$TAG")
echo "$TAG $IMAGE_ID"
