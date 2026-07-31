//! Per-kind channel settings, deserialized from a [`ChannelDecl`](flux_lang::program::ChannelDecl)'s
//! free-form `settings` JSON bag.

use std::collections::BTreeMap;

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
///
/// `Debug` is **hand-written** rather than derived: this struct holds the host-resolved plaintext
/// `token` and, since C-291, a `verify` record holding the host-resolved plaintext signing secret. A
/// derive would print both the first time anyone adds a trace line, and the redactor only scrubs
/// what it was given — so the type itself must not offer the value to a formatter.
#[derive(Clone, Deserialize)]
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
    /// non-loopback `addr` that states no verifying scheme.
    #[serde(default)]
    pub token: Option<String>,
    /// How an inbound delivery's authenticity is established (C-291).
    ///
    /// **Tri-state, and the absent arm is the dangerous one.** Absent means the declaration states
    /// nothing; `verify "none"` means the operator has decided this endpoint carries no signature.
    /// The two must not normalise together — a decision nobody made and a decision made are
    /// different facts, and on a non-loopback bind only the second is admissible.
    #[serde(default)]
    pub verify: Option<VerifyDecl>,
}

impl std::fmt::Debug for WebhookSettings {
    /// Redact everything host-resolved from a `secret "KEY"` reference; keep the shape observable.
    /// `Some("<redacted>")`/`None` on `token` so "this channel has a token" stays diagnosable
    /// without the value, exactly as `flux_credentials::OAuthToken` does it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSettings")
            .field("addr", &self.addr)
            .field("path", &self.path)
            .field("async", &self.is_async)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("verify", &self.verify)
            .finish()
    }
}

/// What a `channel webhook` declaration says about verifying inbound deliveries.
///
/// Untagged because the two spellings are genuinely different shapes in the program text:
/// `verify "none"` is a word, and a scheme is a record. Every field of [`VerifySpec`] is optional
/// *at this layer* so that a defective record still deserializes and is then refused by name —
/// `serde`'s untagged fallback would otherwise collapse every mistake into "data did not match any
/// variant", which names neither the channel nor the field.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum VerifyDecl {
    /// A bare word. Only `"none"` is a verification answer; anything else is a load error.
    Word(String),
    /// A `verify { … }` record naming a scheme and its parameters.
    Scheme(Box<VerifySpec>),
}

impl std::fmt::Debug for VerifyDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Word(word) => f.debug_tuple("Word").field(word).finish(),
            Self::Scheme(spec) => f.debug_tuple("Scheme").field(spec).finish(),
        }
    }
}

/// The declared signature scheme. **Carried and fully validated here; computed by C-292.**
///
/// The parameters are the four axes every vendor scheme in the matrix varies along — which digest,
/// how it is spelled, what string is signed, and how long a signature stays acceptable — plus the
/// header it arrives in and the shared secret. See `docs/stories/C-292-webhook-signature-schemes.md`.
#[derive(Clone, Deserialize)]
pub struct VerifySpec {
    /// The scheme's machine token, e.g. `"hmac"`. Required.
    #[serde(default)]
    pub scheme: Option<String>,
    /// The digest, e.g. `"sha256"`.
    #[serde(default)]
    pub algorithm: Option<String>,
    /// How the digest is spelled on the wire: `"hex"` or `"base64"`.
    #[serde(default)]
    pub encoding: Option<String>,
    /// The header the signature arrives in, e.g. `"X-Hub-Signature-256"`.
    #[serde(default)]
    pub header: Option<String>,
    /// A literal prefix the header value carries before the digest, e.g. `"sha256="`.
    #[serde(default)]
    pub prefix: Option<String>,
    /// The signed-string template over `{body}` and `{timestamp}`, e.g. `"v0:{timestamp}:{body}"`.
    #[serde(default)]
    pub signed: Option<String>,
    /// Where the signed timestamp is read from. **Header-borne by construction** — see
    /// [`VerifySelector`].
    #[serde(default)]
    pub timestamp: Option<VerifySelector>,
    /// The replay window, in the `5m` / `300s` / `1h` grammar.
    #[serde(default)]
    pub tolerance: Option<String>,
    /// The shared signing secret, host-resolved — write it as `secret: secret "KEY"` in the program,
    /// never as a literal.
    #[serde(default)]
    pub secret: Option<String>,
}

