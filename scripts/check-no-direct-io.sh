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
# C-269 added the second half. Once guarded IO has a *port* (`flux_system::port`), "no direct IO" is
# no longer sufficient on its own: a type can now satisfy `GuardedProcess` and enforce nothing, which
# the direct-IO scanner cannot see because it bounds syscall construction, not the semantics of a
# guard. So the whole-tree backend enumeration runs from the same entry point.
#
#   scripts/check-no-direct-io.sh              # scan the classified production packs + port backends
#   scripts/check-no-direct-io.sh --self-test  # exercise every API family and alias bypass fixture
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
source scripts/build-ownership.sh

case "${1:-}" in
  "")
    owned_cargo test -p flux-codegate --lib -- tests::no_unreviewed_direct_io_in_model_facing_operation_crates --exact
    owned_cargo test -p flux-codegate --lib -- tests::no_unreviewed_guarded_port_backend_outside_system --exact
    ;;
  --self-test)
    owned_cargo test -p flux-codegate --lib -- tests::direct_io_scanner_resolves_imports_aliases_and_all_io_families --exact
    owned_cargo test -p flux-codegate --lib -- tests::direct_io_scanner_resolves_local_callable_aliases_for_all_io_families --exact
    owned_cargo test -p flux-codegate --lib -- tests::direct_io_scanner_resolves_known_io_glob_imports --exact
    owned_cargo test -p flux-codegate --lib -- tests::direct_io_allowance_requires_a_real_reason_immediately_above_the_call --exact
    owned_cargo test -p flux-codegate --lib -- tests::port_impl_scanner_finds_production_backends_and_ignores_test_doubles --exact
    owned_cargo test -p flux-codegate --lib -- tests::port_impl_scanner_resolves_renamed_trait_imports --exact
    owned_cargo test -p flux-codegate --lib -- tests::port_impl_scanner_excuses_only_cfg_test_not_other_cfgs --exact
    ;;
  *)
    printf 'usage: %s [--self-test]\n' "$0" >&2
    exit 2
    ;;
esac
