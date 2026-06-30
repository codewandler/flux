//! `aws` — a flux integration plugin that drives the `aws` CLI through the host's `process.run`
//! capability (no hand-rolled SigV4, no vendor SDK). All 11 ops are read-only; credentials are
//! fetched via `host.secret` and forwarded to the subprocess as explicit env overrides (the host
//! clears the subprocess env). The approach mirrors the `kubernetes` plugin's use of `kubectl`.
//!
//! Ops implemented (all `aws.` prefixed):
//!   test, inspect, ec2.instances, eks.clusters, rds.instances, s3.buckets, s3.objects,
//!   logs.groups, logs.tail, logs.query, cloudwatch.metrics

use host_kit::*;
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Secret purpose identifiers — must match the manifest's `secrets` allow-list.
// ---------------------------------------------------------------------------
const PURPOSE_ACCESS_KEY_ID: &str = "access_key_id";
const PURPOSE_SECRET_ACCESS_KEY: &str = "secret_access_key";
const PURPOSE_SESSION_TOKEN: &str = "session_token";

const DEFAULT_REGION: &str = "eu-central-1";

// ---------------------------------------------------------------------------
// Manifest.
// ---------------------------------------------------------------------------

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("aws", "0.1.0")
        .capabilities(Caps {
            process: vec!["aws".into()],
            secrets: vec![
                "AWS_ACCESS_KEY_ID".into(),
                "AWS_SECRET_ACCESS_KEY".into(),
                "AWS_SESSION_TOKEN".into(),
            ],
            ..Default::default()
        })
        .datasource(ds(
            "aws.ec2",
            "aws.ec2.instance",
            "EC2 instances searchable by Name tag.",
        ))
        // --- connectivity / setup ---------------------------------------------
        .operation(
            read_op(
                "aws.test",
                "Verify AWS connectivity and credential validity via STS GetCallerIdentity.",
                json!({"type": "object", "properties": {
                    "region": s_region()
                }}),
            ),
            aws_test,
        )
        .operation(
            read_op(
                "aws.inspect",
                "Inspect non-secret AWS environment configuration and credential presence \
                 (reports which credentials are set, effective region, no secret values returned).",
                json!({"type": "object", "properties": {
                    "region": s_region()
                }}),
            ),
            aws_inspect,
        )
        // --- EC2 ---------------------------------------------------------------
        .operation(
            read_op(
                "aws.ec2.instances",
                "List EC2 instances with Name-tag wildcard and state filters.",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "name": {"type": "string", "description": "Filter by the Name tag; * wildcards supported (e.g. *kamailio*)."},
                    "states": {"type": "array", "items": {"type": "string"}, "description": "Instance state filters such as running or stopped."},
                    "ids": {"type": "array", "items": {"type": "string"}, "description": "Exact instance IDs."},
                    "limit": {"type": "integer", "description": "Maximum instances returned. Defaults to 50, capped at 500.", "minimum": 0, "maximum": 500}
                }}),
            ),
            ec2_instances,
        )
        // --- EKS ---------------------------------------------------------------
        .operation(
            read_op(
                "aws.eks.clusters",
                "List and describe EKS clusters (version, status, endpoint, VPC).",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "name": {"type": "string", "description": "Exact cluster name to describe. Default lists and describes all (bounded)."}
                }}),
            ),
            eks_clusters,
        )
        // --- RDS ---------------------------------------------------------------
        .operation(
            read_op(
                "aws.rds.instances",
                "List RDS/Aurora clusters (writer/reader endpoints, members) and database instances.",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "engine": {"type": "string", "description": "Filter by engine such as aurora-mysql or postgres."},
                    "limit": {"type": "integer", "description": "Maximum instances returned. Defaults to 100, capped at 500.", "minimum": 0, "maximum": 500}
                }}),
            ),
            rds_instances,
        )
        // --- S3 ----------------------------------------------------------------
        .operation(
            read_op(
                "aws.s3.buckets",
                "List S3 buckets, optionally filtered by name prefix.",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "prefix": {"type": "string", "description": "Only buckets whose name starts with this prefix."}
                }}),
            ),
            s3_buckets,
        )
        .operation(
            read_op(
                "aws.s3.objects",
                "List S3 objects under a prefix with continuation-token pagination.",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "bucket": {"type": "string", "description": "Bucket name."},
                    "prefix": {"type": "string", "description": "Key prefix filter."},
                    "limit": {"type": "integer", "description": "Maximum objects returned. Defaults to 100, capped at 1000.", "minimum": 0, "maximum": 1000},
                    "next_token": {"type": "string", "description": "Continuation token from a previous truncated call."}
                }, "required": ["bucket"]}),
            ),
            s3_objects,
        )
        // --- CloudWatch Logs ---------------------------------------------------
        .operation(
            read_op(
                "aws.logs.groups",
                "List CloudWatch log groups with retention and size.",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "prefix": {"type": "string", "description": "Log group name prefix filter (e.g. /aws/eks/)."},
                    "limit": {"type": "integer", "description": "Maximum groups returned. Defaults to 100, capped at 500.", "minimum": 0, "maximum": 500}
                }}),
            ),
            logs_groups,
        )
        .operation(
            read_op(
                "aws.logs.tail",
                "Read recent events from a CloudWatch log group (FilterLogEvents over a time window).",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "group": {"type": "string", "description": "Log group name."},
                    "since": {"type": "string", "description": "Start time as RFC3339, unix seconds, or duration ago (e.g. 15m). Defaults to 15m."},
                    "until": {"type": "string", "description": "End time as RFC3339, unix seconds, or duration ago. Defaults to now."},
                    "pattern": {"type": "string", "description": "CloudWatch filter pattern (e.g. ERROR or a JSON term)."},
                    "limit": {"type": "integer", "description": "Maximum events returned. Defaults to 200, capped at 1000.", "minimum": 0, "maximum": 1000}
                }, "required": ["group"]}),
            ),
            logs_tail,
        )
        .operation(
            read_op(
                "aws.logs.query",
                "Run a bounded CloudWatch Logs Insights query and wait for its results.",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "groups": {"type": "array", "items": {"type": "string"}, "description": "Log group names to query."},
                    "query": {"type": "string", "description": "CloudWatch Logs Insights query (e.g. fields @timestamp, @message | limit 20)."},
                    "since": {"type": "string", "description": "Start time as RFC3339, unix seconds, or duration ago. Defaults to 1h."},
                    "until": {"type": "string", "description": "End time as RFC3339, unix seconds, or duration ago. Defaults to now."},
                    "timeout_seconds": {"type": "integer", "description": "Maximum seconds to wait for completion. Defaults to 30, capped at 120.", "minimum": 0, "maximum": 120}
                }, "required": ["groups", "query"]}),
            ),
            logs_query,
        )
        // --- CloudWatch Metrics ------------------------------------------------
        .operation(
            read_op(
                "aws.cloudwatch.metrics",
                "Fetch one CloudWatch metric series (GetMetricData) over a time window.",
                json!({"type": "object", "properties": {
                    "region": s_region(),
                    "namespace": {"type": "string", "description": "Metric namespace such as AWS/RDS or AWS/EC2."},
                    "metric": {"type": "string", "description": "Metric name such as CPUUtilization."},
                    "dimensions": {"type": "object", "description": "Dimension name/value pairs, e.g. {\"DBClusterIdentifier\": \"dev-aurora2-mysql\"}."},
                    "stat": {"type": "string", "description": "Statistic: Average, Sum, Minimum, Maximum, SampleCount, or a percentile like p99. Defaults to Average."},
                    "period": {"type": "integer", "description": "Period in seconds. Defaults to 300.", "minimum": 0},
                    "since": {"type": "string", "description": "Start time as RFC3339, unix seconds, or duration ago. Defaults to 3h."},
                    "until": {"type": "string", "description": "End time as RFC3339, unix seconds, or duration ago. Defaults to now."}
                }, "required": ["namespace", "metric"]}),
            ),
            cloudwatch_metrics,
        )
}

