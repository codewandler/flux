#!/usr/bin/env bash
#
# check-no-direct-io.sh — the one CI entry point for the structural model-facing I/O gate.
#
# C-263 moved enforcement into flux-codegate's `syn` scanner. It resolves Rust imports, renamed
# imports, module/type aliases, local callable aliases, and multiline calls for filesystem,
# process, socket, HTTP-client, and database opens. The same code owns the exhaustive production
# operation-pack classification; this wrapper intentionally carries no crate list or weaker
# text-pattern fallback.
#
#   scripts/check-no-direct-io.sh              # scan the classified production packs
#   scripts/check-no-direct-io.sh --self-test  # exercise every API family and alias bypass fixture
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

case "${1:-}" in
  "")
    cargo test -p flux-codegate --lib -- tests::no_unreviewed_direct_io_in_model_facing_operation_crates --exact
    ;;
  --self-test)
    cargo test -p flux-codegate --lib -- tests::direct_io_scanner_resolves_imports_aliases_and_all_io_families --exact
    cargo test -p flux-codegate --lib -- tests::direct_io_scanner_resolves_local_callable_aliases_for_all_io_families --exact
    cargo test -p flux-codegate --lib -- tests::direct_io_scanner_resolves_known_io_glob_imports --exact
    cargo test -p flux-codegate --lib -- tests::direct_io_allowance_requires_a_real_reason_immediately_above_the_call --exact
    ;;
  *)
    printf 'usage: %s [--self-test]\n' "$0" >&2
    exit 2
    ;;
esac
