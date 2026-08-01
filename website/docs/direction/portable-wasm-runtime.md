---
title: Portable WebAssembly runtime status
description: What Flux's model-free WebAssembly core proves today, and which pieces of a production sandboxed runtime are still unbuilt.
---

# Portable WebAssembly runtime status

:::info Status as of August 1, 2026
The repository contains a working WebAssembly proof for the portable, model-free `flux-lang` core
and enforces native/Wasm parity in CI. It does **not** contain a production sandboxed runtime for
running submitted `.flux` programs.
:::

## What exists today

The [`flux_portable`
example](https://github.com/codewandler/flux/blob/main/crates/flux-lang/examples/portable/wasm_abi.rs)
compiles the `flux-lang` parser and reference interpreter to `wasm32-unknown-unknown`. Its small
evaluation ABI accepts UTF-8 `.flux` source and returns a JSON result through three exports:
`flux_alloc`, `flux_eval`, and `flux_dealloc`.

That module is deliberately model-free and host-free:

- It evaluates the pure language fragment: values, expressions, formatting, field access,
  constructors, and control flow.
- Its operation catalog is empty. Model calls, filesystem access, process execution, network
  access, and every other operation are refused.
- It declares no Wasm imports, so this proof module receives no clock, filesystem, network,
  process, model, or other ambient capability.

The [`wasm_parity`
test](https://github.com/codewandler/flux/blob/main/crates/flux-lang/tests/wasm_parity.rs) runs the
same checked-in source through the same reference-interpreter core compiled once for the native host
and once for Wasm. It checks for byte-identical results, verifies the Wasm import list is empty, and
verifies that an operation call is refused on both targets. A dedicated
[CI job](https://github.com/codewandler/flux/blob/main/.github/workflows/ci.yml) builds the module
and requires those checks to run rather than silently skip when the artifact is absent.

## Implementation boundary

| Area | Current status |
|---|---|
| Portable `flux-lang` parser and reference interpreter | Implemented for a model-free language fragment. |
| Native/Wasm parity check | Implemented and run in CI. |
| `flux_flow::FlowEngine` in Wasm | Not built. The proof module contains the `flux-lang` reference interpreter, not the production agent engine. |
| Guarded host-import ABI | Not built. The current module has an evaluation **export** ABI and zero host imports. |
| Model calls and built-in operations | Not available. Every operation is denied. |
| Fuel, memory, and wall-clock limits | Not built. The portable evaluator's poll ceiling is not an embedder resource limit. |
| CLI, server, browser package, or API for submitted programs | Not built. The build script and module are a developer and CI proof, not a supported product surface. |

This distinction matters: native/Wasm parity proves that one model-free program has the same
language result on both targets. It does not prove parity with `FlowEngine`, the tool dispatcher,
approval handling, provider calls, or native session behavior.

## Intended production boundary

The longer-term design keeps every security decision on the trusted host side of the sandbox:

> The guard runs outside the sandbox. The module never receives a raw capability.

If host operations are added, they must be narrow, already-authorized imports. For example, the
host would resolve and guard a named HTTP endpoint, pin the vetted address, and inject credentials;
the module would never receive a general `fetch(url)` primitive or the credential itself. The same
rule applies to files, processes, clocks, and model providers.

That host-import boundary is still design work. The current zero-import module demonstrates the
starting posture but does not implement guarded capabilities.

## What WebAssembly would not solve by itself

Even after host imports exist, the embedder still has to enforce explicit limits and policy:

- **CPU, memory, and time:** a Wasm sandbox does not automatically stop an infinite computation or
  allocation bomb. Fuel or epoch interruption, a memory ceiling, and a deadline remain required.
- **Authorized exfiltration:** a destination grant can constrain where data goes, not what the
  program sends there.
- **Secret hygiene:** secrets stay protected only if the host never passes their raw values into
  the module.
- **Native-process confinement:** a future Wasm boundary would complement, not replace, Flux's OS
  sandbox for native execution.
- **Side channels:** timing and memory-growth observations remain outside this design.

## Reproducing the current proof

For repository development, install the `wasm32-unknown-unknown` Rust target and run:

```bash
./scripts/build-portable-wasm.sh
```

The script builds the module and then runs the required native/Wasm parity tests. The full rationale
and remaining architecture are recorded in the
[portable runtime design](https://github.com/codewandler/flux/blob/main/docs/designs/portable-wasm-runtime.md).
