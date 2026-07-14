# Provider-native schema portability

**Status:** accepted (A-81)  
**Date:** 2026-07-14

## Problem

Flux keeps operation input contracts as JSON Schema. Provider-native function interfaces do not all
accept the same JSON Schema vocabulary: Gemini function declarations use an OpenAPI subset, and the
OpenRouter Gemini adapter rejects an array without `items` and a `required` name missing from
`properties`. Both shapes are legal JSON Schema. They appeared in the adaptive cognition catalog and
in Slack Block Kit operations, so Gemini rejected the whole request before generation.

The original schema is also an authority boundary. A wire compatibility fix must not replace the
schema later used by planning, approval, or runtime argument validation.

## Boundary

`flux_provider::Request` remains the provider-neutral, host-owned value. A concrete wire codec may
derive cloned tool definitions while it builds its disposable request body. It never mutates the
request or the registered `ToolSpec`.

OpenRouter owns a model-aware schema profile because it is the adapter that translates the common
Chat or Messages request into Gemini's native declaration. The profile applies only to model ids
under `google/gemini-`; Anthropic, OpenAI, Codex, other OpenRouter models, and Ollama retain their
schemas byte-for-byte.

## Deterministic projection

Google documents function declarations as an OpenAPI subset: `type`, `nullable`, `required`,
`format`, `description`, `properties`, `items`, `enum`, `anyOf`, `$ref`, and `$defs`. This is a
narrower contract than Gemini structured outputs; a keyword documented only for structured output is
not assumed portable for a function declaration. The retained A-78 catalog and focused live probes
also demonstrate that OpenRouter's Gemini adapter accepts `additionalProperties`, `maxItems`, and
`minimum`, so those three are explicit profile additions. The focused two-wire live probe includes a
local `$defs` target reached through `$ref`, resolving the provider documentation's native-vs-OpenAPI
spelling ambiguity at the actual OpenRouter boundary. Enum members must be strings, as required by
Gemini's function schema contract. No other assertion keyword passes through.

The profile recursively applies only deterministic, equivalence-preserving rewrites:

- an array schema with no `items` receives `items: {}`; in JSON Schema both forms permit every item;
- a `required` name absent from `properties` receives the schema that already governed it through
  `additionalProperties` (`{}` when that keyword is absent or `true`);
- a nullable JSON Schema union such as `type: ["string", "null"]` becomes the equivalent OpenAPI
  spelling `type: "string", nullable: true`; the common type-only
  `anyOf: [{type: T}, {type: "null"}]` spelling normalizes identically;
- a multi-concrete `type` union with only annotation/declaration siblings becomes equivalent
  type-only `anyOf` branches; combinations with `null` or assertion siblings reject until an exact
  provider-supported rewrite is proven;
- a non-empty all-string enum with no explicit type receives its already-implied `type: "string"`;
- annotation-only fields (`default`, `title`, examples/comments, and `x-*` metadata) are removed from
  the cloned wire view because they do not change the accepted instance set;
- supported schema-valued children (`properties`, `items`, `anyOf`, `$defs`, and schema-valued
  `additionalProperties`) are visited in stable object/array order.

If `additionalProperties: false` makes an absent required property impossible, or another boolean
`false` subschema, compound union, standalone `nullable: true`, or unsupported assertion keyword
cannot be represented without widening it, the codec rejects the operation locally. When a
required property inherits a schema from `additionalProperties`, any incompatibility retains that
original source pointer rather than naming the synthetic materialized property. The diagnostic
names the operation and its exact RFC 6901 JSON Pointer. Body construction happens before any HTTP
or alternate-stream transport connection, so an incompatible catalog cannot incur a paid request.
The function-parameters root must explicitly remain a non-null object; primitive/array roots and a
nullable root object reject locally, while arrays nested under object properties retain the equivalence
rewrites above.

The projection never strips an assertion. A future incompatibility must gain another proven
equivalence rewrite or explicit provider evidence, or fail locally with the same operation/path
contract.

## Validation and evidence

Returned native calls still resolve to the live registered operation and validate against its
original `ToolSpec::input_schema` before dispatch. Projection grants no permission and changes no
approval, intent, or effect metadata.

Hermetic tests cover both OpenRouter wire shapes, nested arrays, required/property repair, exact
simple-union projection, local rejection before transport (including unsupported keywords,
compound/nullable unions, standalone nullable widening, and non-string enum members),
root-object enforcement, request immutability, and unchanged Anthropic/OpenAI/Codex behavior. A credentialed live probe uses an
inert function declaration with `$defs`/`$ref` only: the model is told not to call it, and no Flux
operation is registered or executed. Both the Chat and Messages wire probes passed.

The rebuilt CLI then passed the exact retained scenarios: A-78 support session `s_1439` cited all
four fixture sources with the correct facts, and Bitcoin-to-Slack session `s_1440` selected Slack,
reached denial, and executed zero action batches. Neither recorded the old Gemini validation error or
a local portability rejection. The provider contracts are documented by
[Gemini function calling](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/tools/function-calling)
and [OpenRouter tool calling](https://openrouter.ai/docs/guides/features/tool-calling).
