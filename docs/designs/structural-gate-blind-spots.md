# Structural gate blind spots — 2026-08-01

## Context

`ASSURE-02` and `ASSURE-03` alleged that the repo's source gates match spellings rather than enforce
invariants. The validation pass mutation-tested both. The filed holes are genuinely closed —
`flux-eval` is in `MODEL_FACING_OPERATION_CRATES`, unresolved risk rows now fail with the op name,
and the registration census AST-walks all of `crates/flux-cli/src` — but the *class* survived in
eleven places, one of them **live in the tree today**.

This is the repo's recorded "guards tested against their own assumptions" pattern: a self-test
fixture agrees with the guard rather than with reality, so a green run proves the guard is
self-consistent, not that the invariant holds.

## Mutations that pass the gates today

| Mutation | Verdict |
| --- | --- |
| `ureq::post(..)` outbound HTTP in a scanned model-facing crate | **MISS — live at `crates/flux-capabilities/src/datasource/embeddings.rs:130`, un-waived** |
| `const OPEN: fn(&str) -> _ = std::fs::read_to_string;` (the `const`/`static`/field twin of the `let` hole C-263 closed) | MISS |
| `reqwest::ClientBuilder::new()`, `Client::default()`, `TcpStream::connect_timeout`, `std::os::unix::fs::symlink` | MISS — the pattern set is an exact type+method allow-list |
| I/O inside any macro body | MISS — `syn` never parses macro token streams; affects **every** syn gate in the repo |
| Move the call into an unscanned crate and call it from a scanned one | MISS for FS/socket/HTTP/DB; only `Command` is enforced tree-wide |
| Delete a crate from `MODEL_FACING_OPERATION_CRATES` | MISS — the only backstop is `scanned > 50`, and 110 files are scanned |
| Register an op from `flux-app`'s assembly | **MISS with a live consequence** — `emit`/`send`/`ask`/`spawn` are in no census and no risk gate |
| `registry.register(..)` / `try_extend` / the infallible `register_*` family | MISS — the visitor records only idents prefixed `try_register` |
| `let source = "new pack"; registry.try_register_all_from(source, ..)` | MISS — the literal token `source` is a blanket exclusion |
| Move an op's row into a table with no Risk column | MISS — 57 of 164 op rows already sit in risk-less tables |
| Document an op in prose on the website page | MISS — `website_contract.rs` still does a substring search, the weakness C-248 fixed only for the in-repo reference |

## Finding-to-story traceability

| Residual | Story |
| --- | --- |
| `ureq` invisible to the direct-I/O gate + the live un-waived hit | C-364 |
| `const`/`static`/field alias capture and macro bodies | C-365 |
| Hand-maintained crate classification, allow-list pattern set, uncapped waivers, cross-crate escape | C-366 |
| `flux-app` is a second production catalog no census covers; infallible registration family | C-367 |
| Risk-tier publication is conditional on the table happening to have a Risk column; website coverage is substring-satisfiable | C-368 |

## Decisions

- **A gate is validated by the mutation it rejects, never by its own fixture.** Every story here
  lands its representative bad change first and proves the gate reds on it.
- **Prefer derivation over enumeration.** Hand-maintained lists (`MODEL_FACING_OPERATION_CRATES`,
  the pattern set, the census roots) drift silently; where a list must exist, a test binds it to the
  production assembly it claims to describe.
- **Waivers get a budget.** The pin census caps exemptions at one; direct-I/O has 35 with no cap and
  accepts any non-empty text. Copy the capped pattern.
- **Macro bodies are a known, repo-wide blind spot.** Record it explicitly rather than letting each
  gate imply coverage it does not have.
