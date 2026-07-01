//! `asterisk` — a flux integration plugin for the Asterisk PBX via the Asterisk Manager Interface
//! (AMI) over a raw TCP socket.  All IO goes through the host `conn.*` capability; secrets are read
//! via `host.secret("username")` and `host.secret("secret")`.  The AMI host:port is taken from the
//! `ASTERISK_AMI_HOST` / `ASTERISK_AMI_PORT` env vars (declared as an endpoint); if the endpoint
//! cannot be resolved it falls back to `localhost:5038`.
//!
//! The plugin implements 8 ops:
//!   - asterisk.ami.ping        (read)
//!   - asterisk.channel.list    (read)
//!   - asterisk.peer.list       (read; param technology)
//!   - asterisk.queue.status    (read; param queue)
//!   - asterisk.devicestate.list (read; param device)
//!   - asterisk.channel.hangup  (write/destructive; params channel, cause)
//!   - asterisk.call.originate  (write/destructive; params channel, exten, context, …)
//!   - asterisk.command         (write/high-risk; param command)
//!
//! Protocol: AMI speaks "Key: Value\r\n" blocks terminated by a blank line.  Login sends
//! `Action: Login\r\nUsername: ..\r\nSecret: ..\r\nEvents: off\r\n\r\n` and expects
//! `Response: Success`.  List actions accumulate `Event:` blocks until a named "complete" event.

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

/// Shared per-call AMI connection fields. Fluxplane embeds `AMITargetInput` (endpoint_ref/url)
/// on every op; those are an architectural split (flux resolves the AMI host from the manifest
/// endpoint), so only the portable `timeout` field is shared. The timeout is parsed/validated
/// for parity but is not wire-enforced because the host `conn.*` capability does not currently
/// expose a per-call timeout.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct AMIConn {
    /// AMI connection timeout, e.g. `5s` or `1m`. Defaults to 10s if omitted.
    timeout: Option<String>,
}

/// `asterisk.ami.ping`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PingInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    ami: AMIConn,
}

/// `asterisk.channel.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ChannelListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    ami: AMIConn,
    /// Maximum channels to return.
    limit: Option<i64>,
}

/// `asterisk.peer.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PeerListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    ami: AMIConn,
    /// Channel technology: `pjsip`, `sip`, or `iax` (default `pjsip`).
    technology: Option<String>,
    /// Maximum peers to return.
    limit: Option<i64>,
}

/// `asterisk.queue.status`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct QueueStatusInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    ami: AMIConn,
    /// Limit status to this queue.
    queue: Option<String>,
}

/// `asterisk.devicestate.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct DeviceStateListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    ami: AMIConn,
    /// Substring filter on the device name (e.g. `PJSIP/agent-7`).
    device: Option<String>,
    /// Maximum device states to return.
    limit: Option<i64>,
}

/// `asterisk.channel.hangup`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct HangupInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    ami: AMIConn,
    /// Exact channel name to hang up.
    channel: String,
    /// ISDN hangup cause code (e.g. 16 normal clearing).
    cause: Option<i64>,
}

/// `asterisk.call.originate`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct OriginateInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    ami: AMIConn,
    /// Channel to call first.
    channel: String,
    /// Extension to connect to (requires `context`; mutually exclusive with `application`).
    exten: Option<String>,
    /// Dialplan context for `exten`.
    context: Option<String>,
    /// Application to run on answer (mutually exclusive with `exten`).
    application: Option<String>,
    /// Application argument data.
    data: Option<String>,
    /// Caller ID for the originated call.
    #[serde(rename = "caller_id")]
    caller_id: Option<String>,
    /// Answer timeout in milliseconds (default 30000).
    #[serde(rename = "timeout_ms")]
    timeout_ms: Option<i64>,
    /// Originate asynchronously (default true).
    #[serde(rename = "async")]
    do_async: Option<bool>,
    /// Channel variables to set on the originated channel.
    variables: Option<HashMap<String, String>>,
    /// Account code for the call.
    #[serde(rename = "account_code")]
    account_code: Option<String>,
    /// Dialplan priority (default 1).
    priority: Option<i64>,
    /// Connect on early media instead of answer.
    #[serde(rename = "early_media")]
    early_media: Option<bool>,
    /// Explicit unique id for the first channel.
    #[serde(rename = "channel_id")]
    channel_id: Option<String>,
    /// Explicit unique id for the second channel.
    #[serde(rename = "other_channel_id")]
    other_channel_id: Option<String>,
}

