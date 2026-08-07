#!/usr/bin/env bash
# Install the Flux remote execution system into a VM or microVM guest.
#
# This is the install contract the cloud-init profile calls, and it is the whole of it: fetch the
# pinned release binary and verify it, create the service identity, create the directories and file
# modes the daemon's secrets need, and install the hardened unit. It configures a guest that already
# exists. It does not create, start, snapshot, resume or destroy one — Firecracker, Cloud Hypervisor,
# Kata and every cloud lifecycle verb are outside the shipped runtime.
#
# Idempotent: safe to re-run, which is what makes it an upgrade path as well as an install.
#
#   install-flux-system.sh --version 0.58.0
#   install-flux-system.sh --version 0.58.0 --binary /tmp/flux    # air-gapped guest
set -euo pipefail

VERSION=
BINARY=
TARGET=x86_64-unknown-linux-gnu
REPO=codewandler/flux
WORKSPACE=/srv/flux/workspace
UNIT_SOURCE=

usage() {
  cat >&2 <<'USAGE'
usage: install-flux-system.sh --version VERSION [options]

  --version VERSION   Flux release to install (required unless --binary is given).
  --binary PATH       Install this binary instead of downloading a release.
  --target TRIPLE     Release target (default: x86_64-unknown-linux-gnu).
  --workspace DIR     Canonical workspace (default: /srv/flux/workspace).
  --unit PATH         flux-system.service to install (default: beside this script).
USAGE
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION=${2:?--version needs a value}; shift ;;
    --binary) BINARY=${2:?--binary needs a path}; shift ;;
    --target) TARGET=${2:?--target needs a value}; shift ;;
    --workspace) WORKSPACE=${2:?--workspace needs a path}; shift ;;
    --unit) UNIT_SOURCE=${2:?--unit needs a path}; shift ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; usage ;;
  esac
  shift
done

[ "$(id -u)" -eq 0 ] || { echo "install-flux-system.sh must run as root" >&2; exit 1; }
[ -n "$VERSION" ] || [ -n "$BINARY" ] || usage
: "${UNIT_SOURCE:=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/flux-system.service}"
[ -f "$UNIT_SOURCE" ] || { echo "no unit file at $UNIT_SOURCE" >&2; exit 1; }

# The service identity. Non-root, no login, no home of its own to leak into.
getent group flux >/dev/null || groupadd --system flux
getent passwd flux >/dev/null || \
  useradd --system --gid flux --home-dir /srv/flux --no-create-home \
          --shell /usr/sbin/nologin flux

# ── Binary ───────────────────────────────────────────────────────────────────────────────────────
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

if [ -n "$BINARY" ]; then
  install -m 0755 -o root -g root "$BINARY" "$STAGE/flux"
else
  ARCHIVE="flux-cli-$TARGET.tar.xz"
  BASE="https://github.com/$REPO/releases/download/v$VERSION"
  # The checksum sidecar is published alongside the archive and is what makes this a pinned fetch
  # rather than a trusted one. On a guest with the gh CLI, `gh attestation verify` on the archive
  # additionally proves which workflow built it.
  curl --proto '=https' --tlsv1.2 -fLsS -o "$STAGE/$ARCHIVE" "$BASE/$ARCHIVE"
  curl --proto '=https' --tlsv1.2 -fLsS -o "$STAGE/$ARCHIVE.sha256" "$BASE/$ARCHIVE.sha256"
  ( cd "$STAGE" && sha256sum --check --status "$ARCHIVE.sha256" ) \
    || { echo "$ARCHIVE failed its published checksum" >&2; exit 1; }
  tar -xJf "$STAGE/$ARCHIVE" -C "$STAGE"
  install -m 0755 -o root -g root "$STAGE/flux-cli-$TARGET/flux" "$STAGE/flux"
fi

# Replace in place so a re-run is an upgrade. The old binary stays as .previous, which is the
# rollback: swap it back and restart.
if [ -x /usr/local/bin/flux ]; then
  cp -a /usr/local/bin/flux /usr/local/bin/flux.previous
fi
install -m 0755 -o root -g root "$STAGE/flux" /usr/local/bin/flux

# ── Directories and file modes ───────────────────────────────────────────────────────────────────
# The workspace is expected to be a mount point for a durable disk. Creating it here means a guest
# whose disk failed to attach serves an empty workspace rather than failing to start, so the unit
# additionally declares RequiresMountsFor.
install -d -o flux -g flux -m 0750 /srv/flux "$WORKSPACE"

# Secrets. The TLS private key is readable only by the service identity; the token environment file
# is read by systemd as root before privileges drop, so the service never needs to open it at all.
install -d -o root -g root -m 0755 /etc/flux
install -d -o root -g flux -m 0750 /etc/flux/tls
[ ! -f /etc/flux/tls/tls.key ] || chown root:flux /etc/flux/tls/tls.key
[ ! -f /etc/flux/tls/tls.key ] || chmod 0640 /etc/flux/tls/tls.key
[ ! -f /etc/flux/tls/tls.crt ] || chmod 0644 /etc/flux/tls/tls.crt
if [ ! -f /etc/flux/remote-system.env ]; then
  # A placeholder, not a token: the daemon refuses an empty one, which is the intended failure for
  # a guest whose operator has not delivered a real secret yet.
  printf 'FLUX_REMOTE_SYSTEM_TOKEN=\n' > /etc/flux/remote-system.env
fi
chown root:root /etc/flux/remote-system.env
chmod 0600 /etc/flux/remote-system.env

# ── Sandbox floor ────────────────────────────────────────────────────────────────────────────────
# The guest keeps the fail-closed sandbox floor, so bubblewrap has to be present. This is the one
# genuine difference from the container and pod profiles, and it is why it is worth running a guest.
if ! command -v bwrap >/dev/null; then
  echo "warning: bubblewrap (bwrap) is not installed. The daemon will refuse to start until it is," \
       "or until --no-sandbox is added to the unit as a deliberate decision." >&2
fi

# ── Unit ─────────────────────────────────────────────────────────────────────────────────────────
install -m 0644 -o root -g root "$UNIT_SOURCE" /etc/systemd/system/flux-system.service
systemctl daemon-reload
systemctl enable flux-system.service

cat <<EOF
flux system serve installed ($(/usr/local/bin/flux --version 2>/dev/null || echo "$VERSION"))

Before starting:
  1. put a long random value in /etc/flux/remote-system.env (FLUX_REMOTE_SYSTEM_TOKEN=…)
  2. place a certificate whose SAN matches the client URL at /etc/flux/tls/tls.{crt,key}
  3. admit TCP 8790 only from intended clients, at the guest firewall and in front of it
  4. systemctl start flux-system
EOF
