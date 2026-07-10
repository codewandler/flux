//! `onepassword` — 1Password Connect integration over host-managed HTTP.
//!
//! The plugin uses the Connect REST API (`OP_CONNECT_HOST`) with bearer-token injection from
//! `OP_CONNECT_TOKEN`. It does not shell out to the `op` CLI.

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

const ENDPOINT: &str = "onepassword.endpoint";
const AUTH_TOKEN: &str = "connect_token";
const GROUP_SERVER: &str = "onepassword.server";
const GROUP_VAULTS: &str = "onepassword.vaults";
const GROUP_ITEMS: &str = "onepassword.items";
const GROUP_FILES: &str = "onepassword.files";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct EmptyInput {}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct VaultInput {
    vault: String,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ItemListInput {
    vault: String,
    filter: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ItemInput {
    vault: String,
    item: String,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ItemCreateInput {
    vault: String,
    item: Value,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ItemReplaceInput {
    vault: String,
    item: String,
    body: Value,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ItemPatchInput {
    vault: String,
    item: String,
    patch: Value,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct FileInput {
    vault: String,
    item: String,
    file: String,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct FileContentInput {
    vault: String,
    item: String,
    file: String,
    name: Option<String>,
}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("onepassword", env!("CARGO_PKG_VERSION"))
        .capabilities(Caps {
            http: true,
            blob: true,
            secrets: vec!["OP_CONNECT_TOKEN".into()],
            private_hosts: vec!["*".into()],
            ..Default::default()
        })
        .auth(AuthMethod::bearer(
            AUTH_TOKEN,
            vec!["OP_CONNECT_TOKEN".into()],
        ))
        .endpoint(EndpointSpec {
            name: ENDPOINT.into(),
            env: vec!["OP_CONNECT_HOST".into()],
            description: "1Password Connect server base URL.".into(),
            ..Default::default()
        })
        .datasource(ds(
            "onepassword.vaults",
            "onepassword.vault",
            "1Password vault metadata.",
        ))
        .datasource(ds(
            "onepassword.items",
            "onepassword.item",
            "1Password item metadata; concealed field values are not indexed.",
        ))
        .datasource(ds(
            "onepassword.files",
            "onepassword.file",
            "1Password file metadata; file bytes are not indexed.",
        ))
        .group(op_group(
            GROUP_SERVER,
            "1Password Connect server health and activity.",
            &[
                "onepassword.heartbeat",
                "onepassword.health",
                "onepassword.activity.list",
            ],
        ))
        .group(op_group(
            GROUP_VAULTS,
            "1Password vault operations.",
            &["onepassword.vault.list", "onepassword.vault.show"],
        ))
        .group(op_group(
            GROUP_ITEMS,
            "1Password item operations.",
            &[
                "onepassword.item.list",
                "onepassword.item.show",
                "onepassword.item.create",
                "onepassword.item.replace",
                "onepassword.item.patch",
                "onepassword.item.delete",
            ],
        ))
        .group(op_group(
            GROUP_FILES,
            "1Password file operations.",
            &[
                "onepassword.file.list",
                "onepassword.file.show",
                "onepassword.file.content",
            ],
        ))
        .operation(
            grouped(
                read_op_typed::<EmptyInput>(
                    "onepassword.heartbeat",
                    "Read the Connect server heartbeat endpoint.",
                ),
                GROUP_SERVER,
            ),
            op_heartbeat,
        )
        .operation(
            grouped(
                read_op_typed::<EmptyInput>(
                    "onepassword.health",
                    "Read the Connect server health endpoint.",
                ),
                GROUP_SERVER,
            ),
            op_health,
        )
        .operation(
            grouped(
                read_op_typed::<EmptyInput>(
                    "onepassword.activity.list",
                    "List 1Password Connect activity events.",
                ),
                GROUP_SERVER,
            ),
            op_activity_list,
        )
        .operation(
            grouped(
                read_op_typed::<EmptyInput>("onepassword.vault.list", "List 1Password vaults."),
                GROUP_VAULTS,
            ),
            op_vault_list,
        )
        .operation(
            grouped(
                read_op_typed::<VaultInput>("onepassword.vault.show", "Show one 1Password vault."),
                GROUP_VAULTS,
            ),
            op_vault_show,
        )
        .operation(
            grouped(
                read_op_typed::<ItemListInput>(
                    "onepassword.item.list",
                    "List 1Password items in a vault.",
                ),
                GROUP_ITEMS,
            ),
            op_item_list,
        )
        .operation(
            grouped(
                read_op_typed::<ItemInput>(
                    "onepassword.item.show",
                    "Show one 1Password item explicitly.",
                ),
                GROUP_ITEMS,
            ),
            op_item_show,
        )
        .operation(
            grouped(
                write_op_typed::<ItemCreateInput>(
                    "onepassword.item.create",
                    "Create a 1Password item.",
                ),
                GROUP_ITEMS,
            ),
            op_item_create,
        )
        .operation(
            grouped(
                write_op_typed::<ItemReplaceInput>(
                    "onepassword.item.replace",
                    "Replace a 1Password item.",
                ),
                GROUP_ITEMS,
            ),
            op_item_replace,
        )
        .operation(
            grouped(
                write_op_typed::<ItemPatchInput>(
                    "onepassword.item.patch",
                    "Patch a 1Password item.",
                ),
                GROUP_ITEMS,
            ),
            op_item_patch,
        )
        .operation(
            grouped(
                risked(
                    write_op_typed::<ItemInput>(
                        "onepassword.item.delete",
                        "Delete a 1Password item.",
                    ),
                    Risk::High,
                ),
                GROUP_ITEMS,
            ),
            op_item_delete,
        )
        .operation(
            grouped(
                read_op_typed::<ItemInput>(
                    "onepassword.file.list",
                    "List files attached to a 1Password item.",
                ),
                GROUP_FILES,
            ),
            op_file_list,
        )
        .operation(
            grouped(
                read_op_typed::<FileInput>(
                    "onepassword.file.show",
                    "Show 1Password file metadata.",
                ),
                GROUP_FILES,
            ),
            op_file_show,
        )
        .operation(
            grouped(
                read_op_typed::<FileContentInput>(
                    "onepassword.file.content",
                    "Download 1Password file content into a host blob.",
                ),
                GROUP_FILES,
            ),
            op_file_content,
        )
}

fn main() {
    manifest_builder().serve();
}

fn req(input: &Value, key: &str) -> Result<String, String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key} is required"))
}

fn op_get(host: &mut Host, path: &str) -> Result<Value, String> {
    host.get_json_ref(ENDPOINT, path, Some(AUTH_TOKEN))
}

fn op_send(host: &mut Host, method: &str, path: &str, body: &Value) -> Result<Value, String> {
    host.send_json_ref(ENDPOINT, method, path, Some(AUTH_TOKEN), body)
}

fn op_delete_json(host: &mut Host, path: &str) -> Result<Value, String> {
    let resp = host.http_ref(ENDPOINT, "DELETE", path, Some(AUTH_TOKEN), &[], None)?;
    if !resp.is_success() {
        return Err(format!(
            "DELETE {ENDPOINT} {path} -> {} {}",
            resp.status, resp.body
        ));
    }
    if resp.body.trim().is_empty() {
        Ok(json!({ "deleted": true }))
    } else {
        resp.json()
    }
}

fn op_heartbeat(_: Value, host: &mut Host) -> Result<Value, String> {
    host.get_json_ref(ENDPOINT, "/heartbeat", None)
}

fn op_health(_: Value, host: &mut Host) -> Result<Value, String> {
    host.get_json_ref(ENDPOINT, "/health", None)
}

fn op_activity_list(_: Value, host: &mut Host) -> Result<Value, String> {
    op_get(host, "/v1/activity")
}

fn op_vault_list(_: Value, host: &mut Host) -> Result<Value, String> {
    let out = op_get(host, "/v1/vaults")?;
    contribute_vaults(host, &out);
    Ok(out)
}

fn op_vault_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    op_get(host, &format!("/v1/vaults/{vault}"))
}

fn op_item_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let mut path = format!("/v1/vaults/{vault}/items");
    if let Some(filter) = input
        .get("filter")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        path.push_str("?filter=");
        path.push_str(filter);
    }
    let out = op_get(host, &path)?;
    contribute_items(host, &vault, &out);
    Ok(out)
}

fn op_item_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let item = req(&input, "item")?;
    op_get(host, &format!("/v1/vaults/{vault}/items/{item}"))
}