/// `asterisk.command`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct CommandInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    ami: AMIConn,
    /// Asterisk CLI command to run.
    command: String,
}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("asterisk", "0.1.0")
        .capabilities(Caps {
            conn: vec!["tcp:*:5038".into()],
            private_hosts: vec!["*".into()],
            secrets: vec![
                "ASTERISK_AMI_USERNAME".into(),
                "ASTERISK_AMI_SECRET".into(),
            ],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "username".into(),
            env: vec!["ASTERISK_AMI_USERNAME".into()],
            description: "Asterisk AMI username".into(),
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "secret".into(),
            env: vec!["ASTERISK_AMI_SECRET".into()],
            description: "Asterisk AMI secret/password".into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "asterisk.ami".into(),
            env: vec![
                "ASTERISK_AMI_HOST".into(),
                "ASTERISK_AMI_PORT".into(),
            ],
            http_hosts: Vec::new(),
            description: "Asterisk AMI endpoint host (ASTERISK_AMI_HOST) and port (ASTERISK_AMI_PORT, default 5038)".into(),
        })
        // ---- reads ----
        .operation(
            read_op_typed::<PingInput>(
                "asterisk.ami.ping",
                "Ping an Asterisk Manager Interface endpoint and return the greeting + pong.",
            ),
            ami_ping,
        )
        .operation(
            read_op_typed::<ChannelListInput>(
                "asterisk.channel.list",
                "List active Asterisk channels (live calls) with state, caller ID, dialplan position, application, and duration.",
            ),
            channel_list,
        )
        .operation(
            read_op_typed::<PeerListInput>(
                "asterisk.peer.list",
                "List Asterisk peers/endpoints (pjsip default, sip, or iax) with registration address and device status.",
            ),
            peer_list,
        )
        .operation(
            read_op_typed::<QueueStatusInput>(
                "asterisk.queue.status",
                "Show Asterisk call queues: stats, members with status/pause, and waiting callers.",
            ),
            queue_status,
        )
        .operation(
            read_op_typed::<DeviceStateListInput>(
                "asterisk.devicestate.list",
                "List Asterisk device states (NOT_INUSE, INUSE, RINGING, …), filterable by device-name substring.",
            ),
            devicestate_list,
        )
        // ---- writes (destructive) ----
        .operation(
            {
                let mut op = write_op_typed::<HangupInput>(
                    "asterisk.channel.hangup",
                    "Hang up one active Asterisk channel by exact name (terminates a live call).",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            channel_hangup,
        )
        .operation(
            {
                let mut op = write_op_typed::<OriginateInput>(
                    "asterisk.call.originate",
                    "Originate a call: dial channel first, then connect to exten+context or run application. Places a real call.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            call_originate,
        )
        .operation(
            {
                let mut op = write_op_typed::<CommandInput>(
                    "asterisk.command",
                    "Run an Asterisk CLI command over AMI and return its output. Powerful — CLI commands can mutate the PBX.",
                );
                op.risk = Some(Risk::High);
                op
            },
            ami_command,
        )
}

/// AMI key:value block.
type AmiBlock = HashMap<String, String>;

// ---------------------------------------------------------------------------
// AMI session: connect → greeting → login → execute → close.
// ---------------------------------------------------------------------------

/// Send one AMI action block (Key: Value\r\n lines + blank line terminator) with an ActionID.
fn ami_send(
    reader: &mut BufReader<ConnStream>,
    fields: &[(&str, &str)],
    action_id: &str,
) -> Result<(), String> {
    let mut buf = String::new();
    for (key, value) in fields {
        if !value.is_empty() {
            buf.push_str(key);
            buf.push_str(": ");
            buf.push_str(value);
            buf.push_str("\r\n");
        }
    }
    buf.push_str("ActionID: ");
    buf.push_str(action_id);
    buf.push_str("\r\n\r\n");
    reader
        .get_mut()
        .write_all(buf.as_bytes())
        .map_err(|e| format!("AMI write: {e}"))
}

/// Read one AMI message block (Key: Value lines up to a blank line).
/// Handles repeated "Output:" keys (joined with newlines) and the legacy
/// "Response: Follows" raw-output format (captured under "CommandOutput").
fn ami_read_block(reader: &mut BufReader<ConnStream>) -> Result<AmiBlock, String> {
    let mut out: AmiBlock = AmiBlock::new();
    let mut follows = false;
    let mut follows_body = false;
    let mut follows_lines: Vec<String> = Vec::new();

    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("AMI read: {e}"))?;
        if n == 0 {
            break; // EOF
        }
        // Trim trailing \r\n
        let line = line.trim_end_matches(['\r', '\n']).to_string();

        if line.trim().is_empty() {
            // Blank line: end of block
            if follows && !out.contains_key("CommandOutput") {
                out.insert("CommandOutput".into(), follows_lines.join("\n"));
            }
            break;
        }

        if follows {
            // Inside a "Response: Follows" block
            if line.trim() == "--END COMMAND--" {
                out.insert("CommandOutput".into(), follows_lines.join("\n"));
                follows_body = false;
                continue;
            }
            // Once we hit non-header content, everything is raw output
            if follows_body || !is_follows_header(&line) {
                follows_body = true;
                follows_lines.push(line);
                continue;
            }
        }

        if let Some(colon) = line.find(':') {
            let key = line[..colon].trim().to_string();
            let value = line[colon + 1..].trim().to_string();

            if key.eq_ignore_ascii_case("Response") && value.eq_ignore_ascii_case("Follows") {
                follows = true;
            }

            // Repeated keys (e.g. Output:) are joined with newlines
            let entry = out.entry(key).or_default();
            if entry.is_empty() {
                *entry = value;
            } else {
                entry.push('\n');
                entry.push_str(&value);
            }
        }
    }
    Ok(out)
}

/// Returns true if a line inside a "Response: Follows" block is still a protocol header.
fn is_follows_header(line: &str) -> bool {
    if let Some(colon) = line.find(':') {
        match line[..colon].trim().to_lowercase().as_str() {
            "actionid" | "privilege" | "message" => return true,
            _ => {}
        }
    }
    false
}

/// Send one action and return its single response block (skips stale/unsolicited messages).
fn ami_do(
    reader: &mut BufReader<ConnStream>,
    fields: &[(&str, &str)],
    counter: &mut u32,
) -> Result<AmiBlock, String> {
    *counter += 1;
    let action_id = format!("flux-{}", *counter);
    ami_send(reader, fields, &action_id)?;

    loop {
        let msg = ami_read_block(reader)?;
        // Skip messages from a different action
        if let Some(id) = msg.get("ActionID") {
            if !id.is_empty() && id != &action_id {
                continue;
            }
        }
        // Skip unsolicited events
        if msg
            .get("Response")
            .map(|s| s.as_str())
            .unwrap_or("")
            .is_empty()
            && !msg
                .get("Event")
                .map(|s| s.as_str())
                .unwrap_or("")
                .is_empty()
        {
            continue;
        }
        return Ok(msg);
    }
}

/// Send a list action and accumulate event blocks until a named completion event.
fn ami_collect(
    reader: &mut BufReader<ConnStream>,
    fields: &[(&str, &str)],
    counter: &mut u32,
    complete_events: &[&str],
) -> Result<(AmiBlock, Vec<AmiBlock>), String> {
    *counter += 1;
    let action_id = format!("flux-{}", *counter);
    ami_send(reader, fields, &action_id)?;

    let complete_set: Vec<String> = complete_events.iter().map(|s| s.to_lowercase()).collect();

    let mut response: Option<AmiBlock> = None;
    let mut events: Vec<AmiBlock> = Vec::new();

    loop {
        let msg = ami_read_block(reader)?;

        // Skip messages from a different action
        if let Some(id) = msg.get("ActionID") {
            if !id.is_empty() && id != &action_id {
                continue;
            }
        }

        let event_name = msg
            .get("Event")
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        let resp_val = msg
            .get("Response")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        if response.is_none() && !resp_val.is_empty() {
            if !resp_val.eq_ignore_ascii_case("Success") {
                let msg_text = msg
                    .get("Message")
                    .map(|s| s.as_str())
                    .unwrap_or(&resp_val)
                    .to_string();
                return Err(format!("AMI action failed: {msg_text}"));
            }
            response = Some(msg);
            continue;
        }

        if complete_set.iter().any(|c| c == &event_name) {
            return Ok((response.unwrap_or_default(), events));
        }

        if !event_name.is_empty() {
            events.push(msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Endpoint resolution: ASTERISK_AMI_HOST + ASTERISK_AMI_PORT from the host.
// ---------------------------------------------------------------------------

fn ami_address(host: &mut Host) -> Result<(String, u16), String> {
    // Try the declared endpoint; fall back to defaults.
    let ami_host = host
        .endpoint("asterisk.ami")
        .unwrap_or_else(|_| "localhost".into());

    // The endpoint env contains ASTERISK_AMI_HOST; ASTERISK_AMI_PORT is a
    // second env var.  In practice the host returns whatever ASTERISK_AMI_HOST
    // resolves to.  We attempt to parse a "host:port" from it; if it's bare we
    // append the default port 5038.
    let (h, p) = if ami_host.contains(':') {
        // Could be "host:port" or an IPv6 "[::1]:5038"
        if let Some(last_colon) = ami_host.rfind(':') {
            let port_str = &ami_host[last_colon + 1..];
            if let Ok(port) = port_str.parse::<u16>() {
                let host_part = ami_host[..last_colon]
                    .trim_matches('[')
                    .trim_matches(']')
                    .to_string();
                (host_part, port)
            } else {
                (ami_host, 5038)
            }
        } else {
            (ami_host, 5038)
        }
    } else {
        (ami_host, 5038)
    };

    Ok((h, p))
}

// ---------------------------------------------------------------------------
// Shared "run AMI session" wrapper.
// ---------------------------------------------------------------------------

/// Run a closure that uses an authenticated AMI session, then close the connection.
/// The closure receives: (reader, action_counter). The `timeout` (D-45) is forwarded to the
/// host's `conn.read` as a per-read deadline bounding the AMI request/response exchange.
fn with_ami<F, T>(host: &mut Host, timeout: Option<Duration>, f: F) -> Result<T, String>
where
    F: FnOnce(&mut BufReader<ConnStream>, &mut u32) -> Result<T, String>,
{
    let username = host.secret("username")?;
    let secret = host.secret("secret")?;
    let (ami_host, ami_port) = ami_address(host)?;

    let cid = host.conn_dial(ConnTarget::Tcp {
        host: &ami_host,
        port: ami_port,
    })?;

    let result = {
        let mut stream = ConnStream::new(host, cid);
        stream.set_read_deadline(timeout);
        let mut reader = BufReader::new(stream);
        let mut counter: u32 = 0;

        // Read greeting
        let mut greeting = String::new();
        reader
            .read_line(&mut greeting)
            .map_err(|e| format!("AMI greeting: {e}"))?;

        // Login
        let login_frame = format!(
            "Action: Login\r\nUsername: {username}\r\nSecret: {secret}\r\nEvents: off\r\n\r\n"
        );
        reader
            .get_mut()
            .write_all(login_frame.as_bytes())
            .map_err(|e| format!("AMI login write: {e}"))?;

        let login_resp = ami_read_block(&mut reader)?;
        let login_status = login_resp.get("Response").map(|s| s.as_str()).unwrap_or("");
        if !login_status.eq_ignore_ascii_case("Success") {
            let msg = login_resp
                .get("Message")
                .map(|s| s.as_str())
                .unwrap_or(login_status);
            return Err(format!("AMI login failed: {msg}"));
        }

        // Run the user closure
        let r = f(&mut reader, &mut counter);

        // Logoff (best-effort)
        counter += 1;
        let _ = ami_send(
            &mut reader,
            &[("Action", "Logoff")],
            &format!("flux-{counter}"),
        );

        r
    };

    // Close the connection
    let _ = host.conn_close(cid);
    result
}

// ---------------------------------------------------------------------------
// Input helpers.
// ---------------------------------------------------------------------------

fn flex_str(input: &Value, key: &str) -> Option<String> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn flex_i64(input: &Value, key: &str) -> Option<i64> {
    match input.get(key) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

fn flex_bool(input: &Value, key: &str) -> Option<bool> {
    match input.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => match s.trim().to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Parse and validate the optional `timeout` field shared by all ops. The host `conn.*`
/// capability does not currently expose a per-call timeout, so the duration is validated for
/// parity but not wired to the connection.
fn ami_timeout(input: &Value) -> Result<Option<Duration>, String> {
    match flex_str(input, "timeout") {
        Some(s) if !s.trim().is_empty() => parse_duration(&s).map(Some),
        _ => Ok(None),
    }
}

/// Tiny Go-style duration parser (`5s`, `1m30s`, `1h`). Errors on invalid/overflow input.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let mut nanos: u128 = 0;
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            return Err(format!("timeout: invalid duration {s:?}"));
        }
        let n: u64 = s[start..i]
            .parse()
            .map_err(|_| format!("timeout: invalid duration {s:?}"))?;
        let unit = if bytes[i..].starts_with(b"ns") {
            i += 2;
            "ns"
        } else if bytes[i..].starts_with(b"us") || bytes[i..].starts_with("µs".as_bytes()) {
            i += 2;
            "us"
        } else if bytes[i..].starts_with(b"ms") {
            i += 2;
            "ms"
        } else if bytes.get(i) == Some(&b's') {
            i += 1;
            "s"
        } else if bytes.get(i) == Some(&b'm') {
            i += 1;
            "m"
        } else if bytes.get(i) == Some(&b'h') {
            i += 1;
            "h"
        } else {
            return Err(format!("timeout: invalid duration {s:?}"));
        };
        let unit_nanos: u128 = match unit {
            "ns" => 1,
            "us" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            _ => return Err(format!("timeout: invalid duration {s:?}")),
        };
        nanos = nanos
            .checked_add(
                u128::from(n)
                    .checked_mul(unit_nanos)
                    .ok_or_else(|| format!("timeout: duration overflow {s:?}"))?,
            )
            .ok_or_else(|| format!("timeout: duration overflow {s:?}"))?;
    }
    if i == 0 {
        return Err(format!("timeout: invalid duration {s:?}"));
    }
    if nanos > u128::from(u64::MAX) {
        return Err(format!("timeout: duration overflow {s:?}"));
    }
    Ok(Duration::from_nanos(nanos as u64))
}

// ---------------------------------------------------------------------------
// Queue member status decoder (mirrors fluxplane source).
// ---------------------------------------------------------------------------

fn queue_member_status(value: &str) -> &str {
    match value.trim() {
        "0" => "unknown",
        "1" => "not_in_use",
        "2" => "in_use",
        "3" => "busy",
        "4" => "invalid",
        "5" => "unavailable",
        "6" => "ringing",
        "7" => "ring_in_use",
        "8" => "on_hold",
        other => other,
    }
}

fn atoi_safe(value: &str) -> i64 {
    value.trim().parse().unwrap_or(0)
}

fn first_non_empty<'a>(a: &'a str, b: &'a str) -> &'a str {
    if !a.trim().is_empty() {
        a
    } else {
        b
    }
}

fn active_channels_comment(value: &str) -> String {
    match value.trim() {
        "" | "0" => String::new(),
        other => format!("{other} active channel(s)"),
    }
}

// ---------------------------------------------------------------------------
// Op handlers.
// ---------------------------------------------------------------------------

fn ami_ping(input: Value, host: &mut Host) -> Result<Value, String> {
    let timeout = ami_timeout(&input)?;
    let start = Instant::now();
    let username = host.secret("username")?;
    let secret = host.secret("secret")?;
    let (ami_host, ami_port) = ami_address(host)?;

    let cid = host.conn_dial(ConnTarget::Tcp {
        host: &ami_host,
        port: ami_port,
    })?;

    let result = (|| {
        let mut stream = ConnStream::new(host, cid);
        stream.set_read_deadline(timeout);
        let mut reader = BufReader::new(stream);
        let mut counter: u32 = 0;

        let mut greeting_line = String::new();
        reader
            .read_line(&mut greeting_line)
            .map_err(|e| format!("AMI greeting: {e}"))?;
        let greeting = greeting_line.trim().to_string();

        let login_frame = format!(
            "Action: Login\r\nUsername: {username}\r\nSecret: {secret}\r\nEvents: off\r\n\r\n"
        );
        reader
            .get_mut()
            .write_all(login_frame.as_bytes())
            .map_err(|e| format!("AMI login write: {e}"))?;

        let login_resp = ami_read_block(&mut reader)?;
        let login_status = login_resp.get("Response").map(|s| s.as_str()).unwrap_or("");
        if !login_status.eq_ignore_ascii_case("Success") {
            let msg = login_resp
                .get("Message")
                .map(|s| s.as_str())
                .unwrap_or(login_status);
            return Err(format!("AMI login failed: {msg}"));
        }

        let pong = ami_do(&mut reader, &[("Action", "Ping")], &mut counter)?;
        let response = pong
            .get("Response")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let ping_val = pong
            .get("Ping")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let msg = pong
            .get("Message")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let ok = response.eq_ignore_ascii_case("Success") && ping_val.eq_ignore_ascii_case("Pong");

        counter += 1;
        let _ = ami_send(
            &mut reader,
            &[("Action", "Logoff")],
            &format!("flux-{counter}"),
        );

        Ok(json!({
            "ok": ok,
            "greeting": greeting,
            "authenticated": true,
            "pong": ok,
            "response": response,
            "message": first_non_empty(&ping_val, &msg),
            "duration_ms": start.elapsed().as_millis() as i64,
        }))
    })();

    let _ = host.conn_close(cid);
    result
}

fn channel_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let timeout = ami_timeout(&input)?;
    let limit = flex_i64(&input, "limit").unwrap_or(0);

    with_ami(host, timeout, |reader, counter| {
        let (_, events) = ami_collect(
            reader,
            &[("Action", "CoreShowChannels")],
            counter,
            &["CoreShowChannelsComplete"],
        )?;

        let mut channels: Vec<Value> = Vec::new();
        for event in &events {
            let event_name = event.get("Event").map(|s| s.as_str()).unwrap_or("");
            if !event_name.eq_ignore_ascii_case("CoreShowChannel") {
                continue;
            }
            channels.push(json!({
                "channel":          event.get("Channel").map(|s| s.as_str()).unwrap_or(""),
                "unique_id":        event.get("Uniqueid").map(|s| s.as_str()).unwrap_or(""),
                "state":            first_non_empty(
                                        event.get("ChannelStateDesc").map(|s| s.as_str()).unwrap_or(""),
                                        event.get("ChannelState").map(|s| s.as_str()).unwrap_or(""),
                                    ),
                "caller_id_num":    event.get("CallerIDNum").map(|s| s.as_str()).unwrap_or(""),
                "caller_id_name":   event.get("CallerIDName").map(|s| s.as_str()).unwrap_or(""),
                "connected_num":    event.get("ConnectedLineNum").map(|s| s.as_str()).unwrap_or(""),
                "context":          event.get("Context").map(|s| s.as_str()).unwrap_or(""),
                "exten":            event.get("Exten").map(|s| s.as_str()).unwrap_or(""),
                "application":      event.get("Application").map(|s| s.as_str()).unwrap_or(""),
                "application_data": event.get("ApplicationData").map(|s| s.as_str()).unwrap_or(""),
                "duration":         event.get("Duration").map(|s| s.as_str()).unwrap_or(""),
                "bridge_id":        event.get("BridgeId").map(|s| s.as_str()).unwrap_or(""),
                "account_code":     event.get("AccountCode").map(|s| s.as_str()).unwrap_or(""),
            }));
        }

        if limit > 0 && (limit as usize) < channels.len() {
            channels.truncate(limit as usize);
        }

        let count = channels.len();
        Ok(json!({ "count": count, "channels": channels }))
    })
}

fn peer_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let timeout = ami_timeout(&input)?;
    let technology = flex_str(&input, "technology")
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "pjsip".into());
    let limit = flex_i64(&input, "limit").unwrap_or(0);

    let (action, complete_events): (&str, &[&str]) = match technology.as_str() {
        "pjsip" => ("PJSIPShowEndpoints", &["EndpointListComplete"]),
        "sip" => ("SIPpeers", &["PeerlistComplete"]),
        "iax" => ("IAXpeerlist", &["PeerlistComplete"]),
        other => {
            return Err(format!(
                "technology must be pjsip, sip, or iax; got {other:?}"
            ))
        }
    };

    with_ami(host, timeout, |reader, counter| {
        let collect_result = ami_collect(reader, &[("Action", action)], counter, complete_events);

        let events = match collect_result {
            Ok((_, evs)) => evs,
            Err(e) => {
                // PJSIPShowEndpoints returns an Error response when zero endpoints configured
                if e.to_lowercase().contains("no endpoints found") {
                    return Ok(json!({ "count": 0, "technology": technology, "peers": [] }));
                }
                return Err(e);
            }
        };

        let mut peers: Vec<Value> = Vec::new();
        for event in &events {
            let ev_name = event
                .get("Event")
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            match ev_name.as_str() {
                "endpointlist" => {
                    // PJSIP
                    peers.push(json!({
                        "technology": "pjsip",
                        "name":    event.get("ObjectName").map(|s| s.as_str()).unwrap_or(""),
                        "address": event.get("Contacts").map(|s| s.as_str()).unwrap_or(""),
                        "status":  first_non_empty(
                                       event.get("DeviceState").map(|s| s.as_str()).unwrap_or(""),
                                       event.get("State").map(|s| s.as_str()).unwrap_or(""),
                                   ),
                        "dynamic": false,
                        "comment": active_channels_comment(
                                       event.get("ActiveChannels").map(|s| s.as_str()).unwrap_or(""),
                                   ),
                    }));
                }
                "peerentry" => {
                    // SIP / IAX
                    let ip = event.get("IPaddress").map(|s| s.as_str()).unwrap_or("");
                    let port = event.get("IPport").map(|s| s.as_str()).unwrap_or("");
                    let address =
                        if !ip.is_empty() && ip != "-none-" && !port.is_empty() && port != "0" {
                            format!("{ip}:{port}")
                        } else {
                            ip.to_string()
                        };
                    let tech = event
                        .get("Channeltype")
                        .map(|s| s.to_lowercase())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| technology.clone());
                    peers.push(json!({
                        "technology": tech,
                        "name":    event.get("ObjectName").map(|s| s.as_str()).unwrap_or(""),
                        "address": address,
                        "status":  event.get("Status").map(|s| s.as_str()).unwrap_or(""),
                        "dynamic": event.get("Dynamic").map(|s| s.eq_ignore_ascii_case("yes")).unwrap_or(false),
                        "comment": event.get("Description").map(|s| s.as_str()).unwrap_or(""),
                    }));
                }
                _ => {}
            }
        }

        if limit > 0 && (limit as usize) < peers.len() {
            peers.truncate(limit as usize);
        }

        let count = peers.len();
        Ok(json!({ "count": count, "technology": technology, "peers": peers }))
    })
}

