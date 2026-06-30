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
    /// Optional bearer token (host-resolved — use `token secret "KEY"` in the program). Required for a
    /// non-loopback `addr`.
    #[serde(default)]
    pub token: Option<String>,
}

fn default_path() -> String {
    "/".to_string()
}

/// `kind = "a2a"` settings — expose a program agent over the HTTP/A2A API (sessions + SSE + A2A +
/// agent-card), the surface formerly served by the standalone `flux serve` command.
#[derive(Debug, Clone, Deserialize)]
pub struct A2aSettings {
    /// Address to bind, e.g. `"127.0.0.1:8787"`.
    pub addr: String,
    /// Which declared agent to serve. Optional when the program declares exactly one agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// Optional bearer token (host-resolved — use `token secret "KEY"` in the program). Required for a
    /// non-loopback `addr`, since the served agent has no interactive approver.
    #[serde(default)]
    pub token: Option<String>,
}

// Secrets are a single mechanism: `secret "ENV"` references in the program (lowered to a
// `{"$secret":…}` marker) are resolved from the environment once at load by `flux_app::resolve_secrets`,
// before any adapter deserializes these settings. So the token fields above are already plain values.

/// `kind = "slack"` settings (feature `slack`).
#[cfg(feature = "slack")]
#[derive(Debug, Clone, Deserialize)]
pub struct SlackSettings {
    /// Bot OAuth token (`xoxb-…`), host-resolved (use `bot_token secret "KEY"` in the program).
    pub bot_token: String,
    /// App-level token for socket mode (`xapp-…`), host-resolved (use `app_token secret "KEY"`).
    pub app_token: String,
    /// If non-empty, only these Slack user ids may trigger the agent.
    #[serde(default)]
    pub allow_users: Vec<String>,
    /// If non-empty, only these Slack channel ids are listened to.
    #[serde(default)]
    pub allow_channels: Vec<String>,
}
