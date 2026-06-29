//! `flux-credentials` — authenticates flux *to the LLM providers* (distinct from `flux-auth`,
//! which authenticates callers *to flux*).
//!
//! Provides OAuth token sources for the subscription providers (`claude`, `codex`): a refreshing
//! [`TokenSource`] backed by a 0600 token store, with import from the official CLIs'
//! credential files (`~/.claude/.credentials.json`, `~/.codex/auth.json`) as the primary
//! acquisition path and PKCE login (`flux auth login claude`) as the alternative.
//!
//! Constants and flows mirror the user's Go implementations (`coder/internal/oauth`,
//! `llm/provider/codex/auth.go`).

use std::collections::HashMap;
use std::sync::Arc;

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

// --- Codex OAuth constants (← llm/provider/codex/auth.go) ------------------
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

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

/// Persist one provider's token to the store, creating `~/.flux` and forcing 0600. Writes
/// atomically (temp file created 0600 + rename) so there is no world-readable window and a crash
/// mid-write can't truncate the existing credentials.
fn save_stored(provider: &str, token: &OAuthToken) -> Result<()> {
    let path = store_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Propagates a corrupt-store error rather than silently dropping the other providers' tokens.
    let mut store = load_store()?;
    store.entries.insert(provider.to_string(), token.clone());
    let body = toml::to_string_pretty(&store)
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
struct Refreshed {
    access: String,
    refresh: Option<String>,
    expires_at_ms: Option<i64>,
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
        parse_token_resp(resp).await
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
        parse_token_resp(resp).await
    }
}

async fn parse_token_resp(resp: reqwest::Response) -> Result<Refreshed> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| Error::Http(e.to_string()))?;
    let parsed: TokenResp = serde_json::from_str(&body)
        .map_err(|e| Error::Auth(format!("decode refresh response (status {status}): {e}")))?;
    parsed.into_refreshed()
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
            "no Codex subscription credentials — log into the Codex CLI (`~/.codex/auth.json`)"
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
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    let verifier = b64().encode(buf);
    let challenge = b64().encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

/// Random URL-safe state value.
pub fn generate_state() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    b64().encode(buf)
}

/// Build the Anthropic authorization URL the user visits to approve flux.
pub fn anthropic_authorize_url(pkce: &Pkce, state: &str) -> String {
    let q = [
        ("code", "true"),
        ("client_id", ANTHROPIC_CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", ANTHROPIC_REDIRECT_URI),
        ("scope", ANTHROPIC_SCOPE),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    let qs = q
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{ANTHROPIC_AUTHORIZE_URL}?{qs}")
}

/// Exchange an authorization code (the user pastes the callback value) for tokens and persist
/// them under the `claude` provider.
pub async fn anthropic_exchange_and_store(code: &str, state: &str, verifier: &str) -> Result<()> {
    // The callback value is pasted as `code#state`. When a state is present it MUST match the one
    // we generated for this login — otherwise the user may have pasted an attacker-supplied code
    // (OAuth login-CSRF / account injection). PKCE is the primary defense; this is the binding.
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
    let refreshed = parse_token_resp(resp).await?;
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
    pub source: String,
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
                format!("${var}")
            } else {
                "not set".into()
            },
        }
    };
    let oauth_status = |provider: &'static str, stored_key: &str, imported: Option<OAuthToken>| {
        if load_stored(stored_key).is_some() {
            ProviderAuth {
                provider,
                available: true,
                source: "flux store".into(),
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
            }
        } else {
            ProviderAuth {
                provider,
                available: false,
                source: "not found".into(),
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

    #[test]
    fn jwt_expiry_decodes_exp() {
        // header.{"exp":2000000000}.sig  (exp = 2033-05-18)
        let payload = b64().encode(br#"{"exp":2000000000}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(jwt_expiry_ms(&token), Some(2_000_000_000 * 1000));
        assert_eq!(jwt_expiry_ms("not-a-jwt"), None);
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
            })
        }
    }

    #[tokio::test]
    async fn force_refresh_ignores_expiry_buffer_and_coalesces() {
        // Redirect HOME so the best-effort `save_stored` writes to a throwaway dir, never the real
        // credential store. (set_var("HOME", ..) is the established test pattern in this workspace.)
        let tmp = std::env::temp_dir().join(format!(
            "flux-cred-c04-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
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