fn queue_status(input: Value, host: &mut Host) -> Result<Value, String> {
    let timeout = ami_timeout(&input)?;
    let queue_filter = flex_str(&input, "queue").unwrap_or_default();

    with_ami(host, timeout, |reader, counter| {
        let mut action_fields = vec![("Action", "QueueStatus")];
        let queue_filter_ref: &str = &queue_filter;
        if !queue_filter_ref.is_empty() {
            action_fields.push(("Queue", queue_filter_ref));
        }

        let (_, events) = ami_collect(reader, &action_fields, counter, &["QueueStatusComplete"])?;

        // Aggregate events into queue records (preserving insertion order)
        let mut by_name: HashMap<String, Value> = HashMap::new();
        let mut order: Vec<String> = Vec::new();

        macro_rules! queue_for {
            ($name:expr) => {{
                let name: String = $name;
                if name.is_empty() {
                    None
                } else {
                    if !by_name.contains_key(&name) {
                        by_name.insert(name.clone(), json!({
                            "name": name,
                            "calls": 0,
                            "members": [],
                            "callers": [],
                        }));
                        order.push(name.clone());
                    }
                    Some(name)
                }
            }};
        }

        for event in &events {
            let ev_name = event
                .get("Event")
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            let q_name = event
                .get("Queue")
                .map(|s| s.to_string())
                .unwrap_or_default();

            match ev_name.as_str() {
                "queueparams" => {
                    if let Some(name) = queue_for!(q_name) {
                        if let Some(rec) = by_name.get_mut(&name) {
                            rec["strategy"] =
                                json!(event.get("Strategy").map(|s| s.as_str()).unwrap_or(""));
                            rec["calls"] = json!(atoi_safe(
                                event.get("Calls").map(|s| s.as_str()).unwrap_or("0")
                            ));
                            rec["hold_time"] = json!(atoi_safe(
                                event.get("Holdtime").map(|s| s.as_str()).unwrap_or("0")
                            ));
                            rec["talk_time"] = json!(atoi_safe(
                                event.get("TalkTime").map(|s| s.as_str()).unwrap_or("0")
                            ));
                            rec["completed"] = json!(atoi_safe(
                                event.get("Completed").map(|s| s.as_str()).unwrap_or("0")
                            ));
                            rec["abandoned"] = json!(atoi_safe(
                                event.get("Abandoned").map(|s| s.as_str()).unwrap_or("0")
                            ));
                            rec["service_level"] = json!(atoi_safe(
                                event.get("ServiceLevel").map(|s| s.as_str()).unwrap_or("0")
                            ));
                        }
                    }
                }
                "queuemember" => {
                    if let Some(name) = queue_for!(q_name) {
                        if let Some(rec) = by_name.get_mut(&name) {
                            let iface = first_non_empty(
                                first_non_empty(
                                    event
                                        .get("StateInterface")
                                        .map(|s| s.as_str())
                                        .unwrap_or(""),
                                    event.get("Location").map(|s| s.as_str()).unwrap_or(""),
                                ),
                                event.get("Interface").map(|s| s.as_str()).unwrap_or(""),
                            )
                            .to_string();
                            let member = json!({
                                "interface":  iface,
                                "name":       first_non_empty(
                                                  event.get("MemberName").map(|s| s.as_str()).unwrap_or(""),
                                                  event.get("Name").map(|s| s.as_str()).unwrap_or(""),
                                              ),
                                "membership": event.get("Membership").map(|s| s.as_str()).unwrap_or(""),
                                "penalty":    atoi_safe(event.get("Penalty").map(|s| s.as_str()).unwrap_or("0")),
                                "calls_taken":atoi_safe(event.get("CallsTaken").map(|s| s.as_str()).unwrap_or("0")),
                                "status":     queue_member_status(event.get("Status").map(|s| s.as_str()).unwrap_or("")),
                                "paused":     event.get("Paused").map(|s| s == "1").unwrap_or(false),
                                "in_call":    event.get("InCall").map(|s| s == "1").unwrap_or(false),
                            });
                            rec["members"].as_array_mut().unwrap().push(member);
                        }
                    }
                }
                "queueentry" => {
                    if let Some(name) = queue_for!(q_name) {
                        if let Some(rec) = by_name.get_mut(&name) {
                            let caller = json!({
                                "position":      atoi_safe(event.get("Position").map(|s| s.as_str()).unwrap_or("0")),
                                "channel":       event.get("Channel").map(|s| s.as_str()).unwrap_or(""),
                                "caller_id_num": event.get("CallerIDNum").map(|s| s.as_str()).unwrap_or(""),
                                "caller_id_name":event.get("CallerIDName").map(|s| s.as_str()).unwrap_or(""),
                                "wait_seconds":  atoi_safe(event.get("Wait").map(|s| s.as_str()).unwrap_or("0")),
                            });
                            rec["callers"].as_array_mut().unwrap().push(caller);
                        }
                    }
                }
                _ => {}
            }
        }

        let queues: Vec<Value> = order
            .iter()
            .filter_map(|n| by_name.get(n).cloned())
            .collect();
        let count = queues.len();
        Ok(json!({ "count": count, "queues": queues }))
    })
}

