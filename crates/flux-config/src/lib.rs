//! `flux-config` — layered project/user configuration for the `flux` binary.
//!
//! Two files are read and merged: `~/.flux/config.toml` (user defaults) then
//! `<cwd>/.flux/config.toml` (project, takes precedence). A missing file is not an error — it
//! contributes nothing; a malformed file is an error. CLI flags layer on top of the result (the
//! caller resolves that). The config carries the coder-style permission rules, an optional default
//! model, an optional [`AuthorizationPolicy`] (extends [`flux_policy::default_local_grants`]), and
//! scoped private-network egress grants. Filesystem discovery and atomic persistence live in the
//! guarded outer control plane; this crate parses, merges, and serializes injected documents.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::io::Write as _;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use flux_core::{Error, Result};
use flux_policy::AuthorizationPolicy;

/// Coder-style permission rules (`read`, `Bash(git:*)`, …): deny wins, then allow, else prompt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// A private-network grant for one egress caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PrivateNetGrant {
    /// `true` means this caller may reach any private host; `false` means none.
    Enabled(bool),
    /// Only these host patterns may reach private addresses.
    Hosts(Vec<String>),
}

impl Default for PrivateNetGrant {
    fn default() -> Self {
        Self::Enabled(false)
    }
}

impl PrivateNetGrant {
    fn is_default(&self) -> bool {
        matches!(self, Self::Enabled(false))
    }

    fn to_hosts(&self) -> Vec<String> {
        match self {
            Self::Enabled(true) => vec!["*".to_string()],
            Self::Enabled(false) => Vec::new(),
            Self::Hosts(hosts) => hosts.clone(),
        }
    }
}

/// Scoped private-network egress grants. Plugin grants are keyed by plugin manifest name;
/// per-endpoint grants are keyed by `"<plugin>:<endpoint_name>"` (finer than a whole plugin).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivateNetConfig {
    /// The family-wide `web` egress scope: grants private-network access to every native `flux-web`
    /// op (`http.request`, `web.fetch`, `browser.*`) — the one policy the whole web family answers
    /// to. (Replaced the per-tool `web_fetch` key in D-120's clean cutover; a legacy `web_fetch = …`
    /// entry in an old config is now silently ignored — migrate it to `web`.)
    #[serde(default, skip_serializing_if = "PrivateNetGrant::is_default")]
    pub web: PrivateNetGrant,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, PrivateNetGrant>,
    /// Per-endpoint grants, keyed by `"<plugin>:<endpoint_name>"`. Merged on top of the
    /// owning plugin's grant, so an endpoint can be granted a private host the plugin as a
    /// whole was not (and vice versa).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub endpoints: BTreeMap<String, PrivateNetGrant>,
}

impl PrivateNetConfig {
    fn is_default(&self) -> bool {
        self.web.is_default() && self.plugins.is_empty() && self.endpoints.is_empty()
    }
}

/// Endpoint-discovery / cross-plugin credential brokerage grants (D-27). Deny-by-default: a consumer
/// plugin can only have a credential owned by a *different* provider plugin materialized on its behalf
/// if an operator listed the `(consumer, provider)` pair here — exactly like the `process`/`conn`/
/// `secrets` allow-lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointConfig {
    /// Cross-plugin credential grants, each `"<consumer>:<provider>"` (or `"<consumer>:*"` to let a
    /// consumer use any provider's credentials). No matching entry → no cross-plugin resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cross_plugin_credentials: Vec<String>,
    /// Statically-declared endpoints (`[[endpoint.static]]`) — the declarative alternative to
    /// `flux endpoint add`. Each is a weak reference (no secret; the credential is a *location*), put
    /// into the session registry at startup so it surfaces the endpoint group, lists, and resolves.
    /// Held as plain strings here (kept a `flux-secret`-free leaf); the surface crate validates and
    /// converts them into `EndpointRef`s.
    #[serde(default, rename = "static", skip_serializing_if = "Vec::is_empty")]
    pub static_endpoints: Vec<StaticEndpoint>,
}

/// One `[[endpoint.static]]` declaration: a named, config-bound endpoint. Fields mirror the weak
/// `EndpointRef` (id + bare url + product/protocol hints + a credential *reference* + labels) — never
/// a secret value. Validation (credential-free url, parseable credential ref, non-`@endpoint/` id)
/// happens in the surface crate against the same rules as `flux endpoint add`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticEndpoint {
    /// The named reference id (a bare name, e.g. `pg-prod`); not an `@endpoint/…` (discovered) id.
    pub id: String,
    /// Bare `scheme://host[:port][/path]` — never with embedded credentials.
    pub url: String,
    /// Product class (`postgres`, …); optional.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub product: String,
    /// Wire-protocol hint (`postgres`, `http`, …); optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Credential *location* in `scheme/...` form (`env/PGPASSWORD`, `kubernetes/<ns>/<name>/<key>`,
    /// `plugin/<p>/<i>/<slot>`); optional (unauthenticated when omitted). Never a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    /// Non-secret labels (region, tags) for display/filtering.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub labels: std::collections::BTreeMap<String, String>,
}

impl EndpointConfig {
    fn is_default(&self) -> bool {
        self.cross_plugin_credentials.is_empty() && self.static_endpoints.is_empty()
    }
}

/// The `[skills]` table — skill-discovery settings (L-02).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillsConfig {
    /// Custom skill directories, layered **above** the built-in well-known set (`.flux/skills`,
    /// `.claude/skills`, `~/.flux/skills`, …). Unlike the permission lists, order here is semantic
    /// — earlier dirs win skill-name clashes — so the merge puts **project** dirs before user dirs
    /// (CLI flags layer on top of all of these; the caller resolves that). Relative paths resolve
    /// against the workspace root; a leading `~/` expands to the home directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
}

/// Provenance of one configured skill root after user/project layering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDirOrigin {
    /// Repository-controlled `.flux/config.toml`.
    Project,
    /// Trusted operator-controlled `~/.flux/config.toml`.
    User,
}

/// A configured skill directory paired with the config layer that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredSkillDir {
    pub path: String,
    pub origin: SkillDirOrigin,
}

/// Agent-runtime selection and declarative model-backed stages.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// `adaptive` or a workspace-relative Flux-Lang source file. Absent selects `adaptive`.
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_spec: Option<String>,
    /// Maximum decision/batch iterations in the authored outer loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<usize>,
    /// Named model stages registered as typed guarded operations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stages: BTreeMap<String, ModelStageConfig>,
    /// Policy for the shipped adaptive loop's built-in model stages.
    #[serde(default, skip_serializing_if = "AdaptiveAgentConfig::is_default")]
    pub adaptive: AdaptiveAgentConfig,
}

impl AgentConfig {
    fn is_default(&self) -> bool {
        self.loop_spec.is_none()
            && self.max_iterations.is_none()
            && self.stages.is_empty()
            && self.adaptive.is_default()
    }
}

/// Resource and model policy for one logical adaptive turn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveAgentConfig {
    /// Total provider-call ceiling across intent, repairs, exploration, and decision resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_model_calls: Option<usize>,
    /// Intent-stage overrides; absent values inherit the agent settings.
    #[serde(default, skip_serializing_if = "AdaptiveStageConfig::is_default")]
    pub intent: AdaptiveStageConfig,
    /// Exploration-stage overrides; absent values inherit the agent settings.
    #[serde(default, skip_serializing_if = "AdaptiveStageConfig::is_default")]
    pub explore: AdaptiveStageConfig,
}

impl AdaptiveAgentConfig {
    fn is_default(&self) -> bool {
        self.max_model_calls.is_none() && self.intent.is_default() && self.explore.is_default()
    }
}