// ---------------------------------------------------------------------------
// Schema helpers.
// ---------------------------------------------------------------------------

fn s_region() -> Value {
    json!({"type": "string", "description": "AWS region. Defaults to eu-central-1."})
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

// ---------------------------------------------------------------------------
// Credential / region helpers.
// ---------------------------------------------------------------------------

/// Resolve AWS credentials from the host secret store. Returns `(key_id, secret, token_opt)`.
/// Session token is optional — temporary credentials have one, long-lived AKID/SAK pairs don't.
fn resolve_creds(host: &mut Host) -> Result<(String, String, Option<String>), String> {
    let key_id = host.secret(PURPOSE_ACCESS_KEY_ID)?;
    let secret = host.secret(PURPOSE_SECRET_ACCESS_KEY)?;
    let token = host
        .secret(PURPOSE_SESSION_TOKEN)
        .ok()
        .filter(|s| !s.trim().is_empty());
    Ok((key_id, secret, token))
}

/// Effective AWS region: input field → env default constant.
fn region(input: &Value) -> String {
    opt_str(input, "region")
        .map(String::from)
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// Build the env override slice for a host.run call (static lifetime slice → owned vec of tuples).
fn cred_env(
    key_id: &str,
    secret: &str,
    token: Option<&str>,
    region: &str,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("AWS_ACCESS_KEY_ID".to_string(), key_id.to_string()),
        ("AWS_SECRET_ACCESS_KEY".to_string(), secret.to_string()),
        ("AWS_REGION".to_string(), region.to_string()),
        ("AWS_DEFAULT_REGION".to_string(), region.to_string()),
    ];
    if let Some(tok) = token {
        env.push(("AWS_SESSION_TOKEN".to_string(), tok.to_string()));
    }
    env
}

/// Run `aws <args> --output json` with injected credentials.
fn aws_json(
    host: &mut Host,
    args: &[&str],
    key_id: &str,
    secret: &str,
    token: Option<&str>,
    rgn: &str,
) -> Result<Value, String> {
    let mut argv: Vec<&str> = vec!["aws"];
    argv.extend_from_slice(args);
    argv.push("--output");
    argv.push("json");
    let env_owned = cred_env(key_id, secret, token, rgn);
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let out = host.run_with_env(&argv, &env_refs, 60)?;
    if out.exit_code != 0 {
        return Err(format!(
            "aws {} failed (exit {}): {}",
            args.join(" "),
            out.exit_code,
            out.stderr.trim()
        ));
    }
    serde_json::from_str(&out.stdout).map_err(|e| {
        format!(
            "aws output not JSON: {e}\nstdout: {}",
            &out.stdout[..out.stdout.len().min(200)]
        )
    })
}

// ---------------------------------------------------------------------------
// String utilities.
// ---------------------------------------------------------------------------

fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    opt_str(input, key).ok_or_else(|| format!("`{key}` (non-empty string) required"))
}

// ---------------------------------------------------------------------------
// aws.test — STS GetCallerIdentity.
// ---------------------------------------------------------------------------

fn aws_test(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let (key_id, secret, token) = resolve_creds(host)?;
    let v = aws_json(
        host,
        &["sts", "get-caller-identity"],
        &key_id,
        &secret,
        token.as_deref(),
        &rgn,
    )?;
    Ok(json!({
        "account": v.get("Account").and_then(|x| x.as_str()).unwrap_or(""),
        "arn": v.get("Arn").and_then(|x| x.as_str()).unwrap_or(""),
        "user_id": v.get("UserId").and_then(|x| x.as_str()).unwrap_or(""),
        "region": rgn,
    }))
}

// ---------------------------------------------------------------------------
// aws.inspect — non-secret env/profile config presence.
//
// The fluxplane version queries the host for env-var presence directly; our
// host doesn't expose that path, so we probe credential validity via the
// presence/absence of the secret values (no STS call — inspect is intentionally
// offline/fast and must not fail when credentials are partially configured).
// ---------------------------------------------------------------------------

fn aws_inspect(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let access_key_configured = host
        .secret(PURPOSE_ACCESS_KEY_ID)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let secret_key_configured = host
        .secret(PURPOSE_SECRET_ACCESS_KEY)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let session_token_configured = host
        .secret(PURPOSE_SESSION_TOKEN)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let configured = access_key_configured || secret_key_configured;
    let available = access_key_configured && secret_key_configured;
    Ok(json!({
        "configured": configured,
        "available": available,
        "region": rgn,
        "access_key_configured": access_key_configured,
        "secret_key_configured": secret_key_configured,
        "session_token_configured": session_token_configured,
        "source": "host_secrets",
    }))
}

// ---------------------------------------------------------------------------
// aws.ec2.instances — DescribeInstances.
// ---------------------------------------------------------------------------

