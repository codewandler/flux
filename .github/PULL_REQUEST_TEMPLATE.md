<!-- Thanks for contributing! Keep PRs focused and atomic. See AGENTS.md + CONTRIBUTING.md. -->

## What & why

<!-- What does this change, and why? Link any issue with `Refs: #123`. -->

## Checklist

- [ ] Ran `scripts/build-embedded-docs.sh`, committed
      `crates/flux-server/assets/public-docs.zip` if it changed, then ran
      `scripts/build-embedded-docs.sh --check` against the committed checkout
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all --check` is clean
- [ ] `cargo test -p flux-codegate` passes (no inner→outer crate dependency)
- [ ] Behavioral changes ship with a test that fails before the change
- [ ] No safety-envelope bypass; no secret values logged or surfaced