/// Optional policy overrides for one built-in adaptive model stage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveStageConfig {
    /// Same-provider model override; a provider prefix must match the agent provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider reasoning-effort spelling (`low`, `medium`, `high`, `xhigh`, or `max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Per-call output-token ceiling; absent inherits the agent ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Provider-call ceiling for this stage within one logical turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<usize>,
}

impl AdaptiveStageConfig {
    fn is_default(&self) -> bool {
        self.model.is_none()
            && self.effort.is_none()
            && self.max_tokens.is_none()
            && self.max_calls.is_none()
    }
}

/// A config-defined model stage. The input/output schemas are the stage's direct operation
/// contract; no common stage-result envelope is imposed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelStageConfig {
    /// Stable model instruction for this stage.
    pub prompt: String,
    /// JSON Schema accepted by the operation.
    pub input_schema: serde_json::Value,
    /// JSON Schema returned by the operation.
    pub output_schema: serde_json::Value,
    /// Optional model override; absent inherits the agent model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional gather-only native operation ceiling for the stage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Per-call generation cap.
    #[serde(default = "default_stage_max_tokens")]
    pub max_tokens: u32,
    /// Optional provider reasoning-effort spelling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

fn default_stage_max_tokens() -> u32 {
    4096
}

impl SkillsConfig {
    fn is_default(&self) -> bool {
        self.dirs.is_empty()
    }
}

/// The `[workspace]` table — filesystem access widening (C-21).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Additional **read-only** roots the CLI may read/glob/grep under, beyond the cwd. A leading `~/`
    /// expands to the home directory; relative paths resolve against the cwd. Writes stay confined to the
    /// cwd. Mirrors the `--add-dir` flag.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_dirs: Vec<String>,
    /// Lift filesystem confinement entirely (read + write anywhere) — the `--allow-all-paths` hatch.
    #[serde(default)]
    pub allow_all: bool,
}

impl WorkspaceConfig {
    fn is_default(&self) -> bool {
        self.add_dirs.is_empty() && !self.allow_all
    }
}

/// The `[sandbox]` table — OS-level process confinement (bubblewrap on Linux, Seatbelt on macOS)
/// applied at `flux-system`'s process choke point (D-130). Opt-in and default off. Real backends
/// ship on Linux (bubblewrap, D-131) and macOS (Seatbelt, D-132); on a platform without one
/// (Windows), or when the backend is present but unusable, `enabled` degrades with a one-line
/// startup warning and `require` fails closed at startup. `#[serde(deny_unknown_fields)]` turns a
/// typo'd key (e.g. `requre = true`) into a hard parse error rather than a silently-dropped —
/// and thus fail-open — setting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    /// Turn on OS sandboxing for spawned processes (shell ops + plugin subprocesses).
    #[serde(default)]
    pub enabled: bool,
    /// Fail closed instead of warn-and-continue when no sandbox backend is available. Implies
    /// `enabled`.
    #[serde(default)]
    pub require: bool,
    /// Whether sandboxed processes may reach the network. Absent means the unrestricted default;
    /// `false` closes the sandbox's network namespace/profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    /// Extra writable paths sandboxed processes may write to, beyond the workspace root, named
    /// roots, `/tmp`/`$TMPDIR`, and the toolchain caches. A leading `~/` expands to the home
    /// directory (mirrors `[workspace] add_dirs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable: Vec<String>,
}

impl SandboxConfig {
    fn is_default(&self) -> bool {
        !self.enabled && !self.require && self.network.is_none() && self.writable.is_empty()
    }
}

/// The merged flux configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Default `provider/model` spec (a CLI `--model` flag overrides this).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Deprecated compatibility flag. If true, the native web family (the `web` scope) gets a
    /// private-net `*` grant; plugins still require `[private_net.plugins]` grants.
    #[serde(default)]
    pub allow_private_net: bool,
    /// Scoped private-network egress grants.
    #[serde(default, skip_serializing_if = "PrivateNetConfig::is_default")]
    pub private_net: PrivateNetConfig,
    /// Endpoint-discovery / cross-plugin credential brokerage grants (D-27).
    #[serde(default, skip_serializing_if = "EndpointConfig::is_default")]
    pub endpoint: EndpointConfig,
    /// Opt into the generic `bash` op (the `shell` group). Off by default — the agent works through
    /// the dedicated ops; setting this surfaces `bash` as an escape hatch. The CLI exports
    /// `FLUX_ENABLE_BASH` from this so the runtime's `shell` signal fires.
    #[serde(default)]
    pub enable_shell: bool,
    #[serde(default)]
    pub permissions: Permissions,
    /// Agent outer-loop and typed stage configuration.
    #[serde(default, skip_serializing_if = "AgentConfig::is_default")]
    pub agent: AgentConfig,
    /// Extra authorization grants, layered onto the built-in local defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<AuthorizationPolicy>,
    /// Resource ceilings for the agent loop (A-10). All off by default.
    #[serde(default, skip_serializing_if = "Limits::is_default")]
    pub limits: Limits,
    /// Knobs for the HTTP/A2A server surface (`flux-server`).
    #[serde(default, skip_serializing_if = "ServerConfig::is_default")]
    pub server: ServerConfig,
    /// Skill-discovery settings (custom skill directories, L-02).
    #[serde(default, skip_serializing_if = "SkillsConfig::is_default")]
    pub skills: SkillsConfig,
    /// Non-serialized provenance retained by [`from_sources`]. Programmatic `Config` values leave
    /// this empty and are treated conservatively as project-controlled by
    /// [`Config::skill_dirs_with_origin`].
    #[serde(skip)]
    #[doc(hidden)]
    pub configured_skill_dirs: Vec<ConfiguredSkillDir>,
    /// Filesystem access widening (C-21): extra read-only roots + the unconfined hatch.
    #[serde(default, skip_serializing_if = "WorkspaceConfig::is_default")]
    pub workspace: WorkspaceConfig,
    /// Path to a Chromium binary for the native browser ops (D-121). Absent → `FLUX_BROWSER_BIN` then
    /// a `PATH` search for well-known Chromium binaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_bin: Option<String>,
    /// OS-level process sandboxing (D-130): opt-in bubblewrap/Seatbelt confinement for spawned
    /// processes.
    #[serde(default, skip_serializing_if = "SandboxConfig::is_default")]
    pub sandbox: SandboxConfig,
}

/// The default A2A session TTL (seconds) when `[server] a2a_session_ttl_secs` is absent: 1 hour.
pub const DEFAULT_A2A_SESSION_TTL_SECS: u64 = 3600;

/// The `[server]` table — settings for the HTTP/A2A surface.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// TTL in seconds for sessions minted by the A2A surface (C-18). Absent means the default
    /// [`DEFAULT_A2A_SESSION_TTL_SECS`] (1h); `0` means never prune. Age is measured from a
    /// session's last activity, not its creation — see [`Config::a2a_session_ttl_secs`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a2a_session_ttl_secs: Option<u64>,
    /// RFC 7662 token-introspection endpoint (D-69). Setting this switches `--serve` into
    /// per-request principal auth: every request's bearer is resolved to a principal, sessions
    /// are realm-scoped, and `external_url` becomes required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introspect_url: Option<String>,
    /// Optional introspection client id (`client_secret_basic`); paired with
    /// `introspect_client_secret_env`. Absent → the endpoint is called without client auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introspect_client_id: Option<String>,
    /// The NAME of the environment variable holding the introspection client secret — never the
    /// secret itself (config files are committed; secrets are env refs, as everywhere in flux).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introspect_client_secret_env: Option<String>,
    /// Claim (literal key first, dot-path on miss) that carries the caller's account/tenant id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introspect_account_claim: Option<String>,
    /// Claim carrying roles (JSON array or one space-separated string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introspect_roles_claim: Option<String>,
    /// Reject tokens whose account claim is missing/empty (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introspect_require_account: Option<bool>,
    /// Allow a plain-http introspection endpoint (trusted-network deployments; default false —
    /// bearer tokens transit this connection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introspect_allow_http: Option<bool>,
    /// Externally reachable base URL advertised on the agent card (e.g.
    /// `https://agents.example.com`). Required in principal mode: the card tells clients where to
    /// send bearer tokens, so it must come from config, never the request's Host header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
}

