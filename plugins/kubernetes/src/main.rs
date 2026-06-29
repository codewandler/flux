//! `kubernetes` — a flux integration plugin that drives the `kubectl` CLI through the host's
//! `process.run` capability (no HTTP, no auth — the kubeconfig is ambient to kubectl). It exposes
//! read-only cluster introspection: namespaces, pods, deployments, pod logs, and events. The pod and
//! deployment list ops contribute datasource records (`k8s.pod` / `k8s.deployment`) so the agent can
//! search the live cluster state.
//!
//! This is the reference template for the subprocess-CLI integration plugins.

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    let ns_arg = json!({
        "type": "object",
        "properties": { "namespace": {"type": "string", "description": "Kubernetes namespace"} },
        "required": ["namespace"],
    });
    PluginBuilder::new("kubernetes", "0.1.0")
        .capabilities(Caps {
            process: vec!["kubectl".into()],
            ..Default::default()
        })
        .datasource(ds("kubernetes.pods", "k8s.pod", "Kubernetes pods (per namespace)."))
        .datasource(ds(
            "kubernetes.deployments",
            "k8s.deployment",
            "Kubernetes deployments (per namespace).",
        ))
        .operation(
            read_op(
                "k8s.namespace.list",
                "List all namespaces in the cluster.",
                json!({"type": "object", "properties": {}}),
            ),
            namespace_list,
        )
        .operation(
            read_op(
                "k8s.pod.list",
                "List pods in a namespace.",
                ns_arg.clone(),
            ),
            pod_list,
        )
        .operation(
            read_op(
                "k8s.deployment.list",
                "List deployments in a namespace.",
                ns_arg.clone(),
            ),
            deployment_list,
        )
        .operation(
            read_op(
                "k8s.pod.logs",
                "Fetch the tail of a pod's logs (plain text).",
                json!({"type": "object", "properties": {
                    "namespace": {"type": "string", "description": "Kubernetes namespace"},
                    "pod": {"type": "string", "description": "pod name"},
                    "tail": {"type": "integer", "description": "number of trailing log lines (default 100)"}
                }, "required": ["namespace", "pod"]}),
            ),
            pod_logs,
        )
        .operation(
            read_op(
                "k8s.event.list",
                "List recent events in a namespace.",
                ns_arg,
            ),
            event_list,
        )
}

fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into()],
        entity_schema: None,
    }
}

/// Run `kubectl <args> -o json` through the host and parse the stdout as JSON. Errors (including the
/// captured stderr) on a non-zero exit code or unparseable output.
fn kubectl_json(host: &mut Host, args: &[&str]) -> Result<Value, String> {
    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 3);
    argv.push("kubectl");
    argv.extend_from_slice(args);
    argv.push("-o");
    argv.push("json");
    let out = host.run(&argv, 30)?;
    if out.exit_code != 0 {
        return Err(format!(
            "kubectl {} failed (exit {}): {}",
            args.join(" "),
            out.exit_code,
            out.stderr.trim()
        ));
    }
    serde_json::from_str(&out.stdout).map_err(|e| format!("kubectl output not JSON: {e}"))
}

/// Require a non-empty string argument (defensive: these values become CLI args).
fn req_nonempty<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.as_str()),
        _ => Err(format!("`{key}` (non-empty string) required")),
    }
}

fn namespace_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    kubectl_json(host, &["get", "namespaces"])
}

fn pod_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let v = kubectl_json(host, &["get", "pods", "-n", ns])?;
    contribute_pods(host, ns, &v);
    Ok(v)
}

fn deployment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let v = kubectl_json(host, &["get", "deployments", "-n", ns])?;
    contribute_deployments(host, ns, &v);
    Ok(v)
}

