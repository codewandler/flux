//! `flux-integrations` — connect flux to external surfaces.
//!
//! Inbound triggering is served by `flux-server`'s `POST /webhook` (an external event creates a
//! session and runs a turn). This crate handles the **Slack** specifics: parsing inbound Events
//! API payloads into a [`SlackMessage`], and posting outbound notifications to an incoming-webhook
//! URL. (Other chat surfaces follow the same shape.)

use serde_json::Value;

use flux_core::{Error, Result};

/// A normalized inbound Slack message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackMessage {
    pub text: String,
    pub channel: Option<String>,
    pub user: Option<String>,
}

/// Parse a Slack Events API payload into a [`SlackMessage`], if it's a user message event.
/// Returns `None` for non-message events (and ignores bot messages to avoid loops).
pub fn parse_slack_event(payload: &Value) -> Option<SlackMessage> {
    let event = payload.get("event")?;
    if event.get("type").and_then(|v| v.as_str()) != Some("message") {
        return None;
    }
    // Ignore messages emitted by bots (including our own) to prevent feedback loops.
    if event.get("bot_id").is_some() || event.get("subtype").is_some() {
        return None;
    }
    let text = event.get("text").and_then(|v| v.as_str())?.to_string();
    Some(SlackMessage {
        text,
        channel: event
            .get("channel")
            .and_then(|v| v.as_str())
            .map(String::from),
        user: event.get("user").and_then(|v| v.as_str()).map(String::from),
    })
}

/// Detect a Slack URL-verification challenge (Slack sends this when you register an endpoint);
/// returns the `challenge` string to echo back.
pub fn slack_url_verification(payload: &Value) -> Option<String> {
    if payload.get("type").and_then(|v| v.as_str()) == Some("url_verification") {
        payload
            .get("challenge")
            .and_then(|v| v.as_str())
            .map(String::from)
    } else {
        None
    }
}

/// Post a text notification to a Slack incoming-webhook URL.
pub async fn notify_slack(webhook_url: &str, text: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .post(webhook_url)
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(Error::Api {
            status: resp.status().as_u16(),
            message: resp.text().await.unwrap_or_default(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_user_message_event() {
        let payload = json!({
            "event": {"type": "message", "text": "hi flux", "channel": "C1", "user": "U1"}
        });
        let m = parse_slack_event(&payload).unwrap();
        assert_eq!(m.text, "hi flux");
        assert_eq!(m.channel.as_deref(), Some("C1"));
        assert_eq!(m.user.as_deref(), Some("U1"));
    }

    #[test]
    fn ignores_bot_and_non_message() {
        let bot = json!({"event": {"type": "message", "text": "x", "bot_id": "B1"}});
        assert!(parse_slack_event(&bot).is_none());
        let other = json!({"event": {"type": "reaction_added"}});
        assert!(parse_slack_event(&other).is_none());
    }

    #[test]
    fn detects_url_verification() {
        let p = json!({"type": "url_verification", "challenge": "abc123"});
        assert_eq!(slack_url_verification(&p).as_deref(), Some("abc123"));
        assert!(slack_url_verification(&json!({"type": "event_callback"})).is_none());
    }
}