impl ServerConfig {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Resource ceilings for the agent loop. Everything here is opt-in — absent means no ceiling.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// Per-turn token budget: once the turn's accumulated model usage (all tiers) crosses this,
    /// the loop ends the turn honestly instead of consulting the model again. Overridden by
    /// `FLUX_TURN_TOKEN_BUDGET` and the `--turn-budget` flag (flag > env > config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_token_budget: Option<u64>,
}

impl Limits {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Config {
    /// Host patterns allowed to bypass the private-network guard for the whole native web family
    /// (`http.request`, `web.fetch`, `browser.*`) — the `[private_net] web` scope. The deprecated
    /// `allow_private_net` compat flag still widens it to `*` when set.
    pub fn web_private_hosts(&self) -> Vec<String> {
        let mut hosts = self.private_net.web.to_hosts();
        if self.allow_private_net && hosts.is_empty() {
            hosts.push("*".to_string());
        }
        dedupe(hosts)
    }

    /// Host patterns granted to a specific plugin for private-network egress.
    pub fn plugin_private_hosts(&self, plugin: &str) -> Vec<String> {
        self.private_net
            .plugins
            .get(plugin)
            .map(PrivateNetGrant::to_hosts)
            .map(dedupe)
            .unwrap_or_default()
    }

    /// Host patterns granted to a specific plugin **endpoint** for private-network egress: the
    /// plugin-level grant merged with the per-endpoint (`"<plugin>:<endpoint>"`) grant. An
    /// endpoint with no entry of its own gets exactly the plugin-level grant (possibly empty).
    pub fn endpoint_private_hosts(&self, plugin: &str, endpoint: &str) -> Vec<String> {
        let mut hosts = self.private_net.plugins.get(plugin).cloned();
        let key = format!("{plugin}:{endpoint}");
        if let Some(ep) = self.private_net.endpoints.get(&key) {
            hosts = Some(match hosts {
                Some(plugin_grant) => merge_grant(plugin_grant, ep.clone()),
                None => ep.clone(),
            });
        }
        hosts
            .as_ref()
            .map(PrivateNetGrant::to_hosts)
            .map(dedupe)
            .unwrap_or_default()
    }

    /// Whether `consumer` is granted to have `provider`'s credentials materialized on its behalf
    /// (deny-by-default). Matches an exact `"<consumer>:<provider>"` entry or a `"<consumer>:*"`
    /// wildcard. Consuming a credential a plugin *owns* itself is not "cross-plugin" and is never gated
    /// here (the caller only consults this when consumer ≠ provider).
    pub fn cross_plugin_credential_granted(&self, consumer: &str, provider: &str) -> bool {
        self.endpoint
            .cross_plugin_credentials
            .iter()
            .any(|g| g == &format!("{consumer}:{provider}") || g == &format!("{consumer}:*"))
    }

    /// The effective A2A session TTL in seconds: `[server] a2a_session_ttl_secs`, defaulting to
    /// [`DEFAULT_A2A_SESSION_TTL_SECS`] (1h). `0` disables pruning entirely (C-18).
    pub fn a2a_session_ttl_secs(&self) -> u64 {
        self.server
            .a2a_session_ttl_secs
            .unwrap_or(DEFAULT_A2A_SESSION_TTL_SECS)
    }

    /// The configured custom skill directories as paths, in precedence order, with a leading `~/`
    /// expanded to the home directory. Relative paths are left relative — the skill-discovery
    /// composer (`flux_skill::skill_dirs`) resolves them against the workspace root.
    pub fn skill_dir_paths(&self) -> Vec<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.skills
            .dirs
            .iter()
            .map(|d| match (d.strip_prefix("~/"), &home) {
                (Some(rest), Some(h)) => h.join(rest),
                _ => PathBuf::from(d),
            })
            .collect()
    }

    /// Configured skill roots with their trust provenance. This keeps a project config from
    /// smuggling an absolute path into the trusted user-global discovery boundary after merge.
    pub fn skill_dirs_with_origin(&self) -> Vec<(PathBuf, SkillDirOrigin)> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let sources = if self.configured_skill_dirs.is_empty() {
            self.skills
                .dirs
                .iter()
                .cloned()
                .map(|path| ConfiguredSkillDir {
                    path,
                    origin: SkillDirOrigin::Project,
                })
                .collect::<Vec<_>>()
        } else {
            self.configured_skill_dirs.clone()
        };
        sources
            .into_iter()
            .map(|entry| {
                let path = match (entry.path.strip_prefix("~/"), &home) {
                    (Some(rest), Some(home)) => home.join(rest),
                    _ => PathBuf::from(entry.path),
                };
                (path, entry.origin)
            })
            .collect()
    }

    /// The configured extra read-only roots as paths (C-21), with a leading `~/` expanded. Relative
    /// paths are left relative (resolved against the cwd by the caller).
    pub fn workspace_add_dirs(&self) -> Vec<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.workspace
            .add_dirs
            .iter()
            .map(|d| match (d.strip_prefix("~/"), &home) {
                (Some(rest), Some(h)) => h.join(rest),
                _ => PathBuf::from(d),
            })
            .collect()
    }

    /// Whether the config lifts filesystem confinement entirely (C-21).
    pub fn workspace_allow_all(&self) -> bool {
        self.workspace.allow_all
    }

    /// Whether the config turns on OS sandboxing for spawned processes (D-130). `require` implies
    /// this even if `enabled` alone is unset.
    pub fn sandbox_enabled(&self) -> bool {
        self.sandbox.enabled || self.sandbox.require
    }

    /// Whether the config requires a working sandbox backend (fail closed rather than warn).
    pub fn sandbox_require(&self) -> bool {
        self.sandbox.require
    }

    /// The configured sandbox network posture. `None` means the unrestricted default.
    pub fn sandbox_network(&self) -> Option<bool> {
        self.sandbox.network
    }

    /// The configured extra sandbox-writable paths, with a leading `~/` expanded (mirrors
    /// `workspace_add_dirs`). Relative paths are left relative (resolved against the cwd by the
    /// caller).
    pub fn sandbox_writable(&self) -> Vec<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.sandbox
            .writable
            .iter()
            .map(|d| match (d.strip_prefix("~/"), &home) {
                (Some(rest), Some(h)) => h.join(rest),
                _ => PathBuf::from(d),
            })
            .collect()
    }
}

#[cfg(test)]
fn home_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".flux").join("config.toml"))
}

/// Resolve the project config's lexical path against the canonical workspace and reject any
/// existing file or parent-directory symlink whose physical target leaves that workspace.
#[cfg(test)]
fn guarded_project_config_path(cwd: &Path) -> Result<PathBuf> {
    let root = cwd
        .canonicalize()
        .map_err(|error| Error::Config(format!("project config workspace: {error}")))?;
    let path = root.join(".flux").join("config.toml");
    let mut existing = path.as_path();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or_else(|| {
                    Error::Config("project config path has no existing ancestor".to_string())
                })?;
            }
            Err(error) => return Err(Error::Io(error)),
        }
    }
    let physical = existing.canonicalize().map_err(|error| {
        Error::Config(format!(
            "project config path `{}` is not a valid workspace path: {error}",
            path.display()
        ))
    })?;
    if !physical.starts_with(&root) {
        return Err(Error::Config(format!(
            "project config path `{}` resolves outside workspace `{}`",
            path.display(),
            root.display()
        )));
    }
    Ok(path)
}

