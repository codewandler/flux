//! `slack` — a flux integration plugin for the Slack Web API: post and read channel messages, list
//! channels and users, and read threads. Authenticates with a bot token injected as a bearer header
//! (purpose `bot_token`); a `user_token` purpose is declared for search-scoped calls. The base URL is
//! the `slack.endpoint` (defaults to `https://slack.com/api`). List ops contribute datasource records
//! (`slack.channel` / `slack.user`) so the agent can search them.
//!
//! Slack replies are JSON carrying an `"ok": bool`; a falsey `ok` is surfaced as an error built from the
//! response's `"error"` field.

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    let channel_arg =
        json!({ "channel": {"type": "string", "description": "channel id (e.g. \"C0123\")"} });
    PluginBuilder::new("slack", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec!["SLACK_BOT_TOKEN".into(), "SLACK_USER_TOKEN".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "bot_token".into(),
            env: vec!["SLACK_BOT_TOKEN".into()],
            description: "Slack bot token (xoxb-…) for posting/reading via the bot.".into(),
        })
        .auth(AuthMethod {
            purpose: "user_token".into(),
            env: vec!["SLACK_USER_TOKEN".into()],
            description: "Slack user token (xoxp-…) for search-scoped calls.".into(),
        })
        .endpoint(EndpointSpec {
            name: "slack.endpoint".into(),
            env: vec!["SLACK_API_URL".into()],
            description: "Slack Web API base URL (default https://slack.com/api)".into(),
        })
        .datasource(ds("slack.channels", "slack.channel", "Slack channels."))
        .datasource(ds("slack.users", "slack.user", "Slack workspace users."))
        .operation(
            write_op(
                "slack.message.send",
                "Post a message to a channel (optionally as a thread reply).",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "text": {"type": "string"},
                    "thread_ts": {"type": "string", "description": "reply in this thread"}
                }, "required": ["channel", "text"]}),
            ),
            message_send,
        )
        .operation(
            read_op(
                "slack.message.list",
                "List recent messages in a channel.",
                json!({"type": "object", "properties": {
                    "channel": {"type": "string"},
                    "limit": {"type": "integer", "description": "max messages (default 20)"}
                }, "required": ["channel"]}),
            ),
            message_list,
        )
        .operation(
            read_op(
                "slack.channel.list",
                "List public and private channels in the workspace.",
                json!({"type": "object", "properties": {}}),
            ),
            channel_list,
        )
        .operation(
            read_op(
                "slack.user.list",
                "List users in the workspace.",
                json!({"type": "object", "properties": {}}),
            ),
            user_list,
        )
        .operation(
            read_op(
                "slack.thread",
                "Read a thread's replies by channel + parent ts.",
                json!({"type": "object", "properties": {
                    "channel": channel_arg["channel"],
                    "ts": {"type": "string", "description": "parent message timestamp"}
                }, "required": ["channel", "ts"]}),
            ),
            thread,
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

/// Resolve the Slack API base URL (config), defaulting to the public endpoint; trailing slash trimmed.
fn base_url(host: &mut Host) -> String {
    host.endpoint("slack.endpoint")
        .unwrap_or_else(|_| "https://slack.com/api".into())
        .trim_end_matches('/')
        .to_string()
}

/// Slack returns `{"ok": bool, …}`; treat a falsey `ok` as an error built from the `"error"` field.
fn check_ok(v: Value) -> Result<Value, String> {
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        Ok(v)
    } else {
        let err = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error");
        Err(format!("slack error: {err}"))
    }
}

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

/// Percent-encode a query-parameter value (alnum + `-_.~` pass, everything else `%XX`).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn message_send(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let text = req_str(&input, "text")?;
    let mut body = json!({ "channel": channel, "text": text });
    if let Some(ts) = input.get("thread_ts").and_then(|v| v.as_str()) {
        body["thread_ts"] = json!(ts);
    }
    let url = format!("{}/chat.postMessage", base_url(host));
    let v = host.send_json("POST", &url, Some("bot_token"), &body)?;
    check_ok(v)
}

