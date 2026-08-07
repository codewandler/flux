//! The agent-facing host ops over the session [`HostRegistry`] (Decision 0018 / C-649, C-654):
//! `host.list` / `host.info` / `host.probe` / `host.metrics`.
//!
//! `list` and `info` are read-only views of the registered bindings; `probe` performs the
//! backend's side-effect-free identity check and `metrics` its bounded self-measurement, both
//! through the injected [`HostProber`]. Everything the agent sees of a *binding* is a weak
//! reference — backend kind, bare address, labels and a credential *presence* marker, never a
//! value; what it sees of a *substrate* is typed readings, where a metric that substrate cannot
//! measure is explicitly unavailable rather than zero. The pack registers at
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

use super::{render_metric_answer, static_availability, HostMetrics, HostProber, HostRegistry};

/// The group every host op belongs to (surfaced by the session-ambient `host` signal the CLI
/// injects when bindings are declared). Shared so the op specs and the group manifest can't drift.
pub const HOST_GROUP: &str = "host";

/// The host ops over `hosts` + `prober`, as a tool vec (the form a surface registers into an
/// agent/app registry).
pub fn host_tools(hosts: Arc<HostRegistry>, prober: Arc<dyn HostProber>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ListOp(hosts.clone())) as Arc<dyn Tool>,
        Arc::new(InfoOp(hosts.clone())),
        Arc::new(ProbeOp(hosts.clone(), prober.clone())),
        Arc::new(MetricsOp(hosts, prober)),
    ]
}

/// Register every host op into `registry`.
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

/// `host.metrics` — the binding's bounded read of its **own** substrate (Decision 0018 rule 6).
///
/// Same placement as the rest of the pack, and for a reason worth stating: this describes a
/// *binding*, not the substrate the current turn is executing on. An agent must be able to ask how
/// the build farm is doing while its own effects are landing somewhere else entirely, which is
/// precisely what `LocalControlPlane` preserves.
struct MetricsOp(Arc<HostRegistry>, Arc<dyn HostProber>);

