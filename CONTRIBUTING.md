# Contributing to flux

Thanks for your interest in flux. Contributions — bug reports, fixes, features, docs — are welcome.

## Before you start

- Read **[AGENTS.md](AGENTS.md)** — it's the contributor contract (the layering rule, the
  non-negotiable safety invariants, and where to make common changes). It applies to humans and
  agents alike.
- Read **[docs/vision.md](docs/vision.md)** for the project's direction and principles. The headline:
  **the LLM is not the runtime** (the model compiles a request into a plan the runtime executes, with
  non-bypassable safety as the invariant that buys), and **quality over quantity** — flux is
  deliberately small, correct, and fully tested, not a sprawling feature pile.

## The green gate (run before opening a PR)

A change isn't done until all of these pass — CI enforces them:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test -p flux-codegate          # architecture layering lint
```

Every behavioral change ships with a test that fails before it. A safety-invariant change (anything
touching the authorization → approval → guarded-IO chain) must keep the no-bypass tests passing and
add to them.

## Pull requests

- Branch off `main`; keep PRs focused and atomic.
- Commit titles in the imperative mood with a short bulleted body explaining what changed and why
  (title-only commits aren't accepted). Ticket references go in a trailing `Refs:` line, not the title.
- Don't introduce an inner→outer crate dependency (the layering lint will fail), bypass the safety
  envelope, or log secret values.

## Reporting bugs / requesting features

Use the issue templates. For anything security-sensitive, **do not open a public issue** — see
[SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contributions are dual-licensed under
[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), the same as the project.
