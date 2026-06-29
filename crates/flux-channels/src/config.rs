//! Per-kind channel settings, deserialized from a [`ChannelDecl`](flux_lang::program::ChannelDecl)'s
//! free-form `settings` JSON bag.

use serde::Deserialize;

/// `kind = "schedule" | "cron"` settings. Exactly one of `schedule` / `on` must be set.
#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleSettings {
    /// A cron expression: 5-field crontab (`"0 9 * * *"`) or 6/7-field seconds-first (`"* * * * * *"`).
    #[serde(default)]
    pub schedule: Option<String>,
    /// A lifecycle hook — only `"startup"` is supported (fire once at boot under this channel's name).
    #[serde(default)]
    pub on: Option<String>,
}

/// `kind = "webhook" | "http"` settings.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookSettings {
    /// Address to bind, e.g. `"127.0.0.1:8790"`.
    pub addr: String,
    /// The POST path, e.g. `"/hook"`.
    #[serde(default = "default_path")]
    pub path: String,
    /// When true, reply `202 Accepted` immediately and run the delivery fire-and-forget.
    #[serde(default, rename = "async")]
    pub is_async: bool,
    /// Optional bearer token (a literal, or a `secret:env/KEY` / `env:KEY` reference). Required for a
    /// non-loopback `addr`.
    #[serde(default)]
    pub token: Option<String>,
}

fn default_path() -> String {
    "/".to_string()
}

/// `kind = "slack"` settings (feature `slack`).
#[cfg(feature = "slack")]
#[derive(Debug, Clone, Deserialize)]
pub struct SlackSettings {
    /// Bot OAuth token (`xoxb-…`), literal or `secret:env/KEY`.
    pub bot_token: String,
    /// App-level token for socket mode (`xapp-…`), literal or `secret:env/KEY`.
    pub app_token: String,
    /// If non-empty, only these Slack user ids may trigger the agent.
    #[serde(default)]
    pub allow_users: Vec<String>,
    /// If non-empty, only these Slack channel ids are listened to.
    #[serde(default)]
    pub allow_channels: Vec<String>,
}

/// Resolve a possible `secret:env/KEY` (or `env:KEY`) reference to its value; a plain string passes
/// through unchanged. Keeps tokens out of the program file.
pub fn resolve_secret(s: &str) -> anyhow::Result<String> {
    let key = s
        .strip_prefix("secret:env/")
        .or_else(|| s.strip_prefix("env:"));
    match key {
        Some(key) => std::env::var(key)
            .map_err(|_| anyhow::anyhow!("env var `{key}` not set (referenced by `{s}`)")),
        None => Ok(s.to_string()),
    }
}