fn op_item_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let body = input
        .get("item")
        .ok_or_else(|| "item is required".to_string())?;
    op_send(host, "POST", &format!("/v1/vaults/{vault}/items"), body)
}

fn op_item_replace(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let item = req(&input, "item")?;
    let body = input
        .get("body")
        .ok_or_else(|| "body is required".to_string())?;
    op_send(
        host,
        "PUT",
        &format!("/v1/vaults/{vault}/items/{item}"),
        body,
    )
}

fn op_item_patch(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let item = req(&input, "item")?;
    let body = input
        .get("patch")
        .ok_or_else(|| "patch is required".to_string())?;
    op_send(
        host,
        "PATCH",
        &format!("/v1/vaults/{vault}/items/{item}"),
        body,
    )
}

fn op_item_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let item = req(&input, "item")?;
    op_delete_json(host, &format!("/v1/vaults/{vault}/items/{item}"))
}

fn op_file_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let item = req(&input, "item")?;
    let out = op_get(host, &format!("/v1/vaults/{vault}/items/{item}/files"))?;
    contribute_files(host, &vault, &item, &out);
    Ok(out)
}

fn op_file_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let item = req(&input, "item")?;
    let file = req(&input, "file")?;
    op_get(
        host,
        &format!("/v1/vaults/{vault}/items/{item}/files/{file}"),
    )
}

