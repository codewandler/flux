//! `flux-credentials` — authenticates flux *to the LLM providers* (distinct from `flux-auth`,
//! which authenticates callers *to flux*).
//!
//! Provides OAuth token sources for the subscription providers (`claude`, `codex`): a refreshing
//! [`TokenSource`] backed by a 0600 token store, with import from the official CLIs'
//! credential files (`~/.claude/.credentials.json`, `~/.codex/auth.json`) as the primary
//! acquisition path and PKCE login (`flux auth login claude|codex`) as the alternative.
//!
//! Constants and flows mirror the user's Go implementations (`coder/internal/oauth`,
//! `llm/provider/codex/auth.go`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use flux_core::{Error, PricingTable, RateOverride, Result};
use flux_provider::TokenSource;

// --- Anthropic OAuth constants (← coder/internal/oauth/oauth.go) -----------
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const ANTHROPIC_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const ANTHROPIC_SCOPE: &str = "org:create_api_key user:profile user:inference";

// --- Codex OAuth constants (← llm/provider/codex/auth.go; authorize/redirect verified against
// the upstream codex CLI, openai/codex `codex-rs/login/src/server.rs`: DEFAULT_ISSUER
// "https://auth.openai.com", DEFAULT_PORT 1455, redirect "http://localhost:{port}/auth/callback",
// form-encoded `authorization_code` exchange against "{issuer}/oauth/token") -------------------
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Codex token endpoint. Public so the CLI login flow can name the production endpoint explicitly
/// where its hermetic test substitutes a stub (see [`codex_exchange_and_store_at`]).
pub const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// The local callback the codex client is registered for: the upstream CLI binds
/// `localhost:1455` and receives the redirect at `/auth/callback` — flux's login mirrors it.
/// Public so `flux auth login codex` derives its callback listener from the same source of truth.
pub const CODEX_REDIRECT_PORT: u16 = 1455;
/// Path component of [`CODEX_REDIRECT_URI`] (see [`CODEX_REDIRECT_PORT`]).
pub const CODEX_REDIRECT_PATH: &str = "/auth/callback";
const CODEX_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// The core OIDC scope set. Upstream additionally requests `api.connectors.read` /
/// `api.connectors.invoke`; flux doesn't use connectors, so it asks for the least it needs.
const CODEX_SCOPE: &str = "openid profile email offline_access";

const REFRESH_BUFFER_MS: i64 = 5 * 60 * 1000;

/// Window in which a forced refresh (on a 401) coalesces a concurrent burst into a single refresh:
/// if we already refreshed this recently, another 401-handler did the work, so re-use that token
/// rather than spending the refresh grant again. Far shorter than a token's lifetime, so a genuine
/// "needs refreshing again" never falls inside it.
const FORCE_REFRESH_DEDUP_MS: i64 = 30 * 1000;

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn home() -> Result<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| Error::Config("HOME is not set".to_string()))
}

// ---------------------------------------------------------------------------
// Token model
// ---------------------------------------------------------------------------

/// An OAuth token set for a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    /// Unix epoch milliseconds at which `access` expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// Decode a JWT's payload (the middle `header.PAYLOAD.sig` segment) into a JSON value. Returns
/// `None` unless the token is a three-part triple whose payload is base64url-encoded JSON. The
/// signature is **not** verified — these tokens are issued by the official CLIs and we only read
/// their claims, never trust them as an authorization decision.
fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = b64().decode(parts[1]).ok()?;
    serde_json::from_slice(&payload).ok()
}

/// Decode a JWT's `exp` claim (seconds) into unix-epoch milliseconds.
fn jwt_expiry_ms(token: &str) -> Option<i64> {
    let exp = jwt_payload(token)?.get("exp")?.as_i64()?;
    if exp == 0 {
        None
    } else {
        Some(exp * 1000)
    }
}

/// Extract the ChatGPT account id from an `id_token` JWT's claims. Real `~/.codex/auth.json` nests
/// it under the `https://api.openai.com/auth` claim as `chatgpt_account_id`; some tokens instead
/// carry a top-level `chatgpt_account_id`. Returns the first non-empty match.
fn account_id_from_id_token(id_token: &str) -> Option<String> {
    let payload = jwt_payload(id_token)?;
    let nested = payload
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"));
    let id = payload
        .get("chatgpt_account_id")
        .or(nested)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    Some(id.to_string())
}

// ---------------------------------------------------------------------------
// Token store (~/.flux/credentials.toml, 0600)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    #[serde(flatten)]
    entries: HashMap<String, OAuthToken>,
}

fn store_path() -> Result<std::path::PathBuf> {
    Ok(home()?.join(".flux").join("credentials.toml"))
}

/// Load the credential store. A corrupt file is an **error**, not an empty default — otherwise a
/// subsequent `save_stored` would happily overwrite it, wiping every other provider's token.
fn load_store() -> Result<Store> {
    let path = store_path()?;
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).map_err(|e| {
            Error::Config(format!(
                "credentials store {} is corrupt ({e}); fix or remove it",
                path.display()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(Error::Io(e)),
    }
}

fn load_stored(provider: &str) -> Option<OAuthToken> {
    // Reads tolerate a corrupt/missing store (fall back to env/import); only writes must not clobber.
    load_store().ok()?.entries.remove(provider)
}

/// Persist one provider's token to the store (see [`write_store`] for the atomic-write contract).
fn save_stored(provider: &str, token: &OAuthToken) -> Result<()> {
    // Propagates a corrupt-store error rather than silently dropping the other providers' tokens.
    let mut store = load_store()?;
    store.entries.insert(provider.to_string(), token.clone());
    write_store(&store)
}

/// Remove `provider`'s entry from the store (e.g. `flux auth set --clear`). A missing entry (or a
/// missing store file) is a no-op, not an error.
fn delete_stored(provider: &str) -> Result<()> {
    let mut store = load_store()?;
    if store.entries.remove(provider).is_none() {
        return Ok(());
    }
    write_store(&store)
}

/// Persist the whole store to `~/.flux/credentials.toml`, creating `~/.flux` and forcing 0600.
/// Writes atomically (temp file created 0600 + rename) so there is no world-readable window and a
/// crash mid-write can't truncate the existing credentials.
fn write_store(store: &Store) -> Result<()> {
    let path = store_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = toml::to_string_pretty(store)
        .map_err(|e| Error::Config(format!("serialize credentials: {e}")))?;

    let tmp = path.with_extension("toml.tmp");
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600); // 0600 from creation — no default-umask race window
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, &path)?; // atomic replace; the temp file's 0600 carries over
    Ok(())
}

// ---------------------------------------------------------------------------
// Pricing overrides (~/.flux/pricing.toml)
// ---------------------------------------------------------------------------
//
// The cost model itself (built-in rate table + cost math) is pure and lives in `flux_core::pricing`.
// This is the only IO seam: it reads an optional user-editable `~/.flux/pricing.toml` and folds its
// per-model partial overrides onto the built-in table. A missing or malformed file falls back to the
// built-ins so a bad edit never breaks cost reporting. C-06's reporting surface consumes the result.
//
// File shape (every field optional; absent fields keep the built-in value):
//
// ```toml
// [models."claude-opus-4-8"]
// input = 20.0
// cache_read = 2.0
// ```

#[derive(Debug, Default, Deserialize)]
struct PricingFile {
    #[serde(default)]
    models: std::collections::HashMap<String, RateOverride>,
}

fn pricing_path() -> Result<std::path::PathBuf> {
    Ok(home()?.join(".flux").join("pricing.toml"))
}

/// Fold the overrides in `path` (if present and parseable) onto `table`. A missing file is a no-op;
/// a malformed file is ignored (the built-ins stand) — a typo in a hand-edited price file must not
/// take cost reporting down.
fn apply_pricing_file(table: &mut PricingTable, path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return; // missing/unreadable → built-ins only
    };
    let Ok(file) = toml::from_str::<PricingFile>(&text) else {
        return; // malformed → built-ins only
    };
    for (model, ov) in &file.models {
        table.apply_override(model, ov);
    }
}

/// The effective pricing table: the built-in curated rates overlaid by `~/.flux/pricing.toml` (if it
/// exists). Always succeeds — a missing or malformed override file yields the built-in table.
pub fn load_pricing_table() -> PricingTable {
    let mut table = PricingTable::builtin();
    if let Ok(path) = pricing_path() {
        apply_pricing_file(&mut table, &path);
    }
    table
}

// ---------------------------------------------------------------------------
// Import from the official CLIs' credential files
// ---------------------------------------------------------------------------

