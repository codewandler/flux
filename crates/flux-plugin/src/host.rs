//! Host-side capability enforcement and its security regression tests.

use super::handshake;
use super::*;

/// The privileged operations a plugin may request of the host during an operation call. Every
/// capability is policy-relevant IO the plugin cannot do itself; the host services it through the
/// guarded [`System`](flux_system::System) and returns a result frame.
#[async_trait]
pub trait HostCapabilities: Send + Sync {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String>;
}

/// Resolves endpoint/credential **references** to their runtime form — host-side only. This is the
/// seam the L5 endpoint broker implements; [`SystemHostCaps`] consults it (when present, see
/// [`SystemHostCaps::with_resolver`]) so a plugin op can pass an `endpoint_ref` instead of a URL,
/// and the host alone turns it into a connection + injected credentials. The plugin and the model
/// never see a resolved URL-with-credentials.
///
/// A *reference* is either a **named** config/manifest endpoint (`"sql.endpoint"`) or a
/// **discovered** `@endpoint/<id>`; the resolver handles both.
#[async_trait]
pub trait ReferenceResolver: Send + Sync {
    /// Resolve an endpoint reference to its runtime form (absolute URL + any injected auth headers).
    /// Host-only — the result has no model-visible serializer.
    ///
    /// This is the consumer-agnostic form. On the IO path prefer
    /// [`resolve_endpoint_for`](Self::resolve_endpoint_for): when a discovered endpoint's credential is
    /// owned by a *different* plugin, injecting it into the request on the caller's behalf is exactly a
    /// cross-plugin credential *use*, which must be gated against the consuming plugin.
    async fn resolve_endpoint(
        &self,
        reference: &str,
    ) -> std::result::Result<flux_secret::endpoint::ResolvedEndpoint, String>;

    /// Resolve an endpoint reference **on behalf of `consumer`** (the plugin doing the IO). Identical
    /// to [`resolve_endpoint`](Self::resolve_endpoint), except a discovered endpoint's `credential_ref`
    /// is materialized as `consumer` — so when that credential is owned by another plugin, the
    /// deny-by-default cross-plugin gate (grant + first-use approval + audit) fires before the host
    /// injects it. The default ignores the consumer and delegates to `resolve_endpoint`; the L5 broker
    /// overrides it.
    async fn resolve_endpoint_for(
        &self,
        _consumer: &str,
        reference: &str,
    ) -> std::result::Result<flux_secret::endpoint::ResolvedEndpoint, String> {
        self.resolve_endpoint(reference).await
    }

    /// Materialize a credential reference to secret material — for raw-socket in-band-auth protocols
    /// (e.g. Postgres SCRAM) that must speak the handshake themselves. Host-side; the value is
    /// delivered only to the trusted plugin binary, never surfaced to the model.
    ///
    /// This is the consumer-agnostic form (no cross-plugin gate). Prefer
    /// [`resolve_credential_for`](Self::resolve_credential_for) on the IO path so the broker can
    /// enforce the deny-by-default cross-plugin grant against the *consuming* plugin.
    async fn resolve_credential(
        &self,
        reference: &flux_secret::Ref,
    ) -> std::result::Result<flux_secret::Material, String>;

    /// Materialize a credential reference **on behalf of `consumer`** (the plugin requesting it). When
    /// the credential is owned by a *different* plugin (a cross-plugin `Kubernetes`/`Plugin` scheme
    /// ref), the resolver gates the resolution against the operator's cross-plugin grant for the
    /// `(consumer, provider)` pair, an optional first-use approval, and an audit record. The default
    /// implementation ignores the consumer and delegates to [`resolve_credential`](Self::resolve_credential)
    /// — overridden by the L5 broker, which alone knows the provider graph and the grants.
    async fn resolve_credential_for(
        &self,
        _consumer: &str,
        reference: &flux_secret::Ref,
    ) -> std::result::Result<flux_secret::Material, String> {
        self.resolve_credential(reference).await
    }

    /// The credential *reference* (a location, never a value) attached to an endpoint reference, for
    /// the `credential`-by-`endpoint_ref` path. The default has no endpoint registry and errors;
    /// the L5 broker overrides it (looking the record up in the [`EndpointRegistry`]).
    async fn credential_ref_for_endpoint(
        &self,
        reference: &str,
    ) -> std::result::Result<flux_secret::Ref, String> {
        Err(format!(
            "this resolver cannot map endpoint `{reference}` to a credential reference"
        ))
    }
}

/// Denies every host-capability callback (the default for `call`). A plugin that needs callbacks
/// must be driven via [`PluginHost::call_with_host`] with a real [`HostCapabilities`].
pub struct DenyHostCaps;

#[async_trait]
impl HostCapabilities for DenyHostCaps {
    async fn handle(&self, command: &str, _p: &Value) -> std::result::Result<Value, String> {
        Err(format!("host capability `{command}` is not available"))
    }
}

/// An audit seam: the host calls [`record_private_admit`](Self::record_private_admit) whenever it
/// admits an egress request to a **private/internal** address under a scoped grant — the auditable
/// security event. This crate (L4) only defines the trait; the concrete, `flux-events`-backed
/// implementation that appends a `PrivateNetAdmit` event lives at a surface (L6), so flux-plugin
/// stays free of an event-store dependency. A host with no audit installed simply admits silently.
pub trait EgressAudit: Send + Sync {
    /// Record that `caller` (a plugin name, or `"web.fetch"`) reached the private `host`, admitted by
    /// `grant_source` (e.g. `"config:plugin/<name>"` or `"config:endpoint/<plugin>:<ep>"`).
    fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str);
}

/// A sink for secret values the host materializes at runtime (the `credential` capability path).
/// Registering a value here ensures it is scrubbed from any model-visible output. The concrete
/// implementation lives at a surface (L6), backed by the executor's [`Redactor`](flux_secret::Redactor);
/// a host with no sink installed simply hands the value to the trusted plugin without registration.
pub trait SecretSink: Send + Sync {
    /// Register `value` as a known secret so it is redacted from captured tool output and logs.
    fn register_secret(&self, value: &str);
}

/// The guarded surface the plugin host needs, and no more (C-269).
///
/// `SystemHostCaps` uses exactly three of `flux-system`'s guarded families — argv-only process
/// execution, scope-admitted host-file reads, and env lookups for `secret:env/KEY` refs — so it names
/// those three rather than a catch-all "whole System" trait. The blanket impl means the native
/// [`System`](flux_system::System) satisfies it for free, and so does any substrate that serves the
/// same three ports (a Wasm embedder reaching host imports, a remote executor, a test double).
///
/// This is the bundle, not the seam: the operations themselves live in `flux_system::port`.
pub trait PluginSystem: GuardedProcess + GuardedHostFiles + GuardedEnv {}

impl<T: GuardedProcess + GuardedHostFiles + GuardedEnv + ?Sized> PluginSystem for T {}

/// Where [`SystemHostCaps`] resolves its guarded system from (C-122).
///
/// A workspace transition (`git_worktree_enter`) swaps the context's active system; host
/// capabilities that captured the system at assembly time kept executing `process.run` in
/// the original root after the rest of the session had moved. This seam makes the resolution
/// dynamic: each `handle()` call snapshots the source once — the same snapshot-per-operation
/// discipline as `ToolContext::system` — so a transition is observed by the *next* op, never
/// mid-call. Surfaces with no transitions (e.g. one-shot `flux plugin call`) use a
/// [`FixedSystem`]; session surfaces bind an adapter over the context's workspace handle (the
/// adapter lives at the surface — this crate stays free of a runtime dependency).
///
/// The two axes are deliberately separate and both needed: this trait answers **which** guarded system
/// is active right now, [`PluginSystem`] answers **what** a guarded system is. Before C-269 the second
/// question had no answer — the snapshot was a concrete `Arc<System>`, so no non-native backend could
/// be plugged in behind it.
pub trait SystemSource: Send + Sync {
    /// Snapshot the currently active guarded system.
    fn system(&self) -> Arc<dyn PluginSystem>;
}

/// A [`SystemSource`] pinned to one system forever — the non-transitioning surfaces' source, and
/// exactly the pre-C-122 capture semantics.
pub struct FixedSystem(pub Arc<dyn PluginSystem>);

impl SystemSource for FixedSystem {
    fn system(&self) -> Arc<dyn PluginSystem> {
        self.0.clone()
    }
}

/// Host capabilities backed by the guarded [`System`](flux_system::System): `process.run` (argv
/// only), `http.do` (GET, loopback/private blocked unless allowed), and `secret` (env refs). This
/// is the bridge that keeps plugin IO inside the same safety boundary as the agent's own tools.
///
/// Every callback is additionally gated by the per-plugin [`PluginCapabilities`] grants (built from
/// the plugin's manifest): `process.run` only for allow-listed programs, `secret` only for
/// allow-listed keys, `http.do` only if the plugin declared it. A fresh `SystemHostCaps` grants
/// nothing — call [`with_grants`](Self::with_grants).
pub struct SystemHostCaps {
    /// The guarded-system resolution seam (C-122): snapshotted once per `handle()` call, so plugin
    /// `process.run`/`process.spawn` execute in the context's *active* root — a worktree transition
    /// is observed by the next op. [`FixedSystem`] for non-transitioning surfaces.
    system: Arc<dyn SystemSource>,
    /// Resolver used by the egress guard. The exact answer is consumed by the HTTP/TCP connection,
    /// not discarded before a second connect-time lookup.
    host_resolver: Arc<dyn flux_system::net::HostResolver>,
    private_net_grants: Vec<String>,
    grants: PluginCapabilities,
    auth: Vec<AuthMethod>,
    endpoints: Vec<EndpointSpec>,
    /// Declared non-secret config values (D-32): the `config` capability's resolution source and
    /// the substitution source for [`EndpointSpec::template`] placeholders.
    configs: Vec<ConfigSpec>,
    /// The caller name recorded in egress-admit audit events (the plugin's manifest name, set by
    /// [`with_manifest`](Self::with_manifest)). Defaults to `"plugin"` until a manifest is pinned.
    caller: String,
    /// How this plugin's private-net grants were sourced, recorded in audit events (defaults to a
    /// generic plugin-scope label; the surface can override via [`with_grant_source`](Self::with_grant_source)).
    grant_source: String,
    /// Optional egress-audit hook: fires when a private host is admitted under a scoped grant.
    audit: Option<Arc<dyn EgressAudit>>,
    /// Optional reference resolver (the L5 endpoint broker, injected as a trait object). When present,
    /// a plugin op may pass an `endpoint_ref` to `http.do`/`conn.dial` instead of a URL/host:port, and
    /// the host alone turns it into a connection + injected credentials — the plugin and the model
    /// never see a resolved URL-with-credentials. Also backs the gated `credential` capability.
    ///
    /// LIFETIME: the resolver is the broker, which holds the `PluginRegistry`, whose entries' caps
    /// transitively hold *this* `SystemHostCaps` → a strong `Arc` cycle. This is intentional and kept
    /// simple: the broker/registry/caps form a **session-lived** object graph torn down at process
    /// exit. It is not a per-request leak (the graph is built once at startup), so a strong `Arc` is
    /// fine; engineering a `Weak` back-edge here would add complexity for no practical benefit.
    resolver: Option<Arc<dyn ReferenceResolver>>,
    /// The consumer plugin's name passed to the resolver on the cross-plugin credential path, so the
    /// broker can gate a `(consumer, provider)` resolution. Defaults to [`caller`](Self::caller).
    consumer: String,
    /// Optional sink for credentials materialized on the `credential` capability path: the host hands
    /// the raw value to the trusted plugin binary, and registers it here so it is scrubbed from any
    /// model-visible output. Backed at the surface by the same [`Redactor`](flux_secret::Redactor) the
    /// executor redacts with.
    secret_sink: Option<Arc<dyn SecretSink>>,
    /// Optional injected credential store for OAuth2 auth methods (plugin-oauth, D-83). `None` uses
    /// the default file backend (`~/.flux/credentials.toml`); a host app can inject a Vault-backed
    /// store the same way it injects a resolver / secret sink, so per-customer tokens live in Vault.
    cred_store: Option<Arc<dyn flux_credentials::CredentialStore>>,
    /// Open `conn.dial` connections for this call scope, keyed by an opaque id. A tokio mutex so a
    /// `conn.read`/`write` can hold the stream across its await without making the guard non-Send.
    conns: tokio::sync::Mutex<std::collections::HashMap<u64, flux_system::net::DialStream>>,
    next_conn: std::sync::atomic::AtomicU64,
    /// `blob.*` content-addressed scratch store for this call scope: `sha256-hex -> (name, bytes)`.
    blobs: tokio::sync::Mutex<std::collections::HashMap<String, (String, Vec<u8>)>>,
    /// Host-managed background processes (`process.spawn`/`read`/`status`/`kill`), keyed by an opaque
    /// id. Persists across op calls (one `SystemHostCaps` is shared for a plugin's whole session), so
    /// a `kubectl port-forward` started in one call is stopped in a later one. A tokio mutex so a
    /// handler can hold the map across the `try_wait`/drain (neither awaits, but the guard stays Send).
    procs: tokio::sync::Mutex<std::collections::HashMap<u64, ManagedProc>>,
    next_proc: std::sync::atomic::AtomicU64,
}

/// A long-lived host-managed process registered in [`SystemHostCaps::procs`].
type ManagedProc = flux_system::ManagedChild;

impl SystemHostCaps {
    /// Capabilities over a fixed system — the pre-C-122 semantics, right for surfaces where no
    /// workspace transition can happen (one-shot `flux plugin call`, tests).
    pub fn new(system: Arc<dyn PluginSystem>) -> Self {
        Self::from_source(Arc::new(FixedSystem(system)))
    }

    /// Snapshot the active guarded system for one operation (C-122). Every handler resolves
    /// through here, so a workspace transition is observed by the next op, never mid-call.
    fn system(&self) -> Arc<dyn PluginSystem> {
        self.system.system()
    }