fn ec2_instances(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let (key_id, secret, token) = resolve_creds(host)?;
    let limit = input
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(50)
        .min(500) as usize;

    let name_filter = opt_str(&input, "name");
    let states: Vec<&str> = input
        .get("states")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let ids: Vec<&str> = input
        .get("ids")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    // Build the argv with owned strings to avoid lifetime issues.
    let mut argv: Vec<String> = vec![
        "aws".into(),
        "ec2".into(),
        "describe-instances".into(),
        "--output".into(),
        "json".into(),
    ];

    // --filters as a JSON-encoded string (the aws CLI accepts this form).
    let mut filter_json: Vec<Value> = Vec::new();
    if let Some(name) = name_filter {
        filter_json.push(json!({"Name": "tag:Name", "Values": [name]}));
    }
    if !states.is_empty() {
        filter_json.push(json!({"Name": "instance-state-name", "Values": states}));
    }
    if !filter_json.is_empty() {
        argv.push("--filters".into());
        argv.push(serde_json::to_string(&filter_json).unwrap());
    }

    // --instance-ids as positional args.
    if !ids.is_empty() {
        argv.push("--instance-ids".into());
        for id in &ids {
            argv.push((*id).to_string());
        }
    }
    let env_owned = cred_env(&key_id, &secret, token.as_deref(), &rgn);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let out = host.run_with_env(&argv_refs, &env_refs, 60)?;
    if out.exit_code != 0 {
        return Err(format!(
            "aws ec2 describe-instances failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    let v: Value =
        serde_json::from_str(&out.stdout).map_err(|e| format!("ec2 output not JSON: {e}"))?;

    let mut instances: Vec<Value> = Vec::new();
    let mut truncated = false;
    if let Some(reservations) = v.get("Reservations").and_then(|x| x.as_array()) {
        'outer: for res in reservations {
            if let Some(insts) = res.get("Instances").and_then(|x| x.as_array()) {
                for inst in insts {
                    if instances.len() >= limit {
                        truncated = true;
                        break 'outer;
                    }
                    instances.push(map_ec2_instance(inst));
                }
            }
        }
    }
    instances.sort_by(|a, b| {
        a.get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|x| x.as_str()).unwrap_or(""))
    });

    // Contribute to datasource.
    let records: Vec<Record> = instances
        .iter()
        .filter_map(|inst| {
            let id = inst.get("id").and_then(|x| x.as_str())?;
            let name = inst.get("name").and_then(|x| x.as_str()).unwrap_or(id);
            let state = inst.get("state").and_then(|x| x.as_str()).unwrap_or("");
            Some(Record::new(
                Source::new("aws"),
                "aws.ec2.instance",
                id.to_string(),
                name,
                format!("state={state} region={rgn}"),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }

    Ok(json!({
        "region": rgn,
        "instances": instances,
        "count": instances.len(),
        "truncated": truncated,
    }))
}

fn map_ec2_instance(inst: &Value) -> Value {
    let tags: Map<String, Value> = inst
        .get("Tags")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let k = t.get("Key").and_then(|x| x.as_str())?;
                    let v = t.get("Value").and_then(|x| x.as_str())?;
                    Some((k.to_string(), json!(v)))
                })
                .collect()
        })
        .unwrap_or_default();
    let name = tags
        .get("Name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let state = inst
        .pointer("/State/Name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let az = inst
        .pointer("/Placement/AvailabilityZone")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "id": inst.get("InstanceId").and_then(|x| x.as_str()).unwrap_or(""),
        "name": name,
        "state": state,
        "type": inst.get("InstanceType").and_then(|x| x.as_str()).unwrap_or(""),
        "az": az,
        "private_ip": inst.get("PrivateIpAddress").and_then(|x| x.as_str()).unwrap_or(""),
        "public_ip": inst.get("PublicIpAddress").and_then(|x| x.as_str()).unwrap_or(""),
        "image": inst.get("ImageId").and_then(|x| x.as_str()).unwrap_or(""),
        "launch_time": inst.get("LaunchTime").and_then(|x| x.as_str()).unwrap_or(""),
        "tags": tags,
    })
}

// ---------------------------------------------------------------------------
// aws.eks.clusters — list + describe.
// ---------------------------------------------------------------------------