fn message_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
    let url = format!(
        "{}/conversations.history?channel={}&limit={limit}",
        base_url(host),
        urlencode(channel),
    );
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

fn channel_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let url = format!(
        "{}/conversations.list?types=public_channel,private_channel&limit=200",
        base_url(host),
    );
    let v = check_ok(host.get_json(&url, Some("bot_token"))?)?;
    contribute_channels(host, &v);
    Ok(v)
}

fn user_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let url = format!("{}/users.list?limit=200", base_url(host));
    let v = check_ok(host.get_json(&url, Some("bot_token"))?)?;
    contribute_users(host, &v);
    Ok(v)
}

fn thread(input: Value, host: &mut Host) -> Result<Value, String> {
    let channel = req_str(&input, "channel")?;
    let ts = req_str(&input, "ts")?;
    let url = format!(
        "{}/conversations.replies?channel={}&ts={}",
        base_url(host),
        urlencode(channel),
        urlencode(ts),
    );
    check_ok(host.get_json(&url, Some("bot_token"))?)
}

fn contribute_channels(host: &mut Host, v: &Value) {
    let Some(arr) = v.get("channels").and_then(|c| c.as_array()) else {
        return;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|c| {
            let id = c.get("id").and_then(|x| x.as_str())?;
            let name = c.get("name").and_then(|x| x.as_str()).unwrap_or(id);
            let body = c
                .get("topic")
                .and_then(|t| t.get("value"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(name);
            Some(Record::new(
                Source::new("slack"),
                "slack.channel",
                id,
                name,
                body,
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_users(host: &mut Host, v: &Value) {
    let Some(arr) = v.get("members").and_then(|m| m.as_array()) else {
        return;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|u| {
            let id = u.get("id").and_then(|x| x.as_str())?;
            let name = u.get("name").and_then(|x| x.as_str()).unwrap_or(id);
            let body = u
                .get("profile")
                .and_then(|p| p.get("real_name"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(name);
            Some(Record::new(
                Source::new("slack"),
                "slack.user",
                id,
                name,
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

    #[test]
    fn channel_list_calls_the_api_and_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_secret("bot_token", "xoxb")
            .with_http(
                "conversations.list",
                json!({
                    "ok": true,
                    "channels": [{ "id": "C1", "name": "dev-team", "topic": { "value": "eng" } }]
                }),
            );
        let out = plugin
            .call("slack.channel.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["channels"][0]["id"], "C1");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "slack.channel");
        assert_eq!(recs[0].id, "C1");
        assert_eq!(recs[0].title, "dev-team");
        assert_eq!(recs[0].body, "eng");
    }

    #[test]
    fn message_send_posts_and_returns_the_ts() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_secret("bot_token", "xoxb")
            .with_http("chat.postMessage", json!({ "ok": true, "ts": "123.45" }));
        let out = plugin
            .call(
                "slack.message.send",
                json!({ "channel": "C1", "text": "hello", "thread_ts": "100.1" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ts"], "123.45");
    }

    #[test]
    fn falsey_ok_surfaces_the_error() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_secret("bot_token", "xoxb")
            .with_http(
                "conversations.history",
                json!({ "ok": false, "error": "channel_not_found" }),
            );
        let err = plugin
            .call("slack.message.list", json!({ "channel": "C9" }), &mut host)
            .unwrap_err();
        assert!(err.contains("channel_not_found"), "got: {err}");
    }

    #[test]
    fn manifest_declares_ops_auth_and_datasources() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 5);
        assert_eq!(m.auth[0].purpose, "bot_token");
        assert!(m.auth.iter().any(|a| a.purpose == "user_token"));
        assert!(m.datasources.iter().any(|d| d.entity == "slack.channel"));
        assert!(m.datasources.iter().any(|d| d.entity == "slack.user"));
    }
}