    /// Capabilities over a dynamic [`SystemSource`] (C-122): session surfaces pass an adapter over
    /// the context's workspace handle so plugin ops follow a worktree transition.
    pub fn from_source(system: Arc<dyn SystemSource>) -> Self {
        Self {
            system,
            host_resolver: Arc::new(flux_system::net::SystemHostResolver),
            private_net_grants: Vec::new(),
            grants: PluginCapabilities::default(),
            auth: Vec::new(),
            endpoints: Vec::new(),
            configs: Vec::new(),
            caller: "plugin".to_string(),
            grant_source: "config:plugin".to_string(),
            audit: None,
            resolver: None,
            consumer: "plugin".to_string(),
            secret_sink: None,
            cred_store: None,
            conns: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            next_conn: std::sync::atomic::AtomicU64::new(1),
            blobs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            procs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            next_proc: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn allow_private_net(mut self, yes: bool) -> Self {
        self.private_net_grants = if yes {
            vec!["*".to_string()]
        } else {
            Vec::new()
        };
        self
    }

    /// Inject the DNS resolver used for both authorization and the pinned connection.
    ///
    /// Production uses the system resolver. Tests use this seam to model a hostname whose first
    /// answer is admissible and whose hypothetical connect-time answer would be private.
    pub fn with_host_resolver(mut self, resolver: Arc<dyn flux_system::net::HostResolver>) -> Self {
        self.host_resolver = resolver;
        self
    }

    /// Operator grants for private-network egress for this plugin. These are intersected with the
    /// plugin's manifest-declared `private_hosts`.
    pub fn with_private_net_grants(mut self, hosts: Vec<String>) -> Self {
        self.private_net_grants = hosts;
        self
    }

    /// Restrict this host's callbacks to the capabilities the plugin declared in its manifest.
    pub fn with_grants(mut self, grants: PluginCapabilities) -> Self {
        self.grants = grants;
        self
    }

    /// Inject a custom credential store (e.g. a Vault backend) for OAuth2 auth-method resolution
    /// (plugin-oauth, D-83). Without one, the default file backend (`~/.flux/credentials.toml`) is
    /// used, so provider logins and the CLI keep working unchanged.
    pub fn with_credential_store(
        mut self,
        store: Arc<dyn flux_credentials::CredentialStore>,
    ) -> Self {
        self.cred_store = Some(store);
        self
    }

    /// Pin this host to a plugin's whole manifest: its capability grants, auth methods (for
    /// secret-by-purpose resolution), and endpoints. The one-call setup for [`load_plugin_tools`].
    pub fn with_manifest(mut self, m: &PluginManifest) -> Self {
        self.grants = m.capabilities.clone();
        self.auth = m.auth.clone();
        self.endpoints = m.endpoints.clone();
        self.configs = m.config.clone();
        if !m.name.is_empty() {
            self.caller = m.name.clone();
            self.grant_source = format!("config:plugin/{}", m.name);
            self.consumer = m.name.clone();
        }
        self
    }

    /// Install an [`EgressAudit`] hook. When set, the host records a private-network-admit event the
    /// moment it lets a request to a private/internal host through under a scoped grant.
    pub fn with_egress_audit(mut self, audit: Arc<dyn EgressAudit>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Inject the [`ReferenceResolver`] (the L5 endpoint broker). With a resolver installed, a plugin
    /// op may pass an `endpoint_ref` (to `http.do`/`conn.dial`) and use the gated `credential`
    /// capability; without one, those paths return a clear "no resolver" error and the legacy
    /// URL-based paths are unaffected. See the field doc for the (intentional) session-lived Arc cycle.
    pub fn with_resolver(mut self, resolver: Arc<dyn ReferenceResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Install a [`SecretSink`] so credentials materialized on the `credential` capability path are
    /// registered with the executor's redactor (scrubbed from any model-visible output).
    pub fn with_secret_sink(mut self, sink: Arc<dyn SecretSink>) -> Self {
        self.secret_sink = Some(sink);
        self
    }

    /// Override the `grant_source` label recorded in egress-admit audit events (e.g. an
    /// endpoint-scoped `"config:endpoint/<plugin>:<ep>"` when grants were resolved per endpoint).
    pub fn with_grant_source(mut self, grant_source: impl Into<String>) -> Self {
        self.grant_source = grant_source.into();
        self
    }

    /// Fire the egress-audit hook (if installed) when `host` is a private/internal address — i.e. the
    /// scoped grant just admitted a request the bare SSRF guard would have refused.
    fn audit_admit(&self, host: &str, pinned: &[std::net::SocketAddr]) {
        if let Some(audit) = &self.audit {
            if flux_system::net::destination_is_private(host, pinned) {
                audit.record_private_admit(&self.caller, host, &self.grant_source);
            }
        }
    }

    /// Resolve a secret **by purpose**. An OAuth2-backed method (plugin-oauth, D-81) resolves a fresh
    /// bearer from the credential store — refreshing via the declared `token_path` when stale — and
    /// falls back to the declared env keys until a login stores tokens. A plain method consults the
    /// same store first (`flux auth set <plugin> <purpose>`, D-126 — the configure-in-advance path
    /// for a session whose environment can't carry the secret), then tries its declared env keys in
    /// order (each must also be a granted `secret`), returning the first value set.
    async fn resolve_purpose(&self, purpose: &str) -> std::result::Result<String, String> {
        let method = self
            .auth
            .iter()
            .find(|a| a.purpose == purpose)
            .ok_or_else(|| format!("no auth method declared for purpose `{purpose}`"))?;
        // OAuth2-backed: the HOST runs the grant/refresh and hands back only a fresh bearer. The token
        // endpoint is built from the method's DECLARED endpoint (never a plugin-supplied URL) and
        // still passes the SSRF guard + the manifest host allow-list, so it can't be pointed at an
        // internal address. A resolved bearer is registered with the redactor.
        if let Some(oauth) = &method.oauth2 {
            let base = self.resolve_endpoint(&oauth.endpoint)?;
            let token_url = format!(
                "{}/{}",
                base.trim_end_matches('/'),
                oauth.token_path.trim_start_matches('/')
            );
            // Validate the declared endpoint's syntax and manifest host grant before consulting
            // the credential store. DNS is deliberately later: absent/fresh tokens perform no
            // network IO, so they must not depend on resolver availability.
            let url = url::Url::parse(&token_url)
                .map_err(|error| format!("oauth token endpoint is invalid: {error}"))?;
            self.ensure_http_host_allowed(&url)?;
            let key = format!("plugin:{}:{}", self.caller, purpose);
            // Use the injected store (D-83) or fall back to the default file backend.
            let file_store = flux_credentials::FileCredentialStore;
            let store: &dyn flux_credentials::CredentialStore =
                self.cred_store.as_deref().unwrap_or(&file_store);
            match flux_credentials::resolve_stored_bearer_with_client_factory(
                store,
                &key,
                url.as_str(),
                &oauth.client_id,
                || {
                    // The credential helper invokes this factory only for a stale stored token
                    // carrying refresh material. Resolve, guard, pin, and audit at that moment so
                    // authorization and connect consume the same DNS answer without making the
                    // offline absent/fresh paths resolve at all.
                    let (guarded_url, pinned) = guard_http_url_pinned(
                        &token_url,
                        &self.private_net_allow(),
                        self.host_resolver.as_ref(),
                    )
                    .map_err(flux_core::Error::Http)?;
                    let client = pinned_http_client(&guarded_url, &pinned, "oauth refresh")
                        .map_err(flux_core::Error::Http)?;
                    if let Some(host) = guarded_url.host_str() {
                        self.audit_admit(host, &pinned);
                    }
                    Ok(client)
                },
            )
            .await
            {
                Ok(Some(bearer)) => {
                    if let Some(sink) = &self.secret_sink {
                        sink.register_secret(&bearer);
                    }
                    return Ok(bearer);
                }
                // No token stored yet (pre-login) — fall through to the env fallback below.
                Ok(None) => {}
                Err(e) => return Err(format!("oauth resolve for purpose `{purpose}`: {e}")),
            }
        }
        // Plain (non-OAuth2) method: a stored bearer (`flux auth set <plugin> <purpose>`, D-126)
        // wins, matching the OAuth2 store-first rule above; the declared env keys are the fallback.
        if method.oauth2.is_none() {
            let key = format!("plugin:{}:{}", self.caller, purpose);
            let file_store = flux_credentials::FileCredentialStore;
            let store: &dyn flux_credentials::CredentialStore =
                self.cred_store.as_deref().unwrap_or(&file_store);
            if let Some(tok) = store.load(&key).await {
                if let Some(sink) = &self.secret_sink {
                    sink.register_secret(&tok.access);
                }
                return Ok(tok.access);
            }
        }
        for key in &method.env {
            if !self.grants.secrets.iter().any(|k| k == key) {
                continue; // not a granted secret — skip
            }
            if let Some(v) = self.system().env(key) {
                if let Some(sink) = &self.secret_sink {
                    sink.register_secret(&v);
                }
                return Ok(v);
            }
        }
        Err(format!(
            "no credential for purpose `{purpose}` — set a declared env key (tried {:?}) or store \
             one with `flux auth set {} {purpose}`",
            method.env, self.caller
        ))
    }

    /// Resolve a credential for a **host-terminated handshake** (D-31) by declared auth-method
    /// purpose — the static/named path of `conn.authenticate`. Reads the auth method's declared env
    /// keys directly host-side, the same way [`resolve_user`](Self::resolve_user) /
    /// [`resolve_endpoint`](Self::resolve_endpoint) read declared env. Deliberately NOT gated by the
    /// plugin-facing `secrets` grant: the plugin can no longer read this key via the `secret`
    /// capability (its grant is removed), but the host resolves it for the declared handshake and
    /// never returns the value to the plugin.
    fn resolve_handshake_secret(&self, purpose: &str) -> std::result::Result<String, String> {
        let method = self
            .auth
            .iter()
            .find(|a| a.purpose == purpose)
            .ok_or_else(|| format!("no auth method declared for handshake purpose `{purpose}`"))?;
        for key in &method.env {
            if let Some(v) = self.system().env(key) {
                return Ok(v);
            }
        }
        Err(format!(
            "no env value for handshake purpose `{purpose}` (tried {:?})",
            method.env
        ))
    }

    /// Resolve a named endpoint base URL — HOST-SIDE ONLY, feeding the ref-based IO paths (there
    /// is no capability handing this URL back to the plugin, D-32). A [`template`]
    /// (`EndpointSpec::template`) composes from declared config values; otherwise the declared env
    /// keys are tried in order.
    fn resolve_endpoint(&self, name: &str) -> std::result::Result<String, String> {
        let ep = self
            .endpoints
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("no endpoint declared named `{name}`"))?;
        if let Some(template) = &ep.template {
            return self.expand_endpoint_template(template);
        }
        for key in &ep.env {
            if let Some(v) = self.system().env(key) {
                return Ok(v);
            }
        }
        if let Some(default) = &ep.default {
            return Ok(default.clone());
        }
        Err(format!(
            "no env value for endpoint `{name}` (tried {:?})",
            ep.env
        ))
    }

    /// Resolve a declared **non-secret** config value by name (the gated `config` capability,
    /// D-32). Deny-by-default: only names declared in the manifest's `config` resolve — and a
    /// declared env key that is secret-classified (a granted `secrets` entry or an auth method's
    /// secret env) is refused outright, so this path can never return a secret value.
    fn resolve_config(&self, name: &str) -> std::result::Result<String, String> {
        let spec = self
            .configs
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| format!("no config declared named `{name}`"))?;
        for key in &spec.env {
            if self.grants.secrets.iter().any(|k| k == key)
                || self.auth.iter().any(|a| a.env.iter().any(|k| k == key))
            {
                return Err(format!(
                    "config `{name}`: env key `{key}` is secret-classified; the config capability never returns secrets"
                ));
            }
        }
        for key in &spec.env {
            if let Some(v) = self.system().env(key) {
                // A value that is itself a credential-bearing URL (a DSN with an embedded
                // password) is refused: the config capability can never hand the plugin a
                // secret, even via an operator-misconfigured env value. Move the password to
                // its own (secret-declared) env key.
                if url::Url::parse(&v)
                    .map(|u| u.password().is_some_and(|p| !p.is_empty()))
                    .unwrap_or(false)
                {
                    return Err(format!(
                        "config `{name}`: the value of `{key}` embeds a credential (a URL with a \
                         password); the config capability never returns secrets — move the \
                         password to a secret-declared env key"
                    ));
                }
                return Ok(v);
            }
        }
        Err(format!(
            "no env value for config `{name}` (tried {:?})",
            spec.env
        ))
    }

    /// Expand an [`EndpointSpec::template`]: each `{name}` placeholder substitutes the declared
    /// config value `name` (percent-encoded), resolved via [`resolve_config`](Self::resolve_config)
    /// — so a secret-classified value can never be smuggled into a composed URL.
    fn expand_endpoint_template(&self, template: &str) -> std::result::Result<String, String> {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let close = after
                .find('}')
                .ok_or_else(|| format!("endpoint template has an unclosed `{{`: `{template}`"))?;
            let value = self.resolve_config(&after[..close])?;
            out.push_str(&percent_encode_component(&value));
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn private_net_allow(&self) -> PrivateNetAllow {
        let declared = normalize_patterns(&self.grants.private_hosts);
        let grants = normalize_patterns(&self.private_net_grants);
        if declared.is_empty() || grants.is_empty() {
            return PrivateNetAllow::None;
        }
        if grants.iter().any(|g| g == "*") {
            return PrivateNetAllow::from_hosts(declared);
        }
        if declared.iter().any(|d| d == "*") {
            return PrivateNetAllow::from_hosts(grants);
        }
        PrivateNetAllow::from_hosts(
            grants
                .into_iter()
                .filter(|grant| host_matches(&declared, grant))
                .collect::<Vec<_>>(),
        )
    }

    fn ensure_http_host_allowed(&self, url: &url::Url) -> std::result::Result<(), String> {
        let host = url
            .host_str()
            .ok_or_else(|| "http.do: url has no host".to_string())?;
        if host_matches(&self.grants.http_hosts, host) || self.endpoint_allows_host(host) {
            Ok(())
        } else {
            Err(format!(
                "http.do: host `{host}` not in this plugin's declared HTTP capabilities"
            ))
        }
    }

    fn endpoint_allows_host(&self, host: &str) -> bool {
        self.endpoints.iter().any(|ep| {
            host_matches(&ep.http_hosts, host)
                || ep
                    .template
                    .as_ref()
                    .and_then(|t| self.expand_endpoint_template(t).ok())
                    .and_then(|raw| url::Url::parse(&raw).ok())
                    .and_then(|url| url.host_str().map(|h| h.eq_ignore_ascii_case(host)))
                    .unwrap_or(false)
                || ep
                    .default
                    .as_ref()
                    .and_then(|raw| url::Url::parse(raw).ok())
                    .and_then(|url| url.host_str().map(|h| h.eq_ignore_ascii_case(host)))
                    .unwrap_or(false)
                || ep.env.iter().any(|key| {
                    self.system()
                        .env(key)
                        .and_then(|raw| url::Url::parse(&raw).ok())
                        .and_then(|url| url.host_str().map(|h| h.eq_ignore_ascii_case(host)))
                        .unwrap_or(false)
                })
        })
    }

    /// Resolve the username half of Basic auth from a method's `user_env` (config, not a gated secret —
    /// resolved directly from declared env, like an endpoint).
    fn resolve_user(&self, user_env: &[String]) -> std::result::Result<String, String> {
        for key in user_env {
            if let Some(v) = self.system().env(key) {
                return Ok(v);
            }
        }
        Err(format!(
            "no env value for basic-auth username (tried {user_env:?})"
        ))
    }

    /// Decide what auth the host injects into an `http.do` request: the legacy `bearer_purpose` (always
    /// Bearer) or `auth_purpose` (respects the declared [`AuthScheme`]). Pure given the resolved env, so
    /// it is unit-testable without a network round-trip.
    async fn resolve_auth(&self, payload: &Value) -> std::result::Result<AuthInjection, String> {
        if let Some(p) = payload.get("bearer_purpose").and_then(|v| v.as_str()) {
            return Ok(AuthInjection::Bearer(self.resolve_purpose(p).await?));
        }
        let Some(p) = payload.get("auth_purpose").and_then(|v| v.as_str()) else {
            return Ok(AuthInjection::None);
        };
        let method = self
            .auth
            .iter()
            .find(|a| a.purpose == p)
            .ok_or_else(|| format!("no auth method declared for purpose `{p}`"))?;
        let scheme = method.scheme.clone();
        let user_env = method.user_env.clone();
        let secret = self.resolve_purpose(p).await?;
        Ok(match scheme {
            AuthScheme::Bearer => AuthInjection::Bearer(secret),
            AuthScheme::Basic => AuthInjection::Basic {
                user: self.resolve_user(&user_env)?,
                secret,
            },
            AuthScheme::Header { name } => AuthInjection::Header {
                name,
                value: secret,
            },
            AuthScheme::Query { name } => AuthInjection::Query {
                name,
                value: secret,
            },
        })
    }

    /// Send an `http.do` request through a redirect loop the host controls. Only GET/HEAD redirects
    /// are followed; request bodies are never replayed. Each target passes through the shared SSRF
    /// guard and this plugin's manifest host allow-list before any bytes leave the process.
    async fn send_http_guarded(
        &self,
        initial_url: url::Url,
        method: reqwest::Method,
        mut headers: reqwest::header::HeaderMap,
        mut body: Option<Vec<u8>>,
        query_auth_name: Option<&str>,
    ) -> std::result::Result<reqwest::Response, String> {
        const MAX_REDIRECTS: usize = 5;

        let follows_redirects = method == reqwest::Method::GET || method == reqwest::Method::HEAD;
        let mut url = initial_url;
        let mut redirects = 0usize;
        loop {
            let (guarded_url, pinned) = guard_http_url_pinned(
                url.as_str(),
                &self.private_net_allow(),
                self.host_resolver.as_ref(),
            )?;
            self.ensure_http_host_allowed(&guarded_url)?;
            let client = pinned_http_client(&guarded_url, &pinned, "http.do")?;
            url = guarded_url;
            let mut request = client
                .request(method.clone(), url.clone())
                .headers(headers.clone());
            if let Some(bytes) = &body {
                request = request.body(bytes.clone());
            }
            let response = request.send().await.map_err(|e| format!("http.do: {e}"))?;
            if let Some(host) = url.host_str() {
                self.audit_admit(host, &pinned);
            }

            if !follows_redirects || !is_followed_http_redirect(response.status()) {
                return Ok(response);
            }
            let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                return Ok(response);
            };
            if redirects == MAX_REDIRECTS {
                return Err(format!(
                    "http.do: too many redirects (maximum {MAX_REDIRECTS})"
                ));
            }
            let location = location
                .to_str()
                .map_err(|_| "http.do: redirect Location is not valid text".to_string())?;
            let joined = url
                .join(location)
                .map_err(|e| format!("http.do: invalid redirect Location: {e}"))?;
            let mut next = joined;
            if url.scheme() == "https" && next.scheme() == "http" {
                return Err(format!(
                    "http.do: refusing HTTPS-to-HTTP redirect to {next}"
                ));
            }
            if !same_http_origin(&url, &next) {
                // The host cannot reliably classify arbitrary custom headers as credentials, so a
                // cross-origin redirect gets none of them — including caller, endpoint-ref, and
                // auth-purpose headers. Query auth is host-injected too and is removed explicitly.
                headers.clear();
                if let Some(name) = query_auth_name {
                    remove_query_pair(&mut next, name);
                }
            }
            body = None;
            url = next;
            redirects += 1;
        }
    }
}

/// The auth the host injects into an `http.do` request, resolved from the payload + manifest. The
/// secret never crosses back to the plugin on this path.
#[derive(Debug, PartialEq)]
enum AuthInjection {
    None,
    Bearer(String),
    Basic { user: String, secret: String },
    Header { name: String, value: String },
    Query { name: String, value: String },
}

#[async_trait]
impl HostCapabilities for SystemHostCaps {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String> {
        match command {
            "process.run" => {
                let argv: Vec<String> = payload
                    .get("argv")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if argv.is_empty() {
                    return Err("process.run: `argv` (non-empty array) required".into());
                }
                // The plugin may only run argv shapes it declared in its manifest's capabilities:
                // each grant entry is an argv prefix (program + optional pinned leading
                // subcommand tokens, C-90), so `kubectl get` does not authorize `kubectl delete`.
                if !flux_plugin_protocol::process_grant_allows(&self.grants.process, &argv) {
                    return Err(format!(
                        "process.run: `{}` does not match this plugin's granted process capabilities",
                        argv.join(" ")
                    ));
                }
                let secs = payload
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(60);
                let out = self
                    .system()
                    .run(&argv, std::time::Duration::from_secs(secs))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({ "stdout": out.stdout, "stderr": out.stderr, "exit_code": out.exit_code }),
                )
            }
            "process.spawn" => {
                let argv: Vec<String> = payload
                    .get("argv")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if argv.is_empty() {
                    return Err("process.spawn: `argv` (non-empty array) required".into());
                }
                // Same deny-by-default argv-prefix gate as `process.run` (C-90).
                if !flux_plugin_protocol::process_grant_allows(&self.grants.process, &argv) {
                    return Err(format!(
                        "process.spawn: `{}` does not match this plugin's granted process capabilities",
                        argv.join(" ")
                    ));
                }
                // Optional caller env overrides (applied on top of the cleared+allow-listed env by
                // `spawn_background`); only string values are taken.
                let env: Vec<(String, String)> = payload
                    .get("env")
                    .and_then(|v| v.as_object())
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
                let child = self
                    .system()
                    .spawn_background(&argv, &env)
                    .map_err(|e| e.to_string())?;
                let id = self
                    .next_proc
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.procs.lock().await.insert(id, child);
                Ok(json!({ "proc_id": id }))
            }
            "process.read" => {
                let id = payload
                    .get("proc_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("process.read: `proc_id` required")?;
                let mut guard = self.procs.lock().await;
                let child = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("process.read: no managed process {id}"))?;
                let (stdout, stderr) = child.read_output();
                let st = child.status();
                let mut out = json!({
                    "stdout": stdout,
                    "stderr": stderr,
                    "running": st.running,
                });
                if let Some(code) = st.exit_code {
                    out["exit_code"] = json!(code);
                }
                Ok(out)
            }
            "process.status" => {
                let id = payload
                    .get("proc_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("process.status: `proc_id` required")?;
                let mut guard = self.procs.lock().await;
                let child = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("process.status: no managed process {id}"))?;
                let st = child.status();
                let mut out = json!({ "running": st.running });
                if let Some(code) = st.exit_code {
                    out["exit_code"] = json!(code);
                }
                Ok(out)
            }
            "process.kill" => {
                let id = payload
                    .get("proc_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("process.kill: `proc_id` required")?;
                if let Some(mut child) = self.procs.lock().await.remove(&id) {
                    child.kill();
                }
                Ok(json!({ "ok": true }))
            }
            "secret" => {
                // Resolve by `purpose` (auth-method indirection) or a direct `key`. Either way only
                // granted env keys are read — never arbitrary host secrets.
                if let Some(purpose) = payload.get("purpose").and_then(|v| v.as_str()) {
                    return self
                        .resolve_purpose(purpose)
                        .await
                        .map(|v| json!({ "value": v }));
                }
                let key = payload.get("key").and_then(|v| v.as_str()).unwrap_or("");
                if !self.grants.secrets.iter().any(|k| k == key) {
                    return Err(format!(
                        "secret `{key}` not in this plugin's granted capabilities"
                    ));
                }
                match self.system().env(key) {
                    Some(v) => {
                        if let Some(sink) = &self.secret_sink {
                            sink.register_secret(&v);
                        }
                        Ok(json!({ "value": v }))
                    }
                    None => Err(format!("secret `{key}` not set")),
                }
            }
            "auth.available" => {
                let purpose = payload.get("purpose").and_then(Value::as_str).unwrap_or("");
                if !self.auth.iter().any(|method| method.purpose == purpose) {
                    return Err(format!("no auth method declared for purpose `{purpose}`"));
                }
                Ok(json!({
                    "available": self.resolve_purpose(purpose).await.is_ok()
                }))
            }
            "config" => {
                // A declared NON-secret config value (D-32) — e.g. jira's Atlassian `cloud_id`.
                // Deny-by-default: only names declared in the manifest's `config` resolve, and a
                // secret-classified env key is refused (see `resolve_config`). This replaces the
                // retired `endpoint` URL-handback for the config-value reads that abused it; URLs
                // themselves now reach the wire only through the ref-based IO paths.
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.resolve_config(name).map(|v| json!({ "value": v }))
            }
            "credential" => {
                // The in-band-auth path for raw-socket protocols (e.g. Postgres SCRAM needs the
                // password value). DENY-BY-DEFAULT: only available if the plugin's manifest granted
                // `credential`. The materialized value is delivered to the (trusted) plugin binary,
                // registered with the redactor so it never leaks into model-visible output, and is
                // NEVER returned through any discovery/endpoint path — only this explicit capability.
                if !self.grants.credential {
                    return Err("credential not granted to this plugin".into());
                }
                let resolver = self
                    .resolver
                    .as_ref()
                    .ok_or("credential requires a reference resolver (none installed)")?;
                // Either a direct `credential_ref` (string or object), or an `endpoint_ref` whose
                // record carries a `credential_ref` to materialize.
                let reference = if let Some(cr) = payload.get("credential_ref") {
                    parse_credential_ref(cr)?
                } else if let Some(endpoint_ref) =
                    payload.get("endpoint_ref").and_then(|v| v.as_str())
                {
                    resolver.credential_ref_for_endpoint(endpoint_ref).await?
                } else {
                    return Err(
                        "credential: `credential_ref` or `endpoint_ref` required".to_string()
                    );
                };
                let material = resolver
                    .resolve_credential_for(&self.consumer, &reference)
                    .await?;
                // Register the value with the redactor (if a sink is installed) so it is scrubbed
                // from any captured/model-visible output even though the trusted plugin receives it.
                if let Some(sink) = &self.secret_sink {
                    sink.register_secret(&material.value);
                }
                Ok(json!({ "value": material.value }))
            }
            "conn.authenticate" => {
                // Host-terminated raw-socket auth (D-31): the host speaks the protocol's startup +
                // in-band auth handshake on an already-dialed `conn_id`, so the trusted plugin is
                // handed a POST-AUTH connection and NEVER receives the password. This closes the last
                // gap in the references-only invariant — the one place a plugin still held a secret
                // value. The credential is resolved host-side (the SAME resolution path as the gated
                // `credential` capability — cross-plugin grant + audit unchanged; only WHO SPEAKS THE
                // HANDSHAKE moves to the host), used on the wire, and never serialized back.
                let conn_id = payload
                    .get("conn_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("conn.authenticate: `conn_id` (u64) required")?;
                let protocol = payload
                    .get("protocol")
                    .and_then(|v| v.as_str())
                    .unwrap_or("postgres");
                let user = payload
                    .get("user")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let database = payload
                    .get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let application_name = payload
                    .get("application_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("flux-plugin")
                    .to_string();
                let timeout = payload
                    .get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .map(std::time::Duration::from_millis);

                // Resolve the credential host-side. Three sources, mirroring the `credential`
                // capability plus the static/env path: an explicit `credential_ref`, an endpoint's
                // attached `credential_ref` (both via the broker, cross-plugin gated), or a declared
                // manifest auth method (`auth_purpose`, the static/named env path). The value never
                // leaves the host.
                let password = if let Some(cr) = payload.get("credential_ref") {
                    let reference = parse_credential_ref(cr)?;
                    let resolver = self.resolver.as_ref().ok_or(
                        "conn.authenticate: credential_ref requires a reference resolver (none installed)",
                    )?;
                    resolver
                        .resolve_credential_for(&self.consumer, &reference)
                        .await?
                        .value
                } else if let Some(endpoint_ref) =
                    payload.get("endpoint_ref").and_then(|v| v.as_str())
                {
                    let resolver = self.resolver.as_ref().ok_or(
                        "conn.authenticate: endpoint_ref requires a reference resolver (none installed)",
                    )?;
                    let reference = resolver.credential_ref_for_endpoint(endpoint_ref).await?;
                    resolver
                        .resolve_credential_for(&self.consumer, &reference)
                        .await?
                        .value
                } else if let Some(purpose) = payload.get("auth_purpose").and_then(|v| v.as_str()) {
                    // Static/named endpoint: resolve the credential from the declared auth method's
                    // env HOST-SIDE. Not gated by the plugin-facing `secrets` grant (same as
                    // `resolve_user`/`resolve_endpoint` reading declared env) — the whole point is
                    // that the plugin can NOT read this key via the `secret` capability, but the host
                    // may resolve it for the declared handshake.
                    self.resolve_handshake_secret(purpose)?
                } else {
                    return Err(
                        "conn.authenticate: one of `credential_ref`/`endpoint_ref`/`auth_purpose` required"
                            .into(),
                    );
                };
                // Register the resolved value with the redactor so it is scrubbed from any captured
                // output even though it never reaches the plugin.
                if let Some(sink) = &self.secret_sink {
                    sink.register_secret(&password);
                }

                let params = handshake::HandshakeParams {
                    user,
                    database,
                    application_name,
                };
                let mut guard = self.conns.lock().await;
                let stream = guard
                    .get_mut(&conn_id)
                    .ok_or_else(|| format!("conn.authenticate: no open connection {conn_id}"))?;
                let result =
                    handshake::terminate_handshake(protocol, stream, &params, &password, timeout)
                        .await?;
                drop(guard);
                // The response carries ONLY negotiated non-secret parameters — never the password.
                let mut out = json!({
                    "parameters": result.parameters,
                    "server_version": result.server_version(),
                });
                if let Some(pid) = result.backend_pid {
                    out["backend_pid"] = json!(pid);
                }
                if let Some(key) = result.backend_key {
                    out["backend_key"] = json!(key);
                }
                // MySQL only: the negotiated capability flags, reported for diagnosis. The `sql`
                // plugin deliberately does NOT consume these — it decodes both CLIENT_DEPRECATE_EOF
                // result-set shapes from packet sizes instead, because surfacing the flag through
                // `host_kit::HandshakeInfo` would mean a new public field on a 1.0.0 protocol-line
                // type (a semver break). See the D-197 story for the full reasoning.
                if let Some(caps) = result.capabilities {
                    out["capabilities"] = json!(caps);
                }
                Ok(out)
            }
            "fs.read" => {
                // Path-scoped HOST-file read (C-09a). For the `aws-bedrock` plugin to read
                // `~/.aws/config` + `~/.aws/sso/cache` (the SSO refresh-token cache) without an
                // `aws` CLI. These are HOST paths OUTSIDE the workspace jail (which `System::read_file`
                // cannot reach), so the capability has its own manifest-declared scope: the host reads
                // ONLY paths matching a declared [`FsReadScope`], denies anything out of scope, rejects
                // `..` traversal, caps the size, and registers `secret: true` reads with the
                // [`Redactor`](flux_secret::Redactor) so refresh tokens never leak into model-visible
                // output. Deny-by-default: an empty `fs` grant refuses every read.
                let raw_path = payload
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or("fs.read: `path` (string) required")?;
                let expanded = expand_home(raw_path);
                // Reject `..` traversal before matching (defense-in-depth; the scope match is the
                // primary gate, but a naive glob join could otherwise reach outside the scope dir).
                if path_has_traversal(&expanded) {
                    return Err(format!(
                        "fs.read: path `{raw_path}` contains a `..` traversal; denied"
                    ));
                }
                const MAX_FS_READ: usize = 256 * 1024;
                let mut admitted = None;
                for grant in &self.grants.fs {
                    let expanded_scope = expand_home(&grant.path);
                    match self
                        .system()
                        .read_file_scoped(&expanded, &expanded_scope, MAX_FS_READ)
                        .await
                    {
                        Ok(read) => {
                            admitted = Some((grant.path.clone(), grant.secret, read));
                            break;
                        }
                        Err(flux_core::Error::Config(_)) => {}
                        Err(err) => {
                            return Err(format!(
                                "fs.read: {raw_path}: {err} (scope: {})",
                                grant.path
                            ))
                        }
                    }
                }
                let Some((_scope, secret, read)) = admitted else {
                    return Err(format!(
                        "fs.read: path `{raw_path}` not in this plugin's fs.read scope"
                    ));
                };
                let size = read.size;
                let truncated = read.truncated;
                let bytes = read.bytes;
                // Binary (NUL-bearing or invalid UTF-8) -> base64; else UTF-8 text. Same shape as
                // `http.do`'s body/body_b64 split, and byte-capped on a char boundary.
                let utf8 = std::str::from_utf8(&bytes);
                let incomplete_utf8_tail =
                    truncated && utf8.as_ref().is_err_and(|err| err.error_len().is_none());
                let is_binary = bytes.contains(&0) || (utf8.is_err() && !incomplete_utf8_tail);
                if is_binary {
                    let capped = bytes;
                    let body_b64 = base64::engine::general_purpose::STANDARD.encode(&capped);
                    if secret {
                        if let Some(sink) = &self.secret_sink {
                            sink.register_secret(&String::from_utf8_lossy(&capped));
                        }
                    }
                    Ok(json!({ "path": raw_path, "size": size, "body_b64": body_b64 }))
                } else {
                    let text = if incomplete_utf8_tail {
                        let valid_up_to = utf8
                            .as_ref()
                            .err()
                            .map_or(bytes.len(), |err| err.valid_up_to());
                        String::from_utf8_lossy(&bytes[..valid_up_to]).into_owned()
                    } else {
                        String::from_utf8_lossy(&bytes).into_owned()
                    };
                    let text = truncate_on_char_boundary(text, MAX_FS_READ);
                    if secret {
                        if let Some(sink) = &self.secret_sink {
                            sink.register_secret(&text);
                        }
                    }
                    Ok(json!({ "path": raw_path, "size": size, "body": text }))
                }
            }
            "http.do" => {
                if !self.grants.http {
                    return Err("http.do not granted to this plugin".into());
                }
                // Ref-based IO (D-27): when the plugin passes an `endpoint_ref`, the host resolves it
                // to an absolute URL + injected auth headers — the plugin (and the model) never see the
                // URL or the credential. The composed URL still runs through the SAME egress guard +
                // host allow-list as the legacy `url` path, so SSRF/private-net rules still apply.
                //
                // NAMED vs DISCOVERED split: a *discovered* `@endpoint/<id>` ref goes to the injected
                // resolver (the L5 broker, which owns the discovery registry + the cross-plugin gate). A
                // *named* manifest endpoint resolves LOCALLY here from the plugin's own `EndpointSpec`
                // env binding + the declared `auth_purpose` injection — so a static plugin needs NO host
                // config beyond "set the documented env var and go" and works with no resolver installed.
                let mut ref_injected: Vec<(String, String)> = Vec::new();
                let mut url = if let Some(endpoint_ref) =
                    payload.get("endpoint_ref").and_then(|v| v.as_str())
                {
                    let path = payload.get("path").and_then(|v| v.as_str());
                    let base =
                        if flux_secret::endpoint::EndpointRef::is_discovered_ref(endpoint_ref) {
                            let resolver = self.resolver.as_ref().ok_or(
                            "http.do: endpoint_ref requires a reference resolver (none installed)",
                        )?;
                            // Resolve on behalf of THIS plugin (the real consumer): if the endpoint's
                            // credential is owned by another plugin, host-injecting it is a cross-plugin use
                            // and the broker's deny-by-default gate fires against `self.consumer`.
                            let resolved = resolver
                                .resolve_endpoint_for(&self.consumer, endpoint_ref)
                                .await?;
                            ref_injected = resolved.injected_headers;
                            resolved.url
                        } else {
                            // Named manifest endpoint → resolve its base URL locally from the declared env.
                            self.resolve_endpoint(endpoint_ref)?
                        };
                    let composed = compose_url(&base, path)?;
                    let url = guard_http_url(&composed, &self.private_net_allow())?;
                    self.ensure_http_host_allowed(&url)?;
                    url
                } else {
                    let raw = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let url = guard_http_url(raw, &self.private_net_allow())?;
                    self.ensure_http_host_allowed(&url)?;
                    url
                };
                let method = payload
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET")
                    .to_uppercase();
                let m = reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|e| format!("http.do: bad method `{method}`: {e}"))?;
                // Auth injection by purpose: the host resolves the secret and injects it per the
                // method's declared scheme — the plugin never sees raw tokens on this path. `Query`
                // mutates the URL, so resolve before building the request.
                let inject = self.resolve_auth(payload).await?;
                let query_auth_name = match &inject {
                    AuthInjection::Query { name, .. } => Some(name.clone()),
                    _ => None,
                };
                if let AuthInjection::Query { name, value } = &inject {
                    url.query_pairs_mut().append_pair(name, value);
                }
                let mut headers = reqwest::header::HeaderMap::new();
                if let Some(requested) = payload.get("headers").and_then(|v| v.as_object()) {
                    for (k, v) in requested {
                        if let Some(s) = v.as_str() {
                            insert_http_header(&mut headers, k, s)?;
                        }
                    }
                }
                // Host-injected auth from the resolved endpoint (the `endpoint_ref` path): applied
                // host-side BEFORE the legacy `auth_purpose` injection, so a ref-resolved credential
                // reaches the wire without the plugin ever holding the value.
                for (name, value) in ref_injected {
                    insert_http_header(&mut headers, &name, &value)?;
                }
                match inject {
                    AuthInjection::None | AuthInjection::Query { .. } => {}
                    AuthInjection::Bearer(token) => insert_http_header(
                        &mut headers,
                        "Authorization",
                        &format!("Bearer {token}"),
                    )?,
                    AuthInjection::Basic { user, secret } => {
                        let encoded = base64::engine::general_purpose::STANDARD
                            .encode(format!("{user}:{secret}"));
                        insert_http_header(
                            &mut headers,
                            "Authorization",
                            &format!("Basic {encoded}"),
                        )?;
                    }
                    AuthInjection::Header { name, value } => {
                        insert_http_header(&mut headers, &name, &value)?;
                    }
                }
                // Request body: a base64 `body_b64` (byte-exact upload) wins over the text `body`;
                // either one (never both) becomes the request body.
                let body = if let Some(b64) = payload.get("body_b64").and_then(|v| v.as_str()) {
                    Some(
                        base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .map_err(|e| format!("http.do: bad body_b64: {e}"))?,
                    )
                } else {
                    payload
                        .get("body")
                        .and_then(|v| v.as_str())
                        .map(|body| body.as_bytes().to_vec())
                };
                let response_binary = payload
                    .get("response_binary")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let timeout_ms = http_do_timeout_ms(payload)?;
                let exchange = async move {
                    let resp = self
                        .send_http_guarded(url, m, headers, body, query_auth_name.as_deref())
                        .await?;
                    let status = resp.status().as_u16();
                    // Binary download path (`response_binary: true`): return raw base64 bytes,
                    // capped without character truncation so a byte-exact download survives.
                    if response_binary {
                        const MAX_BIN_BODY: usize = 16 * 1024 * 1024;
                        let (bytes, _) = read_http_body_capped(resp, MAX_BIN_BODY).await?;
                        let body_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                        return Ok(json!({ "status": status, "body_b64": body_b64 }));
                    }
                    let (bytes, _) = read_http_body_capped(resp, 256 * 1024).await?;
                    let body = truncate_on_char_boundary(
                        String::from_utf8_lossy(&bytes).into_owned(),
                        256 * 1024,
                    );
                    Ok(json!({ "status": status, "body": body }))
                };
                match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), exchange)
                    .await
                {
                    Ok(result) => result,
                    Err(_) => Err(format!("http.do: timed out after {timeout_ms}ms")),
                }
            }
            "conn.dial" => {
                // Ref-based dial (D-27): when the plugin passes an `endpoint_ref`, the host resolves
                // it and takes host:port from the resolved URL — the plugin passes the ref, not the
                // host:port. The resolved target still runs through the same `dial_scoped` guard +
                // grant check below.
                //
                // NAMED vs DISCOVERED split (mirrors `http.do`): a *discovered* `@endpoint/<id>` ref
                // resolves through the injected resolver; a *named* manifest endpoint resolves its
                // host:port LOCALLY from the plugin's own `EndpointSpec` env binding (no host config,
                // no resolver needed).
                let target = if let Some(endpoint_ref) =
                    payload.get("endpoint_ref").and_then(|v| v.as_str())
                {
                    let base = if flux_secret::endpoint::EndpointRef::is_discovered_ref(
                        endpoint_ref,
                    ) {
                        let resolver = self.resolver.as_ref().ok_or(
                            "conn.dial: endpoint_ref requires a reference resolver (none installed)",
                        )?;
                        // Resolve on behalf of THIS plugin (the real consumer) — same cross-plugin
                        // gating rationale as the `http.do` path (the resolved URL's host:port is what
                        // we dial; any cross-plugin credential on the record is gated against
                        // `self.consumer`).
                        let resolved = resolver
                            .resolve_endpoint_for(&self.consumer, endpoint_ref)
                            .await?;
                        resolved.url
                    } else {
                        // Named manifest endpoint → resolve its base URL locally from the declared env.
                        self.resolve_endpoint(endpoint_ref)?
                    };
                    dial_target_from_url(&base)?
                } else {
                    let kind = payload
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tcp");
                    match kind {
                        "tcp" => {
                            let host = payload
                                .get("host")
                                .and_then(|v| v.as_str())
                                .ok_or("conn.dial: `host` required for tcp")?
                                .to_string();
                            let port = payload
                                .get("port")
                                .and_then(|v| v.as_u64())
                                .ok_or("conn.dial: `port` required for tcp")?
                                as u16;
                            flux_system::net::DialTarget::Tcp { host, port }
                        }
                        "unix" => {
                            let path = payload
                                .get("path")
                                .and_then(|v| v.as_str())
                                .ok_or("conn.dial: `path` required for unix")?
                                .to_string();
                            flux_system::net::DialTarget::Unix { path }
                        }
                        other => return Err(format!("conn.dial: unknown kind `{other}`")),
                    }
                };
                // A Unix grant names a filesystem location, so it can only be enforced against the
                // socket's *physical* identity. Rejecting `.`/`..` and confining `*` to one segment
                // still leaves `alias.sock` — a dot-free, single-segment, perfectly granted spelling
                // — free to be a symlink the kernel then follows to a listener outside the grant.
                // Both sides are reduced, never just the target: a grant and a target that name one
                // socket through different symlinked spellings (`/tmp` vs macOS `/private/tmp`) must
                // still match. Resolving here also makes the checked path the dialed path, so the
                // link cannot be repointed between the two.
                let unix_path = match &target {
                    flux_system::net::DialTarget::Unix { path } => Some(path.clone()),
                    flux_system::net::DialTarget::Tcp { .. } => None,
                };
                let (target, grants) = if let Some(path) = unix_path {
                    let sys = self.system();
                    let physical = sys.host_path_identity(&path).map_err(|e| {
                        format!("conn.dial: cannot resolve unix path `{path}`: {e}")
                    })?;
                    let grants = self
                        .grants
                        .conn
                        .iter()
                        .map(|g| physical_unix_grant(sys.as_ref(), g))
                        .collect::<Vec<_>>();
                    (
                        flux_system::net::DialTarget::Unix { path: physical },
                        std::borrow::Cow::Owned(grants),
                    )
                } else {
                    (target, std::borrow::Cow::Borrowed(&self.grants.conn))
                };
                let tstr = conn_target_str(&target);
                if !conn_granted(&grants[..], &tstr) {
                    return Err(format!(
                        "conn.dial: target `{tstr}` not in this plugin's granted conn capabilities"
                    ));
                }
                let (stream, pinned) = flux_system::net::dial_scoped_pinned_with_resolver(
                    &target,
                    &self.private_net_allow(),
                    self.host_resolver.as_ref(),
                )
                .await
                .map_err(|e| e.to_string())?;
                // The dial was admitted. A TCP target that resolves private was let through by the
                // scoped grant (Unix sockets aren't IP egress) — audit it.
                if let flux_system::net::DialTarget::Tcp { host, .. } = &target {
                    self.audit_admit(host, &pinned);
                }
                let id = self
                    .next_conn
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.conns.lock().await.insert(id, stream);
                Ok(json!({ "conn_id": id }))
            }
            "conn.read" => {
                let id = payload
                    .get("conn_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("conn.read: `conn_id` required")?;
                let max = payload
                    .get("max")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(64 * 1024)
                    .min(1024 * 1024) as usize;
                // Optional per-call read deadline (D-45: sql/asterisk `timeout` parity). When set,
                // `stream.read` is raced against the deadline; on elapsed the connection stays open
                // (the plugin decides to retry or close) and a `timed_out` flag is returned so the
                // plugin's wire-protocol loop can surface a timeout error rather than a silent hang.
                let timeout_ms = payload.get("timeout_ms").and_then(|v| v.as_u64());
                let mut guard = self.conns.lock().await;
                let stream = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("conn.read: no open connection {id}"))?;
                let read_fut = stream.read(max);
                let (data, timed_out) = match timeout_ms {
                    Some(ms) => {
                        let dur = std::time::Duration::from_millis(ms);
                        match tokio::time::timeout(dur, read_fut).await {
                            Ok(Ok(data)) => (data, false),
                            Ok(Err(e)) => return Err(format!("conn.read: {e}")),
                            Err(_) => (Vec::new(), true),
                        }
                    }
                    None => (read_fut.await.map_err(|e| e.to_string())?, false),
                };
                let eof = data.is_empty() && !timed_out;
                Ok(json!({
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(&data),
                    "eof": eof,
                    "timed_out": timed_out
                }))
            }
            "conn.write" => {
                let id = payload
                    .get("conn_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("conn.write: `conn_id` required")?;
                let data_b64 = payload
                    .get("data_b64")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_b64)
                    .map_err(|e| format!("conn.write: bad base64: {e}"))?;
                let mut guard = self.conns.lock().await;
                let stream = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("conn.write: no open connection {id}"))?;
                stream.write_all(&data).await.map_err(|e| e.to_string())?;
                Ok(json!({ "written": data.len() }))
            }
            "conn.close" => {
                let id = payload
                    .get("conn_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("conn.close: `conn_id` required")?;
                if let Some(mut stream) = self.conns.lock().await.remove(&id) {
                    let _ = stream.shutdown().await;
                }
                Ok(json!({ "ok": true }))
            }
            "blob.put" => {
                if !self.grants.blob {
                    return Err("blob.put not granted to this plugin".into());
                }
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let b64 = payload
                    .get("data_b64")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let data = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| format!("blob.put: bad base64: {e}"))?;
                let mut h = Sha256::new();
                h.update(&data);
                let blob_ref = h
                    .finalize()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                self.blobs
                    .lock()
                    .await
                    .insert(blob_ref.clone(), (name, data));
                Ok(json!({ "blob_ref": blob_ref }))
            }
            "blob.get" => {
                if !self.grants.blob {
                    return Err("blob.get not granted to this plugin".into());
                }
                let r = payload
                    .get("blob_ref")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let guard = self.blobs.lock().await;
                let (_, data) = guard
                    .get(r)
                    .ok_or_else(|| format!("blob.get: no blob {r}"))?;
                Ok(json!({ "data_b64": base64::engine::general_purpose::STANDARD.encode(data) }))
            }
            "blob.info" => {
                if !self.grants.blob {
                    return Err("blob.info not granted to this plugin".into());
                }
                let r = payload
                    .get("blob_ref")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let guard = self.blobs.lock().await;
                let (name, data) = guard
                    .get(r)
                    .ok_or_else(|| format!("blob.info: no blob {r}"))?;
                Ok(json!({ "name": name, "size": data.len(), "sha256": r }))
            }
            other => Err(format!("unknown host capability: {other}")),
        }
    }
}