fn op_file_content(input: Value, host: &mut Host) -> Result<Value, String> {
    let vault = req(&input, "vault")?;
    let item = req(&input, "item")?;
    let file = req(&input, "file")?;
    let resp = host.http_bytes_ref(
        ENDPOINT,
        "GET",
        &format!("/v1/vaults/{vault}/items/{item}/files/{file}/content"),
        Some(AUTH_TOKEN),
        &[],
        None,
        true,
    )?;
    if !(200..300).contains(&resp.status) {
        return Err(format!(
            "GET {ENDPOINT} file content -> status {}",
            resp.status
        ));
    }
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(&file);
    let blob_ref = host.blob_put(name, &resp.bytes)?;
    Ok(json!({
        "blob_ref": blob_ref,
        "name": name,
        "size": resp.bytes.len(),
        "vault": vault,
        "item": item,
        "file": file
    }))
}

fn array_items(v: &Value) -> Vec<&Value> {
    if let Some(arr) = v.as_array() {
        return arr.iter().collect();
    }
    v.get("items")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().collect())
        .unwrap_or_default()
}

fn contribute_vaults(host: &mut Host, out: &Value) {
    let records: Vec<Record> = array_items(out)
        .into_iter()
        .filter_map(|v| {
            let id = v
                .get("id")
                .or_else(|| v.get("uuid"))
                .and_then(Value::as_str)?;
            let name = v.get("name").and_then(Value::as_str).unwrap_or(id);
            let mut rec = Record::new(
                Source::new("onepassword"),
                "onepassword.vault",
                id,
                name,
                format!("1Password vault {name}"),
            );
            rec.meta = json!({ "id": id, "name": name });
            Some(rec)
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_items(host: &mut Host, vault: &str, out: &Value) {
    let records: Vec<Record> = array_items(out)
        .into_iter()
        .filter_map(|v| {
            let id = v
                .get("id")
                .or_else(|| v.get("uuid"))
                .and_then(Value::as_str)?;
            let title = v.get("title").and_then(Value::as_str).unwrap_or(id);
            let category = v.get("category").and_then(Value::as_str).unwrap_or("");
            let mut rec = Record::new(
                Source::new("onepassword"),
                "onepassword.item",
                format!("{vault}/{id}"),
                title,
                format!("1Password item {title} {category}"),
            );
            rec.meta = json!({
                "vault": vault,
                "id": id,
                "title": title,
                "category": category,
                "concealed_values_indexed": false
            });
            Some(rec)
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_files(host: &mut Host, vault: &str, item: &str, out: &Value) {
    let records: Vec<Record> = array_items(out)
        .into_iter()
        .filter_map(|v| {
            let id = v
                .get("id")
                .or_else(|| v.get("uuid"))
                .and_then(Value::as_str)?;
            let name = v.get("name").and_then(Value::as_str).unwrap_or(id);
            let mut rec = Record::new(
                Source::new("onepassword"),
                "onepassword.file",
                format!("{vault}/{item}/{id}"),
                name,
                format!("1Password file {name}"),
            );
            rec.meta = json!({
                "vault": vault,
                "item": item,
                "id": id,
                "name": name,
                "file_bytes_indexed": false
            });
            Some(rec)
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_connect_groups_and_blob() {
        let m = manifest_builder().manifest();
        assert!(m.groups.iter().any(|g| g.name == GROUP_ITEMS));
        assert!(m.capabilities.blob);
        assert!(m.capabilities.private_hosts.contains(&"*".to_string()));
        let op = m
            .operations
            .iter()
            .find(|op| op.name == "onepassword.file.content")
            .unwrap();
        assert_eq!(op.group.as_deref(), Some(GROUP_FILES));
    }

    #[test]
    fn item_list_contributes_metadata_without_fields() {
        let mut mock = MockHost::default()
            .with_endpoint_ref(ENDPOINT, "http://connect.test")
            .with_http(
                "/v1/vaults/v1/items",
                json!([{"id":"i1","title":"Database","category":"LOGIN","fields":[{"value":"secret"}]}]),
            );
        let mut host = Host::new(&mut mock);
        let _ = op_item_list(json!({"vault":"v1"}), &mut host).unwrap();
        let recs = mock.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].id, "v1/i1");
        assert_eq!(recs[0].meta["concealed_values_indexed"], false);
        assert!(recs[0].meta.get("fields").is_none());
    }

    #[test]
    fn item_show_returns_explicit_item_payload_but_does_not_contribute() {
        let mut mock = MockHost::default()
            .with_endpoint_ref(ENDPOINT, "http://connect.test")
            .with_http(
                "/v1/vaults/v1/items/i1",
                json!({"id":"i1","fields":[{"value":"secret"}]}),
            );
        let mut host = Host::new(&mut mock);
        let out = op_item_show(json!({"vault":"v1","item":"i1"}), &mut host).unwrap();
        assert_eq!(out["fields"][0]["value"], "secret");
        assert!(mock.contributed.borrow().is_empty());
    }

    #[test]
    fn file_content_returns_blob_ref() {
        let mut mock = MockHost::default()
            .with_endpoint_ref(ENDPOINT, "http://connect.test")
            .with_http_bytes(
                "/v1/vaults/v1/items/i1/files/f1/content",
                b"binary data".to_vec(),
            );
        let mut host = Host::new(&mut mock);
        let out = op_file_content(
            json!({"vault":"v1","item":"i1","file":"f1","name":"secret.bin"}),
            &mut host,
        )
        .unwrap();
        let blob_ref = out["blob_ref"].as_str().unwrap();
        let blob = mock.blobs.borrow().get(blob_ref).unwrap().1.clone();
        assert_eq!(blob, b"binary data");
    }
}