fn devicestate_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let timeout = ami_timeout(&input)?;
    let device_filter = flex_str(&input, "device")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let limit = flex_i64(&input, "limit").unwrap_or(0);

    with_ami(host, timeout, |reader, counter| {
        let (_, events) = ami_collect(
            reader,
            &[("Action", "DeviceStateList")],
            counter,
            &["DeviceStateListComplete"],
        )?;

        let mut states: Vec<Value> = Vec::new();
        for event in &events {
            let ev_name = event.get("Event").map(|s| s.as_str()).unwrap_or("");
            if !ev_name.eq_ignore_ascii_case("DeviceStateChange") {
                continue;
            }
            let device = event.get("Device").map(|s| s.as_str()).unwrap_or("");
            if !device_filter.is_empty() && !device.to_lowercase().contains(&device_filter) {
                continue;
            }
            states.push(json!({
                "device": device,
                "state":  event.get("State").map(|s| s.as_str()).unwrap_or(""),
            }));
        }

        if limit > 0 && (limit as usize) < states.len() {
            states.truncate(limit as usize);
        }

        let count = states.len();
        Ok(json!({ "count": count, "states": states }))
    })
}

fn channel_hangup(input: Value, host: &mut Host) -> Result<Value, String> {
    let timeout = ami_timeout(&input)?;
    let channel = flex_str(&input, "channel").ok_or("`channel` (string) required")?;
    let cause = flex_i64(&input, "cause").unwrap_or(0);

    with_ami(host, timeout, |reader, counter| {
        let cause_str = cause.to_string();
        let mut fields = vec![("Action", "Hangup"), ("Channel", channel.as_str())];
        if cause > 0 {
            fields.push(("Cause", cause_str.as_str()));
        }

        let resp = ami_do(reader, &fields, counter)?;
        let response = resp
            .get("Response")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let message = resp
            .get("Message")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let ok = response.eq_ignore_ascii_case("Success");

        if !ok {
            return Err(format!(
                "hangup failed: {}",
                first_non_empty(&message, &response)
            ));
        }

        Ok(json!({
            "ok": true,
            "channel": channel,
            "response": response,
            "message": message,
        }))
    })
}