/// The canonical grant string for a dial target (`tcp:host:port` / `unix:/path`).
fn conn_target_str(t: &flux_system::net::DialTarget) -> String {
    match t {
        flux_system::net::DialTarget::Tcp { host, port } => format!("tcp:{host}:{port}"),
        flux_system::net::DialTarget::Unix { path } => format!("unix:{path}"),
    }
}

/// Whether a plugin's `conn` grant list permits `target`. Entries match exactly or with a single `*`
/// wildcard segment (e.g. `tcp:*:5432`, `tcp:db.internal:*`, `unix:/var/run/*.sock`). Unix targets
/// containing `.`/`..` components are refused before matching so the kernel cannot resolve a
/// granted spelling to a socket outside that grant.
fn conn_granted(grants: &[String], target: &str) -> bool {
    if target
        .strip_prefix("unix:")
        .is_some_and(unix_path_has_dot_components)
    {
        return false;
    }
    grants.iter().any(|g| conn_glob(g, target))
}

/// Reduce a `unix:` conn grant to physical form so it can be matched against a physically-reduced
/// target. Only the literal directory prefix *before* any wildcard is resolved — the wildcard and
/// the segment holding it are pattern text, not a path, so they are carried through untouched. A
/// non-unix grant, a relative one, or one whose prefix cannot be resolved passes through verbatim:
/// failing to reduce a grant must never silently widen it, and an unreduced grant simply fails to
/// match a reduced target.
fn physical_unix_grant(sys: &dyn PluginSystem, grant: &str) -> String {
    let Some(pattern) = grant.strip_prefix("unix:") else {
        return grant.to_string();
    };
    let head = match pattern.split_once('*') {
        Some((literal, _)) => literal,
        None => pattern,
    };
    // Everything up to the last separator is a real directory; the remainder stays as written.
    let Some(cut) = head.rfind('/') else {
        return grant.to_string();
    };
    let dir = &pattern[..cut];
    if dir.is_empty() {
        return grant.to_string();
    }
    match sys.host_path_identity(dir) {
        Ok(physical) => format!("unix:{physical}{}", &pattern[cut..]),
        Err(_) => grant.to_string(),
    }
}

/// Match a pattern with at most one `*` wildcard against a string.
fn conn_glob(pat: &str, s: &str) -> bool {
    if let (Some(pattern), Some(target)) = (pat.strip_prefix("unix:"), s.strip_prefix("unix:")) {
        if unix_path_has_dot_components(pattern) || pat.matches('*').count() > 1 {
            return false;
        }
        return match pattern.split_once('*') {
            Some((pre, suf)) => {
                if target.len() < pre.len() + suf.len()
                    || !target.starts_with(pre)
                    || !target.ends_with(suf)
                {
                    return false;
                }
                let wildcard = &target[pre.len()..target.len() - suf.len()];
                !wildcard.contains('/')
            }
            None => pattern == target,
        };
    }
    match pat.split_once('*') {
        Some((pre, suf)) => {
            s.len() >= pre.len() + suf.len() && s.starts_with(pre) && s.ends_with(suf)
        }
        None => pat == s,
    }
}

fn unix_path_has_dot_components(path: &str) -> bool {
    path.split('/').any(|part| matches!(part, "." | ".."))
}

/// Truncate a `String` to at most `max` bytes without splitting a UTF-8 codepoint (`String::truncate`
/// panics off a char boundary on attacker-controlled bodies).
fn truncate_on_char_boundary(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}

fn insert_http_header(
    headers: &mut reqwest::header::HeaderMap,
    name: &str,
    value: &str,
) -> std::result::Result<(), String> {
    let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
        .map_err(|e| format!("http.do: invalid header name `{name}`: {e}"))?;
    let value = reqwest::header::HeaderValue::from_str(value)
        .map_err(|e| format!("http.do: invalid value for header `{name}`: {e}"))?;
    headers.insert(name, value);
    Ok(())
}

fn is_followed_http_redirect(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::MOVED_PERMANENTLY
            | reqwest::StatusCode::FOUND
            | reqwest::StatusCode::SEE_OTHER
            | reqwest::StatusCode::TEMPORARY_REDIRECT
            | reqwest::StatusCode::PERMANENT_REDIRECT
    )
}

fn same_http_origin(a: &url::Url, b: &url::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str()
            .zip(b.host_str())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
        && a.port_or_known_default() == b.port_or_known_default()
}

fn remove_query_pair(url: &mut url::Url, name: &str) {
    let kept = url
        .query_pairs()
        .filter(|(key, _)| key != name)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !kept.is_empty() {
        url.query_pairs_mut().extend_pairs(kept);
    }
}

/// Overall `http.do` deadline. Plugins may configure milliseconds directly (or seconds for parity
/// with `process.run`); absent configuration gets a finite default, and hostile values cannot
/// create an effectively unbounded request.
fn http_do_timeout_ms(payload: &Value) -> std::result::Result<u64, String> {
    const DEFAULT_MS: u64 = 30_000;
    const MAX_MS: u64 = 300_000;

    let requested = if let Some(value) = payload.get("timeout_ms") {
        value
            .as_u64()
            .ok_or_else(|| "http.do: `timeout_ms` must be a non-negative integer".to_string())?
    } else if let Some(value) = payload.get("timeout_secs") {
        value
            .as_u64()
            .ok_or_else(|| "http.do: `timeout_secs` must be a non-negative integer".to_string())?
            .saturating_mul(1_000)
    } else {
        DEFAULT_MS
    };
    Ok(requested.clamp(1, MAX_MS))
}

/// Incrementally retain at most `max` bytes from an HTTP response. The response is dropped as soon
/// as the budget fills instead of first allocating an attacker-controlled whole body.
async fn read_http_body_capped(
    mut response: reqwest::Response,
    max: usize,
) -> std::result::Result<(Vec<u8>, bool), String> {
    let declared_over_cap = response
        .content_length()
        .is_some_and(|len| len > max as u64);
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .map(|len| len.min(max as u64) as usize)
            .unwrap_or(0),
    );
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("http.do: response body read failed: {e}"))?
    {
        let remaining = max.saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        let take = remaining.min(chunk.len());
        bytes.extend_from_slice(&chunk[..take]);
        if take < chunk.len() || (declared_over_cap && bytes.len() == max) {
            truncated = true;
            break;
        }
    }
    Ok((bytes, truncated))
}

/// Reject non-HTTP(S) schemes and (unless `allow_private`) private/loopback/link-local hosts —
/// delegating to the shared egress guard in `flux-system` (host→IP resolution, IPv6/IPv4-mapped
/// coverage), the same SSRF policy the agent's own `web.fetch` uses.
fn guard_http_url(raw: &str, allow: &PrivateNetAllow) -> std::result::Result<url::Url, String> {
    flux_system::net::guard_url_scoped(raw, allow).map_err(|e| e.to_string())
}

fn guard_http_url_pinned(
    raw: &str,
    allow: &PrivateNetAllow,
    resolver: &dyn flux_system::net::HostResolver,
) -> std::result::Result<(url::Url, Vec<std::net::SocketAddr>), String> {
    flux_system::net::guard_url_scoped_pinned_with_resolver(raw, allow, resolver)
        .map_err(|e| e.to_string())
}

/// Build a redirect-disabled client for exactly one authorized HTTP hop. The host mapping is
/// replaced with the guard's vetted socket set, closing the gap between DNS authorization and the
/// connection. An empty answer is a failed authorization, not permission to resolve again.
fn pinned_http_client(
    url: &url::Url,
    pinned: &[std::net::SocketAddr],
    op: &str,
) -> std::result::Result<reqwest::Client, String> {
    let host = url
        .host_str()
        .ok_or_else(|| format!("{op}: guarded URL has no host"))?;
    if pinned.is_empty() {
        return Err(format!(
            "{op}: refusing to connect to {host} — DNS returned no vetted addresses"
        ));
    }
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // Never allow ambient proxy configuration to replace the guard-vetted connection peer or
        // perform the hostname resolution behind the authorization boundary.
        .no_proxy()
        .resolve_to_addrs(host, pinned)
        .build()
        .map_err(|e| format!("{op}: building pinned HTTP client failed: {e}"))
}

/// Compose an absolute request URL from a resolved base and an optional plugin-supplied `path`.
/// The base is the host-resolved endpoint URL (already credential-free); `path` is APPENDED with
/// slash-normalized concatenation (base `…/api` + `/auth.test` → `…/api/auth.test`) — the same
/// join the host-kit `MockHost` and the OAuth `token_path` resolution use. Deliberately NOT
/// RFC-3986 `Url::join` (D-125): under join semantics a leading-slash path REPLACES a path-bearing
/// base's path (dropping slack's `/api` and 404ing every op), and a full-URL path could swap out
/// the pinned endpoint base entirely. The composed string still re-parses through the egress guard
/// at the call site, so a malformed concat fails loudly there. A `None` or empty path returns the
/// base unchanged.
fn compose_url(base: &str, path: Option<&str>) -> std::result::Result<String, String> {
    match path {
        None | Some("") => Ok(base.to_string()),
        Some(p) => {
            // Parse-check the base so a broken endpoint binding still surfaces as a base error.
            url::Url::parse(base).map_err(|e| format!("http.do: bad base url: {e}"))?;
            Ok(format!(
                "{}/{}",
                base.trim_end_matches('/'),
                p.trim_start_matches('/')
            ))
        }
    }
}

/// Build a TCP [`DialTarget`](flux_system::net::DialTarget) from a resolved endpoint URL's host+port
/// (defaulting the port to the URL scheme's known default, plus the SQL DSN schemes the `url` crate
/// doesn't know: postgres 5432, mysql/mariadb 3306). For the ref-based `conn.dial` path.
fn dial_target_from_url(raw: &str) -> std::result::Result<flux_system::net::DialTarget, String> {
    let url = url::Url::parse(raw).map_err(|e| format!("conn.dial: bad endpoint url: {e}"))?;
    let host = url
        .host_str()
        .ok_or("conn.dial: resolved endpoint url has no host")?
        .to_string();
    let port = url
        .port_or_known_default()
        .or(match url.scheme() {
            "postgres" | "postgresql" => Some(5432),
            "mysql" | "mariadb" => Some(3306),
            _ => None,
        })
        .ok_or("conn.dial: resolved endpoint url has no port (and scheme has no default)")?;
    Ok(flux_system::net::DialTarget::Tcp { host, port })
}

/// Percent-encode a URL path/query component: unreserved chars (`alnum` `-_.~`) pass through, all
/// else `%XX` — for substituting config values into an [`EndpointSpec::template`].
fn percent_encode_component(s: &str) -> String {
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

/// Parse a credential reference from the `credential` capability payload: either a `Ref`-shaped
/// object (`{scheme, plugin, instance, slot}`) or a `scheme/...` string.
fn parse_credential_ref(v: &Value) -> std::result::Result<flux_secret::Ref, String> {
    match v {
        Value::String(s) => flux_secret::Ref::parse(s),
        Value::Object(_) => serde_json::from_value(v.clone())
            .map_err(|e| format!("credential: bad credential_ref object: {e}")),
        _ => Err("credential: `credential_ref` must be a string or object".to_string()),
    }
}

fn normalize_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

// --- C-09a `fs.read` path helpers -------------------------------------------------------------

/// Expand a leading `~` to `$HOME` (matching `Workspace::resolve`). `~` alone or `~/...` expands;
/// `~user/...` is left as-is.
fn expand_home(input: &str) -> String {
    if let Some(rest) = input.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            let home = std::env::var("HOME").unwrap_or_default();
            return format!("{home}{rest}");
        }
    }
    input.to_string()
}

