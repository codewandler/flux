//! `vault` — HashiCorp Vault integration over host-managed HTTP.
//!
//! The plugin addresses `vault.endpoint` by reference, injects `VAULT_TOKEN` as `X-Vault-Token`
//! host-side, and optionally forwards the non-secret `VAULT_NAMESPACE` config as `X-Vault-Namespace`.
//! Admin ops are read-only diagnostics; KV-v2 ops are grouped separately under `vault.kv`.

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

const ENDPOINT: &str = "vault.endpoint";
const AUTH_TOKEN: &str = "token";
const GROUP_ADMIN: &str = "vault.admin";
const GROUP_KV: &str = "vault.kv";

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct EmptyInput {}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PolicyReadInput {
    name: String,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct KvListInput {
    mount: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct KvPathInput {
    mount: Option<String>,
    path: String,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct KvReadInput {
    mount: Option<String>,
    path: String,
    version: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct KvWriteInput {
    mount: Option<String>,
    path: String,
    data: Value,
    cas: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct KvMetadataUpdateInput {
    mount: Option<String>,
    path: String,
    max_versions: Option<u64>,
    cas_required: Option<bool>,
    delete_version_after: Option<String>,
    custom_metadata: Option<Value>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct KvVersionsInput {
    mount: Option<String>,
    path: String,
    versions: Vec<u64>,
}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("vault", env!("CARGO_PKG_VERSION"))
        .capabilities(Caps {
            http: true,
            secrets: vec!["VAULT_TOKEN".into()],
            private_hosts: vec!["*".into()],
            ..Default::default()
        })
        .auth(AuthMethod::header(
            AUTH_TOKEN,
            "X-Vault-Token",
            vec!["VAULT_TOKEN".into()],
        ))
        .config(ConfigSpec {
            name: "namespace".into(),
            env: vec!["VAULT_NAMESPACE".into()],
            description: "Optional Vault Enterprise namespace.".into(),
        })
        .endpoint(EndpointSpec {
            name: ENDPOINT.into(),
            env: vec!["VAULT_ADDR".into()],
            description: "Vault base URL, for example https://vault.example.com.".into(),
            ..Default::default()
        })
        .datasource(ds(
            "vault.kv",
            "vault.kv_key",
            "Vault KV-v2 key names and metadata; secret values are never indexed.",
        ))
        .group(op_group(
            GROUP_ADMIN,
            "Vault read-only admin diagnostics.",
            &[
                "vault.health",
                "vault.auth.list",
                "vault.mount.list",
                "vault.policy.list",
                "vault.policy.read",
                "vault.token.lookup_self",
            ],
        ))
        .group(op_group(
            GROUP_KV,
            "Vault KV-v2 read/write operations.",
            &[
                "vault.kv.list",
                "vault.kv.read",
                "vault.kv.write",
                "vault.kv.patch",
                "vault.kv.metadata",
                "vault.kv.metadata.update",
                "vault.kv.delete_latest",
                "vault.kv.delete_versions",
                "vault.kv.undelete_versions",
                "vault.kv.destroy_versions",
                "vault.kv.metadata_delete",
            ],
        ))
        .operation(
            grouped(
                read_op_typed::<EmptyInput>("vault.health", "Read Vault sys/health status."),
                GROUP_ADMIN,
            ),
            op_health,
        )
        .operation(
            grouped(
                read_op_typed::<EmptyInput>("vault.auth.list", "List enabled Vault auth methods."),
                GROUP_ADMIN,
            ),
            op_auth_list,
        )
        .operation(
            grouped(
                read_op_typed::<EmptyInput>(
                    "vault.mount.list",
                    "List mounted Vault secrets engines.",
                ),
                GROUP_ADMIN,
            ),
            op_mount_list,
        )
        .operation(
            grouped(
                read_op_typed::<EmptyInput>("vault.policy.list", "List Vault ACL policies."),
                GROUP_ADMIN,
            ),
            op_policy_list,
        )
        .operation(
            grouped(
                read_op_typed::<PolicyReadInput>("vault.policy.read", "Read one Vault ACL policy."),
                GROUP_ADMIN,
            ),
            op_policy_read,
        )
        .operation(
            grouped(
                read_op_typed::<EmptyInput>(
                    "vault.token.lookup_self",
                    "Lookup metadata for the current Vault token.",
                ),
                GROUP_ADMIN,
            ),
            op_token_lookup_self,
        )
        .operation(
            grouped(
                read_op_typed::<KvListInput>(
                    "vault.kv.list",
                    "List Vault KV-v2 keys under a path.",
                ),
                GROUP_KV,
            ),
            op_kv_list,
        )
        .operation(
            grouped(
                read_op_typed::<KvReadInput>(
                    "vault.kv.read",
                    "Read one Vault KV-v2 secret version explicitly.",
                ),
                GROUP_KV,
            ),
            op_kv_read,
        )
        .operation(
            grouped(
                write_op_typed::<KvWriteInput>("vault.kv.write", "Write a Vault KV-v2 secret."),
                GROUP_KV,
            ),
            op_kv_write,
        )
        .operation(
            grouped(
                write_op_typed::<KvWriteInput>(
                    "vault.kv.patch",
                    "Patch a Vault KV-v2 secret using JSON merge-patch semantics.",
                ),
                GROUP_KV,
            ),
            op_kv_patch,
        )
        .operation(
            grouped(
                read_op_typed::<KvPathInput>(
                    "vault.kv.metadata",
                    "Read metadata for one Vault KV-v2 key.",
                ),
                GROUP_KV,
            ),
            op_kv_metadata,
        )
        .operation(
            grouped(
                write_op_typed::<KvMetadataUpdateInput>(
                    "vault.kv.metadata.update",
                    "Update metadata for one Vault KV-v2 key.",
                ),
                GROUP_KV,
            ),
            op_kv_metadata_update,
        )
        .operation(
            grouped(
                write_op_typed::<KvPathInput>(
                    "vault.kv.delete_latest",
                    "Delete the latest version of a Vault KV-v2 key.",
                ),
                GROUP_KV,
            ),
            op_kv_delete_latest,
        )
        .operation(
            grouped(
                write_op_typed::<KvVersionsInput>(
                    "vault.kv.delete_versions",
                    "Soft-delete specific Vault KV-v2 versions.",
                ),
                GROUP_KV,
            ),
            op_kv_delete_versions,
        )
        .operation(
            grouped(
                write_op_typed::<KvVersionsInput>(
                    "vault.kv.undelete_versions",
                    "Undelete specific Vault KV-v2 versions.",
                ),
                GROUP_KV,
            ),
            op_kv_undelete_versions,
        )
        .operation(
            grouped(
                risked(
                    write_op_typed::<KvVersionsInput>(
                        "vault.kv.destroy_versions",
                        "Permanently destroy specific Vault KV-v2 versions.",
                    ),
                    Risk::High,
                ),
                GROUP_KV,
            ),
            op_kv_destroy_versions,
        )
        .operation(
            grouped(
                risked(
                    write_op_typed::<KvPathInput>(
                        "vault.kv.metadata_delete",
                        "Delete all Vault KV-v2 versions and metadata for a key.",
                    ),
                    Risk::High,
                ),
                GROUP_KV,
            ),
            op_kv_metadata_delete,
        )
}

fn main() {
    manifest_builder().serve();
}

fn mount(input: &Value) -> String {
    input
        .get("mount")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("secret")
        .trim_matches('/')
        .to_string()
}

fn path(input: &Value) -> String {
    input
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_matches('/')
        .to_string()
}

fn required_path(input: &Value) -> Result<String, String> {
    let p = path(input);
    if p.is_empty() {
        Err("path is required".into())
    } else {
        Ok(p)
    }
}

fn kv_path(input: &Value, family: &str) -> Result<String, String> {
    let p = required_path(input)?;
    Ok(format!("/v1/{}/{}/{}", mount(input), family, p))
}

fn namespace_headers(host: &mut Host) -> Vec<(String, String)> {
    host.config("namespace")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|ns| vec![("X-Vault-Namespace".into(), ns)])
        .unwrap_or_default()
}

fn vault_http(
    host: &mut Host,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: Option<&Value>,
    content_type: &str,
) -> Result<Value, String> {
    let ns = namespace_headers(host);
    let mut headers: Vec<(&str, &str)> = ns.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    if body.is_some() {
        headers.push(("content-type", content_type));
    }
    let encoded;
    let body_bytes = if let Some(v) = body {
        encoded = serde_json::to_vec(v).map_err(|e| e.to_string())?;
        Some(encoded.as_slice())
    } else {
        None
    };
    let resp = host.http_ref(ENDPOINT, method, path, auth, &headers, body_bytes)?;
    if !resp.is_success() {
        return Err(format!(
            "{method} {ENDPOINT} {path} -> {} {}",
            resp.status, resp.body
        ));
    }
    if resp.body.trim().is_empty() {
        return Ok(json!({ "ok": true }));
    }
    resp.json()
}

fn admin_get(host: &mut Host, path: &str) -> Result<Value, String> {
    vault_http(
        host,
        "GET",
        path,
        Some(AUTH_TOKEN),
        None,
        "application/json",
    )
}

fn op_health(_: Value, host: &mut Host) -> Result<Value, String> {
    vault_http(
        host,
        "GET",
        "/v1/sys/health",
        None,
        None,
        "application/json",
    )
}

fn op_auth_list(_: Value, host: &mut Host) -> Result<Value, String> {
    admin_get(host, "/v1/sys/auth")
}

fn op_mount_list(_: Value, host: &mut Host) -> Result<Value, String> {
    admin_get(host, "/v1/sys/mounts")
}

fn op_policy_list(_: Value, host: &mut Host) -> Result<Value, String> {
    admin_get(host, "/v1/sys/policy")
}

fn op_policy_read(input: Value, host: &mut Host) -> Result<Value, String> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "name is required".to_string())?;
    admin_get(host, &format!("/v1/sys/policy/{name}"))
}

fn op_token_lookup_self(_: Value, host: &mut Host) -> Result<Value, String> {
    admin_get(host, "/v1/auth/token/lookup-self")
}

fn op_kv_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let p = path(&input);
    let api_path = if p.is_empty() {
        format!("/v1/{}/metadata", mount(&input))
    } else {
        format!("/v1/{}/metadata/{}", mount(&input), p)
    };
    let out = vault_http(
        host,
        "LIST",
        &api_path,
        Some(AUTH_TOKEN),
        None,
        "application/json",
    )?;
    contribute_kv_keys(host, &mount(&input), &p, &out);
    Ok(out)
}

fn op_kv_read(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut api_path = kv_path(&input, "data")?;
    if let Some(version) = input.get("version").and_then(Value::as_u64) {
        api_path.push_str(&format!("?version={version}"));
    }
    vault_http(
        host,
        "GET",
        &api_path,
        Some(AUTH_TOKEN),
        None,
        "application/json",
    )
}

fn op_kv_write(input: Value, host: &mut Host) -> Result<Value, String> {
    let data = input
        .get("data")
        .cloned()
        .ok_or_else(|| "data is required".to_string())?;
    let mut payload = json!({ "data": data });
    if let Some(cas) = input.get("cas").and_then(Value::as_i64) {
        payload["options"] = json!({ "cas": cas });
    }
    vault_http(
        host,
        "POST",
        &kv_path(&input, "data")?,
        Some(AUTH_TOKEN),
        Some(&payload),
        "application/json",
    )
}

fn op_kv_patch(input: Value, host: &mut Host) -> Result<Value, String> {
    let data = input
        .get("data")
        .cloned()
        .ok_or_else(|| "data is required".to_string())?;
    let mut payload = json!({ "data": data });
    if let Some(cas) = input.get("cas").and_then(Value::as_i64) {
        payload["options"] = json!({ "cas": cas });
    }
    vault_http(
        host,
        "PATCH",
        &kv_path(&input, "data")?,
        Some(AUTH_TOKEN),
        Some(&payload),
        "application/merge-patch+json",
    )
}

fn op_kv_metadata(input: Value, host: &mut Host) -> Result<Value, String> {
    let out = vault_http(
        host,
        "GET",
        &kv_path(&input, "metadata")?,
        Some(AUTH_TOKEN),
        None,
        "application/json",
    )?;
    contribute_kv_metadata(host, &mount(&input), &required_path(&input)?, &out);
    Ok(out)
}

fn op_kv_metadata_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut payload = serde_json::Map::new();
    for key in [
        "max_versions",
        "cas_required",
        "delete_version_after",
        "custom_metadata",
    ] {
        if let Some(v) = input.get(key) {
            payload.insert(key.into(), v.clone());
        }
    }
    vault_http(
        host,
        "POST",
        &kv_path(&input, "metadata")?,
        Some(AUTH_TOKEN),
        Some(&Value::Object(payload)),
        "application/json",
    )
}