fn call_originate(input: Value, host: &mut Host) -> Result<Value, String> {
    let timeout = ami_timeout(&input)?;
    let channel = flex_str(&input, "channel").ok_or("`channel` (string) required")?;
    let exten = flex_str(&input, "exten").unwrap_or_default();
    let context = flex_str(&input, "context").unwrap_or_default();
    let application = flex_str(&input, "application").unwrap_or_default();
    let data = flex_str(&input, "data").unwrap_or_default();
    let caller_id = flex_str(&input, "caller_id").unwrap_or_default();
    let timeout_ms = flex_i64(&input, "timeout_ms").unwrap_or(30000);
    let do_async = flex_bool(&input, "async").unwrap_or(true);
    let account_code = flex_str(&input, "account_code").unwrap_or_default();
    let priority = flex_i64(&input, "priority").unwrap_or(1).max(1);
    let early_media = flex_bool(&input, "early_media").unwrap_or(false);
    let channel_id = flex_str(&input, "channel_id").unwrap_or_default();
    let other_channel_id = flex_str(&input, "other_channel_id").unwrap_or_default();

    // Validate
    if exten.is_empty() && application.is_empty() {
        return Err("provide exten+context or application".into());
    }
    if !exten.is_empty() && !application.is_empty() {
        return Err("exten and application are mutually exclusive".into());
    }
    if !exten.is_empty() && context.is_empty() {
        return Err("`context` is required when `exten` is set".into());
    }

    // Build variable string
    let variable_str = if let Some(vars) = input.get("variables").and_then(|v| v.as_object()) {
        let pairs: Vec<String> = vars
            .iter()
            .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or_default()))
            .collect();
        pairs.join(",")
    } else {
        String::new()
    };

    let timeout_str = timeout_ms.to_string();
    let priority_str = priority.to_string();

    with_ami(host, timeout, |reader, counter| {
        let mut fields: Vec<(&str, &str)> =
            vec![("Action", "Originate"), ("Channel", channel.as_str())];

        if !exten.is_empty() {
            fields.push(("Exten", exten.as_str()));
            fields.push(("Context", context.as_str()));
            fields.push(("Priority", priority_str.as_str()));
        } else {
            fields.push(("Application", application.as_str()));
            if !data.is_empty() {
                fields.push(("Data", data.as_str()));
            }
        }

        fields.push(("Timeout", timeout_str.as_str()));

        if !caller_id.is_empty() {
            fields.push(("CallerID", caller_id.as_str()));
        }
        if do_async {
            fields.push(("Async", "true"));
        }
        if !account_code.is_empty() {
            fields.push(("Account", account_code.as_str()));
        }
        if early_media {
            fields.push(("EarlyMedia", "true"));
        }
        if !channel_id.is_empty() {
            fields.push(("ChannelId", channel_id.as_str()));
        }
        if !other_channel_id.is_empty() {
            fields.push(("OtherChannelId", other_channel_id.as_str()));
        }
        if !variable_str.is_empty() {
            fields.push(("Variable", variable_str.as_str()));
        }

        let resp = ami_do(reader, &fields, counter)?;
        let response = resp
            .get("Response")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let message = resp
            .get("Message")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let unique_id = resp
            .get("Uniqueid")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let ok = response.eq_ignore_ascii_case("Success");

        if !ok {
            return Err(format!(
                "originate failed: {}",
                first_non_empty(&message, &response)
            ));
        }

        Ok(json!({
            "ok": true,
            "channel": channel,
            "response": response,
            "message": message,
            "unique_id": unique_id,
        }))
    })
}

