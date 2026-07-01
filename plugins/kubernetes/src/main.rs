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
use std::time::SystemTime;

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
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

// ===========================================================================
// Schema-only op input structs (D-36)
// ===========================================================================
// Each op's `input_schema` is derived from the structs below via schemars
// (`host_kit::read_op_typed::<T>` / `write_op_typed::<T>` / `op_spec_typed::<T>`)
// instead of hand-written `json!({...})` literals. The structs are schema-only:
// handlers keep their existing `flex_str` / `flex_i64` / `flex_arr` extractors
// (the D-34 schema-only precedent).

/// `kubernetes.cluster.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ClusterListInput {}

/// `kubernetes.test`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ClusterTestInput {
    /// Kubeconfig context override.
    context: Option<String>,
}

/// `kubernetes.endpoint.discover`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct EndpointDiscoverInput {
    context: Option<String>,
    /// Short alias resolved against kubeconfig context names.
    cluster: Option<String>,
    namespace: Option<String>,
    product: Option<String>,
    query: Option<String>,
    latest_namespace: Option<bool>,
    limit: Option<i64>,
}

/// `kubernetes.secret.read`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SecretReadInput {
    context: Option<String>,
    namespace: String,
    name: String,
    keys: Option<Vec<String>>,
}

/// Shared inventory-list params: `kubernetes.namespace.list`, `kubernetes.service.list`,
/// `kubernetes.pod.list`, `kubernetes.deployment.list`, `kubernetes.ingress.list`,
/// `kubernetes.container.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct InventoryListInput {
    context: Option<String>,
    namespace: Option<String>,
    query: Option<String>,
    limit: Option<i64>,
}

/// Shared inventory-show params: `kubernetes.service.show`, `kubernetes.pod.show`,
/// `kubernetes.deployment.show`, `kubernetes.container.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct InventoryShowInput {
    context: Option<String>,
    namespace: Option<String>,
    name: String,
}

/// `kubernetes.pod.logs`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PodLogsInput {
    context: Option<String>,
    namespace: String,
    name: Option<String>,
    selector: Option<String>,
    container: Option<String>,
    tail_lines: Option<i64>,
    limit_bytes: Option<i64>,
    since: Option<String>,
    /// Absolute RFC3339 timestamp upper bound; filtered client-side.
    until: Option<String>,
    previous: Option<bool>,
    timestamps: Option<bool>,
}

/// Resource type for the `resource_type` field of `kubernetes.portforward.start`.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum ResourceType {
    Service,
    Pod,
    Deployment,
}

/// `kubernetes.portforward.start`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PortForwardStartInput {
    context: Option<String>,
    namespace: String,
    resource: Option<String>,
    name: Option<String>,
    resource_type: Option<ResourceType>,
    remote_port: i64,
    local_port: Option<i64>,
    address: Option<String>,
    /// Auto-cleanup timeout in seconds (default 3600, capped at 28800).
    duration_seconds: Option<i64>,
}

/// `kubernetes.portforward.stop`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PortForwardStopInput {
    id: String,
    process_group: Option<i64>,
    pid: Option<i64>,
}

/// `kubernetes.portforward.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PortForwardListInput {
    namespace: Option<String>,
    context: Option<String>,
    live: Option<bool>,
}

/// `kubernetes.deployment.history`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DeploymentHistoryInput {
    context: Option<String>,
    namespace: Option<String>,
    name: String,
    limit: Option<i64>,
}

/// `kubernetes.deployment.scale`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DeploymentScaleInput {
    context: Option<String>,
    namespace: String,
    name: String,
    replicas: i64,
}

/// `kubernetes.deployment.restart`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DeploymentRestartInput {
    context: Option<String>,
    namespace: String,
    name: String,
}

/// `kubernetes.event.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct EventListInput {
    context: Option<String>,
    namespace: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    warnings_only: Option<bool>,
    limit: Option<i64>,
}

/// `kubernetes.node.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct NodeListInput {
    context: Option<String>,
    query: Option<String>,
    limit: Option<i64>,
}

