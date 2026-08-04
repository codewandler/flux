#!/usr/bin/env bash
#
# build-portable-wasm.sh — build the portable Flux evaluation core for WebAssembly and prove it
# agrees with the native engine (C-271, epic C-268 "a portable Flux runtime").
#
# The artifact is a `wasm32-unknown-unknown` cdylib of `flux-lang` — the language plus its reference
# interpreter — behind the hand-written three-function ABI in
# `crates/flux-lang/examples/portable/wasm_abi.rs`. It declares **zero imports**: a model-free `.flux`
# program needs no clock, no filesystem, no socket and no model, so the "no ambient authority"
# property is structural rather than policy.
#
#   scripts/build-portable-wasm.sh          # build, then run the parity test against the artifact
#   scripts/build-portable-wasm.sh --build  # build only
#
# Prerequisite: `rustup target add wasm32-unknown-unknown`. The script refuses to run without it
# rather than falling back to a host build that would prove nothing.
#
# Design: docs/designs/portable-wasm-runtime.md · Story: docs/stories/C-271-portable-core-wasm-parity.md
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
source scripts/build-ownership.sh
TARGET_ROOT=$(owned_target)

TARGET=wasm32-unknown-unknown
PKG=codewandler-flux-lang
EXAMPLE=flux_portable

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "FAIL: the $TARGET target is not installed." >&2
  echo "      Install it with: rustup target add $TARGET" >&2
  exit 2
fi

echo "== building the portable core for $TARGET =="
owned_cargo build -p "$PKG" --example "$EXAMPLE" --target "$TARGET" --release

ARTIFACT="$TARGET_ROOT/$TARGET/release/examples/$EXAMPLE.wasm"
[ -f "$ARTIFACT" ] || { echo "FAIL: expected $ARTIFACT to exist" >&2; exit 1; }
echo "   $ARTIFACT ($(wc -c <"$ARTIFACT") bytes)"

if [ "${1:-}" = "--build" ]; then
  exit 0
fi

# FLUX_PORTABLE_WASM_REQUIRED turns "the module is not built" from a skip into a failure, so this
# command cannot report success without actually having executed the module.
echo "== parity: the same .flux through the native engine and the wasm module =="
FLUX_PORTABLE_WASM="$ARTIFACT" \
FLUX_PORTABLE_WASM_REQUIRED=1 \
  owned_cargo test -p "$PKG" --test wasm_parity

printf '\033[32mPASS\033[0m the wasm32 portable core matches the native engine\n'