/// Read a config file, returning `None` if it doesn't exist and erroring if it's malformed.
#[cfg(test)]
fn read_optional(path: &Path) -> Result<Option<Config>> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let cfg = toml::from_str(&s)
                .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
            Ok(Some(cfg))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Parse one injected configuration document. Filesystem discovery belongs to the guarded outer
/// control plane; this L0 contract only interprets bytes and preserves the source in diagnostics.
pub fn parse_source(source: &str, text: &str) -> Result<Config> {
    toml::from_str(text).map_err(|error| Error::Config(format!("{source}: {error}")))
}

/// Parse and merge injected user/project configuration documents. Project values retain their
/// existing precedence over trusted user-global defaults; missing documents contribute defaults.
pub fn from_sources(user: Option<(&str, &str)>, project: Option<(&str, &str)>) -> Result<Config> {
    let parse = |source: Option<(&str, &str)>| -> Result<Config> {
        source
            .map(|(name, text)| parse_source(name, text))
            .transpose()
            .map(|value| value.unwrap_or_default())
    };
    Ok(merge(parse(user)?, parse(project)?))
}

/// Merge new always-allow rules into an injected project config and return the complete serialized
/// document. The guarded outer control plane owns the atomic write.
pub fn render_allow_rules(project: Option<(&str, &str)>, rules: &[String]) -> Result<String> {
    let mut cfg = project
        .map(|(source, text)| parse_source(source, text))
        .transpose()?
        .unwrap_or_default();
    let mut seen = BTreeSet::new();
    cfg.permissions.allow = cfg
        .permissions
        .allow
        .iter()
        .chain(rules)
        .filter(|rule| seen.insert((*rule).clone()))
        .cloned()
        .collect();
    toml::to_string_pretty(&cfg).map_err(|error| Error::Config(error.to_string()))
}

/// Merge `project` onto `user`: lists (and policy grants) concatenate (user first), scalars prefer
/// project, legacy `allow_private_net` is true if either enables it, scoped private-net grants merge.
fn merge(user: Config, project: Config) -> Config {
    let configured_skill_dirs = {
        let mut seen = BTreeSet::new();
        project
            .skills
            .dirs
            .iter()
            .map(|path| (path, SkillDirOrigin::Project))
            .chain(
                user.skills
                    .dirs
                    .iter()
                    .map(|path| (path, SkillDirOrigin::User)),
            )
            .filter(|(path, _)| seen.insert((*path).clone()))
            .map(|(path, origin)| ConfiguredSkillDir {
                path: path.clone(),
                origin,
            })
            .collect::<Vec<_>>()
    };
    Config {
        model: project.model.or(user.model),
        allow_private_net: user.allow_private_net || project.allow_private_net,
        private_net: merge_private_net(user.private_net, project.private_net),
        endpoint: EndpointConfig {
            cross_plugin_credentials: dedupe(
                [
                    user.endpoint.cross_plugin_credentials,
                    project.endpoint.cross_plugin_credentials,
                ]
                .concat(),
            ),
            static_endpoints: merge_static_endpoints(
                user.endpoint.static_endpoints,
                project.endpoint.static_endpoints,
            ),
        },
        enable_shell: user.enable_shell || project.enable_shell,
        permissions: Permissions {
            allow: [user.permissions.allow, project.permissions.allow].concat(),
            deny: [user.permissions.deny, project.permissions.deny].concat(),
        },
        agent: AgentConfig {
            loop_spec: project.agent.loop_spec.or(user.agent.loop_spec),
            max_iterations: project.agent.max_iterations.or(user.agent.max_iterations),
            stages: {
                let mut stages = user.agent.stages;
                stages.extend(project.agent.stages);
                stages
            },
            adaptive: merge_adaptive_agent(user.agent.adaptive, project.agent.adaptive),
        },
        // Concatenate grants like permissions — a project policy refines (adds to) the user's, it
        // doesn't silently discard it. (Previously `project.policy.or(user.policy)` dropped every
        // user grant the moment a project defined any policy block.)
        policy: match (user.policy, project.policy) {
            (None, None) => None,
            (Some(u), None) => Some(u),
            (None, Some(p)) => Some(p),
            (Some(u), Some(p)) => Some(AuthorizationPolicy {
                grants: [u.grants, p.grants].concat(),
            }),
        },
        limits: Limits {
            // A project ceiling overrides the user's (a scalar, not a set — nearest wins).
            turn_token_budget: project
                .limits
                .turn_token_budget
                .or(user.limits.turn_token_budget),
        },
        server: ServerConfig {
            // Same scalar rule throughout: a project value (including an explicit 0/false)
            // overrides the user's.
            a2a_session_ttl_secs: project
                .server
                .a2a_session_ttl_secs
                .or(user.server.a2a_session_ttl_secs),
            introspect_url: project.server.introspect_url.or(user.server.introspect_url),
            introspect_client_id: project
                .server
                .introspect_client_id
                .or(user.server.introspect_client_id),
            introspect_client_secret_env: project
                .server
                .introspect_client_secret_env
                .or(user.server.introspect_client_secret_env),
            introspect_account_claim: project
                .server
                .introspect_account_claim
                .or(user.server.introspect_account_claim),
            introspect_roles_claim: project
                .server
                .introspect_roles_claim
                .or(user.server.introspect_roles_claim),
            introspect_require_account: project
                .server
                .introspect_require_account
                .or(user.server.introspect_require_account),
            introspect_allow_http: project
                .server
                .introspect_allow_http
                .or(user.server.introspect_allow_http),
            external_url: project.server.external_url.or(user.server.external_url),
        },
        // Skill-dir order is name-clash precedence, so the project's dirs come FIRST (project >
        // user) — deliberately the opposite concatenation order from the permission lists, where
        // order carries no meaning.
        skills: SkillsConfig {
            dirs: dedupe([project.skills.dirs, user.skills.dirs].concat()),
        },
        configured_skill_dirs,
        // Extra read-only roots concatenate (project first); the unconfined hatch is true if either sets it.
        workspace: WorkspaceConfig {
            add_dirs: dedupe([project.workspace.add_dirs, user.workspace.add_dirs].concat()),
            allow_all: user.workspace.allow_all || project.workspace.allow_all,
        },
        // Scalar: a project value overrides the user's.
        browser_bin: project.browser_bin.or(user.browser_bin),
        sandbox: merge_sandbox(user.sandbox, project.sandbox),
    }
}

/// Security-directional merge for `[sandbox]`: `enabled`/`require` are OR'd (a project may tighten
/// confinement the user didn't ask for, never loosen a user's `require`); `network` is
/// strictest-wins (either side explicitly closing the network wins, matching `enabled`/`require`'s
/// "either side may only tighten" direction); `writable` concatenates (a documented widening, like
/// `[workspace] add_dirs` — a project may need to declare a build-output dir the sandbox must
/// allow writes to).
fn merge_sandbox(user: SandboxConfig, project: SandboxConfig) -> SandboxConfig {
    SandboxConfig {
        enabled: user.enabled || project.enabled,
        require: user.require || project.require,
        network: match (user.network, project.network) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), _) | (_, Some(true)) => Some(true),
            (None, None) => None,
        },
        writable: dedupe([project.writable, user.writable].concat()),
    }
}

