//! `kubernetes` — a flux integration plugin that drives the `kubectl` CLI through the host's
//! `process.run` capability (no HTTP, no auth — the kubeconfig is ambient to kubectl). It exposes
//! cluster discovery (contexts, reachability, endpoint discovery), read-only inventory
//! (namespaces, services, pods, deployments, containers, ingresses, nodes), debugging (pod logs,
//! events, exec, deployment history), long-lived port-forwards (start/stop/list, held by the host's
//! managed-process registry), and a few guarded mutations (secret read, deployment scale/restart).
//! List ops contribute datasource records (`kubernetes.<kind>`) to the single `kubernetes.inventory`
//! search datasource so the agent can search live cluster state.
//!
//! Op names/inputs mirror the fluxplane `kubernetes` plugin; semantics are re-expressed against
//! `kubectl` rather than the Go client-go API.
//!
//! This is the reference template for the subprocess-CLI integration plugins.

use std::collections::HashMap;
use std::sync::Mutex;

use host_kit::*;
use serde_json::{json, Map, Value};

/// Metadata for one managed port-forward, tracked in the module-level [`FORWARDS`] registry so
/// `kubernetes.portforward.list` can report what `start` launched. The host owns the process; this
/// only mirrors the descriptive fields (the host has no "list all procs" command).
#[derive(Clone)]
struct ForwardMeta {
    proc_id: u64,
    context: String,
    namespace: String,
    resource: String,
    address: String,
    local_port: i64,
    remote_port: i64,
}

/// Module-level registry of port-forwards this plugin started, keyed by `proc_id`. The plugin
/// process and the host's managed-process registry both persist for the session, so a forward
/// started in `portforward.start` is visible to a later `portforward.list`/`portforward.stop`.
///
/// Limitation: this only knows about forwards *this* plugin instance started — it is the plugin's
/// own view, not a query of every managed process on the host (the host exposes no list-all
/// capability). A forward `stop`ped is dropped from the registry; a forward whose process died on
/// its own is still listed but reported `alive=false` via `process_status`.
static FORWARDS: Mutex<Option<HashMap<u64, ForwardMeta>>> = Mutex::new(None);

fn forwards_lock() -> std::sync::MutexGuard<'static, Option<HashMap<u64, ForwardMeta>>> {
    let mut guard = FORWARDS.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    guard
}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("kubernetes", "0.2.0")
        .capabilities(Caps {
            process: vec!["kubectl".into()],
            ..Default::default()
        })
        .datasource(ds(
            "kubernetes.inventory",
            "kubernetes.resource",
            "Kubernetes namespaces, services, pods, deployments, containers, and ingresses.",
        ))
        // --- cluster discovery -------------------------------------------------
        .operation(
            read_op(
                "kubernetes.cluster.list",
                "List kubeconfig contexts.",
                json!({"type": "object", "properties": {}}),
            ),
            cluster_list,
        )
        .operation(
            read_op(
                "kubernetes.test",
                "Probe Kubernetes cluster reachability through kubeconfig.",
                json!({"type": "object", "properties": {"context": s_context()}}),
            ),
            cluster_test,
        )
        .operation(
            read_op(
                "kubernetes.endpoint.discover",
                "Discover product endpoints from Kubernetes services.",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace_filter(),
                    "product": {"type": "string", "description": "product to discover, e.g. prometheus or loki (substring-matched on service name)"},
                    "limit": s_limit()
                }}),
            ),
            endpoint_discover,
        )
        // --- secrets (sensitive) ----------------------------------------------
        .operation(
            op_spec(
                "kubernetes.secret.read",
                "Read one Kubernetes secret's decoded values. Sensitive: the result is secret \
                 material intended for piping into auth or secret stores, not for display.",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace(),
                    "name": {"type": "string", "description": "secret name"},
                    "keys": {"type": "array", "items": {"type": "string"}, "description": "data keys to read; empty means all keys"}
                }, "required": ["namespace", "name"]}),
                vec![Effect::Read, Effect::Network],
                Risk::High,
                Idempotency::Idempotent,
            ),
            secret_read,
        )
        // --- inventory ---------------------------------------------------------
        .operation(
            read_op(
                "kubernetes.namespace.list",
                "List Kubernetes namespaces.",
                json!({"type": "object", "properties": {"context": s_context()}}),
            ),
            namespace_list,
        )
        .operation(
            read_op(
                "kubernetes.service.list",
                "List Kubernetes services (all namespaces unless `namespace` is set).",
                inventory_list_schema(),
            ),
            service_list,
        )
        .operation(
            read_op(
                "kubernetes.service.show",
                "Show one Kubernetes service.",
                show_schema(),
            ),
            service_show,
        )
        .operation(
            read_op(
                "kubernetes.pod.list",
                "List Kubernetes pods (all namespaces unless `namespace` is set).",
                inventory_list_schema(),
            ),
            pod_list,
        )
        .operation(
            read_op(
                "kubernetes.pod.show",
                "Show one Kubernetes pod.",
                show_schema(),
            ),
            pod_show,
        )
        .operation(
            read_op(
                "kubernetes.pod.logs",
                "Read bounded logs for one Kubernetes pod (by name) or label selector.",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace(),
                    "name": {"type": "string", "description": "pod name (provide name or selector)"},
                    "selector": {"type": "string", "description": "label selector (e.g. app=web) — merges logs from matching pods"},
                    "container": {"type": "string", "description": "container name; empty uses Kubernetes default"},
                    "tail_lines": {"type": "integer", "description": "number of trailing log lines (default 100)"},
                    "limit_bytes": {"type": "integer", "description": "maximum bytes to return"},
                    "since": {"type": "string", "description": "relative duration such as 2h, or RFC3339 timestamp"},
                    "previous": {"type": "boolean", "description": "return previous terminated container logs"},
                    "timestamps": {"type": "boolean", "description": "include Kubernetes log timestamps"}
                }, "required": ["namespace"]}),
            ),
            pod_logs,
        )
        // --- port-forward (held by the host's managed-process registry) --------
        .operation(
            write_op(
                "kubernetes.portforward.start",
                "Start a managed Kubernetes port-forward for a service, pod, or deployment. The \
                 forward is held by the host as a long-lived `kubectl port-forward` process that \
                 persists across calls; list with kubernetes.portforward.list and stop with \
                 kubernetes.portforward.stop.",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace(),
                    "resource": {"type": "string", "description": "resource ref such as service/loki, pod/api-123, or deployment/api"},
                    "name": {"type": "string", "description": "resource name when `resource` is not used"},
                    "resource_type": {"type": "string", "enum": ["service", "pod", "deployment"], "description": "resource type when `name` is used"},
                    "remote_port": {"type": "integer", "description": "remote service or pod port to forward"},
                    "local_port": {"type": "integer", "description": "local port; 0 lets kubectl allocate an available port"},
                    "address": {"type": "string", "description": "local bind address (default 127.0.0.1)"}
                }, "required": ["namespace", "remote_port"]}),
            ),
            portforward_start,
        )
        .operation(
            op_spec(
                "kubernetes.portforward.stop",
                "Stop a managed Kubernetes port-forward by ID (the `id` returned by \
                 kubernetes.portforward.start).",
                json!({"type": "object", "properties": {
                    "id": {"type": "string", "description": "managed port-forward ID returned by kubernetes.portforward.start"}
                }, "required": ["id"]}),
                vec![Effect::Write, Effect::Process],
                Risk::Medium,
                Idempotency::Idempotent,
            ),
            portforward_stop,
        )
        .operation(
            read_op(
                "kubernetes.portforward.list",
                "List the managed Kubernetes port-forwards this plugin started, each probed for \
                 liveness, with local URL and target metadata. Filterable by namespace/context.",
                json!({"type": "object", "properties": {
                    "namespace": s_namespace_filter(),
                    "context": s_context(),
                    "live": {"type": "boolean", "description": "only list forwards whose process is still alive"}
                }}),
            ),
            portforward_list,
        )
        // --- deployments -------------------------------------------------------
        .operation(
            read_op(
                "kubernetes.deployment.list",
                "List Kubernetes deployments (all namespaces unless `namespace` is set).",
                inventory_list_schema(),
            ),
            deployment_list,
        )
        .operation(
            read_op(
                "kubernetes.deployment.show",
                "Show one Kubernetes deployment.",
                show_schema(),
            ),
            deployment_show,
        )
        .operation(
            read_op(
                "kubernetes.deployment.history",
                "List a deployment's rollout revisions (ReplicaSets, newest first) with images, \
                 replica counts, and creation timestamps. `name` is the deployment.",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace_filter(),
                    "name": {"type": "string", "description": "deployment name"},
                    "limit": s_limit()
                }, "required": ["name"]}),
            ),
            deployment_history,
        )
        .operation(
            op_spec(
                "kubernetes.deployment.scale",
                "Scale a Kubernetes deployment to a desired replica count.",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace(),
                    "name": {"type": "string", "description": "deployment name"},
                    "replicas": {"type": "integer", "description": "desired replica count (>= 0)"}
                }, "required": ["namespace", "name", "replicas"]}),
                vec![Effect::Write, Effect::Network],
                Risk::High,
                Idempotency::Idempotent,
            ),
            deployment_scale,
        )
        .operation(
            op_spec(
                "kubernetes.deployment.restart",
                "Rolling-restart a Kubernetes deployment (kubectl rollout restart).",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace(),
                    "name": {"type": "string", "description": "deployment name"}
                }, "required": ["namespace", "name"]}),
                vec![Effect::Write, Effect::Network],
                Risk::High,
                Idempotency::NonIdempotent,
            ),
            deployment_restart,
        )
        // --- ingresses / containers / nodes -----------------------------------
        .operation(
            read_op(
                "kubernetes.ingress.list",
                "List Kubernetes ingresses (all namespaces unless `namespace` is set).",
                inventory_list_schema(),
            ),
            ingress_list,
        )
        .operation(
            read_op(
                "kubernetes.container.list",
                "List Kubernetes containers derived from pods.",
                inventory_list_schema(),
            ),
            container_list,
        )
        .operation(
            read_op(
                "kubernetes.container.show",
                "Show one Kubernetes container (by name) derived from a pod.",
                show_schema(),
            ),
            container_show,
        )
        .operation(
            read_op(
                "kubernetes.event.list",
                "List Kubernetes events (newest-first via the API), filterable by namespace, \
                 involved object name/kind, and Warning type.",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace_filter(),
                    "name": {"type": "string", "description": "filter to events about this object (involvedObject.name)"},
                    "kind": {"type": "string", "description": "filter to this object kind (e.g. Pod, Deployment, Node)"},
                    "warnings_only": {"type": "boolean", "description": "only return Warning events"},
                    "limit": {"type": "integer", "description": "maximum events to return (default 50)"}
                }}),
            ),
            event_list,
        )
        .operation(
            read_op(
                "kubernetes.node.list",
                "List Kubernetes nodes with readiness, roles, kubelet version, and capacity.",
                json!({"type": "object", "properties": {"context": s_context()}}),
            ),
            node_list,
        )
        // --- exec (sensitive) --------------------------------------------------
        .operation(
            op_spec(
                "kubernetes.pod.exec",
                "Run a one-shot command in a pod container and return bounded stdout/stderr with \
                 the exit code. No TTY or stdin.",
                json!({"type": "object", "properties": {
                    "context": s_context(),
                    "namespace": s_namespace(),
                    "name": {"type": "string", "description": "pod name"},
                    "container": {"type": "string", "description": "container name; empty uses Kubernetes default"},
                    "command": {"type": "array", "items": {"type": "string"}, "description": "command argv to run (no shell; use [\"sh\",\"-c\",\"...\"] for shell syntax)"},
                    "timeout_seconds": {"type": "integer", "description": "command timeout in seconds (default 30, max 300)"}
                }, "required": ["namespace", "name", "command"]}),
                vec![Effect::Process, Effect::Network],
                Risk::High,
                Idempotency::NonIdempotent,
            ),
            pod_exec,
        )
}