fn ami_command(input: Value, host: &mut Host) -> Result<Value, String> {
    let timeout = ami_timeout(&input)?;
    let command = flex_str(&input, "command").ok_or("`command` (string) required")?;

    with_ami(host, timeout, |reader, counter| {
        let resp = ami_do(
            reader,
            &[("Action", "Command"), ("Command", command.as_str())],
            counter,
        )?;

        let response = resp
            .get("Response")
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if !response.eq_ignore_ascii_case("Success") && !response.eq_ignore_ascii_case("Follows") {
            let msg = resp
                .get("Message")
                .map(|s| s.as_str())
                .unwrap_or(&response)
                .to_string();
            return Err(format!("AMI command failed: {msg}"));
        }

        let raw_output = first_non_empty(
            resp.get("Output").map(|s| s.as_str()).unwrap_or(""),
            resp.get("CommandOutput").map(|s| s.as_str()).unwrap_or(""),
        )
        .to_string();
        let output = raw_output.trim_end_matches('\n').to_string();

        let lines: Vec<&str> = if output.is_empty() {
            Vec::new()
        } else {
            output.split('\n').collect()
        };

        Ok(json!({
            "command": command,
            "output": output,
            "lines": lines,
        }))
    })
}

// ---------------------------------------------------------------------------
// Entry point.
// ---------------------------------------------------------------------------

fn main() {
    manifest_builder().serve();
}

