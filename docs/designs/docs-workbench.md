# Docs workbench: one editor, guarded local execution

**Story:** L-128
**Status:** shipped

L-127 deliberately shipped an effect-free structural playground. L-128 keeps that surface as the
portable baseline and adds a separate, loopback-only execution plane. The distinction is a security
boundary: a public bind serves docs and parsing, while a loopback bind may create isolated scratch
sessions only after the browser proves it holds the unguessable launch secret printed by the CLI.

## Surface and trust model

`FluxWorkbench` is the single React component used by `/console` and opted-in Flux documentation
blocks. It loads Monaco on interaction, speaks ordinary LSP JSON-RPC over a same-origin WebSocket,
and progressively exposes capabilities returned by `/api/workbench/bootstrap`. The hosted site has
no bootstrap runtime capability, so it offers syntax, editing, and structural checking only.

The launch URL carries a one-use secret in its fragment. The fragment is not sent in HTTP requests;
the client exchanges it for an HttpOnly, SameSite=Strict session cookie and immediately clears the
fragment. Runtime and LSP routes require that cookie plus same-origin/host checks. Sessions are
in-memory and scoped to the serving process.

Non-loopback mode does not merely reject execution requests: it does not construct the runtime and
does not mount execution, approval, scratch, app, or LSP routes. This preserves L-127's safe public
preview workflow.

## Execution boundary

The browser names a checked-in fixture manifest; it cannot submit an arbitrary capability set.
Each manifest supplies authored Flux, allowed scratch files, input schema, and its supported mode:
single flow or persistent app. The server copies only those files into a fresh
`flux_system::ScratchWorkspace`, synchronizes user edits through guarded `System` calls, parses the
program, freezes a plan, and returns graph/risk data before execution.

Execution uses `FlowClient` or `App` with the production `Executor::dispatch` path, a narrow registry
(built-ins, cognition, web, and markdown datasource as required), accurate authorization, resource
limits, cancellation, and `RemoteApprover`. Approval decisions are keyed to the runtime's canonical
fingerprint; editing or replanning invalidates them. Shell, plugins, the repository workspace,
sub-agents, host secrets, and private-network grants are never registered.

Scratch directories live for the docs browser session so files and app state can be inspected. They
are removed when the session ends or the server shuts down. App input is serialized through one
live app session; changing Part A/Part B requires an explicit restart so program identity cannot
silently change under accumulated state.

## LSP transport

`flux-lsp` exposes its existing tower-lsp service through a reusable IO bootstrap. Stdio keeps the
client-provided workspace-root policy. The docs WebSocket constructs the same backend with a fixed
scratch root; `initialize.rootUri`, workspace-folder changes, and document URIs outside that root
cannot redirect it to host files. WebSocket messages remain standard LSP JSON-RPC, with the bridge
adding/removing Content-Length framing only at the server boundary.

## Declared examples

The examples page declares `summarize-readme`, `latest-release`, `cached-page`,
`wait-for-artifact`, and `rust-files` as runnable with purpose-built fixtures. `route-ticket` has an
undefined classifier, `repo-survey` assumes a real Git checkout, the improvement excerpt is
abridged, and the Zendesk example needs an external integration, so those remain edit/check-only.

The first-app tutorial declares Part A and Part B app fixtures, including the guide and markdown
files introduced by the previous tutorial steps. Their browser sessions persist until stop/reset or
browser/server termination.

## Failure posture

Parse, type, and LSP diagnostics are normal editor results. Runtime errors and denial are streamed
as terminal events without exposing secrets. Disconnect cancels a flow run but does not implicitly
approve or deliver app input. Missing model credentials fail lazily at the first model-backed op;
the docs server itself still starts and deterministic examples remain usable.
