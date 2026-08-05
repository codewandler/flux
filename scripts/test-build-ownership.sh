#!/usr/bin/env bash
# Deterministic compiler-output disappearance regression and optional real bundled-SQLite proof.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WRAPPER="$ROOT/scripts/build_ownership.py"
PYTHON_RUNNER="$ROOT/scripts/run-python3.sh"
ACTOR="$ROOT/scripts/fixtures/build-ownership/compiler-at-barrier.sh"
SQLITE_CC="$ROOT/scripts/fixtures/build-ownership/sqlite-cc-barrier.sh"
SOURCE="$ROOT/scripts/fixtures/build-ownership/sqlite-object.c"
TMP=$(mktemp -d)
PIDS=()

cleanup() {
  local pid
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  rm -rf -- "$TMP"
}
trap cleanup EXIT INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

run_disappearance_class() {
  local case_root=$TMP/disappearance
  local baseline_target=$case_root/baseline-target
  local selected=$case_root/baseline-selected
  local release=$case_root/baseline-release
  local output=$baseline_target/release/build/libsqlite3-sys-baseline/out/sqlite3.o
  mkdir -p "$case_root"
  mkfifo "$selected" "$release"

  "$ACTOR" "$output" "$selected" "$release" "$SOURCE" >"$case_root/baseline.out" 2>"$case_root/baseline.err" &
  local build_pid=$!
  PIDS+=("$build_pid")
  local selected_output
  IFS= read -r selected_output <"$selected"
  [ "$selected_output" = "$output" ] || fail "baseline selected an unexpected compiler output"

  # This is the pre-repair repository cleanup class: it can unlink an output selected by live cc.
  rm -rf -- "$baseline_target"
  printf '%s\n' continue >"$release"
  if wait "$build_pid"; then
    fail "pre-repair compiler unexpectedly survived disappearance of its selected output directory"
  fi
  grep -Eq 'No such file|no such file|cannot open output file' "$case_root/baseline.err" \
    || fail "pre-repair compiler failure did not name the disappeared output"

  local fixed_target=$case_root/fixed-target
  selected=$case_root/fixed-selected
  release=$case_root/fixed-release
  output=$fixed_target/release/build/libsqlite3-sys-fixed/out/sqlite3.o
  mkfifo "$selected" "$release"
  CARGO_TARGET_DIR=$fixed_target "$PYTHON_RUNNER" "$WRAPPER" shared --workspace-root "$case_root" -- \
    "$ACTOR" "$output" "$selected" "$release" "$SOURCE" \
    >"$case_root/fixed.out" 2>"$case_root/fixed.err" &
  build_pid=$!
  PIDS+=("$build_pid")
  IFS= read -r selected_output <"$selected"
  [ "$selected_output" = "$output" ] || fail "fixed build selected an unexpected compiler output"

  set +e
  CARGO_TARGET_DIR=$fixed_target "$PYTHON_RUNNER" "$WRAPPER" exclusive --refuse \
    --workspace-root "$case_root" -- "$PYTHON_RUNNER" -c \
    'import os, shutil; shutil.rmtree(os.environ["CARGO_TARGET_DIR"])' \
    >"$case_root/cleanup.out" 2>"$case_root/cleanup.err"
  local cleanup_status=$?
  set -e
  [ "$cleanup_status" -eq 75 ] || fail "live-target cleanup did not refuse with exit 75"
  [ -d "$(dirname "$output")" ] || fail "cleanup removed the live compiler output directory"
  printf '%s\n' continue >"$release"
  wait "$build_pid" || fail "owned compiler failed after cleanup refusal"
  [ -s "$output" ] || fail "owned compiler object is missing"
}

run_sqlite_proof() {
  [ "$(uname -s)" = Linux ] || fail "the bundled-SQLite native compiler proof is Linux-only"
  command -v cc >/dev/null 2>&1 || fail "the bundled-SQLite proof requires a native cc compiler"

  local case_root=$TMP/sqlite
  local target=$case_root/target
  local selected=$case_root/selected
  local release=$case_root/release
  mkdir -p "$case_root"
  mkfifo "$selected" "$release"

  CARGO_TARGET_DIR=$target \
  CC=$SQLITE_CC \
  FLUX_REAL_CC=$(command -v cc) \
  FLUX_CC_BARRIER_CLAIM=$case_root/claimed \
  FLUX_CC_SELECTED_FIFO=$selected \
  FLUX_CC_RELEASE_FIFO=$release \
    "$PYTHON_RUNNER" "$WRAPPER" shared --workspace-root "$ROOT" -- \
    cargo test --release --no-run -p codewandler-flux-events --lib \
    >"$case_root/build.out" 2>"$case_root/build.err" &
  local build_pid=$!
  PIDS+=("$build_pid")
  local selected_output
  IFS= read -r selected_output <"$selected"
  case "$selected_output" in
    "$target"/release/build/libsqlite3-sys-*/out/*.o) ;;
    *) fail "SQLite cc selected output outside the reported release/build/.../out class" ;;
  esac

  set +e
  CARGO_TARGET_DIR=$target "$PYTHON_RUNNER" "$WRAPPER" exclusive --refuse \
    --workspace-root "$ROOT" -- cargo clean --workspace \
    >"$case_root/cleanup.out" 2>"$case_root/cleanup.err"
  local cleanup_status=$?
  set -e
  [ "$cleanup_status" -eq 75 ] || fail "SQLite proof cleanup did not refuse with exit 75"
  [ -d "$(dirname "$selected_output")" ] || fail "SQLite output directory disappeared while cc was live"
  printf '%s\n' continue >"$release"
  wait "$build_pid" || { sed -n '1,200p' "$case_root/build.err" >&2; fail "bundled SQLite build failed"; }
  [ -s "$selected_output" ] || fail "bundled SQLite object was not produced"
}

run_disappearance_class
if [ "${1:-}" = --sqlite ]; then
  run_sqlite_proof
elif [ "$#" -ne 0 ]; then
  fail "usage: scripts/test-build-ownership.sh [--sqlite]"
fi

echo "build ownership compiler regression passed"