fn merge_adaptive_agent(
    user: AdaptiveAgentConfig,
    project: AdaptiveAgentConfig,
) -> AdaptiveAgentConfig {
    AdaptiveAgentConfig {
        max_model_calls: project.max_model_calls.or(user.max_model_calls),
        intent: merge_adaptive_stage(user.intent, project.intent),
        explore: merge_adaptive_stage(user.explore, project.explore),
    }
}

fn merge_adaptive_stage(
    user: AdaptiveStageConfig,
    project: AdaptiveStageConfig,
) -> AdaptiveStageConfig {
    AdaptiveStageConfig {
        model: project.model.or(user.model),
        effort: project.effort.or(user.effort),
        max_tokens: project.max_tokens.or(user.max_tokens),
        max_calls: project.max_calls.or(user.max_calls),
    }
}

fn merge_private_net(user: PrivateNetConfig, project: PrivateNetConfig) -> PrivateNetConfig {
    PrivateNetConfig {
        web: merge_grant(user.web, project.web),
        plugins: merge_grant_map(user.plugins, project.plugins),
        endpoints: merge_grant_map(user.endpoints, project.endpoints),
    }
}

/// Merge two keyed grant maps (user + project): a key present in both has its grants combined
/// via [`merge_grant`]; project-only keys are added.
fn merge_grant_map(
    mut user: BTreeMap<String, PrivateNetGrant>,
    project: BTreeMap<String, PrivateNetGrant>,
) -> BTreeMap<String, PrivateNetGrant> {
    for (name, grant) in project {
        user.entry(name)
            .and_modify(|existing| *existing = merge_grant(existing.clone(), grant.clone()))
            .or_insert(grant);
    }
    user
}

fn merge_grant(a: PrivateNetGrant, b: PrivateNetGrant) -> PrivateNetGrant {
    match (a, b) {
        (PrivateNetGrant::Enabled(true), _) | (_, PrivateNetGrant::Enabled(true)) => {
            PrivateNetGrant::Enabled(true)
        }
        (PrivateNetGrant::Enabled(false), other) | (other, PrivateNetGrant::Enabled(false)) => {
            other
        }
        (PrivateNetGrant::Hosts(a), PrivateNetGrant::Hosts(b)) => {
            PrivateNetGrant::Hosts(dedupe([a, b].concat()))
        }
    }
}

fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        let trimmed = item.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Merge static endpoint declarations: user first, then project — a project entry with the same `id`
/// overrides the user's (so a repo can retarget a named endpoint), otherwise it is appended.
/// Insertion order is preserved (deterministic display / registry seeding).
fn merge_static_endpoints(
    user: Vec<StaticEndpoint>,
    project: Vec<StaticEndpoint>,
) -> Vec<StaticEndpoint> {
    let mut out = user;
    for ep in project {
        if let Some(slot) = out.iter_mut().find(|e| e.id == ep.id) {
            *slot = ep;
        } else {
            out.push(ep);
        }
    }
    out
}

/// Load and merge `~/.flux/config.toml` (user) then `<cwd>/.flux/config.toml` (project).
#[cfg(test)]
fn load(cwd: &Path) -> Result<Config> {
    let user = match home_config_path() {
        Some(p) => read_optional(&p)?.unwrap_or_default(),
        None => Config::default(),
    };
    let project = read_optional(&guarded_project_config_path(cwd)?)?.unwrap_or_default();
    Ok(merge(user, project))
}

/// Persist allow rules back to the **project** config (`<cwd>/.flux/config.toml`), unioned with
/// whatever is already there (order-preserving, de-duplicated). Creates `.flux/` if needed.
#[cfg(test)]
fn persist_allow_rules(cwd: &Path, rules: &[String]) -> Result<()> {
    let path = guarded_project_config_path(cwd)?;
    let mut cfg = read_optional(&path)?.unwrap_or_default();

    let mut seen = BTreeSet::new();
    let mut merged = Vec::new();
    for r in cfg.permissions.allow.iter().chain(rules.iter()) {
        if seen.insert(r.clone()) {
            merged.push(r.clone());
        }
    }
    cfg.permissions.allow = merged;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let body = toml::to_string_pretty(&cfg).map_err(|e| Error::Config(e.to_string()))?;
    // Write a sibling with create-new semantics, then atomically replace the config. Besides
    // avoiding torn TOML, this prevents the destination file itself from being opened through a
    // symlink between validation and the write.
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let temp = path.with_extension(format!(
        "toml.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let write_result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(Error::Io)?;
        file.write_all(body.as_bytes()).map_err(Error::Io)?;
        file.sync_all().map_err(Error::Io)?;
        std::fs::rename(&temp, &path).map_err(Error::Io)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    write_result?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tool-group manifest (.flux/groups.toml)
// ---------------------------------------------------------------------------

/// The `.flux/groups.toml` manifest: a list of `[[group]]` entries declaring evidence-gated tool
/// groups (name, optional membership `tools`, and `surface_when` signal matches).
#[derive(Debug, Clone, Default, Deserialize)]
struct GroupsManifest {
    #[serde(default, rename = "group")]
    groups: Vec<flux_evidence::ToolGroup>,
}

/// Parse one injected tool-group manifest, retaining its source in diagnostics.
pub fn parse_groups_source(source: &str, text: &str) -> Result<Vec<flux_evidence::ToolGroup>> {
    toml::from_str::<GroupsManifest>(text)
        .map(|manifest| manifest.groups)
        .map_err(|error| Error::Config(format!("{source}: {error}")))
}

/// Merge injected user/project tool-group manifests. A project group replaces a trusted
/// user-global group with the same name, matching the historical precedence.
pub fn groups_from_sources(
    user: Option<(&str, &str)>,
    project: Option<(&str, &str)>,
) -> Result<Vec<flux_evidence::ToolGroup>> {
    let parse = |source: Option<(&str, &str)>| -> Result<Vec<flux_evidence::ToolGroup>> {
        source
            .map(|(name, text)| parse_groups_source(name, text))
            .transpose()
            .map(|value| value.unwrap_or_default())
    };
    Ok(merge_groups(parse(user)?, parse(project)?))
}

#[cfg(test)]
fn home_groups_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".flux").join("groups.toml"))
}

#[cfg(test)]
fn project_groups_path(cwd: &Path) -> PathBuf {
    cwd.join(".flux").join("groups.toml")
}

/// Load user (`~/.flux/groups.toml`) then project (`<cwd>/.flux/groups.toml`) group definitions.
/// A project entry overrides a user entry of the same `name`. Missing files are not an error; a
/// malformed file is skipped (a warning is printed) rather than failing the session.
#[cfg(test)]
fn load_groups(cwd: &Path) -> Vec<flux_evidence::ToolGroup> {
    let mut out: Vec<flux_evidence::ToolGroup> = Vec::new();
    let paths = home_groups_path()
        .into_iter()
        .chain(std::iter::once(project_groups_path(cwd)));
    for p in paths {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue; // missing file: fine
        };
        match toml::from_str::<GroupsManifest>(&text) {
            Ok(m) => {
                for g in m.groups {
                    // Later file (project) overrides an earlier (user) group of the same name.
                    if let Some(slot) = out.iter_mut().find(|e| e.name == g.name) {
                        *slot = g;
                    } else {
                        out.push(g);
                    }
                }
            }
            Err(e) => eprintln!("(ignoring malformed {}: {e})", p.display()),
        }
    }
    out
}