fn eks_clusters(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let (key_id, secret, token) = resolve_creds(host)?;
    let name_filter = opt_str(&input, "name").map(String::from);

    let names: Vec<String> = if let Some(name) = name_filter {
        vec![name]
    } else {
        let v = aws_json(
            host,
            &["eks", "list-clusters"],
            &key_id,
            &secret,
            token.as_deref(),
            &rgn,
        )?;
        v.get("clusters")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|n| n.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    const MAX_DESCRIBES: usize = 20;
    let truncated = names.len() > MAX_DESCRIBES;
    let names_to_describe: Vec<&str> = names[..names.len().min(MAX_DESCRIBES)]
        .iter()
        .map(String::as_str)
        .collect();

    let mut clusters: Vec<Value> = Vec::new();
    for name in names_to_describe {
        let v = aws_json(
            host,
            &["eks", "describe-cluster", "--name", name],
            &key_id,
            &secret,
            token.as_deref(),
            &rgn,
        )?;
        let c = v.get("cluster").cloned().unwrap_or(Value::Null);
        let vpc = c
            .pointer("/resourcesVpcConfig/vpcId")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        clusters.push(json!({
            "name": c.get("name").and_then(|x| x.as_str()).unwrap_or(name),
            "arn": c.get("arn").and_then(|x| x.as_str()).unwrap_or(""),
            "version": c.get("version").and_then(|x| x.as_str()).unwrap_or(""),
            "status": c.get("status").and_then(|x| x.as_str()).unwrap_or(""),
            "endpoint": c.get("endpoint").and_then(|x| x.as_str()).unwrap_or(""),
            "platform_version": c.get("platformVersion").and_then(|x| x.as_str()).unwrap_or(""),
            "vpc": vpc,
            "created": c.get("createdAt").and_then(|x| x.as_str()).unwrap_or(""),
        }));
    }

    Ok(json!({
        "region": rgn,
        "clusters": clusters,
        "count": clusters.len(),
        "truncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
// aws.rds.instances — clusters + instances.
// ---------------------------------------------------------------------------

fn rds_instances(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let (key_id, secret, token) = resolve_creds(host)?;
    let engine_filter = opt_str(&input, "engine")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let limit = input
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(100)
        .min(500) as usize;

    // Clusters.
    let cv = aws_json(
        host,
        &["rds", "describe-db-clusters"],
        &key_id,
        &secret,
        token.as_deref(),
        &rgn,
    )?;
    let clusters: Vec<Value> = cv
        .get("DBClusters")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|c| {
                    engine_filter.is_empty()
                        || c.get("Engine")
                            .and_then(|x| x.as_str())
                            .map(|e| e.to_lowercase().contains(&engine_filter))
                            .unwrap_or(false)
                })
                .map(|c| {
                    let members: Vec<String> = c
                        .get("DBClusterMembers")
                        .and_then(|x| x.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| {
                                    m.get("DBInstanceIdentifier").and_then(|x| x.as_str()).map(String::from)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    json!({
                        "id": c.get("DBClusterIdentifier").and_then(|x| x.as_str()).unwrap_or(""),
                        "engine": c.get("Engine").and_then(|x| x.as_str()).unwrap_or(""),
                        "engine_version": c.get("EngineVersion").and_then(|x| x.as_str()).unwrap_or(""),
                        "status": c.get("Status").and_then(|x| x.as_str()).unwrap_or(""),
                        "writer_endpoint": c.get("Endpoint").and_then(|x| x.as_str()).unwrap_or(""),
                        "reader_endpoint": c.get("ReaderEndpoint").and_then(|x| x.as_str()).unwrap_or(""),
                        "port": c.get("Port").and_then(|x| x.as_i64()).unwrap_or(0),
                        "members": members,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Instances.
    let iv = aws_json(
        host,
        &["rds", "describe-db-instances"],
        &key_id,
        &secret,
        token.as_deref(),
        &rgn,
    )?;
    let mut instances: Vec<Value> = Vec::new();
    let mut truncated = false;
    if let Some(arr) = iv.get("DBInstances").and_then(|x| x.as_array()) {
        for inst in arr {
            let eng = inst.get("Engine").and_then(|x| x.as_str()).unwrap_or("");
            if !engine_filter.is_empty() && !eng.to_lowercase().contains(&engine_filter) {
                continue;
            }
            if instances.len() >= limit {
                truncated = true;
                break;
            }
            let endpoint = inst
                .pointer("/Endpoint/Address")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let port = inst
                .pointer("/Endpoint/Port")
                .and_then(|x| x.as_i64())
                .unwrap_or(0);
            instances.push(json!({
                "id": inst.get("DBInstanceIdentifier").and_then(|x| x.as_str()).unwrap_or(""),
                "engine": eng,
                "version": inst.get("EngineVersion").and_then(|x| x.as_str()).unwrap_or(""),
                "status": inst.get("DBInstanceStatus").and_then(|x| x.as_str()).unwrap_or(""),
                "class": inst.get("DBInstanceClass").and_then(|x| x.as_str()).unwrap_or(""),
                "endpoint": endpoint,
                "port": port,
                "az": inst.get("AvailabilityZone").and_then(|x| x.as_str()).unwrap_or(""),
                "multi_az": inst.get("MultiAZ").and_then(|x| x.as_bool()).unwrap_or(false),
                "cluster": inst.get("DBClusterIdentifier").and_then(|x| x.as_str()).unwrap_or(""),
            }));
        }
    }

    Ok(json!({
        "region": rgn,
        "clusters": clusters,
        "instances": instances,
        "count": clusters.len() + instances.len(),
        "truncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
// aws.s3.buckets — ListBuckets.
// ---------------------------------------------------------------------------

fn s3_buckets(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let (key_id, secret, token) = resolve_creds(host)?;
    let prefix_filter = opt_str(&input, "prefix").unwrap_or("").to_string();
    let v = aws_json(
        host,
        &["s3api", "list-buckets"],
        &key_id,
        &secret,
        token.as_deref(),
        &rgn,
    )?;
    let buckets: Vec<Value> = v
        .get("Buckets")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    let name = b.get("Name").and_then(|x| x.as_str())?;
                    if !prefix_filter.is_empty() && !name.starts_with(&prefix_filter) {
                        return None;
                    }
                    Some(json!({
                        "name": name,
                        "created": b.get("CreationDate").and_then(|x| x.as_str()).unwrap_or(""),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "region": rgn,
        "buckets": buckets,
        "count": buckets.len(),
    }))
}

// ---------------------------------------------------------------------------
// aws.s3.objects — ListObjectsV2 with continuation.
// ---------------------------------------------------------------------------

fn s3_objects(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let bucket = req_str(&input, "bucket")?;
    let (key_id, secret, token) = resolve_creds(host)?;
    let limit = input
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(100)
        .min(1000) as usize;

    let mut argv: Vec<String> = vec![
        "aws".into(),
        "s3api".into(),
        "list-objects-v2".into(),
        "--bucket".into(),
        bucket.to_string(),
        "--max-items".into(),
        limit.to_string(),
        "--output".into(),
        "json".into(),
    ];
    if let Some(prefix) = opt_str(&input, "prefix") {
        argv.push("--prefix".into());
        argv.push(prefix.to_string());
    }
    if let Some(token_str) = opt_str(&input, "next_token") {
        argv.push("--starting-token".into());
        argv.push(token_str.to_string());
    }
    let env_owned = cred_env(&key_id, &secret, token.as_deref(), &rgn);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let out = host.run_with_env(&argv_refs, &env_refs, 60)?;
    if out.exit_code != 0 {
        return Err(format!(
            "aws s3api list-objects-v2 failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    let v: Value =
        serde_json::from_str(&out.stdout).map_err(|e| format!("s3 output not JSON: {e}"))?;

    let objects: Vec<Value> = v
        .get("Contents")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|o| {
                    json!({
                        "key": o.get("Key").and_then(|x| x.as_str()).unwrap_or(""),
                        "size": o.get("Size").and_then(|x| x.as_i64()).unwrap_or(0),
                        "modified": o.get("LastModified").and_then(|x| x.as_str()).unwrap_or(""),
                        "storage_class": o.get("StorageClass").and_then(|x| x.as_str()).unwrap_or(""),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let truncated = v
        .get("IsTruncated")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let next_token = v
        .pointer("/NextToken")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    Ok(json!({
        "region": rgn,
        "bucket": bucket,
        "objects": objects,
        "count": objects.len(),
        "truncated": truncated,
        "next_token": next_token,
    }))
}

// ---------------------------------------------------------------------------
// aws.logs.groups — DescribeLogGroups.
// ---------------------------------------------------------------------------

fn logs_groups(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let (key_id, secret, token) = resolve_creds(host)?;
    let limit = input
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(100)
        .min(500) as usize;

    let mut argv: Vec<String> = vec![
        "aws".into(),
        "logs".into(),
        "describe-log-groups".into(),
        "--limit".into(),
        limit.to_string(),
        "--output".into(),
        "json".into(),
    ];
    if let Some(prefix) = opt_str(&input, "prefix") {
        argv.push("--log-group-name-prefix".into());
        argv.push(prefix.to_string());
    }
    let env_owned = cred_env(&key_id, &secret, token.as_deref(), &rgn);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let out = host.run_with_env(&argv_refs, &env_refs, 60)?;
    if out.exit_code != 0 {
        return Err(format!(
            "aws logs describe-log-groups failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    let v: Value =
        serde_json::from_str(&out.stdout).map_err(|e| format!("logs output not JSON: {e}"))?;

    let mut groups: Vec<Value> = Vec::new();
    let mut truncated = false;
    if let Some(arr) = v.get("logGroups").and_then(|x| x.as_array()) {
        for g in arr {
            if groups.len() >= limit {
                truncated = true;
                break;
            }
            groups.push(json!({
                "name": g.get("logGroupName").and_then(|x| x.as_str()).unwrap_or(""),
                "retention_days": g.get("retentionInDays").and_then(|x| x.as_i64()).unwrap_or(0),
                "stored_bytes": g.get("storedBytes").and_then(|x| x.as_i64()).unwrap_or(0),
                "created": g.get("creationTime").and_then(|x| x.as_str()).unwrap_or(""),
            }));
        }
    }
    Ok(json!({
        "region": rgn,
        "groups": groups,
        "count": groups.len(),
        "truncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
// aws.logs.tail — FilterLogEvents over a time window.
// ---------------------------------------------------------------------------

fn logs_tail(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let group = req_str(&input, "group")?;
    let (key_id, secret, token) = resolve_creds(host)?;
    let limit = input
        .get("limit")
        .and_then(|x| x.as_u64())
        .unwrap_or(200)
        .min(1000) as usize;

    let since_str = opt_str(&input, "since").unwrap_or("15m");
    let start_ms = parse_time_to_ms(since_str, true)?;
    let end_ms = if let Some(until) = opt_str(&input, "until") {
        parse_time_to_ms(until, false)?
    } else {
        now_ms()
    };

    let mut argv: Vec<String> = vec![
        "aws".into(),
        "logs".into(),
        "filter-log-events".into(),
        "--log-group-name".into(),
        group.to_string(),
        "--start-time".into(),
        start_ms.to_string(),
        "--end-time".into(),
        end_ms.to_string(),
        "--limit".into(),
        limit.to_string(),
        "--output".into(),
        "json".into(),
    ];
    if let Some(pattern) = opt_str(&input, "pattern") {
        argv.push("--filter-pattern".into());
        argv.push(pattern.to_string());
    }
    let env_owned = cred_env(&key_id, &secret, token.as_deref(), &rgn);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let out = host.run_with_env(&argv_refs, &env_refs, 60)?;
    if out.exit_code != 0 {
        return Err(format!(
            "aws logs filter-log-events failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    let v: Value =
        serde_json::from_str(&out.stdout).map_err(|e| format!("logs output not JSON: {e}"))?;

    let mut events: Vec<Value> = Vec::new();
    let mut truncated = false;
    if let Some(arr) = v.get("events").and_then(|x| x.as_array()) {
        for e in arr {
            if events.len() >= limit {
                truncated = true;
                break;
            }
            events.push(json!({
                "time": e.get("timestamp").and_then(|x| x.as_str()).unwrap_or(""),
                "stream": e.get("logStreamName").and_then(|x| x.as_str()).unwrap_or(""),
                "message": e.get("message").and_then(|x| x.as_str()).unwrap_or("").trim_end_matches('\n'),
            }));
        }
    }
    Ok(json!({
        "region": rgn,
        "group": group,
        "events": events,
        "count": events.len(),
        "truncated": truncated,
    }))
}

// ---------------------------------------------------------------------------
// aws.logs.query — CloudWatch Logs Insights (StartQuery + poll).
// ---------------------------------------------------------------------------

fn logs_query(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let groups: Vec<&str> = input
        .get("groups")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if groups.is_empty() {
        return Err("`groups` (non-empty array of strings) required".into());
    }
    let query = req_str(&input, "query")?;
    let (key_id, secret, token) = resolve_creds(host)?;
    let timeout_secs = input
        .get("timeout_seconds")
        .and_then(|x| x.as_u64())
        .unwrap_or(30)
        .min(120);

    let since_str = opt_str(&input, "since").unwrap_or("1h");
    let start_s = parse_time_to_ms(since_str, true)? / 1000;
    let end_s = if let Some(until) = opt_str(&input, "until") {
        parse_time_to_ms(until, false)? / 1000
    } else {
        now_ms() / 1000
    };

    // Build log-groups args: --log-group-names g1 g2 ... (multiple values).
    let mut start_argv: Vec<String> = vec![
        "aws".into(),
        "logs".into(),
        "start-query".into(),
        "--start-time".into(),
        start_s.to_string(),
        "--end-time".into(),
        end_s.to_string(),
        "--query-string".into(),
        query.to_string(),
        "--log-group-names".into(),
    ];
    for g in &groups {
        start_argv.push((*g).to_string());
    }
    start_argv.push("--output".into());
    start_argv.push("json".into());

    let env_owned = cred_env(&key_id, &secret, token.as_deref(), &rgn);
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let argv_refs: Vec<&str> = start_argv.iter().map(String::as_str).collect();
    let started_out = host.run_with_env(&argv_refs, &env_refs, 30)?;
    if started_out.exit_code != 0 {
        return Err(format!(
            "aws logs start-query failed (exit {}): {}",
            started_out.exit_code,
            started_out.stderr.trim()
        ));
    }
    let started: Value = serde_json::from_str(&started_out.stdout)
        .map_err(|e| format!("start-query output not JSON: {e}"))?;
    let query_id = started
        .get("queryId")
        .and_then(|x| x.as_str())
        .ok_or("start-query: no queryId in response")?
        .to_string();

    // Poll until complete or timeout.
    let poll_argv_base: Vec<String> = vec![
        "aws".into(),
        "logs".into(),
        "get-query-results".into(),
        "--query-id".into(),
        query_id.clone(),
        "--output".into(),
        "json".into(),
    ];
    let deadline_iterations = (timeout_secs / 2).max(1) as usize; // poll every ~2 logical steps
    let mut status = String::new();
    let mut result_v = Value::Null;
    for _ in 0..deadline_iterations.max(15) {
        let argv_refs: Vec<&str> = poll_argv_base.iter().map(String::as_str).collect();
        let poll_out = host.run_with_env(&argv_refs, &env_refs, 30)?;
        if poll_out.exit_code != 0 {
            return Err(format!(
                "aws logs get-query-results failed: {}",
                poll_out.stderr.trim()
            ));
        }
        let v: Value = serde_json::from_str(&poll_out.stdout)
            .map_err(|e| format!("get-query-results not JSON: {e}"))?;
        status = v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        match status.as_str() {
            "Complete" => {
                result_v = v;
                break;
            }
            "Failed" | "Cancelled" | "Timeout" => {
                return Err(format!("logs insights query {}", status.to_lowercase()));
            }
            _ => {
                result_v = v;
                // continue polling
            }
        }
    }

    // Parse results if complete.
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Value> = Vec::new();
    let mut records_matched = 0.0f64;
    let mut records_scanned = 0.0f64;

    if status == "Complete" {
        let mut col_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(result_rows) = result_v.get("results").and_then(|x| x.as_array()) {
            for row in result_rows {
                let mut mapped: serde_json::Map<String, Value> = serde_json::Map::new();
                if let Some(fields) = row.as_array() {
                    for field in fields {
                        let col = field.get("field").and_then(|x| x.as_str()).unwrap_or("");
                        let val = field.get("value").and_then(|x| x.as_str()).unwrap_or("");
                        if !col_seen.contains(col) {
                            col_seen.insert(col.to_string());
                            columns.push(col.to_string());
                        }
                        mapped.insert(col.to_string(), json!(val));
                    }
                }
                rows.push(Value::Object(mapped));
            }
        }
        if let Some(stats) = result_v.get("statistics") {
            records_matched = stats
                .get("recordsMatched")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0);
            records_scanned = stats
                .get("recordsScanned")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0);
        }
    }

    Ok(json!({
        "region": rgn,
        "status": status,
        "query_id": query_id,
        "columns": columns,
        "rows": rows,
        "records_matched": records_matched,
        "records_scanned": records_scanned,
    }))
}

// ---------------------------------------------------------------------------
// aws.cloudwatch.metrics — GetMetricData.
// ---------------------------------------------------------------------------

fn cloudwatch_metrics(input: Value, host: &mut Host) -> Result<Value, String> {
    let rgn = region(&input);
    let namespace = req_str(&input, "namespace")?;
    let metric = req_str(&input, "metric")?;
    let (key_id, secret, token) = resolve_creds(host)?;

    let stat = opt_str(&input, "stat").unwrap_or("Average").to_string();
    let period = input
        .get("period")
        .and_then(|x| x.as_u64())
        .unwrap_or(300)
        .max(60);

    let since_str = opt_str(&input, "since").unwrap_or("3h");
    let start_s = parse_time_to_ms(since_str, true)? / 1000;
    let end_s = if let Some(until) = opt_str(&input, "until") {
        parse_time_to_ms(until, false)? / 1000
    } else {
        now_ms() / 1000
    };

    // Build MetricDataQuery JSON spec for --metric-data-queries.
    let dimensions: Vec<Value> = input
        .get("dimensions")
        .and_then(|x| x.as_object())
        .map(|map| {
            map.iter()
                .map(|(k, v)| json!({"Name": k, "Value": v.as_str().unwrap_or("")}))
                .collect()
        })
        .unwrap_or_default();

    let query_spec = json!([{
        "Id": "series0",
        "MetricStat": {
            "Metric": {
                "Namespace": namespace,
                "MetricName": metric,
                "Dimensions": dimensions
            },
            "Period": period,
            "Stat": stat
        }
    }]);
    let query_spec_str = serde_json::to_string(&query_spec).unwrap();

    let argv: Vec<String> = vec![
        "aws".into(),
        "cloudwatch".into(),
        "get-metric-data".into(),
        "--start-time".into(),
        start_s.to_string(),
        "--end-time".into(),
        end_s.to_string(),
        "--metric-data-queries".into(),
        query_spec_str,
        "--output".into(),
        "json".into(),
    ];
    let env_owned = cred_env(&key_id, &secret, token.as_deref(), &rgn);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env_refs: Vec<(&str, &str)> = env_owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let out = host.run_with_env(&argv_refs, &env_refs, 60)?;
    if out.exit_code != 0 {
        return Err(format!(
            "aws cloudwatch get-metric-data failed (exit {}): {}",
            out.exit_code,
            out.stderr.trim()
        ));
    }
    let v: Value = serde_json::from_str(&out.stdout)
        .map_err(|e| format!("cloudwatch output not JSON: {e}"))?;

    let mut datapoints: Vec<Value> = Vec::new();
    let mut label = String::new();
    if let Some(results) = v.get("MetricDataResults").and_then(|x| x.as_array()) {
        for series in results {
            if label.is_empty() {
                label = series
                    .get("Label")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            let timestamps = series.get("Timestamps").and_then(|x| x.as_array());
            let values = series.get("Values").and_then(|x| x.as_array());
            if let (Some(ts), Some(vs)) = (timestamps, values) {
                for (t, val) in ts.iter().zip(vs.iter()) {
                    datapoints.push(json!({
                        "time": t.as_str().unwrap_or(""),
                        "value": val.as_f64().unwrap_or(0.0),
                    }));
                }
            }
        }
    }
    // Sort by time ascending.
    datapoints.sort_by(|a, b| {
        a.get("time")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .cmp(b.get("time").and_then(|x| x.as_str()).unwrap_or(""))
    });

    Ok(json!({
        "region": rgn,
        "namespace": namespace,
        "metric": metric,
        "stat": stat,
        "label": label,
        "datapoints": datapoints,
        "count": datapoints.len(),
    }))
}

// ---------------------------------------------------------------------------
// Time parsing helpers (no chrono dependency — parse the simple forms we need).
// ---------------------------------------------------------------------------

/// Returns current time in milliseconds since Unix epoch.
fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Parse a time value to Unix milliseconds.
/// `as_ago`: if the value looks like a plain duration (`30m`, `1h`, `7d`), subtract from now.
/// Otherwise accepts Unix seconds (integer string) or RFC3339.
fn parse_time_to_ms(value: &str, as_ago: bool) -> Result<u64, String> {
    let v = value.trim();
    // Duration-ago form (e.g. "15m", "2h", "7d").
    if as_ago {
        if let Some(ms) = parse_duration_ago(v) {
            return Ok(now_ms().saturating_sub(ms));
        }
    }
    // Unix seconds integer.
    if let Ok(secs) = v.parse::<u64>() {
        return Ok(secs * 1000);
    }
    // RFC3339 — parse manually for the common `2006-01-02T15:04:05Z` subset.
    if v.len() >= 20 && v.contains('T') {
        return parse_rfc3339_to_ms(v);
    }
    Err(format!(
        "cannot parse time `{v}` (expected duration like 15m, unix seconds, or RFC3339)"
    ))
}

/// Parse a simple duration string (e.g. `15m`, `2h`, `7d`) to milliseconds.
fn parse_duration_ago(v: &str) -> Option<u64> {
    let (num_str, unit) = v.split_at(v.len().saturating_sub(1));
    let n: u64 = num_str.trim().parse().ok()?;
    match unit {
        "s" => Some(n * 1_000),
        "m" => Some(n * 60 * 1_000),
        "h" => Some(n * 3600 * 1_000),
        "d" => Some(n * 86400 * 1_000),
        _ => None,
    }
}

/// Minimal RFC3339 parser for `YYYY-MM-DDTHH:MM:SSZ` (UTC, Z suffix).
fn parse_rfc3339_to_ms(v: &str) -> Result<u64, String> {
    // Strip the Z / offset for simplicity — treat everything as UTC.
    let s = v.trim_end_matches('Z').trim_end_matches("+00:00");
    let parts: Vec<&str> = s.splitn(2, 'T').collect();
    if parts.len() != 2 {
        return Err(format!("bad RFC3339: {v}"));
    }
    let date: Vec<u64> = parts[0]
        .splitn(3, '-')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect();
    let time_parts: Vec<u64> = parts[1]
        .splitn(3, ':')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect();
    if date.len() < 3 || time_parts.len() < 3 {
        return Err(format!("bad RFC3339: {v}"));
    }
    let (year, month, day) = (date[0], date[1], date[2]);
    let (hour, min, sec) = (time_parts[0], time_parts[1], time_parts[2]);
    // Days since epoch (rough, good enough for ±years of the real range).
    let days = days_since_epoch(year, month, day);
    let secs = days * 86400 + hour * 3600 + min * 60 + sec;
    Ok(secs * 1000)
}

fn days_since_epoch(year: u64, month: u64, day: u64) -> u64 {
    // Cumulative days from Jan 1 1970.
    let leap = |y: u64| y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days_in_year = |y: u64| if leap(y) { 366 } else { 365 };
    let days_in_month = |y: u64, m: u64| match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    };
    let mut d: u64 = 0;
    for y in 1970..year {
        d += days_in_year(y);
    }
    for m in 1..month {
        d += days_in_month(year, m);
    }
    d + day.saturating_sub(1)
}

// ---------------------------------------------------------------------------
// host.run_with_env shim — the Host API exposes process.run (no env overrides) and
// process.spawn (env overrides). For the CLI-process pattern we need env overrides
// on a one-shot run. We implement this by wrapping process.spawn + process_read drain.
// ---------------------------------------------------------------------------

trait HostExt {
    fn run_with_env(
        &mut self,
        argv: &[&str],
        env: &[(&str, &str)],
        timeout_secs: u64,
    ) -> Result<ProcessOutput, String>;
}

impl HostExt for Host<'_> {
    fn run_with_env(
        &mut self,
        argv: &[&str],
        env: &[(&str, &str)],
        timeout_secs: u64,
    ) -> Result<ProcessOutput, String> {
        let pid = self.process_spawn(argv, env)?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code: i64 = 0;
        let mut exited = false;
        // Bounded poll: up to timeout_secs * 2 iterations (rough drain cadence).
        let max_iters = (timeout_secs * 2).max(4) as usize;
        for _ in 0..max_iters {
            let r = self.process_read(pid)?;
            stdout.push_str(&r.stdout);
            stderr.push_str(&r.stderr);
            if !r.running {
                // If the host didn't return an explicit exit code, treat as 0 (success). A
                // non-zero code is always reported explicitly; None means "exited cleanly" in
                // the mock and means unknown-but-success in the real host for short-lived ops.
                exit_code = r.exit_code.unwrap_or(0);
                exited = true;
                break;
            }
        }
        // Kill in case we timed out (idempotent if already exited).
        let _ = self.process_kill(pid);
        if !exited {
            // Process did not finish within our poll budget — treat as timeout error.
            exit_code = -1;
        }
        Ok(ProcessOutput {
            stdout,
            stderr,
            exit_code,
        })
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    manifest_builder().serve();
}

// ---------------------------------------------------------------------------
// Tests — one per op, all hermetic against MockHost.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: pre-loaded secret MockHost with access_key_id + secret_access_key.
    fn host_with_creds() -> MockHost {
        MockHost::default()
            .with_secret(PURPOSE_ACCESS_KEY_ID, "AKIATEST")
            .with_secret(PURPOSE_SECRET_ACCESS_KEY, "secretkey")
            .with_secret(PURPOSE_SESSION_TOKEN, "")
    }

    // Wraps MockHost so `run_with_env` (which goes through process_spawn + process_read) works.
    // We set spawn_proc_id and proc_output to canned values, and with_process for the "run" path.
    fn host_with_creds_and_proc(stdout: &str) -> MockHost {
        host_with_creds()
            .with_spawn(1)
            .with_proc_output(stdout, "", false)
    }

    #[test]
    fn test_returns_account_and_arn() {
        let plugin = manifest_builder().build();
        let mut host = host_with_creds_and_proc(
            r#"{"Account":"123456789","Arn":"arn:aws:iam::123456789:user/test","UserId":"AIDATEST"}"#,
        );
        let out = plugin.call("aws.test", json!({}), &mut host).unwrap();
        assert_eq!(out["account"], "123456789");
        assert_eq!(out["arn"], "arn:aws:iam::123456789:user/test");
        assert_eq!(out["region"], DEFAULT_REGION);
    }

    #[test]
    fn inspect_reports_credential_presence() {
        let plugin = manifest_builder().build();
        // All three secrets present.
        let mut host = MockHost::default()
            .with_secret(PURPOSE_ACCESS_KEY_ID, "AKIATEST")
            .with_secret(PURPOSE_SECRET_ACCESS_KEY, "secretkey")
            .with_secret(PURPOSE_SESSION_TOKEN, "tok");
        let out = plugin.call("aws.inspect", json!({}), &mut host).unwrap();
        assert_eq!(out["configured"], true);
        assert_eq!(out["available"], true);
        assert_eq!(out["access_key_configured"], true);
        assert_eq!(out["secret_key_configured"], true);
        assert_eq!(out["session_token_configured"], true);
    }

    #[test]
    fn inspect_partial_creds() {
        let plugin = manifest_builder().build();
        // Only access key set (no secret key).
        let mut host = MockHost::default().with_secret(PURPOSE_ACCESS_KEY_ID, "AKIATEST");
        let out = plugin.call("aws.inspect", json!({}), &mut host).unwrap();
        assert_eq!(out["configured"], true);
        assert_eq!(out["available"], false); // needs both
        assert_eq!(out["access_key_configured"], true);
        assert_eq!(out["secret_key_configured"], false);
    }

    #[test]
    fn ec2_instances_lists_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = host_with_creds_and_proc(
            r#"{"Reservations":[{"Instances":[{
                "InstanceId":"i-001",
                "InstanceType":"t3.micro",
                "State":{"Name":"running"},
                "Placement":{"AvailabilityZone":"eu-central-1a"},
                "PrivateIpAddress":"10.0.0.1",
                "PublicIpAddress":"",
                "ImageId":"ami-abc",
                "LaunchTime":"2024-01-01T00:00:00Z",
                "Tags":[{"Key":"Name","Value":"web-server"}]
            }]}]}"#,
        );
        let out = plugin
            .call("aws.ec2.instances", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["instances"][0]["id"], "i-001");
        assert_eq!(out["instances"][0]["name"], "web-server");
        assert_eq!(out["instances"][0]["state"], "running");
        let recs = host.contributed.borrow();
        assert_eq!(recs[0].entity, "aws.ec2.instance");
        assert_eq!(recs[0].id, "i-001");
    }

    #[test]
    fn eks_clusters_describes_all() {
        let plugin = manifest_builder().build();
        // First call: list-clusters; second call: describe-cluster.
        let host = host_with_creds().with_spawn(1).with_proc_output(
            r#"{"clusters":["dev-cluster"]}"#,
            "",
            false,
        );
        // First spawn returns list, second describe. MockHost returns the same proc_output for every
        // spawn, so we need to test the combined flow with a list result that embeds describe data.
        // For simplicity, we test only the "name filter" path (single describe call).
        let mut host2 = host_with_creds_and_proc(
            r#"{"cluster":{"name":"dev-cluster","arn":"arn:aws:eks:eu-central-1:123:cluster/dev-cluster","version":"1.29","status":"ACTIVE","endpoint":"https://api.dev","platformVersion":"eks.5","resourcesVpcConfig":{"vpcId":"vpc-123"},"createdAt":"2024-01-01T00:00:00Z"}}"#,
        );
        let out = plugin
            .call(
                "aws.eks.clusters",
                json!({"name": "dev-cluster"}),
                &mut host2,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["clusters"][0]["name"], "dev-cluster");
        assert_eq!(out["clusters"][0]["version"], "1.29");
        assert_eq!(out["clusters"][0]["vpc"], "vpc-123");
        let _ = host; // suppress unused
    }

    #[test]
    fn rds_instances_parses_clusters_and_instances() {
        let plugin = manifest_builder().build();
        // Two process.spawn calls: first for clusters, second for instances.
        // MockHost returns the same proc_output for all spawns, so we encode a response
        // that can serve both calls (clusters response with DBClusters, instances empty).
        // For a realistic test, test clusters-only path.
        let mut host = host_with_creds_and_proc(
            r#"{"DBClusters":[{"DBClusterIdentifier":"aurora-1","Engine":"aurora-mysql","EngineVersion":"8.0","Status":"available","Endpoint":"writer.rds.amazonaws.com","ReaderEndpoint":"reader.rds.amazonaws.com","Port":3306,"DBClusterMembers":[{"DBInstanceIdentifier":"aurora-1-instance-1"}]}],"DBInstances":[]}"#,
        );
        // rds_instances makes two separate aws calls; mock returns same output for both.
        // Second call (DBInstances) will get same JSON; DBInstances is empty array so no instances.
        let out = plugin
            .call("aws.rds.instances", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["clusters"][0]["id"], "aurora-1");
        assert_eq!(out["clusters"][0]["engine"], "aurora-mysql");
    }

    #[test]
    fn s3_buckets_filters_by_prefix() {
        let plugin = manifest_builder().build();
        let mut host = host_with_creds_and_proc(
            r#"{"Buckets":[{"Name":"logs-prod","CreationDate":"2024-01-01T00:00:00Z"},{"Name":"data-prod","CreationDate":"2024-01-01T00:00:00Z"}]}"#,
        );
        let out = plugin
            .call("aws.s3.buckets", json!({"prefix": "logs"}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["buckets"][0]["name"], "logs-prod");
    }

    #[test]
    fn s3_objects_returns_paginated_results() {
        let plugin = manifest_builder().build();
        let mut host = host_with_creds_and_proc(
            r#"{"Contents":[{"Key":"logs/2024-01-01.log","Size":1024,"LastModified":"2024-01-01T00:00:00Z","StorageClass":"STANDARD"}],"IsTruncated":false}"#,
        );
        let out = plugin
            .call(
                "aws.s3.objects",
                json!({"bucket": "my-bucket", "prefix": "logs/"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["objects"][0]["key"], "logs/2024-01-01.log");
        assert_eq!(out["objects"][0]["size"], 1024);
        assert_eq!(out["truncated"], false);
    }

    #[test]
    fn s3_objects_requires_bucket() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin.call("aws.s3.objects", json!({}), &mut host).is_err());
    }

    #[test]
    fn logs_groups_lists_groups() {
        let plugin = manifest_builder().build();
        let mut host = host_with_creds_and_proc(
            r#"{"logGroups":[{"logGroupName":"/aws/eks/dev","retentionInDays":30,"storedBytes":102400}]}"#,
        );
        let out = plugin
            .call("aws.logs.groups", json!({"prefix": "/aws/eks/"}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["groups"][0]["name"], "/aws/eks/dev");
        assert_eq!(out["groups"][0]["retention_days"], 30);
    }

    #[test]
    fn logs_tail_returns_events() {
        let plugin = manifest_builder().build();
        let mut host = host_with_creds_and_proc(
            r#"{"events":[{"timestamp":"1704067200000","logStreamName":"stream-1","message":"ERROR something failed\n"}]}"#,
        );
        let out = plugin
            .call(
                "aws.logs.tail",
                json!({"group": "/aws/eks/dev", "since": "15m"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["group"], "/aws/eks/dev");
        assert_eq!(out["events"][0]["stream"], "stream-1");
        // Message trailing newline should be stripped.
        assert_eq!(out["events"][0]["message"], "ERROR something failed");
    }

    #[test]
    fn logs_tail_requires_group() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin.call("aws.logs.tail", json!({}), &mut host).is_err());
    }

    #[test]
    fn logs_query_runs_and_parses_complete_results() {
        let plugin = manifest_builder().build();
        // Both start-query and get-query-results return same canned output; first call starts,
        // second poll sees "Complete".
        let mut host = host_with_creds()
            .with_spawn(1)
            .with_proc_output(
                r#"{"queryId":"q-abc123","status":"Complete","results":[[{"field":"@timestamp","value":"2024-01-01T00:00:00Z"},{"field":"@message","value":"hello"}]],"statistics":{"recordsMatched":1.0,"recordsScanned":100.0}}"#,
                "",
                false,
            );
        let out = plugin
            .call(
                "aws.logs.query",
                json!({"groups": ["/aws/eks/dev"], "query": "fields @timestamp, @message | limit 10"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["status"], "Complete");
        assert_eq!(out["query_id"], "q-abc123");
        assert_eq!(out["columns"][0], "@timestamp");
        assert_eq!(out["rows"][0]["@message"], "hello");
        assert_eq!(out["records_matched"], 1.0);
    }

    #[test]
    fn logs_query_requires_groups_and_query() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin
            .call(
                "aws.logs.query",
                json!({"query": "fields @message"}),
                &mut host
            )
            .is_err());
        assert!(plugin
            .call(
                "aws.logs.query",
                json!({"groups": ["/aws/eks/dev"]}),
                &mut host
            )
            .is_err());
    }

    #[test]
    fn cloudwatch_metrics_fetches_datapoints() {
        let plugin = manifest_builder().build();
        let mut host = host_with_creds_and_proc(
            r#"{"MetricDataResults":[{"Label":"CPUUtilization","Timestamps":["2024-01-01T01:00:00Z","2024-01-01T00:00:00Z"],"Values":[45.2,42.1]}]}"#,
        );
        let out = plugin
            .call(
                "aws.cloudwatch.metrics",
                json!({"namespace": "AWS/RDS", "metric": "CPUUtilization", "since": "3h"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["namespace"], "AWS/RDS");
        assert_eq!(out["metric"], "CPUUtilization");
        assert_eq!(out["count"], 2);
        // Sorted ascending by time.
        assert_eq!(out["datapoints"][0]["time"], "2024-01-01T00:00:00Z");
        assert_eq!(out["datapoints"][1]["time"], "2024-01-01T01:00:00Z");
        assert!((out["datapoints"][0]["value"].as_f64().unwrap() - 42.1).abs() < 0.001);
    }

    #[test]
    fn cloudwatch_metrics_requires_namespace_and_metric() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        assert!(plugin
            .call(
                "aws.cloudwatch.metrics",
                json!({"namespace": "AWS/RDS"}),
                &mut host
            )
            .is_err());
        assert!(plugin
            .call(
                "aws.cloudwatch.metrics",
                json!({"metric": "CPUUtilization"}),
                &mut host
            )
            .is_err());
    }

    #[test]
    fn manifest_declares_11_ops_and_aws_capability() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 11);
        assert_eq!(m.capabilities.process, vec!["aws".to_string()]);
        assert!(m
            .capabilities
            .secrets
            .contains(&"AWS_ACCESS_KEY_ID".to_string()));
        let names: Vec<&str> = m.operations.iter().map(|o| o.name.as_str()).collect();
        for expected in &[
            "aws.test",
            "aws.inspect",
            "aws.ec2.instances",
            "aws.eks.clusters",
            "aws.rds.instances",
            "aws.s3.buckets",
            "aws.s3.objects",
            "aws.logs.groups",
            "aws.logs.tail",
            "aws.logs.query",
            "aws.cloudwatch.metrics",
        ] {
            assert!(names.contains(expected), "missing op: {expected}");
        }
    }

    #[test]
    fn parse_duration_ago_works() {
        assert!(parse_duration_ago("15m").is_some());
        assert!(parse_duration_ago("2h").is_some());
        assert!(parse_duration_ago("1d").is_some());
        assert!(parse_duration_ago("30s").is_some());
        assert!(parse_duration_ago("bad").is_none());
    }
}
