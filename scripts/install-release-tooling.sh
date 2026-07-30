#!/usr/bin/env bash
# Install the exact cargo-dist release used by release.yml after verifying its repository-pinned
# archive digest. Unlike cargo-dist-installer.sh, no downloaded script is ever executed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="0.32.0"
DEST="${1:-${HOME}/.cargo/bin}"

runner_os="${RUNNER_OS:-}"
runner_arch="${RUNNER_ARCH:-}"
if [ -z "$runner_os" ]; then
  case "$(uname -s)" in
    Darwin) runner_os="macOS" ;;
    Linux) runner_os="Linux" ;;
    MINGW*|MSYS*|CYGWIN*) runner_os="Windows" ;;
    *) echo "unsupported release-tooling OS: $(uname -s)" >&2; exit 1 ;;
  esac
fi
if [ -z "$runner_arch" ]; then
  case "$(uname -m)" in
    x86_64|amd64) runner_arch="X64" ;;
    aarch64|arm64) runner_arch="ARM64" ;;
    *) echo "unsupported release-tooling architecture: $(uname -m)" >&2; exit 1 ;;
  esac
fi

case "$runner_os/$runner_arch" in
  Linux/X64) target="x86_64-unknown-linux-gnu"; ext="tar.xz"; binary="dist" ;;
  Linux/ARM64) target="aarch64-unknown-linux-gnu"; ext="tar.xz"; binary="dist" ;;
  macOS/X64) target="x86_64-apple-darwin"; ext="tar.xz"; binary="dist" ;;
  macOS/ARM64) target="aarch64-apple-darwin"; ext="tar.xz"; binary="dist" ;;
  Windows/X64) target="x86_64-pc-windows-msvc"; ext="zip"; binary="dist.exe" ;;
  *) echo "unsupported release-tooling runner: $runner_os/$runner_arch" >&2; exit 1 ;;
esac

archive="cargo-dist-${target}.${ext}"
expected="$(awk -v name="$archive" '$2 == name { print $1 }' "$ROOT/scripts/release-tooling.sha256" | head -1)"
if ! [[ "$expected" =~ ^[0-9a-f]{64}$ ]]; then
  echo "no pinned SHA-256 for $archive" >&2
  exit 1
fi

tmp="$(mktemp -d)"
cleanup() { rm -rf -- "$tmp"; }
trap cleanup EXIT
url="https://github.com/axodotdev/cargo-dist/releases/download/v${VERSION}/${archive}"
curl --proto '=https' --tlsv1.2 -LsSf -o "$tmp/$archive" "$url"

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
else
  echo "neither sha256sum nor shasum is available" >&2
  exit 1
fi
if [ "$actual" != "$expected" ]; then
  echo "SHA-256 mismatch for $archive: expected $expected, got $actual" >&2
  exit 1
fi

mkdir -p "$tmp/unpack" "$DEST"
if [ "$ext" = "zip" ]; then
  command -v unzip >/dev/null 2>&1 || {
    echo "unzip is required to install verified Windows release tooling" >&2
    exit 1
  }
  unzip -q "$tmp/$archive" -d "$tmp/unpack"
else
  tar -xf "$tmp/$archive" -C "$tmp/unpack"
fi
if [ "$runner_os" = "Windows" ]; then
  # The upstream Windows zip is flat (`dist.exe` at archive root); Unix archives carry a
  # `cargo-dist-<target>/` directory. Keep both layouts explicit so a verified archive cannot be
  # mistaken for the wrong platform shape.
  source_binary="$tmp/unpack/$binary"
else
  source_binary="$tmp/unpack/cargo-dist-${target}/${binary}"
fi
if [ ! -f "$source_binary" ]; then
  echo "verified archive did not contain expected $binary" >&2
  exit 1
fi
cp "$source_binary" "$DEST/$binary"
chmod +x "$DEST/$binary"
if [ -n "${GITHUB_PATH:-}" ]; then
  printf '%s\n' "$DEST" >> "$GITHUB_PATH"
fi
"$DEST/$binary" --version