/// Merge built-in groups with config `overrides`: a config group with the same `name` replaces the
/// built-in (so a project can retune surfacing or membership); new names are appended.
pub fn merge_groups(
    base: Vec<flux_evidence::ToolGroup>,
    overrides: Vec<flux_evidence::ToolGroup>,
) -> Vec<flux_evidence::ToolGroup> {
    let mut out = base;
    for g in overrides {
        if let Some(slot) = out.iter_mut().find(|e| e.name == g.name) {
            *slot = g;
        } else {
            out.push(g);
        }
    }
    out
}

/// Serializes the tests that repoint `HOME` — the process env is shared across parallel test
/// threads, so two concurrent `set_var("HOME", …)` tests race and flake.
#[cfg(test)]
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod groups_tests {
    use super::*;

    #[test]
    fn load_and_merge_groups() {
        let dir = std::env::temp_dir().join(format!("flux-cfg-groups-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".flux")).unwrap();
        std::fs::write(
            dir.join(".flux").join("groups.toml"),
            r#"
[[group]]
name = "git"
surface_when = [{ signal = "git_repo" }]

[[group]]
name = "custom"
tools = ["my_op"]
surface_when = [{ kind = "project.signal", signal = "custom" }]
"#,
        )
        .unwrap();
        // HOME points elsewhere so only the project file is read.
        let _home = crate::HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", dir.join("nohome"));
        let cfg = load_groups(&dir);
        assert!(cfg
            .iter()
            .any(|g| g.name == "custom" && g.tools == vec!["my_op".to_string()]));
        let git = cfg.iter().find(|g| g.name == "git").unwrap();
        // `kind` defaulted to project.signal.
        assert_eq!(git.surface_when[0].kind, "project.signal");

        // merge: config "git" replaces a built-in "git"; "custom" is appended.
        let base = vec![flux_evidence::ToolGroup {
            name: "git".into(),
            description: "builtin".into(),
            ..Default::default()
        }];
        let merged = merge_groups(base, cfg);
        let git = merged.iter().find(|g| g.name == "git").unwrap();
        assert!(git.description.is_empty()); // replaced by config (no description)
        assert!(merged.iter().any(|g| g.name == "custom"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-config-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(dir.join(".flux")).unwrap();
        dir
    }

    fn write_project(cwd: &Path, body: &str) {
        std::fs::write(cwd.join(".flux").join("config.toml"), body).unwrap();
    }

    #[test]
    fn unknown_top_level_config_key_is_rejected() {
        let err = toml::from_str::<Config>("future_knob = true").unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn agent_loop_and_typed_model_stages_parse_without_a_common_envelope() {
        let config = toml::from_str::<Config>(
            r#"
[agent]
loop = "loops/support.flux"
max_iterations = 37

[agent.adaptive]
max_model_calls = 9

[agent.adaptive.intent]
model = "google/gemini-2.5-flash"
effort = "low"
max_tokens = 512
max_calls = 2

[agent.adaptive.explore]
max_calls = 6

[agent.stages.classify]
prompt = "Classify the support request and return its typed result."
input_schema = { type = "object", properties = { text = { type = "string" } }, required = ["text"], additionalProperties = false }
output_schema = { type = "object", properties = { queue = { type = "string" }, urgent = { type = "boolean" } }, required = ["queue", "urgent"], additionalProperties = false }
tools = ["search"]
model = "google/gemini-2.5-flash"
max_tokens = 768
effort = "low"
"#,
        )
        .unwrap();

        assert_eq!(
            config.agent.loop_spec.as_deref(),
            Some("loops/support.flux")
        );
        assert_eq!(config.agent.max_iterations, Some(37));
        assert_eq!(config.agent.adaptive.max_model_calls, Some(9));
        assert_eq!(
            config.agent.adaptive.intent.model.as_deref(),
            Some("google/gemini-2.5-flash")
        );
        assert_eq!(config.agent.adaptive.intent.effort.as_deref(), Some("low"));
        assert_eq!(config.agent.adaptive.intent.max_tokens, Some(512));
        assert_eq!(config.agent.adaptive.intent.max_calls, Some(2));
        assert_eq!(config.agent.adaptive.explore.max_calls, Some(6));
        let stage = &config.agent.stages["classify"];
        assert_eq!(stage.input_schema["required"][0], "text");
        assert_eq!(stage.output_schema["required"][0], "queue");
        assert_eq!(stage.tools, vec!["search"]);
        assert_eq!(stage.max_tokens, 768);
        assert_eq!(stage.effort.as_deref(), Some("low"));
    }

    #[test]
    fn adaptive_policy_merges_project_fields_without_dropping_user_defaults() {
        let mut user = Config::default();
        user.agent.max_iterations = Some(41);
        user.agent.adaptive.max_model_calls = Some(11);
        user.agent.adaptive.intent.model = Some("fast-router".into());
        user.agent.adaptive.intent.max_tokens = Some(512);
        user.agent.adaptive.explore.effort = Some("medium".into());

        let mut project = Config::default();
        project.agent.max_iterations = Some(37);
        project.agent.adaptive.max_model_calls = Some(8);
        project.agent.adaptive.intent.max_tokens = Some(768);
        project.agent.adaptive.explore.max_calls = Some(6);

        let merged = merge(user, project);
        assert_eq!(merged.agent.max_iterations, Some(37));
        assert_eq!(merged.agent.adaptive.max_model_calls, Some(8));
        assert_eq!(
            merged.agent.adaptive.intent.model.as_deref(),
            Some("fast-router")
        );
        assert_eq!(merged.agent.adaptive.intent.max_tokens, Some(768));
        assert_eq!(
            merged.agent.adaptive.explore.effort.as_deref(),
            Some("medium")
        );
        assert_eq!(merged.agent.adaptive.explore.max_calls, Some(6));
    }

    #[test]
    fn cross_plugin_credential_grant_is_deny_by_default_and_matches_wildcard() {
        // No config → no grant.
        assert!(!Config::default().cross_plugin_credential_granted("sql", "kubernetes"));

        let dir = temp_dir();
        write_project(
            &dir,
            r#"
[endpoint]
cross_plugin_credentials = ["sql:kubernetes", "report:*"]
"#,
        );
        let cfg = load(&dir).unwrap();
        // Exact pair matches; an unlisted pair does not.
        assert!(cfg.cross_plugin_credential_granted("sql", "kubernetes"));
        assert!(!cfg.cross_plugin_credential_granted("sql", "vault"));
        // A `<consumer>:*` wildcard matches any provider for that consumer.
        assert!(cfg.cross_plugin_credential_granted("report", "kubernetes"));
        assert!(cfg.cross_plugin_credential_granted("report", "anything"));
        // The wildcard is scoped to its consumer, not global.
        assert!(!cfg.cross_plugin_credential_granted("other", "kubernetes"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn static_endpoints_parse_from_config() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
[[endpoint.static]]
id = "pg-prod"
url = "postgres://db.example:5432/app"
product = "postgres"
protocol = "postgres"
credential_ref = "env/PGPASSWORD"
labels = { region = "eu" }

[[endpoint.static]]
id = "metrics"
url = "http://prom.internal:9090"
"#,
        );
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.endpoint.static_endpoints.len(), 2);
        let pg = &cfg.endpoint.static_endpoints[0];
        assert_eq!(pg.id, "pg-prod");
        assert_eq!(pg.url, "postgres://db.example:5432/app");
        assert_eq!(pg.product, "postgres");
        assert_eq!(pg.protocol.as_deref(), Some("postgres"));
        assert_eq!(pg.credential_ref.as_deref(), Some("env/PGPASSWORD"));
        assert_eq!(pg.labels.get("region").map(String::as_str), Some("eu"));
        // A minimal declaration keeps the optional fields empty (unauthenticated).
        assert_eq!(cfg.endpoint.static_endpoints[1].id, "metrics");
        assert!(cfg.endpoint.static_endpoints[1].credential_ref.is_none());
        // A declared static endpoint means the endpoint config is no longer default.
        assert!(!cfg.endpoint.is_default());
        assert!(Config::default().endpoint.is_default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge_static_endpoints_project_overrides_user_by_id() {
        let user = vec![
            StaticEndpoint {
                id: "pg".into(),
                url: "postgres://user-host:5432/app".into(),
                ..Default::default()
            },
            StaticEndpoint {
                id: "cache".into(),
                url: "redis://user-host:6379".into(),
                ..Default::default()
            },
        ];
        let project = vec![StaticEndpoint {
            id: "pg".into(),
            url: "postgres://project-host:5432/app".into(),
            ..Default::default()
        }];
        let merged = merge_static_endpoints(user, project);
        // `pg` retargeted to the project url in place; `cache` retained; order preserved.
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, "pg");
        assert_eq!(merged[0].url, "postgres://project-host:5432/app");
        assert_eq!(merged[1].id, "cache");
    }

    #[test]
    fn missing_files_yield_default() {
        let dir = std::env::temp_dir().join(format!("flux-config-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = load(&dir).unwrap();
        assert!(cfg.model.is_none());
        assert!(cfg.permissions.allow.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_project_config() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
model = "claude/opus"
allow_private_net = true

[permissions]
allow = ["read", "Bash(git:*)"]
deny = ["Bash(rm:*)"]
"#,
        );
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.model.as_deref(), Some("claude/opus"));
        assert!(cfg.allow_private_net);
        assert_eq!(cfg.web_private_hosts(), vec!["*"]);
        assert!(cfg.plugin_private_hosts("prometheus").is_empty());
        assert_eq!(cfg.permissions.allow, vec!["read", "Bash(git:*)"]);
        assert_eq!(cfg.permissions.deny, vec!["Bash(rm:*)"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_config_errors() {
        let dir = temp_dir();
        write_project(&dir, "this is = = not toml");
        assert!(load(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_policy_grants() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
[[policy.grants]]
subjects = [{ kind = "user", id = "*" }]
resources = [{ kind = "workspace", id = "*" }]
actions = ["workspace.read"]
"#,
        );
        let cfg = load(&dir).unwrap();
        let policy = cfg.policy.expect("policy present");
        assert_eq!(policy.grants.len(), 1);
        assert_eq!(policy.grants[0].actions[0].0, "workspace.read");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn policy_grants_concatenate_across_user_and_project() {
        use flux_policy::{
            Action, AuthorizationPolicy, Grant, ResourceKind, ResourceRef, SubjectKind, SubjectRef,
            TrustLevel,
        };
        let mk = |action: &str| AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "*".into(),
                }],
                resources: vec![ResourceRef::any(ResourceKind::Workspace)],
                actions: vec![Action::from(action)],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: false,
            }],
        };
        let user = Config {
            policy: Some(mk("workspace.read")),
            ..Default::default()
        };
        let project = Config {
            policy: Some(mk("workspace.write")),
            ..Default::default()
        };
        let merged = merge(user, project);
        let grants = merged.policy.expect("policy present").grants;
        assert_eq!(
            grants.len(),
            2,
            "user + project policy grants must concatenate, not replace"
        );
    }

    #[test]
    fn scoped_private_net_grants_parse_and_merge() {
        let project = temp_dir();
        let home = temp_dir();
        let _home = crate::HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &home);
        std::fs::write(
            home.join(".flux").join("config.toml"),
            r#"
[private_net]
web = ["localhost"]

[private_net.plugins]
prometheus = ["prometheus.local"]
loki = ["loki.local"]
"#,
        )
        .unwrap();
        write_project(
            &project,
            r#"
[private_net]
web = ["127.0.0.1"]

[private_net.plugins]
prometheus = ["127.0.0.1"]
gitlab = true
"#,
        );

        let cfg = load(&project).unwrap();
        assert_eq!(cfg.web_private_hosts(), vec!["localhost", "127.0.0.1"]);
        assert_eq!(
            cfg.plugin_private_hosts("prometheus"),
            vec!["prometheus.local", "127.0.0.1"]
        );
        assert_eq!(cfg.plugin_private_hosts("loki"), vec!["loki.local"]);
        assert_eq!(cfg.plugin_private_hosts("gitlab"), vec!["*"]);

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn per_endpoint_grant_merges_with_plugin_level() {
        let project = temp_dir();
        let home = temp_dir();
        let _home = crate::HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &home);
        write_project(
            &project,
            r#"
[private_net.plugins]
gitlab = ["gitlab.internal"]

[private_net.endpoints]
"gitlab:api.endpoint" = ["api.internal"]
"#,
        );

        let cfg = load(&project).unwrap();
        // The declared endpoint merges its own grant on top of the plugin-level grant.
        assert_eq!(
            cfg.endpoint_private_hosts("gitlab", "api.endpoint"),
            vec!["gitlab.internal", "api.internal"]
        );
        // An undeclared endpoint of the same plugin inherits only the plugin-level grant.
        assert_eq!(
            cfg.endpoint_private_hosts("gitlab", "other.endpoint"),
            vec!["gitlab.internal"]
        );
        // An endpoint of a plugin with no grant at all is empty.
        assert!(cfg
            .endpoint_private_hosts("prometheus", "metrics.endpoint")
            .is_empty());

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn a2a_session_ttl_defaults_and_merges_project_over_user() {
        // Absent everywhere → the built-in 1h default.
        assert_eq!(Config::default().a2a_session_ttl_secs(), 3600);
        assert_eq!(DEFAULT_A2A_SESSION_TTL_SECS, 3600);

        let project = temp_dir();
        let home = temp_dir();
        let _home = crate::HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &home);
        std::fs::write(
            home.join(".flux").join("config.toml"),
            "[server]\na2a_session_ttl_secs = 120\n",
        )
        .unwrap();

        // User-only: the user value beats the default.
        let cfg = load(&project).unwrap();
        assert_eq!(cfg.a2a_session_ttl_secs(), 120);

        // Project sets the disable value 0: it overrides the user's 120 (an explicit 0 is a
        // real setting, not "absent" — `Some(0)` must survive the merge).
        write_project(&project, "[server]\na2a_session_ttl_secs = 0\n");
        let cfg = load(&project).unwrap();
        assert_eq!(cfg.server.a2a_session_ttl_secs, Some(0));
        assert_eq!(cfg.a2a_session_ttl_secs(), 0, "0 = never prune");

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    /// L-02: `[skills] dirs` is a layered list of custom skill directories. Dir order is
    /// name-clash precedence, so unlike the permission lists the **project's** dirs come first
    /// (project > user), and `skill_dir_paths` expands a leading `~/`.
    #[test]
    fn skill_dirs_merge_project_before_user_and_expand_tilde() {
        let project = temp_dir();
        let home = temp_dir();
        let _home = crate::HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &home);
        std::fs::write(
            home.join(".flux").join("config.toml"),
            "[skills]\ndirs = [\"~/global-skills\", \"shared\"]\n",
        )
        .unwrap();
        write_project(&project, "[skills]\ndirs = [\"team-skills\", \"shared\"]\n");

        let cfg = load(&project).unwrap();
        assert_eq!(
            cfg.skills.dirs,
            vec!["team-skills", "shared", "~/global-skills"],
            "project dirs first (highest precedence), de-duplicated"
        );
        let paths = cfg.skill_dir_paths();
        assert_eq!(paths[0], PathBuf::from("team-skills"));
        assert_eq!(
            paths[2],
            home.join("global-skills"),
            "~/ expands against HOME"
        );

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    /// C-21: `[workspace]` add_dirs merge (project first), `allow_all` OR-merges, `~/` expands.
    #[test]
    fn workspace_add_dirs_merge_and_allow_all() {
        let project = temp_dir();
        let home = temp_dir();
        let _home = crate::HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &home);
        std::fs::write(
            home.join(".flux").join("config.toml"),
            "[workspace]\nadd_dirs = [\"~/refs\"]\n",
        )
        .unwrap();
        write_project(
            &project,
            "[workspace]\nadd_dirs = [\"/data/shared\"]\nallow_all = true\n",
        );

        let cfg = load(&project).unwrap();
        assert_eq!(
            cfg.workspace.add_dirs,
            vec!["/data/shared", "~/refs"],
            "project dirs first, de-duplicated"
        );
        assert!(
            cfg.workspace_allow_all(),
            "allow_all is true if either sets it"
        );
        let paths = cfg.workspace_add_dirs();
        assert_eq!(paths[0], PathBuf::from("/data/shared"));
        assert_eq!(paths[1], home.join("refs"), "~/ expands against HOME");

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    /// D-130: `[sandbox]` parses, and the merge is security-directional — `enabled`/`require`
    /// OR (a project may tighten confinement, never loosen a user's `require`), `writable`
    /// concatenates (project first, deduplicated) like `[workspace] add_dirs`, and `~/` expands.
    #[test]
    fn sandbox_config_parses_and_merges_security_directional() {
        let project = temp_dir();
        let home = temp_dir();
        let _home = crate::HOME_LOCK.lock().unwrap();
        std::env::set_var("HOME", &home);
        std::fs::write(
            home.join(".flux").join("config.toml"),
            "[sandbox]\nrequire = true\nwritable = [\"~/scratch\"]\n",
        )
        .unwrap();
        write_project(
            &project,
            "[sandbox]\nenabled = true\nnetwork = false\nwritable = [\"/data/out\"]\n",
        );

        let cfg = load(&project).unwrap();
        // The user's `require` survives even though the project only set `enabled`.
        assert!(cfg.sandbox_enabled());
        assert!(cfg.sandbox_require(), "user require is not lost");
        assert_eq!(
            cfg.sandbox_network(),
            Some(false),
            "an explicit false narrows the network posture"
        );
        assert_eq!(
            cfg.sandbox.writable,
            vec!["/data/out", "~/scratch"],
            "project writable first, de-duplicated"
        );
        let paths = cfg.sandbox_writable();
        assert_eq!(paths[0], PathBuf::from("/data/out"));
        assert_eq!(paths[1], home.join("scratch"), "~/ expands against HOME");

        // An absent `[sandbox]` table is the default: off, unrestricted network, no extras.
        let default_cfg = Config::default();
        assert!(!default_cfg.sandbox_enabled());
        assert!(!default_cfg.sandbox_require());
        assert_eq!(default_cfg.sandbox_network(), None);
        assert!(default_cfg.sandbox_writable().is_empty());
        assert!(default_cfg.sandbox.is_default());

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    /// The strictest-wins direction for `network`: either side explicitly narrowing to `false`
    /// wins over the other side explicitly widening to `true` — narrowing a security posture is
    /// never overridden by a looser co-located config.
    #[test]
    fn sandbox_network_merge_is_strictest_wins() {
        let closed = SandboxConfig {
            network: Some(false),
            ..Default::default()
        };
        let open = SandboxConfig {
            network: Some(true),
            ..Default::default()
        };
        let unset = SandboxConfig::default();

        assert_eq!(
            merge_sandbox(closed.clone(), open.clone()).network,
            Some(false)
        );
        assert_eq!(
            merge_sandbox(open.clone(), closed.clone()).network,
            Some(false)
        );
        assert_eq!(
            merge_sandbox(open.clone(), unset.clone()).network,
            Some(true)
        );
        assert_eq!(merge_sandbox(unset.clone(), open).network, Some(true));
        assert_eq!(merge_sandbox(unset.clone(), unset).network, None);
    }

    /// D-130 (finding 13): a typo'd key inside `[sandbox]` is a hard parse error, not a silently
    /// dropped setting — otherwise `requre = true` would fail *open* (no `require`), the worst
    /// possible direction for a security posture. `#[serde(deny_unknown_fields)]` enforces this.
    #[test]
    fn unknown_sandbox_key_is_rejected() {
        let err = toml::from_str::<Config>("[sandbox]\nrequre = true\n").unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
        // A malformed `[sandbox]` table therefore fails the whole load (caller surfaces it).
        let dir = temp_dir();
        write_project(&dir, "[sandbox]\nrequre = true\n");
        assert!(load(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persist_allow_rules_unions_and_dedups() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
[permissions]
allow = ["read"]
"#,
        );
        persist_allow_rules(&dir, &["read".into(), "Bash(git:*)".into()]).unwrap();
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.permissions.allow, vec!["read", "Bash(git:*)"]);

        // A second persist with a new rule appends without duplicating.
        persist_allow_rules(&dir, &["write".into()]).unwrap();
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.permissions.allow, vec!["read", "Bash(git:*)", "write"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn injected_sources_parse_merge_and_render_without_filesystem_authority() {
        let user = "model = \"user\"\n[permissions]\nallow = [\"read\"]\n[skills]\ndirs = [\"/trusted\", \"shared\"]\n";
        let project = "model = \"project\"\n[permissions]\nallow = [\"write\"]\n[skills]\ndirs = [\"/project-escape\", \"shared\"]\n";
        let merged = from_sources(
            Some(("trusted-user", user)),
            Some(("guarded-project", project)),
        )
        .unwrap();
        assert_eq!(merged.model.as_deref(), Some("project"));
        assert_eq!(merged.permissions.allow, ["read", "write"]);
        assert_eq!(
            merged.configured_skill_dirs,
            [
                ConfiguredSkillDir {
                    path: "/project-escape".into(),
                    origin: SkillDirOrigin::Project,
                },
                ConfiguredSkillDir {
                    path: "shared".into(),
                    origin: SkillDirOrigin::Project,
                },
                ConfiguredSkillDir {
                    path: "/trusted".into(),
                    origin: SkillDirOrigin::User,
                },
            ]
        );

        let rendered = render_allow_rules(
            Some(("guarded-project", project)),
            &["write".into(), "bash".into()],
        )
        .unwrap();
        let reparsed = parse_source("rendered", &rendered).unwrap();
        assert_eq!(reparsed.model.as_deref(), Some("project"));
        assert_eq!(reparsed.permissions.allow, ["write", "bash"]);

        let error = from_sources(None, Some(("guarded-project", "unknown = true")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("guarded-project"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn project_config_file_symlink_cannot_read_or_overwrite_outside_workspace() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir();
        let outside = temp_dir();
        let target = outside.join("config.toml");
        std::fs::write(&target, "model = \"outside-secret\"\n").unwrap();
        symlink(&target, dir.join(".flux").join("config.toml")).unwrap();

        assert!(load(&dir).is_err(), "project config read followed symlink");
        assert!(
            persist_allow_rules(&dir, &["read".into()]).is_err(),
            "project config write followed symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "model = \"outside-secret\"\n"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[cfg(unix)]
    #[test]
    fn project_config_parent_symlink_cannot_write_outside_workspace() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir();
        let outside = temp_dir();
        std::fs::remove_dir_all(dir.join(".flux")).unwrap();
        symlink(&outside, dir.join(".flux")).unwrap();

        let error = persist_allow_rules(&dir, &["read".into()]).unwrap_err();
        assert!(error.to_string().contains("outside workspace"), "{error}");
        assert!(
            !outside.join("config.toml").exists(),
            "guarded persistence created an external config"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
