#!/usr/bin/env bash
set -euo pipefail

readonly SOURCE_REPOSITORY="https://github.com/asterisk/asterisk"
readonly SOURCE_TAG="22.10.1"
readonly SOURCE_TAG_OBJECT="4f85d05889cf9fb9c9e2ae44cc3f4a825a74545a"
readonly SOURCE_COMMIT="f0e408a7b0d829c85bf15fa4b487870a50cb3000"
readonly RAW_BASE="https://raw.githubusercontent.com/asterisk/asterisk/${SOURCE_COMMIT}"
readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly PLUGIN_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
readonly TARGET_DIR="${PLUGIN_DIR}/specs/ari-${SOURCE_TAG}"
readonly PINNED_HASHES="${PLUGIN_DIR}/specs/ari-${SOURCE_TAG}.sha256"

readonly -a API_DOCUMENTS=(
  applications.json
  asterisk.json
  bridges.json
  channels.json
  deviceStates.json
  endpoints.json
  events.json
  mailboxes.json
  playbacks.json
  recordings.json
  sounds.json
)

source_dir=""

usage() {
  printf '%s\n' \
    "usage: $0 [--source-dir ASTERISK_CHECKOUT]" \
    "" \
    "Without --source-dir, fetches only commit ${SOURCE_COMMIT}." \
    "With --source-dir, replays from ASTERISK_CHECKOUT/rest-api and COPYING."
}