/// Import Claude Code's OAuth tokens from `~/.claude/.credentials.json`.
pub fn import_claude() -> Option<OAuthToken> {
    let path = home().ok()?.join(".claude").join(".credentials.json");
    let data = std::fs::read(&path).ok()?;
    #[derive(Deserialize)]
    struct Creds {
        #[serde(rename = "claudeAiOauth")]
        oauth: ClaudeOauth,
    }
    #[derive(Deserialize)]
    struct ClaudeOauth {
        #[serde(rename = "accessToken")]
        access_token: String,
        #[serde(rename = "refreshToken", default)]
        refresh_token: Option<String>,
        #[serde(rename = "expiresAt", default)]
        expires_at: Option<i64>,
    }
    let creds: Creds = serde_json::from_slice(&data).ok()?;
    if creds.oauth.access_token.is_empty() {
        return None;
    }
    Some(OAuthToken {
        access: creds.oauth.access_token,
        refresh: creds.oauth.refresh_token,
        expires_at_ms: creds.oauth.expires_at, // already ms
        account_id: None,
    })
}

/// Import Codex's OAuth tokens from `~/.codex/auth.json`.
pub fn import_codex() -> Option<OAuthToken> {
    let path = home().ok()?.join(".codex").join("auth.json");
    let data = std::fs::read(&path).ok()?;
    #[derive(Deserialize)]
    struct AuthFile {
        #[serde(default)]
        tokens: Tokens,
    }
    #[derive(Default, Deserialize)]
    struct Tokens {
        #[serde(default)]
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        account_id: Option<String>,
        /// The OIDC id token. Real codex auth files nest the ChatGPT account id in its claims.
        #[serde(default)]
        id_token: Option<String>,
    }
    let auth: AuthFile = serde_json::from_slice(&data).ok()?;
    if auth.tokens.access_token.is_empty() && auth.tokens.refresh_token.is_none() {
        return None;
    }
    let expires_at_ms = jwt_expiry_ms(&auth.tokens.access_token);
    // The ChatGPT backend rejects requests without `chatgpt-account-id`. Prefer the explicit
    // top-level field, but fall back to the `id_token` claims, where the official CLI actually
    // puts the account id in practice.
    let account_id = auth
        .tokens
        .account_id
        .filter(|s| !s.is_empty())
        .or_else(|| {
            auth.tokens
                .id_token
                .as_deref()
                .and_then(account_id_from_id_token)
        });
    Some(OAuthToken {
        access: auth.tokens.access_token,
        refresh: auth.tokens.refresh_token,
        expires_at_ms,
        account_id,
    })
}

// ---------------------------------------------------------------------------
// Refreshers (provider-specific token refresh)
// ---------------------------------------------------------------------------

/// The result of a refresh: a new access token + (possibly rotated) refresh token + expiry.
/// `id_token` is only present on codex responses; its claims carry the ChatGPT account id.
#[derive(Debug)]
struct Refreshed {
    access: String,
    refresh: Option<String>,
    expires_at_ms: Option<i64>,
    id_token: Option<String>,
}

#[async_trait]
trait Refresher: Send + Sync {
    async fn refresh(&self, refresh_token: &str) -> Result<Refreshed>;
}

#[derive(Deserialize)]
struct TokenResp {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    /// OIDC id token (codex responses); the ChatGPT account id lives in its claims.
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

impl TokenResp {
    fn into_refreshed(self) -> Result<Refreshed> {
        if let Some(err) = self.error {
            return Err(Error::Auth(format!(
                "token refresh failed: {err}: {}",
                self.error_description.unwrap_or_default()
            )));
        }
        if self.access_token.is_empty() {
            return Err(Error::Auth(
                "empty access token in refresh response".to_string(),
            ));
        }
        let expires_at_ms = self
            .expires_in
            .map(|s| now_ms() + s * 1000)
            .or_else(|| jwt_expiry_ms(&self.access_token));
        Ok(Refreshed {
            access: self.access_token,
            refresh: self.refresh_token,
            expires_at_ms,
            id_token: self.id_token,
        })
    }
}

struct AnthropicRefresher {
    http: reqwest::Client,
}

#[async_trait]
impl Refresher for AnthropicRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<Refreshed> {
        let resp = self
            .http
            .post(ANTHROPIC_TOKEN_URL)
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": ANTHROPIC_CLIENT_ID,
            }))
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        parse_token_resp(resp, Some("claude")).await
    }
}

struct CodexRefresher {
    http: reqwest::Client,
}

#[async_trait]
impl Refresher for CodexRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<Refreshed> {
        let resp = self
            .http
            .post(CODEX_TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", CODEX_CLIENT_ID),
            ])
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        parse_token_resp(resp, Some("codex")).await
    }
}

async fn parse_token_resp(
    resp: reqwest::Response,
    relogin_hint: Option<&str>,
) -> Result<Refreshed> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| Error::Http(e.to_string()))?;
    refreshed_from_body(status.as_u16(), &body, relogin_hint)
}

/// Turn a token-grant HTTP response (status + body) into a [`Refreshed`], or an actionable auth
/// error. Pure and testable.
///
/// A **failed** grant does not return the success shape: the provider replies with an OAuth error,
/// and OpenAI (codex) wraps it in a NESTED envelope (`{"error":{"message":…,"type":…}}`) rather than
/// the RFC-6749 flat form (`{"error":"invalid_grant",…}`). The old code decoded *every* response into
/// the success struct [`TokenResp`], whose `error` is `Option<String>` — so a nested envelope died
/// with `invalid type: map, expected a string`, masking the real reason (usually an expired refresh
/// token). Non-2xx bodies are now read leniently and surfaced as the reason, plus (for a token
/// *refresh*, where the fix is to re-authenticate) a `flux auth login <relogin_hint>` hint.
fn refreshed_from_body(status: u16, body: &str, relogin_hint: Option<&str>) -> Result<Refreshed> {
    if !(200..300).contains(&status) {
        let hint = relogin_hint
            .map(|p| format!(" Re-authenticate with `flux auth login {p}`."))
            .unwrap_or_default();
        return Err(Error::Auth(format!(
            "token grant failed (status {status}): {}.{hint}",
            oauth_error_detail(body)
        )));
    }
    let parsed: TokenResp = serde_json::from_str(body)
        .map_err(|e| Error::Auth(format!("decode refresh response (status {status}): {e}")))?;
    parsed.into_refreshed()
}

/// Extract a human-readable reason from a failed OAuth token-grant body, tolerant of both shapes:
/// the RFC-6749 flat form (`{"error":"invalid_grant","error_description":"…"}`) and OpenAI's nested
/// envelope (`{"error":{"message":"…","type":"…"}}`). Falls back to a truncated raw body.
fn oauth_error_detail(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => match v.get("error") {
            Some(serde_json::Value::String(code)) => {
                match v.get("error_description").and_then(|d| d.as_str()) {
                    Some(desc) if !desc.is_empty() => format!("{code}: {desc}"),
                    _ => code.clone(),
                }
            }
            Some(serde_json::Value::Object(obj)) => obj
                .get("message")
                .or_else(|| obj.get("type"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| truncate_body(body)),
            _ => truncate_body(body),
        },
        Err(_) => truncate_body(body),
    }
}

/// A trimmed, length-bounded view of a raw response body for error messages (char-safe).
fn truncate_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() > 300 {
        format!("{}…", trimmed.chars().take(300).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Generic plugin OAuth (plugin-oauth epic, D-81) — the host runs every grant so a
// plugin only declares its endpoints and consumes a fresh bearer.
// ---------------------------------------------------------------------------

/// Run a generic RFC-6749 token grant (form-encoded) against `token_url` and return the storable
/// token. The host runs every plugin OAuth grant through here — the plugin never touches
/// `/oauth/token` (plugin-oauth, D-81). `params` is the grant body (e.g. `grant_type=refresh_token`,
/// `authorization_code`, `password`, or `client_credentials` + the matching credentials).
pub async fn oauth_token_grant(token_url: &str, params: &[(&str, &str)]) -> Result<OAuthToken> {
    let resp = reqwest::Client::new()
        .post(token_url)
        .form(params)
        .send()
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    let r = parse_token_resp(resp, None).await?;
    Ok(OAuthToken {
        access: r.access,
        refresh: r.refresh,
        expires_at_ms: r.expires_at_ms,
        account_id: r.id_token.as_deref().and_then(account_id_from_id_token),
    })
}

/// Persist a token for `key` in the credential store (file backend, `~/.flux/credentials.toml`,
/// 0600). Keyed by an arbitrary string, so a plugin token is stored under `plugin:<name>:<purpose>`
/// while provider tokens keep their `claude`/`codex` keys (plugin-oauth, D-81/D-83).
pub fn save_token(key: &str, token: &OAuthToken) -> Result<()> {
    save_stored(key, token)
}

/// Load a stored token for `key`, if any.
pub fn load_token(key: &str) -> Option<OAuthToken> {
    load_stored(key)
}

/// Delete the token stored under `key` (`flux auth set --clear`). Missing entries are a no-op.
pub fn delete_token(key: &str) -> Result<()> {
    delete_stored(key)
}

/// Resolve a fresh bearer access token for `key` from the credential store, refreshing it via a
/// generic `refresh_token` grant against `token_url` when the stored token is within the refresh
/// buffer of expiry (plugin-oauth, D-81). Returns `Ok(None)` when nothing is stored under `key` — the
/// caller then falls back to a declared env secret. A refreshed/rotated token is persisted back
/// (best-effort: a failed write must not fail the request).
pub async fn resolve_stored_bearer(
    store: &dyn CredentialStore,
    key: &str,
    token_url: &str,
    client_id: &str,
) -> Result<Option<String>> {
    let Some(mut tok) = store.load(key).await else {
        return Ok(None);
    };
    let stale = tok
        .expires_at_ms
        .map(|exp| now_ms() + REFRESH_BUFFER_MS >= exp)
        .unwrap_or(false);
    if stale {
        if let Some(refresh) = tok.refresh.clone() {
            let refreshed = oauth_token_grant(
                token_url,
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &refresh),
                    ("client_id", client_id),
                ],
            )
            .await?;
            tok.access = refreshed.access;
            if refreshed.refresh.is_some() {
                tok.refresh = refreshed.refresh;
            }
            tok.expires_at_ms = refreshed.expires_at_ms;
            let _ = store.save(key, &tok).await;
        }
        // Stale with no refresh token: return the stale access and let the API 401.
    }
    Ok(Some(tok.access))
}

