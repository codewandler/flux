# Model request lifecycle trace

## Boundary

`NativeProvider::stream` is the last credential-free request boundary shared by native Anthropic,
OpenAI, OpenRouter, Ollama, Bedrock, Codex HTTP, and Codex WebSocket transports. It sees the final
codec body after credential-required system prefixes are applied, but before authentication headers
are attached. Instrumenting here covers planner, completion, compaction, cognition, and sub-agent
calls without changing their APIs or prompt layout.

## Modes

- unset: no trace object and no per-chunk inspection;
- `FLUX_MODEL_TRACE=1` (or `summary`): request dimensions and timing milestones only;
- `FLUX_MODEL_TRACE=full`: the summary plus the exact JSON body. This can contain prompts, source
  text, and tool results. It never includes credential headers, but is explicitly sensitive debug
  output and must stay opt-in.

Each request gets a process-local correlation id. The request record includes provider/model,
thinking/effort, max output tokens, message/tool counts, system/message/body byte sizes, and cache
segment sizes. The stream record reports monotonic microseconds from provider entry to body built,
transport connected/response headers, first decoded chunk, first reasoning/tool/text/usage/done,
stream end, plus final usage and error/cancellation status.

The terminal record also reports HTTP attempt count, forced OAuth refresh count, and whether the
preferred streaming transport fell back to HTTP. A request that fails before a chunk stream exists
still emits `terminal: "request_error"`; a cancelled/dropped stream emits `terminal: "dropped"`.

The trace is observation only: it wraps the already-decoded `ChunkStream`, returns every item
unchanged, and emits the terminal record on EOF or drop. A stream dropped by cancellation is marked
incomplete rather than misreported as a provider finish.

## Live finding

The first trace split provider time from roughly 2.5 seconds of command overhead. A no-plugin HOME
control isolated installed-plugin startup; the plugin-loader fix then reduced three warm mock runs
from 2.222–2.246 seconds to 0.585–0.592 seconds. A subsequent live Codex run spent 1.652 seconds in
the provider and about 0.75 seconds elsewhere. Model choice still controls TTFT, but it was not the
dominant structural regression.
