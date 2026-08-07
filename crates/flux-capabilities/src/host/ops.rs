//! The agent-facing host ops over the session [`HostRegistry`] (Decision 0018 / C-649):
//! `host.list` / `host.info` / `host.probe`.
//!
//! `list` and `info` are read-only views of the registered bindings; `probe` performs the
//! backend's side-effect-free identity check through the injected [`HostProber`]. Everything the
//! agent sees is a weak reference — backend kind, bare address, labels and a credential
//! *presence* marker, never a value. The pack registers at
//! [`OperationPlacement::LocalControlPlane`]: host bindings are session substrate state the local
//! coordinator owns, and they must stay operable precisely when a non-native substrate is
//! selected (hiding them there would make the selected binding uninspectable).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{
    AuthorityRequirement, OperationPlacement, Tool, ToolContext, ToolRegistry, ToolResult,
};
use flux_secret::host::HostRecord;
use flux_spec::{AccessKind, ToolSpec};

use super::{static_availability, HostProber, HostRegistry};

/// The group all three host ops belong to (surfaced by the session-ambient `host` signal the CLI
/// injects when bindings are declared). Shared so the op specs and the group manifest can't drift.
pub const HOST_GROUP: &str = "host";

/// The three host ops over `hosts` + `prober`, as a tool vec (the form a surface registers into
/// an agent/app registry).
pub fn host_tools(hosts: Arc<HostRegistry>, prober: Arc<dyn HostProber>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListOp(hosts.clone())) as Arc<dyn Tool>,
        Arc::new(InfoOp(hosts.clone())),
        Arc::new(ProbeOp(hosts, prober)),
    ]
}

/// Register all three host ops into `registry`.
pub fn register_host_ops(
    registry: &mut ToolRegistry,
    hosts: Arc<HostRegistry>,
    prober: Arc<dyn HostProber>,
) {
    try_register_host_ops(registry, hosts, prober)
        .expect("flux host operation pack registration failed");
}

/// Fallibly register host operations with an auditable source label. `LocalControlPlane`
/// placement is deliberate (see the module docs): the ops describe and verify substrate bindings;
/// they never execute effects on one.
pub fn try_register_host_ops(
    registry: &mut ToolRegistry,
    hosts: Arc<HostRegistry>,
    prober: Arc<dyn HostProber>,
) -> Result<()> {
    registry.try_register_all_from_with_placement(
        "flux-capabilities host pack",
        host_tools(hosts, prober),
        OperationPlacement::LocalControlPlane,
    )
}

/// A required, non-empty string field.
fn req_str(op: &str, params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Other(format!("{op}: `{key}` (non-empty string) required")))
}

fn host_subject(params: &Value) -> String {
    params
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("host-registry")
        .to_string()
}

/// `id backend address availability owner [credential: host-injected] {labels}` — a one-line
/// weak-ref summary. The credential renders as a *presence* marker only.
fn render_record(r: &HostRecord) -> String {
    let host = &r.host;
    let mut out = format!(
        "{} [{}] {} {}",
        host.id,
        host.backend,
        host.display_address(),
        static_availability(host.backend)
    );
    out.push_str(&format!(" owner={}", r.owner));
    if host.credential_ref.is_some() {
        // The *presence* of a credential location is useful context — never the value.
        out.push_str(" [credential: host-injected]");
    }
    if !host.labels.is_empty() {
        let labels: Vec<String> = host
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        out.push_str(&format!(" {{{}}}", labels.join(", ")));
    }
    out
}

/// `host.list` — every binding registered in this session.
struct ListOp(Arc<HostRegistry>);

#[async_trait]
impl Tool for ListOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "host.list",
            "List the named execution-substrate bindings registered in this session ([[host]] \
             config plus the hosts store), with backend kind, address and static availability. \
             Weak references only — a credential shows as a presence marker, never a value.",
            json!({"type": "object", "properties": {}}),
        )
        .with_access(vec![AccessKind::LocalSystem])
        .with_group(HOST_GROUP)
    }

    fn authority_requirements(
        &self,
        _params: &Value,
        _subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        Ok(vec![AuthorityRequirement::host_read("host-registry")])
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let records = self.0.list();
        if records.is_empty() {
            return Ok(ToolResult::ok(
                "no host bindings declared — add a [[host]] entry or run `flux host add`",
            ));
        }
        Ok(ToolResult::ok(
            records
                .iter()
                .map(render_record)
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }
}

/// `host.info` — one binding by name.
struct InfoOp(Arc<HostRegistry>);

#[async_trait]
impl Tool for InfoOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "host.info",
            "Show one named host binding in full: backend kind, address, availability, labels and \
             the credential *presence* (never a value).",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Binding name (e.g. \"build-farm\")"}
                },
                "required": ["id"]
            }),
        )
        .with_access(vec![AccessKind::LocalSystem])
        .with_group(HOST_GROUP)
    }

    fn authority_requirements(
        &self,
        params: &Value,
        _subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        Ok(vec![AuthorityRequirement::host_read(host_subject(params))])
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let id = req_str("host.info", &params, "id")?;
        match self.0.get(&id) {
            Some(r) => Ok(ToolResult::ok(render_record(&r))),
            None => {
                let known = self.0.known_names();
                Ok(ToolResult::ok(if known.is_empty() {
                    format!("no host binding `{id}` (none declared)")
                } else {
                    format!("no host binding `{id}`; known: {}", known.join(", "))
                }))
            }
        }
    }
}