// ---------------------------------------------------------------------------
// Tests — one MockHost test per op (8 total).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // AMI frames use \r\n line endings.  This helper builds a canned server byte
    // sequence from a list of "Key: Value" strings, terminated by a blank line.
    fn frame(lines: &[&str]) -> Vec<u8> {
        let mut s = String::new();
        for line in lines {
            s.push_str(line);
            s.push_str("\r\n");
        }
        s.push_str("\r\n");
        s.into_bytes()
    }

    // Concatenate multiple frames into one byte blob.
    fn frames(chunks: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend_from_slice(chunk);
        }
        out
    }

    // Build a MockHost with the mandatory greeting + login-success pre-loaded, then
    // any extra server bytes, and the two auth secrets.
    fn mock_with(extra: &[u8]) -> MockHost {
        let greeting = b"Asterisk Call Manager/2.10.4\r\n".to_vec();
        let login_ok = frame(&["Response: Success", "Message: Authentication accepted"]);
        let mut buf = Vec::new();
        buf.extend_from_slice(&greeting);
        buf.extend_from_slice(&login_ok);
        buf.extend_from_slice(extra);
        // Queue as one chunk so BufReader can read it line by line.
        MockHost::default()
            .with_conn_response(buf)
            .with_secret("username", "admin")
            .with_secret("secret", "s3cret")
    }

    fn call(op: &str, input: Value, mock: &mut MockHost) -> Result<Value, String> {
        let plugin = manifest_builder().build();
        plugin.call(op, input, mock)
    }

    // ---- 1. ami.ping -------------------------------------------------------

    #[test]
    fn test_ami_ping() {
        let pong = frame(&["Response: Success", "ActionID: flux-1", "Ping: Pong"]);
        // After Pong, server must also service the Logoff.
        let logoff_resp = frame(&["Response: Goodbye", "Message: Thanks for all the fish."]);
        let server = frames(&[&pong, &logoff_resp]);
        let mut mock = mock_with(&server);

        let result = call("asterisk.ami.ping", json!({}), &mut mock).expect("ping");
        assert_eq!(result["ok"], true);
        assert_eq!(result["pong"], true);
        assert!(result["greeting"].as_str().unwrap().contains("Asterisk"));
    }

    // ---- 2. channel.list ---------------------------------------------------

    #[test]
    fn test_channel_list() {
        let action_resp = frame(&[
            "Response: Success",
            "ActionID: flux-1",
            "EventList: start",
            "Message: Channels will follow",
        ]);
        let ch_event = frame(&[
            "Event: CoreShowChannel",
            "ActionID: flux-1",
            "Channel: PJSIP/agent-7-00000123",
            "Uniqueid: 1717920000.123",
            "ChannelStateDesc: Up",
            "CallerIDNum: 7000",
            "Application: Queue",
            "Duration: 00:02:13",
        ]);
        let complete = frame(&[
            "Event: CoreShowChannelsComplete",
            "ActionID: flux-1",
            "EventList: Complete",
            "ListItems: 1",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &ch_event, &complete, &logoff]);
        let mut mock = mock_with(&server);

        let result = call("asterisk.channel.list", json!({}), &mut mock).expect("channel.list");
        assert_eq!(result["count"], 1);
        let ch = &result["channels"][0];
        assert_eq!(ch["channel"], "PJSIP/agent-7-00000123");
        assert_eq!(ch["state"], "Up");
        assert_eq!(ch["application"], "Queue");
        assert_eq!(ch["duration"], "00:02:13");
    }

    // ---- 3. peer.list (PJSIP) ----------------------------------------------

    #[test]
    fn test_peer_list_pjsip() {
        let action_resp = frame(&["Response: Success", "ActionID: flux-1", "EventList: start"]);
        let ep_event = frame(&[
            "Event: EndpointList",
            "ActionID: flux-1",
            "ObjectName: agent-7",
            "Contacts: agent-7/sip:agent-7@10.0.0.9:5060",
            "DeviceState: Not in use",
            "ActiveChannels: 0",
        ]);
        let complete = frame(&[
            "Event: EndpointListComplete",
            "ActionID: flux-1",
            "EventList: Complete",
            "ListItems: 1",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &ep_event, &complete, &logoff]);
        let mut mock = mock_with(&server);

        let result = call(
            "asterisk.peer.list",
            json!({"technology": "pjsip"}),
            &mut mock,
        )
        .expect("peer.list pjsip");
        assert_eq!(result["technology"], "pjsip");
        assert_eq!(result["count"], 1);
        assert_eq!(result["peers"][0]["name"], "agent-7");
        assert_eq!(result["peers"][0]["status"], "Not in use");
    }

    // ---- 4. queue.status ---------------------------------------------------

    #[test]
    fn test_queue_status() {
        let action_resp = frame(&["Response: Success", "ActionID: flux-1", "EventList: start"]);
        let params = frame(&[
            "Event: QueueParams",
            "ActionID: flux-1",
            "Queue: support",
            "Strategy: rrmemory",
            "Calls: 2",
            "Holdtime: 45",
            "Completed: 17",
            "Abandoned: 3",
            "ServiceLevel: 60",
        ]);
        let member = frame(&[
            "Event: QueueMember",
            "ActionID: flux-1",
            "Queue: support",
            "MemberName: Agent Seven",
            "StateInterface: PJSIP/agent-7",
            "Membership: dynamic",
            "Status: 2",
            "Paused: 1",
            "InCall: 1",
        ]);
        let caller = frame(&[
            "Event: QueueEntry",
            "ActionID: flux-1",
            "Queue: support",
            "Position: 1",
            "Channel: PJSIP/caller-0000007b",
            "CallerIDNum: 4930123456",
            "Wait: 37",
        ]);
        let complete = frame(&[
            "Event: QueueStatusComplete",
            "ActionID: flux-1",
            "EventList: Complete",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &params, &member, &caller, &complete, &logoff]);
        let mut mock = mock_with(&server);

        let result = call(
            "asterisk.queue.status",
            json!({"queue": "support"}),
            &mut mock,
        )
        .expect("queue.status");
        assert_eq!(result["count"], 1);
        let q = &result["queues"][0];
        assert_eq!(q["name"], "support");
        assert_eq!(q["strategy"], "rrmemory");
        assert_eq!(q["calls"], 2);
        assert_eq!(q["abandoned"], 3);
        assert_eq!(q["members"][0]["status"], "in_use");
        assert_eq!(q["members"][0]["paused"], true);
        assert_eq!(q["callers"][0]["position"], 1);
        assert_eq!(q["callers"][0]["wait_seconds"], 37);
    }

    // ---- 5. devicestate.list -----------------------------------------------

    #[test]
    fn test_devicestate_list() {
        let action_resp = frame(&["Response: Success", "ActionID: flux-1", "EventList: start"]);
        let ds1 = frame(&[
            "Event: DeviceStateChange",
            "ActionID: flux-1",
            "Device: PJSIP/agent-7",
            "State: NOT_INUSE",
        ]);
        let ds2 = frame(&[
            "Event: DeviceStateChange",
            "ActionID: flux-1",
            "Device: PJSIP/agent-9",
            "State: RINGING",
        ]);
        let complete = frame(&[
            "Event: DeviceStateListComplete",
            "ActionID: flux-1",
            "EventList: Complete",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &ds1, &ds2, &complete, &logoff]);
        let mut mock = mock_with(&server);

        let result = call(
            "asterisk.devicestate.list",
            json!({"device": "agent-9"}),
            &mut mock,
        )
        .expect("devicestate.list");
        assert_eq!(result["count"], 1);
        assert_eq!(result["states"][0]["device"], "PJSIP/agent-9");
        assert_eq!(result["states"][0]["state"], "RINGING");
    }

    // ---- 6. channel.hangup -------------------------------------------------

    #[test]
    fn test_channel_hangup() {
        let action_resp = frame(&[
            "Response: Success",
            "ActionID: flux-1",
            "Message: Channel Hungup",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &logoff]);
        let mut mock = mock_with(&server);

        let result = call(
            "asterisk.channel.hangup",
            json!({"channel": "PJSIP/agent-7-00000123", "cause": 16}),
            &mut mock,
        )
        .expect("channel.hangup");
        assert_eq!(result["ok"], true);
        assert_eq!(result["channel"], "PJSIP/agent-7-00000123");
    }

    // ---- 7. call.originate -------------------------------------------------

    #[test]
    fn test_call_originate() {
        let action_resp = frame(&[
            "Response: Success",
            "ActionID: flux-1",
            "Message: Originate successfully queued",
            "Uniqueid: 1717920000.55",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &logoff]);
        let mut mock = mock_with(&server);

        let result = call(
            "asterisk.call.originate",
            json!({
                "channel":   "PJSIP/agent-7",
                "exten":     "100",
                "context":   "from-internal",
                "caller_id": "Flux <7000>",
                "timeout_ms": 30000
            }),
            &mut mock,
        )
        .expect("call.originate");
        assert_eq!(result["ok"], true);
        assert_eq!(result["unique_id"], "1717920000.55");
    }

    // ---- 8. command --------------------------------------------------------

    #[test]
    fn test_ami_command_modern_output() {
        // Modern Asterisk returns repeated Output: headers
        let action_resp = frame(&[
            "Response: Success",
            "ActionID: flux-1",
            "Output: System uptime: 3 weeks",
            "Output: Last reload: 2 days",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &logoff]);
        let mut mock = mock_with(&server);

        let result = call(
            "asterisk.command",
            json!({"command": "core show uptime"}),
            &mut mock,
        )
        .expect("command");
        // Repeated Output: keys are joined with \n
        assert_eq!(result["command"], "core show uptime");
        let output = result["output"].as_str().unwrap();
        assert!(
            output.contains("System uptime: 3 weeks"),
            "output={output:?}"
        );
        assert!(output.contains("Last reload: 2 days"), "output={output:?}");
        assert_eq!(result["lines"].as_array().unwrap().len(), 2);

        // Timeout field is accepted without changing behavior.
        let mut mock2 = mock_with(&server);
        let result2 = call(
            "asterisk.command",
            json!({"command": "core show uptime", "timeout": "5s"}),
            &mut mock2,
        )
        .expect("command with timeout");
        assert_eq!(result2["command"], "core show uptime");
    }

    // ---- parity / D-36 tests -----------------------------------------------

    #[test]
    fn test_ami_ping_rejects_invalid_timeout() {
        let mut mock = MockHost::default();
        let result = call("asterisk.ami.ping", json!({"timeout": "5x"}), &mut mock);
        assert!(result.is_err(), "expected invalid timeout to fail");
        assert!(
            result.unwrap_err().contains("timeout"),
            "error should mention timeout"
        );
    }

    #[test]
    fn test_ami_ping_timeout_is_enforced_when_no_greeting() {
        // Failing-first for D-45: a valid `timeout` forwards a per-read deadline to the host's
        // `conn.read`. With no greeting queued, the first read returns ErrorKind::TimedOut (not a
        // silent hang) — surfaced via the greeting read's error. Before the wiring it would be a
        // clean EOF ("AMI greeting: ... failed to fill whole buffer" or empty).
        let mut mock = MockHost::default()
            .with_secret("username", "admin")
            .with_secret("secret", "s3cret");
        let result = call("asterisk.ami.ping", json!({"timeout": "1ms"}), &mut mock);
        assert!(result.is_err(), "expected a timeout error");
        assert!(
            result.unwrap_err().contains("timed out"),
            "error should mention timed out"
        );
    }

    #[test]
    fn test_peer_list_pjsip_includes_active_channels_comment() {
        let action_resp = frame(&["Response: Success", "ActionID: flux-1", "EventList: start"]);
        let ep_event = frame(&[
            "Event: EndpointList",
            "ActionID: flux-1",
            "ObjectName: agent-7",
            "Contacts: agent-7/sip:agent-7@10.0.0.9:5060",
            "DeviceState: Not in use",
            "ActiveChannels: 3",
        ]);
        let complete = frame(&[
            "Event: EndpointListComplete",
            "ActionID: flux-1",
            "EventList: Complete",
            "ListItems: 1",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &ep_event, &complete, &logoff]);
        let mut mock = mock_with(&server);

        let result = call("asterisk.peer.list", json!({}), &mut mock).expect("peer.list");
        assert_eq!(result["count"], 1);
        let peer = &result["peers"][0];
        assert_eq!(peer["name"], "agent-7");
        assert_eq!(peer["comment"], "3 active channel(s)");
    }

    #[test]
    fn test_call_originate_sends_extra_ami_fields() {
        let action_resp = frame(&[
            "Response: Success",
            "ActionID: flux-1",
            "Message: Originate successfully queued",
            "Uniqueid: 1717920000.55",
        ]);
        let logoff = frame(&["Response: Goodbye"]);
        let server = frames(&[&action_resp, &logoff]);
        let mut mock = mock_with(&server);

        let result = call(
            "asterisk.call.originate",
            json!({
                "channel":          "PJSIP/agent-7",
                "application":      "Playback",
                "data":             "hello",
                "early_media":      true,
                "channel_id":       "chan-1",
                "other_channel_id": "chan-2"
            }),
            &mut mock,
        )
        .expect("call.originate extra fields");
        assert_eq!(result["ok"], true);

        let written = String::from_utf8(mock.conn_buf.borrow().clone()).unwrap();
        assert!(
            written.contains("EarlyMedia: true"),
            "missing EarlyMedia in AMI request: {written}"
        );
        assert!(
            written.contains("ChannelId: chan-1"),
            "missing ChannelId in AMI request: {written}"
        );
        assert!(
            written.contains("OtherChannelId: chan-2"),
            "missing OtherChannelId in AMI request: {written}"
        );
    }
}