/// Whether a path contains a `..` path component (traversal). Rejects `..` as any segment, not
/// just a leading one, so `a/../b` and `/x/..` both trip it — defense-in-depth before the scope
/// match (a naive glob join could otherwise reach outside the scope dir).
fn path_has_traversal(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

fn host_matches(patterns: &[String], host: &str) -> bool {
    let host = host
        .trim()
        .trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase();
    patterns.iter().any(|pattern| {
        let p = pattern
            .trim()
            .trim_matches('[')
            .trim_matches(']')
            .to_ascii_lowercase();
        p == "*"
            || p == host
            || p.strip_prefix("*.").is_some_and(|suffix| {
                host.ends_with(suffix)
                    && host.len() > suffix.len()
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            })
    })
}

pub mod credential_boundary;
mod loading;
mod refresh;

pub(crate) use loading::invalid_plugin_name;
pub use loading::*;
#[cfg(test)]
use loading::{op_coherence_warnings, plugin_tool_spec, semantic_effect_tags};
pub use refresh::CatalogRefresh;
#[cfg(test)]
use refresh::{capability_widenings, op_scope_weakenings};

#[cfg(test)]
mod tests {
    // The pg/mysql protocol modules are exercised directly by the D-31/D-196 handshake tests; the
    // non-test host path reaches them only through `handshake::terminate_handshake`.
    use super::pg;
    use super::*;

    struct SequenceResolver {
        answers: Vec<std::net::IpAddr>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl SequenceResolver {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|answer| answer.parse().unwrap())
                    .collect(),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl flux_system::net::HostResolver for SequenceResolver {
        fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<std::net::IpAddr>> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self
                .answers
                .get(call)
                .or_else(|| self.answers.last())
                .copied()
                .into_iter()
                .collect())
        }
    }

    #[test]
    fn remove_descriptor_deletes_file_and_reports_missing_as_false() {
        let dir = std::env::temp_dir().join(format!("flux-rm-desc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No descriptor yet → `remove_descriptor` reports `false`, not an error.
        assert!(!remove_descriptor(&dir, "ghost").unwrap());

        // Add one, then remove → reports `true`, and discovery no longer lists it.
        add_descriptor(
            &dir,
            "p",
            &PluginDescriptor {
                program: "/bin/true".into(),
                args: vec![],
                pinned: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(remove_descriptor(&dir, "p").unwrap());
        assert!(
            discover(&dir).is_empty(),
            "the descriptor is gone after uninstall"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A plugin name with `..`, a path separator, or an absolute component must be rejected before
    /// any filesystem op — `remove_descriptor` is a destructive `remove_file`, so an escaped name
    /// would delete a file outside the plugins dir (D-35). One guard in `descriptor_path` covers
    /// `add` / `load` / `remove` (and the pack's versioned-store paths).
    #[test]
    fn descriptor_path_rejects_traversal_names() {
        let dir = std::env::temp_dir().join(format!("flux-desc-traversal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A sentinel file *outside* `dir` (a sibling, reachable via `..`). Every traversal name
        // below would, if joined literally, point at or past it. The guard must refuse before any
        // filesystem op touches it.
        let outside = dir.parent().unwrap().join("flux-desc-traversal-sentinel");
        std::fs::write(&outside, b"keep me").unwrap();

        let desc = PluginDescriptor {
            program: "/bin/true".into(),
            args: vec![],
            pinned: None,
            ..Default::default()
        };
        let bad_names = [
            "../sentinel",
            "../../flux-desc-traversal-sentinel",
            "/etc/passwd",
            "a/b",
            "..",
            ".",
            "",
        ];
        for name in bad_names {
            assert!(
                remove_descriptor(&dir, name).is_err(),
                "remove_descriptor(`{name}`) must be rejected"
            );
            assert!(
                add_descriptor(&dir, name, &desc).is_err(),
                "add_descriptor(`{name}`) must be rejected"
            );
            assert!(
                load_descriptor(&dir, name).is_err(),
                "load_descriptor(`{name}`) must be rejected"
            );
            assert!(
                pack::purge_store(&dir, name).is_err(),
                "purge_store(`{name}`) must be rejected"
            );
        }

        // The sentinel outside `dir` is untouched — no traversal reached it.
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "keep me",
            "no traversal name reached a file outside the plugins dir"
        );
        // And nothing was written *inside* `dir` either.
        assert!(
            discover(&dir).is_empty(),
            "no descriptor was created for a rejected name"
        );

        // Legitimate names still work (the guard must not over-reach).
        add_descriptor(&dir, "my-plugin_v2.0", &desc).unwrap();
        assert!(load_descriptor(&dir, "my-plugin_v2.0").unwrap().is_some());
        assert!(remove_descriptor(&dir, "my-plugin_v2.0").unwrap());

        std::fs::remove_file(&outside).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn host_caps_deny_ungranted_and_allow_granted() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-caps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        // A fresh SystemHostCaps grants nothing.
        let none = SystemHostCaps::new(sys.clone());
        assert!(
            none.handle("process.run", &json!({"argv": ["echo", "hi"]}))
                .await
                .is_err(),
            "ungranted process.run must be denied"
        );
        assert!(
            none.handle("secret", &json!({"key": "PATH"}))
                .await
                .is_err(),
            "ungranted secret must be denied (no arbitrary env reads)"
        );
        assert!(
            none.handle("http.do", &json!({"url": "http://example.com"}))
                .await
                .is_err(),
            "ungranted http.do must be denied"
        );

        // Granting only `echo` lets echo run but nothing else; secret stays denied.
        let limited = SystemHostCaps::new(sys.clone()).with_grants(PluginCapabilities {
            process: vec!["echo".into()],
            secrets: vec![],
            http: false,
            ..Default::default()
        });
        assert!(
            limited
                .handle("process.run", &json!({"argv": ["echo", "hi"]}))
                .await
                .is_ok(),
            "a granted program should run"
        );
        assert!(
            limited
                .handle("process.run", &json!({"argv": ["cat", "/etc/passwd"]}))
                .await
                .is_err(),
            "a non-granted program must be denied"
        );
        assert!(
            limited
                .handle("secret", &json!({"key": "PATH"}))
                .await
                .is_err(),
            "secret not in the grant list must be denied"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-122: plugin `process.run` executes in the context's ACTIVE root, not the root captured
    /// at assembly. A workspace transition (`git_worktree_enter` swapping the active system) must
    /// be observed by the next plugin op — the pre-C-122 captured-`Arc<System>` semantics kept
    /// every plugin process in the original root for the rest of the session.
    #[tokio::test]
    async fn process_run_follows_the_active_system_across_a_transition() {
        use flux_system::{System, Workspace};
        let base = std::env::temp_dir().join(format!("flux-c122-seam-{}", std::process::id()));
        let (root_a, root_b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        let sys_a = Arc::new(System::new(Workspace::new(&root_a).unwrap()));
        let sys_b = Arc::new(System::new(Workspace::new(&root_b).unwrap()));

        /// A swappable source — what the surfaces' `WorkspaceContext` adapter looks like to this
        /// crate: `system()` snapshots whatever is active now.
        struct Swappable(std::sync::Mutex<Arc<dyn PluginSystem>>);
        impl SystemSource for Swappable {
            fn system(&self) -> Arc<dyn PluginSystem> {
                self.0.lock().unwrap().clone()
            }
        }

        let source = Arc::new(Swappable(std::sync::Mutex::new(sys_a)));
        let caps = SystemHostCaps::from_source(source.clone()).with_grants(PluginCapabilities {
            process: vec!["pwd".into()],
            ..Default::default()
        });

        let cwd_of = |out: Value| {
            out["stdout"]
                .as_str()
                .expect("pwd writes its cwd")
                .trim()
                .to_string()
        };
        let before = caps
            .handle("process.run", &json!({"argv": ["pwd"]}))
            .await
            .expect("granted pwd runs");
        // `pwd` may print a symlink-resolved path (/tmp vs /private/tmp), so compare canonically.
        let canon = |p: &std::path::Path| p.canonicalize().unwrap().display().to_string();
        assert_eq!(cwd_of(before), canon(&root_a), "before: the original root");

        // The transition: the context's active system swaps (what enter_worktree does).
        *source.0.lock().unwrap() = sys_b;

        let after = caps
            .handle("process.run", &json!({"argv": ["pwd"]}))
            .await
            .expect("granted pwd still runs");
        assert_eq!(
            cwd_of(after),
            canon(&root_b),
            "after: the NEXT op observes the transitioned root — the captured-Arc semantics \
             would still report the original"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// C-90: a multi-token grant pins the leading subcommand, so a read-shaped grant
    /// (`kubectl get`) is structurally unable to run a mutation (`kubectl delete …`) — on both
    /// the one-shot and the background spawn path.
    #[tokio::test]
    async fn process_grant_pins_leading_arguments() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-argcaps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        let caps = SystemHostCaps::new(sys.clone()).with_grants(PluginCapabilities {
            process: vec!["kubectl get".into(), "printf ok".into()],
            ..Default::default()
        });
        assert!(
            caps.handle(
                "process.run",
                &json!({"argv": ["kubectl", "delete", "pod", "x"]})
            )
            .await
            .is_err(),
            "a grant of `kubectl get` must deny `kubectl delete`"
        );
        assert!(
            caps.handle("process.run", &json!({"argv": ["kubectl"]}))
                .await
                .is_err(),
            "argv shorter than every grant prefix must be denied"
        );
        assert!(
            caps.handle(
                "process.spawn",
                &json!({"argv": ["kubectl", "delete", "pod", "x"]})
            )
            .await
            .is_err(),
            "process.spawn must apply the same argument gate"
        );
        // The allowed shape actually runs (printf is argv-executable on every CI runner).
        let out = caps
            .handle("process.run", &json!({"argv": ["printf", "ok"]}))
            .await
            .expect("a matching argv prefix must be admitted");
        assert_eq!(out["exit_code"], 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- C-09a piece 1: the `internal`/host-only op flag ------------------------------------------
    // An op marked `internal: true` is NOT advertised to the LLM as a callable tool — it is a
    // host-only channel (the aws-bedrock plugin's `auth` op returning raw AWS keys is the canonical
    // case: the model must never call it, or the keys would appear in the tool result). The op stays
    // dispatchable by the host (via the shared `PluginHost` handle, like the broker calls
    // `endpoint.discover`); only the *projection* as an agent tool is suppressed.

    #[test]
    fn internal_op_is_not_projected_as_a_tool() {
        // Failing-first for C-09a piece 1: before the `internal` flag existed every manifest op
        // became an LLM-callable tool, so an `auth` op returning raw keys would be model-callable.
        let manifest = PluginManifest {
            name: "aws-bedrock".into(),
            operations: vec![
                OperationSpec {
                    name: "aws-bedrock.chat".into(),
                    description: "run a bedrock turn".into(),
                    ..Default::default()
                },
                OperationSpec {
                    name: "aws-bedrock.auth".into(),
                    description: "resolve AWS creds (host-only)".into(),
                    internal: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let visible: Vec<&str> = visible_ops(&manifest).map(|o| o.name.as_str()).collect();
        assert_eq!(
            visible,
            vec!["aws-bedrock.chat"],
            "only the non-internal op projects"
        );
        assert!(
            !visible.contains(&"aws-bedrock.auth"),
            "the internal `auth` op must NOT be advertised to the LLM"
        );
    }

    #[test]
    fn duplicate_manifest_operations_are_rejected_before_projection() {
        let operation = OperationSpec {
            name: "acme.get".into(),
            description: "read a thing".into(),
            ..Default::default()
        };
        let manifest = PluginManifest {
            name: "acme".into(),
            operations: vec![operation.clone(), operation],
            ..Default::default()
        };

        let error = validate_manifest_operations(&manifest)
            .unwrap_err()
            .to_string();
        assert!(error.contains("duplicate operation `acme.get`"));
    }

    #[test]
    fn qualified_and_unqualified_manifest_aliases_cannot_share_a_public_name() {
        let manifest = PluginManifest {
            name: "acme".into(),
            operations: vec![
                OperationSpec {
                    name: "get".into(),
                    ..Default::default()
                },
                OperationSpec {
                    name: "acme.get".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let error = validate_manifest_operations(&manifest)
            .unwrap_err()
            .to_string();
        assert!(error.contains("both project as `acme.get`"));
    }

    #[test]
    fn explicit_public_aliases_are_nonblank_and_unique() {
        let aliased = |name: &str, public_name: &str| OperationSpec {
            name: name.into(),
            public_name: Some(public_name.into()),
            ..Default::default()
        };
        let duplicate = PluginManifest {
            name: "acme".into(),
            operations: vec![
                aliased("acme.first", "shared.search"),
                aliased("acme.second", "shared.search"),
            ],
            ..Default::default()
        };
        assert!(validate_manifest_operations(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("both project as `shared.search`"));

        let blank = PluginManifest {
            name: "acme".into(),
            operations: vec![aliased("acme.search", "  ")],
            ..Default::default()
        };
        assert!(validate_manifest_operations(&blank)
            .unwrap_err()
            .to_string()
            .contains("blank public name"));
    }

    #[test]
    fn internal_flag_defaults_false_so_existing_plugins_unchanged() {
        // Backwards compat: a manifest that says nothing about `internal` (every existing plugin)
        // projects all its ops — the flag is opt-in, not a behavior change for current manifests.
        let op = serde_json::from_value::<OperationSpec>(serde_json::json!({
            "name": "kubernetes.pod.list",
            "description": "list pods"
        }))
        .unwrap();
        assert!(!op.internal);
    }

    /// Every projected plugin op must survive the registry's authority-contract validation, or the
    /// plugin cannot load at all — registration is all-or-nothing, so one invalid op takes down the
    /// whole session. The CLI-driven plugins (`kubernetes` via `kubectl`, `aws` via `aws`) declare
    /// process capability only, and their ops legitimately reach the network and mutate remote
    /// state; the effect-less default projection is `[Process, Network]` for the same reason. All
    /// three shapes must be valid contracts against process-only capabilities.
    #[test]
    fn process_only_capabilities_project_valid_authority_contracts() {
        let caps = PluginCapabilities {
            process: vec!["kubectl".into()],
            ..Default::default()
        };
        let cases = [
            // The declared shapes in `plugins/kubernetes`.
            vec![Effect::Read, Effect::Network],
            vec![Effect::Write, Effect::Network],
            vec![Effect::Process, Effect::Network],
            // The conservative fallback `plugin_tool_spec` applies when an op declares none.
            vec![],
        ];

        for effects in cases {
            let op = OperationSpec {
                name: "kubernetes.deployment.scale".into(),
                description: "scale a deployment".into(),
                effects: effects.clone(),
                ..Default::default()
            };
            let (_, spec) = plugin_tool_spec("kubernetes", &op, &caps);
            let requirements = flux_runtime::authority_requirements_from_declaration(
                &spec,
                &["kubectl".into()],
                &[],
            )
            .unwrap_or_else(|err| panic!("effects {effects:?} must be a valid contract: {err}"));
            assert!(
                requirements
                    .iter()
                    .any(|req| req.action.0 == "process.exec"),
                "effects {effects:?} must be gated by the named program",
            );
        }
    }

    /// C-309: the effect-less `[Process, Network]` default and `flux-runtime`'s authority contract
    /// are two safety mechanisms that must not compose into an unsatisfiable requirement. The
    /// contract refuses any tool declaring an effect it holds no matching access for, so before
    /// this fix every effect-less op of a plugin without a `process` capability was **impossible to
    /// load** — which is how `flux-sdk`'s fixture plugin sat red behind a feature no gate compiled.
    ///
    /// The fix is on the ACCESS side, and the direction matters: authority requirements derive from
    /// `access`, not `effects` (`authority_requirements_from_declaration`), so relaxing the effects
    /// default instead would have projected an op with neither — carrying no requirement at all and
    /// skipping the authorization floor. Pin both halves here.
    #[test]
    fn every_plugin_op_projects_a_loadable_and_gated_authority_contract() {
        let op = OperationSpec {
            name: "pure".into(),
            description: "declares no effects".into(),
            ..Default::default()
        };
        let cases = [
            ("nothing granted", PluginCapabilities::default()),
            (
                "http only",
                PluginCapabilities {
                    http: true,
                    ..Default::default()
                },
            ),
            (
                "process only",
                PluginCapabilities {
                    process: vec!["kubectl".into()],
                    ..Default::default()
                },
            ),
        ];

        for (label, caps) in cases {
            let (_, spec) = plugin_tool_spec("acme", &op, &caps);
            let requirements =
                flux_runtime::authority_requirements_from_declaration(&spec, &caps.process, &[])
                    .unwrap_or_else(|err| {
                        panic!("`{label}` must project a loadable authority contract: {err}")
                    });
            // Loadable is only half of it — an op that loads while requiring nothing would slip the
            // floor, which is the failure mode the effects-side fix would have introduced.
            assert!(
                requirements
                    .iter()
                    .any(|req| req.action.0 == "process.exec"),
                "`{label}` must still be gated on dispatching to its subprocess: {requirements:?}",
            );
        }
    }

    /// C-191: a plugin's `effects` / `risk` / `idempotency` are authored outside this repo and are
    /// then trusted verbatim by every approval gate. A manifest that declares a mutating operation
    /// while keeping the read-only risk class is named at load.
    #[test]
    fn a_mis_declared_plugin_operation_is_reported_at_load() {
        let drifted = OperationSpec {
            name: "acme.deploy".into(),
            description: "ship the current build".into(),
            effects: vec![Effect::Write, Effect::Network],
            risk: Some(Risk::Low),
            idempotency: Some(Idempotency::NonIdempotent),
            ..Default::default()
        };
        let manifest = PluginManifest {
            name: "acme".into(),
            operations: vec![drifted.clone()],
            ..Default::default()
        };

        let warnings = op_coherence_warnings(&manifest);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("acme.deploy"), "{warnings:?}");
        assert!(warnings[0].starts_with("I1 "), "{warnings:?}");

        // Raising the tier — the correction the manifest actually needs — clears it. The rule is a
        // floor on the declaration, not a ban on plugins that mutate.
        let corrected = PluginManifest {
            name: "acme".into(),
            operations: vec![OperationSpec {
                risk: Some(Risk::Medium),
                ..drifted
            }],
            ..Default::default()
        };
        assert!(op_coherence_warnings(&corrected).is_empty());
    }

    /// The projection is what gets checked, not the raw manifest — `access` comes from the
    /// *plugin's* capabilities plus the unconditional `Process` every plugin op carries (C-309),
    /// and an op that declares no effects is defaulted to `[Process, Network]`. Both are
    /// properties of `plugin_tool_spec`, so this pins that the coherence pass sees them (C-191).
    #[test]
    fn plugin_coherence_reads_the_projected_spec_not_the_raw_declaration() {
        // Declares nothing: `plugin_tool_spec` defaults it to `[Process, Network]`, which is
        // consequence-bearing, so `risk = "low"` understates it.
        let undeclared = OperationSpec {
            name: "acme.run".into(),
            description: "run something".into(),
            risk: Some(Risk::Low),
            idempotency: Some(Idempotency::NonIdempotent),
            ..Default::default()
        };
        let warnings = op_coherence_warnings(&PluginManifest {
            name: "acme".into(),
            operations: vec![undeclared],
            ..Default::default()
        });
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("I1 "), "{warnings:?}");

        // A declared read under a `process`-capable plugin projects `AccessKind::Process` from the
        // plugin's capabilities, not the op's. That must NOT be read as a consequence, or every
        // read op of a process-capable plugin would be flagged.
        let read_under_process_caps = PluginManifest {
            name: "kubernetes".into(),
            capabilities: PluginCapabilities {
                process: vec!["kubectl".into()],
                ..Default::default()
            },
            operations: vec![OperationSpec {
                name: "kubernetes.pod.list".into(),
                description: "list pods".into(),
                effects: vec![Effect::Read],
                risk: Some(Risk::Low),
                idempotency: Some(Idempotency::Idempotent),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(op_coherence_warnings(&read_under_process_caps).is_empty());
    }

    /// An `internal: true` op never becomes an agent tool, so it never reaches the gates these
    /// invariants protect and is deliberately out of scope for the check.
    #[test]
    fn plugin_coherence_skips_host_only_operations() {
        let manifest = PluginManifest {
            name: "acme".into(),
            operations: vec![OperationSpec {
                name: "acme.auth".into(),
                description: "resolve credentials (host-only)".into(),
                internal: true,
                effects: vec![Effect::Write, Effect::Network],
                risk: Some(Risk::Low),
                idempotency: Some(Idempotency::Idempotent),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(op_coherence_warnings(&manifest).is_empty());
    }

    // --- C-310 refresh guards ---------------------------------------------------------------

    /// The load-time grant every `capability_widenings` case below is measured against — one
    /// non-empty entry in each family so a widening is expressible everywhere.
    fn granted_capabilities() -> PluginCapabilities {
        PluginCapabilities {
            process: vec!["kubectl get".into()],
            secrets: vec!["API_TOKEN".into()],
            http: true,
            http_hosts: vec!["api.example.com".into()],
            private_hosts: vec!["internal.example.com".into()],
            conn: vec!["tcp:db.example.com:5432".into()],
            fs: vec![flux_plugin_protocol::FsReadScope {
                path: "~/.aws/config".into(),
                secret: false,
            }],
            ..PluginCapabilities::default()
        }
    }

    /// A refresh that restates the grant verbatim is not a widening — the common case, where only
    /// the op set moved.
    #[test]
    fn an_unchanged_capability_set_is_not_a_widening() {
        assert!(capability_widenings(&granted_capabilities(), &granted_capabilities()).is_empty());
    }

    /// Every capability family the plugin host gates on, widened one at a time. Each must be caught
    /// and each must name itself — an unnamed refusal is one an operator cannot act on.
    ///
    /// The cases below are built with `..granted.clone()`, which would keep compiling if an
    /// eleventh field were added to [`PluginCapabilities`], so the destructure at the end of this
    /// test is what actually reds the build — deliberately mirroring the one in
    /// `capability_widenings` itself.
    #[test]
    fn every_capability_family_is_checked_for_widening() {
        let granted = granted_capabilities();
        let cases: Vec<(&str, PluginCapabilities)> = vec![
            (
                "process",
                PluginCapabilities {
                    // Broadening the argv prefix: `kubectl` admits `kubectl delete …`.
                    process: vec!["kubectl".into()],
                    ..granted.clone()
                },
            ),
            (
                "secrets",
                PluginCapabilities {
                    secrets: vec!["API_TOKEN".into(), "DEPLOY_KEY".into()],
                    ..granted.clone()
                },
            ),
            (
                "http_hosts",
                PluginCapabilities {
                    http_hosts: vec!["api.example.com".into(), "*.attacker.test".into()],
                    ..granted.clone()
                },
            ),
            (
                "private_hosts",
                PluginCapabilities {
                    private_hosts: vec!["10.0.0.1".into()],
                    ..granted.clone()
                },
            ),
            (
                "conn",
                PluginCapabilities {
                    conn: vec!["tcp:*:5432".into()],
                    ..granted.clone()
                },
            ),
            (
                "fs",
                PluginCapabilities {
                    fs: vec![flux_plugin_protocol::FsReadScope {
                        path: "~/.ssh/**".into(),
                        secret: false,
                    }],
                    ..granted.clone()
                },
            ),
            (
                "blob",
                PluginCapabilities {
                    blob: true,
                    ..granted.clone()
                },
            ),
            (
                "discover",
                PluginCapabilities {
                    discover: true,
                    ..granted.clone()
                },
            ),
            (
                "credential",
                PluginCapabilities {
                    credential: true,
                    ..granted.clone()
                },
            ),
        ];
        for (family, refreshed) in cases {
            let widenings = capability_widenings(&granted, &refreshed);
            assert!(
                widenings.iter().any(|w| w.contains(family)),
                "widening `{family}` must be refused and named; got {widenings:?}"
            );
        }
        // `http` is already granted here, so prove the false→true flip separately.
        let http_off = PluginCapabilities {
            http: false,
            ..granted.clone()
        };
        assert!(capability_widenings(&http_off, &granted)
            .iter()
            .any(|w| w.contains("http")));

        // The exhaustiveness anchor. Adding a field to `PluginCapabilities` reds HERE, which is the
        // prompt to add a case to `cases` above — the `..granted.clone()` spread would otherwise
        // absorb a new authority family in silence.
        let PluginCapabilities {
            process: _,
            secrets: _,
            http: _,
            http_hosts: _,
            private_hosts: _,
            conn: _,
            blob: _,
            discover: _,
            credential: _,
            fs: _,
        } = granted;
    }

    /// Turning a granted `fs` scope's `secret` flag off would stop its contents being registered
    /// with the Redactor — a widening of what can leak, not a narrowing.
    #[test]
    fn dropping_an_fs_scopes_secret_flag_is_a_widening() {
        let granted = PluginCapabilities {
            fs: vec![flux_plugin_protocol::FsReadScope {
                path: "~/.aws/sso/cache/**".into(),
                secret: true,
            }],
            ..PluginCapabilities::default()
        };
        let refreshed = PluginCapabilities {
            fs: vec![flux_plugin_protocol::FsReadScope {
                path: "~/.aws/sso/cache/**".into(),
                secret: false,
            }],
            ..PluginCapabilities::default()
        };
        assert!(!capability_widenings(&granted, &refreshed).is_empty());
    }

    /// Giving a capability *back* is always allowed — the refusal is one-directional.
    #[test]
    fn surrendering_capabilities_is_never_a_widening() {
        assert!(
            capability_widenings(&granted_capabilities(), &PluginCapabilities::default())
                .is_empty()
        );
    }

    /// An operation keeping its name may not shed the scope it was gated under. Each of these is
    /// a way the same `permission_subjects` string would come to mean something laxer.
    #[test]
    fn a_retained_op_may_not_weaken_its_gating_scope() {
        let caps = granted_capabilities();
        let granted_op = OperationSpec {
            name: "deploy".into(),
            description: "roll out".into(),
            effects: vec![Effect::Write, Effect::Process],
            risk: Some(Risk::Destructive),
            secret_purposes: vec!["api_token".into()],
            process: vec!["kubectl get".into()],
            ..Default::default()
        };

        let weaker = |op: OperationSpec| op_scope_weakenings("acme", &caps, &granted_op, &op);

        assert!(
            weaker(OperationSpec {
                risk: Some(Risk::Low),
                ..granted_op.clone()
            })
            .iter()
            .any(|w| w.contains("risk")),
            "a risk downgrade under a stable name"
        );
        assert!(
            weaker(OperationSpec {
                effects: vec![Effect::Read],
                ..granted_op.clone()
            })
            .iter()
            .any(|w| w.contains("effect")),
            "shedding a declared effect"
        );
        assert!(
            weaker(OperationSpec {
                secret_purposes: Vec::new(),
                ..granted_op.clone()
            })
            .iter()
            .any(|w| w.contains("secret purpose")),
            "shedding a declared secret purpose"
        );
        assert!(
            weaker(OperationSpec {
                process: Vec::new(),
                ..granted_op.clone()
            })
            .iter()
            .any(|w| w.contains("process")),
            "dropping the per-op process narrowing widens it to the manifest grant"
        );
        assert!(
            weaker(OperationSpec {
                public_name: Some("acme.deploy".into()),
                name: "deploy_v2".into(),
                ..granted_op.clone()
            })
            .iter()
            .any(|w| w.contains("dispatches to")),
            "re-pointing the dispatch identity behind a stable public name"
        );

        // The identity case, and the two legitimate directions: tightening the tier, and narrowing
        // the per-op process grant further.
        assert!(weaker(granted_op.clone()).is_empty());
        assert!(weaker(OperationSpec {
            process: vec!["kubectl get".into()],
            risk: Some(Risk::Destructive),
            ..granted_op.clone()
        })
        .is_empty());
    }

    #[test]
    fn plugin_operation_group_projects_to_tool_spec() {
        let op = OperationSpec {
            name: "vault.kv.read".into(),
            description: "read kv".into(),
            group: Some("vault.kv".into()),
            effects: vec![Effect::Read],
            risk: Some(Risk::Low),
            idempotency: Some(Idempotency::Idempotent),
            ..Default::default()
        };
        let (_, spec) = plugin_tool_spec("vault", &op, &PluginCapabilities::default());
        assert_eq!(spec.name, "vault.kv.read");
        assert_eq!(spec.group.as_deref(), Some("vault.kv"));
    }

    #[test]
    fn operation_public_name_projects_a_compatibility_alias() {
        let op = OperationSpec {
            name: "websearch.search".into(),
            public_name: Some("web.search".into()),
            ..Default::default()
        };

        let (dispatch_name, spec) =
            plugin_tool_spec("websearch", &op, &PluginCapabilities::default());
        assert_eq!(dispatch_name, "websearch.search");
        assert_eq!(spec.name, "web.search");
    }

    /// D-164 failing-first: output documentation belongs to the plugin manifest and must survive
    /// both the subprocess wire and the agent-tool projection. Legacy manifests omit the field.
    #[test]
    fn operation_output_schema_round_trips_and_defaults_for_legacy_manifests() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        });
        let op = serde_json::from_value::<OperationSpec>(serde_json::json!({
            "name": "tickets.create",
            "output_schema": schema
        }))
        .unwrap();
        assert_eq!(op.output_schema.as_ref(), Some(&schema));

        let encoded = serde_json::to_value(&op).unwrap();
        assert_eq!(encoded["output_schema"], schema);

        let legacy = serde_json::from_value::<OperationSpec>(serde_json::json!({
            "name": "tickets.list"
        }))
        .unwrap();
        assert!(legacy.output_schema.is_none());
        assert!(serde_json::to_value(legacy)
            .unwrap()
            .get("output_schema")
            .is_none());
    }

    #[test]
    fn plugin_operation_output_schema_projects_to_tool_spec() {
        let schema = serde_json::json!({ "type": "array", "items": { "type": "string" } });
        let op = OperationSpec {
            name: "tickets.labels".into(),
            output_schema: Some(schema.clone()),
            ..Default::default()
        };
        let (_, spec) = plugin_tool_spec("tickets", &op, &PluginCapabilities::default());
        assert_eq!(spec.output_schema, Some(schema));
    }

    /// D-138: a manifest-declared `OperationSpec::semantic_effects` (`Money`) projects onto the
    /// plain tag strings [`Tool::semantic_effects`] returns — the plugin-side half of the
    /// manifest→catalog adapter (`flux-flow`'s `OpRegistry` parses these tags back onto
    /// `OpSignature::semantic_effects`; see `analyze::tests::
    /// annotate_effects_folds_catalog_declared_semantics_without_an_authored_tag` for the catalog
    /// side).
    #[test]
    fn operation_spec_semantic_effects_project_onto_tag_strings() {
        let op = OperationSpec {
            name: "billing.charge".into(),
            description: "charge a customer".into(),
            effects: vec![Effect::Network],
            semantic_effects: vec![FlowEffect::Money],
            ..Default::default()
        };
        assert_eq!(semantic_effect_tags(&op.semantic_effects), vec!["money"]);
    }

    #[test]
    fn plugin_manifest_groups_are_backward_compatible() {
        let legacy = serde_json::from_value::<PluginManifest>(serde_json::json!({
            "name": "legacy",
            "operations": [{"name": "legacy.ping"}]
        }))
        .unwrap();
        assert!(legacy.groups.is_empty());
        assert!(legacy.operations[0].group.is_none());

        let grouped = serde_json::from_value::<PluginManifest>(serde_json::json!({
            "name": "vault",
            "operations": [{"name": "vault.kv.read", "group": "vault.kv"}],
            "groups": [{
                "name": "vault.kv",
                "description": "Vault KV-v2",
                "tools": ["vault.kv.read"],
                "surface_when": []
            }]
        }))
        .unwrap();
        assert_eq!(grouped.groups[0].name, "vault.kv");
        assert!(grouped.groups[0].surface_when.is_empty());
        assert_eq!(grouped.operations[0].group.as_deref(), Some("vault.kv"));
    }

    // --- C-09a piece 2: the path-scoped deny-by-default `fs.read` capability ----------------------
    // For the aws-bedrock plugin to read `~/.aws/config` + `~/.aws/sso/cache` (the SSO refresh-token
    // cache) without an `aws` CLI. These are HOST paths outside the workspace jail, so they can't go
    // through `System::read_file`; the capability has its own manifest-declared scope, denies anything
    // out of scope, rejects `..` traversal, and registers secret-bearing reads with the Redactor.

    #[tokio::test]
    async fn fs_read_denies_out_of_scope_paths() {
        let dir = std::env::temp_dir().join(format!("flux-fs-deny-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("aws/config");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"[default]").unwrap();
        let outside = dir.join("secret.txt");
        std::fs::write(&outside, b"TOPSECRET").unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                path: format!("{}/aws/config", dir.display()),
                secret: false,
            }],
            ..Default::default()
        });

        // In-scope: allowed.
        assert!(
            caps.handle(
                "fs.read",
                &serde_json::json!({"path": target.to_str().unwrap()})
            )
            .await
            .is_ok(),
            "in-scope fs.read must be allowed"
        );
        // Out-of-scope: denied (deny-by-default — not a silent empty read).
        let err = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": outside.to_str().unwrap()}),
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("not in this plugin's fs.read scope"),
            "out-of-scope read must be denied with a clear error, got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_read_denies_symlink_escape_from_declared_scope() {
        let dir = std::env::temp_dir().join(format!("flux-fs-symlink-deny-{}", std::process::id()));
        let granted = dir.join("granted");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOPSECRET").unwrap();
        std::os::unix::fs::symlink(&outside, granted.join("alias")).unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                path: format!("{}/**", granted.display()),
                secret: false,
            }],
            ..Default::default()
        });

        let requested = granted.join("alias/secret.txt");
        let err = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": requested.to_str().unwrap()}),
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("not in this plugin's fs.read scope"),
            "symlink escape must be denied by physical path identity, got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_read_allows_symlink_that_resolves_inside_declared_scope() {
        let dir =
            std::env::temp_dir().join(format!("flux-fs-symlink-allow-{}", std::process::id()));
        let granted = dir.join("granted");
        std::fs::create_dir_all(granted.join("real")).unwrap();
        std::fs::write(granted.join("real/config"), "safe").unwrap();
        std::os::unix::fs::symlink("real", granted.join("alias")).unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                path: format!("{}/**", granted.display()),
                secret: false,
            }],
            ..Default::default()
        });

        let requested = granted.join("alias/config");
        let got = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": requested.to_str().unwrap()}),
            )
            .await
            .unwrap();
        assert_eq!(got["body"], "safe");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fs_read_recursive_glob_matches_nested_files() {
        let dir = std::env::temp_dir().join(format!("flux-fs-glob-{}", std::process::id()));
        let cache = dir.join("aws/sso/cache");
        std::fs::create_dir_all(&cache).unwrap();
        let token_file = cache.join("abc.json");
        std::fs::write(&token_file, b"{\"refreshToken\":\"rt\"}").unwrap();
        let nested = cache.join("sub/deep.json");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, b"{}").unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                // `/**` matches the dir itself + everything under it (incl. nested subdirs).
                path: format!("{}/aws/sso/cache/**", dir.display()),
                secret: true,
            }],
            ..Default::default()
        });

        let got = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": token_file.to_str().unwrap()}),
            )
            .await
            .unwrap();
        assert_eq!(got["body"], "{\"refreshToken\":\"rt\"}");
        // A nested file under the cache dir also matches.
        assert!(
            caps.handle(
                "fs.read",
                &serde_json::json!({"path": nested.to_str().unwrap()})
            )
            .await
            .is_ok(),
            "`/**` must match nested files"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fs_read_secret_scope_registers_content_with_redactor() {
        // A `secret: true` scope's content is registered with the SecretSink (the executor's
        // Redactor) so that if it ever flows into model-visible output it is scrubbed. This is the
        // `~/.aws/sso/cache` refresh-token privilege boundary.
        let dir = std::env::temp_dir().join(format!("flux-fs-secret-{}", std::process::id()));
        let cache = dir.join("aws/sso/cache");
        std::fs::create_dir_all(&cache).unwrap();
        let token_file = cache.join("token.json");
        // The full file content is what `fs.read` registers with the Redactor (the capability
        // registers what it READ; the consumer — the aws-bedrock plugin in C-09b — registers the
        // specific secrets it EXTRACTS). So the redaction guarantee is: if the raw file content is
        // ever echoed into model-visible output, it is scrubbed.
        let file_content = "{\"accessToken\":\"super-secret-refresh-token-xyz\"}";
        std::fs::write(&token_file, file_content).unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let redactor = flux_secret::Redactor::new();
        let sink = Arc::new(RedactorSink {
            redactor: redactor.clone(),
        });
        let caps = SystemHostCaps::new(sys)
            .with_grants(PluginCapabilities {
                fs: vec![FsReadScope {
                    path: format!("{}/aws/sso/cache/**", dir.display()),
                    secret: true,
                }],
                ..Default::default()
            })
            .with_secret_sink(sink);

        let _ = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": token_file.to_str().unwrap()}),
            )
            .await
            .unwrap();

        // The refresh-token value the host just read must be registered with the Redactor — so a
        // later capture that echoes it back is scrubbed, not leaked.
        let leaked = format!("the cache file reads: {file_content}");
        let scrubbed = redactor.redact(&leaked);
        assert_ne!(
            &scrubbed, &leaked,
            "secret fs.read content must be redactor-registered"
        );
        assert!(!scrubbed.contains(file_content));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fs_read_rejects_path_traversal_even_when_pattern_could_match() {
        let dir = std::env::temp_dir().join(format!("flux-fs-trav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outside = dir.parent().unwrap().join("flux-fs-trav-sentinel");
        std::fs::write(&outside, b"keep me").unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        // A wildcard scope that, naively joined, could reach outside via `..`.
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                path: format!("{}/aws/**", dir.display()),
                secret: false,
            }],
            ..Default::default()
        });

        let traversal = format!("{}/aws/../../flux-fs-trav-sentinel", dir.display());
        let err = caps
            .handle("fs.read", &serde_json::json!({"path": &traversal}))
            .await
            .unwrap_err();
        assert!(
            err.contains("traversal") || err.contains("not in this plugin's fs.read scope"),
            "`..` traversal must be rejected, got: {err}"
        );
        // The sentinel outside the scope is untouched.
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep me");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_file(&outside).ok();
    }

    #[tokio::test]
    async fn fs_read_returns_binary_as_base64() {
        let dir = std::env::temp_dir().join(format!("flux-fs-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin_file = dir.join("aws/blob");
        std::fs::create_dir_all(bin_file.parent().unwrap()).unwrap();
        std::fs::write(&bin_file, [0u8, 255, 0, 128]).unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                path: format!("{}/aws/**", dir.display()),
                secret: false,
            }],
            ..Default::default()
        });

        let got = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": bin_file.to_str().unwrap()}),
            )
            .await
            .unwrap();
        // Binary (NUL-bearing) content comes back base64-encoded, not as a UTF-8-mangled `body`.
        assert!(
            got.get("body_b64").is_some(),
            "binary read must return body_b64"
        );
        assert!(got.get("body").is_none());
        assert_eq!(got["size"], 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn secret_by_purpose_and_endpoint_resolution() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-purpose-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        // Unique env keys so the process-global set_var doesn't collide with other tests.
        std::env::set_var("FLUX_TEST_API_TOKEN_XZ", "s3cr3t");
        std::env::set_var("FLUX_TEST_GITLAB_URL_XZ", "https://gl.example.com");

        let manifest = PluginManifest {
            name: "gl".into(),
            auth: vec![AuthMethod {
                purpose: "api_token".into(),
                env: vec!["FLUX_TEST_API_TOKEN_XZ".into()],
                description: String::new(),
                ..Default::default()
            }],
            endpoints: vec![EndpointSpec {
                name: "gitlab.endpoint".into(),
                env: vec!["FLUX_TEST_GITLAB_URL_XZ".into()],
                http_hosts: vec!["gl.example.com".into()],
                ..Default::default()
            }],
            capabilities: PluginCapabilities {
                secrets: vec!["FLUX_TEST_API_TOKEN_XZ".into()],
                http: true,
                http_hosts: vec!["gl.example.com".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys).with_manifest(&manifest);

        // secret-by-purpose resolves the granted env key
        let got = caps
            .handle("secret", &json!({"purpose": "api_token"}))
            .await
            .unwrap();
        assert_eq!(got["value"], "s3cr3t");
        // endpoint resolves from its declared env — HOST-SIDE ONLY (it feeds the ref-based IO
        // paths). The `endpoint` URL-handback capability itself is retired (D-32): a plugin can
        // never ask the host for the resolved URL string.
        assert_eq!(
            caps.resolve_endpoint("gitlab.endpoint").unwrap(),
            "https://gl.example.com"
        );
        let err = caps
            .handle("endpoint", &json!({"name": "gitlab.endpoint"}))
            .await
            .unwrap_err();
        assert!(
            err.contains("unknown host capability"),
            "the URL-handback must be gone, not just failing: {err}"
        );
        // an undeclared purpose is denied
        assert!(caps
            .handle("secret", &json!({"purpose": "nope"}))
            .await
            .is_err());

        std::env::remove_var("FLUX_TEST_API_TOKEN_XZ");
        std::env::remove_var("FLUX_TEST_GITLAB_URL_XZ");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-81: an OAuth2-backed purpose resolves a fresh bearer from the credential store (keyed by
    /// `plugin:<name>:<purpose>`); with no stored token it falls back to the declared env secret. The
    /// token endpoint is built from the DECLARED endpoint and still passes the manifest host check
    /// before credential lookup. DNS guard/pinning is lazy: absent/fresh tokens stay offline, while
    /// stale refresh still fails closed. (The store→refresh mechanics themselves are covered in
    /// flux-credentials.)
    #[tokio::test]
    async fn oauth2_purpose_resolves_stored_bearer_else_env_fallback() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-oauth-d81-{}", std::process::id()));
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::set_var("HOME", &home);
        std::env::set_var("FLUX_TEST_OAUTH_ENV", "env-fallback-tok");

        let mut method = AuthMethod::oauth2(
            "api",
            OAuth2Spec {
                endpoint: "api".into(),
                token_path: "/oauth/token".into(),
                client_id: "cid".into(),
                grants: vec![OAuthGrant::RefreshToken],
                ..Default::default()
            },
        );
        method.env = vec!["FLUX_TEST_OAUTH_ENV".into()];
        let manifest = PluginManifest {
            name: "acme".into(),
            auth: vec![method],
            endpoints: vec![EndpointSpec {
                name: "api".into(),
                default: Some("https://api.example.com".into()),
                http_hosts: vec!["api.example.com".into()],
                ..Default::default()
            }],
            capabilities: PluginCapabilities {
                secrets: vec!["FLUX_TEST_OAUTH_ENV".into()],
                http: true,
                http_hosts: vec!["api.example.com".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let resolver = Arc::new(SequenceResolver::new(&[]));
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_host_resolver(resolver.clone());

        // No stored token → env fallback.
        let got = caps
            .handle("secret", &json!({"purpose": "api"}))
            .await
            .unwrap();
        assert_eq!(got["value"], "env-fallback-tok");
        assert_eq!(
            resolver.calls(),
            0,
            "an absent stored token must not resolve the refresh endpoint"
        );

        // A stored token (no expiry → never auto-refreshes) is returned as the bearer, over env.
        flux_credentials::save_token(
            "plugin:acme:api",
            &flux_credentials::OAuthToken {
                access: "stored-bearer".into(),
                refresh: Some("rt".into()),
                expires_at_ms: None,
                account_id: None,
            },
        )
        .unwrap();
        let got = caps
            .handle("secret", &json!({"purpose": "api"}))
            .await
            .unwrap();
        assert_eq!(got["value"], "stored-bearer");
        assert_eq!(
            resolver.calls(),
            0,
            "a fresh stored token must not resolve the refresh endpoint"
        );

        std::env::remove_var("HOME");
        std::env::remove_var("FLUX_TEST_OAUTH_ENV");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-257: OAuth refresh uses the same pinned transport as plugin HTTP instead of constructing a
    /// fresh client from the token URL. The fake hostname can reach the loopback token stub only
    /// through the guard's pin, and the resolver's metadata answer is never requested.
    #[tokio::test]
    async fn oauth_refresh_consumes_the_guard_vetted_address() {
        use async_trait::async_trait;
        use flux_system::{System, Workspace};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        struct RefreshStore(std::sync::Mutex<flux_credentials::OAuthToken>);
        #[async_trait]
        impl flux_credentials::CredentialStore for RefreshStore {
            async fn load(&self, _key: &str) -> Option<flux_credentials::OAuthToken> {
                Some(self.0.lock().unwrap().clone())
            }

            async fn save(
                &self,
                _key: &str,
                token: &flux_credentials::OAuthToken,
            ) -> flux_core::Result<()> {
                *self.0.lock().unwrap() = token.clone();
                Ok(())
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let body = r#"{"access_token":"fresh-pinned","expires_in":3600}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let dir = std::env::temp_dir().join(format!("flux-oauth-pin-c257-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let resolver = Arc::new(SequenceResolver::new(&["127.0.0.1", "169.254.169.254"]));
        let store = Arc::new(RefreshStore(std::sync::Mutex::new(
            flux_credentials::OAuthToken {
                access: "stale".into(),
                refresh: Some("refresh-secret".into()),
                expires_at_ms: Some(0),
                account_id: None,
            },
        )));
        let manifest = PluginManifest {
            name: "oauth-pin".into(),
            auth: vec![AuthMethod::oauth2(
                "api",
                OAuth2Spec {
                    endpoint: "api".into(),
                    token_path: "/oauth/token".into(),
                    client_id: "cid".into(),
                    grants: vec![OAuthGrant::RefreshToken],
                    ..Default::default()
                },
            )],
            endpoints: vec![EndpointSpec {
                name: "api".into(),
                default: Some(format!("http://oauth-pin.test:{port}")),
                http_hosts: vec!["oauth-pin.test".into()],
                ..Default::default()
            }],
            capabilities: PluginCapabilities {
                http: true,
                http_hosts: vec!["oauth-pin.test".into()],
                private_hosts: vec!["oauth-pin.test".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_host_resolver(resolver.clone())
            .with_private_net_grants(vec!["oauth-pin.test".into()])
            .with_credential_store(store);

        let bearer = caps
            .handle("secret", &json!({"purpose": "api"}))
            .await
            .unwrap();
        assert_eq!(bearer["value"], "fresh-pinned");
        server.await.unwrap();
        assert_eq!(resolver.calls(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-125: the ref-based `http.do` join must APPEND the op path onto the resolved endpoint
    /// base. RFC-3986 `Url::join` semantics made a leading-slash path REPLACE a path-bearing
    /// base's path (slack's `https://slack.com/api` + `/auth.test` → `https://slack.com/auth.test`),
    /// 404ing every op of any plugin whose endpoint base carries a path segment.
    #[test]
    fn compose_url_appends_path_onto_path_bearing_base() {
        // The slack shape: path-bearing base + leading-slash op path.
        assert_eq!(
            compose_url("https://slack.com/api", Some("/auth.test")).unwrap(),
            "https://slack.com/api/auth.test"
        );
        // Host-only base + absolute path (the gitlab shape) — unchanged by the fix.
        assert_eq!(
            compose_url("https://gitlab.com", Some("/api/v4/projects")).unwrap(),
            "https://gitlab.com/api/v4/projects"
        );
        // Trailing-slash base + relative path — exactly one separating slash.
        assert_eq!(
            compose_url("https://api.example.com/v1/", Some("query")).unwrap(),
            "https://api.example.com/v1/query"
        );
        // None/empty path returns the base unchanged.
        assert_eq!(
            compose_url("https://api.example.com/v1", None).unwrap(),
            "https://api.example.com/v1"
        );
        assert_eq!(
            compose_url("https://api.example.com/v1", Some("")).unwrap(),
            "https://api.example.com/v1"
        );
        // A non-URL base still errors (the endpoint binding is broken, not the path).
        assert!(compose_url("not a url", Some("/x")).is_err());
    }

    /// D-126: a PLAIN (non-OAuth2) auth method resolves a stored bearer (`flux auth set`) when
    /// present — the stored token wins over the declared env keys, matching the OAuth2 store-first
    /// rule — and the resolved value is registered with the secret sink for redaction.
    #[tokio::test]
    async fn plain_purpose_resolves_stored_bearer_over_env() {
        use flux_system::{System, Workspace};
        use std::collections::HashMap;
        struct MemStore(HashMap<String, flux_credentials::OAuthToken>);
        #[async_trait]
        impl flux_credentials::CredentialStore for MemStore {
            async fn load(&self, key: &str) -> Option<flux_credentials::OAuthToken> {
                self.0.get(key).cloned()
            }
            async fn save(
                &self,
                _key: &str,
                _token: &flux_credentials::OAuthToken,
            ) -> flux_core::Result<()> {
                Ok(())
            }
        }
        #[derive(Default)]
        struct SinkSpy(std::sync::Mutex<Vec<String>>);
        impl SecretSink for SinkSpy {
            fn register_secret(&self, value: &str) {
                self.0.lock().unwrap().push(value.to_string());
            }
        }
        let dir = std::env::temp_dir().join(format!("flux-plain-d126-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::set_var("FLUX_TEST_PLAIN_ENV_D126", "env-tok");

        let manifest = PluginManifest {
            name: "acme".into(),
            auth: vec![AuthMethod::bearer(
                "api_token",
                vec!["FLUX_TEST_PLAIN_ENV_D126".into()],
            )],
            capabilities: PluginCapabilities {
                secrets: vec!["FLUX_TEST_PLAIN_ENV_D126".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let sink = Arc::new(SinkSpy::default());
        let stored = MemStore(HashMap::from([(
            "plugin:acme:api_token".to_string(),
            flux_credentials::OAuthToken {
                access: "stored-tok".into(),
                refresh: None,
                expires_at_ms: None,
                account_id: None,
            },
        )]));

        // Stored token wins over the set env key, and is registered with the secret sink.
        let caps = SystemHostCaps::new(sys.clone())
            .with_manifest(&manifest)
            .with_credential_store(Arc::new(stored))
            .with_secret_sink(sink.clone());
        let got = caps
            .handle("secret", &json!({"purpose": "api_token"}))
            .await
            .unwrap();
        assert_eq!(got["value"], "stored-tok");
        assert!(sink.0.lock().unwrap().contains(&"stored-tok".to_string()));

        // Nothing stored → the declared env key resolves (the pre-D-126 behavior).
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_credential_store(Arc::new(MemStore(HashMap::new())));
        let got = caps
            .handle("secret", &json!({"purpose": "api_token"}))
            .await
            .unwrap();
        assert_eq!(got["value"], "env-tok");

        std::env::remove_var("FLUX_TEST_PLAIN_ENV_D126");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-32: the gated `config` capability reads a DECLARED non-secret config value (e.g. jira's
    /// Atlassian `cloud_id`); undeclared names are denied; and a secret-classified env key (a
    /// granted `secrets` entry or an auth method's secret env) is REFUSED even when declared as
    /// config — the capability can never return a secret value.
    #[tokio::test]
    async fn config_capability_reads_declared_non_secret_values_only() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-config-cap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::set_var("FLUX_TEST_CLOUD_ID_D32", "cloud-123");
        std::env::set_var("FLUX_TEST_TOKEN_D32", "tok-s3cr3t");
        std::env::set_var("FLUX_TEST_AUTH_ONLY_D32", "auth-s3cr3t");

        let manifest = PluginManifest {
            name: "jira".into(),
            auth: vec![AuthMethod::bearer(
                "api_token",
                vec!["FLUX_TEST_AUTH_ONLY_D32".into()],
            )],
            config: vec![
                ConfigSpec {
                    name: "cloud_id".into(),
                    env: vec!["FLUX_TEST_CLOUD_ID_D32".into()],
                    description: String::new(),
                },
                // Misdeclared: the env key is a granted secret — must be refused.
                ConfigSpec {
                    name: "sneaky_secret".into(),
                    env: vec!["FLUX_TEST_TOKEN_D32".into()],
                    description: String::new(),
                },
                // Misdeclared: the env key backs an auth method — must be refused too.
                ConfigSpec {
                    name: "sneaky_auth".into(),
                    env: vec!["FLUX_TEST_AUTH_ONLY_D32".into()],
                    description: String::new(),
                },
                ConfigSpec {
                    name: "unset".into(),
                    env: vec!["FLUX_TEST_UNSET_D32".into()],
                    description: String::new(),
                },
            ],
            capabilities: PluginCapabilities {
                secrets: vec!["FLUX_TEST_TOKEN_D32".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys).with_manifest(&manifest);

        // Declared + non-secret → resolves.
        let got = caps
            .handle("config", &json!({"name": "cloud_id"}))
            .await
            .unwrap();
        assert_eq!(got["value"], "cloud-123");
        // Undeclared name → denied (deny-by-default, like every host capability).
        assert!(caps
            .handle("config", &json!({"name": "nope"}))
            .await
            .is_err());
        // A granted-secret env key → refused; the value never crosses (not even in the error).
        let err = caps
            .handle("config", &json!({"name": "sneaky_secret"}))
            .await
            .unwrap_err();
        assert!(
            err.contains("secret-classified"),
            "names the refusal: {err}"
        );
        assert!(!err.contains("tok-s3cr3t"), "no secret in the error: {err}");
        // An auth-method env key → refused the same way.
        let err = caps
            .handle("config", &json!({"name": "sneaky_auth"}))
            .await
            .unwrap_err();
        assert!(
            err.contains("secret-classified"),
            "names the refusal: {err}"
        );
        assert!(
            !err.contains("auth-s3cr3t"),
            "no secret in the error: {err}"
        );
        // Declared but unset env → a clear error.
        assert!(caps
            .handle("config", &json!({"name": "unset"}))
            .await
            .is_err());

        std::env::remove_var("FLUX_TEST_CLOUD_ID_D32");
        std::env::remove_var("FLUX_TEST_TOKEN_D32");
        std::env::remove_var("FLUX_TEST_AUTH_ONLY_D32");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-32: a config value that is itself a credential-bearing URL (userinfo with a password, e.g.
    /// a DSN `postgres://user:pass@host/db`) is refused — the config capability can never hand the
    /// plugin an embedded secret, even through an operator-misconfigured env value.
    #[tokio::test]
    async fn config_capability_refuses_credential_bearing_urls() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-config-dsn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::set_var(
            "FLUX_TEST_DSN_WITH_PW_D32",
            "postgres://app:sup3rs3cret@db.internal:5432/warehouse",
        );
        std::env::set_var(
            "FLUX_TEST_DSN_BARE_D32",
            "postgres://app@db.internal:5432/warehouse",
        );

        let manifest = PluginManifest {
            name: "sql".into(),
            config: vec![
                ConfigSpec {
                    name: "dsn_with_pw".into(),
                    env: vec!["FLUX_TEST_DSN_WITH_PW_D32".into()],
                    description: String::new(),
                },
                ConfigSpec {
                    name: "dsn_bare".into(),
                    env: vec!["FLUX_TEST_DSN_BARE_D32".into()],
                    description: String::new(),
                },
            ],
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys).with_manifest(&manifest);

        // A bare (credential-free) DSN is plain config — allowed.
        let got = caps
            .handle("config", &json!({"name": "dsn_bare"}))
            .await
            .unwrap();
        assert_eq!(got["value"], "postgres://app@db.internal:5432/warehouse");
        // A password-bearing DSN is refused, and the password never crosses (not even in the error).
        let err = caps
            .handle("config", &json!({"name": "dsn_with_pw"}))
            .await
            .unwrap_err();
        assert!(err.contains("embeds a credential"), "{err}");
        assert!(
            !err.contains("sup3rs3cret"),
            "no secret in the error: {err}"
        );

        std::env::remove_var("FLUX_TEST_DSN_WITH_PW_D32");
        std::env::remove_var("FLUX_TEST_DSN_BARE_D32");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-32: an endpoint may declare a **default** base URL used when no env key resolves —
    /// host-side, so a plugin with a well-known public default (gitlab.com, the Opsgenie EU API)
    /// works with zero config while the URL still never crosses to the plugin. A set env wins;
    /// the default's host is HTTP-allow-listed like an env-resolved one.
    #[tokio::test]
    async fn endpoint_default_url_resolves_when_env_unset() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-ep-default-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::remove_var("FLUX_TEST_EP_DEFAULT_URL_D32");

        let manifest = PluginManifest {
            name: "gitlab".into(),
            endpoints: vec![EndpointSpec {
                name: "gitlab.endpoint".into(),
                env: vec!["FLUX_TEST_EP_DEFAULT_URL_D32".into()],
                default: Some("https://gitlab.com".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys.clone()).with_manifest(&manifest);

        // Env unset → the declared default resolves, and its host is allow-listed.
        assert_eq!(
            caps.resolve_endpoint("gitlab.endpoint").unwrap(),
            "https://gitlab.com"
        );
        assert!(caps.endpoint_allows_host("gitlab.com"));
        // A set env always wins over the default.
        std::env::set_var("FLUX_TEST_EP_DEFAULT_URL_D32", "https://gl.corp.example");
        assert_eq!(
            caps.resolve_endpoint("gitlab.endpoint").unwrap(),
            "https://gl.corp.example"
        );
        std::env::remove_var("FLUX_TEST_EP_DEFAULT_URL_D32");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-32: a **template endpoint** composes its base URL host-side from declared non-secret
    /// config values (`{name}` placeholders, percent-encoded) — the dynamic-endpoint resolution
    /// that replaces jira/confluence's plugin-constructed Atlassian gateway URL. Placeholders that
    /// name an undeclared config error; secret-classified placeholders are refused (a secret can
    /// never be smuggled into a composed URL); the composed host is HTTP-allow-listed like an
    /// env-resolved endpoint host.
    #[tokio::test]
    async fn template_endpoint_composes_from_config() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-tpl-ep-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        // A value that needs percent-encoding, to prove the substitution is encoded.
        std::env::set_var("FLUX_TEST_TPL_CLOUD_D32", "cloud/123");
        std::env::set_var("FLUX_TEST_TPL_TOKEN_D32", "tpl-s3cr3t");

        let manifest = PluginManifest {
            name: "jira".into(),
            auth: vec![AuthMethod::bearer(
                "api_token",
                vec!["FLUX_TEST_TPL_TOKEN_D32".into()],
            )],
            config: vec![
                ConfigSpec {
                    name: "cloud_id".into(),
                    env: vec!["FLUX_TEST_TPL_CLOUD_D32".into()],
                    description: String::new(),
                },
                ConfigSpec {
                    name: "token".into(),
                    env: vec!["FLUX_TEST_TPL_TOKEN_D32".into()],
                    description: String::new(),
                },
            ],
            endpoints: vec![
                EndpointSpec {
                    name: "jira.gateway".into(),
                    template: Some("https://gw.example.com/ex/jira/{cloud_id}".into()),
                    ..Default::default()
                },
                EndpointSpec {
                    name: "jira.evil".into(),
                    template: Some("https://gw.example.com/ex/{token}".into()),
                    ..Default::default()
                },
                EndpointSpec {
                    name: "jira.unknown".into(),
                    template: Some("https://gw.example.com/{nope}".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys).with_manifest(&manifest);

        // The template composes host-side, with the config value percent-encoded.
        assert_eq!(
            caps.resolve_endpoint("jira.gateway").unwrap(),
            "https://gw.example.com/ex/jira/cloud%2F123"
        );
        // The composed host is allow-listed for http.do exactly like an env-resolved endpoint host
        // (no separate http_hosts declaration needed).
        assert!(caps.endpoint_allows_host("gw.example.com"));
        assert!(!caps.endpoint_allows_host("evil.example.com"));
        // A secret-classified placeholder is refused — never substituted into a URL.
        let err = caps.resolve_endpoint("jira.evil").unwrap_err();
        assert!(err.contains("secret-classified"), "{err}");
        assert!(!err.contains("tpl-s3cr3t"), "no secret in the error: {err}");
        // An undeclared placeholder errors.
        assert!(caps.resolve_endpoint("jira.unknown").is_err());

        std::env::remove_var("FLUX_TEST_TPL_CLOUD_D32");
        std::env::remove_var("FLUX_TEST_TPL_TOKEN_D32");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-32: the ref-based `conn.dial` resolution knows the default ports of the SQL DSN schemes
    /// (postgres/mysql have no `url`-crate known default), so `sql`'s named endpoint dials by
    /// reference even when the operator's DSN omits the port.
    #[test]
    fn dial_target_from_url_defaults_sql_scheme_ports() {
        let t = dial_target_from_url("postgres://db.internal/app").unwrap();
        assert_eq!(conn_target_str(&t), "tcp:db.internal:5432");
        let t = dial_target_from_url("mysql://db.internal/app").unwrap();
        assert_eq!(conn_target_str(&t), "tcp:db.internal:3306");
        // An explicit port always wins.
        let t = dial_target_from_url("postgres://db.internal:6543/app").unwrap();
        assert_eq!(conn_target_str(&t), "tcp:db.internal:6543");
        // Known URL defaults still apply; a scheme with no default is still an error.
        let t = dial_target_from_url("https://svc.internal/x").unwrap();
        assert_eq!(conn_target_str(&t), "tcp:svc.internal:443");
        assert!(dial_target_from_url("foo://svc.internal/x").is_err());
    }

    #[tokio::test]
    async fn http_do_denies_undeclared_hosts_before_network() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-http-host-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            http: true,
            http_hosts: vec!["api.example.com".into()],
            ..Default::default()
        });

        let err = caps
            .handle("http.do", &json!({"url": "https://evil.example.com/"}))
            .await
            .unwrap_err();
        assert!(err.contains("not in this plugin's declared HTTP capabilities"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn endpoint_env_hosts_are_http_allow_listed() {
        use flux_system::{System, Workspace};
        let dir =
            std::env::temp_dir().join(format!("flux-http-endpoint-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::set_var(
            "FLUX_TEST_ENDPOINT_HOST_XZ",
            "https://selfhosted.example.com/base",
        );
        let caps = SystemHostCaps::new(sys)
            .with_grants(PluginCapabilities {
                http: true,
                ..Default::default()
            })
            .with_manifest(&PluginManifest {
                endpoints: vec![EndpointSpec {
                    name: "service.endpoint".into(),
                    env: vec!["FLUX_TEST_ENDPOINT_HOST_XZ".into()],
                    ..Default::default()
                }],
                capabilities: PluginCapabilities {
                    http: true,
                    ..Default::default()
                },
                ..Default::default()
            });
        let url = url::Url::parse("https://selfhosted.example.com/path").unwrap();

        assert!(caps.ensure_http_host_allowed(&url).is_ok());

        std::env::remove_var("FLUX_TEST_ENDPOINT_HOST_XZ");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn private_net_requires_manifest_declaration_and_operator_grant() {
        use flux_system::{System, Workspace};
        let dir =
            std::env::temp_dir().join(format!("flux-private-host-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let loopback = "http://127.0.0.1:8123/";

        let operator_only =
            SystemHostCaps::new(sys.clone()).with_private_net_grants(vec!["127.0.0.1".into()]);
        assert!(guard_http_url(loopback, &operator_only.private_net_allow()).is_err());

        let manifest_only = SystemHostCaps::new(sys.clone()).with_grants(PluginCapabilities {
            private_hosts: vec!["127.0.0.1".into()],
            ..Default::default()
        });
        assert!(guard_http_url(loopback, &manifest_only.private_net_allow()).is_err());

        let both = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        assert!(guard_http_url(loopback, &both.private_net_allow()).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_oauth2_and_legacy_auth_round_trip() {
        // D-80: an OAuth2 auth method round-trips through the manifest JSON, and a legacy env→secret
        // method (no `scheme`, no `oauth2`) still deserializes to Bearer + `oauth2: None` and
        // re-serializes without an `oauth2` key — backward compatibility.
        use serde_json::json;

        let legacy: AuthMethod =
            serde_json::from_value(json!({ "purpose": "api_token", "env": ["GITLAB_TOKEN"] }))
                .unwrap();
        assert_eq!(
            legacy.scheme,
            AuthScheme::Bearer,
            "an omitted scheme defaults to Bearer"
        );
        assert!(
            legacy.oauth2.is_none(),
            "a legacy method has no oauth2 block"
        );
        let legacy_json = serde_json::to_value(&legacy).unwrap();
        assert!(
            legacy_json.get("oauth2").is_none(),
            "a legacy method serializes without an oauth2 key"
        );

        let spec = OAuth2Spec {
            endpoint: "api".into(),
            authorize_path: "/oauth/authorize".into(),
            token_path: "/oauth/token".into(),
            client_id: "cid".into(),
            scopes: vec!["read".into(), "write".into()],
            grants: vec![OAuthGrant::AuthorizationCode, OAuthGrant::RefreshToken],
            redirect: Some(OAuthRedirect {
                port: 1456,
                path: "/auth/callback".into(),
            }),
        };
        let method = AuthMethod::oauth2("bot_token", spec.clone());
        let back: AuthMethod =
            serde_json::from_value(serde_json::to_value(&method).unwrap()).unwrap();
        assert_eq!(back.oauth2, Some(spec));
        assert_eq!(
            back.scheme,
            AuthScheme::Bearer,
            "the OAuth access token injects as Bearer"
        );
        // Grants serialize snake_case.
        let j = serde_json::to_value(&method).unwrap();
        assert_eq!(j["oauth2"]["grants"][0], "authorization_code");
    }

    #[tokio::test]
    async fn auth_injection_resolves_per_scheme() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-authinj-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::set_var("FLUX_TEST_BEARER_AJ", "bear-tok");
        std::env::set_var("FLUX_TEST_BASIC_TOK_AJ", "basic-tok");
        std::env::set_var("FLUX_TEST_BASIC_USER_AJ", "user@example.com");
        std::env::set_var("FLUX_TEST_HDR_AJ", "hdr-tok");
        std::env::set_var("FLUX_TEST_QRY_AJ", "qry-tok");

        let manifest = PluginManifest {
            name: "multi".into(),
            auth: vec![
                AuthMethod::bearer("bear", vec!["FLUX_TEST_BEARER_AJ".into()]),
                AuthMethod::basic(
                    "basic",
                    vec!["FLUX_TEST_BASIC_USER_AJ".into()],
                    vec!["FLUX_TEST_BASIC_TOK_AJ".into()],
                ),
                AuthMethod::header("genie", "GenieKey", vec!["FLUX_TEST_HDR_AJ".into()]),
                AuthMethod {
                    purpose: "qry".into(),
                    env: vec!["FLUX_TEST_QRY_AJ".into()],
                    scheme: AuthScheme::Query {
                        name: "apikey".into(),
                    },
                    ..Default::default()
                },
            ],
            capabilities: PluginCapabilities {
                secrets: vec![
                    "FLUX_TEST_BEARER_AJ".into(),
                    "FLUX_TEST_BASIC_TOK_AJ".into(),
                    "FLUX_TEST_HDR_AJ".into(),
                    "FLUX_TEST_QRY_AJ".into(),
                ],
                http: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys).with_manifest(&manifest);

        // legacy bearer_purpose → Bearer (unchanged behaviour)
        assert_eq!(
            caps.resolve_auth(&json!({"bearer_purpose": "bear"}))
                .await
                .unwrap(),
            AuthInjection::Bearer("bear-tok".into())
        );
        // auth_purpose respects each declared scheme
        assert_eq!(
            caps.resolve_auth(&json!({"auth_purpose": "bear"}))
                .await
                .unwrap(),
            AuthInjection::Bearer("bear-tok".into())
        );
        assert_eq!(
            caps.resolve_auth(&json!({"auth_purpose": "basic"}))
                .await
                .unwrap(),
            AuthInjection::Basic {
                user: "user@example.com".into(),
                secret: "basic-tok".into()
            }
        );
        assert_eq!(
            caps.resolve_auth(&json!({"auth_purpose": "genie"}))
                .await
                .unwrap(),
            AuthInjection::Header {
                name: "GenieKey".into(),
                value: "hdr-tok".into()
            }
        );
        assert_eq!(
            caps.resolve_auth(&json!({"auth_purpose": "qry"}))
                .await
                .unwrap(),
            AuthInjection::Query {
                name: "apikey".into(),
                value: "qry-tok".into()
            }
        );
        // no auth requested → None; undeclared purpose → error
        assert_eq!(
            caps.resolve_auth(&json!({})).await.unwrap(),
            AuthInjection::None
        );
        assert!(caps
            .resolve_auth(&json!({"auth_purpose": "nope"}))
            .await
            .is_err());

        for k in [
            "FLUX_TEST_BEARER_AJ",
            "FLUX_TEST_BASIC_TOK_AJ",
            "FLUX_TEST_BASIC_USER_AJ",
            "FLUX_TEST_HDR_AJ",
            "FLUX_TEST_QRY_AJ",
        ] {
            std::env::remove_var(k);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn conn_dial_round_trips_and_is_gated() {
        use flux_system::{System, Workspace};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = std::env::temp_dir().join(format!("flux-conn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        // A loopback echo server (hermetic).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 64];
                if let Ok(n) = sock.read(&mut buf).await {
                    let _ = sock.write_all(&buf[..n]).await;
                }
            }
        });
        let dial = json!({"kind": "tcp", "host": "127.0.0.1", "port": port});

        // Ungranted conn.dial is denied even with private-net allowed (the grant is the gate).
        let none =
            SystemHostCaps::new(sys.clone()).with_private_net_grants(vec!["127.0.0.1".into()]);
        assert!(none.handle("conn.dial", &dial).await.is_err());

        // Granted (loopback wildcard) → dial/write/read/close round-trips.
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                conn: vec!["tcp:127.0.0.1:*".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        let id = caps.handle("conn.dial", &dial).await.unwrap()["conn_id"]
            .as_u64()
            .unwrap();
        let ping = base64::engine::general_purpose::STANDARD.encode(b"ping");
        let wrote = caps
            .handle("conn.write", &json!({"conn_id": id, "data_b64": ping}))
            .await
            .unwrap();
        assert_eq!(wrote["written"], 4);
        let read = caps
            .handle("conn.read", &json!({"conn_id": id, "max": 64}))
            .await
            .unwrap();
        let got = base64::engine::general_purpose::STANDARD
            .decode(read["data_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(&got, b"ping");
        caps.handle("conn.close", &json!({"conn_id": id}))
            .await
            .unwrap();
        // reading a closed/unknown connection errors
        assert!(caps
            .handle("conn.read", &json!({"conn_id": id, "max": 8}))
            .await
            .is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unix_conn_wildcards_are_single_segment_and_traversal_safe() {
        let grants = vec!["unix:/tmp/plugin/*.sock".to_string()];
        assert!(conn_granted(&grants, "unix:/tmp/plugin/db.sock"));
        assert!(!conn_granted(&grants, "unix:/tmp/plugin/nested/db.sock"));
        assert!(!conn_granted(
            &grants,
            "unix:/tmp/plugin/../../var/run/docker.sock"
        ));
        assert!(!conn_granted(&grants, "unix:/tmp/plugin/./db.sock"));

        let exact = vec!["unix:/tmp/plugin/../docker.sock".to_string()];
        assert!(!conn_granted(&exact, "unix:/tmp/plugin/../docker.sock"));
    }

    /// C-257 closure: a path wildcard is not a directory capability when the final socket name can
    /// be a symlink. The host must deny the alias before the kernel follows it to an ungranted
    /// listener outside the nominally granted directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_conn_grant_does_not_follow_symlink_outside_granted_directory() {
        use flux_system::{System, Workspace};
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "flux-conn-unix-symlink-c257-{}",
            std::process::id()
        ));
        let granted = dir.join("granted");
        std::fs::create_dir_all(&granted).unwrap();
        let outside_socket = dir.join("outside.sock");
        let listener = tokio::net::UnixListener::bind(&outside_socket).unwrap();
        let alias = granted.join("alias.sock");
        symlink(&outside_socket, &alias).unwrap();

        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            conn: vec![format!("unix:{}/*.sock", granted.display())],
            ..Default::default()
        });
        let result = caps
            .handle(
                "conn.dial",
                &json!({"kind": "unix", "path": alias.to_string_lossy()}),
            )
            .await;

        assert!(
            result.is_err(),
            "a Unix grant wildcard must not authorize a symlink to an outside socket"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err(),
            "the ungranted outside listener must never receive a connection"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-257 closure, the other half: enforcing physical identity must deny only what actually
    /// escapes. A symlink resolving to a socket *inside* the granted directory is still granted —
    /// otherwise the fix would be a blanket ban on symlinked sockets rather than a containment
    /// boundary, and would break the ordinary `/run/service/current.sock` indirection.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_conn_grant_allows_symlink_that_resolves_inside_granted_directory() {
        use flux_system::{System, Workspace};
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "flux-conn-unix-symlink-inside-c257-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        let granted = dir.join("granted");
        std::fs::create_dir_all(&granted).unwrap();
        let real_socket = granted.join("real.sock");
        let _listener = tokio::net::UnixListener::bind(&real_socket).unwrap();
        let alias = granted.join("alias.sock");
        symlink(&real_socket, &alias).unwrap();

        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            conn: vec![format!("unix:{}/*.sock", granted.display())],
            ..Default::default()
        });
        let result = caps
            .handle(
                "conn.dial",
                &json!({"kind": "unix", "path": alias.to_string_lossy()}),
            )
            .await;

        assert!(
            result.is_ok(),
            "a symlink resolving inside the granted directory must stay authorized, got: {result:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-257: `conn.dial` must connect directly to the address returned by its authorization
    /// lookup. A fake hostname has no system DNS entry; reaching the listener proves the pin was
    /// consumed, and the single resolver call proves connect did not ask for the metadata answer.
    #[tokio::test]
    async fn conn_dial_consumes_the_vetted_address_without_rebinding() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-conn-pin-c257-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = tokio::spawn(async move { listener.accept().await.unwrap() });
        let resolver = Arc::new(SequenceResolver::new(&["127.0.0.1", "169.254.169.254"]));
        let caps = SystemHostCaps::new(sys)
            .with_host_resolver(resolver.clone())
            .with_private_net_grants(vec!["plugin-db.test".into()])
            .with_grants(PluginCapabilities {
                conn: vec![format!("tcp:plugin-db.test:{port}")],
                private_hosts: vec!["plugin-db.test".into()],
                ..Default::default()
            });

        let opened = caps
            .handle(
                "conn.dial",
                &json!({"kind": "tcp", "host": "plugin-db.test", "port": port}),
            )
            .await
            .unwrap();
        assert!(opened["conn_id"].is_u64());
        accepted.await.unwrap();
        assert_eq!(resolver.calls(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn conn_read_timeout_returns_timed_out_without_closing() {
        // D-45: a `conn.read` with `timeout_ms` that elapses before data arrives returns
        // `timed_out: true` (and an empty body) while leaving the connection open — the plugin
        // can retry or close. A server that accepts but never writes exercises the deadline path.
        use flux_system::{System, Workspace};
        use tokio::io::AsyncReadExt;
        let dir = std::env::temp_dir().join(format!("flux-conn-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        // A loopback server that accepts but never writes (so the client's read blocks until the
        // deadline). It holds the socket open for the whole test so the read doesn't get a clean
        // EOF — only the timeout fires.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept_task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Keep the socket open (no write) so the client's read blocks; drain until closed.
            let mut buf = [0u8; 64];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });

        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                conn: vec!["tcp:127.0.0.1:*".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        let dial = json!({"kind": "tcp", "host": "127.0.0.1", "port": port});
        let id = caps.handle("conn.dial", &dial).await.unwrap()["conn_id"]
            .as_u64()
            .unwrap();

        // A 10ms deadline on a read against a server that never writes → timed_out, empty body.
        let read = caps
            .handle(
                "conn.read",
                &json!({"conn_id": id, "max": 64, "timeout_ms": 10}),
            )
            .await
            .unwrap();
        assert_eq!(read["timed_out"], true, "the read should time out: {read}");
        let body = base64::engine::general_purpose::STANDARD
            .decode(read["data_b64"].as_str().unwrap())
            .unwrap();
        assert!(body.is_empty(), "no data should arrive before the deadline");
        // The connection stays open (not closed by the timeout): a write still succeeds.
        let ping = base64::engine::general_purpose::STANDARD.encode(b"ping");
        let wrote = caps
            .handle("conn.write", &json!({"conn_id": id, "data_b64": ping}))
            .await
            .unwrap();
        assert_eq!(
            wrote["written"], 4,
            "the connection is still usable after a timeout"
        );

        caps.handle("conn.close", &json!({"conn_id": id}))
            .await
            .unwrap();
        accept_task.await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn blob_put_get_info_round_trips_and_is_gated() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-blob-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let payload = base64::engine::general_purpose::STANDARD.encode(b"hello blob");

        // Ungranted blob.* is denied.
        let none = SystemHostCaps::new(sys.clone());
        assert!(none
            .handle("blob.put", &json!({"name": "x", "data_b64": payload}))
            .await
            .is_err());

        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            blob: true,
            ..Default::default()
        });
        let put = caps
            .handle(
                "blob.put",
                &json!({"name": "greeting.txt", "data_b64": payload}),
            )
            .await
            .unwrap();
        let r = put["blob_ref"].as_str().unwrap().to_string();
        // content-addressed: same content → same ref
        let put2 = caps
            .handle(
                "blob.put",
                &json!({"name": "again.txt", "data_b64": payload}),
            )
            .await
            .unwrap();
        assert_eq!(put2["blob_ref"].as_str().unwrap(), r);

        let info = caps
            .handle("blob.info", &json!({"blob_ref": r}))
            .await
            .unwrap();
        assert_eq!(info["size"], 10);
        assert_eq!(info["sha256"], r);

        let got = caps
            .handle("blob.get", &json!({"blob_ref": r}))
            .await
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(got["data_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(&bytes, b"hello blob");

        // unknown ref errors
        assert!(caps
            .handle("blob.get", &json!({"blob_ref": "deadbeef"}))
            .await
            .is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn process_spawn_denies_ungranted_program() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-procdeny-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        // A fresh caps grants no programs → process.spawn is denied.
        let none = SystemHostCaps::new(sys.clone());
        assert!(
            none.handle("process.spawn", &json!({"argv": ["echo", "hi"]}))
                .await
                .is_err(),
            "ungranted process.spawn must be denied"
        );
        // Granting only `printf` still denies `sleep` (same allow-list as process.run).
        let limited = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            process: vec!["printf".into()],
            ..Default::default()
        });
        assert!(
            limited
                .handle("process.spawn", &json!({"argv": ["sleep", "30"]}))
                .await
                .is_err(),
            "a non-granted program must be denied on process.spawn"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn process_spawn_read_status_kill_lifecycle() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-proclife-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            process: vec!["sleep".into()],
            ..Default::default()
        });

        // spawn a long-lived child
        let spawned = caps
            .handle("process.spawn", &json!({"argv": ["sleep", "30"]}))
            .await
            .unwrap();
        let id = spawned["proc_id"].as_u64().unwrap();

        // read + status both report it running (and no exit_code yet)
        let read = caps
            .handle("process.read", &json!({"proc_id": id}))
            .await
            .unwrap();
        assert_eq!(read["running"], true);
        assert!(read.get("exit_code").is_none());
        let st = caps
            .handle("process.status", &json!({"proc_id": id}))
            .await
            .unwrap();
        assert_eq!(st["running"], true);

        // kill removes it from the registry
        let killed = caps
            .handle("process.kill", &json!({"proc_id": id}))
            .await
            .unwrap();
        assert_eq!(killed["ok"], true);
        assert!(
            caps.handle("process.status", &json!({"proc_id": id}))
                .await
                .is_err(),
            "a killed process is no longer in the registry"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn process_read_captures_output_and_exit_code() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-procout-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            process: vec!["printf".into()],
            ..Default::default()
        });
        let id = caps
            .handle("process.spawn", &json!({"argv": ["printf", "out-bg"]}))
            .await
            .unwrap()["proc_id"]
            .as_u64()
            .unwrap();

        // Poll read (it drains) accumulating stdout. The drain task copies the pipe asynchronously,
        // so the final bytes can arrive a tick *after* the child is observed exited — keep reading
        // until the expected output shows up, not just until exit.
        let mut combined = String::new();
        let mut exit_code: Option<i64> = None;
        let mut saw_exit = false;
        for _ in 0..200 {
            let r = caps
                .handle("process.read", &json!({"proc_id": id}))
                .await
                .unwrap();
            combined.push_str(r["stdout"].as_str().unwrap());
            if r["running"] == false {
                saw_exit = true;
                exit_code = r.get("exit_code").and_then(|v| v.as_i64());
            }
            if saw_exit && combined.contains("out-bg") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(saw_exit, "child should have exited");
        assert_eq!(exit_code, Some(0));
        assert_eq!(combined, "out-bg");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A throwaway HTTP/1.1 server that echoes each request body back as the response body. Lets the
    /// `http.do` binary-body paths round-trip without a network dependency. Returns the bound port.
    async fn spawn_echo_http_server() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 2048];
                    // Read until we have the full header block, then parse Content-Length.
                    let (headers_end, content_length) = loop {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header_str = String::from_utf8_lossy(&buf[..pos]);
                            let cl = header_str
                                .lines()
                                .find_map(|l| {
                                    let lower = l.to_ascii_lowercase();
                                    lower
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            break (pos + 4, cl);
                        }
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break (buf.len(), 0),
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    };
                    // Read the remaining body bytes.
                    while buf.len() < headers_end + content_length {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let end = (headers_end + content_length).min(buf.len());
                    let body = buf[headers_end..end].to_vec();
                    let mut resp =
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                            .into_bytes();
                    resp.extend_from_slice(&body);
                    let _ = sock.write_all(&resp).await;
                    let _ = sock.flush().await;
                });
            }
        });
        port
    }

    async fn spawn_cross_origin_redirect() -> (u16, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = target.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let _ = seen_tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .await;
            }
        });

        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_port = source.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = source.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nlocation: http://{target_addr}/final\r\ncontent-length: 0\r\n\r\n"
                );
                let _ = sock.write_all(response.as_bytes()).await;
            }
        });
        (source_port, seen_rx)
    }

    async fn spawn_same_origin_redirect() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut first, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = first.read(&mut buf).await;
                let _ = first
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nlocation: /final\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            }
            if let Ok((mut second, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = second.read(&mut buf).await;
                let _ = second
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nfinal")
                    .await;
            }
        });
        port
    }

    async fn spawn_stalled_http_server() -> u16 {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut request = [0u8; 2048];
                let _ = socket.read(&mut request).await;
                // Keep the connection open without producing response headers or a body.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn http_do_guards_every_redirect_target() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!(
            "flux-http-redirect-guard-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let (port, _seen) = spawn_cross_origin_redirect().await;
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["localhost".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["localhost".into(), "127.0.0.1".into()],
                private_hosts: vec!["localhost".into()],
                ..Default::default()
            });
        let err = caps
            .handle(
                "http.do",
                &json!({"url": format!("http://localhost:{port}/start")}),
            )
            .await
            .expect_err("127.0.0.1 is outside the scoped private-net grant");
        assert!(
            err.contains("private/loopback"),
            "shared guard denial: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-257: every HTTP hop is pinned to the answer that its guard call vetted. The resolver's
    /// third answer models a connect-time rebind to metadata; two-hop success plus exactly two
    /// resolver calls proves neither connection performs a hidden lookup.
    #[tokio::test]
    async fn http_do_pins_the_initial_request_and_each_redirect_hop() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!(
            "flux-http-pinned-redirect-c257-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let port = spawn_same_origin_redirect().await;
        let resolver = Arc::new(SequenceResolver::new(&[
            "127.0.0.1",
            "127.0.0.1",
            "169.254.169.254",
        ]));
        let caps = SystemHostCaps::new(sys)
            .with_host_resolver(resolver.clone())
            .with_private_net_grants(vec!["plugin-http.test".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["plugin-http.test".into()],
                private_hosts: vec!["plugin-http.test".into()],
                ..Default::default()
            });

        let result = caps
            .handle(
                "http.do",
                &json!({"url": format!("http://plugin-http.test:{port}/start")}),
            )
            .await
            .unwrap();
        assert_eq!(result["body"], "final");
        assert_eq!(
            resolver.calls(),
            2,
            "one authorization lookup per hop; no connect-time re-resolution"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-257 closure: reqwest's ambient proxy support must not replace the address admitted by the
    /// guard. Run in an isolated child process because proxy variables are process-global.
    #[tokio::test]
    async fn pinned_http_client_ignores_ambient_proxy() {
        const CHILD: &str = "FLUX_PLUGIN_PROXY_REGRESSION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "host::tests::pinned_http_client_ignores_ambient_proxy",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "isolated proxy regression failed");
            return;
        }

        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn serve_once(listener: tokio::net::TcpListener, reached: Arc<AtomicBool>) {
            let accepted =
                tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept())
                    .await;
            let Ok(Ok((mut socket, _))) = accepted else {
                return;
            };
            reached.store(true, Ordering::SeqCst);
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }

        let pinned = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pinned_addr = pinned.local_addr().unwrap();
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let pinned_reached = Arc::new(AtomicBool::new(false));
        let proxy_reached = Arc::new(AtomicBool::new(false));
        let pinned_task = tokio::spawn(serve_once(pinned, pinned_reached.clone()));
        let proxy_task = tokio::spawn(serve_once(proxy, proxy_reached.clone()));

        let proxy_url = format!("http://{proxy_addr}");
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            std::env::set_var(key, &proxy_url);
        }
        std::env::set_var("NO_PROXY", "");
        std::env::set_var("no_proxy", "");

        let url = url::Url::parse(&format!("http://plugin.test:{}/", pinned_addr.port())).unwrap();
        let client = pinned_http_client(&url, &[pinned_addr], "http.do").unwrap();
        client.get(url).send().await.unwrap();

        pinned_task.await.unwrap();
        proxy_task.await.unwrap();
        assert!(pinned_reached.load(Ordering::SeqCst));
        assert!(!proxy_reached.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn http_do_fails_closed_when_dns_vets_no_address() {
        use flux_system::{System, Workspace};
        let dir =
            std::env::temp_dir().join(format!("flux-http-empty-pin-c257-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys)
            .with_host_resolver(Arc::new(SequenceResolver::new(&[])))
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["empty.test".into()],
                ..Default::default()
            });
        let err = caps
            .handle("http.do", &json!({"url": "https://empty.test/path"}))
            .await
            .expect_err("an empty guard answer must not reach connect-time DNS");
        assert!(err.contains("no vetted addresses"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn http_do_cross_origin_redirect_strips_all_credentials() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!(
            "flux-http-redirect-header-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let (port, seen) = spawn_cross_origin_redirect().await;
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        let result = caps
            .handle(
                "http.do",
                &json!({
                    "url": format!("http://127.0.0.1:{port}/start"),
                    "headers": {
                        "Authorization": "Bearer plugin-secret",
                        "Cookie": "session=plugin-secret",
                        "Proxy-Authorization": "Basic plugin-secret",
                        "X-Api-Key": "plugin-secret",
                        "X-Custom": "also-sensitive"
                    }
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["body"], "ok");
        let request = seen.await.expect("redirect target received the request");
        let lower = request.to_ascii_lowercase();
        for name in [
            "authorization:",
            "cookie:",
            "proxy-authorization:",
            "x-api-key:",
            "x-custom:",
        ] {
            assert!(!lower.contains(name), "forwarded {name}: {request}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn host_injected_bearer_is_redacted_and_not_forwarded_cross_origin() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!(
            "flux-http-auth-redirect-c70-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let (port, seen) = spawn_cross_origin_redirect().await;
        let env_key = "FLUX_TEST_TAVILY_KEY_C70";
        let secret = "tvly-c70-host-injected-secret";
        std::env::set_var(env_key, secret);
        let manifest = PluginManifest {
            name: "websearch".into(),
            capabilities: PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                secrets: vec![env_key.into()],
                ..Default::default()
            },
            auth: vec![AuthMethod::bearer("tavily_api_key", vec![env_key.into()])],
            ..Default::default()
        };
        let redactor = flux_secret::Redactor::new();
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_secret_sink(Arc::new(RedactorSink {
                redactor: redactor.clone(),
            }));

        let availability = caps
            .handle("auth.available", &json!({ "purpose": "tavily_api_key" }))
            .await
            .unwrap();
        assert_eq!(availability, json!({ "available": true }));
        assert!(!availability.to_string().contains(secret));

        let result = caps
            .handle(
                "http.do",
                &json!({
                    "url": format!("http://127.0.0.1:{port}/start"),
                    "auth_purpose": "tavily_api_key"
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["body"], "ok");
        let request = seen.await.expect("redirect target received the request");
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        assert!(!request.contains(secret));
        let scrubbed = redactor.redact(secret);
        assert_ne!(scrubbed, secret);
        assert!(!scrubbed.contains(secret));

        std::env::remove_var(env_key);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn http_do_follows_bounded_same_origin_redirect() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!(
            "flux-http-same-origin-redirect-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let port = spawn_same_origin_redirect().await;
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        let result = caps
            .handle(
                "http.do",
                &json!({"url": format!("http://127.0.0.1:{port}/start")}),
            )
            .await
            .unwrap();
        assert_eq!(result, json!({"status": 200, "body": "final"}));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn http_do_timeout_covers_server_that_never_responds() {
        use flux_system::{System, Workspace};
        let dir =
            std::env::temp_dir().join(format!("flux-http-timeout-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let port = spawn_stalled_http_server().await;
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            caps.handle(
                "http.do",
                &json!({
                    "url": format!("http://127.0.0.1:{port}/stall"),
                    "timeout_ms": 25
                }),
            ),
        )
        .await
        .expect("http.do enforces its own timeout")
        .expect_err("a stalled response is a timeout error");
        assert!(err.contains("timed out after 25ms"), "clear timeout: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn http_body_b64_round_trips_and_response_binary_is_byte_exact() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-httpbin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        // http granted + loopback declared and operator-granted so the test server is reachable.
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        let port = spawn_echo_http_server().await;
        let url = format!("http://127.0.0.1:{port}/echo");

        // Raw, non-UTF-8 bytes: body_b64 upload + response_binary download must round-trip exactly.
        let raw: Vec<u8> = vec![0u8, 159, 146, 150, 255, 0, 1, 2];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let resp = caps
            .handle(
                "http.do",
                &json!({
                    "method": "POST",
                    "url": url,
                    "body_b64": b64,
                    "response_binary": true,
                }),
            )
            .await
            .unwrap();
        assert_eq!(resp["status"], 200);
        assert!(
            resp.get("body").is_none(),
            "binary response must not carry a text body"
        );
        let got = base64::engine::general_purpose::STANDARD
            .decode(resp["body_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(got, raw, "binary body must be byte-exact");

        // Default (no response_binary): body_b64 still uploads, response comes back as text.
        let text_b64 = base64::engine::general_purpose::STANDARD.encode(b"hello-text");
        let resp2 = caps
            .handle(
                "http.do",
                &json!({"method": "POST", "url": url, "body_b64": text_b64}),
            )
            .await
            .unwrap();
        assert_eq!(resp2["status"], 200);
        assert_eq!(resp2["body"], "hello-text");
        assert!(resp2.get("body_b64").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A test [`EgressAudit`] double that records every admit as `(caller, host, grant_source)`.
    #[derive(Default)]
    struct RecordingAudit {
        admits: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl EgressAudit for RecordingAudit {
        fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str) {
            self.admits.lock().unwrap().push((
                caller.to_string(),
                host.to_string(),
                grant_source.to_string(),
            ));
        }
    }

    #[tokio::test]
    async fn egress_audit_fires_on_private_admit_only() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-audit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let audit = Arc::new(RecordingAudit::default());

        // A loopback echo server so a *private* host can actually be reached (admitted).
        let port = spawn_echo_http_server().await;

        // Manifest names the plugin (→ caller + grant_source), grants http + a loopback private host
        // (declared + operator-granted), plus a *public* host allow so a public request isn't blocked.
        let manifest = PluginManifest {
            name: "auditplug".into(),
            capabilities: PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into(), "example.com".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_egress_audit(audit.clone());

        // Admitting a PRIVATE host fires the audit with the plugin name + plugin grant_source.
        caps.handle(
            "http.do",
            &json!({"url": format!("http://127.0.0.1:{port}/echo")}),
        )
        .await
        .unwrap();
        {
            let admits = audit.admits.lock().unwrap();
            assert_eq!(
                admits.len(),
                1,
                "private admit must record exactly one event"
            );
            assert_eq!(admits[0].0, "auditplug");
            assert_eq!(admits[0].1, "127.0.0.1");
            assert_eq!(admits[0].2, "config:plugin/auditplug");
        }

        // A PUBLIC host does NOT fire the audit (the request fails at connect/DNS, but the host is
        // allow-listed so it passes the host gate and reaches the audit check — which must not fire).
        let _ = caps
            .handle("http.do", &json!({"url": "http://example.com/"}))
            .await;
        assert_eq!(
            audit.admits.lock().unwrap().len(),
            1,
            "a public host must not record a private-admit event"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn truncate_on_char_boundary_never_panics() {
        let s = format!("{}é", "a".repeat(100)); // multibyte char near the cut
                                                 // Cut at a byte that lands inside the 'é' — must not panic and stays valid UTF-8.
        let out = truncate_on_char_boundary(s.clone(), 101);
        assert!(out.len() <= 101);
        assert!(out.is_char_boundary(out.len()));
        assert_eq!(truncate_on_char_boundary("short".into(), 1024), "short");
    }

    #[test]
    fn frame_roundtrips_as_ndjson() {
        let f = Frame::request("r1", "manifest", Value::Null);
        let line = serde_json::to_string(&f).unwrap();
        assert!(!line.contains('\n'));
        let back: Frame = serde_json::from_str(&line).unwrap();
        assert_eq!(back.command, "manifest");
        assert_eq!(back.kind, FrameKind::Request);
    }

    #[test]
    fn responses_carry_ok_and_error() {
        let ok = Frame::ok_response("r1", serde_json::json!({"x": 1}));
        assert!(ok.ok);
        assert_eq!(ok.result["x"], 1);
        let err = Frame::err_response("r1", "boom");
        assert!(!err.ok);
        assert_eq!(err.error.as_deref(), Some("boom"));
    }

    /// A mock [`ReferenceResolver`] for the ref-based IO tests: a fixed endpoint resolution (URL +
    /// one injected header) and a fixed credential materialization, recording whether each was
    /// consulted.
    struct MockResolver {
        endpoint_url: String,
        inject: (String, String),
        credential_value: String,
        endpoint_consulted: std::sync::atomic::AtomicBool,
        credential_consulted: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl ReferenceResolver for MockResolver {
        async fn resolve_endpoint(
            &self,
            reference: &str,
        ) -> std::result::Result<flux_secret::endpoint::ResolvedEndpoint, String> {
            self.endpoint_consulted
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(
                flux_secret::endpoint::ResolvedEndpoint::new(reference, &self.endpoint_url)
                    .with_header(&self.inject.0, &self.inject.1),
            )
        }

        async fn resolve_credential(
            &self,
            reference: &flux_secret::Ref,
        ) -> std::result::Result<flux_secret::Material, String> {
            self.credential_consulted
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(flux_secret::Material {
                reference: reference.clone(),
                kind: flux_secret::Kind::ApiKey,
                value: self.credential_value.clone(),
                media_type: None,
            })
        }
    }

    /// A [`SecretSink`] double backed by a [`Redactor`](flux_secret::Redactor), so a test can assert a
    /// materialized credential is registered (and thus redacted from any captured output).
    struct RedactorSink {
        redactor: flux_secret::Redactor,
    }

    impl SecretSink for RedactorSink {
        fn register_secret(&self, value: &str) {
            self.redactor.add_secret(value);
        }
    }

    /// A throwaway HTTP/1.1 server that echoes the request's `Authorization` header value back as the
    /// response body (or `none`). Lets a test prove the host-injected header reached the wire.
    async fn spawn_header_echo_server() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 2048];
                    loop {
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let headers = String::from_utf8_lossy(&buf);
                    let auth = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("authorization:")
                                .map(|_| {
                                    l.split_once(':')
                                        .map(|(_, v)| v.trim().to_string())
                                        .unwrap_or_default()
                                })
                        })
                        .unwrap_or_else(|| "none".to_string());
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                        auth.len(),
                        auth
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn http_by_ref_injects_host_side() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-httpref-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let port = spawn_header_echo_server().await;

        // The resolver returns the loopback test server's URL + an Authorization: Bearer header. The
        // plugin passes only an `endpoint_ref` — never the URL or the token.
        let token = "sk-super-secret-ref-token";
        let resolver = Arc::new(MockResolver {
            endpoint_url: format!("http://127.0.0.1:{port}/"),
            inject: ("Authorization".into(), format!("Bearer {token}")),
            credential_value: String::new(),
            endpoint_consulted: std::sync::atomic::AtomicBool::new(false),
            credential_consulted: std::sync::atomic::AtomicBool::new(false),
        });
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            })
            .with_resolver(resolver.clone());

        let result = caps
            .handle(
                "http.do",
                &json!({ "endpoint_ref": "@endpoint/svc-1", "path": "v1/ping" }),
            )
            .await
            .unwrap();

        // The resolver was consulted, and the outbound request carried the host-injected header.
        assert!(resolver
            .endpoint_consulted
            .load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(result["status"], 200);
        assert_eq!(
            result["body"],
            format!("Bearer {token}"),
            "the server saw the host-injected Authorization header"
        );
        // The frame the plugin gets back contains neither the resolved URL nor… the token would have
        // been in `body` only because our echo server reflects it; in production the plugin gets only
        // the real response. Assert the *frame fields* never carry the URL or the credential ref.
        let frame = result.to_string();
        assert!(
            !frame.contains("127.0.0.1"),
            "frame must not carry the URL: {frame}"
        );
        assert!(
            result.get("url").is_none() && result.get("endpoint_ref").is_none(),
            "frame must not echo the URL/ref back to the plugin"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn http_by_named_ref_resolves_from_manifest() {
        use flux_system::{System, Workspace};
        let dir =
            std::env::temp_dir().join(format!("flux-httpnamedref-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let port = spawn_header_echo_server().await;

        // A NAMED manifest endpoint bound to the loopback test server via env, plus a declared
        // Bearer auth method. The plugin passes only the named `endpoint_ref` + `path`; the host
        // resolves the base URL locally from the manifest (no resolver installed), composes the URL,
        // and injects the declared `auth_purpose` host-side. A resolver is deliberately NOT installed
        // to prove a named ref resolves entirely from the manifest.
        std::env::set_var(
            "FLUX_TEST_NAMEDREF_URL",
            format!("http://127.0.0.1:{port}/"),
        );
        std::env::set_var("FLUX_TEST_NAMEDREF_TOK", "named-bear-tok");
        let manifest = PluginManifest {
            name: "svc".into(),
            auth: vec![AuthMethod::bearer(
                "api_token",
                vec!["FLUX_TEST_NAMEDREF_TOK".into()],
            )],
            endpoints: vec![EndpointSpec {
                name: "svc.endpoint".into(),
                env: vec!["FLUX_TEST_NAMEDREF_URL".into()],
                http_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            }],
            capabilities: PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                secrets: vec!["FLUX_TEST_NAMEDREF_TOK".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_private_net_grants(vec!["127.0.0.1".into()]);

        let result = caps
            .handle(
                "http.do",
                &json!({
                    "endpoint_ref": "svc.endpoint",
                    "path": "/api/x",
                    "auth_purpose": "api_token",
                }),
            )
            .await
            .unwrap();

        // The host composed `{base}/api/x` and injected the declared Bearer token; the echo server
        // reflects the Authorization header it saw.
        assert_eq!(result["status"], 200);
        assert_eq!(
            result["body"], "Bearer named-bear-tok",
            "the host injected the declared auth_purpose for a named ref"
        );
        // The frame carries neither the resolved URL nor the token — only the ref + path went in.
        let frame = result.to_string();
        assert!(
            !frame.contains("127.0.0.1") && !frame.contains("FLUX_TEST_NAMEDREF"),
            "frame must not carry the URL/env: {frame}"
        );

        std::env::remove_var("FLUX_TEST_NAMEDREF_URL");
        std::env::remove_var("FLUX_TEST_NAMEDREF_TOK");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn raw_socket_credential_gated_to_plugin_not_model() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-credgate-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        let secret = "pg-scram-password-value";
        let resolver = Arc::new(MockResolver {
            endpoint_url: "postgres://db.internal:5432/app".into(),
            inject: (String::new(), String::new()),
            credential_value: secret.into(),
            endpoint_consulted: std::sync::atomic::AtomicBool::new(false),
            credential_consulted: std::sync::atomic::AtomicBool::new(false),
        });
        let redactor = flux_secret::Redactor::new();
        let sink = Arc::new(RedactorSink {
            redactor: redactor.clone(),
        });
        let cred_payload = json!({ "credential_ref": "kubernetes/monitoring/pg-creds/password" });

        // WITHOUT the `credential` grant → refused (deny-by-default).
        let ungranted = SystemHostCaps::new(sys.clone())
            .with_resolver(resolver.clone())
            .with_secret_sink(sink.clone());
        let err = ungranted
            .handle("credential", &cred_payload)
            .await
            .unwrap_err();
        assert!(
            err.contains("not granted"),
            "ungranted credential must be refused: {err}"
        );
        assert!(
            !resolver
                .credential_consulted
                .load(std::sync::atomic::Ordering::SeqCst),
            "the resolver must not even be consulted without the grant"
        );

        // WITH the grant → the (trusted) plugin receives the materialized value.
        let granted = SystemHostCaps::new(sys)
            .with_grants(PluginCapabilities {
                credential: true,
                ..Default::default()
            })
            .with_resolver(resolver.clone())
            .with_secret_sink(sink.clone());
        let got = granted.handle("credential", &cred_payload).await.unwrap();
        assert_eq!(
            got["value"], secret,
            "the trusted plugin receives the value"
        );
        assert!(resolver
            .credential_consulted
            .load(std::sync::atomic::Ordering::SeqCst));

        // The value is registered with the redactor → it would be scrubbed from any captured output.
        assert_eq!(
            redactor.redact(&format!("connecting with {secret} now")),
            "connecting with [redacted] now",
            "the materialized credential is redacted from model-visible output"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn descriptors_add_and_discover() {
        let dir = std::env::temp_dir().join(format!("flux-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // missing dir → empty discovery
        assert!(discover(&dir).is_empty());

        add_descriptor(
            &dir,
            "gitlab",
            &PluginDescriptor {
                program: "/usr/bin/gitlab-plugin".into(),
                args: vec!["--v2".into()],
                pinned: None,
                ..Default::default()
            },
        )
        .unwrap();
        add_descriptor(
            &dir,
            "slack",
            &PluginDescriptor {
                program: "slack-plugin".into(),
                args: vec![],
                pinned: None,
                ..Default::default()
            },
        )
        .unwrap();

        let found = discover(&dir);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "gitlab"); // sorted
        assert_eq!(found[0].descriptor.args, vec!["--v2"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-48: `verify_descriptor` — the three verification states. Hashless → unverified-local;
    /// matching hash → verified; mismatching hash (or unreadable binary) → drift, never a pass.
    #[test]
    fn verify_descriptor_reports_verified_drift_and_unverified() {
        let dir = std::env::temp_dir().join(format!("flux-verify-desc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("flux-plugin-alpha");
        std::fs::write(&bin, b"alpha-bytes").unwrap();
        let good = pack::sha256_hex(b"alpha-bytes");

        let hashless = PluginDescriptor {
            program: bin.to_string_lossy().into_owned(),
            ..Default::default()
        };
        assert_eq!(verify_descriptor(&hashless), Verification::UnverifiedLocal);

        let verified = PluginDescriptor {
            sha256: Some(good.clone()),
            ..hashless.clone()
        };
        assert_eq!(verify_descriptor(&verified), Verification::Verified);

        // Tamper the binary → drift naming both hashes.
        std::fs::write(&bin, b"tampered-bytes").unwrap();
        match verify_descriptor(&verified) {
            Verification::HashDrift { expected, actual } => {
                assert_eq!(expected, good);
                assert_eq!(actual, pack::sha256_hex(b"tampered-bytes"));
            }
            other => panic!("expected drift, got {other:?}"),
        }

        // A recorded hash over a missing binary is drift too (never a silent pass).
        std::fs::remove_file(&bin).unwrap();
        assert!(matches!(
            verify_descriptor(&verified),
            Verification::HashDrift { .. }
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// D-87: a hashless descriptor that carries git provenance (`git_url`) is
    /// `UnverifiedFromSource`, NOT `UnverifiedLocal` — and, crucially, NOT `HashDrift`, so
    /// `spawn_verified` (whose only refusal is `HashDrift`) admits a from-source plugin exactly as
    /// it admits an `--dir` local one. A `git_url` alongside a recorded `sha256` still verifies by
    /// hash (provenance does not disable integrity enforcement when a hash IS present).
    #[test]
    fn verify_descriptor_labels_from_source_and_spawn_verified_admits_it() {
        let from_source = PluginDescriptor {
            program: "/opt/flux-plugin-acme-manager".into(),
            git_url: Some("https://gitlab.example/group/flux-plugin-acme-manager.git".into()),
            git_commit: Some("abc123".into()),
            ..Default::default()
        };
        assert_eq!(
            verify_descriptor(&from_source),
            Verification::UnverifiedFromSource
        );
        // The property spawn_verified relies on: a from-source descriptor is never HashDrift, so it
        // is never a spawn refusal.
        assert!(
            !matches!(
                verify_descriptor(&from_source),
                Verification::HashDrift { .. }
            ),
            "a from-source descriptor must never be a HashDrift refusal"
        );

        // Provenance without a hash on a plain local descriptor stays UnverifiedLocal.
        let plain = PluginDescriptor {
            program: "/opt/flux-plugin-x".into(),
            ..Default::default()
        };
        assert_eq!(verify_descriptor(&plain), Verification::UnverifiedLocal);
    }

    // ===========================================================================
    // D-31: host-terminated PostgreSQL auth handshake (the `pg` module) against a scripted
    // PG-server stub over a real loopback TcpListener. Hermetic — no external postgres.
    // ===========================================================================

    /// Build a tagged backend message: 1 byte tag + int32 length (incl. itself) + body.
    fn pg_frame(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut m = vec![tag];
        m.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        m.extend_from_slice(body);
        m
    }

    /// Read one tagged frontend message from the client on the server side.
    async fn pg_read_tagged(sock: &mut tokio::net::TcpStream) -> (u8, Vec<u8>) {
        use tokio::io::AsyncReadExt;
        let mut hdr = [0u8; 5];
        sock.read_exact(&mut hdr).await.unwrap();
        let len = i32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        let mut body = vec![0u8; len - 4];
        sock.read_exact(&mut body).await.unwrap();
        (hdr[0], body)
    }

    /// Read the untagged StartupMessage (int32 length, then length-4 body).
    async fn pg_read_startup(sock: &mut tokio::net::TcpStream) {
        use tokio::io::AsyncReadExt;
        let mut lenbuf = [0u8; 4];
        sock.read_exact(&mut lenbuf).await.unwrap();
        let len = i32::from_be_bytes(lenbuf) as usize;
        let mut body = vec![0u8; len - 4];
        sock.read_exact(&mut body).await.unwrap();
    }

    /// The trailing startup frames a successful auth is followed by: server_version + ReadyForQuery.
    fn pg_ready_frames() -> Vec<u8> {
        let mut out = Vec::new();
        let mut ps = b"server_version\0".to_vec();
        ps.extend_from_slice(b"16.2\0");
        out.extend(pg_frame(b'S', &ps));
        out.extend(pg_frame(b'K', &{
            let mut k = 4321i32.to_be_bytes().to_vec();
            k.extend_from_slice(&8765i32.to_be_bytes());
            k
        }));
        out.extend(pg_frame(b'Z', b"I"));
        out
    }

    #[derive(Clone, Copy, PartialEq)]
    enum ScramMode {
        Ok,
        WrongPassword,
        BadServerSig,
        /// Server-first reports an iteration count one above `pg::MAX_SCRAM_ITERATIONS` — must be
        /// rejected before any PBKDF2 work (D-52), not silently computed.
        HugeIterations,
    }

    /// A scripted PostgreSQL server speaking SCRAM-SHA-256 (no channel binding). Reuses the host's own
    /// crypto for the server-side keys so the success path produces a correct server signature the
    /// client must accept, and `BadServerSig` a wrong one the client must REJECT (the MITM guard).
    async fn spawn_scram_server(password: &'static str, mode: ScramMode) -> u16 {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            pg_read_startup(&mut sock).await;

            // AuthenticationSASL: mechanism list "SCRAM-SHA-256" (NUL-separated, extra trailing NUL).
            let mut sasl = 10i32.to_be_bytes().to_vec();
            sasl.extend_from_slice(b"SCRAM-SHA-256\0\0");
            sock.write_all(&pg_frame(b'R', &sasl)).await.unwrap();

            // SASLInitialResponse: mechanism CString + int32 len + client-first ("n,,n=,r=<nonce>").
            let (_tag, body) = pg_read_tagged(&mut sock).await;
            let nul = body.iter().position(|&b| b == 0).unwrap();
            let client_first = String::from_utf8_lossy(&body[nul + 5..]).into_owned();
            let client_first_bare = client_first.trim_start_matches("n,,").to_string();
            let client_nonce = client_first_bare.split("r=").nth(1).unwrap().to_string();

            let server_nonce = "3rfcNHYJY1ZVvWVs7jserverpart";
            let combined = format!("{client_nonce}{server_nonce}");
            let salt = [
                7u8, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67,
            ];
            let salt_b64 = crate::pg::base64_encode(&salt);
            let iterations: u32 = if mode == ScramMode::HugeIterations {
                crate::pg::MAX_SCRAM_ITERATIONS + 1
            } else {
                4096
            };
            let server_first = format!("r={combined},s={salt_b64},i={iterations}");
            let mut cont = 11i32.to_be_bytes().to_vec();
            cont.extend_from_slice(server_first.as_bytes());
            sock.write_all(&pg_frame(b'R', &cont)).await.unwrap();

            if mode == ScramMode::HugeIterations {
                // The client must reject an over-ceiling iteration count right here, before PBKDF2,
                // and never send a client-final — nothing more for the scripted server to do.
                let _ = sock.shutdown().await;
                return;
            }

            // SASLResponse: client-final "c=biws,r=<combined>,p=<proof>".
            let (_tag, body) = pg_read_tagged(&mut sock).await;
            let client_final = String::from_utf8_lossy(&body).into_owned();
            let client_final_no_proof = client_final.split(",p=").next().unwrap().to_string();

            if mode == ScramMode::WrongPassword {
                // Auth failure after the client-final — an ErrorResponse (like a real server).
                let mut err = Vec::new();
                err.extend_from_slice(b"SFATAL\0");
                err.extend_from_slice(b"C28P01\0");
                err.extend_from_slice(b"Mpassword authentication failed\0");
                err.push(0);
                sock.write_all(&pg_frame(b'E', &err)).await.unwrap();
                let _ = sock.shutdown().await;
                return;
            }

            let auth_message =
                format!("{client_first_bare},{server_first},{client_final_no_proof}");
            let salted = crate::pg::pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations);
            let server_key = crate::pg::hmac_sha256(&salted, b"Server Key");
            let mut server_sig = crate::pg::hmac_sha256(&server_key, auth_message.as_bytes());
            if mode == ScramMode::BadServerSig {
                server_sig[0] ^= 0xff; // corrupt it: the client must reject.
            }
            let server_final = format!("v={}", crate::pg::base64_encode(&server_sig));
            let mut fin = 12i32.to_be_bytes().to_vec();
            fin.extend_from_slice(server_final.as_bytes());
            sock.write_all(&pg_frame(b'R', &fin)).await.unwrap();

            // AuthenticationOk + startup params + ReadyForQuery.
            sock.write_all(&pg_frame(b'R', &0i32.to_be_bytes()))
                .await
                .unwrap();
            sock.write_all(&pg_ready_frames()).await.unwrap();
            let _ = sock.flush().await;
            // Keep the socket briefly so the client's reads complete.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });
        port
    }

    async fn dial_loopback(port: u16) -> flux_system::net::DialStream {
        flux_system::net::dial(
            &flux_system::net::DialTarget::Tcp {
                host: "127.0.0.1".into(),
                port,
            },
            true,
        )
        .await
        .unwrap()
    }

    fn pg_params() -> handshake::HandshakeParams {
        handshake::HandshakeParams {
            user: "app".into(),
            database: "warehouse".into(),
            application_name: "flux-test".into(),
        }
    }

    #[tokio::test]
    async fn pg_scram_handshake_succeeds_and_captures_parameters() {
        let port = spawn_scram_server("pencil", ScramMode::Ok).await;
        let mut stream = dial_loopback(port).await;
        let result = pg::authenticate(
            &mut stream,
            &pg_params(),
            "pencil",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .expect("SCRAM handshake should succeed");
        assert_eq!(result.server_version(), Some("16.2"));
        assert_eq!(result.backend_pid, Some(4321));
        assert_eq!(result.backend_key, Some(8765));
    }

    #[tokio::test]
    async fn pg_scram_rejects_wrong_password() {
        // The server rejects the proof with an ErrorResponse; the terminator surfaces it as an error.
        let port = spawn_scram_server("pencil", ScramMode::WrongPassword).await;
        let mut stream = dial_loopback(port).await;
        let err = pg::authenticate(
            &mut stream,
            &pg_params(),
            "wrong-password",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("28P01") || err.to_lowercase().contains("password"),
            "wrong-password must be surfaced as an auth error: {err}"
        );
    }

    #[tokio::test]
    async fn pg_scram_rejects_bad_server_signature() {
        // MITM guard: even with the right password, a bad server-final `v=` must be rejected.
        let port = spawn_scram_server("pencil", ScramMode::BadServerSig).await;
        let mut stream = dial_loopback(port).await;
        let err = pg::authenticate(
            &mut stream,
            &pg_params(),
            "pencil",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("server signature verification failed"),
            "a corrupt server signature must be rejected: {err}"
        );
    }

    /// D-52: a server-first `i=` one above `MAX_SCRAM_ITERATIONS` must be rejected before any PBKDF2
    /// work, not silently accepted and computed. `MAX+1` (not `u32::MAX`) keeps this test fast (it's
    /// still well within a legitimate-looking magnitude) while proving the ceiling is enforced —
    /// pre-fix this assertion fails because the handshake actually SUCCEEDS (both sides derive the
    /// same keys at that iteration count), not because it hangs.
    #[tokio::test]
    async fn pg_scram_rejects_iteration_count_above_ceiling() {
        let port = spawn_scram_server("pencil", ScramMode::HugeIterations).await;
        let mut stream = dial_loopback(port).await;
        let err = pg::authenticate(
            &mut stream,
            &pg_params(),
            "pencil",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("exceeds the maximum")
                && err.contains(&pg::MAX_SCRAM_ITERATIONS.to_string()),
            "an over-ceiling iteration count must be rejected before PBKDF2: {err}"
        );
    }

    /// A scripted server speaking the legacy MD5 auth method.
    async fn spawn_md5_server() -> u16 {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            pg_read_startup(&mut sock).await;
            // AuthenticationMD5Password: int32(5) + 4-byte salt.
            let mut md5 = 5i32.to_be_bytes().to_vec();
            md5.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
            sock.write_all(&pg_frame(b'R', &md5)).await.unwrap();
            // PasswordMessage ('p') with the md5 token — accept whatever the client sends.
            let (_tag, _body) = pg_read_tagged(&mut sock).await;
            sock.write_all(&pg_frame(b'R', &0i32.to_be_bytes()))
                .await
                .unwrap();
            sock.write_all(&pg_ready_frames()).await.unwrap();
            let _ = sock.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });
        port
    }

    #[tokio::test]
    async fn pg_md5_handshake_succeeds() {
        let port = spawn_md5_server().await;
        let mut stream = dial_loopback(port).await;
        let result = pg::authenticate(
            &mut stream,
            &pg_params(),
            "hunter2",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .expect("MD5 handshake should succeed");
        assert_eq!(result.server_version(), Some("16.2"));
    }

    /// The SCRAM-SHA-256 client derivation matches the RFC 7677 §3 well-known vector (user "user",
    /// password "pencil") — moved here from the `sql` plugin when the handshake became host-terminated.
    #[test]
    fn pg_scram_derivation_matches_rfc7677_vector() {
        let password = "pencil";
        let salt = crate::pg::base64_decode("W22ZaJ0SNY7soEsUEjb6gQ==").unwrap();
        let salted = crate::pg::pbkdf2_hmac_sha256(password.as_bytes(), &salt, 4096);
        let client_key = crate::pg::hmac_sha256(&salted, b"Client Key");
        let stored_key = crate::pg::sha256(&client_key);

        let client_first_bare = "n=user,r=rOprNGfwEbeRWgbNEkqO";
        let server_first =
            "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
        let client_final_no_proof = "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0";
        let auth_message = format!("{client_first_bare},{server_first},{client_final_no_proof}");
        let client_signature = crate::pg::hmac_sha256(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        assert_eq!(
            crate::pg::base64_encode(&proof),
            "dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        let server_key = crate::pg::hmac_sha256(&salted, b"Server Key");
        let server_sig = crate::pg::hmac_sha256(&server_key, auth_message.as_bytes());
        assert_eq!(
            crate::pg::base64_encode(&server_sig),
            "6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="
        );
    }

    #[test]
    fn pg_md5_digest_matches_known_vectors() {
        assert_eq!(crate::pg::md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(
            crate::pg::md5_hex(b"abc"),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }

    /// The `conn.authenticate` host capability end to end: dial a scripted SCRAM server through the
    /// host's own `conn.dial`, then `conn.authenticate` by `auth_purpose` — the credential is resolved
    /// HOST-SIDE from the declared auth method's env (registered with the redactor) and NEVER appears
    /// in the response frame handed back to the plugin.
    #[tokio::test]
    async fn conn_authenticate_terminates_handshake_without_returning_the_password() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-connauth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        let password = "pg-scram-env-password";
        std::env::set_var("FLUX_TEST_PG_PW_D31", password);
        let port = spawn_scram_server("pg-scram-env-password", ScramMode::Ok).await;

        let redactor = flux_secret::Redactor::new();
        let sink = Arc::new(RedactorSink {
            redactor: redactor.clone(),
        });
        // The plugin declares a "password" auth method (env-backed) but does NOT grant that env as a
        // readable `secret` — exactly the D-31 sql manifest shape. `conn` is granted for loopback.
        let manifest = PluginManifest {
            name: "sql".into(),
            auth: vec![AuthMethod {
                purpose: "password".into(),
                env: vec!["FLUX_TEST_PG_PW_D31".into()],
                ..Default::default()
            }],
            capabilities: PluginCapabilities {
                conn: vec!["tcp:127.0.0.1:*".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_secret_sink(sink.clone());

        // Dial through the host, then host-terminate the handshake by declared purpose.
        let dialed = caps
            .handle(
                "conn.dial",
                &json!({"kind": "tcp", "host": "127.0.0.1", "port": port}),
            )
            .await
            .unwrap();
        let conn_id = dialed["conn_id"].as_u64().unwrap();
        let result = caps
            .handle(
                "conn.authenticate",
                &json!({
                    "conn_id": conn_id,
                    "protocol": "postgres",
                    "user": "app",
                    "database": "warehouse",
                    "auth_purpose": "password",
                    "timeout_ms": 5000,
                }),
            )
            .await
            .expect("host-terminated auth should succeed");

        assert_eq!(result["server_version"], "16.2");
        // The password is NOWHERE in the frame the plugin receives.
        let frame = result.to_string();
        assert!(
            !frame.contains(password),
            "conn.authenticate must never return the password to the plugin: {frame}"
        );
        // It WAS registered with the redactor (scrubbed from any captured output).
        assert_eq!(
            redactor.redact(&format!("used {password} to connect")),
            "used [redacted] to connect"
        );

        caps.handle("conn.close", &json!({"conn_id": conn_id}))
            .await
            .ok();
        std::env::remove_var("FLUX_TEST_PG_PW_D31");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ===========================================================================
    // D-196: host-terminated MySQL/MariaDB auth (the `mysql` module) against a scripted
    // MySQL-server stub over a real loopback TcpListener. Hermetic — no external mysqld.
    //
    // HONESTY: these replay hand-crafted server frames. They prove the frame parser and message
    // assembly against bytes this test author wrote — NOT live interop with a real MariaDB.
    // ===========================================================================

    /// The 20-byte scramble every scripted MySQL server below issues.
    const MY_SCRAMBLE: &[u8; 20] = b"ABCDEFGHIJKLMNOPQRST";

    /// Frame a MySQL packet: 3-byte little-endian payload length + 1-byte sequence id + payload.
    fn my_packet(seq: u8, payload: &[u8]) -> Vec<u8> {
        let mut m = (payload.len() as u32).to_le_bytes()[..3].to_vec();
        m.push(seq);
        m.extend_from_slice(payload);
        m
    }

    /// Read one MySQL packet from the client on the server side, returning its payload.
    async fn my_read_packet(sock: &mut tokio::net::TcpStream) -> Vec<u8> {
        use tokio::io::AsyncReadExt;
        let mut hdr = [0u8; 4];
        sock.read_exact(&mut hdr).await.unwrap();
        let len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], 0]) as usize;
        let mut body = vec![0u8; len];
        sock.read_exact(&mut body).await.unwrap();
        body
    }

    /// A Handshake v10 greeting advertising `auth_plugin` and 4.1 + secure-connection +
    /// plugin-auth + deprecate-EOF.
    fn my_greeting(auth_plugin: &str) -> Vec<u8> {
        let caps: u32 = 0x0108_A20D;
        let mut p = vec![10u8];
        p.extend_from_slice(b"11.4.2-MariaDB\0");
        p.extend_from_slice(&77u32.to_le_bytes()); // connection id
        p.extend_from_slice(&MY_SCRAMBLE[..8]);
        p.push(0); // filler
        p.extend_from_slice(&(caps as u16).to_le_bytes());
        p.push(45); // charset
        p.extend_from_slice(&[0, 0]); // status flags
        p.extend_from_slice(&((caps >> 16) as u16).to_le_bytes());
        p.push(21); // auth-plugin-data length (20 + NUL)
        p.extend_from_slice(&[0u8; 10]); // reserved
        p.extend_from_slice(&MY_SCRAMBLE[8..]); // 12 bytes
        p.push(0); // scramble terminator
        p.extend_from_slice(auth_plugin.as_bytes());
        p.push(0);
        p
    }

    fn my_ok_packet() -> Vec<u8> {
        vec![0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]
    }

    fn my_err_packet(code: u16, sqlstate: &str, message: &str) -> Vec<u8> {
        let mut p = vec![0xff];
        p.extend_from_slice(&code.to_le_bytes());
        p.push(b'#');
        p.extend_from_slice(sqlstate.as_bytes());
        p.extend_from_slice(message.as_bytes());
        p
    }

    #[derive(Clone, Copy, PartialEq)]
    enum MyMode {
        /// Native password, correct credential accepted.
        Ok,
        /// Native password, but the server rejects the token with ERR 1045.
        WrongPassword,
        /// The greeting advertises `caching_sha2_password` (the MySQL 8.0+ default).
        CachingSha2,
        /// Native greeting, then an AuthSwitchRequest to `ed25519` mid-handshake.
        Ed25519Switch,
    }

    /// A scripted MySQL server. On the success path it recomputes the expected
    /// `mysql_native_password` token with the host's own helper, so the test proves the client's
    /// scramble arithmetic rather than merely that it sent 20 bytes.
    async fn spawn_mysql_server(password: &'static str, mode: MyMode) -> u16 {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();

            let advertised = match mode {
                MyMode::CachingSha2 => "caching_sha2_password",
                _ => "mysql_native_password",
            };
            sock.write_all(&my_packet(0, &my_greeting(advertised)))
                .await
                .unwrap();

            if mode == MyMode::CachingSha2 {
                // The client must refuse before sending anything — nothing left to script.
                let _ = sock.shutdown().await;
                return;
            }

            let response = my_read_packet(&mut sock).await;

            if mode == MyMode::Ed25519Switch {
                let mut sw = vec![0xfe];
                sw.extend_from_slice(b"ed25519\0");
                sw.extend_from_slice(MY_SCRAMBLE);
                sw.push(0);
                sock.write_all(&my_packet(2, &sw)).await.unwrap();
                let _ = sock.shutdown().await;
                return;
            }

            // Parse HandshakeResponse41 far enough to reach the 1-byte-prefixed auth response:
            // 4 caps + 4 max-packet + 1 charset + 23 reserved, then the NUL-terminated user.
            let user_start = 32;
            let user_end =
                user_start + response[user_start..].iter().position(|&b| b == 0).unwrap();
            let auth_len = response[user_end + 1] as usize;
            let got = &response[user_end + 2..user_end + 2 + auth_len];
            let want = crate::mysql::native_password(password, MY_SCRAMBLE);

            let reply = if mode == MyMode::Ok && got == want.as_slice() {
                my_ok_packet()
            } else {
                my_err_packet(
                    1045,
                    "28000",
                    "Access denied for user 'app'@'localhost' (using password: YES)",
                )
            };
            sock.write_all(&my_packet(2, &reply)).await.unwrap();
            let _ = sock.shutdown().await;
        });
        port
    }

    fn my_params() -> handshake::HandshakeParams {
        handshake::HandshakeParams {
            user: "app".into(),
            database: "warehouse".into(),
            application_name: "flux-test".into(),
        }
    }

    #[tokio::test]
    async fn mysql_native_password_handshake_succeeds_and_captures_capabilities() {
        let port = spawn_mysql_server("pencil", MyMode::Ok).await;
        let mut stream = dial_loopback(port).await;
        let result = crate::mysql::authenticate(
            &mut stream,
            &my_params(),
            "pencil",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .expect("native-password handshake should succeed");

        assert_eq!(result.server_version(), Some("11.4.2-MariaDB"));
        assert_eq!(result.backend_pid, Some(77));
        // D-197 needs CLIENT_DEPRECATE_EOF (0x0100_0000) to know the result-set shape.
        let caps = result.capabilities.expect("mysql must report capabilities");
        assert_ne!(
            caps & 0x0100_0000,
            0,
            "CLIENT_DEPRECATE_EOF must be negotiated when the server offers it: {caps:#x}"
        );
        // CLIENT_LOCAL_FILES (0x80) must never be announced — it lets a hostile server ask the host
        // to read a local file (`LOAD DATA LOCAL INFILE`).
        assert_eq!(
            caps & 0x0000_0080,
            0,
            "CLIENT_LOCAL_FILES must never be negotiated: {caps:#x}"
        );
    }

    #[tokio::test]
    async fn mysql_rejects_wrong_password() {
        let port = spawn_mysql_server("pencil", MyMode::WrongPassword).await;
        let mut stream = dial_loopback(port).await;
        let err = crate::mysql::authenticate(
            &mut stream,
            &my_params(),
            "wrong-password",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("1045") && err.contains("28000"),
            "the server's ERR code and SQLSTATE must survive: {err}"
        );
    }

    #[tokio::test]
    async fn mysql_rejects_caching_sha2_password_by_name() {
        let port = spawn_mysql_server("pencil", MyMode::CachingSha2).await;
        let mut stream = dial_loopback(port).await;
        let err = crate::mysql::authenticate(
            &mut stream,
            &my_params(),
            "pencil",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("caching_sha2_password") && err.contains("mysql_native_password"),
            "the error must name the unsupported plugin AND the workaround: {err}"
        );
    }

    #[tokio::test]
    async fn mysql_rejects_ed25519_auth_switch_by_name() {
        // The mid-handshake AuthSwitchRequest path: a clean named error, not a hang.
        let port = spawn_mysql_server("pencil", MyMode::Ed25519Switch).await;
        let mut stream = dial_loopback(port).await;
        let err = crate::mysql::authenticate(
            &mut stream,
            &my_params(),
            "pencil",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("ed25519"),
            "an auth-switch to an unsupported plugin must name it: {err}"
        );
    }

    /// A pre-4.1 "old auth switch request" is a bare `0xfe` packet: no plugin name, no nonce.
    /// Answering it with a native-password token would derive that token from the password ALONE,
    /// destroying the replay resistance the scheme rests on — so it must be refused, not answered.
    #[tokio::test]
    async fn mysql_refuses_a_nonce_free_auth_downgrade() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            sock.write_all(&my_packet(0, &my_greeting("mysql_native_password")))
                .await
                .unwrap();
            let _ = my_read_packet(&mut sock).await;
            // The bare 0xfe downgrade request.
            sock.write_all(&my_packet(2, &[0xfe])).await.unwrap();
            let _ = sock.shutdown().await;
        });
        let mut stream = dial_loopback(port).await;
        let err = crate::mysql::authenticate(
            &mut stream,
            &my_params(),
            "pencil",
            Some(std::time::Duration::from_secs(5)),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("downgrade"),
            "a nonce-free auth switch must be refused by name: {err}"
        );
    }

    /// Directly: a short scramble can never yield a token, whatever path reaches it.
    #[test]
    fn mysql_native_password_requires_a_full_scramble() {
        assert!(crate::mysql::auth_response_for("mysql_native_password", "pw", &[]).is_err());
        assert!(
            crate::mysql::auth_response_for("mysql_native_password", "pw", &MY_SCRAMBLE[..8])
                .is_err(),
            "a truncated scramble must be refused, not silently padded"
        );
        assert!(
            crate::mysql::auth_response_for("mysql_native_password", "pw", MY_SCRAMBLE).is_ok()
        );
    }

    /// The vendored SHA-1 against RFC 3174 vectors. Vendored rather than depended on for the same
    /// reason `pg.rs` vendors MD5 — the *server* picks the algorithm, so it is not a security
    /// boundary we chose — which makes a known-vector test the thing that keeps it honest.
    #[test]
    fn mysql_sha1_matches_known_vectors() {
        let hex = |b: [u8; 20]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(
            hex(crate::mysql::sha1(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex(crate::mysql::sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(crate::mysql::sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    /// An empty password sends an empty response — the protocol's own convention, and a case a
    /// naive XOR implementation would get wrong by emitting 20 bytes of `SHA1("")` material.
    #[test]
    fn mysql_empty_password_sends_an_empty_token() {
        assert!(crate::mysql::native_password("", MY_SCRAMBLE).is_empty());
        assert_eq!(
            crate::mysql::native_password("pencil", MY_SCRAMBLE).len(),
            20
        );
    }

    /// The D-31 invariant, re-asserted on the MySQL path: `conn.authenticate` resolves the credential
    /// host-side and the password appears NOWHERE in the frame handed back to the plugin.
    #[tokio::test]
    async fn conn_authenticate_terminates_mysql_handshake_without_returning_the_password() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-connauth-my-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        let password = "mariadb-env-password";
        std::env::set_var("FLUX_TEST_MY_PW_D196", password);
        let port = spawn_mysql_server("mariadb-env-password", MyMode::Ok).await;

        let redactor = flux_secret::Redactor::new();
        let sink = Arc::new(RedactorSink {
            redactor: redactor.clone(),
        });
        let manifest = PluginManifest {
            name: "sql".into(),
            auth: vec![AuthMethod {
                purpose: "password".into(),
                env: vec!["FLUX_TEST_MY_PW_D196".into()],
                ..Default::default()
            }],
            capabilities: PluginCapabilities {
                conn: vec!["tcp:127.0.0.1:*".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_secret_sink(sink.clone());

        let dialed = caps
            .handle(
                "conn.dial",
                &json!({"kind": "tcp", "host": "127.0.0.1", "port": port}),
            )
            .await
            .unwrap();
        let conn_id = dialed["conn_id"].as_u64().unwrap();
        let result = caps
            .handle(
                "conn.authenticate",
                &json!({
                    "conn_id": conn_id,
                    "protocol": "mariadb",
                    "user": "app",
                    "database": "warehouse",
                    "auth_purpose": "password",
                    "timeout_ms": 5000,
                }),
            )
            .await
            .expect("host-terminated mariadb auth should succeed");

        assert_eq!(result["server_version"], "11.4.2-MariaDB");
        assert!(
            result["capabilities"].is_number(),
            "the negotiated capability flags are reported for diagnosis: {result}"
        );
        let frame = result.to_string();
        assert!(
            !frame.contains(password),
            "conn.authenticate must never return the password to the plugin: {frame}"
        );
        assert_eq!(
            redactor.redact(&format!("used {password} to connect")),
            "used [redacted] to connect"
        );

        caps.handle("conn.close", &json!({"conn_id": conn_id}))
            .await
            .ok();
        std::env::remove_var("FLUX_TEST_MY_PW_D196");
        std::fs::remove_dir_all(&dir).ok();
    }

    // -----------------------------------------------------------------

    /// GL-031: a plugin op that declares a secret-like field has that field's value MASKED wherever
    /// the host echoes it — the stringified result `PluginTool::execute` produces, the dry-run input
    /// preview, and audit. This test locks the exact masking `execute` applies (it stringifies the
    /// value through `to_string_pretty` after `redact_secret_fields`), including the nested/array
    /// case a flat JSON pointer could not reach, and proves non-secret fields survive untouched.
    #[test]
    fn redact_secret_fields_masks_declared_fields_at_any_depth() {
        let fields = vec!["value".to_string()];

        // Flat field (a CI variable write's `value` in dry-run input echo and its response).
        let mut flat = json!({ "key": "DEPLOY_TOKEN", "value": "s3cr3t-token" });
        redact_secret_fields(&mut flat, &fields);
        assert_eq!(flat["key"], "DEPLOY_TOKEN", "non-secret field is untouched");
        assert_eq!(flat["value"], REDACTED_MARKER);

        // Nested inside an array of `{key, value}` pipeline-variable objects.
        let mut nested =
            json!({ "ref": "main", "variables": [{ "key": "K", "value": "hunter2" }] });
        redact_secret_fields(&mut nested, &fields);
        assert_eq!(nested["ref"], "main");
        assert_eq!(nested["variables"][0]["key"], "K");
        assert_eq!(nested["variables"][0]["value"], REDACTED_MARKER);

        // The stringified form `PluginTool::execute` prints must not contain the raw secret.
        let rendered = serde_json::to_string_pretty(&nested).unwrap();
        assert!(
            !rendered.contains("hunter2"),
            "raw secret leaked into stringified result: {rendered}"
        );
        assert!(rendered.contains(REDACTED_MARKER));

        // No declared fields → the value is returned verbatim (the common case).
        let mut untouched = json!({ "value": "kept" });
        redact_secret_fields(&mut untouched, &[]);
        assert_eq!(untouched["value"], "kept");
    }
}
