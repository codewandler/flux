//! The **slack** adapter (`kind = "slack"`, feature `slack`): a socket-mode listener that delivers each
//! mention / message under the channel name and posts the journeys' results back to the thread.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use slack_morphism::prelude::*;
use tokio_util::sync::CancellationToken;

use flux_lang::program::ChannelDecl;

use crate::config::SlackSettings;
use crate::{Channel, Deliverer};

pub struct SlackChannel {
    name: String,
    bot_token: String,
    app_token: String,
    allow_users: Vec<String>,
    allow_channels: Vec<String>,
}

impl SlackChannel {
    pub fn from_decl(decl: &ChannelDecl) -> anyhow::Result<Self> {
        let s: SlackSettings = serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{}` settings: {e}", decl.name))?;
        Ok(Self {
            name: decl.name.clone(),
            // Secrets are already host-resolved (the `{"$secret":…}` marker → env value) before these
            // settings deserialize, so the token fields are plain values here.
            bot_token: s.bot_token,
            app_token: s.app_token,
            allow_users: s.allow_users,
            allow_channels: s.allow_channels,
        })
    }
}

/// Shared into the socket-mode push callback via the listener's user-state (keyed by type).
struct SlackContext {
    name: String,
    deliverer: Arc<dyn Deliverer>,
    bot_token: SlackApiToken,
    allow_users: Vec<String>,
    allow_channels: Vec<String>,
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()> {
        let client = Arc::new(SlackClient::new(SlackClientHyperConnector::new().map_err(
            |e| anyhow::anyhow!("channel `{}`: slack connector: {e}", self.name),
        )?));
        let ctx = Arc::new(SlackContext {
            name: self.name.clone(),
            deliverer: d,
            bot_token: SlackApiToken::new(SlackApiTokenValue(self.bot_token.clone())),
            allow_users: self.allow_users.clone(),
            allow_channels: self.allow_channels.clone(),
        });
        let env = Arc::new(
            SlackClientEventsListenerEnvironment::new(client.clone()).with_user_state(ctx),
        );
        let callbacks = SlackSocketModeListenerCallbacks::new().with_push_events(on_push);
        let listener =
            SlackClientSocketModeListener::new(&SlackClientSocketModeConfig::new(), env, callbacks);

        let app_token = SlackApiToken::new(SlackApiTokenValue(self.app_token.clone()));
        listener
            .listen_for(&app_token)
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: slack listen: {e}", self.name))?;
        listener.start().await;
        cancel.cancelled().await;
        listener.shutdown().await;
        Ok(())
    }
}

/// Socket-mode push callback: map a mention / human message to a delivery, then post the journeys'
/// joined result back to the originating thread. Bot/subtype messages are ignored to avoid reply loops.
async fn on_push(
    event: SlackPushEventCallback,
    client: Arc<SlackClient<SlackClientHyperHttpsConnector>>,
    state: SlackClientEventsUserState,
) -> UserCallbackResult<()> {
    let ctx = match state.read().await.get_user_state::<Arc<SlackContext>>() {
        Some(c) => c.clone(),
        None => return Ok(()),
    };

    let parsed = match &event.event {
        SlackEventCallbackBody::AppMention(m) => Some((
            m.user.0.clone(),
            m.channel.0.clone(),
            m.content.text.clone().unwrap_or_default(),
            m.origin
                .thread_ts
                .clone()
                .unwrap_or_else(|| m.origin.ts.clone()),
        )),
        SlackEventCallbackBody::Message(m) => {
            // Skip our own + other bots' messages and edits/joins (subtypes) — those would loop.
            if m.subtype.is_some() || m.sender.bot_id.is_some() {
                None
            } else {
                Some((
                    m.sender.user.clone().map(|u| u.0).unwrap_or_default(),
                    m.origin.channel.clone().map(|c| c.0).unwrap_or_default(),
                    m.content
                        .as_ref()
                        .and_then(|c| c.text.clone())
                        .unwrap_or_default(),
                    m.origin
                        .thread_ts
                        .clone()
                        .unwrap_or_else(|| m.origin.ts.clone()),
                ))
            }
        }
        _ => None,
    };
    let Some((user, channel, text, thread)) = parsed else {
        return Ok(());
    };
    if channel.is_empty() || !allowed(&ctx.allow_users, &ctx.allow_channels, &user, &channel) {
        return Ok(());
    }

    let payload = build_payload(&text, &user, &channel, &thread.0);
    let runs = match ctx.deliverer.deliver(&ctx.name, payload).await {
        Ok(runs) => runs,
        Err(e) => {
            eprintln!("slack `{}`: delivery failed: {e}", ctx.name);
            return Ok(());
        }
    };
    let reply = runs
        .into_iter()
        .map(|r| r.result)
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !reply.is_empty() {
        let req = SlackApiChatPostMessageRequest::new(
            SlackChannelId(channel),
            SlackMessageContent::new().with_text(reply),
        )
        .with_thread_ts(thread);
        if let Err(e) = client
            .open_session(&ctx.bot_token)
            .chat_post_message(&req)
            .await
        {
            eprintln!("slack `{}`: post reply failed: {e}", ctx.name);
        }
    }
    Ok(())
}

/// The conversation id for a Slack event: the thread ts when present, else the channel id.
fn conversation_id(channel: &str, thread_ts: Option<&str>) -> String {
    thread_ts.unwrap_or(channel).to_string()
}

/// The delivery payload a Slack-triggered journey receives (seeded into its flow store).
fn build_payload(text: &str, user: &str, channel: &str, thread_ts: &str) -> Value {
    json!({
        "text": text,
        "user": user,
        "channel": channel,
        "thread": thread_ts,
        "conversation": conversation_id(channel, Some(thread_ts)),
    })
}

/// Allow-list gate: an empty list allows everyone; otherwise the id must be present.
fn allowed(allow_users: &[String], allow_channels: &[String], user: &str, channel: &str) -> bool {
    let user_ok = allow_users.is_empty() || allow_users.iter().any(|u| u == user);
    let channel_ok = allow_channels.is_empty() || allow_channels.iter().any(|c| c == channel);
    user_ok && channel_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_maps_fields_with_thread_as_conversation() {
        let p = build_payload("hi there", "U1", "C1", "T1");
        assert_eq!(p["text"], "hi there");
        assert_eq!(p["user"], "U1");
        assert_eq!(p["channel"], "C1");
        assert_eq!(p["thread"], "T1");
        assert_eq!(p["conversation"], "T1"); // thread wins over channel
    }

    #[test]
    fn conversation_falls_back_to_channel() {
        assert_eq!(conversation_id("C1", None), "C1");
        assert_eq!(conversation_id("C1", Some("T9")), "T9");
    }

    #[test]
    fn allow_list_filters_users_and_channels() {
        assert!(allowed(&[], &[], "U1", "C1")); // empty = allow all
        assert!(allowed(&["U1".into()], &[], "U1", "C1"));
        assert!(!allowed(&["U2".into()], &[], "U1", "C1")); // user not allowed
        assert!(!allowed(&[], &["C2".into()], "U1", "C1")); // channel not allowed
        assert!(allowed(&["U1".into()], &["C1".into()], "U1", "C1"));
    }
}