while (($# > 0)); do
  case "$1" in
    --source-dir)
      if (($# < 2)); then
        printf 'error: --source-dir requires a path\n' >&2
        exit 2
      fi
      source_dir="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      printf 'error: unknown argument %q\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -n "$source_dir" ]]; then
  if [[ ! -d "$source_dir" ]]; then
    printf 'error: source directory does not exist: %s\n' "$source_dir" >&2
    exit 2
  fi
  source_dir="$(cd -- "$source_dir" && pwd)"
fi

stage_dir="$(mktemp -d "${TMPDIR:-/tmp}/flux-asterisk-ari.XXXXXX")"
trap 'rm -rf -- "$stage_dir"' EXIT

copy_or_fetch() {
  local relative="$1"
  local destination="$stage_dir/$relative"
  mkdir -p -- "$(dirname -- "$destination")"
  if [[ -n "$source_dir" ]]; then
    local source="$source_dir/$relative"
    if [[ ! -f "$source" ]]; then
      printf 'error: pinned source file is missing: %s\n' "$source" >&2
      exit 1
    fi
    cp -- "$source" "$destination"
  else
    curl --fail --silent --show-error --location \
      "${RAW_BASE}/${relative}" \
      --output "$destination"
  fi
}

if [[ -n "$source_dir" ]]; then
  python3 - "$source_dir/rest-api/api-docs" "${API_DOCUMENTS[@]}" <<'PY'
import pathlib
import sys

directory = pathlib.Path(sys.argv[1])
expected = set(sys.argv[2:])
if not directory.is_dir():
    raise SystemExit(f"error: source API document directory is missing: {directory}")
actual = {path.name for path in directory.iterdir() if path.suffix == ".json"}
if actual != expected:
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    raise SystemExit(
        f"error: source API document inventory differs; missing={missing}, extra={extra}"
    )
PY
fi

copy_or_fetch COPYING
copy_or_fetch rest-api/resources.json
for document in "${API_DOCUMENTS[@]}"; do
  copy_or_fetch "rest-api/api-docs/${document}"
done

mv -- "$stage_dir/rest-api/resources.json" "$stage_dir/resources.json"
mv -- "$stage_dir/rest-api/api-docs" "$stage_dir/api-docs"
rmdir -- "$stage_dir/rest-api"

python3 - "$stage_dir" "$PINNED_HASHES" "${API_DOCUMENTS[@]}" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
hashes_path = pathlib.Path(sys.argv[2])
expected = list(sys.argv[3:])
expected_set = set(expected)

pinned = {}
for raw_line in hashes_path.read_text(encoding="utf-8").splitlines():
    line = raw_line.strip()
    if not line or line.startswith("#"):
        continue
    digest, relative = line.split(None, 1)
    relative = relative.strip()
    if relative.startswith("/") or ".." in pathlib.PurePosixPath(relative).parts:
        raise SystemExit(f"error: unsafe pinned path: {relative!r}")
    if relative in pinned:
        raise SystemExit(f"error: duplicate pinned path: {relative!r}")
    if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
        raise SystemExit(f"error: invalid pinned SHA-256 for {relative!r}")
    pinned[relative] = digest

expected_pinned = {"COPYING", "resources.json"}
expected_pinned.update(f"api-docs/{name}" for name in expected)
if set(pinned) != expected_pinned:
    raise SystemExit(
        "error: pinned hash inventory differs; "
        f"missing={sorted(expected_pinned - set(pinned))}, "
        f"extra={sorted(set(pinned) - expected_pinned)}"
    )
for relative, expected_digest in pinned.items():
    actual_digest = hashlib.sha256((root / relative).read_bytes()).hexdigest()
    if actual_digest != expected_digest:
        raise SystemExit(
            f"error: pinned SHA-256 mismatch for {relative}: "
            f"expected {expected_digest}, got {actual_digest}"
        )

resources = json.loads((root / "resources.json").read_text(encoding="utf-8"))
declared = set()
for api in resources.get("apis", []):
    path = api.get("path", "")
    prefix = "/api-docs/"
    suffix = ".{format}"
    if not path.startswith(prefix) or not path.endswith(suffix):
        raise SystemExit(f"error: unexpected resources.json API path: {path!r}")
    declared.add(path[len(prefix):-len(suffix)] + ".json")

actual = {path.name for path in (root / "api-docs").glob("*.json")}
if declared != expected_set or actual != expected_set:
    raise SystemExit(
        "error: pinned ARI inventory differs; "
        f"declared={sorted(declared)}, actual={sorted(actual)}, expected={sorted(expected_set)}"
    )

files = [pathlib.Path("COPYING"), pathlib.Path("resources.json")]
files.extend(pathlib.Path("api-docs") / name for name in sorted(expected))

lines = [
    "# Generated by plugins/asterisk/scripts/vendor-ari-specs.sh; do not edit.",
    'source_repository = "https://github.com/asterisk/asterisk"',
    'source_tag = "22.10.1"',
    'source_tag_object = "4f85d05889cf9fb9c9e2ae44cc3f4a825a74545a"',
    'source_commit = "f0e408a7b0d829c85bf15fa4b487870a50cb3000"',
    'upstream_license = "GPL-2.0-only"',
]
for relative in files:
    data = (root / relative).read_bytes()
    lines.extend(
        [
            "",
            "[[files]]",
            f'path = "{relative.as_posix()}"',
            f'sha256 = "{hashlib.sha256(data).hexdigest()}"',
            f"bytes = {len(data)}",
        ]
    )
(root / "provenance.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
PY

if [[ -d "$TARGET_DIR/api-docs" ]]; then
  python3 - "$TARGET_DIR/api-docs" "${API_DOCUMENTS[@]}" <<'PY'
import pathlib
import sys

directory = pathlib.Path(sys.argv[1])
expected = set(sys.argv[2:])
actual = {path.name for path in directory.iterdir() if path.suffix == ".json"}
if actual != expected:
    raise SystemExit(
        "error: target contains an unexpected API document; refusing to overwrite: "
        f"missing={sorted(expected - actual)}, extra={sorted(actual - expected)}"
    )
PY
fi

mkdir -p -- "$TARGET_DIR/api-docs"
install -m 0644 -- "$stage_dir/COPYING" "$TARGET_DIR/COPYING"
install -m 0644 -- "$stage_dir/resources.json" "$TARGET_DIR/resources.json"
install -m 0644 -- "$stage_dir/provenance.toml" "$TARGET_DIR/provenance.toml"
for document in "${API_DOCUMENTS[@]}"; do
  install -m 0644 -- "$stage_dir/api-docs/$document" "$TARGET_DIR/api-docs/$document"
done

printf 'vendored Asterisk ARI %s (%s): %s resources, %s API documents\n' \
  "$SOURCE_TAG" "$SOURCE_COMMIT" "$TARGET_DIR/resources.json" "${#API_DOCUMENTS[@]}"