fn op_kv_delete_latest(input: Value, host: &mut Host) -> Result<Value, String> {
    vault_http(
        host,
        "DELETE",
        &kv_path(&input, "data")?,
        Some(AUTH_TOKEN),
        None,
        "application/json",
    )
}

fn versions_payload(input: &Value) -> Result<Value, String> {
    let versions = input
        .get("versions")
        .and_then(Value::as_array)
        .ok_or_else(|| "versions is required".to_string())?;
    if versions.is_empty() {
        return Err("versions must not be empty".into());
    }
    Ok(json!({ "versions": versions }))
}

fn op_kv_delete_versions(input: Value, host: &mut Host) -> Result<Value, String> {
    vault_http(
        host,
        "POST",
        &kv_path(&input, "delete")?,
        Some(AUTH_TOKEN),
        Some(&versions_payload(&input)?),
        "application/json",
    )
}

fn op_kv_undelete_versions(input: Value, host: &mut Host) -> Result<Value, String> {
    vault_http(
        host,
        "POST",
        &kv_path(&input, "undelete")?,
        Some(AUTH_TOKEN),
        Some(&versions_payload(&input)?),
        "application/json",
    )
}

fn op_kv_destroy_versions(input: Value, host: &mut Host) -> Result<Value, String> {
    vault_http(
        host,
        "PUT",
        &kv_path(&input, "destroy")?,
        Some(AUTH_TOKEN),
        Some(&versions_payload(&input)?),
        "application/json",
    )
}

