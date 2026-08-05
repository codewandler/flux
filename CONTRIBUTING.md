# Contributing to flux

Thanks for your interest in flux. Contributions — bug reports, fixes, features, docs — are welcome.

## Before you start

- Read **[AGENTS.md](AGENTS.md)** — it's the contributor contract (the layering rule, the
  non-negotiable safety invariants, and where to make common changes). It applies to humans and
  agents alike.
- Read **[docs/vision.md](docs/vision.md)** for the project's direction and principles. The headline:
  **the LLM is not the runtime** (the model supplies bounded judgment inside an authored adaptive
  loop; the runtime owns control flow and executes approved action batches, with non-bypassable
  safety as the invariant that buys), and **quality over quantity** — flux is deliberately small,
  correct, and fully tested, not a sprawling feature pile.

## The green gate (run before opening a PR)

A change isn't done until all of these pass — CI enforces them:

```bash
scripts/build-embedded-docs.sh
# If the archive changed, include it in the same commit as the website/source change:
git add crates/flux-server/assets/public-docs.zip
git commit
scripts/build-embedded-docs.sh --check

cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test -p flux-codegate          # architecture layering lint
```

The order matters: the final freshness check is against the committed checkout. Do not open the PR
with an uncommitted regenerated archive; a change outside `website/**` can still affect generated
public documentation, so CI runs this check on every pull request without a path filter.

Repository build scripts use `scripts/owned-cargo` to hold shared OS ownership of the resolved
Cargo target; `task clean` is the only repository cleanup entry point and requires exclusive
ownership. This keeps `CARGO_TARGET_DIR` reusable across compatible builds while preventing cleanup
from unlinking live compiler output. Python 3.10+ is the pre-Cargo prerequisite: Task selects
`python3`/`python` on Linux and macOS and `python`/`py -3` on Windows, or accepts an explicit
`PYTHON=<executable>`. If a direct operator `cargo` command shares the target, finish it before
running `task clean`.

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