/// `host.probe` — the backend's side-effect-free identity check for one binding.
struct ProbeOp(Arc<HostRegistry>, Arc<dyn HostProber>);

#[async_trait]
impl Tool for ProbeOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "host.probe",
            "Verify one named host binding by its backend's side-effect-free identity check: the \
             resolved substrate identity (kind, workspace, confinement, remotely_reported) and, \
             for a remote backend, the negotiated protocol version. Executes nothing on the \
             substrate.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Binding name to probe"}
                },
                "required": ["id"]
            }),
        )
        .with_access(vec![AccessKind::Network, AccessKind::LocalSystem])
        .with_group(HOST_GROUP)
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn authority_requirements(
        &self,
        params: &Value,
        _subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        let subject = host_subject(params);
        Ok(vec![
            AuthorityRequirement::network_fetch(subject.clone()),
            AuthorityRequirement::host_read(subject),
        ])
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let id = req_str("host.probe", &params, "id")?;
        let Some(record) = self.0.get(&id) else {
            let known = self.0.known_names();
            return Ok(ToolResult::ok(if known.is_empty() {
                format!("no host binding `{id}` (none declared)")
            } else {
                format!("no host binding `{id}`; known: {}", known.join(", "))
            }));
        };
        match self.1.probe(&record.host).await {
            Ok(report) => {
                let mut line = format!(
                    "{id}: kind={} workspace={} confinement={} remotely_reported={}",
                    report.kind, report.workspace, report.confinement, report.remotely_reported
                );
                if let Some(version) = report.protocol_version {
                    line.push_str(&format!(" protocol=v{version}"));
                }
                Ok(ToolResult::ok(line))
            }
            Err(failure) => Ok(ToolResult::ok(format!("{id}: probe failed — {failure}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{HostProbeFailure, HostProbeReport};
    use flux_secret::host::{HostBackend, HostRef};
    use flux_secret::Ref;
    use flux_system::{System, Workspace};

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!("flux-host-ops-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    struct StaticProber(std::result::Result<HostProbeReport, HostProbeFailure>);

    #[async_trait]
    impl HostProber for StaticProber {
        async fn probe(
            &self,
            _host: &HostRef,
        ) -> std::result::Result<HostProbeReport, HostProbeFailure> {
            self.0.clone()
        }
    }

    fn registry() -> Arc<HostRegistry> {
        let reg = HostRegistry::new();
        reg.put(HostRecord::config(HostRef {
            url: Some("https://farm.example:8443".into()),
            credential_ref: Some(Ref::env("FARM_TOKEN")),
            ..HostRef::declared("build-farm", HostBackend::Remote)
        }));
        Arc::new(reg)
    }

    fn ok_report() -> HostProbeReport {
        HostProbeReport {
            kind: "remote".into(),
            workspace: "/srv/work".into(),
            confinement: "bubblewrap".into(),
            remotely_reported: true,
            protocol_version: Some(2),
        }
    }

    #[tokio::test]
    async fn list_and_info_render_weak_refs_and_never_a_secret() {
        std::env::set_var("FARM_TOKEN", "sk-super-secret");
        let hosts = registry();
        let prober = Arc::new(StaticProber(Ok(ok_report())));
        let tools = host_tools(hosts, prober);
        let ctx = ctx();
        for (tool, params) in [
            (&tools[0], json!({})),
            (&tools[1], json!({"id": "build-farm"})),
        ] {
            let out = tool.execute(&ctx, params).await.unwrap();
            let text = out.content;
            assert!(text.contains("build-farm"), "{text}");
            assert!(text.contains("[remote]"), "backend kind rendered: {text}");
            assert!(text.contains("https://farm.example:8443"), "{text}");
            assert!(text.contains("host-injected"), "presence marker: {text}");
            assert!(!text.contains("sk-super-secret"), "never a value: {text}");
            assert!(
                !text.contains("FARM_TOKEN"),
                "ops render presence, not location: {text}"
            );
        }
        std::env::remove_var("FARM_TOKEN");
    }

    #[tokio::test]
    async fn probe_reports_identity_and_typed_failures() {
        let hosts = registry();
        let ok = host_tools(hosts.clone(), Arc::new(StaticProber(Ok(ok_report()))));
        let ctx = ctx();
        let out = ok[2]
            .execute(&ctx, json!({"id": "build-farm"}))
            .await
            .unwrap();
        assert!(
            out.content.contains("kind=remote")
                && out.content.contains("protocol=v2")
                && out.content.contains("remotely_reported=true"),
            "{}",
            out.content
        );

        let failing = host_tools(
            hosts,
            Arc::new(StaticProber(Err(HostProbeFailure::BackendUnwired {
                backend: "container".into(),
            }))),
        );
        let out = failing[2]
            .execute(&ctx, json!({"id": "build-farm"}))
            .await
            .unwrap();
        assert!(out.content.contains("probe failed"), "{}", out.content);
        assert!(out.content.contains("container"), "{}", out.content);

        // An unknown binding names the known ones instead of failing stringly.
        let out = failing[2]
            .execute(&ctx, json!({"id": "gone"}))
            .await
            .unwrap();
        assert!(out.content.contains("known: build-farm"), "{}", out.content);
    }

    #[test]
    fn ops_declare_the_host_group() {
        let hosts = Arc::new(HostRegistry::new());
        let prober = Arc::new(StaticProber(Ok(ok_report())));
        for tool in host_tools(hosts, prober) {
            assert_eq!(tool.spec().group.as_deref(), Some(HOST_GROUP));
        }
    }
}