fn pod_logs(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let pod = req_nonempty(&input, "pod")?;
    let tail = input.get("tail").and_then(|v| v.as_i64()).unwrap_or(100);
    let tail_arg = format!("--tail={tail}");
    // Logs are plain text, not JSON — run kubectl directly.
    let out = host.run(&["kubectl", "logs", "-n", ns, pod, &tail_arg], 30)?;
    if out.exit_code != 0 {
        return Err(format!(
            "kubectl logs {pod} failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    Ok(json!({ "logs": out.stdout }))
}

fn event_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    kubectl_json(host, &["get", "events", "-n", ns])
}

/// Contribute one `k8s.pod` record per `.items[]`: id `<ns>/<name>`, title = name, body = phase + node.
fn contribute_pods(host: &mut Host, ns: &str, v: &Value) {
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return;
    };
    let records: Vec<Record> = items
        .iter()
        .filter_map(|it| {
            let name = it.pointer("/metadata/name").and_then(|x| x.as_str())?;
            let phase = it
                .pointer("/status/phase")
                .and_then(|x| x.as_str())
                .unwrap_or("Unknown");
            let node = it
                .pointer("/spec/nodeName")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            Some(Record::new(
                Source::new("kubernetes"),
                "k8s.pod",
                format!("{ns}/{name}"),
                name,
                format!("phase={phase} node={node}"),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

/// Contribute one `k8s.deployment` record per `.items[]`: id `<ns>/<name>`, title = name, body = a
/// replica summary when present.
fn contribute_deployments(host: &mut Host, ns: &str, v: &Value) {
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return;
    };
    let records: Vec<Record> = items
        .iter()
        .filter_map(|it| {
            let name = it.pointer("/metadata/name").and_then(|x| x.as_str())?;
            Some(Record::new(
                Source::new("kubernetes"),
                "k8s.deployment",
                format!("{ns}/{name}"),
                name,
                deployment_summary(it),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

/// A short "replicas ready=R desired=D" line from a deployment's status/spec (empty if absent).
fn deployment_summary(it: &Value) -> String {
    let desired = it.pointer("/spec/replicas").and_then(|x| x.as_i64());
    let ready = it.pointer("/status/readyReplicas").and_then(|x| x.as_i64());
    match (ready, desired) {
        (Some(r), Some(d)) => format!("replicas ready={r} desired={d}"),
        (None, Some(d)) => format!("replicas desired={d}"),
        (Some(r), None) => format!("replicas ready={r}"),
        (None, None) => String::new(),
    }
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pod_list_runs_kubectl_and_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get pods -n prod",
            r#"{"items":[{"metadata":{"name":"api-1"},"status":{"phase":"Running"},"spec":{"nodeName":"n1"}}]}"#,
        );
        let out = plugin
            .call("k8s.pod.list", json!({ "namespace": "prod" }), &mut host)
            .unwrap();
        assert_eq!(out["items"][0]["metadata"]["name"], "api-1");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "k8s.pod");
        assert_eq!(recs[0].id, "prod/api-1");
        assert_eq!(recs[0].title, "api-1");
        assert!(recs[0].body.contains("phase=Running"));
        assert!(recs[0].body.contains("node=n1"));
    }

    #[test]
    fn deployment_list_contributes_with_replica_summary() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get deployments -n prod",
            r#"{"items":[{"metadata":{"name":"web"},"spec":{"replicas":3},"status":{"readyReplicas":2}}]}"#,
        );
        let out = plugin
            .call(
                "k8s.deployment.list",
                json!({ "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["items"][0]["metadata"]["name"], "web");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "k8s.deployment");
        assert_eq!(recs[0].id, "prod/web");
        assert!(recs[0].body.contains("ready=2"));
        assert!(recs[0].body.contains("desired=3"));
    }

    #[test]
    fn pod_logs_returns_plain_text() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process("logs -n prod api-1", "line1\nline2\n");
        let out = plugin
            .call(
                "k8s.pod.logs",
                json!({ "namespace": "prod", "pod": "api-1" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["logs"], "line1\nline2\n");
    }

    #[test]
    fn rejects_missing_or_empty_namespace() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin.call("k8s.pod.list", json!({}), &mut host).is_err());
        assert!(plugin
            .call("k8s.pod.list", json!({ "namespace": "" }), &mut host)
            .is_err());
        // a non-string namespace is rejected too (defensive — it would become a CLI arg)
        assert!(plugin
            .call("k8s.pod.list", json!({ "namespace": 7 }), &mut host)
            .is_err());
    }

    #[test]
    fn manifest_declares_ops_and_kubectl_capability() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 5);
        assert_eq!(m.capabilities.process, vec!["kubectl".to_string()]);
        assert!(m.datasources.iter().any(|d| d.entity == "k8s.pod"));
        assert!(m.datasources.iter().any(|d| d.entity == "k8s.deployment"));
    }
}