#[async_trait]
impl Tool for MetricsOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "host.metrics",
            "Read one named host binding's own condition: CPU, load, memory, swap, disk, uptime, \
             temperature and fans, measured by that substrate about itself. Values are typed and \
             unit-bearing; a metric the substrate cannot measure is reported as explicitly \
             unavailable with a reason, never as zero. A remote binding's readings are marked as \
             remotely reported.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Binding name to measure"}
                },
                "required": ["id"]
            }),
        )
        // Reaching a remote binding is network egress, and reading a local one touches this
        // machine — the same pair `host.probe` declares, for the same two reasons.
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
        let id = req_str("host.metrics", &params, "id")?;
        let Some(record) = self.0.get(&id) else {
            let known = self.0.known_names();
            return Ok(ToolResult::ok(if known.is_empty() {
                format!("no host binding `{id}` (none declared)")
            } else {
                format!("no host binding `{id}`; known: {}", known.join(", "))
            }));
        };
        match self.1.read_metrics(&record.host).await {
            Ok(HostMetrics::Served {
                remotely_reported,
                answers,
            }) => {
                let mut lines = vec![format!(
                    "{id}: {} reading(s){}",
                    answers.len(),
                    if remotely_reported {
                        " (remotely reported by the serving substrate)"
                    } else {
                        " (observed locally)"
                    }
                )];
                lines.extend(answers.iter().map(render_metric_answer));
                Ok(ToolResult::ok(lines.join("\n")))
            }
            // The two negatives stay apart all the way out to the model: "serves nothing" is not
            // "has no instrument", and neither is a zero.
            Ok(HostMetrics::Unserved { detail }) => Ok(ToolResult::ok(format!(
                "{id}: this substrate does not serve host metrics — {detail}"
            ))),
            Err(failure) => Ok(ToolResult::ok(format!("{id}: metrics failed — {failure}"))),
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

    /// C-654, acceptance 3: the metrics read joins the ambient-gated `host.*` group, and it carries
    /// the same deliberate `LocalControlPlane` placement as the rest of the pack — a binding has to
    /// stay measurable precisely when the turn's effects are landing on a different substrate.
    #[test]
    fn the_host_group_carries_the_metrics_read_at_the_packs_placement() {
        let hosts = Arc::new(HostRegistry::new());
        let prober = Arc::new(StaticProber(Ok(ok_report())));
        let names: Vec<String> = host_tools(hosts.clone(), prober.clone())
            .iter()
            .map(|tool| tool.spec().name)
            .collect();
        assert!(
            names.iter().any(|name| name == "host.metrics"),
            "the host pack must expose the metrics seam: {names:?}"
        );

        let mut registry = ToolRegistry::new();
        try_register_host_ops(&mut registry, hosts, prober).expect("the pack registers");
        assert_eq!(
            registry.declared_placement("host.metrics"),
            Some(OperationPlacement::LocalControlPlane),
            "the metrics read describes a *binding*, not the selected substrate"
        );
    }

    /// C-654, acceptance 2 and 3: the op renders typed readings, marks the readings a remote
    /// binding reported as such, and renders an instrument this machine lacks as explicitly
    /// unavailable — never as a zero a reader would take for a measurement.
    #[tokio::test]
    async fn the_metrics_op_renders_typed_readings_and_explicit_unavailability() {
        use flux_system::metrics::{
            MemoryUsage, MetricAnswer, MetricKind, MetricReading, MetricSnapshot, MetricUnavailable,
        };

        struct Measuring;
        #[async_trait]
        impl HostProber for Measuring {
            async fn probe(
                &self,
                _host: &HostRef,
            ) -> std::result::Result<HostProbeReport, HostProbeFailure> {
                unreachable!("this prober is only asked for metrics")
            }

            async fn read_metrics(
                &self,
                _host: &HostRef,
            ) -> std::result::Result<HostMetrics, HostProbeFailure> {
                Ok(HostMetrics::Served {
                    remotely_reported: true,
                    answers: vec![
                        MetricAnswer::Served(MetricSnapshot {
                            sampled_at: std::time::UNIX_EPOCH
                                + std::time::Duration::from_millis(1_700_000_000_000),
                            reading: MetricReading::Memory(MemoryUsage {
                                total_bytes: 16 * 1024 * 1024 * 1024,
                                available_bytes: 4 * 1024 * 1024 * 1024,
                                used_bytes: 12 * 1024 * 1024 * 1024,
                            }),
                            remotely_reported: true,
                        }),
                        MetricAnswer::unavailable_for(
                            MetricKind::FanSpeed,
                            MetricUnavailable::NoInstrument,
                        ),
                    ],
                })
            }
        }

        let tools = host_tools(registry(), Arc::new(Measuring));
        let metrics = tools
            .iter()
            .find(|tool| tool.spec().name == "host.metrics")
            .expect("the metrics op is registered");
        let text = metrics
            .execute(&ctx(), json!({"id": "build-farm"}))
            .await
            .unwrap()
            .content;

        assert!(text.contains("memory: 12.0 GiB used of 16.0 GiB"), "{text}");
        assert!(
            text.contains("remotely reported"),
            "a remote binding's readings must carry their provenance: {text}"
        );
        assert!(
            text.contains("fan: unavailable — this substrate has no such instrument"),
            "an absent instrument must say so rather than render as a measurement: {text}"
        );
        assert!(
            !text.contains("fan: 0"),
            "an absent instrument must never render as zero: {text}"
        );

        // A substrate that serves no metrics at all is the *other* negative, and stays distinct.
        let bare = host_tools(registry(), Arc::new(StaticProber(Ok(ok_report()))));
        let text = bare
            .iter()
            .find(|tool| tool.spec().name == "host.metrics")
            .unwrap()
            .execute(&ctx(), json!({"id": "build-farm"}))
            .await
            .unwrap()
            .content;
        assert!(text.contains("does not serve host metrics"), "{text}");
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