// ---------------------------------------------------------------------------
// Credential store backends (plugin-oauth epic, D-83) — a pluggable store so
// tokens live in a local 0600 file for dev/CLI, or in Vault when deployed.
// ---------------------------------------------------------------------------

/// A backend that persists OAuth tokens, keyed by an arbitrary string (`plugin:<name>:<purpose>` for
/// a plugin, `claude`/`codex` for a provider). The default is [`FileCredentialStore`]; a host app can
/// inject a [`VaultCredentialStore`] (or its own) the way it injects custom host capabilities, so
/// credentials never sit in a file on a pod (plugin-oauth, D-83).
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Load the token stored under `key`, if any.
    async fn load(&self, key: &str) -> Option<OAuthToken>;
    /// Persist `token` under `key`.
    async fn save(&self, key: &str, token: &OAuthToken) -> Result<()>;
}

/// The default backend: `~/.flux/credentials.toml` (0600), the same store provider logins use — so
/// `claude`/`codex` keep working unchanged.
#[derive(Debug, Default, Clone, Copy)]
pub struct FileCredentialStore;

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self, key: &str) -> Option<OAuthToken> {
        load_stored(key)
    }
    async fn save(&self, key: &str, token: &OAuthToken) -> Result<()> {
        save_stored(key, token)
    }
}

/// Configuration for authenticating a [`VaultCredentialStore`] through Vault's Kubernetes auth
/// method. The projected service-account JWT is read for every login, so kubelet token rotation is
/// honored when a Vault lease expires or is rejected (D-130).
#[derive(Debug, Clone)]
pub struct VaultKubernetesConfig {
    pub addr: String,
    pub role: String,
    pub auth_mount: String,
    pub service_account_token_path: PathBuf,
    pub mount: String,
    pub prefix: String,
}

impl VaultKubernetesConfig {
    /// Build the deployment defaults: auth mount `kubernetes`, the standard projected
    /// service-account token path, KV-v2 mount `secret`, and credential prefix `flux`.
    pub fn new(addr: impl Into<String>, role: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            role: role.into(),
            auth_mount: "kubernetes".to_string(),
            service_account_token_path: PathBuf::from(
                "/var/run/secrets/kubernetes.io/serviceaccount/token",
            ),
            mount: "secret".to_string(),
            prefix: "flux".to_string(),
        }
    }

    /// Override the Vault auth-method mount (default `kubernetes`).
    pub fn with_auth_mount(mut self, mount: impl Into<String>) -> Self {
        self.auth_mount = mount.into();
        self
    }

    /// Override the projected Kubernetes service-account JWT path.
    pub fn with_service_account_token_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.service_account_token_path = path.into();
        self
    }

    /// Override the KV-v2 secrets-engine mount (default `secret`).
    pub fn with_mount(mut self, mount: impl Into<String>) -> Self {
        self.mount = mount.into();
        self
    }

    /// Override the path prefix beneath the KV-v2 mount (default `flux`).
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }
}

struct VaultSession {
    token: String,
    expires_at: Instant,
    renewable: bool,
}

struct KubernetesVaultAuth {
    role: String,
    auth_mount: String,
    service_account_token_path: PathBuf,
    session: tokio::sync::Mutex<Option<VaultSession>>,
}

enum VaultAuth {
    Static(String),
    Kubernetes(KubernetesVaultAuth),
}

const VAULT_RENEW_BUFFER: Duration = Duration::from_secs(60);

/// A HashiCorp Vault KV-v2 backend (plugin-oauth, D-83): tokens are read/written at
/// `<addr>/v1/<mount>/data/<prefix>/<key>` with an `X-Vault-Token` header. Host-injectable for a
/// deployment where per-customer tokens must live in Vault, not a file on a pod. Key `:` separators
/// map to Vault path segments. It supports either the original static token or an eagerly-validated,
/// renewable Kubernetes-auth session (D-130).
pub struct VaultCredentialStore {
    addr: String,
    auth: VaultAuth,
    mount: String,
    prefix: String,
    http: reqwest::Client,
}

impl VaultCredentialStore {
    /// `addr` = the Vault base URL, `token` = a Vault token, `mount` = the KV-v2 mount (e.g.
    /// `secret`), `prefix` = a path prefix under the mount (e.g. `flux`).
    pub fn new(
        addr: impl Into<String>,
        token: impl Into<String>,
        mount: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Self {
        Self {
            addr: addr.into().trim_end_matches('/').to_string(),
            auth: VaultAuth::Static(token.into()),
            mount: mount.into(),
            prefix: prefix.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Build from the standard Vault env (`VAULT_ADDR`, `VAULT_TOKEN`) plus optional
    /// `FLUX_VAULT_MOUNT` (default `secret`) / `FLUX_VAULT_PREFIX` (default `flux`). `None` when the
    /// address or token is unset — the caller then keeps the file backend.
    pub fn from_env() -> Option<Self> {
        let addr = std::env::var("VAULT_ADDR").ok().filter(|s| !s.is_empty())?;
        let token = std::env::var("VAULT_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())?;
        let mount = std::env::var("FLUX_VAULT_MOUNT").unwrap_or_else(|_| "secret".to_string());
        let prefix = std::env::var("FLUX_VAULT_PREFIX").unwrap_or_else(|_| "flux".to_string());
        Some(Self::new(addr, token, mount, prefix))
    }

    /// Authenticate eagerly through Vault's Kubernetes auth method and return a KV-v2 store whose
    /// Vault token renews before lease expiry. A failed renewal or a 401/403 re-reads the projected
    /// service-account JWT and logs in again; construction fails rather than leaving a deployment
    /// to discover invalid Vault configuration on its first customer request.
    pub async fn connect_kubernetes(config: VaultKubernetesConfig) -> Result<Self> {
        for (name, value) in [
            ("Vault address", config.addr.as_str()),
            ("Vault Kubernetes role", config.role.as_str()),
            ("Vault auth mount", config.auth_mount.as_str()),
            ("Vault KV mount", config.mount.as_str()),
            ("Vault credential prefix", config.prefix.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(Error::Config(format!("{name} must not be empty")));
            }
        }
        let store = Self {
            addr: config.addr.trim_end_matches('/').to_string(),
            auth: VaultAuth::Kubernetes(KubernetesVaultAuth {
                role: config.role,
                auth_mount: config.auth_mount,
                service_account_token_path: config.service_account_token_path,
                session: tokio::sync::Mutex::new(None),
            }),
            mount: config.mount,
            prefix: config.prefix,
            http: reqwest::Client::new(),
        };
        store.vault_token().await?;
        Ok(store)
    }

    fn data_url(&self, key: &str) -> String {
        let path = key.replace(':', "/");
        format!(
            "{}/v1/{}/data/{}/{}",
            self.addr, self.mount, self.prefix, path
        )
    }

    async fn login_kubernetes(&self, auth: &KubernetesVaultAuth) -> Result<VaultSession> {
        let jwt = tokio::fs::read_to_string(&auth.service_account_token_path)
            .await
            .map_err(|e| {
                Error::Auth(format!(
                    "read projected Kubernetes service-account token at {}: {e}",
                    auth.service_account_token_path.display()
                ))
            })?;
        let jwt = jwt.trim();
        if jwt.is_empty() {
            return Err(Error::Auth(
                "projected Kubernetes service-account token is empty".to_string(),
            ));
        }
        let url = format!(
            "{}/v1/auth/{}/login",
            self.addr,
            auth.auth_mount.trim_matches('/')
        );
        let resp = self
            .http
            .post(url)
            .json(&serde_json::json!({ "role": auth.role, "jwt": jwt }))
            .send()
            .await
            .map_err(|e| Error::Http(format!("Vault Kubernetes login failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Auth(format!(
                "Vault Kubernetes login returned {}",
                resp.status()
            )));
        }
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Auth(format!("decode Vault Kubernetes login response: {e}")))?;
        Self::session_from_response(&value, None)
    }

    async fn renew_kubernetes(&self, previous: &VaultSession) -> Result<VaultSession> {
        let url = format!("{}/v1/auth/token/renew-self", self.addr);
        let resp = self
            .http
            .post(url)
            .header("X-Vault-Token", &previous.token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| Error::Http(format!("Vault token renewal failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Auth(format!(
                "Vault token renewal returned {}",
                resp.status()
            )));
        }
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Auth(format!("decode Vault token renewal response: {e}")))?;
        Self::session_from_response(&value, Some(&previous.token))
    }

