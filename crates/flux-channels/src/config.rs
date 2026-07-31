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
    /// non-loopback `addr`, since the served agent has no interactive approver. Ignored (with a
    /// warning) when `introspect_url` selects per-request principal auth.
    #[serde(default)]
    pub token: Option<String>,

    // ── Per-request principal auth (D-69), parity with `flux --serve` ──
    /// RFC 7662 token-introspection endpoint. Setting this switches the channel into per-request
    /// principal auth: every request's bearer is resolved to a principal, sessions are
    /// realm-scoped, and `external_url` becomes required.
    #[serde(default)]
    pub introspect_url: Option<String>,
    /// Externally reachable base URL advertised on the agent card (e.g. `https://x.example.com`).
    /// Required with `introspect_url`: the public card tells clients where to send bearer tokens,
    /// so it must come from config, never the request `Host` header.
    #[serde(default)]
    pub external_url: Option<String>,
    /// Optional introspection client id (`client_secret_basic`); paired with `introspect_secret`.
    #[serde(default)]
    pub introspect_client_id: Option<String>,
    /// The introspection client secret — host-resolved like `token`, so write it as
    /// `introspect_secret secret "KEY"` in the program (never a plaintext literal).
    #[serde(default)]
    pub introspect_secret: Option<String>,
    /// Claim (literal key first, dot-path on miss) carrying the caller's account/tenant id.
    #[serde(default)]
    pub introspect_account_claim: Option<String>,
    /// Claim carrying roles (JSON array or one space-separated string).
    #[serde(default)]
    pub introspect_roles_claim: Option<String>,
    /// Reject tokens whose account claim is missing/empty.
    #[serde(default)]
    pub introspect_require_account: Option<bool>,
    /// Allow a plain-`http` introspection endpoint (trusted-network deployments; bearer tokens
    /// transit this connection, so default is https-only).
    #[serde(default)]
    pub introspect_allow_http: Option<bool>,
}

// Secrets are a single mechanism: `secret "ENV"` references in the program (lowered to a
// `{"$secret":…}` marker) are resolved from the environment once at load by `flux_app::resolve_secrets`,
// before any adapter deserializes these settings. So the token fields above are already plain values.

/// `kind = "room"` settings — a many-party meeting room flux is one participant in (D-204).
#[derive(Debug, Clone, Deserialize)]
pub struct RoomSettings {
    /// Which [`Room`](crate::rooms::Room) backend to join with. `"mock"` is the in-process one;
    /// `"xmpp"` (D-205) and `"jaas"` (D-206) follow. An unrecognized backend is a load error, exactly
    /// like an unrecognized channel `kind`.
    pub backend: String,
    /// The room address, as the server spells it (an XMPP MUC JID).
    pub room: String,
    /// The nick to join under. Defaults to [`DEFAULT_ROOM_NICK`].
    #[serde(default)]
    pub nick: Option<String>,
    /// When the agent should treat a turn as addressed to it — nick mention, private whisper, or a
    /// wake phrase.
    ///
    /// **Carried, not yet enforced.** D-207 owns the rule's vocabulary and its enforcement; the field
    /// lives here so the declaration shape is stable and a program written for D-207 loads today. The
    /// value is passed through unvalidated on purpose: validating it now would fix a vocabulary that
    /// story has not chosen yet.
    #[serde(default)]
    pub address_rule: Option<String>,
}

/// The nick flux joins a room under when the declaration does not say. A room containing humans is
/// owed an honest answer about what just joined it.
pub const DEFAULT_ROOM_NICK: &str = "flux";

/// `kind = "slack"` settings (compiled in by default; gated only for `--no-default-features` builds).
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