impl std::fmt::Debug for VerifySpec {
    /// Redacts `secret` for the same reason [`WebhookSettings`] does, and nothing else: every other
    /// field here is a declaration an operator wrote and needs to be able to read back.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifySpec")
            .field("scheme", &self.scheme)
            .field("algorithm", &self.algorithm)
            .field("encoding", &self.encoding)
            .field("header", &self.header)
            .field("prefix", &self.prefix)
            .field("signed", &self.signed)
            .field("timestamp", &self.timestamp)
            .field("tolerance", &self.tolerance)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Where one named value is read off an inbound request. Mirrors the connector manifest's selector
/// so an operator writing either reads one grammar.
#[derive(Debug, Clone, Deserialize)]
pub struct VerifySelector {
    /// `"header"`. `"body"` is spellable and refused: a value read out of the body has to be parsed
    /// *before* the bytes carrying it are authenticated, which inverts the order C-291 exists to fix.
    #[serde(default)]
    pub source: Option<String>,
    /// The header name.
    #[serde(default)]
    pub name: Option<String>,
}

fn default_path() -> String {
    "/".to_string()
}

/// `kind = "connector"` settings — the operator's half of a channel binding (D-216).
///
/// The division of labour is the whole design: the **manifest** carries the semantics (which events,
/// how a delivery is verified, which fields map to which flow symbols, which operation replies) and
/// this struct carries everything a published artifact must never know — where flux listens, and
/// which of *this* deployment's secrets back the credentials the binding names by name.
///
/// Deliberately **not** `deny_unknown_fields`: the same bag also carries the future `allow { … }`
/// block (D-219), and a program written for a later flux must not fail to parse on an older one.
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectorSettings {
    /// The connector id — selects the manifest. **Validated against a name grammar before it is
    /// joined onto the connectors directory**; see `adapters::connector::validate_name`.
    pub connector: String,
    /// The `[[channels]]` binding name within that manifest.
    pub binding: String,
    /// The connector service, when the connector publishes more than one. Absent selects the
    /// reserved default service, which the producing repository elides from the manifest.
    #[serde(default)]
    pub service: Option<String>,
    /// An explicit manifest path, overriding `~/.flux/connectors/<connector>.connector.toml`.
    /// Operator input, never manifest-derived — no field read *out of* a manifest may influence a
    /// path.
    #[serde(default)]
    pub manifest: Option<String>,
    /// Credential name (as the binding spells it, e.g. `slack.signing_secret`) → this deployment's
    /// value. Host-resolved before this struct deserializes, so write
    /// `credentials { "slack.signing_secret": secret "SLACK_SIGNING_SECRET" }` in the program — a
    /// record separates key from value with `:`, the same grammar `{ "a": 1 }` takes anywhere else.
    #[serde(default)]
    pub credentials: BTreeMap<String, String>,

    // ── the `webhook` transport's own settings ──
    /// Address to bind, e.g. `"0.0.0.0:8790"`. Required for a `webhook`-transport binding.
    #[serde(default)]
    pub addr: Option<String>,
    /// The POST path, e.g. `"/slack/events"`.
    #[serde(default = "default_path")]
    pub path: String,
    /// Optional bearer token, host-resolved like every other credential here. Required for a
    /// non-loopback `addr` on a binding whose verification is `none` — see
    /// [`ConnectorChannel::from_decl`](crate::ConnectorChannel::from_decl).
    #[serde(default)]
    pub token: Option<String>,
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
///
/// `#[non_exhaustive]`: the credential and endpoint fields below arrived with the first real backend
/// (D-205) and D-206's vendor one will add more, so an external literal construction must not break
/// on each new backend. Deserialization and the adapters inside this crate are unaffected.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct RoomSettings {
    /// Which [`Room`](crate::rooms::Room) backend to join with. `"mock"` is the in-process one,
    /// `"xmpp"` the portable standards-compliant one (D-205); `"jaas"` (D-206) follows. An
    /// unrecognized backend is a load error, exactly like an unrecognized channel `kind`.
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

    // ── `backend = "xmpp"` (D-205) ──
    /// The RFC 7395 WebSocket endpoint, e.g. `wss://example.org/xmpp-websocket`. Required for the
    /// `xmpp` backend: a MUC JID says which room, never where to connect.
    #[serde(default)]
    pub url: Option<String>,
    /// The XMPP domain for the stream open. Defaults to the endpoint's host, which is right for the
    /// common single-host deployment but not for a MUC on a separate `conference.` component.
    #[serde(default)]
    pub domain: Option<String>,
    /// SASL `PLAIN` username. Absent selects `ANONYMOUS`.
    #[serde(default)]
    pub user: Option<String>,
    /// SASL `PLAIN` password — host-resolved like every other credential here, so write it as
    /// `password secret "KEY"` in the program rather than a literal.
    #[serde(default)]
    pub password: Option<String>,
    /// The MUC's own room password (XEP-0045). Host-resolved the same way.
    #[serde(default)]
    pub muc_password: Option<String>,
    /// Allow the endpoint to resolve to a private/loopback address — a self-hosted prosody on the
    /// LAN. Off by default: this is the scoped grant `flux_system::net`'s egress guard takes, and
    /// widening it is an operator decision, not a default.
    #[serde(default)]
    pub allow_private_net: bool,
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