/// `kubernetes.pod.exec`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PodExecInput {
    context: Option<String>,
    namespace: String,
    name: String,
    container: Option<String>,
    command: Vec<String>,
    timeout_seconds: Option<i64>,
}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("kubernetes", "0.2.0")
        .capabilities(Caps {
            process: vec!["kubectl".into()],
            ..Default::default()
        })
        // Discovery products (D-28): the host's fan-out broker routes a consumer's discovery query
        // for any of these to this plugin's `kubernetes.endpoint.discover` op. `kubernetes` yields
        // cluster endpoints (one per kubeconfig context); the rest are matched against in-cluster
        // Services/Ingresses (and Secrets, for the database products).
        .discovers("kubernetes")
        .discovers("prometheus")
        .discovers("loki")
        .discovers("grafana")
        .discovers("alertmanager")
        .discovers("postgres")
        .discovers("mysql")
        .datasource(ds(
            "kubernetes.inventory",
            "kubernetes.resource",
            "Kubernetes namespaces, services, pods, deployments, containers, and ingresses.",
        ))
        // --- cluster discovery -------------------------------------------------
        .operation(
            read_op_typed::<ClusterListInput>("kubernetes.cluster.list", "List kubeconfig contexts."),
            cluster_list,
        )
        .operation(
            read_op_typed::<ClusterTestInput>(
                "kubernetes.test",
                "Probe Kubernetes cluster reachability through kubeconfig.",
            ),
            cluster_test,
        )
        .operation(
            read_op_typed::<EndpointDiscoverInput>(
                "kubernetes.endpoint.discover",
                "Discover product endpoints as weak references (URL + credential location, never a \
                 secret). `product=kubernetes` yields one cluster endpoint per kubeconfig context; \
                 other products match in-cluster Services and Ingresses by name or \
                 `app.kubernetes.io/name`. `postgres`/`mysql` also scan Secrets for crossplane/RDS \
                 connection patterns and return a database endpoint whose credential is a \
                 `kubernetes/<ns>/<secret>/<key>` reference. `cluster` is a short alias (e.g. `dev`) \
                 resolved against kubeconfig context names — it matches a context whose name contains \
                 the alias (case-insensitive), and a multi-match is a loud error rather than a silent \
                 empty result. `namespace` is a literal namespace name. Set `latest_namespace: true` to \
                 target the newest namespace by creation time (a literal namespace named `latest` is \
                 just `namespace: \"latest\"`; the free-text `query` no longer triggers the heuristic).",
            ),
            endpoint_discover,
        )
        // --- secrets (sensitive) ----------------------------------------------
        .operation(
            op_spec_typed::<SecretReadInput>(
                "kubernetes.secret.read",
                "Read one Kubernetes secret's decoded values. Sensitive: the result is secret \
                 material intended for piping into auth or secret stores, not for display.",
                vec![Effect::Read, Effect::Network],
                Risk::High,
                Idempotency::Idempotent,
            ),
            secret_read,
        )
        // --- inventory ---------------------------------------------------------
        .operation(
            read_op_typed::<InventoryListInput>("kubernetes.namespace.list", "List Kubernetes namespaces."),
            namespace_list,
        )
        .operation(
            read_op_typed::<InventoryListInput>(
                "kubernetes.service.list",
                "List Kubernetes services (all namespaces unless `namespace` is set).",
            ),
            service_list,
        )
        .operation(
            read_op_typed::<InventoryShowInput>("kubernetes.service.show", "Show one Kubernetes service."),
            service_show,
        )
        .operation(
            read_op_typed::<InventoryListInput>(
                "kubernetes.pod.list",
                "List Kubernetes pods (all namespaces unless `namespace` is set).",
            ),
            pod_list,
        )
        .operation(
            read_op_typed::<InventoryShowInput>("kubernetes.pod.show", "Show one Kubernetes pod."),
            pod_show,
        )
        .operation(
            read_op_typed::<PodLogsInput>(
                "kubernetes.pod.logs",
                "Read bounded logs for one Kubernetes pod (by name) or label selector.",
            ),
            pod_logs,
        )
        // --- port-forward (held by the host's managed-process registry) --------
        .operation(
            op_spec_typed::<PortForwardStartInput>(
                "kubernetes.portforward.start",
                "Start a managed Kubernetes port-forward for a service, pod, or deployment. The \
                 forward is held by the host as a long-lived `kubectl port-forward` process that \
                 persists across calls; list with kubernetes.portforward.list and stop with \
                 kubernetes.portforward.stop.",
                vec![Effect::Write, Effect::Network],
                Risk::Medium,
                Idempotency::NonIdempotent,
            ),
            portforward_start,
        )
        .operation(
            op_spec_typed::<PortForwardStopInput>(
                "kubernetes.portforward.stop",
                "Stop a managed Kubernetes port-forward by ID (the `id` returned by \
                 kubernetes.portforward.start).",
                vec![Effect::Write, Effect::Process],
                Risk::Medium,
                Idempotency::Idempotent,
            ),
            portforward_stop,
        )
        .operation(
            read_op_typed::<PortForwardListInput>(
                "kubernetes.portforward.list",
                "List the managed Kubernetes port-forwards this plugin started, each probed for \
                 liveness, with local URL and target metadata. Filterable by namespace/context.",
            ),
            portforward_list,
        )
        // --- deployments -------------------------------------------------------
        .operation(
            read_op_typed::<InventoryListInput>(
                "kubernetes.deployment.list",
                "List Kubernetes deployments (all namespaces unless `namespace` is set).",
            ),
            deployment_list,
        )
        .operation(
            read_op_typed::<InventoryShowInput>("kubernetes.deployment.show", "Show one Kubernetes deployment."),
            deployment_show,
        )
        .operation(
            read_op_typed::<DeploymentHistoryInput>(
                "kubernetes.deployment.history",
                "List a deployment's rollout revisions (ReplicaSets, newest first) with images, \
                 replica counts, and creation timestamps. `name` is the deployment.",
            ),
            deployment_history,
        )
        .operation(
            op_spec_typed::<DeploymentScaleInput>(
                "kubernetes.deployment.scale",
                "Scale a Kubernetes deployment to a desired replica count.",
                vec![Effect::Write, Effect::Network],
                Risk::High,
                Idempotency::Idempotent,
            ),
            deployment_scale,
        )
        .operation(
            op_spec_typed::<DeploymentRestartInput>(
                "kubernetes.deployment.restart",
                "Rolling-restart a Kubernetes deployment (kubectl rollout restart).",
                vec![Effect::Write, Effect::Network],
                Risk::High,
                Idempotency::NonIdempotent,
            ),
            deployment_restart,
        )
        // --- ingresses / containers / nodes -----------------------------------
        .operation(
            read_op_typed::<InventoryListInput>(
                "kubernetes.ingress.list",
                "List Kubernetes ingresses (all namespaces unless `namespace` is set).",
            ),
            ingress_list,
        )
        .operation(
            read_op_typed::<InventoryListInput>(
                "kubernetes.container.list",
                "List Kubernetes containers derived from pods.",
            ),
            container_list,
        )
        .operation(
            read_op_typed::<InventoryShowInput>(
                "kubernetes.container.show",
                "Show one Kubernetes container (by name) derived from a pod.",
            ),
            container_show,
        )
        .operation(
            read_op_typed::<EventListInput>(
                "kubernetes.event.list",
                "List Kubernetes events (newest-first via the API), filterable by namespace, \
                 involved object name/kind, and Warning type.",
            ),
            event_list,
        )
        .operation(
            read_op_typed::<NodeListInput>(
                "kubernetes.node.list",
                "List Kubernetes nodes with readiness, roles, kubelet version, and capacity.",
            ),
            node_list,
        )
        // --- exec (sensitive) --------------------------------------------------
        .operation(
            op_spec_typed::<PodExecInput>(
                "kubernetes.pod.exec",
                "Run a one-shot command in a pod container and return bounded stdout/stderr with \
                 the exit code. No TTY or stdin.",
                vec![Effect::Process, Effect::Network],
                Risk::High,
                Idempotency::NonIdempotent,
            ),
            pod_exec,
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

/// A typed operation spec with explicit effects/risk/idempotency (for the high-impact ops where
/// the generic [`read_op_typed`]/[`write_op_typed`] presets are not accurate enough).
fn op_spec_typed<T: JsonSchema>(
    name: &str,
    description: &str,
    effects: Vec<Effect>,
    risk: Risk,
    idempotency: Idempotency,
) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        description: description.into(),
        input_schema: op_input_schema::<T>(),
        effects,
        risk: Some(risk),
        idempotency: Some(idempotency),
        secret_purposes: Vec::new(),
    }
}

/// Apply optional `query` substring filtering and `limit` truncation to a `kubectl -o json`
/// `items` array in-place. Matching is case-insensitive and considers the whole object JSON.
fn apply_query_limit(items_array: &mut Vec<Value>, query: Option<&str>, limit: Option<i64>) {
    if let Some(q) = query.filter(|s| !s.trim().is_empty()) {
        let qlc = q.trim().to_lowercase();
        items_array.retain(|it| it.to_string().to_lowercase().contains(&qlc));
    }
    if let Some(l) = limit.filter(|l| *l > 0) {
        let l = l as usize;
        if items_array.len() > l {
            items_array.truncate(l);
        }
    }
}

/// Parse an RFC3339 timestamp to unix seconds (minimal, matching the fluxplane `until` bound)
/// and to a nanosecond count for ordering.
fn parse_rfc3339_seconds(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 20 {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;
    // Days since epoch (Rata Die).
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 12 } else { month };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m - 3) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

/// Current UTC time as an RFC3339 string (second precision).
fn now_rfc3339() -> String {
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_rfc3339(ts)
}

fn format_rfc3339(ts: i64) -> String {
    let mut rem = ts;
    let secs = rem % 60;
    rem /= 60;
    let mins = rem % 60;
    rem /= 60;
    let hrs = rem % 24;
    let days = rem / 24;
    let (y, mo, d) = days_from_civil(days);
    format!("{y:04}-{mo:02}-{d:02}T{hrs:02}:{mins:02}:{secs:02}Z")
}