// ---------------------------------------------------------------------------
// Schema helpers (shared property fragments).
// ---------------------------------------------------------------------------

fn s_context() -> Value {
    json!({"type": "string", "description": "kubeconfig context override"})
}

fn s_namespace() -> Value {
    json!({"type": "string", "description": "Kubernetes namespace"})
}

fn s_namespace_filter() -> Value {
    json!({"type": "string", "description": "namespace filter; empty means all namespaces"})
}

fn s_limit() -> Value {
    json!({"type": "integer", "description": "maximum records to return"})
}

/// Schema for the list ops: optional context + namespace filter.
fn inventory_list_schema() -> Value {
    json!({"type": "object", "properties": {
        "context": s_context(),
        "namespace": s_namespace_filter()
    }})
}

/// Schema for the show ops: required `name`, optional context + namespace.
fn show_schema() -> Value {
    json!({"type": "object", "properties": {
        "context": s_context(),
        "namespace": s_namespace(),
        "name": {"type": "string", "description": "resource name"}
    }, "required": ["name"]})
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

/// An operation spec with explicit effects/risk/idempotency (for the high-impact ops where the
/// generic [`read_op`]/[`write_op`] presets are not accurate enough).
fn op_spec(
    name: &str,
    description: &str,
    input_schema: Value,
    effects: Vec<Effect>,
    risk: Risk,
    idempotency: Idempotency,
) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        effects,
        risk: Some(risk),
        idempotency: Some(idempotency),
        secret_purposes: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// kubectl plumbing.
// ---------------------------------------------------------------------------

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

/// [`kubectl_json`] for an owned arg vector (built with optional flags).
fn kubectl_json_v(host: &mut Host, args: &[String]) -> Result<Value, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    kubectl_json(host, &refs)
}

/// Require a non-empty string argument (defensive: these values become CLI args).
fn req_nonempty<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.as_str()),
        _ => Err(format!("`{key}` (non-empty string) required")),
    }
}

/// An optional, trimmed-non-empty string field.
fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

/// `--context <ctx>` when a non-empty context is given.
fn ctx_args(input: &Value) -> Vec<String> {
    match opt_str(input, "context") {
        Some(c) => vec!["--context".into(), c.into()],
        None => Vec::new(),
    }
}