    fn session_from_response(
        value: &serde_json::Value,
        previous: Option<&str>,
    ) -> Result<VaultSession> {
        let auth = value
            .get("auth")
            .ok_or_else(|| Error::Auth("Vault auth response had no `auth` object".to_string()))?;
        let token = auth
            .get("client_token")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .or(previous)
            .ok_or_else(|| Error::Auth("Vault auth response had no client token".to_string()))?;
        let lease = auth
            .get("lease_duration")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| Error::Auth("Vault auth response had no lease duration".to_string()))?;
        let expires_at = Instant::now()
            .checked_add(Duration::from_secs(lease))
            .ok_or_else(|| Error::Auth("Vault auth lease duration overflowed".to_string()))?;
        Ok(VaultSession {
            token: token.to_string(),
            expires_at,
            renewable: auth
                .get("renewable")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
    }

    async fn vault_token(&self) -> Result<String> {
        match &self.auth {
            VaultAuth::Static(token) => Ok(token.clone()),
            VaultAuth::Kubernetes(auth) => {
                let mut session = auth.session.lock().await;
                let expiring = session
                    .as_ref()
                    .map(|current| {
                        current.expires_at.saturating_duration_since(Instant::now())
                            <= VAULT_RENEW_BUFFER
                    })
                    .unwrap_or(true);
                if expiring {
                    let next = match session.as_ref() {
                        Some(current) if current.renewable => {
                            match self.renew_kubernetes(current).await {
                                Ok(renewed) => renewed,
                                Err(_) => self.login_kubernetes(auth).await?,
                            }
                        }
                        _ => self.login_kubernetes(auth).await?,
                    };
                    *session = Some(next);
                }
                Ok(session
                    .as_ref()
                    .expect("an authenticated Vault session")
                    .token
                    .clone())
            }
        }
    }

    async fn invalidate_kubernetes_token(&self, rejected: &str) {
        if let VaultAuth::Kubernetes(auth) = &self.auth {
            let mut session = auth.session.lock().await;
            if session.as_ref().map(|s| s.token.as_str()) == Some(rejected) {
                *session = None;
            }
        }
    }

    async fn send_vault(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response> {
        let token = self.vault_token().await?;
        let response = self
            .send_vault_once(method.clone(), url, body, &token)
            .await?;
        if matches!(response.status().as_u16(), 401 | 403)
            && matches!(self.auth, VaultAuth::Kubernetes(_))
        {
            self.invalidate_kubernetes_token(&token).await;
            let fresh = self.vault_token().await?;
            return self.send_vault_once(method, url, body, &fresh).await;
        }
        Ok(response)
    }

    async fn send_vault_once(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<&serde_json::Value>,
        token: &str,
    ) -> Result<reqwest::Response> {
        let request = self
            .http
            .request(method, url)
            .header("X-Vault-Token", token);
        let request = match body {
            Some(body) => request.json(body),
            None => request,
        };
        request
            .send()
            .await
            .map_err(|e| Error::Http(format!("Vault request failed: {e}")))
    }
}

#[async_trait]
impl CredentialStore for VaultCredentialStore {
    async fn load(&self, key: &str) -> Option<OAuthToken> {
        let resp = self
            .send_vault(reqwest::Method::GET, &self.data_url(key), None)
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None; // 404 = no such secret
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        // KV v2 nests the payload under data.data.
        let s = v.get("data")?.get("data")?.get("token")?.as_str()?;
        serde_json::from_str(s).ok()
    }
    async fn save(&self, key: &str, token: &OAuthToken) -> Result<()> {
        let payload = serde_json::json!({
            "data": { "token": serde_json::to_string(token).map_err(|e| Error::Config(e.to_string()))? }
        });
        let resp = self
            .send_vault(reqwest::Method::POST, &self.data_url(key), Some(&payload))
            .await?;
        if !resp.status().is_success() {
            return Err(Error::Http(format!(
                "vault write failed: {}",
                resp.status()
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RefreshingToken — the TokenSource handed to OAuth credentials
// ---------------------------------------------------------------------------

/// A [`TokenSource`] that lazily refreshes an [`OAuthToken`] when it is near expiry, persisting
/// the refreshed token back to the store. Refresh is serialized behind an async mutex.
pub struct RefreshingToken {
    provider: String,
    token: tokio::sync::Mutex<OAuthToken>,
    refresher: Box<dyn Refresher>,
    account_id: Option<String>,
    /// Unix-ms of the last successful refresh (0 = never); used to coalesce a burst of forced 401
    /// refreshes. Read/written under the `token` mutex.
    last_refresh_ms: std::sync::atomic::AtomicI64,
}

impl RefreshingToken {
    fn new(provider: &str, token: OAuthToken, refresher: Box<dyn Refresher>) -> Self {
        let account_id = token.account_id.clone();
        Self {
            provider: provider.to_string(),
            token: tokio::sync::Mutex::new(token),
            refresher,
            account_id,
            last_refresh_ms: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// Run the refresh POST and apply the result to `tok` (caller holds the lock), persisting and
    /// stamping the refresh time. Errors if there is no refresh token to spend.
    async fn refresh_locked(&self, tok: &mut OAuthToken) -> Result<()> {
        let Some(refresh) = tok.refresh.clone() else {
            return Err(Error::Auth(format!(
                "cannot refresh {} token: no refresh token (re-import or re-login)",
                self.provider
            )));
        };
        let refreshed = self.refresher.refresh(&refresh).await?;
        tok.access = refreshed.access;
        if refreshed.refresh.is_some() {
            tok.refresh = refreshed.refresh;
        }
        tok.expires_at_ms = refreshed.expires_at_ms;
        self.last_refresh_ms
            .store(now_ms(), std::sync::atomic::Ordering::SeqCst);
        // Best-effort persistence; a failed write must not break the request.
        let _ = save_stored(&self.provider, tok);
        Ok(())
    }
}

#[async_trait]
impl TokenSource for RefreshingToken {
    async fn access_token(&self) -> Result<String> {
        let mut tok = self.token.lock().await;

        let needs_refresh = match tok.expires_at_ms {
            Some(exp) => now_ms() + REFRESH_BUFFER_MS >= exp,
            None => false,
        };

        if needs_refresh {
            if tok.refresh.is_none() {
                // Expired with no refresh token — return what we have and let the API reject it.
                return Ok(tok.access.clone());
            }
            self.refresh_locked(&mut tok).await?;
        }

        Ok(tok.access.clone())
    }

    fn account_id(&self) -> Option<String> {
        self.account_id.clone()
    }

    /// Force a refresh ignoring the expiry buffer (called by the HTTP path on a 401). Coalesces a
    /// concurrent burst into a single refresh: if one already succeeded within the dedup window the
    /// in-memory token is already fresh, so reuse it rather than spending the grant again.
    async fn refresh(&self) -> Result<()> {
        let mut tok = self.token.lock().await;
        let last = self
            .last_refresh_ms
            .load(std::sync::atomic::Ordering::SeqCst);
        if last != 0 && now_ms() - last < FORCE_REFRESH_DEDUP_MS {
            return Ok(());
        }
        self.refresh_locked(&mut tok).await
    }
}

// ---------------------------------------------------------------------------
// Token-source acquisition (stored → imported)
// ---------------------------------------------------------------------------

/// Token source for the `claude` provider: stored flux credential, else imported Claude Code.
pub fn claude_token_source() -> Result<Arc<dyn TokenSource>> {
    let token = load_stored("claude").or_else(import_claude).ok_or_else(|| {
        Error::Auth(
            "no Claude subscription credentials — log into Claude Code, or run `flux auth login claude`"
                .to_string(),
        )
    })?;
    Ok(Arc::new(RefreshingToken::new(
        "claude",
        token,
        Box::new(AnthropicRefresher {
            http: reqwest::Client::new(),
        }),
    )))
}

/// Token source for the `codex` provider: stored flux credential, else imported Codex CLI.
pub fn codex_token_source() -> Result<Arc<dyn TokenSource>> {
    let token = load_stored("codex").or_else(import_codex).ok_or_else(|| {
        Error::Auth(
            "no Codex subscription credentials — log into the Codex CLI, or run `flux auth login codex`"
                .to_string(),
        )
    })?;
    Ok(Arc::new(RefreshingToken::new(
        "codex",
        token,
        Box::new(CodexRefresher {
            http: reqwest::Client::new(),
        }),
    )))
}

// ---------------------------------------------------------------------------
// PKCE + Anthropic login
// ---------------------------------------------------------------------------

/// A PKCE verifier/challenge pair.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Generate a PKCE pair (verifier = base64url(32 random bytes), challenge = base64url(sha256)).
pub fn generate_pkce() -> Pkce {
    // rand 0.10 renamed `OsRng` to `SysRng` behind the fallible `TryRng`; rand 0.8's
    // `OsRng::fill_bytes` panicked on OS-entropy failure, so `expect` preserves behavior.
    use rand::{rngs::SysRng, TryRng};
    let mut buf = [0u8; 32];
    SysRng
        .try_fill_bytes(&mut buf)
        .expect("OS entropy unavailable");
    let verifier = b64().encode(buf);
    let challenge = b64().encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

/// Random URL-safe state value.
pub fn generate_state() -> String {
    use rand::{rngs::SysRng, TryRng};
    let mut buf = [0u8; 32];
    SysRng
        .try_fill_bytes(&mut buf)
        .expect("OS entropy unavailable");
    b64().encode(buf)
}

/// `base?k=v&…` with percent-encoded values.
fn build_url(base: &str, q: &[(&str, &str)]) -> String {
    let qs = q
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}?{qs}")
}

/// Split a callback value of the shape `code[#state]` and enforce the CSRF state binding: when a
/// state is present it MUST match the one we generated for this login — otherwise the code may be
/// attacker-supplied (OAuth login-CSRF / account injection). PKCE is the primary defense; this is
/// the binding. Returns the bare code. Runs **before** any network I/O in the exchange paths.
fn bind_callback_state<'a>(code: &'a str, state: &str) -> Result<&'a str> {
    let (code, callback_state) = match code.split_once('#') {
        Some((c, s)) => (c.trim(), Some(s.trim())),
        None => (code.trim(), None),
    };
    if let Some(cb) = callback_state {
        if cb != state {
            return Err(Error::Config(
                "OAuth state mismatch — aborting login (possible CSRF or a code from a different \
                 session was pasted)"
                    .into(),
            ));
        }
    }
    Ok(code)
}

/// Build the Anthropic authorization URL the user visits to approve flux.
pub fn anthropic_authorize_url(pkce: &Pkce, state: &str) -> String {
    build_url(
        ANTHROPIC_AUTHORIZE_URL,
        &[
            ("code", "true"),
            ("client_id", ANTHROPIC_CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", ANTHROPIC_REDIRECT_URI),
            ("scope", ANTHROPIC_SCOPE),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
        ],
    )
}

/// Exchange an authorization code (the user pastes the callback value) for tokens and persist
/// them under the `claude` provider.
pub async fn anthropic_exchange_and_store(code: &str, state: &str, verifier: &str) -> Result<()> {
    // The callback value is pasted as `code#state`; enforce the CSRF binding before any network.
    let code = bind_callback_state(code, state)?;
    let resp = reqwest::Client::new()
        .post(ANTHROPIC_TOKEN_URL)
        .json(&serde_json::json!({
            "code": code,
            "state": state,
            "grant_type": "authorization_code",
            "client_id": ANTHROPIC_CLIENT_ID,
            "redirect_uri": ANTHROPIC_REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .send()
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    let refreshed = parse_token_resp(resp, None).await?;
    save_stored(
        "claude",
        &OAuthToken {
            access: refreshed.access,
            refresh: refreshed.refresh,
            expires_at_ms: refreshed.expires_at_ms,
            account_id: None,
        },
    )
}

/// Build the Codex (ChatGPT subscription) authorization URL the user visits to approve flux.
///
/// Mirrors the upstream codex CLI's `build_authorize_url` (openai/codex,
/// `codex-rs/login/src/server.rs`): same client id, the registered `localhost:1455` redirect,
/// S256 PKCE, and the `id_token_add_organizations` / `codex_cli_simplified_flow` switches the CLI
/// sets (the former puts the org/account claims in the id token, where flux reads the ChatGPT
/// account id from). Scope is the core OIDC subset (see [`CODEX_SCOPE`]).
pub fn codex_authorize_url(pkce: &Pkce, state: &str) -> String {
    build_url(
        CODEX_AUTHORIZE_URL,
        &[
            ("response_type", "code"),
            ("client_id", CODEX_CLIENT_ID),
            ("redirect_uri", CODEX_REDIRECT_URI),
            ("scope", CODEX_SCOPE),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("state", state),
        ],
    )
}

/// Build a generic RFC-6749 `authorization_code` + PKCE authorize URL (plugin-oauth, D-82) — the
/// provider-agnostic form of [`codex_authorize_url`], parameterized on a plugin's manifest config
/// instead of provider constants.
pub fn oauth_authorize_url(
    authorize_url: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    pkce: &Pkce,
    state: &str,
) -> String {
    build_url(
        authorize_url,
        &[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("scope", scope),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
        ],
    )
}

/// Exchange a codex authorization code for tokens and persist them under the `codex` provider —
/// the same store slot the `~/.codex/auth.json` import path fills, so everything downstream
/// (`codex_token_source`, refresh) is shared.
///
/// `code` is the callback value, optionally suffixed `#state` (the login harness forwards the
/// callback's `code#state`); a present state MUST match the `state` generated for this login —
/// the same CSRF binding as the claude flow, enforced before any network I/O. The exchange itself
/// mirrors the upstream codex CLI: a form-encoded `authorization_code` grant with the PKCE
/// verifier, whose response carries an `id_token` with the ChatGPT account id in its claims.
pub async fn codex_exchange_and_store(code: &str, state: &str, verifier: &str) -> Result<()> {
    codex_exchange_and_store_at(CODEX_TOKEN_URL, code, state, verifier).await
}

/// [`codex_exchange_and_store`] against an explicit token endpoint — the seam hermetic login
/// tests use to point the exchange at a loopback stub instead of auth.openai.com.
pub async fn codex_exchange_and_store_at(
    token_url: &str,
    code: &str,
    state: &str,
    verifier: &str,
) -> Result<()> {
    let code = bind_callback_state(code, state)?;
    let resp = reqwest::Client::new()
        .post(token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", CODEX_REDIRECT_URI),
            ("client_id", CODEX_CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    let refreshed = parse_token_resp(resp, None).await?;
    // The ChatGPT backend rejects requests without `chatgpt-account-id`; pull it from the
    // id token's claims, exactly as the import path does.
    let account_id = refreshed
        .id_token
        .as_deref()
        .and_then(account_id_from_id_token);
    save_stored(
        "codex",
        &OAuthToken {
            access: refreshed.access,
            refresh: refreshed.refresh,
            expires_at_ms: refreshed.expires_at_ms,
            account_id,
        },
    )
}

/// Minimal percent-encoding for query values (alnum and `-._~` pass through).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Status reporting
// ---------------------------------------------------------------------------

/// Per-provider auth availability, for `flux auth status`.
pub struct ProviderAuth {
    pub provider: &'static str,
    pub available: bool,
    /// Where the credential resolved from when available — e.g. `ANTHROPIC_API_KEY (env)`,
    /// `flux store`, `imported ~/.claude/.credentials.json`. A short status when not (`not set`).
    pub source: String,
    /// How to configure this provider when it is NOT available — e.g. `flux auth login claude`,
    /// `set ANTHROPIC_API_KEY`. `None` once the provider is available.
    pub hint: Option<String>,
}

/// The environment variables whose values are provider credentials — the single source hosts use
/// to seed a `flux_secret::Redactor`, so a leaked `env`/`printenv`/debug dump in tool output is
/// scrubbed. Covers the API-key providers plus the AWS secret material the Bedrock credential
/// chain materializes into the process environment (the access-key *id* is an identifier, not a
/// secret, and appears legitimately in ARNs and logs — deliberately not listed).
pub fn provider_env_keys() -> &'static [&'static str] {
    &[
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
    ]
}

/// Report what credentials are available for each provider, in resolution-chain order.
pub fn auth_status() -> Vec<ProviderAuth> {
    let env_status = |provider: &'static str, var: &str| {
        let ok = std::env::var(var)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        ProviderAuth {
            provider,
            available: ok,
            source: if ok {
                format!("{var} (env)")
            } else {
                "not set".into()
            },
            hint: if ok {
                None
            } else {
                Some(format!("set ${var}"))
            },
        }
    };
    let oauth_status = |provider: &'static str, stored_key: &str, imported: Option<OAuthToken>| {
        if load_stored(stored_key).is_some() {
            ProviderAuth {
                provider,
                available: true,
                source: "flux store".into(),
                hint: None,
            }
        } else if imported.is_some() {
            let file = if stored_key == "claude" {
                "~/.claude/.credentials.json"
            } else {
                "~/.codex/auth.json"
            };
            ProviderAuth {
                provider,
                available: true,
                source: format!("imported {file}"),
                hint: None,
            }
        } else {
            ProviderAuth {
                provider,
                available: false,
                source: "not found".into(),
                hint: Some(format!("flux auth login {provider}")),
            }
        }
    };

    vec![
        env_status("anthropic", "ANTHROPIC_API_KEY"),
        oauth_status("claude", "claude", import_claude()),
        env_status("openai", "OPENAI_API_KEY"),
        oauth_status("codex", "codex", import_codex()),
        env_status("openrouter", "OPENROUTER_API_KEY"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that repoint `HOME` — the process env is shared across the parallel
    /// test threads, so two concurrent `set_var("HOME", …)` tests race and flake.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn jwt_expiry_decodes_exp() {
        // header.{"exp":2000000000}.sig  (exp = 2033-05-18)
        let payload = b64().encode(br#"{"exp":2000000000}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(jwt_expiry_ms(&token), Some(2_000_000_000 * 1000));
        assert_eq!(jwt_expiry_ms("not-a-jwt"), None);
    }

    #[test]
    fn nested_oauth_error_envelope_yields_actionable_error_not_a_decode_crash() {
        // OpenAI (codex) returns a NESTED error envelope on a failed refresh. The old code decoded
        // it into the string-typed `TokenResp.error` and died with the cryptic
        // `invalid type: map, expected a string`, hiding the real reason (an expired refresh token).
        let body =
            r#"{"error":{"message":"refresh token is expired","type":"invalid_request_error"}}"#;
        let err = refreshed_from_body(401, body, Some("codex")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("refresh token is expired"),
            "surfaces the real reason: {msg}"
        );
        assert!(
            msg.contains("flux auth login codex"),
            "gives an actionable re-login hint: {msg}"
        );
        assert!(
            !msg.contains("invalid type: map"),
            "no raw serde decode crash leaks through: {msg}"
        );
    }

    #[test]
    fn flat_rfc6749_error_form_is_surfaced_with_description() {
        let body = r#"{"error":"invalid_grant","error_description":"token has expired"}"#;
        let err = refreshed_from_body(401, body, Some("claude")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid_grant"), "{msg}");
        assert!(msg.contains("token has expired"), "{msg}");
        assert!(msg.contains("flux auth login claude"), "{msg}");
    }

    #[test]
    fn non_json_error_body_falls_back_to_raw_text() {
        let err = refreshed_from_body(502, "upstream unavailable", Some("codex")).unwrap_err();
        assert!(err.to_string().contains("upstream unavailable"), "{err}");
    }

    #[test]
    fn successful_refresh_body_still_decodes() {
        let body = r#"{"access_token":"at_abc123","expires_in":3600}"#;
        let refreshed = refreshed_from_body(200, body, Some("codex")).unwrap();
        assert_eq!(refreshed.access, "at_abc123");
        assert!(refreshed.expires_at_ms.is_some());
    }

    #[test]
    fn import_codex_reads_account_id_from_id_token() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        // Hermetic temp HOME (unique per run) so no real ~/.codex is read.
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!("flux-cred-codex-{}-{n}", std::process::id()));
        std::fs::create_dir_all(home.join(".codex")).unwrap();

        // Real codex auth files leave `tokens.account_id` absent and nest the account id inside the
        // `id_token` JWT claims. Build an *unsigned* fixture JWT (header.base64url(json).sig) so the
        // test never touches the network or a real token.
        let claims = br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_test_123"}}"#;
        let id_token = format!("h.{}.s", b64().encode(claims));
        let auth_json = serde_json::json!({
            "tokens": {
                "access_token": "at_dummy",
                "refresh_token": "rt_dummy",
                "account_id": null,
                "id_token": id_token,
            }
        });
        std::fs::write(
            home.join(".codex").join("auth.json"),
            serde_json::to_vec(&auth_json).unwrap(),
        )
        .unwrap();

        let _home = HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &home);
        let tok = import_codex().expect("import_codex should read the fixture auth.json");
        std::fs::remove_dir_all(&home).ok();

        assert_eq!(tok.account_id.as_deref(), Some("acct_test_123"));
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let p = generate_pkce();
        let expected = b64().encode(Sha256::digest(p.verifier.as_bytes()));
        assert_eq!(p.challenge, expected);
        assert!(!p.verifier.is_empty());
    }

    #[test]
    fn authorize_url_has_pkce_and_state() {
        let p = Pkce {
            verifier: "v".into(),
            challenge: "chal".into(),
        };
        let url = anthropic_authorize_url(&p, "st8");
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("code_challenge=chal"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st8"));
        assert!(url.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
    }

    #[test]
    fn token_resp_maps_expiry_from_expires_in() {
        let r = TokenResp {
            access_token: "tok".into(),
            refresh_token: Some("r".into()),
            expires_in: Some(3600),
            id_token: None,
            error: None,
            error_description: None,
        }
        .into_refreshed()
        .unwrap();
        assert_eq!(r.access, "tok");
        assert!(r.expires_at_ms.unwrap() > now_ms());
    }

    #[tokio::test]
    async fn oauth_rejects_state_mismatch_before_any_network() {
        // A pasted `code#state` whose state doesn't match the one we generated must abort the login
        // (CSRF / wrong-session guard). The mismatch returns before any HTTP call.
        let r =
            anthropic_exchange_and_store("attackercode#attackerstate", "my-real-state", "verifier")
                .await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("state mismatch"));
    }

    #[test]
    fn codex_authorize_url_has_pkce_and_state() {
        let p = Pkce {
            verifier: "v".into(),
            challenge: "chal".into(),
        };
        let url = codex_authorize_url(&p, "st8");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("code_challenge=chal"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st8"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        // The redirect the codex CLI client is registered for: the localhost:1455 callback.
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("scope=openid%20profile%20email%20offline_access"));
        assert!(url.contains("response_type=code"));
    }

    #[tokio::test]
    async fn codex_oauth_rejects_state_mismatch_before_any_network() {
        // A callback `code#state` whose state doesn't match the one we generated must abort the
        // login (CSRF / wrong-session guard) — same binding as claude. The mismatch returns before
        // any HTTP call: the real token endpoint is unreachable from this test, so anything but the
        // pre-network state check would surface as a connection error, not this message.
        let r = codex_exchange_and_store("attackercode#attackerstate", "my-real-state", "verifier")
            .await;
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("state mismatch"));
    }

    /// Read one HTTP request off `sock` (headers + `Content-Length` body) and answer with a JSON
    /// token response — a stub token endpoint for the exchange tests.
    async fn serve_one_token_response(listener: tokio::net::TcpListener, body: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = sock.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            req.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&req);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let content_length = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if req.len() >= head_end + 4 + content_length {
                    break;
                }
            }
        }
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&req).into_owned()
    }

    async fn read_http_request(sock: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut req = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = sock.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            req.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&req);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let content_length = text
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if req.len() >= head_end + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&req).into_owned()
    }

    async fn write_http_json(sock: &mut tokio::net::TcpStream, status: u16, body: &str) {
        use tokio::io::AsyncWriteExt;
        let reason = match status {
            200 => "OK",
            403 => "Forbidden",
            _ => "Error",
        };
        let resp = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    }

    #[tokio::test]
    // HOME must stay repointed across the exchange (save_stored writes ~/.flux); current-thread
    // test runtime, so holding the std guard across await is safe (same pattern as the C-04 test).
    #[allow(clippy::await_holding_lock)]
    async fn codex_exchange_persists_under_codex_with_account_id() {
        // Hermetic: a loopback stub stands in for auth.openai.com and HOME is a throwaway dir.
        let tmp = std::env::temp_dir().join(format!(
            "flux-cred-c08-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let _home = HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &tmp);

        // Token response whose id_token nests the ChatGPT account id, like real codex tokens.
        let claims = br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_c08"}}"#;
        let id_token = format!("h.{}.s", b64().encode(claims));
        let body = serde_json::json!({
            "access_token": "at_c08",
            "refresh_token": "rt_c08",
            "id_token": id_token,
            "expires_in": 3600,
        })
        .to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_one_token_response(listener, body));

        codex_exchange_and_store_at(
            &format!("http://{addr}/oauth/token"),
            "authcode#st8",
            "st8",
            "verifier-xyz",
        )
        .await
        .unwrap();

        // The exchange was a form-encoded PKCE authorization_code grant (upstream codex shape)...
        let req = server.await.unwrap();
        assert!(req.contains("grant_type=authorization_code"));
        assert!(req.contains("code=authcode"));
        assert!(req.contains("code_verifier=verifier-xyz"));
        assert!(req.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));

        // ...and the token landed under the `codex` provider — the same store import uses.
        let stored = load_stored("codex").expect("token stored under `codex`");
        std::fs::remove_dir_all(&tmp).ok();
        assert_eq!(stored.access, "at_c08");
        assert_eq!(stored.refresh.as_deref(), Some("rt_c08"));
        assert_eq!(stored.account_id.as_deref(), Some("acct_c08"));
        assert!(stored.expires_at_ms.unwrap() > now_ms());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // HOME_LOCK only serializes the HOME-env tests
    async fn resolve_stored_bearer_returns_stored_refreshes_stale_and_none_when_absent() {
        // D-81: resolve_stored_bearer returns a fresh stored bearer, refreshes a stale one via the
        // token endpoint (persisting the result), and returns None when nothing is stored (the caller
        // then falls back to a declared env secret).
        let tmp = std::env::temp_dir().join(format!(
            "flux-cred-d81-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let _home = HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &tmp);

        let key = "plugin:acme:api";

        // No entry → None (env fallback happens on the caller side).
        assert!(
            resolve_stored_bearer(&FileCredentialStore, key, "http://unused/token", "cid")
                .await
                .unwrap()
                .is_none()
        );

        // Fresh token → returned as-is, no network call.
        save_token(
            key,
            &OAuthToken {
                access: "fresh".into(),
                refresh: Some("rt".into()),
                expires_at_ms: Some(now_ms() + 3_600_000),
                account_id: None,
            },
        )
        .unwrap();
        assert_eq!(
            resolve_stored_bearer(&FileCredentialStore, key, "http://unused/token", "cid")
                .await
                .unwrap()
                .as_deref(),
            Some("fresh")
        );

        // Stale token → refreshed via the mock endpoint, persisted, new access returned.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body =
            serde_json::json!({"access_token":"refreshed","refresh_token":"rt2","expires_in":3600})
                .to_string();
        let server = tokio::spawn(serve_one_token_response(listener, body));
        save_token(
            key,
            &OAuthToken {
                access: "stale".into(),
                refresh: Some("rt".into()),
                expires_at_ms: Some(now_ms() - 1000),
                account_id: None,
            },
        )
        .unwrap();
        let got = resolve_stored_bearer(
            &FileCredentialStore,
            key,
            &format!("http://{addr}/oauth/token"),
            "cid",
        )
        .await
        .unwrap();
        assert_eq!(got.as_deref(), Some("refreshed"));
        let req = server.await.unwrap();
        assert!(req.contains("grant_type=refresh_token"));
        assert!(req.contains("refresh_token=rt"));
        assert_eq!(load_token(key).unwrap().access, "refreshed");
        assert_eq!(load_token(key).unwrap().refresh.as_deref(), Some("rt2"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn credential_store_trait_round_trips_and_is_injectable() {
        // D-83: tokens round-trip through the `CredentialStore` trait, and `resolve_stored_bearer`
        // reads from the INJECTED store (proven by an in-memory mock) — not always the file backend.
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Mutex;

        struct MockStore {
            map: Mutex<std::collections::HashMap<String, OAuthToken>>,
            loads: AtomicUsize,
        }
        #[async_trait]
        impl CredentialStore for MockStore {
            async fn load(&self, key: &str) -> Option<OAuthToken> {
                self.loads.fetch_add(1, Ordering::SeqCst);
                self.map.lock().unwrap().get(key).cloned()
            }
            async fn save(&self, key: &str, token: &OAuthToken) -> Result<()> {
                self.map
                    .lock()
                    .unwrap()
                    .insert(key.to_string(), token.clone());
                Ok(())
            }
        }

        let store = MockStore {
            map: Mutex::new(std::collections::HashMap::new()),
            loads: AtomicUsize::new(0),
        };
        // Round-trip through the trait.
        store
            .save(
                "plugin:acme:api",
                &OAuthToken {
                    access: "tok".into(),
                    refresh: None,
                    expires_at_ms: None,
                    account_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(store.load("plugin:acme:api").await.unwrap().access, "tok");

        // `resolve_stored_bearer` reads from the injected mock (no file/network).
        let got = resolve_stored_bearer(&store, "plugin:acme:api", "http://unused/", "cid")
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some("tok"));
        assert!(
            store.loads.load(Ordering::SeqCst) >= 2,
            "the injected store was consulted"
        );
        assert!(
            resolve_stored_bearer(&store, "plugin:absent:x", "http://unused/", "cid")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn vault_credential_store_round_trips_via_kv_v2() {
        // D-83: the Vault backend composes the KV-v2 path (`:` key separators → path segments), POSTs
        // the token, and reads the KV-v2-nested payload back. A stateful loopback stub stands in for
        // Vault (no server needed).
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let vs = VaultCredentialStore::new("http://vault.example/", "tok", "secret", "flux");
        assert_eq!(
            vs.data_url("plugin:acme:api"),
            "http://vault.example/v1/secret/data/flux/plugin/acme/api"
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // POST then GET are handled sequentially by one task, so a plain local var carries the token
        // between them — no shared lock (which clippy would flag as held across the write await).
        let server = tokio::spawn(async move {
            let mut saved: Option<String> = None;
            for _ in 0..2 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let body = if req.starts_with("POST") {
                    let sent = req.split("\r\n\r\n").nth(1).unwrap_or("");
                    let v: serde_json::Value = serde_json::from_str(sent).unwrap();
                    saved = v["data"]["token"].as_str().map(|s| s.to_string());
                    "{}".to_string()
                } else {
                    let tok = saved.clone().unwrap_or_default();
                    serde_json::json!({ "data": { "data": { "token": tok } } }).to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });

        let vs = VaultCredentialStore::new(format!("http://{addr}"), "vault-tok", "secret", "flux");
        vs.save(
            "plugin:acme:api",
            &OAuthToken {
                access: "va".into(),
                refresh: Some("vr".into()),
                expires_at_ms: None,
                account_id: None,
            },
        )
        .await
        .unwrap();
        let got = vs
            .load("plugin:acme:api")
            .await
            .expect("token round-trips through Vault KV-v2");
        assert_eq!(got.access, "va");
        assert_eq!(got.refresh.as_deref(), Some("vr"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn vault_kubernetes_auth_logs_in_and_round_trips_via_kv_v2() {
        let dir = std::env::temp_dir().join(format!(
            "flux-vault-k8s-login-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let jwt_path = dir.join("token");
        std::fs::write(&jwt_path, "projected-jwt-one").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut saved = None;
            for step in 0..3 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let req = read_http_request(&mut sock).await;
                match step {
                    0 => {
                        assert!(req.starts_with("POST /v1/auth/kubernetes/login "));
                        assert!(req.contains(r#""role":"ai-agent-platform""#));
                        assert!(req.contains(r#""jwt":"projected-jwt-one""#));
                        write_http_json(
                            &mut sock,
                            200,
                            r#"{"auth":{"client_token":"vault-one","lease_duration":3600,"renewable":true}}"#,
                        )
                        .await;
                    }
                    1 => {
                        assert!(req.starts_with("POST /v1/secret/data/flux/plugin/acme/api "));
                        assert!(req
                            .to_ascii_lowercase()
                            .contains("x-vault-token: vault-one"));
                        let sent = req.split("\r\n\r\n").nth(1).unwrap_or("");
                        let value: serde_json::Value = serde_json::from_str(sent).unwrap();
                        saved = value["data"]["token"].as_str().map(ToOwned::to_owned);
                        write_http_json(&mut sock, 200, "{}").await;
                    }
                    _ => {
                        assert!(req.starts_with("GET /v1/secret/data/flux/plugin/acme/api "));
                        assert!(req
                            .to_ascii_lowercase()
                            .contains("x-vault-token: vault-one"));
                        let body = serde_json::json!({
                            "data": { "data": { "token": saved.as_deref().expect("saved token") } }
                        })
                        .to_string();
                        write_http_json(&mut sock, 200, &body).await;
                    }
                }
            }
        });

        let config = VaultKubernetesConfig::new(format!("http://{addr}"), "ai-agent-platform")
            .with_service_account_token_path(&jwt_path);
        let store = VaultCredentialStore::connect_kubernetes(config)
            .await
            .expect("Kubernetes login succeeds eagerly");
        let token = OAuthToken {
            access: "customer-access".into(),
            refresh: Some("customer-refresh".into()),
            expires_at_ms: None,
            account_id: Some("acme".into()),
        };
        store.save("plugin:acme:api", &token).await.unwrap();
        assert_eq!(
            store.load("plugin:acme:api").await.unwrap().access,
            "customer-access"
        );
        server.await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn vault_kubernetes_auth_renews_an_expiring_lease() {
        let dir = std::env::temp_dir().join(format!(
            "flux-vault-k8s-renew-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let jwt_path = dir.join("token");
        std::fs::write(&jwt_path, "projected-jwt").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for step in 0..3 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let req = read_http_request(&mut sock).await;
                match step {
                    0 => write_http_json(
                        &mut sock,
                        200,
                        r#"{"auth":{"client_token":"vault-short","lease_duration":0,"renewable":true}}"#,
                    )
                    .await,
                    1 => {
                        assert!(req.starts_with("POST /v1/auth/token/renew-self "));
                        assert!(req.to_ascii_lowercase().contains("x-vault-token: vault-short"));
                        write_http_json(
                            &mut sock,
                            200,
                            r#"{"auth":{"client_token":"vault-renewed","lease_duration":3600,"renewable":true}}"#,
                        )
                        .await;
                    }
                    _ => {
                        assert!(req.starts_with("POST /v1/secret/data/flux/plugin/acme/api "));
                        assert!(req
                            .to_ascii_lowercase()
                            .contains("x-vault-token: vault-renewed"));
                        write_http_json(&mut sock, 200, "{}").await;
                    }
                }
            }
        });

        let config = VaultKubernetesConfig::new(format!("http://{addr}"), "role")
            .with_service_account_token_path(&jwt_path);
        let store = VaultCredentialStore::connect_kubernetes(config)
            .await
            .unwrap();
        store
            .save(
                "plugin:acme:api",
                &OAuthToken {
                    access: "value".into(),
                    refresh: None,
                    expires_at_ms: None,
                    account_id: None,
                },
            )
            .await
            .unwrap();
        server.await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn vault_kubernetes_auth_reloads_rotated_jwt_after_forbidden() {
        let dir = std::env::temp_dir().join(format!(
            "flux-vault-k8s-reauth-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let jwt_path = dir.join("token");
        std::fs::write(&jwt_path, "projected-jwt-one").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let rotated_path = jwt_path.clone();
        let server = tokio::spawn(async move {
            for step in 0..4 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let req = read_http_request(&mut sock).await;
                match step {
                    0 => write_http_json(
                        &mut sock,
                        200,
                        r#"{"auth":{"client_token":"vault-one","lease_duration":3600,"renewable":true}}"#,
                    )
                    .await,
                    1 => {
                        assert!(req.starts_with("GET /v1/secret/data/flux/plugin/acme/api "));
                        assert!(req.to_ascii_lowercase().contains("x-vault-token: vault-one"));
                        std::fs::write(&rotated_path, "projected-jwt-two").unwrap();
                        write_http_json(&mut sock, 403, r#"{"errors":["expired"]}"#).await;
                    }
                    2 => {
                        assert!(req.starts_with("POST /v1/auth/kubernetes/login "));
                        assert!(req.contains(r#""jwt":"projected-jwt-two""#));
                        write_http_json(
                            &mut sock,
                            200,
                            r#"{"auth":{"client_token":"vault-two","lease_duration":3600,"renewable":true}}"#,
                        )
                        .await;
                    }
                    _ => {
                        assert!(req.starts_with("GET /v1/secret/data/flux/plugin/acme/api "));
                        assert!(req.to_ascii_lowercase().contains("x-vault-token: vault-two"));
                        let stored = serde_json::to_string(&OAuthToken {
                            access: "rotated-access".into(),
                            refresh: None,
                            expires_at_ms: None,
                            account_id: None,
                        })
                        .unwrap();
                        let body = serde_json::json!({
                            "data": { "data": { "token": stored } }
                        })
                        .to_string();
                        write_http_json(&mut sock, 200, &body).await;
                    }
                }
            }
        });

        let config = VaultKubernetesConfig::new(format!("http://{addr}"), "role")
            .with_service_account_token_path(&jwt_path);
        let store = VaultCredentialStore::connect_kubernetes(config)
            .await
            .unwrap();
        assert_eq!(
            store.load("plugin:acme:api").await.unwrap().access,
            "rotated-access"
        );
        server.await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pricing_toml_overrides_builtin() {
        // Hermetic: write a fixture pricing.toml in a unique temp dir and fold it directly (no env
        // mutation, so it can't race the HOME-dependent store tests).
        let dir =
            std::env::temp_dir().join(format!("flux-pricing-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pricing.toml");
        std::fs::write(
            &path,
            r#"
[models."claude-opus-4-8"]
input = 1.0
"#,
        )
        .unwrap();

        let builtin = PricingTable::builtin();
        let mut table = builtin.clone();
        apply_pricing_file(&mut table, &path);

        // The overridden model's input rate changed...
        let opus = *table.rates_for("claude-opus-4-8").unwrap();
        assert_eq!(opus.input, 1.0);
        // ...while its other tiers keep the built-in values...
        let builtin_opus = *builtin.rates_for("claude-opus-4-8").unwrap();
        assert_eq!(opus.output, builtin_opus.output);
        assert_eq!(opus.cache_read, builtin_opus.cache_read);
        // ...and an untouched model keeps every built-in rate.
        assert_eq!(
            table.rates_for("claude-sonnet-4-6"),
            builtin.rates_for("claude-sonnet-4-6"),
        );

        // A missing file is a no-op (built-ins stand).
        let mut t2 = PricingTable::builtin();
        apply_pricing_file(&mut t2, &dir.join("does-not-exist.toml"));
        assert_eq!(t2, builtin);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn token_resp_surfaces_error() {
        let err = TokenResp {
            access_token: String::new(),
            refresh_token: None,
            expires_in: None,
            id_token: None,
            error: Some("invalid_grant".into()),
            error_description: Some("bad".into()),
        }
        .into_refreshed();
        assert!(err.is_err());
    }

    // --- forced refresh (C-04) -------------------------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A [`Refresher`] that hands out `fresh-<n>` on each call and counts how often it ran.
    struct CountingRefresher {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Refresher for CountingRefresher {
        async fn refresh(&self, _refresh_token: &str) -> Result<Refreshed> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Refreshed {
                access: format!("fresh-{n}"),
                refresh: Some("rotated".into()),
                expires_at_ms: Some(now_ms() + 3_600_000),
                id_token: None,
            })
        }
    }

    #[tokio::test]
    // HOME must stay repointed across the `refresh().await` calls (save_stored reads it there), and
    // #[tokio::test] runs on a current-thread runtime, so holding the std guard across await is safe.
    #[allow(clippy::await_holding_lock)]
    async fn force_refresh_ignores_expiry_buffer_and_coalesces() {
        // Redirect HOME so the best-effort `save_stored` writes to a throwaway dir, never the real
        // credential store. (set_var("HOME", ..) is the established test pattern in this workspace.)
        let tmp = std::env::temp_dir().join(format!(
            "flux-cred-c04-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let _home = HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &tmp);

        let calls = Arc::new(AtomicUsize::new(0));
        // A token comfortably WITHIN its validity window: the lazy access path must not refresh it.
        let tok = OAuthToken {
            access: "stale".into(),
            refresh: Some("rt".into()),
            expires_at_ms: Some(now_ms() + 3_600_000),
            account_id: None,
        };
        let rt = RefreshingToken::new(
            "claude-c04-test",
            tok,
            Box::new(CountingRefresher {
                calls: calls.clone(),
            }),
        );

        // Lazy path: not near expiry → returns the existing token without refreshing.
        assert_eq!(rt.access_token().await.unwrap(), "stale");
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // Forced refresh ignores the buffer and swaps in a fresh token.
        rt.refresh().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(rt.access_token().await.unwrap(), "fresh-0");

        // A burst (second forced refresh within the dedup window) coalesces — no extra grant spend.
        rt.refresh().await.unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "concurrent burst of 401s coalesces into a single refresh"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn force_refresh_without_refresh_token_errors() {
        struct NeverRefresher;
        #[async_trait]
        impl Refresher for NeverRefresher {
            async fn refresh(&self, _refresh_token: &str) -> Result<Refreshed> {
                panic!("refresh must not be attempted without a refresh token");
            }
        }
        let tok = OAuthToken {
            access: "x".into(),
            refresh: None,
            expires_at_ms: None,
            account_id: None,
        };
        let rt = RefreshingToken::new("p", tok, Box::new(NeverRefresher));
        assert!(
            rt.refresh().await.is_err(),
            "a forced refresh with no refresh token is an error"
        );
    }
}