fn days_from_civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Split log output into lines, optionally filtering to those whose RFC3339 timestamp is not after
/// `until` (fluxplane parity). When `until` is set, kubectl is forced to emit timestamps; we strip
/// them from the returned line unless `keep_timestamps` is true.
fn filter_log_lines(
    stdout: &str,
    until: Option<&str>,
    keep_timestamps: bool,
) -> Result<Vec<String>, String> {
    let until_ts = until.and_then(parse_rfc3339_seconds);
    let stdout = stdout.trim_end_matches('\n');
    let mut out = Vec::new();
    for line in stdout.lines() {
        if let Some(limit_ts) = until_ts {
            // kubectl log timestamps are RFC3339Nano: "2006-01-02T15:04:05.999999999Z ...".
            let (head, rest) = line.split_once(' ').unwrap_or((line, ""));
            let ts = parse_rfc3339_seconds(head);
            if matches!(ts, Some(ts) if ts > limit_ts) {
                continue;
            }
            if keep_timestamps || rest.is_empty() {
                out.push(line.to_string());
            } else {
                out.push(rest.to_string());
            }
        } else {
            out.push(line.to_string());
        }
    }
    Ok(out)
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

fn flex_i64(input: &Value, key: &str) -> Option<i64> {
    input.get(key).and_then(|v| v.as_i64())
}

/// `--context <ctx>` when a non-empty context is given.
fn ctx_args(input: &Value) -> Vec<String> {
    match opt_str(input, "context") {
        Some(c) => vec!["--context".into(), c.into()],
        None => Vec::new(),
    }
}

/// Resolve a short `cluster` alias (e.g. `"dev"`) against the kubeconfig context names. A context
/// whose name *contains* the alias (case-insensitive) matches; an exact name match wins outright.
/// A single match returns the concrete context name; zero matches or an ambiguous (>1) match return
/// a loud error — never a silent empty result (the s_251 failure: `"dev"` is not a real context, but
/// the op passed it literally to `kubectl --context` and kubectl failed, or — through the broker —
/// never set `context` at all). Returns `None` when no `cluster` alias was supplied (caller falls
/// back to the raw `context` field / the current context).
fn resolve_context_alias(input: &Value, host: &mut Host) -> Result<Option<String>, String> {
    let Some(alias) = opt_str(input, "cluster") else {
        return Ok(None);
    };
    let alias = alias.trim();
    if alias.is_empty() {
        return Ok(None);
    }
    let v = kubectl_json(host, &["config", "view"])?;
    let names: Vec<String> = v
        .get("contexts")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("name").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // Exact match wins outright (a caller who knows the full name is honored as-is).
    if names.iter().any(|n| n == alias) {
        return Ok(Some(alias.to_string()));
    }
    let alias_lc = alias.to_lowercase();
    let matches: Vec<&String> = names
        .iter()
        .filter(|n| n.to_lowercase().contains(&alias_lc))
        .collect();
    match matches.len() {
        0 => Err(format!(
            "cluster alias `{alias}` matched no kubeconfig context (contexts: {})",
            names.join(", ")
        )),
        1 => Ok(Some(matches[0].clone())),
        _ => {
            let matched: Vec<&str> = matches.iter().map(|s| s.as_str()).collect();
            Err(format!(
                "cluster alias `{alias}` is ambiguous — matched {} contexts: {}; refine the alias",
                matches.len(),
                matched.join(", ")
            ))
        }
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

/// Discover endpoints for a product as weak `EndpointCandidate`s (D-28). Each candidate is the flat
/// JSON `EndpointRef` (`id`/`url`/`product`/`protocol?`/`source:"discovered"`/`credential_ref?`/
/// `labels{}`) plus `score`/`reasons[]`. The host's broker deserializes these into
/// `flux_secret::endpoint::EndpointCandidate`. Never emits a secret value — only a credential
/// *reference* (`kubernetes/<ns>/<secret>/<key>`).
fn endpoint_discover(mut input: Value, host: &mut Host) -> Result<Value, String> {
    let product = opt_str(&input, "product").unwrap_or("").to_lowercase();
    let limit = input.get("limit").and_then(|x| x.as_u64()).unwrap_or(50) as usize;

    // `product == kubernetes` is the cluster-discovery path: one endpoint per kubeconfig context.
    if product == "kubernetes" {
        return discover_clusters(host, limit);
    }

    // Resolve a short `cluster` alias (e.g. "dev") to the concrete kubeconfig context once, then
    // inject it as `context` so every downstream `ctx_args` call targets the resolved cluster. A
    // multi-match / unknown alias is a loud error here rather than a silent empty result.
    if let Some(concrete) = resolve_context_alias(&input, host)? {
        input["context"] = json!(concrete);
    }

    let namespaces = resolve_namespaces(&input, host)?;
    let mut candidates: Vec<Value> = Vec::new();

    // Services + Ingresses matched to the product by name / `app.kubernetes.io/name`.
    discover_services(&input, host, &namespaces, &product, limit, &mut candidates)?;
    discover_ingresses(&input, host, &namespaces, &product, limit, &mut candidates)?;
    // postgres/mysql also resolve from connection Secrets (crossplane / RDS), with a credential ref.
    if product == "postgres" || product == "mysql" {
        discover_db_secrets(&input, host, &namespaces, &product, limit, &mut candidates)?;
    }

    candidates.truncate(limit);
    Ok(json!({ "candidates": candidates }))
}

/// Build a weak-reference candidate (flat `EndpointRef` + `score`/`reasons`). `credential_ref` is the
/// serialized `flux_secret::Ref` struct (`{scheme,plugin,instance,slot}`) or `Value::Null`.
#[allow(clippy::too_many_arguments)]
fn candidate(
    id: String,
    url: String,
    product: &str,
    protocol: Option<&str>,
    credential_ref: Value,
    labels: Value,
    score: f64,
    reasons: Vec<String>,
) -> Value {
    let mut obj = json!({
        "id": id,
        "url": url,
        "product": product,
        "source": "discovered",
        "labels": labels,
        "score": score,
        "reasons": reasons,
    });
    if let Some(p) = protocol {
        obj["protocol"] = json!(p);
    }
    if !credential_ref.is_null() {
        obj["credential_ref"] = credential_ref;
    }
    obj
}

/// A `flux_secret::Ref` (Kubernetes scheme) as serialized JSON: `kubernetes/<ns>/<name>/<key>` maps
/// to `{scheme:"kubernetes", plugin:<ns>, instance:<name>, slot:<key>}` (the host deserializes this
/// into a `Ref`). Defined inline (no flux-secret dep in the plugin sandbox).
fn k8s_credential_ref(namespace: &str, secret: &str, key: &str) -> Value {
    json!({
        "scheme": "kubernetes",
        "plugin": namespace,
        "instance": secret,
        "slot": key,
    })
}

/// One cluster endpoint per kubeconfig context, `url` = the context's cluster server.
fn discover_clusters(host: &mut Host, limit: usize) -> Result<Value, String> {
    let v = kubectl_json(host, &["config", "view"])?;
    // Map cluster name -> server URL so a context can resolve its server.
    let mut servers: HashMap<String, String> = HashMap::new();
    if let Some(clusters) = v.get("clusters").and_then(|x| x.as_array()) {
        for c in clusters {
            let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let server = c
                .pointer("/cluster/server")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if !name.is_empty() {
                servers.insert(name.to_string(), server.to_string());
            }
        }
    }
    let current = v
        .get("current-context")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let mut candidates: Vec<Value> = Vec::new();
    if let Some(contexts) = v.get("contexts").and_then(|x| x.as_array()) {
        for c in contexts {
            let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let cluster = c
                .pointer("/context/cluster")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let server = servers.get(cluster).cloned().unwrap_or_default();
            let is_current = name == current;
            candidates.push(candidate(
                format!("@endpoint/k8s-{name}"),
                server,
                "kubernetes",
                None,
                Value::Null,
                json!({ "context": name }),
                if is_current { 1.0 } else { 0.6 },
                vec![if is_current {
                    format!("kubeconfig context `{name}` (current)")
                } else {
                    format!("kubeconfig context `{name}`")
                }],
            ));
            if candidates.len() >= limit {
                break;
            }
        }
    }
    Ok(json!({ "candidates": candidates }))
}

/// Resolve the target namespaces. An explicit `namespace` wins (single). Otherwise, if `latest` is
/// requested (via `latest_namespace:true` or `query` containing "latest"), the single newest
/// namespace by `creationTimestamp`. Otherwise empty = "all namespaces" (the list ops use
/// `--all-namespaces`, so an empty vec means "do not restrict").
fn resolve_namespaces(input: &Value, host: &mut Host) -> Result<Vec<String>, String> {
    if let Some(ns) = opt_str(input, "namespace") {
        return Ok(vec![ns.to_string()]);
    }
    if wants_latest(input) {
        if let Some(ns) = latest_namespace(input, host)? {
            return Ok(vec![ns]);
        }
    }
    Ok(Vec::new())
}

/// Whether the caller asked for the "latest" namespace. Only the explicit `latest_namespace: true`
/// flag triggers the newest-namespace heuristic — a free-text `query` containing "latest" no longer
/// does (the s_251 ambiguity: "namespace=latest" meant a literal name but the substring heuristic
/// reinterpreted it as "newest namespace"). A literal namespace named `latest` is just
/// `namespace: "latest"`.
fn wants_latest(input: &Value) -> bool {
    input
        .get("latest_namespace")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

/// The namespace with the newest `metadata.creationTimestamp` (RFC3339, lexicographically sortable).
fn latest_namespace(input: &Value, host: &mut Host) -> Result<Option<String>, String> {
    let mut args = vec!["get".to_string(), "namespaces".to_string()];
    args.extend(ctx_args(input));
    let v = kubectl_json_v(host, &args)?;
    let mut best: Option<(String, String)> = None; // (timestamp, name)
    if let Some(items) = v.get("items").and_then(|x| x.as_array()) {
        for it in items {
            let name = it
                .pointer("/metadata/name")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let ts = it
                .pointer("/metadata/creationTimestamp")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if name.is_empty() {
                continue;
            }
            // RFC3339 timestamps sort lexicographically; keep the max.
            if best.as_ref().is_none_or(|(b, _)| ts > b.as_str()) {
                best = Some((ts.to_string(), name.to_string()));
            }
        }
    }
    Ok(best.map(|(_, name)| name))
}

/// Scope args for a discovery list call: `-n <ns>` for a single target namespace, else
/// `--all-namespaces`.
fn discover_scope(namespaces: &[String]) -> Vec<String> {
    match namespaces.first() {
        Some(ns) if namespaces.len() == 1 => vec!["-n".into(), ns.into()],
        _ => vec!["--all-namespaces".into()],
    }
}

/// Whether a resource (by name + `app.kubernetes.io/name` label) matches `product`. Returns the match
/// score: exact name or label == product is 1.0; a substring name match is 0.7; no match is 0.0.
fn product_match_score(name: &str, app_label: &str, product: &str) -> f64 {
    let n = name.to_lowercase();
    if product.is_empty() || n == product || app_label.eq_ignore_ascii_case(product) {
        if product.is_empty() {
            return 0.5; // no product filter: surface everything at a neutral score
        }
        return 1.0;
    }
    if n.contains(product) {
        return 0.7;
    }
    0.0
}

/// Match in-cluster Services to the product → one HTTP endpoint per service port.
fn discover_services(
    input: &Value,
    host: &mut Host,
    namespaces: &[String],
    product: &str,
    limit: usize,
    out: &mut Vec<Value>,
) -> Result<(), String> {
    let mut args = vec!["get".to_string(), "services".to_string()];
    args.extend(ctx_args(input));
    args.extend(discover_scope(namespaces));
    let v = kubectl_json_v(host, &args)?;
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return Ok(());
    };
    for it in items {
        if out.len() >= limit {
            break;
        }
        let name = it
            .pointer("/metadata/name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let app_label = it
            .pointer("/metadata/labels/app.kubernetes.io~1name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let score = product_match_score(name, app_label, product);
        if score == 0.0 {
            continue;
        }
        let ns = it
            .pointer("/metadata/namespace")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if let Some(ports) = it.pointer("/spec/ports").and_then(|x| x.as_array()) {
            for p in ports {
                if out.len() >= limit {
                    break;
                }
                let port = p.get("port").and_then(|x| x.as_i64()).unwrap_or(0);
                out.push(candidate(
                    format!("@endpoint/{ns}-{name}"),
                    format!("http://{name}.{ns}.svc.cluster.local:{port}"),
                    product,
                    Some("http"),
                    Value::Null,
                    json!({ "namespace": ns, "service": name }),
                    score,
                    vec![if app_label.eq_ignore_ascii_case(product) {
                        format!("service `{name}` labelled app.kubernetes.io/name={product}")
                    } else {
                        format!("service name `{name}` matches `{product}`")
                    }],
                ));
            }
        }
    }
    Ok(())
}

/// Match in-cluster Ingresses to the product → one endpoint per ingress host.
fn discover_ingresses(
    input: &Value,
    host: &mut Host,
    namespaces: &[String],
    product: &str,
    limit: usize,
    out: &mut Vec<Value>,
) -> Result<(), String> {
    let mut args = vec!["get".to_string(), "ingress".to_string()];
    args.extend(ctx_args(input));
    args.extend(discover_scope(namespaces));
    let v = kubectl_json_v(host, &args)?;
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return Ok(());
    };
    for it in items {
        if out.len() >= limit {
            break;
        }
        let name = it
            .pointer("/metadata/name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let app_label = it
            .pointer("/metadata/labels/app.kubernetes.io~1name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let score = product_match_score(name, app_label, product);
        if score == 0.0 {
            continue;
        }
        let ns = it
            .pointer("/metadata/namespace")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if let Some(rules) = it.pointer("/spec/rules").and_then(|x| x.as_array()) {
            for r in rules {
                if out.len() >= limit {
                    break;
                }
                let Some(host_name) = r.get("host").and_then(|h| h.as_str()) else {
                    continue;
                };
                if host_name.is_empty() {
                    continue;
                }
                out.push(candidate(
                    format!("@endpoint/{ns}-{name}"),
                    format!("https://{host_name}"),
                    product,
                    Some("http"),
                    Value::Null,
                    json!({ "namespace": ns, "service": name, "ingress": host_name }),
                    // Slightly below an exact service match: an ingress host is a coarser signal.
                    (score - 0.05).max(0.1),
                    vec![format!(
                        "ingress `{name}` host `{host_name}` matches `{product}`"
                    )],
                ));
            }
        }
    }
    Ok(())
}

/// Scan Secrets for a crossplane / RDS database connection pattern: a Secret carrying a host/endpoint
/// key plus a password-like key. Emits a `postgres`/`mysql` endpoint whose `url` is built from the
/// host/port keys and whose credential is a `kubernetes/<ns>/<secret>/<password-key>` REFERENCE — the
/// secret value itself is never read here.
fn discover_db_secrets(
    input: &Value,
    host: &mut Host,
    namespaces: &[String],
    product: &str,
    limit: usize,
    out: &mut Vec<Value>,
) -> Result<(), String> {
    let mut args = vec!["get".to_string(), "secrets".to_string()];
    args.extend(ctx_args(input));
    args.extend(discover_scope(namespaces));
    let v = kubectl_json_v(host, &args)?;
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return Ok(());
    };
    for it in items {
        if out.len() >= limit {
            break;
        }
        let name = it
            .pointer("/metadata/name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let ns = it
            .pointer("/metadata/namespace")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let Some(data) = it.get("data").and_then(|d| d.as_object()) else {
            continue;
        };
        // Keys are case-insensitive; build a lookup of present data keys (lowercase -> original).
        let keys: HashMap<String, String> =
            data.keys().map(|k| (k.to_lowercase(), k.clone())).collect();
        let find = |candidates: &[&str]| -> Option<String> {
            candidates.iter().find_map(|c| keys.get(*c).cloned())
        };
        // A connection secret needs a host/endpoint key AND a password-like key.
        let host_key = find(&["endpoint", "host", "hostname", "address", "server"]);
        let pass_key = find(&["password", "passwd", "pass"]);
        let (Some(host_key), Some(pass_key)) = (host_key, pass_key) else {
            continue;
        };
        // Crossplane connection secrets are the canonical case; a `connectionSecret`/`rds`-ish name is
        // a stronger signal but a host+password pair already qualifies.
        let lname = name.to_lowercase();
        let crossplane_ish = lname.contains("rds")
            || lname.contains("connection")
            || lname.contains("conn")
            || lname.contains(product);
        // Decode the host/port reference fields (these are non-secret connection coordinates — not the
        // password). They are still base64 in a Secret's `.data`, so decode just these two keys.
        let host_val = decode_secret_field(data, &host_key);
        let port_val = find(&["port"]).and_then(|k| decode_secret_field(data, &k));
        let scheme = product; // postgres / mysql
        let url = match (&host_val, &port_val) {
            (Some(h), Some(p)) if !h.is_empty() => format!("{scheme}://{h}:{p}"),
            (Some(h), None) if !h.is_empty() => format!("{scheme}://{h}"),
            _ => format!("{scheme}://{name}.{ns}"), // fall back to the secret coordinates
        };
        let reasons = vec![if crossplane_ish {
            format!("secret `{name}` matches a crossplane/RDS connection pattern ({host_key}+{pass_key})")
        } else {
            format!("secret `{name}` has connection keys {host_key}+{pass_key}")
        }];
        out.push(candidate(
            format!("@endpoint/{ns}-{name}"),
            url,
            product,
            Some(scheme),
            // The credential is a REFERENCE (location), never the value.
            k8s_credential_ref(ns, name, &pass_key),
            json!({ "namespace": ns, "secret": name }),
            if crossplane_ish { 0.95 } else { 0.6 },
            reasons,
        ));
    }
    Ok(())
}

/// Decode one base64 `.data` field of a Secret to a UTF-8 string (connection coordinates only — the
/// host/port, never surfaced as a credential). Returns `None` if absent or not decodable.
fn decode_secret_field(data: &Map<String, Value>, key: &str) -> Option<String> {
    let enc = data.get(key).and_then(|x| x.as_str())?;
    let bytes = b64_decode(enc).ok()?;
    Some(String::from_utf8_lossy(&bytes).trim().to_string())
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
    let mut v = kubectl_json_v(host, &args)?;
    if let Some(items) = v.get_mut("items").and_then(|x| x.as_array_mut()) {
        apply_query_limit(items, opt_str(&input, "query"), flex_i64(&input, "limit"));
    }
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
    let mut v = kubectl_json_v(host, &args)?;
    if let Some(items) = v.get_mut("items").and_then(|x| x.as_array_mut()) {
        apply_query_limit(items, opt_str(&input, "query"), flex_i64(&input, "limit"));
    }
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
    let mut v = kubectl_json_v(host, &args)?;
    if let Some(items) = v.get_mut("items").and_then(|x| x.as_array_mut()) {
        apply_query_limit(items, opt_str(&input, "query"), flex_i64(&input, "limit"));
    }
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
    let mut v = kubectl_json_v(host, &args)?;
    if let Some(items) = v.get_mut("items").and_then(|x| x.as_array_mut()) {
        apply_query_limit(items, opt_str(&input, "query"), flex_i64(&input, "limit"));
    }
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
    let mut v = kubectl_json_v(host, &args)?;
    if let Some(items) = v.get_mut("items").and_then(|x| x.as_array_mut()) {
        apply_query_limit(items, opt_str(&input, "query"), flex_i64(&input, "limit"));
    }
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
    let mut containers = containers_from_pods(&v);
    apply_query_limit(
        &mut containers,
        opt_str(&input, "query"),
        flex_i64(&input, "limit"),
    );
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
    let until = opt_str(&input, "until");
    if until.is_some() {
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
    let keep_timestamps = input
        .get("timestamps")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let lines: Vec<String> = filter_log_lines(&out.stdout, until, keep_timestamps)?;
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
    let mut v = kubectl_json_v(host, &args)?;
    if let Some(items) = v.get_mut("items").and_then(|x| x.as_array_mut()) {
        apply_query_limit(items, opt_str(&input, "query"), flex_i64(&input, "limit"));
    }
    Ok(v)
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

    // Capture the previous replica count before scaling (fluxplane parity).
    let previous_replicas = fetch_deployment_replicas(host, name, ns, &input)?;

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
        "previous_replicas": previous_replicas,
        "replicas": replicas,
        "output": out.stdout.trim(),
    }))
}

fn fetch_deployment_replicas(
    host: &mut Host,
    name: &str,
    ns: &str,
    input: &Value,
) -> Result<i64, String> {
    let mut argv: Vec<String> = vec![
        "kubectl".into(),
        "get".into(),
        format!("deployment/{name}"),
        "-n".into(),
        ns.into(),
    ];
    argv.extend(ctx_args(input));
    argv.push("-o".into());
    argv.push("json".into());
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = host.run(&refs, 30)?;
    if out.exit_code != 0 {
        // Scaling itself will fail if the deployment does not exist; don't hard-fail here.
        return Ok(0);
    }
    let v: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
    Ok(v.pointer("/spec/replicas")
        .and_then(|x| x.as_i64())
        .unwrap_or(0))
}

fn deployment_restart(input: Value, host: &mut Host) -> Result<Value, String> {
    let ns = req_nonempty(&input, "namespace")?;
    let name = req_nonempty(&input, "name")?;
    let restarted_at = now_rfc3339();
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
        "restarted_at": restarted_at,
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
    let duration_seconds = input
        .get("duration_seconds")
        .and_then(|x| x.as_i64())
        .map(|d| if d <= 0 { 3600 } else { d.min(28800) })
        .unwrap_or(3600);

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
    let started_at = now_rfc3339();
    let expires_at = format_rfc3339(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            + duration_seconds,
    );
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
        "duration_seconds": duration_seconds,
        "started_at": started_at,
        "expires_at": expires_at,
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

    /// Serialize port-forward tests that touch the shared `FORWARDS` registry. The registry is
    /// global state, and tests that `clear_forwards()` at start/end can otherwise race when run
    /// in parallel by the test harness.
    static PF_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Standard-alphabet base64 (test-only; mirrors what kubectl emits for a Secret's `.data`).
    fn b64_encode(bytes: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(A[((n >> 18) & 63) as usize] as char);
            out.push(A[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                A[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                A[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        out
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
    fn services_become_product_endpoints() {
        // A service matching the product becomes an EndpointCandidate (flat EndpointRef shape).
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_process(
                "get services -n monitoring",
                r#"{"items":[{"metadata":{"name":"prometheus","namespace":"monitoring"},"spec":{"type":"ClusterIP","ports":[{"port":9090}]}}]}"#,
            )
            // Ingress lookup runs too (no matches here).
            .with_process("get ingress -n monitoring", r#"{"items":[]}"#);
        let out = plugin
            .call(
                "kubernetes.endpoint.discover",
                json!({ "namespace": "monitoring", "product": "prometheus" }),
                &mut host,
            )
            .unwrap();
        let c = &out["candidates"][0];
        assert_eq!(c["id"], "@endpoint/monitoring-prometheus");
        assert_eq!(c["product"], "prometheus");
        assert_eq!(c["protocol"], "http");
        assert_eq!(c["source"], "discovered");
        assert_eq!(
            c["url"],
            "http://prometheus.monitoring.svc.cluster.local:9090"
        );
        assert_eq!(c["labels"]["namespace"], "monitoring");
        assert_eq!(c["labels"]["service"], "prometheus");
        assert_eq!(c["score"], 1.0); // exact name match
                                     // No credential ref for a plain service endpoint.
        assert!(c.get("credential_ref").is_none());
    }

    #[test]
    fn contexts_become_cluster_endpoints() {
        // product=kubernetes → one endpoint per kubeconfig context, url = the cluster server.
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "config view",
            r#"{"current-context":"prod","clusters":[
                {"name":"c-prod","cluster":{"server":"https://prod.example:6443"}},
                {"name":"c-dev","cluster":{"server":"https://dev.example:6443"}}],
              "contexts":[
                {"name":"prod","context":{"cluster":"c-prod"}},
                {"name":"dev","context":{"cluster":"c-dev"}}]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.endpoint.discover",
                json!({ "product": "kubernetes" }),
                &mut host,
            )
            .unwrap();
        let cands = out["candidates"].as_array().unwrap();
        assert_eq!(cands.len(), 2);
        let prod = cands
            .iter()
            .find(|c| c["id"] == "@endpoint/k8s-prod")
            .unwrap();
        assert_eq!(prod["product"], "kubernetes");
        assert_eq!(prod["url"], "https://prod.example:6443");
        assert_eq!(prod["labels"]["context"], "prod");
        assert_eq!(prod["score"], 1.0); // current context scores highest
        let dev = cands
            .iter()
            .find(|c| c["id"] == "@endpoint/k8s-dev")
            .unwrap();
        assert_eq!(dev["score"], 0.6);
    }

    #[test]
    fn rds_secret_becomes_credential_ref() {
        // A crossplane/RDS connection secret (host+password keys) becomes a postgres endpoint whose
        // credential is a `kubernetes/...` REFERENCE — and NO password value is in the candidate.
        let plugin = manifest_builder().build();
        // base64: "orders.abc.rds.amazonaws.com" / "5432" / "s3cr3t-pw"
        let host_b64 = b64_encode(b"orders.abc.rds.amazonaws.com");
        let port_b64 = b64_encode(b"5432");
        let pw_b64 = b64_encode(b"s3cr3t-pw");
        let secrets_json = format!(
            r#"{{"items":[{{"metadata":{{"name":"orders-rds-conn","namespace":"team-orders"}},"data":{{"endpoint":"{host_b64}","port":"{port_b64}","username":"{port_b64}","password":"{pw_b64}"}}}}]}}"#
        );
        let mut host = MockHost::default()
            .with_process("get services -n team-orders", r#"{"items":[]}"#)
            .with_process("get ingress -n team-orders", r#"{"items":[]}"#)
            .with_process("get secrets -n team-orders", &secrets_json);
        let out = plugin
            .call(
                "kubernetes.endpoint.discover",
                json!({ "namespace": "team-orders", "product": "postgres" }),
                &mut host,
            )
            .unwrap();
        let c = &out["candidates"][0];
        assert_eq!(c["id"], "@endpoint/team-orders-orders-rds-conn");
        assert_eq!(c["product"], "postgres");
        assert_eq!(c["protocol"], "postgres");
        assert_eq!(c["url"], "postgres://orders.abc.rds.amazonaws.com:5432");
        // The credential is a REFERENCE (a location), never a value.
        assert_eq!(c["credential_ref"]["scheme"], "kubernetes");
        assert_eq!(c["credential_ref"]["plugin"], "team-orders");
        assert_eq!(c["credential_ref"]["instance"], "orders-rds-conn");
        assert_eq!(c["credential_ref"]["slot"], "password");
        // Critically: the decoded password is NOWHERE in the candidate JSON.
        let serialized = serde_json::to_string(c).unwrap();
        assert!(
            !serialized.contains("s3cr3t-pw"),
            "candidate must never carry the password value: {serialized}"
        );
    }

    #[test]
    fn endpoint_discover_selects_latest_namespace() {
        // With `latest_namespace: true` and no explicit namespace, the newest namespace by
        // creationTimestamp is chosen, then matched services in THAT namespace become candidates.
        // (The free-text `query` no longer triggers this heuristic — see
        // `literal_latest_namespace_preferred_over_heuristic`.)
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_process(
                "get namespaces",
                r#"{"items":[
                    {"metadata":{"name":"team-old","creationTimestamp":"2024-01-01T00:00:00Z"}},
                    {"metadata":{"name":"team-new","creationTimestamp":"2026-06-01T00:00:00Z"}}]}"#,
            )
            .with_process(
                "get services -n team-new",
                r#"{"items":[{"metadata":{"name":"postgres","namespace":"team-new"},"spec":{"ports":[{"port":5432}]}}]}"#,
            )
            .with_process("get ingress -n team-new", r#"{"items":[]}"#)
            .with_process("get secrets -n team-new", r#"{"items":[]}"#);
        let out = plugin
            .call(
                "kubernetes.endpoint.discover",
                json!({ "product": "postgres", "latest_namespace": true }),
                &mut host,
            )
            .unwrap();
        let c = &out["candidates"][0];
        assert_eq!(c["id"], "@endpoint/team-new-postgres");
        assert_eq!(c["labels"]["namespace"], "team-new");
    }

    /// A short `cluster` alias resolves to the concrete kubeconfig context and discovery runs
    /// against *that* cluster — not the current one. The s_251 failure: `"dev"` is not a real
    /// kubeconfig context (the real ones are long ARN-like names), so the op either passed it
    /// literally to `kubectl --context` (→ kubectl error) or — through the broker — never set
    /// `context` at all and queried the current cluster. Today `cluster` is ignored, so discovery
    /// runs against the *current* context (prod here) and finds nothing.
    #[test]
    fn cluster_alias_resolves_to_concrete_context() {
        let plugin = manifest_builder().build();
        // kubeconfig: `dev-eu` and `prod-eu` contexts; `prod-eu` is current (mirrors the ARN case
        // from s_251, shortened for readability — the resolution logic is substring-based either way).
        let mut host = MockHost::default().with_process(
            "config view",
            r#"{"current-context":"prod-eu","contexts":[
                {"name":"dev-eu","context":{"cluster":"c-dev","user":"u"}},
                {"name":"prod-eu","context":{"cluster":"c-prod","user":"u"}}]}"#,
        );
        // The resolved-dev-context calls must return the backend postgres service; the no-context
        // (current/prod) calls return nothing. MockHost matches the first registered substring, so
        // register the dev-context-specific entries before the bare fallbacks.
        host = host
            .with_process(
                "get services --context dev-eu",
                r#"{"items":[{"metadata":{"name":"postgres","namespace":"backend"},"spec":{"ports":[{"port":5432}]}}]}"#,
            )
            .with_process("get ingress --context dev-eu", r#"{"items":[]}"#)
            .with_process("get secrets --context dev-eu", r#"{"items":[]}"#)
            // Fallbacks for the current-context (prod) path — today's behavior.
            .with_process("get services", r#"{"items":[]}"#)
            .with_process("get ingress", r#"{"items":[]}"#)
            .with_process("get secrets", r#"{"items":[]}"#);
        let out = plugin
            .call(
                "kubernetes.endpoint.discover",
                json!({ "product": "postgres", "cluster": "dev" }),
                &mut host,
            )
            .unwrap();
        let cands = out["candidates"].as_array().unwrap();
        assert!(
            !cands.is_empty(),
            "cluster=`dev` resolves to the dev-eu context and finds the backend postgres"
        );
        let c = &cands[0];
        assert_eq!(c["id"], "@endpoint/backend-postgres");
        assert_eq!(c["labels"]["namespace"], "backend");
    }

    /// A literal namespace named `latest` is found instead of the newest-namespace heuristic
    /// misfiring on the word "latest". The s_251 ambiguity: the user wrote `namespace=latest` meaning
    /// a literal namespace, but the free-text `query` substring heuristic reinterpreted it as "newest
    /// namespace by creation time" and searched the wrong namespace. After retiring the substring
    /// heuristic, `query` no longer triggers it — a literal `latest` namespace's endpoints surface
    /// instead of being silently dropped.
    #[test]
    fn literal_latest_namespace_preferred_over_heuristic() {
        let plugin = manifest_builder().build();
        // `latest` is an OLDER namespace that nonetheless holds the backend postgres service; the
        // newer `team-new` namespace has none. Today the substring heuristic picks `team-new` (newest)
        // and finds nothing — the literal `latest` namespace is never searched.
        let mut host = MockHost::default()
            // The heuristic path (today) lists namespaces and picks team-new.
            .with_process(
                "get namespaces",
                r#"{"items":[
                    {"metadata":{"name":"latest","creationTimestamp":"2024-01-01T00:00:00Z"}},
                    {"metadata":{"name":"team-new","creationTimestamp":"2026-06-01T00:00:00Z"}}]}"#,
            )
            // Today's heuristic searches team-new (empty).
            .with_process("get services -n team-new", r#"{"items":[]}"#)
            .with_process("get ingress -n team-new", r#"{"items":[]}"#)
            .with_process("get secrets -n team-new", r#"{"items":[]}"#)
            // After retiring the substring heuristic, `query` no longer sets a namespace, so discovery
            // is `--all-namespaces` and the postgres in the literal `latest` namespace surfaces.
            .with_process(
                "get services --all-namespaces",
                r#"{"items":[{"metadata":{"name":"postgres","namespace":"latest"},"spec":{"ports":[{"port":5432}]}}]}"#,
            )
            .with_process("get ingress --all-namespaces", r#"{"items":[]}"#)
            .with_process("get secrets --all-namespaces", r#"{"items":[]}"#);
        let out = plugin
            .call(
                "kubernetes.endpoint.discover",
                json!({ "product": "postgres", "query": "latest backend" }),
                &mut host,
            )
            .unwrap();
        let cands = out["candidates"].as_array().unwrap();
        assert!(
            !cands.is_empty(),
            "the literal `latest` namespace's endpoint surfaces (not the heuristic's newest)"
        );
        let c = &cands[0];
        assert_eq!(c["id"], "@endpoint/latest-postgres");
        assert_eq!(c["labels"]["namespace"], "latest");
    }

    #[test]
    fn declares_discovery_products() {
        let m = manifest_builder().build().manifest();
        for p in [
            "kubernetes",
            "prometheus",
            "loki",
            "grafana",
            "alertmanager",
            "postgres",
            "mysql",
        ] {
            assert!(
                m.discovers.iter().any(|d| d == p),
                "manifest should declare discovery product `{p}`"
            );
        }
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
    fn service_list_filters_by_query_and_limit() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get services --all-namespaces",
            r#"{"items":[
                {"metadata":{"name":"api","namespace":"prod"}},
                {"metadata":{"name":"billing","namespace":"prod"}},
                {"metadata":{"name":"api-canary","namespace":"prod"}}
            ]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.service.list",
                json!({ "query": "api", "limit": 1 }),
                &mut host,
            )
            .unwrap();
        let items = out["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0]["metadata"]["name"]
            .as_str()
            .unwrap()
            .contains("api"));
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
    fn pod_logs_filters_until_bound_and_strips_timestamps() {
        let plugin = manifest_builder().build();
        let logs = "2024-01-01T00:00:00Z line1\n2024-01-01T00:00:02Z line2\n";
        // `--timestamps` is forced when `until` is provided, so the canned command substring still
        // matches "logs -n prod api-1".
        let mut host = MockHost::default().with_process("logs -n prod api-1", logs);
        let out = plugin
            .call(
                "kubernetes.pod.logs",
                json!({ "namespace": "prod", "name": "api-1", "until": "2024-01-01T00:00:01Z" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["line_count"], 1);
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
        let mut host = MockHost::default()
            .with_process(
                "get deployment/web -n prod",
                r#"{"spec":{"replicas":2},"status":{}}"#,
            )
            .with_process(
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
        assert_eq!(out["previous_replicas"], 2);
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
        assert!(
            out["restarted_at"].as_str().unwrap().starts_with("20"),
            "restarted_at should be an RFC3339 timestamp"
        );
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
    fn node_list_filters_by_query_and_limit() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_process(
            "get nodes",
            r#"{"items":[
                {"metadata":{"name":"node-1"}},
                {"metadata":{"name":"node-2"}},
                {"metadata":{"name":"worker-3"}}
            ]}"#,
        );
        let out = plugin
            .call(
                "kubernetes.node.list",
                json!({ "query": "node", "limit": 2 }),
                &mut host,
            )
            .unwrap();
        let items = out["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .all(|n| n["metadata"]["name"].as_str().unwrap().contains("node")));
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
        let _guard = PF_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = PF_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn portforward_start_honors_duration_seconds() {
        let _guard = PF_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_forwards();
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_spawn(88).with_proc_output(
            "Forwarding from 127.0.0.1:8080 -> 80\n",
            "",
            true,
        );
        let start = plugin
            .call(
                "kubernetes.portforward.start",
                json!({ "namespace": "prod", "resource": "service/web", "remote_port": 80, "duration_seconds": 1800 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(start["duration_seconds"], 1800);
        assert!(
            start["expires_at"].as_str().unwrap().starts_with("20"),
            "expires_at should be an RFC3339 timestamp"
        );
        clear_forwards();
    }

    #[test]
    fn portforward_start_errors_when_kubectl_exits_before_ready() {
        // A process that exits without ever printing the readiness line is a startup failure.
        let _guard = PF_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

// ===========================================================================
// D-36: schema-derivation contract test (kubernetes).
// Locks each op's derived schemars schema to its intended field/required/type
// contract after the migration + gap-ports.
// ===========================================================================
#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        ArrayStr,
        Enum(Vec<String>),
    }

    #[derive(Clone)]
    struct Prop {
        name: &'static str,
        kind: Kind,
    }

    struct OpContract {
        props: Vec<Prop>,
        required: Vec<&'static str>,
    }

    fn p(name: &'static str, kind: Kind) -> Prop {
        Prop { name, kind }
    }

    fn c(props: Vec<Prop>, required: Vec<&'static str>) -> OpContract {
        OpContract { props, required }
    }

    fn inventory_list_contract() -> OpContract {
        c(
            vec![
                p("context", Kind::Str),
                p("namespace", Kind::Str),
                p("query", Kind::Str),
                p("limit", Kind::Int),
            ],
            vec![],
        )
    }

    fn inventory_show_contract() -> OpContract {
        c(
            vec![
                p("context", Kind::Str),
                p("namespace", Kind::Str),
                p("name", Kind::Str),
            ],
            vec!["name"],
        )
    }

    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            ("kubernetes.cluster.list", c(vec![], vec![])),
            ("kubernetes.test", c(vec![p("context", Kind::Str)], vec![])),
            (
                "kubernetes.endpoint.discover",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("cluster", Kind::Str),
                        p("namespace", Kind::Str),
                        p("product", Kind::Str),
                        p("query", Kind::Str),
                        p("latest_namespace", Kind::Bool),
                        p("limit", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "kubernetes.secret.read",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("namespace", Kind::Str),
                        p("name", Kind::Str),
                        p("keys", Kind::ArrayStr),
                    ],
                    vec!["namespace", "name"],
                ),
            ),
            ("kubernetes.namespace.list", inventory_list_contract()),
            ("kubernetes.service.list", inventory_list_contract()),
            ("kubernetes.service.show", inventory_show_contract()),
            ("kubernetes.pod.list", inventory_list_contract()),
            ("kubernetes.pod.show", inventory_show_contract()),
            (
                "kubernetes.pod.logs",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("namespace", Kind::Str),
                        p("name", Kind::Str),
                        p("selector", Kind::Str),
                        p("container", Kind::Str),
                        p("tail_lines", Kind::Int),
                        p("limit_bytes", Kind::Int),
                        p("since", Kind::Str),
                        p("until", Kind::Str),
                        p("previous", Kind::Bool),
                        p("timestamps", Kind::Bool),
                    ],
                    vec!["namespace"],
                ),
            ),
            (
                "kubernetes.portforward.start",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("namespace", Kind::Str),
                        p("resource", Kind::Str),
                        p("name", Kind::Str),
                        p(
                            "resource_type",
                            Kind::Enum(vec!["service".into(), "pod".into(), "deployment".into()]),
                        ),
                        p("remote_port", Kind::Int),
                        p("local_port", Kind::Int),
                        p("address", Kind::Str),
                        p("duration_seconds", Kind::Int),
                    ],
                    vec!["namespace", "remote_port"],
                ),
            ),
            (
                "kubernetes.portforward.stop",
                c(
                    vec![
                        p("id", Kind::Str),
                        p("process_group", Kind::Int),
                        p("pid", Kind::Int),
                    ],
                    vec!["id"],
                ),
            ),
            (
                "kubernetes.portforward.list",
                c(
                    vec![
                        p("namespace", Kind::Str),
                        p("context", Kind::Str),
                        p("live", Kind::Bool),
                    ],
                    vec![],
                ),
            ),
            ("kubernetes.deployment.list", inventory_list_contract()),
            ("kubernetes.deployment.show", inventory_show_contract()),
            (
                "kubernetes.deployment.history",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("namespace", Kind::Str),
                        p("name", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec!["name"],
                ),
            ),
            (
                "kubernetes.deployment.scale",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("namespace", Kind::Str),
                        p("name", Kind::Str),
                        p("replicas", Kind::Int),
                    ],
                    vec!["namespace", "name", "replicas"],
                ),
            ),
            (
                "kubernetes.deployment.restart",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("namespace", Kind::Str),
                        p("name", Kind::Str),
                    ],
                    vec!["namespace", "name"],
                ),
            ),
            ("kubernetes.ingress.list", inventory_list_contract()),
            ("kubernetes.container.list", inventory_list_contract()),
            ("kubernetes.container.show", inventory_show_contract()),
            (
                "kubernetes.event.list",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("namespace", Kind::Str),
                        p("name", Kind::Str),
                        p("kind", Kind::Str),
                        p("warnings_only", Kind::Bool),
                        p("limit", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "kubernetes.node.list",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("query", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec![],
                ),
            ),
            (
                "kubernetes.pod.exec",
                c(
                    vec![
                        p("context", Kind::Str),
                        p("namespace", Kind::Str),
                        p("name", Kind::Str),
                        p("container", Kind::Str),
                        p("command", Kind::ArrayStr),
                        p("timeout_seconds", Kind::Int),
                    ],
                    vec!["namespace", "name", "command"],
                ),
            ),
        ]
    }

    fn resolve<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
        if let Some(obj) = node.as_object() {
            if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
                if let Some(name) = r.strip_prefix("#/definitions/") {
                    return defs.get(name).unwrap_or(node);
                }
            }
            if let Some(any) = obj.get("anyOf").and_then(|v| v.as_array()) {
                for m in any {
                    if m.get("type").and_then(|v| v.as_str()) != Some("null") {
                        return resolve(m, defs);
                    }
                }
            }
            if let Some(one) = obj.get("oneOf").and_then(|v| v.as_array()) {
                for m in one {
                    if m.get("type").and_then(|v| v.as_str()) != Some("null") {
                        return resolve(m, defs);
                    }
                }
            }
        }
        node
    }

    fn kind_of(node: &Value) -> Kind {
        let t = node.get("type");
        if let Some(arr) = t.and_then(|v| v.as_array()) {
            let first = arr
                .iter()
                .find(|v| v.as_str() != Some("null"))
                .and_then(|v| v.as_str())
                .unwrap_or("null");
            return base_kind(first, node);
        }
        base_kind(t.and_then(|v| v.as_str()).unwrap_or(""), node)
    }

    fn base_kind(t: &str, node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "boolean" => Kind::Bool,
            "string" => {
                if let Some(e) = node.get("enum").and_then(|v| v.as_array()) {
                    Kind::Enum(
                        e.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect(),
                    )
                } else {
                    Kind::Str
                }
            }
            "array" => Kind::ArrayStr,
            other => panic!("unsupported property type: {other} ({node})"),
        }
    }

    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        assert_eq!(schema["type"], "object", "{op_name}: root type");
        let defs = schema.get("definitions").cloned().unwrap_or(json!({}));
        let props_obj = schema.get("properties").and_then(|v| v.as_object());
        let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
        if let Some(props) = props_obj {
            for (k, v) in props {
                let resolved = resolve(v, &defs);
                got.insert(k.as_str(), kind_of(resolved));
            }
        }
        let want: BTreeMap<&str, Kind> = contract
            .props
            .iter()
            .map(|Prop { name, kind }| (*name, kind.clone()))
            .collect();
        assert_eq!(got.len(), want.len(), "{op_name}: property count");
        for Prop { name, kind } in &contract.props {
            let got_kind = got
                .get(*name)
                .unwrap_or_else(|| panic!("{op_name}: missing property `{name}`"));
            assert_eq!(got_kind, kind, "{op_name}: property `{name}` kind");
        }
        let mut req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        req.sort();
        let mut want_req = contract.required.clone();
        want_req.sort();
        assert_eq!(req, want_req, "{op_name}: required set");
    }

    #[test]
    fn derived_schemas_match_contract() {
        let ops = contracts();
        let manifest = manifest_builder().build().manifest();
        let by_name: BTreeMap<&str, &OperationSpec> = manifest
            .operations
            .iter()
            .map(|o| (o.name.as_str(), o))
            .collect();
        assert_eq!(by_name.len(), ops.len(), "op count changed");
        for (name, contract) in &ops {
            let spec = by_name
                .get(*name)
                .unwrap_or_else(|| panic!("missing op {name}"));
            assert_contract(name, &spec.input_schema, contract);
        }
    }
}