fn op_kv_metadata_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    vault_http(
        host,
        "DELETE",
        &kv_path(&input, "metadata")?,
        Some(AUTH_TOKEN),
        None,
        "application/json",
    )
}

fn contribute_kv_keys(host: &mut Host, mount: &str, prefix: &str, out: &Value) {
    let Some(keys) = out
        .get("data")
        .and_then(|d| d.get("keys"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let records: Vec<Record> = keys
        .iter()
        .filter_map(Value::as_str)
        .map(|key| {
            let full = if prefix.is_empty() {
                key.to_string()
            } else {
                format!("{}/{}", prefix.trim_matches('/'), key)
            };
            let mut rec = Record::new(
                Source::new("vault"),
                "vault.kv_key",
                format!("{mount}/{full}"),
                format!("{mount}/{full}"),
                format!("Vault KV key {mount}/{full}"),
            );
            rec.meta = json!({ "mount": mount, "path": full, "secret_values_indexed": false });
            rec
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_kv_metadata(host: &mut Host, mount: &str, path: &str, out: &Value) {
    let metadata = out.get("data").cloned().unwrap_or(Value::Null);
    let mut rec = Record::new(
        Source::new("vault"),
        "vault.kv_key",
        format!("{mount}/{path}"),
        format!("{mount}/{path}"),
        format!("Vault KV metadata for {mount}/{path}"),
    );
    rec.meta = json!({ "mount": mount, "path": path, "metadata": metadata, "secret_values_indexed": false });
    let _ = host.contribute(&[rec]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_groups_and_secret_header_auth() {
        let m = manifest_builder().manifest();
        assert!(m.groups.iter().any(|g| g.name == GROUP_KV));
        let kv_read = m
            .operations
            .iter()
            .find(|op| op.name == "vault.kv.read")
            .unwrap();
        assert_eq!(kv_read.group.as_deref(), Some(GROUP_KV));
        assert!(m.capabilities.private_hosts.contains(&"*".to_string()));
        assert_eq!(m.auth[0].purpose, AUTH_TOKEN);
    }

    #[test]
    fn kv_list_contributes_key_metadata_only() {
        let mut mock = MockHost::default()
            .with_endpoint_ref(ENDPOINT, "https://vault.test")
            .with_http(
                "/v1/secret/metadata/apps",
                json!({"data":{"keys":["db","api/"]}}),
            );
        let mut host = Host::new(&mut mock);
        let out = op_kv_list(json!({"mount":"secret","path":"apps"}), &mut host).unwrap();
        assert_eq!(out["data"]["keys"][0], "db");
        let recs = mock.contributed.borrow();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].id, "secret/apps/db");
        assert_eq!(recs[0].meta["secret_values_indexed"], false);
    }

    #[test]
    fn kv_read_does_not_contribute_secret_values() {
        let mut mock = MockHost::default()
            .with_endpoint_ref(ENDPOINT, "https://vault.test")
            .with_http(
                "/v1/secret/data/apps/db",
                json!({"data":{"data":{"password":"s3cr3t"},"metadata":{"version":1}}}),
            );
        let mut host = Host::new(&mut mock);
        let out = op_kv_read(json!({"mount":"secret","path":"apps/db"}), &mut host).unwrap();
        assert_eq!(out["data"]["data"]["password"], "s3cr3t");
        assert!(mock.contributed.borrow().is_empty());
    }

    #[test]
    fn namespace_header_is_sent_when_configured() {
        let mut mock = MockHost::default()
            .with_endpoint_ref(ENDPOINT, "https://vault.test")
            .with_config("namespace", "admin/team-a")
            .with_http("/v1/sys/health", json!({"initialized":true}));
        let mut host = Host::new(&mut mock);
        let _ = op_health(json!({}), &mut host).unwrap();
        let calls = mock.calls.borrow();
        let headers = &calls[1].1["headers"];
        assert_eq!(headers["X-Vault-Namespace"], "admin/team-a");
    }
}
