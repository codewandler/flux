#!/usr/bin/env bash
# Shared Bash adapter for repository-owned Cargo entry points. The Python module remains the sole
# owner of target resolution and lock lifecycle; this file only locates that checked-in bootstrap.

BUILD_OWNERSHIP_SOURCE_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

with_build_ownership_at() {
  if [ "$#" -lt 2 ]; then
    echo "with_build_ownership_at requires <workspace-root> <command...>" >&2
    return 2
  fi
  local workspace_root=$1
  shift
  "$BUILD_OWNERSHIP_SOURCE_ROOT/scripts/run-python3.sh" \
    "$BUILD_OWNERSHIP_SOURCE_ROOT/scripts/build_ownership.py" shared \
    --workspace-root "$workspace_root" -- "$@"
}

with_build_ownership() {
  local workspace_root
  workspace_root=$(git rev-parse --show-toplevel) || return
  with_build_ownership_at "$workspace_root" "$@"
}

owned_cargo_at() {
  if [ "$#" -lt 2 ]; then
    echo "owned_cargo_at requires <workspace-root> <cargo-arguments...>" >&2
    return 2
  fi
  local workspace_root=$1
  shift
  with_build_ownership_at "$workspace_root" cargo "$@"
}

owned_cargo() {
  local workspace_root
  workspace_root=$(git rev-parse --show-toplevel) || return
  owned_cargo_at "$workspace_root" "$@"
}

owned_target_at() {
  if [ "$#" -ne 1 ]; then
    echo "owned_target_at requires <workspace-root>" >&2
    return 2
  fi
  "$BUILD_OWNERSHIP_SOURCE_ROOT/scripts/run-python3.sh" \
    "$BUILD_OWNERSHIP_SOURCE_ROOT/scripts/build_ownership.py" resolve \
    --workspace-root "$1"
}

owned_target() {
  local workspace_root
  workspace_root=$(git rev-parse --show-toplevel) || return
  owned_target_at "$workspace_root"
}
