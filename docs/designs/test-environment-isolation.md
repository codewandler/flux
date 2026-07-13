# Test environment isolation

## Failure

Sandbox discovery tests need controlled `PATH` contents. They previously achieved that with
`std::env::set_var("PATH", temporary_directory)` under a mutex shared only by tests that also mutate
sandbox environment variables. Cargo runs ordinary process tests in parallel without that lock.
Those tests could therefore construct a child while the temporary PATH was active; the guarded
environment correctly forwarded that value, but basic programs such as `sh`, `printf`, `env`, and
`sleep` were no longer resolvable. Thirteen process tests then failed as one cluster, with secondary
symptoms such as a missing PID file.

## Contract

Environment discovery keeps a small pure seam: production wrappers read `PATH` and override variables
once, then pass their values into helpers. Tests call the helpers with explicit `OsStr` values and do
not modify process-wide PATH. This tests the same splitting, lookup, canonicalization, and preflight
logic without exposing unrelated threads to temporary state.

The existing environment guard remains appropriate for sandbox variables whose production entry
point itself is under test. Test-only temporary-workspace construction takes that guard while reading
`TMPDIR`, preventing the remaining TMPDIR mutation tests from redirecting unrelated fixtures.