#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        Object,
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

    fn shared() -> Vec<Prop> {
        vec![p("timeout", Kind::Str)]
    }

    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            ("asterisk.ami.ping", c(shared(), vec![])),
            (
                "asterisk.channel.list",
                c(
                    {
                        let mut v = shared();
                        v.push(p("limit", Kind::Int));
                        v
                    },
                    vec![],
                ),
            ),
            (
                "asterisk.peer.list",
                c(
                    {
                        let mut v = shared();
                        v.push(p("technology", Kind::Str));
                        v.push(p("limit", Kind::Int));
                        v
                    },
                    vec![],
                ),
            ),
            (
                "asterisk.queue.status",
                c(
                    {
                        let mut v = shared();
                        v.push(p("queue", Kind::Str));
                        v
                    },
                    vec![],
                ),
            ),
            (
                "asterisk.devicestate.list",
                c(
                    {
                        let mut v = shared();
                        v.push(p("device", Kind::Str));
                        v.push(p("limit", Kind::Int));
                        v
                    },
                    vec![],
                ),
            ),
            (
                "asterisk.channel.hangup",
                c(
                    {
                        let mut v = shared();
                        v.push(p("channel", Kind::Str));
                        v.push(p("cause", Kind::Int));
                        v
                    },
                    vec!["channel"],
                ),
            ),
            (
                "asterisk.call.originate",
                c(
                    {
                        let mut v = shared();
                        v.push(p("channel", Kind::Str));
                        v.push(p("exten", Kind::Str));
                        v.push(p("context", Kind::Str));
                        v.push(p("application", Kind::Str));
                        v.push(p("data", Kind::Str));
                        v.push(p("caller_id", Kind::Str));
                        v.push(p("timeout_ms", Kind::Int));
                        v.push(p("async", Kind::Bool));
                        v.push(p("variables", Kind::Object));
                        v.push(p("account_code", Kind::Str));
                        v.push(p("priority", Kind::Int));
                        v.push(p("early_media", Kind::Bool));
                        v.push(p("channel_id", Kind::Str));
                        v.push(p("other_channel_id", Kind::Str));
                        v
                    },
                    vec!["channel"],
                ),
            ),
            (
                "asterisk.command",
                c(
                    {
                        let mut v = shared();
                        v.push(p("command", Kind::Str));
                        v
                    },
                    vec!["command"],
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
                    let vals: Vec<String> = e
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    if !vals.is_empty() {
                        return Kind::Str; // contract normalizes enums to base string types
                    }
                }
                Kind::Str
            }
            "object" | "" => Kind::Object,
            other => panic!("unsupported property type: {other} ({node})"),
        }
    }

    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        let defs = schema.get("definitions").cloned().unwrap_or(json!({}));
        assert_eq!(schema["type"], "object", "{op_name}: root type");

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
            let got_kind = got.get(*name).unwrap_or_else(|| {
                panic!("{op_name}: missing property `{name}` in derived schema")
            });
            assert_eq!(got_kind, kind, "{op_name}: property `{name}` kind");
        }

        let req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let mut req_set: Vec<&str> = req.clone();
        req_set.sort();
        let mut want_req: Vec<&str> = contract.required.clone();
        want_req.sort();
        assert_eq!(req_set, want_req, "{op_name}: required set");
    }

    #[test]
    fn derived_schemas_match_legacy_contract() {
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
