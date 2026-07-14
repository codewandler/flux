# Typed plugin operation migration

`host_kit::PluginBuilder::operation_typed<I, O>` is the default registration path for closed Rust
contracts. It derives both schemas, deserializes once with a field path, normalizes serde aliases and
defaults before shared preflight, invokes `Fn(I, &mut Host) -> Result<O, String>`, and serializes the
typed result. `operation_flexible` is the explicit escape hatch for genuinely open vendor payloads;
the compatibility `operation` spelling is deprecated and must not be used by first-party plugins.

The first migrations deliberately cover both ends of the spectrum: `websearch` has the empty-input
`provider.list` operation and the alias-heavy, multi-query `web.search` operation; Jira has the
stable attachment list/get family while preserving each open Atlassian attachment object verbatim.
These result families have generated output schemas. Other operations already derive input schemas
(D-36), but their handlers remain on the explicit flexible adapter until each vendor result family
is typed without erasing vendor-specific semantics.

| Plugin | Typed executable handlers | Next migration unit |
| --- | --- | --- |
| alertmanager | pending | status/alert list result families |
| asterisk | pending | ping/channel list, then AMI command variants |
| aws | pending | caller identity/list outputs before credential-heavy operations |
| confluence | pending | test/page show/list outputs |
| docker | pending | version/container list, then stream variants |
| gitlab | pending | project/MR/issue list+show families in bounded batches |
| grafana | pending | health/dashboard list+show |
| homer | pending | service list/status |
| huggingface | pending | model/dataset/space list+get |
| jira | attachment list/get, typed stable envelopes | issue/project list+show |
| kubernetes | pending | version/pod list+show before apply/log streams |
| loki | pending | labels/query result families |
| onepassword | pending | vault/item list+get, preserving secret redaction |
| opsgenie | pending | alert/schedule list+show |
| prometheus | pending | labels/series/query result families |
| slack | pending | channel/user/message list+show |
| sql | pending | server info and row-envelope output |
| vault | pending | mount/list metadata before secret-bearing reads |
| websearch | **migrated** | `web.search`, `websearch.provider.list`, typed ranked/provider outputs |

Migration rules:

1. Derive `Deserialize + Serialize + JsonSchema` for input and `Serialize + JsonSchema` for output.
2. Register with `operation_typed`; remove schema-only `allow(dead_code)` and manual field extraction.
3. Put serde aliases/defaults on the input type. Custom preflight sees the normalized canonical JSON.
4. Keep `operation_flexible` only for an intentionally open payload and state why beside the call.
5. Pin the manifest input/output schemas and existing vendor contract fixtures in the same change.