/// Namespace scope for list ops: `-n <ns>` when set, else `--all-namespaces`.
fn scope_args(input: &Value) -> Vec<String> {
    match opt_str(input, "namespace") {
        Some(n) => vec!["-n".into(), n.into()],
        None => vec!["--all-namespaces".into()],
    }
}

/// Namespace flag for single-resource ops: `-n <ns>` when set, else nothing (kubectl default).
fn ns_flag_opt(input: &Value) -> Vec<String> {
    match opt_str(input, "namespace") {
        Some(n) => vec!["-n".into(), n.into()],
        None => Vec::new(),
    }
}

/// Minimal standard-alphabet base64 decoder (kubectl secret `.data` values; no extra deps).
fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = val(c).ok_or_else(|| format!("invalid base64 char `{}`", c as char))? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Cluster discovery.
// ---------------------------------------------------------------------------

fn cluster_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let v = kubectl_json(host, &["config", "view"])?;
    let current = v
        .get("current-context")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let contexts: Vec<Value> = v
        .get("contexts")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    let ctx = c.get("context");
                    json!({
                        "name": name,
                        "current": name == current,
                        "cluster": ctx.and_then(|x| x.get("cluster")).and_then(|x| x.as_str()).unwrap_or(""),
                        "user": ctx.and_then(|x| x.get("user")).and_then(|x| x.as_str()).unwrap_or(""),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({ "contexts": contexts }))
}

fn cluster_test(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut argv: Vec<String> = vec![
        "kubectl".into(),
        "version".into(),
        "-o".into(),
        "json".into(),
    ];
    argv.extend(ctx_args(&input));
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = host.run(&refs, 30)?;
    let ctx = opt_str(&input, "context").unwrap_or("");
    if out.exit_code != 0 {
        return Ok(json!({ "context": ctx, "ok": false, "error": out.stderr.trim() }));
    }
    let parsed: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
    let sv = parsed.get("serverVersion");
    let server_version = sv
        .and_then(|x| x.get("gitVersion"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let platform = sv
        .and_then(|x| x.get("platform"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    Ok(json!({
        "context": ctx,
        "ok": !server_version.is_empty(),
        "server_version": server_version,
        "platform": platform,
    }))
}

fn endpoint_discover(input: Value, host: &mut Host) -> Result<Value, String> {
    let product = opt_str(&input, "product").unwrap_or("").to_lowercase();
    let limit = input.get("limit").and_then(|x| x.as_u64()).unwrap_or(50) as usize;
    let mut args = vec!["get".to_string(), "services".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let v = kubectl_json_v(host, &args)?;
    let mut candidates: Vec<Value> = Vec::new();
    if let Some(items) = v.get("items").and_then(|x| x.as_array()) {
        for it in items {
            let name = it
                .pointer("/metadata/name")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if name.is_empty() || (!product.is_empty() && !name.to_lowercase().contains(&product)) {
                continue;
            }
            let ns = it
                .pointer("/metadata/namespace")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let svc_type = it
                .pointer("/spec/type")
                .and_then(|x| x.as_str())
                .unwrap_or("ClusterIP");
            if let Some(ports) = it.pointer("/spec/ports").and_then(|x| x.as_array()) {
                for p in ports {
                    let port = p.get("port").and_then(|x| x.as_i64()).unwrap_or(0);
                    candidates.push(json!({
                        "product": if product.is_empty() { Value::Null } else { json!(product) },
                        "namespace": ns,
                        "service": name,
                        "type": svc_type,
                        "port": port,
                        "url": format!("http://{name}.{ns}.svc.cluster.local:{port}"),
                    }));
                    if candidates.len() >= limit {
                        break;
                    }
                }
            }
            if candidates.len() >= limit {
                break;
            }
        }
    }
    Ok(json!({ "candidates": candidates }))
}

// ---------------------------------------------------------------------------
// Secrets (sensitive).
// ---------------------------------------------------------------------------

fn secret_read(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let name = req_nonempty(&input, "name")?;
    let mut args = vec![
        "get".to_string(),
        "secret".to_string(),
        name.to_string(),
        "-n".to_string(),
        ns.to_string(),
    ];
    args.extend(ctx_args(&input));
    let v = kubectl_json_v(host, &args)?;
    let data = v.get("data").and_then(|d| d.as_object());
    let mut values = Map::new();
    if let Some(map) = data {
        match input.get("keys").and_then(|k| k.as_array()) {
            Some(keys) if !keys.is_empty() => {
                for key in keys {
                    let key = key.as_str().unwrap_or("").trim();
                    if key.is_empty() {
                        continue;
                    }
                    let enc = map
                        .get(key)
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| format!("secret {ns}/{name} has no key `{key}`"))?;
                    let dec = b64_decode(enc)?;
                    values.insert(key.into(), json!(String::from_utf8_lossy(&dec)));
                }
            }
            _ => {
                for (key, raw) in map {
                    let dec = b64_decode(raw.as_str().unwrap_or(""))?;
                    values.insert(key.clone(), json!(String::from_utf8_lossy(&dec)));
                }
            }
        }
    }
    Ok(json!({ "namespace": ns, "name": name, "values": values }))
}

// ---------------------------------------------------------------------------
// Inventory.
// ---------------------------------------------------------------------------

fn namespace_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut args = vec!["get".to_string(), "namespaces".to_string()];
    args.extend(ctx_args(&input));
    let v = kubectl_json_v(host, &args)?;
    contribute_simple(host, &v, "kubernetes.namespace", |it| {
        let name = it.pointer("/metadata/name").and_then(|x| x.as_str())?;
        let phase = it
            .pointer("/status/phase")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        Some((
            name.to_string(),
            name.to_string(),
            format!("status={phase}"),
        ))
    });
    Ok(v)
}

fn service_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut args = vec!["get".to_string(), "services".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let v = kubectl_json_v(host, &args)?;
    contribute_namespaced(host, &v, "kubernetes.service", |it| {
        let svc_type = it
            .pointer("/spec/type")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let cluster_ip = it
            .pointer("/spec/clusterIP")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        format!("type={svc_type} clusterIP={cluster_ip}")
    });
    Ok(v)
}

fn service_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let name = req_nonempty(&input, "name")?;
    let mut args = vec!["get".to_string(), "service".to_string(), name.to_string()];
    args.extend(ns_flag_opt(&input));
    args.extend(ctx_args(&input));
    kubectl_json_v(host, &args)
}

fn pod_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut args = vec!["get".to_string(), "pods".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let v = kubectl_json_v(host, &args)?;
    contribute_namespaced(host, &v, "kubernetes.pod", |it| {
        let phase = it
            .pointer("/status/phase")
            .and_then(|x| x.as_str())
            .unwrap_or("Unknown");
        let node = it
            .pointer("/spec/nodeName")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        format!("phase={phase} node={node}")
    });
    Ok(v)
}

fn pod_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let name = req_nonempty(&input, "name")?;
    let mut args = vec!["get".to_string(), "pod".to_string(), name.to_string()];
    args.extend(ns_flag_opt(&input));
    args.extend(ctx_args(&input));
    kubectl_json_v(host, &args)
}

fn deployment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut args = vec!["get".to_string(), "deployments".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let v = kubectl_json_v(host, &args)?;
    contribute_namespaced(host, &v, "kubernetes.deployment", deployment_summary);
    Ok(v)
}

fn deployment_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let name = req_nonempty(&input, "name")?;
    let mut args = vec![
        "get".to_string(),
        "deployment".to_string(),
        name.to_string(),
    ];
    args.extend(ns_flag_opt(&input));
    args.extend(ctx_args(&input));
    kubectl_json_v(host, &args)
}

fn ingress_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut args = vec!["get".to_string(), "ingress".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let v = kubectl_json_v(host, &args)?;
    contribute_namespaced(host, &v, "kubernetes.ingress", |it| {
        let hosts: Vec<&str> = it
            .pointer("/spec/rules")
            .and_then(|x| x.as_array())
            .map(|rules| {
                rules
                    .iter()
                    .filter_map(|r| r.get("host").and_then(|h| h.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        format!("hosts={}", hosts.join(","))
    });
    Ok(v)
}

// ---------------------------------------------------------------------------
// Containers (derived from pods).
// ---------------------------------------------------------------------------

/// Flatten a pod-list JSON into per-container records.
fn containers_from_pods(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return out;
    };
    for pod in items {
        let pod_name = pod
            .pointer("/metadata/name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let ns = pod
            .pointer("/metadata/namespace")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let statuses = pod
            .pointer("/status/containerStatuses")
            .and_then(|x| x.as_array());
        let Some(containers) = pod.pointer("/spec/containers").and_then(|x| x.as_array()) else {
            continue;
        };
        for c in containers {
            let cname = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let image = c.get("image").and_then(|x| x.as_str()).unwrap_or("");
            let st = statuses.and_then(|arr| {
                arr.iter()
                    .find(|s| s.get("name").and_then(|n| n.as_str()) == Some(cname))
            });
            out.push(json!({
                "name": cname,
                "namespace": ns,
                "pod": pod_name,
                "image": image,
                "ready": st.and_then(|s| s.get("ready")).and_then(|x| x.as_bool()).unwrap_or(false),
                "restart_count": st.and_then(|s| s.get("restartCount")).and_then(|x| x.as_i64()).unwrap_or(0),
            }));
        }
    }
    out
}

fn container_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut args = vec!["get".to_string(), "pods".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let v = kubectl_json_v(host, &args)?;
    let containers = containers_from_pods(&v);
    let records: Vec<Record> = containers
        .iter()
        .filter_map(|c| {
            let name = c.get("name").and_then(|x| x.as_str())?;
            let ns = c.get("namespace").and_then(|x| x.as_str()).unwrap_or("");
            let pod = c.get("pod").and_then(|x| x.as_str()).unwrap_or("");
            let image = c.get("image").and_then(|x| x.as_str()).unwrap_or("");
            Some(Record::new(
                Source::new("kubernetes"),
                "kubernetes.container",
                format!("{ns}/{pod}/{name}"),
                name,
                format!("pod={pod} image={image}"),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
    Ok(json!({ "count": containers.len(), "containers": containers }))
}

fn container_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let name = req_nonempty(&input, "name")?;
    let want_ns = opt_str(&input, "namespace");
    let mut args = vec!["get".to_string(), "pods".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let v = kubectl_json_v(host, &args)?;
    let container = containers_from_pods(&v).into_iter().find(|c| {
        c.get("name").and_then(|x| x.as_str()) == Some(name)
            && want_ns.is_none_or(|ns| c.get("namespace").and_then(|x| x.as_str()) == Some(ns))
    });
    match container {
        Some(c) => Ok(json!({ "container": c })),
        None => Err(format!("container `{name}` not found")),
    }
}

// ---------------------------------------------------------------------------
// Debugging: logs, events, nodes, exec, history.
// ---------------------------------------------------------------------------

fn pod_logs(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let name = opt_str(&input, "name");
    let selector = opt_str(&input, "selector");
    if name.is_none() && selector.is_none() {
        return Err("`name` or `selector` (non-empty string) required".into());
    }
    let mut argv: Vec<String> = vec!["kubectl".into(), "logs".into(), "-n".into(), ns.into()];
    argv.extend(ctx_args(&input));
    if let Some(n) = name {
        argv.push(n.into());
    }
    if let Some(s) = selector {
        argv.push("-l".into());
        argv.push(s.into());
    }
    if let Some(c) = opt_str(&input, "container") {
        argv.push("-c".into());
        argv.push(c.into());
    }
    let tail = input
        .get("tail_lines")
        .and_then(|v| v.as_i64())
        .unwrap_or(100);
    argv.push(format!("--tail={tail}"));
    if let Some(b) = input.get("limit_bytes").and_then(|v| v.as_i64()) {
        argv.push(format!("--limit-bytes={b}"));
    }
    if let Some(s) = opt_str(&input, "since") {
        argv.push(format!("--since={s}"));
    }
    if input
        .get("previous")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        argv.push("--previous".into());
    }
    if input
        .get("timestamps")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        argv.push("--timestamps".into());
    }
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = host.run(&refs, 30)?;
    if out.exit_code != 0 {
        return Err(format!(
            "kubectl logs failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    let lines: Vec<&str> = out.stdout.lines().collect();
    Ok(json!({
        "namespace": ns,
        "name": name.unwrap_or(""),
        "selector": selector.unwrap_or(""),
        "container": opt_str(&input, "container").unwrap_or(""),
        "line_count": lines.len(),
        "lines": lines,
    }))
}

fn event_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut args = vec!["get".to_string(), "events".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let mut fs: Vec<String> = Vec::new();
    if input
        .get("warnings_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        fs.push("type=Warning".into());
    }
    if let Some(n) = opt_str(&input, "name") {
        fs.push(format!("involvedObject.name={n}"));
    }
    if let Some(k) = opt_str(&input, "kind") {
        fs.push(format!("involvedObject.kind={k}"));
    }
    if !fs.is_empty() {
        args.push("--field-selector".into());
        args.push(fs.join(","));
    }
    let mut v = kubectl_json_v(host, &args)?;
    let limit = input.get("limit").and_then(|x| x.as_u64()).unwrap_or(50) as usize;
    if let Some(items) = v.get_mut("items").and_then(|x| x.as_array_mut()) {
        if items.len() > limit {
            items.truncate(limit);
        }
    }
    Ok(v)
}

fn node_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut args = vec!["get".to_string(), "nodes".to_string()];
    args.extend(ctx_args(&input));
    kubectl_json_v(host, &args)
}

fn deployment_history(input: Value, host: &mut Host) -> Result<Value, String> {
    let name = req_nonempty(&input, "name")?;
    let mut args = vec!["get".to_string(), "replicasets".to_string()];
    args.extend(ctx_args(&input));
    args.extend(scope_args(&input));
    let v = kubectl_json_v(host, &args)?;
    let mut revisions: Vec<Value> = Vec::new();
    if let Some(items) = v.get("items").and_then(|x| x.as_array()) {
        for rs in items {
            let owned = rs
                .pointer("/metadata/ownerReferences")
                .and_then(|x| x.as_array())
                .is_some_and(|owners| {
                    owners.iter().any(|o| {
                        o.get("kind").and_then(|k| k.as_str()) == Some("Deployment")
                            && o.get("name").and_then(|k| k.as_str()) == Some(name)
                    })
                });
            if !owned {
                continue;
            }
            let revision = rs
                .pointer("/metadata/annotations/deployment.kubernetes.io~1revision")
                .and_then(|x| x.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let images: Vec<String> = rs
                .pointer("/spec/template/spec/containers")
                .and_then(|x| x.as_array())
                .map(|cs| {
                    cs.iter()
                        .filter_map(|c| c.get("image").and_then(|i| i.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let desired = rs
                .pointer("/spec/replicas")
                .and_then(|x| x.as_i64())
                .unwrap_or(0);
            revisions.push(json!({
                "revision": revision,
                "replica_set": rs.pointer("/metadata/name").and_then(|x| x.as_str()).unwrap_or(""),
                "images": images,
                "desired_replicas": desired,
                "ready_replicas": rs.pointer("/status/readyReplicas").and_then(|x| x.as_i64()).unwrap_or(0),
                "current": desired > 0,
                "created_at": rs.pointer("/metadata/creationTimestamp").and_then(|x| x.as_str()).unwrap_or(""),
            }));
        }
    }
    revisions.sort_by(|a, b| {
        b["revision"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&a["revision"].as_i64().unwrap_or(0))
    });
    if let Some(limit) = input.get("limit").and_then(|x| x.as_u64()) {
        revisions.truncate(limit as usize);
    }
    Ok(json!({
        "deployment": name,
        "namespace": opt_str(&input, "namespace").unwrap_or(""),
        "count": revisions.len(),
        "revisions": revisions,
    }))
}

fn pod_exec(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let name = req_nonempty(&input, "name")?;
    let cmd = input
        .get("command")
        .and_then(|x| x.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`command` (non-empty array of strings) required")?;
    let cmd_strs: Vec<&str> = cmd
        .iter()
        .map(|c| c.as_str())
        .collect::<Option<Vec<_>>>()
        .ok_or("`command` must be an array of strings")?;
    let timeout = input
        .get("timeout_seconds")
        .and_then(|x| x.as_u64())
        .unwrap_or(30)
        .clamp(1, 300);
    let container = opt_str(&input, "container");
    let mut argv: Vec<String> = vec!["kubectl".into(), "exec".into(), "-n".into(), ns.into()];
    argv.extend(ctx_args(&input));
    if let Some(c) = container {
        argv.push("-c".into());
        argv.push(c.into());
    }
    argv.push(name.into());
    argv.push("--".into());
    for c in &cmd_strs {
        argv.push((*c).into());
    }
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = host.run(&refs, timeout)?;
    // fluxplane's exec is a *bounded* command (default 30s, max 300s) that returns the final
    // exit/stdout/stderr — not a persistent stream. `kubectl exec` via the one-shot `host.run`
    // matches that contract exactly, so no managed-process handle is needed here. `transport` mirrors
    // the reference's field (always `kubectl` for the CLI path).
    Ok(json!({
        "namespace": ns,
        "name": name,
        "container": container.unwrap_or(""),
        "command": cmd_strs,
        "transport": "kubectl",
        "exit_code": out.exit_code,
        "stdout": out.stdout,
        "stderr": out.stderr,
    }))
}

// ---------------------------------------------------------------------------
// Deployment mutations.
// ---------------------------------------------------------------------------

fn deployment_scale(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let name = req_nonempty(&input, "name")?;
    let replicas = input
        .get("replicas")
        .and_then(|x| x.as_i64())
        .ok_or("`replicas` (integer >= 0) required")?;
    if replicas < 0 {
        return Err("`replicas` must be >= 0".into());
    }
    let mut argv: Vec<String> = vec![
        "kubectl".into(),
        "scale".into(),
        format!("deployment/{name}"),
        format!("--replicas={replicas}"),
        "-n".into(),
        ns.into(),
    ];
    argv.extend(ctx_args(&input));
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = host.run(&refs, 30)?;
    if out.exit_code != 0 {
        return Err(format!(
            "kubectl scale {name} failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    Ok(json!({
        "ok": true,
        "namespace": ns,
        "name": name,
        "replicas": replicas,
        "output": out.stdout.trim(),
    }))
}

fn deployment_restart(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let name = req_nonempty(&input, "name")?;
    let mut argv: Vec<String> = vec![
        "kubectl".into(),
        "rollout".into(),
        "restart".into(),
        format!("deployment/{name}"),
        "-n".into(),
        ns.into(),
    ];
    argv.extend(ctx_args(&input));
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = host.run(&refs, 30)?;
    if out.exit_code != 0 {
        return Err(format!(
            "kubectl rollout restart {name} failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    Ok(json!({
        "ok": true,
        "namespace": ns,
        "name": name,
        "output": out.stdout.trim(),
    }))
}

// ---------------------------------------------------------------------------
// Port-forward — long-lived `kubectl port-forward` held by the host's managed-process registry.
// ---------------------------------------------------------------------------

/// Resolve the `<type>/<name>` resource ref from the fluxplane input shape: an explicit `resource`
/// wins; otherwise `<resource_type|service>/<name>`.
fn normalized_pf_resource(input: &Value) -> Option<String> {
    if let Some(r) = opt_str(input, "resource") {
        return Some(r.to_string());
    }
    let name = opt_str(input, "name")?;
    let rtype = opt_str(input, "resource_type").unwrap_or("service");
    Some(format!("{rtype}/{name}"))
}

/// Parse the actual local port from kubectl's readiness line, e.g.
/// `Forwarding from 127.0.0.1:19080 -> 80` (handles the `local_port=0` auto-allocate case).
fn parse_forwarding_local_port(line: &str) -> Option<i64> {
    let after = line.split("Forwarding from").nth(1)?;
    // after looks like " 127.0.0.1:19080 -> 80" (or "[::1]:19080 -> 80")
    let addr_port = after.split("->").next()?.trim();
    let port = addr_port.rsplit(':').next()?.trim();
    port.parse::<i64>().ok()
}

/// `proc_id` parsed from a `kpf-<id>` handle (the stable id returned by start).
fn parse_forward_id(id: &str) -> Option<u64> {
    id.strip_prefix("kpf-").and_then(|s| s.parse::<u64>().ok())
}

fn portforward_start(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let resource = normalized_pf_resource(&input)
        .ok_or("`resource` (or `name`) is required for port-forward")?;
    let remote_port = input
        .get("remote_port")
        .and_then(|x| x.as_i64())
        .filter(|p| *p > 0)
        .ok_or("`remote_port` (positive integer) required")?;
    let local_port = input
        .get("local_port")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let address = opt_str(&input, "address")
        .unwrap_or("127.0.0.1")
        .to_string();
    let context = opt_str(&input, "context").unwrap_or("").to_string();

    // kubectl port-forward <resource> [<local>]:<remote> -n <ns> [--address <a>] [--context <c>].
    // A local of 0 becomes `:<remote>` so kubectl picks a free port (recovered from the readiness line).
    let ports = if local_port > 0 {
        format!("{local_port}:{remote_port}")
    } else {
        format!(":{remote_port}")
    };
    let mut argv: Vec<String> = vec![
        "kubectl".into(),
        "port-forward".into(),
        resource.clone(),
        ports,
        "-n".into(),
        ns.into(),
        "--address".into(),
        address.clone(),
    ];
    argv.extend(ctx_args(&input));
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();

    // No KUBECONFIG override is forced here (kubeconfig is ambient to kubectl); pass it through when
    // the host environment carries one so the spawned process sees the same config as `host.run`.
    let env: Vec<(&str, &str)> = Vec::new();
    let proc_id = host.process_spawn(&refs, &env)?;

    // Poll the spawned process for kubectl's readiness line ("Forwarding from ...") to confirm the
    // forward is up (or surface an early failure). Bounded: a handful of reads, no real sleep
    // available in the sandbox, so we just drain a few times.
    let mut ready_local = if local_port > 0 { local_port } else { 0 };
    let mut ready = false;
    let mut last_err = String::new();
    for _ in 0..50 {
        let r = host.process_read(proc_id)?;
        if let Some(p) = r.stdout.lines().chain(r.stderr.lines()).find_map(|l| {
            if l.contains("Forwarding from") {
                parse_forwarding_local_port(l)
            } else {
                None
            }
        }) {
            ready_local = p;
            ready = true;
            break;
        }
        if !r.running {
            // kubectl exited before forwarding — propagate its diagnostics, then clean up.
            last_err = format!(
                "{}{}",
                r.stdout.trim(),
                if r.stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(" {}", r.stderr.trim())
                }
            );
            break;
        }
        if !r.stderr.trim().is_empty() {
            last_err = r.stderr.trim().to_string();
        }
    }
    if !ready {
        let _ = host.process_kill(proc_id);
        return Err(format!(
            "kubectl port-forward {resource} did not become ready{}",
            if last_err.is_empty() {
                String::new()
            } else {
                format!(": {last_err}")
            }
        ));
    }

    let id = format!("kpf-{proc_id}");
    forwards_lock().as_mut().unwrap().insert(
        proc_id,
        ForwardMeta {
            proc_id,
            context: context.clone(),
            namespace: ns.to_string(),
            resource: resource.clone(),
            address: address.clone(),
            local_port: ready_local,
            remote_port,
        },
    );
    Ok(json!({
        "id": id,
        "started": true,
        "context": context,
        "namespace": ns,
        "resource": resource,
        "address": address,
        "local_port": ready_local,
        "remote_port": remote_port,
        "local_url": format!("http://{address}:{ready_local}"),
        "command": argv,
    }))
}

fn portforward_stop(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = req_nonempty(&input, "id")?;
    let proc_id =
        parse_forward_id(id).ok_or_else(|| format!("`{id}` is not a valid port-forward id"))?;
    // Idempotent: killing an already-gone process is fine; drop it from our registry either way.
    let kill = host.process_kill(proc_id);
    forwards_lock().as_mut().unwrap().remove(&proc_id);
    match kill {
        Ok(()) => Ok(json!({ "id": id, "stopped": true, "signal": "SIGTERM" })),
        Err(e) => Ok(json!({ "id": id, "stopped": false, "error": e })),
    }
}

fn portforward_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns_filter = opt_str(&input, "namespace");
    let ctx_filter = opt_str(&input, "context");
    let live_only = input.get("live").and_then(|v| v.as_bool()).unwrap_or(false);

    // Snapshot the registry (clone out) so we don't hold the lock across host calls.
    let metas: Vec<ForwardMeta> = forwards_lock()
        .as_ref()
        .unwrap()
        .values()
        .cloned()
        .collect();

    let mut forwards: Vec<Value> = Vec::new();
    for m in metas {
        if ns_filter.is_some_and(|n| n != m.namespace) {
            continue;
        }
        if ctx_filter.is_some_and(|c| c != m.context) {
            continue;
        }
        // Probe liveness through the host's managed-process registry.
        let alive = host
            .process_status(m.proc_id)
            .map(|s| s.running)
            .unwrap_or(false);
        if live_only && !alive {
            continue;
        }
        forwards.push(json!({
            "id": format!("kpf-{}", m.proc_id),
            "context": m.context,
            "namespace": m.namespace,
            "resource": m.resource,
            "address": m.address,
            "local_port": m.local_port,
            "remote_port": m.remote_port,
            "local_url": format!("http://{}:{}", m.address, m.local_port),
            "alive": alive,
        }));
    }
    Ok(json!({ "count": forwards.len(), "forwards": forwards }))
}

// ---------------------------------------------------------------------------
// Datasource contribution helpers.
// ---------------------------------------------------------------------------

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

/// Contribute one record per `.items[]` for a namespaced kind: id `<ns>/<name>`, title = name,
/// body from `body_of`.
fn contribute_namespaced(
    host: &mut Host,
    v: &Value,
    entity: &str,
    body_of: impl Fn(&Value) -> String,
) {
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return;
    };
    let records: Vec<Record> = items
        .iter()
        .filter_map(|it| {
            let name = it.pointer("/metadata/name").and_then(|x| x.as_str())?;
            let ns = it
                .pointer("/metadata/namespace")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            Some(Record::new(
                Source::new("kubernetes"),
                entity,
                format!("{ns}/{name}"),
                name,
                body_of(it),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

/// Contribute one record per `.items[]` for a cluster-scoped kind, where `map_of` yields
/// `(id, title, body)`.
fn contribute_simple(
    host: &mut Host,
    v: &Value,
    entity: &str,
    map_of: impl Fn(&Value) -> Option<(String, String, String)>,
) {
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return;
    };
    let records: Vec<Record> = items
        .iter()
        .filter_map(|it| {
            let (id, title, body) = map_of(it)?;
            Some(Record::new(
                Source::new("kubernetes"),
                entity,
                id,
                title,
                body,
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reset the module-level port-forward registry so the port-forward tests don't leak state into
    /// one another (they share `FORWARDS`).
    fn clear_forwards() {
        forwards_lock().as_mut().unwrap().clear();
    }

    #[test]
    fn cluster_list_reshapes_contexts() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "config view",
            r#"{"current-context":"prod","contexts":[
                {"name":"prod","context":{"cluster":"c1","user":"u1"}},
                {"name":"dev","context":{"cluster":"c2","user":"u2"}}]}"#,
        );
        let out = plugin
            .call("kubernetes.cluster.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["contexts"][0]["name"], "prod");
        assert_eq!(out["contexts"][0]["current"], true);
        assert_eq!(out["contexts"][1]["current"], false);
    }

    #[test]
    fn test_probes_server_version() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "version",
            r#"{"serverVersion":{"gitVersion":"v1.29.0","platform":"linux/amd64"}}"#,
        );
        let out = plugin
            .call("kubernetes.test", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["server_version"], "v1.29.0");
        assert_eq!(out["platform"], "linux/amd64");
    }

    #[test]
    fn endpoint_discover_builds_service_candidates() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get services -n monitoring",
            r#"{"items":[{"metadata":{"name":"prometheus","namespace":"monitoring"},"spec":{"type":"ClusterIP","ports":[{"port":9090}]}}]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.endpoint.discover",
                json!({ "namespace": "monitoring", "product": "prometheus" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["candidates"][0]["service"], "prometheus");
        assert_eq!(
            out["candidates"][0]["url"],
            "http://prometheus.monitoring.svc.cluster.local:9090"
        );
    }

    #[test]
    fn secret_read_decodes_base64_values() {
        let plugin = manifest_builder().build();
        // base64("hunter2") = aHVudGVyMg==
        let mut host = MockHost::default().with_process(
            "get secret db-creds",
            r#"{"data":{"password":"aHVudGVyMg=="}}"#,
        );
        let out = plugin
            .call(
                "kubernetes.secret.read",
                json!({ "namespace": "prod", "name": "db-creds" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["values"]["password"], "hunter2");
    }

    #[test]
    fn secret_read_errors_on_missing_requested_key() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get secret db-creds",
            r#"{"data":{"password":"aHVudGVyMg=="}}"#,
        );
        assert!(plugin
            .call(
                "kubernetes.secret.read",
                json!({ "namespace": "prod", "name": "db-creds", "keys": ["nope"] }),
                &mut host,
            )
            .is_err());
    }

    #[test]
    fn namespace_list_runs_kubectl_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get namespaces",
            r#"{"items":[{"metadata":{"name":"prod"},"status":{"phase":"Active"}}]}"#,
        );
        let out = plugin
            .call("kubernetes.namespace.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["items"][0]["metadata"]["name"], "prod");
        let recs = host.contributed.borrow();
        assert_eq!(recs[0].entity, "kubernetes.namespace");
        assert_eq!(recs[0].id, "prod");
    }

    #[test]
    fn service_list_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get services --all-namespaces",
            r#"{"items":[{"metadata":{"name":"api","namespace":"prod"},"spec":{"type":"ClusterIP","clusterIP":"10.0.0.1"}}]}"#,
        );
        let out = plugin
            .call("kubernetes.service.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["items"][0]["metadata"]["name"], "api");
        let recs = host.contributed.borrow();
        assert_eq!(recs[0].entity, "kubernetes.service");
        assert_eq!(recs[0].id, "prod/api");
    }

    #[test]
    fn service_show_targets_one_resource() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get service api -n prod",
            r#"{"metadata":{"name":"api","namespace":"prod"}}"#,
        );
        let out = plugin
            .call(
                "kubernetes.service.show",
                json!({ "name": "api", "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["metadata"]["name"], "api");
    }

    #[test]
    fn pod_list_runs_kubectl_and_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get pods -n prod",
            r#"{"items":[{"metadata":{"name":"api-1","namespace":"prod"},"status":{"phase":"Running"},"spec":{"nodeName":"n1"}}]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.pod.list",
                json!({ "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["items"][0]["metadata"]["name"], "api-1");
        let recs = host.contributed.borrow();
        assert_eq!(recs[0].entity, "kubernetes.pod");
        assert_eq!(recs[0].id, "prod/api-1");
        assert!(recs[0].body.contains("phase=Running"));
        assert!(recs[0].body.contains("node=n1"));
    }

    #[test]
    fn pod_show_targets_one_resource() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get pod api-1 -n prod",
            r#"{"metadata":{"name":"api-1","namespace":"prod"}}"#,
        );
        let out = plugin
            .call(
                "kubernetes.pod.show",
                json!({ "name": "api-1", "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["metadata"]["name"], "api-1");
    }

    #[test]
    fn pod_logs_returns_lines() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process("logs -n prod api-1", "line1\nline2\n");
        let out = plugin
            .call(
                "kubernetes.pod.logs",
                json!({ "namespace": "prod", "name": "api-1" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["line_count"], 2);
        assert_eq!(out["lines"][0], "line1");
    }

    #[test]
    fn pod_logs_requires_name_or_selector() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin
            .call(
                "kubernetes.pod.logs",
                json!({ "namespace": "prod" }),
                &mut host,
            )
            .is_err());
    }

    #[test]
    fn deployment_list_contributes_with_replica_summary() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get deployments -n prod",
            r#"{"items":[{"metadata":{"name":"web","namespace":"prod"},"spec":{"replicas":3},"status":{"readyReplicas":2}}]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.deployment.list",
                json!({ "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["items"][0]["metadata"]["name"], "web");
        let recs = host.contributed.borrow();
        assert_eq!(recs[0].entity, "kubernetes.deployment");
        assert_eq!(recs[0].id, "prod/web");
        assert!(recs[0].body.contains("ready=2"));
        assert!(recs[0].body.contains("desired=3"));
    }

    #[test]
    fn deployment_show_targets_one_resource() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get deployment web -n prod",
            r#"{"metadata":{"name":"web","namespace":"prod"}}"#,
        );
        let out = plugin
            .call(
                "kubernetes.deployment.show",
                json!({ "name": "web", "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["metadata"]["name"], "web");
    }

    #[test]
    fn deployment_history_filters_owned_replicasets_newest_first() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get replicasets -n prod",
            r#"{"items":[
                {"metadata":{"name":"web-1","namespace":"prod","annotations":{"deployment.kubernetes.io/revision":"1"},"ownerReferences":[{"kind":"Deployment","name":"web"}]},"spec":{"replicas":0,"template":{"spec":{"containers":[{"image":"web:v1"}]}}}},
                {"metadata":{"name":"web-2","namespace":"prod","annotations":{"deployment.kubernetes.io/revision":"2"},"ownerReferences":[{"kind":"Deployment","name":"web"}]},"spec":{"replicas":3,"template":{"spec":{"containers":[{"image":"web:v2"}]}}}},
                {"metadata":{"name":"other-1","namespace":"prod","ownerReferences":[{"kind":"Deployment","name":"other"}]},"spec":{"replicas":1,"template":{"spec":{"containers":[{"image":"other:v1"}]}}}}
            ]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.deployment.history",
                json!({ "name": "web", "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["revisions"][0]["revision"], 2);
        assert_eq!(out["revisions"][0]["images"][0], "web:v2");
        assert_eq!(out["revisions"][0]["current"], true);
    }

    #[test]
    fn deployment_scale_invokes_kubectl_scale() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "scale deployment/web --replicas=5",
            "deployment.apps/web scaled\n",
        );
        let out = plugin
            .call(
                "kubernetes.deployment.scale",
                json!({ "namespace": "prod", "name": "web", "replicas": 5 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["replicas"], 5);
    }

    #[test]
    fn deployment_scale_rejects_negative_replicas() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin
            .call(
                "kubernetes.deployment.scale",
                json!({ "namespace": "prod", "name": "web", "replicas": -1 }),
                &mut host,
            )
            .is_err());
    }

    #[test]
    fn deployment_restart_invokes_rollout_restart() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "rollout restart deployment/web",
            "deployment.apps/web restarted\n",
        );
        let out = plugin
            .call(
                "kubernetes.deployment.restart",
                json!({ "namespace": "prod", "name": "web" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["name"], "web");
    }

    #[test]
    fn ingress_list_contributes_with_hosts() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get ingress -n prod",
            r#"{"items":[{"metadata":{"name":"web-ing","namespace":"prod"},"spec":{"rules":[{"host":"web.example.com"}]}}]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.ingress.list",
                json!({ "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["items"][0]["metadata"]["name"], "web-ing");
        let recs = host.contributed.borrow();
        assert_eq!(recs[0].entity, "kubernetes.ingress");
        assert!(recs[0].body.contains("web.example.com"));
    }

    #[test]
    fn container_list_flattens_pods() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get pods -n prod",
            r#"{"items":[{"metadata":{"name":"api-1","namespace":"prod"},"spec":{"containers":[{"name":"app","image":"api:v1"}]},"status":{"containerStatuses":[{"name":"app","ready":true,"restartCount":2}]}}]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.container.list",
                json!({ "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["containers"][0]["name"], "app");
        assert_eq!(out["containers"][0]["pod"], "api-1");
        assert_eq!(out["containers"][0]["restart_count"], 2);
        let recs = host.contributed.borrow();
        assert_eq!(recs[0].entity, "kubernetes.container");
        assert_eq!(recs[0].id, "prod/api-1/app");
    }

    #[test]
    fn container_show_finds_by_name() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get pods -n prod",
            r#"{"items":[{"metadata":{"name":"api-1","namespace":"prod"},"spec":{"containers":[{"name":"app","image":"api:v1"}]}}]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.container.show",
                json!({ "name": "app", "namespace": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["container"]["name"], "app");
        assert!(plugin
            .call(
                "kubernetes.container.show",
                json!({ "name": "missing", "namespace": "prod" }),
                &mut host,
            )
            .is_err());
    }

    #[test]
    fn event_list_truncates_to_limit() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get events -n prod",
            r#"{"items":[{"reason":"A"},{"reason":"B"},{"reason":"C"}]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.event.list",
                json!({ "namespace": "prod", "limit": 2 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["items"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn node_list_runs_kubectl() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_process("get nodes", r#"{"items":[{"metadata":{"name":"node-1"}}]}"#);
        let out = plugin
            .call("kubernetes.node.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["items"][0]["metadata"]["name"], "node-1");
    }

    #[test]
    fn pod_exec_captures_output_and_exit() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process("exec -n prod", "/tmp\n");
        let out = plugin
            .call(
                "kubernetes.pod.exec",
                json!({ "namespace": "prod", "name": "api-1", "command": ["ls", "/tmp"] }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["exit_code"], 0);
        assert_eq!(out["stdout"], "/tmp\n");
        assert_eq!(out["command"][0], "ls");
    }

    #[test]
    fn pod_exec_requires_command() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin
            .call(
                "kubernetes.pod.exec",
                json!({ "namespace": "prod", "name": "api-1", "command": [] }),
                &mut host,
            )
            .is_err());
    }

    #[test]
    fn portforward_start_spawns_and_confirms_readiness() {
        // start spawns `kubectl port-forward` and parses kubectl's readiness line for the local port,
        // then list reports it (alive), then stop kills it and drops it from the registry. Uses the
        // module-level FORWARDS registry, so isolate the proc_id from other tests via a unique value.
        clear_forwards();
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_spawn(4242).with_proc_output(
            "Forwarding from 127.0.0.1:19080 -> 80\n",
            "",
            true,
        );

        let start = plugin
            .call(
                "kubernetes.portforward.start",
                json!({ "namespace": "monitoring", "resource": "service/homer", "remote_port": 80, "local_port": 19080 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(start["started"], true);
        assert_eq!(start["id"], "kpf-4242");
        assert_eq!(start["local_port"], 19080);
        assert_eq!(start["remote_port"], 80);
        assert_eq!(start["resource"], "service/homer");
        assert_eq!(start["local_url"], "http://127.0.0.1:19080");

        // list sees the tracked forward and probes liveness (mock reports running=true). Scope by the
        // unique namespace so this is robust to the shared FORWARDS registry under parallel tests.
        let list = plugin
            .call(
                "kubernetes.portforward.list",
                json!({ "namespace": "monitoring" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(list["count"], 1);
        assert_eq!(list["forwards"][0]["id"], "kpf-4242");
        assert_eq!(list["forwards"][0]["alive"], true);
        assert_eq!(list["forwards"][0]["namespace"], "monitoring");

        // namespace filter excludes a non-matching forward.
        let filtered = plugin
            .call(
                "kubernetes.portforward.list",
                json!({ "namespace": "monitoring-other" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(filtered["count"], 0);

        // stop kills the proc and removes it from the registry.
        let stop = plugin
            .call(
                "kubernetes.portforward.stop",
                json!({ "id": "kpf-4242" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(stop["stopped"], true);
        let after = plugin
            .call(
                "kubernetes.portforward.list",
                json!({ "namespace": "monitoring" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(after["count"], 0);
    }

    #[test]
    fn portforward_start_auto_allocates_local_port_from_readiness_line() {
        // local_port=0 → kubectl allocates; the actual port is recovered from "Forwarding from".
        clear_forwards();
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_spawn(77).with_proc_output(
            "Forwarding from 127.0.0.1:54321 -> 9090\n",
            "",
            true,
        );
        let start = plugin
            .call(
                "kubernetes.portforward.start",
                json!({ "namespace": "prod", "name": "api", "resource_type": "deployment", "remote_port": 9090 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(start["local_port"], 54321);
        assert_eq!(start["resource"], "deployment/api");
        clear_forwards();
    }

    #[test]
    fn portforward_start_errors_when_kubectl_exits_before_ready() {
        // A process that exits without ever printing the readiness line is a startup failure.
        clear_forwards();
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_spawn(9).with_proc_output(
            "",
            "error: unable to forward port\n",
            false,
        );
        let err = plugin.call(
            "kubernetes.portforward.start",
            json!({ "namespace": "prod", "resource": "service/api", "remote_port": 80 }),
            &mut host,
        );
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("did not become ready"));
    }

    #[test]
    fn portforward_stop_rejects_bad_id() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin
            .call(
                "kubernetes.portforward.stop",
                json!({ "id": "not-a-pf" }),
                &mut host,
            )
            .is_err());
    }

    #[test]
    fn rejects_missing_or_empty_required_args() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        // show requires name
        assert!(plugin
            .call("kubernetes.pod.show", json!({}), &mut host)
            .is_err());
        assert!(plugin
            .call("kubernetes.pod.show", json!({ "name": "" }), &mut host)
            .is_err());
        // a non-string name is rejected too (defensive — it would become a CLI arg)
        assert!(plugin
            .call("kubernetes.pod.show", json!({ "name": 7 }), &mut host)
            .is_err());
        // secret.read requires namespace + name
        assert!(plugin
            .call(
                "kubernetes.secret.read",
                json!({ "namespace": "prod" }),
                &mut host,
            )
            .is_err());
    }

    #[test]
    fn manifest_declares_ops_and_kubectl_capability() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 24);
        assert_eq!(m.capabilities.process, vec!["kubectl".to_string()]);
        assert!(m
            .datasources
            .iter()
            .any(|d| d.name == "kubernetes.inventory"));
        // op names mirror fluxplane exactly
        assert!(m
            .operations
            .iter()
            .any(|o| o.name == "kubernetes.cluster.list"));
        assert!(m.operations.iter().any(|o| o.name == "kubernetes.pod.exec"));
        // secret.read is a high-risk read; pod.exec a high-risk process write
        let secret = m
            .operations
            .iter()
            .find(|o| o.name == "kubernetes.secret.read")
            .unwrap();
        assert_eq!(secret.risk, Some(Risk::High));
        assert!(secret.effects.contains(&Effect::Read));
        let exec = m
            .operations
            .iter()
            .find(|o| o.name == "kubernetes.pod.exec")
            .unwrap();
        assert_eq!(exec.risk, Some(Risk::High));
        assert!(exec.effects.contains(&Effect::Process));
    }
}
