//! `flux-config` — layered project/user configuration for the `flux` binary.
//!
//! Two files are read and merged by [`from_sources`]: `~/.flux/config.toml` (user defaults) then
//! `<cwd>/.flux/config.toml` (project, takes precedence). A missing file is not an error — it
//! contributes nothing; a malformed file is an error. CLI flags layer on top of the result (the
//! caller resolves that). The config carries the coder-style permission rules, an optional default
//! model, an optional [`AuthorizationPolicy`] (extends [`flux_policy::default_local_grants`]), and
//! scoped private-network egress grants. Filesystem discovery and atomic persistence live in the
//! guarded outer control plane; this crate parses, merges, and serializes injected documents.
//!
//! [`from_sources_with_managed`] adds an optional third **managed** layer ahead of both (C-165): a
//! system-owned floor (`/etc/flux/config.toml` on Linux/macOS, or `FLUX_MANAGED_CONFIG` for
//! containerized deploys — the guarded read lives in `flux-runtime`'s metadata loader, same as the
//! other two). Its `[managed] pins` distinguish plain defaults (the user may still change them)
//! from pins (a downstream layer may only make the value *more* restrictive; a relaxation is a
//! named, refused [`Error::Config`], not a silent merge). This is an **operator** control backed
//! by filesystem permissions on the managed file — not a defense against a user who owns the
//! machine and can edit the binary; see `website/docs/security/overview.md`.

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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

/// Native web-operation settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Environment-variable names `http.request` may resolve from a `{"$secret": "NAME"}` marker.
    /// Entries may carry C-459 scope parameters (`NAME;to=host;by=principal;in=header|query`).
    /// `None` preserves the `FLUX_WEB_SECRET_ALLOW` fallback; `Some([])` is explicit deny-all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_secrets: Option<Vec<String>>,
}

impl WebConfig {
    fn is_default(&self) -> bool {
        self.allowed_secrets.is_none()
    }
}

/// Endpoint-discovery / cross-plugin credential brokerage grants (D-27). Deny-by-default: a consumer
/// plugin can only have a credential owned by a *different* provider plugin materialized on its behalf
/// if an operator listed the `(consumer, provider)` pair here — exactly like the `process`/`conn`/
/// `secrets` allow-lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// The closed backend vocabulary for a `[[host]]` declaration (Decision 0018 rule 3). Typed — not a
/// free string — so an unknown backend kind is a **hard config error** at parse time, unlike the
/// warn-and-skip semantic validation the string fields get in the surface crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostBackendKind {
    Local,
    Sandboxed,
    Container,
    Kubernetes,
    /// A VM/microVM guest serving the remote protocol (C-677). Declarable with the endpoint its
    /// guest serves, or without one yet — flux never provisions the guest, so a binding written
    /// before the endpoint exists is honestly unwired rather than a config error.
    Microvm,
    /// An ssh-bootstrapped substrate composing the remote protocol (C-683).
    Ssh,
    Remote,
}

impl HostBackendKind {
    /// The lowercase wire/display form (matches the serde encoding).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sandboxed => "sandboxed",
            Self::Container => "container",
            Self::Kubernetes => "kubernetes",
            Self::Microvm => "microvm",
            Self::Ssh => "ssh",
            Self::Remote => "remote",
        }
    }
}

/// The `ssh` sub-table of a `[[host]]` declaration (C-683): what the binding declares about the far
/// machine. `deny_unknown_fields` for the same reason the parent table has it — a silently dropped
/// typo in a substrate binding is a safety problem. Mirrors `flux_secret::host::HostSsh`; the two
/// crates may not depend on each other, so the surface crate owns the conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSshEntry {
    /// The far-side flux binary; absent means `flux` on the far side's `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// The far-side loopback port the serve binds and the tunnel forwards to; absent means 8790.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serve_port: Option<u16>,
    /// The far-side workspace root a started serve is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The far-side TLS certificate a started serve is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    /// The far-side TLS key a started serve is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// A local PEM whose roots this binding's client trusts (the `--remote-ca` pinning form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
    /// A local `known_hosts` file scoping strict host-key verification to this binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_hosts: Option<String>,
    /// The name the far side's certificate carries; absent means `127.0.0.1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    /// Credential *location* of the serving endpoint's bearer token; absent means
    /// `env/FLUX_REMOTE_SYSTEM_TOKEN`. Never a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_ref: Option<String>,
}

/// One `[[host]]` declaration: a named, first-class binding to an execution substrate
/// (Decision 0018). Mirrors the weak `HostRef` — id + backend kind + bare address + a credential
/// *reference* + labels — never a secret value. `deny_unknown_fields` because a silently dropped
/// typo in a substrate binding is a safety problem, not a formatting one. The remaining semantic
/// validation (credential-free url, parseable credential ref) happens in the surface crate against
/// the same rules as `flux host add`, keeping this crate a `flux-secret`-free leaf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEntry {
    /// The binding name (a bare name, e.g. `build-farm`).
    pub id: String,
    /// Which substrate backend this binding selects.
    pub backend: HostBackendKind,
    /// Bare `scheme://host[:port]` for backends with an address — never with embedded credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Credential *location* in `scheme/...` form (`env/FARM_TOKEN`,
    /// `kubernetes/<ns>/<name>/<key>`, `plugin/<p>/<i>/<slot>`); optional. Never a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    /// Filesystem *location* of the private CA certificate (PEM) this binding's endpoint chains to
    /// — the `[[host]]` equivalent of `--remote-ca` (C-684). Optional; omit for ordinary public
    /// trust. A CA certificate is public material, so this is a plain path rather than a secret
    /// reference; what it keeps from `credential_ref` is that the config declares a *location* and
    /// resolution validates it, failing closed rather than falling back to the default trust store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_cert: Option<String>,
    /// Surface classes granted to *select* this binding (`operator`, `unattended`). The default
    /// is deny (Decision 0018 rule 4): an ungranted binding lists and probes but selects for
    /// nobody. Held as plain strings here; the surface crate validates the vocabulary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grant: Vec<String>,
    /// Non-secret labels (region, cluster, tags) for display/filtering.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub labels: std::collections::BTreeMap<String, String>,
    /// The far-side bootstrap contract for an `ssh` binding (C-683); meaningless for every other
    /// backend, which the surface crate refuses rather than ignores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<HostSshEntry>,
}

/// The `[exchange]` table — the declared home for the Exchange catalogue binding (C-650). Names a
/// `[[host]]` binding whose `url` is the Exchange origin and whose `credential_ref` locates the
/// service-account token. The transitional `FLUX_EXCHANGE_URL`/token environment pair keeps
/// working and wins while present.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeConfig {
    /// The `[[host]]` binding name serving the Exchange catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl ExchangeConfig {
    fn is_default(&self) -> bool {
        self.host.is_none()
    }
}

/// The `[skills]` table — skill-discovery settings (L-02).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsConfig {
    /// Custom skill directories, layered **above** the built-in well-known set (`.flux/skills`,
    /// `.claude/skills`, `~/.flux/skills`, …). Unlike the permission lists, order here is semantic
    /// — earlier dirs win skill-name clashes — so the merge puts **project** dirs before user dirs
    /// (CLI flags layer on top of all of these; the caller resolves that). Relative paths resolve
    /// against the workspace root; a leading `~/` expands to the home directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<String>,
    /// Opt into Claude-style progressive skill disclosure (D-188): every discovered,
    /// non-`disable-model-invocation` skill's name+description is surfaced to the model, and it
    /// can pull a skill's full body into context on demand via `skill.load`. Off by default —
    /// manual `--skill` activation stays the measured-cheaper default path
    /// (`docs/designs/manual-skill-activation.md`); this is additive, not a replacement. Mirrors
    /// the CLI's `--skills-model-invoked` flag.
    #[serde(default)]
    pub model_invoked: bool,
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
        self.dirs.is_empty() && !self.model_invoked
    }
}

/// The `[workspace]` table — filesystem access widening (C-21).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// The `[tools] disable` table (C-162): a plain, subtractive blocklist for turning ops off. Tool
/// groups (`flux_evidence::ToolGroup`) are purely additive — they surface ops when evidence fires,
/// never hide them — so this is the one deliberately subtractive knob, for an operator who wants
/// less prompt surface / attack surface in a given repo (e.g. `disable = ["browser.*", "web.*"]`)
/// rather than an authorization decision. **Not a security boundary**: the authorization policy
/// remains the actual gate, and wins if the two ever disagree (see `Executor`'s dispatch-time
/// refusal in `flux-runtime`, which is defense-in-depth, not a second permission system).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Op names to remove from the surfaced tool set entirely: an exact name (`"bash"`) or a
    /// `family.*` glob matching every op under that dotted prefix (`"browser.*"` matches
    /// `browser.navigate`, `browser.click`, …, but not a bare `browser`). An entry that matches no
    /// known op is reported by the caller as a startup warning rather than silently doing nothing —
    /// see [`tool_disable_matches`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disable: Vec<String>,
}

impl ToolsConfig {
    fn is_default(&self) -> bool {
        self.disable.is_empty()
    }
}

/// Whether `op_name` matches a single `[tools] disable` entry (C-162): an exact-name match, or —
/// when `pattern` ends in `.*` — membership in that dotted family (`op_name` starts with the
/// family prefix followed by `.`). A bare family name with no `.*` suffix is treated as an exact
/// name, so `"browser"` (no glob) does NOT match `browser.navigate`; only `"browser.*"` does. Pure
/// and side-effect-free so both the config layer and the runtime's registry-resolution step
/// (`flux_runtime::ToolRegistry::resolve_disabled`) can share it without disagreeing.
pub fn tool_disable_matches(pattern: &str, op_name: &str) -> bool {
    match pattern.strip_suffix(".*") {
        Some(family) if !family.is_empty() => op_name
            .strip_prefix(family)
            .is_some_and(|rest| rest.starts_with('.')),
        _ => pattern == op_name,
    }
}

/// The `[consult]` table (A-96): the second-opinion op's default target and per-turn call cap.
/// `model`'s mere presence is what surfaces the `consult` op into the catalog at all — absent
/// means the op stays off the model-facing catalog (evidence-gated, the A-95 cache-stability
/// lesson: within a session the surfacing decision is made once at assembly time and never
/// churns), since an operator who hasn't named a target hasn't opted into the extra spend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsultConfig {
    /// Default `provider/model` spec consulted when a `consult` call omits its own `model`
    /// argument (e.g. `openrouter/anthropic/claude-opus-4.6`). Resolved through the same
    /// provider/model routing as `-m`/`--model`, subscription providers included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Per-turn call cap — a cheap second opinion, not a council of models. Absent means the
    /// built-in default (see `flux_cognition::DEFAULT_CONSULT_MAX_CALLS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_calls: Option<usize>,
}

impl ConsultConfig {
    fn is_default(&self) -> bool {
        self.model.is_none() && self.max_calls.is_none()
    }
}

/// The `[wakeup]` table (A-98): the agent-set wake-up op's surfacing switch, per-session cap, and
/// maximum horizon. `enabled = false` (the default) keeps `schedule_wakeup` off the catalog
/// entirely — an operator opts in explicitly, mirroring `enable_shell`'s off-by-default posture.
/// Registering a wake-up ALSO needs authority (an `AuthorityRequirement::host_write` resolved
/// against the existing approval-gated `host.write` default policy grant) — this table only
/// bounds an already-approved registration, it does not itself grant one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeupConfig {
    /// Surfaces the `schedule_wakeup` op. Off by default.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum horizon (seconds) a single wake-up may be scheduled for. Absent means the built-in
    /// default (see `flux_flow::wakeup::DEFAULT_MAX_HORIZON_SECS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_horizon_secs: Option<u64>,
    /// Maximum number of wake-ups that may be pending at once per session. Absent means the
    /// built-in default (see `flux_flow::wakeup::DEFAULT_MAX_PENDING_PER_SESSION`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pending_per_session: Option<usize>,
}

impl WakeupConfig {
    fn is_default(&self) -> bool {
        !self.enabled && self.max_horizon_secs.is_none() && self.max_pending_per_session.is_none()
    }
}

/// One security-relevant key a **managed** config layer may pin (C-165). Deliberately a closed
/// enum rather than a free-form dotted string: every entry has a defined "would a downstream
/// value relax this" comparator in [`pin_violation`], so growing the pinnable set means adding a
/// variant + arm, not accepting an arbitrary path nobody checks. v1 covers the categories named in
/// the story — the authorization floor, egress/private-network grants, the `[tools] disable`
/// blocklist, and the sandbox/workspace-confinement knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PinnableKey {
    /// `[tools] disable` (C-162). Union-merged already, so the pinned entries can never be
    /// removed by a downstream layer — pinning documents the floor rather than closing a live
    /// relax path.
    ToolsDisable,
    /// `[private_net] web` — the family-wide egress scope for the native web ops. `merge_grant`'s
    /// direction genuinely widens on merge (`Enabled(true)` wins over a narrower grant), so this
    /// is the one pin with a real downstream relax path.
    PrivateNetWeb,
    /// `[[policy.grants]]` — the authorization floor. Grants concatenate (any additional grant
    /// only ever widens what's allowed), so pinning closes the effective policy to exactly the
    /// managed set: no downstream layer may add a grant of its own.
    Policy,
    /// `[sandbox] enabled`. OR-merged already (monotonically safe): no downstream layer can turn
    /// it back off once any layer sets it. Pinning documents operator intent.
    SandboxEnabled,
    /// `[sandbox] require`. Same OR-merged safety as `enabled`.
    SandboxRequire,
    /// `[sandbox] network`. Already strictest-wins on merge (`Some(false)` beats everything).
    /// Pinning documents operator intent.
    SandboxNetwork,
    /// `[workspace] allow_all` — the unconfined filesystem hatch. OR-merged already.
    WorkspaceAllowAll,
}

impl PinnableKey {
    /// Every pinnable key paired with its `[managed] pins` spelling, in declaration order.
    pub const ALL: &'static [(&'static str, PinnableKey)] = &[
        ("tools.disable", PinnableKey::ToolsDisable),
        ("private_net.web", PinnableKey::PrivateNetWeb),
        ("policy", PinnableKey::Policy),
        ("sandbox.enabled", PinnableKey::SandboxEnabled),
        ("sandbox.require", PinnableKey::SandboxRequire),
        ("sandbox.network", PinnableKey::SandboxNetwork),
        ("workspace.allow_all", PinnableKey::WorkspaceAllowAll),
    ];

    /// Parse a `[managed] pins` entry; `None` for an unrecognized spelling (a load-time error at
    /// the caller, not a silently-ignored typo).
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
    }

    /// The canonical `[managed] pins` spelling for this key.
    pub fn as_str(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, k)| *k == self)
            .map(|(n, _)| *n)
            .expect("every PinnableKey variant is listed in ALL")
    }
}

/// The `[managed]` table (C-165): present only in a **managed** config layer, naming which of its
/// own security-relevant keys are **pins** rather than mere defaults. Meaningless (and harmlessly
/// round-tripped) in a user/project file — only [`from_sources_with_managed`] interprets it, and
/// only against the config parsed from the managed source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedMeta {
    /// Dotted key paths naming pinned keys (see [`PinnableKey::ALL`]). An entry that doesn't name
    /// a recognized pinnable key is a load-time error, not a silent no-op.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pins: Vec<String>,
}

impl ManagedMeta {
    fn is_default(&self) -> bool {
        self.pins.is_empty()
    }
}

/// Which config layer supplied an effective setting's value (C-165), for display/inspection only
/// — the actual merge precedence lives in [`merge`] and [`from_sources_with_managed`]. Ordered
/// lowest-precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLayer {
    /// The system-owned managed floor (a documented system path, or `FLUX_MANAGED_CONFIG`).
    Managed,
    /// The trusted user-global `~/.flux/config.toml`.
    User,
    /// The repository's `.flux/config.toml`.
    Project,
    /// No layer set this key; the built-in default is in effect.
    BuiltIn,
}

/// One pinnable key's effective provenance: which layer supplied the winning value, a short
/// display of that value, and whether the managed layer pinned it. The API-level answer to "why
/// can't I enable this" (C-165) — independent of any CLI surface, so it composes with whatever
/// inspects it (the natural home is the `flux doctor` diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSetting {
    pub key: PinnableKey,
    pub layer: ConfigLayer,
    pub pinned: bool,
    pub value: String,
}

/// Report the effective provenance of every pinnable security-relevant key across the three raw
/// (pre-merge) layers. `managed`/`user`/`project` are each the parsed config for that layer alone
/// (as returned by [`parse_source`]), not an already-merged [`Config`] — provenance is determined
/// by which layer's own value is non-default, checked in precedence order (project, then user,
/// then managed).
pub fn effective_settings(
    managed: &Config,
    user: &Config,
    project: &Config,
) -> Vec<EffectiveSetting> {
    PinnableKey::ALL
        .iter()
        .map(|(_, key)| {
            let key = *key;
            let pinned = managed
                .managed
                .pins
                .iter()
                .any(|p| PinnableKey::parse(p) == Some(key));
            let (layer, value) = if let Some(v) = key_value(project, key) {
                (ConfigLayer::Project, v)
            } else if let Some(v) = key_value(user, key) {
                (ConfigLayer::User, v)
            } else if let Some(v) = key_value(managed, key) {
                (ConfigLayer::Managed, v)
            } else {
                (
                    ConfigLayer::BuiltIn,
                    key_value(&Config::default(), key).unwrap_or_default(),
                )
            };
            EffectiveSetting {
                key,
                layer,
                pinned,
                value,
            }
        })
        .collect()
}

/// This layer's own value for `key`, and whether that value is non-default (i.e. this layer
/// actually set it, as opposed to leaving the built-in default in place).
fn key_value(cfg: &Config, key: PinnableKey) -> Option<String> {
    let (is_set, display) = match key {
        PinnableKey::ToolsDisable => (
            !cfg.tools.disable.is_empty(),
            format!("{:?}", cfg.tools.disable),
        ),
        PinnableKey::PrivateNetWeb => (
            !cfg.private_net.web.is_default(),
            format!("{:?}", cfg.private_net.web),
        ),
        PinnableKey::Policy => (
            cfg.policy.as_ref().is_some_and(|p| !p.grants.is_empty()),
            format!(
                "{} grant(s)",
                cfg.policy.as_ref().map_or(0, |p| p.grants.len())
            ),
        ),
        PinnableKey::SandboxEnabled => (cfg.sandbox.enabled, format!("{}", cfg.sandbox.enabled)),
        PinnableKey::SandboxRequire => (cfg.sandbox.require, format!("{}", cfg.sandbox.require)),
        PinnableKey::SandboxNetwork => (
            cfg.sandbox.network.is_some(),
            format!("{:?}", cfg.sandbox.network),
        ),
        PinnableKey::WorkspaceAllowAll => (
            cfg.workspace.allow_all,
            format!("{}", cfg.workspace.allow_all),
        ),
    };
    is_set.then_some(display)
}

/// Whether `candidate` (a downstream user/project value) allows something `floor` (the managed
/// pin) does not — i.e. whether merging it in would relax the pin. `Enabled(true)` allows every
/// host; `Hosts(_)` allows exactly its listed patterns; `Enabled(false)` allows none. Equal or
/// narrower is never a relaxation, matching "a project may still make itself more restrictive."
fn grant_more_permissive(candidate: &PrivateNetGrant, floor: &PrivateNetGrant) -> bool {
    match floor {
        PrivateNetGrant::Enabled(true) => false, // already unrestricted; nothing widens it further
        PrivateNetGrant::Enabled(false) => !matches!(candidate, PrivateNetGrant::Enabled(false)),
        PrivateNetGrant::Hosts(allowed) => match candidate {
            PrivateNetGrant::Enabled(true) => true,
            PrivateNetGrant::Enabled(false) => false,
            PrivateNetGrant::Hosts(requested) => requested.iter().any(|h| !allowed.contains(h)),
        },
    }
}

/// Whether `downstream` (the pure `merge(user, project)` result, with no managed floor folded in)
/// relaxes the managed layer's pin at `key`. Returns `None` when it does not (either downstream
/// left the key alone, made it stricter, or the key's merge rule is already monotonically safe by
/// construction — see the per-variant doc comments on [`PinnableKey`]); `Some(diagnostic)` names
/// the key and the reason when it does.
fn pin_violation(key: PinnableKey, managed: &Config, downstream: &Config) -> Option<String> {
    match key {
        // Union-merged: the pinned entries can never be removed by a downstream layer, so there is
        // no relax path to detect.
        PinnableKey::ToolsDisable => None,
        PinnableKey::PrivateNetWeb => {
            let floor = &managed.private_net.web;
            let candidate = &downstream.private_net.web;
            grant_more_permissive(candidate, floor).then(|| {
                format!(
                    "managed config pins `private_net.web` at {floor:?}: a downstream config \
                     would widen private-network egress to {candidate:?}, which relaxes the \
                     operator floor and is refused"
                )
            })
        }
        PinnableKey::Policy => {
            let extra = downstream
                .policy
                .as_ref()
                .is_some_and(|p| !p.grants.is_empty());
            extra.then(|| {
                "managed config pins `policy`: a downstream config declares additional \
                 [[policy.grants]], which is refused once the authorization floor is pinned \
                 (any added grant only ever widens what's allowed)"
                    .to_string()
            })
        }
        // OR-merged / strictest-wins already: monotonically safe by construction, so no downstream
        // value can relax these — nothing to detect.
        PinnableKey::SandboxEnabled
        | PinnableKey::SandboxRequire
        | PinnableKey::SandboxNetwork
        | PinnableKey::WorkspaceAllowAll => None,
    }
}

/// The merged flux configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Default `provider/model` spec (a CLI `--model` flag overrides this).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// TUI color theme name (`dark` / `light` / `mono`); the in-TUI `/theme` command persists it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Deprecated compatibility flag. If true, the native web family (the `web` scope) gets a
    /// private-net `*` grant; plugins still require `[private_net.plugins]` grants.
    #[serde(default)]
    pub allow_private_net: bool,
    /// Scoped private-network egress grants.
    #[serde(default, skip_serializing_if = "PrivateNetConfig::is_default")]
    pub private_net: PrivateNetConfig,
    /// Native web-operation settings, including the `$secret` allowlist.
    #[serde(default, skip_serializing_if = "WebConfig::is_default")]
    pub web: WebConfig,
    /// Endpoint-discovery / cross-plugin credential brokerage grants (D-27).
    #[serde(default, skip_serializing_if = "EndpointConfig::is_default")]
    pub endpoint: EndpointConfig,
    /// Named execution-substrate bindings (`[[host]]`, Decision 0018).
    #[serde(default, rename = "host", skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostEntry>,
    /// The named home for the Exchange catalogue binding (`[exchange] host = "<binding>"`).
    #[serde(default, skip_serializing_if = "ExchangeConfig::is_default")]
    pub exchange: ExchangeConfig,
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
    /// A plain, subtractive op blocklist (C-162): `disable = ["browser.*", "web.*"]`. Surface-only —
    /// see [`ToolsConfig`].
    #[serde(default, skip_serializing_if = "ToolsConfig::is_default")]
    pub tools: ToolsConfig,
    /// The second-opinion `consult` op's default target and per-turn call cap (A-96).
    #[serde(default, skip_serializing_if = "ConsultConfig::is_default")]
    pub consult: ConsultConfig,
    /// The agent-set wake-up op's surfacing switch, per-session cap, and maximum horizon (A-98).
    #[serde(default, skip_serializing_if = "WakeupConfig::is_default")]
    pub wakeup: WakeupConfig,
    /// Pin declarations (C-165). Only meaningful in a **managed** config layer — see
    /// [`ManagedMeta`] and [`from_sources_with_managed`]; harmlessly round-tripped elsewhere.
    #[serde(default, skip_serializing_if = "ManagedMeta::is_default")]
    pub managed: ManagedMeta,
}

/// The default A2A session TTL (seconds) when `[server] a2a_session_ttl_secs` is absent: 1 hour.
pub const DEFAULT_A2A_SESSION_TTL_SECS: u64 = 3600;

/// The `[server]` table — settings for the HTTP/A2A surface.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// TTL in seconds for sessions minted by the A2A surface (C-18). Absent means the default
    /// [`DEFAULT_A2A_SESSION_TTL_SECS`] (1h); `0` means never prune. Age is measured from a
    /// session's last activity, not its creation — see [`Config::a2a_session_ttl_secs`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a2a_session_ttl_secs: Option<u64>,
    /// Work requests admitted per authenticated principal/realm each minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    /// Concurrent live turns admitted per authenticated principal/realm across HTTP surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_inflight_per_principal: Option<usize>,
    /// Provider calls admitted per authenticated principal/realm per 24-hour process window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_calls_per_day: Option<u64>,
    /// Priced provider spend admitted per authenticated principal/realm per 24-hour process window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_spend_usd_per_day: Option<f64>,
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
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Per-turn token budget: once the turn's accumulated model usage (all tiers) crosses this,
    /// the loop ends the turn honestly instead of consulting the model again. Overridden by
    /// `FLUX_TURN_TOKEN_BUDGET` and the `--turn-budget` flag (flag > env > config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_token_budget: Option<u64>,
    /// C-290: how many tool calls may be executing simultaneously in one in-process runtime.
    /// Absent means no ceiling. Unlike `[server] max_inflight_per_principal` this is not
    /// per-principal and not server-side — it binds inside the safety envelope, so it applies to an
    /// embedded runtime too. `0` is read as `1`.
    ///
    /// **Per agent, including sub-agents (C-299):** every `task`-delegated sub-agent now inherits
    /// this ceiling too (it previously ran unbounded), but with its **own** budget rather than a
    /// share of one — so with k live children the process may run up to N×(k+1) tool calls at once.
    /// A single shared budget would be the stronger guarantee and deadlocks: the agent-loop op
    /// driving the delegation (`execute_batch`) holds a permit for the child's whole turn, and the
    /// task-local exemption that covers the nested `task` does not survive the spawn the child is
    /// reached through. See `flux_runtime::ResourceLimits::independent_copy`.
    ///
    /// **Bounding the tree needs `max_live_agents` too (C-444):** this number alone leaves k — and so
    /// the process total — unbounded. Set both and the total is `N × max_live_agents`.
    ///
    /// Applied by the `flux` binary (`flux-cli` reads this table at executor assembly) and by an
    /// embedding host through `flux_runtime::ResourceLimits::from_config` →
    /// `ClientBuilder::resource_limits`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_tool_calls: Option<usize>,
    /// C-471: how many agents may be live at once across one delegated tree, including the root.
    /// Absent means no tree-wide ceiling. `1` disables delegation; `0` is read as `1` because the
    /// root itself already occupies one census place.
    ///
    /// Unlike `max_concurrent_tool_calls`, this budget is shared by the root and every transitive
    /// child. A spawn over the ceiling is refused immediately rather than queued, avoiding a child
    /// waiting on an ancestor that is itself waiting for that child. Set both ceilings to bound
    /// simultaneous tool execution across the tree at
    /// `max_concurrent_tool_calls × max_live_agents`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_live_agents: Option<usize>,
    /// C-290: how long a tool call waits for a concurrency slot before it is refused with an
    /// actionable message. Absent means the runtime default (30s). Meaningful only alongside
    /// `max_concurrent_tool_calls`.
    ///
    /// **No sentinel means "wait forever", and the 30s default binds when this is absent — but this
    /// value is not clamped.** It is milliseconds handed to `Duration::from_millis`, so `u64::MAX`
    /// is a ~584,942,417-year wait that `tokio::time::timeout` will honor rather than cap. An
    /// operator who writes an absurd number here has chosen a hang; that is deliberate and visible,
    /// and nothing overrides it. See `flux_runtime::DEFAULT_TOOL_CALL_QUEUE_TIMEOUT` for why no
    /// maximum is imposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_queue_timeout_ms: Option<u64>,
    /// C-290: how many bytes of tool results the runtime may retain in its deterministic op cache.
    /// Absent means no byte ceiling (the entry-count bound still applies). Eviction is
    /// correctness-neutral — a miss re-runs the op — so this never truncates a visible result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retained_result_bytes: Option<usize>,
    /// C-298: how many bytes of observation `data` payload the runtime may retain in the in-memory
    /// evidence log. Absent means no ceiling — the log grows for the process lifetime, one payload
    /// per dispatch.
    ///
    /// Unlike `max_retained_result_bytes` this is **not** a cache bound, so it is not
    /// correctness-neutral: reaching it elides the *oldest* payloads. No observation is ever
    /// dropped — count, order, kind and phase are preserved, and each elided payload is replaced by
    /// a self-describing marker naming this key — but the payload itself is gone from memory.
    /// Payloads from turns that already completed remain in full in the session event store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_evidence_payload_bytes: Option<usize>,
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
    /// composer (`flux_runtime::metadata::discover_skills_from`) resolves them against the
    /// workspace root.
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

/// The user config under an *explicitly passed* home (C-332). `None` means "this fixture has no
/// user layer" — which is what every test that only exercises the project layer wants, and what
/// reading process `HOME` silently failed to give it: the operator's real `~/.flux/config.toml`
/// merged under every fixture, so the verdict depended on the machine rather than the fixture.
#[cfg(test)]
fn home_config_path(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| h.join(".flux").join("config.toml"))
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

/// Parse and merge three layers — a **managed** floor, then user, then project, in that
/// precedence (C-165) — enforcing every pin the managed layer declares before folding it in as
/// the lowest-precedence default. `from_sources` (unchanged) is the two-layer case; this is
/// additive so existing callers are unaffected.
///
/// Enforcement first computes `downstream = merge(user, project)` — what the effective config
/// would be with **no** managed floor at all — and checks each of the managed layer's
/// `[managed] pins` against it via [`pin_violation`]. A relaxation returns a named
/// [`Error::Config`] diagnostic instead of silently folding the pin away or silently letting the
/// downstream value win; a permitted (equal-or-stricter) downstream value proceeds to the actual
/// three-layer fold, `merge(merge(managed, user), project)`, so managed acts as the base default
/// for every key the pin didn't block.
pub fn from_sources_with_managed(
    managed: Option<(&str, &str)>,
    user: Option<(&str, &str)>,
    project: Option<(&str, &str)>,
) -> Result<Config> {
    let parse = |source: Option<(&str, &str)>| -> Result<Config> {
        source
            .map(|(name, text)| parse_source(name, text))
            .transpose()
            .map(|value| value.unwrap_or_default())
    };
    let managed_cfg = parse(managed)?;
    let user_cfg = parse(user)?;
    let project_cfg = parse(project)?;

    // What user+project alone would produce, untainted by the managed floor — pin violations are
    // judged against this, not against the final fold, so detection never depends on fold order.
    let downstream = merge(user_cfg.clone(), project_cfg.clone());

    for name in &managed_cfg.managed.pins {
        let key = PinnableKey::parse(name).ok_or_else(|| {
            Error::Config(format!(
                "managed config: `[managed] pins` names `{name}`, which is not a recognized \
                 pinnable key (see PinnableKey::ALL for the supported set)"
            ))
        })?;
        if let Some(reason) = pin_violation(key, &managed_cfg, &downstream) {
            return Err(Error::Config(reason));
        }
    }

    Ok(merge(merge(managed_cfg, user_cfg), project_cfg))
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

/// Re-render `current` (usually `~/.flux/config.toml`) with the TUI theme set to `theme`,
/// round-tripping every other setting — the pure half of the `/theme` persistence path (C-104).
pub fn render_theme(current: Option<(&str, &str)>, theme: &str) -> Result<String> {
    let mut cfg = current
        .map(|(source, text)| parse_source(source, text))
        .transpose()?
        .unwrap_or_default();
    cfg.theme = Some(theme.to_string());
    toml::to_string_pretty(&cfg).map_err(|error| Error::Config(error.to_string()))
}

/// Re-render `current` (usually `~/.flux/config.toml`) with `host` upserted into the `[[host]]`
/// table by id, round-tripping every other setting — the pure half of `flux host add` (C-649).
/// The guarded outer control plane owns the atomic write.
pub fn render_host_upsert(current: Option<(&str, &str)>, host: HostEntry) -> Result<String> {
    let mut cfg = current
        .map(|(source, text)| parse_source(source, text))
        .transpose()?
        .unwrap_or_default();
    if let Some(slot) = cfg.hosts.iter_mut().find(|h| h.id == host.id) {
        *slot = host;
    } else {
        cfg.hosts.push(host);
    }
    toml::to_string_pretty(&cfg).map_err(|error| Error::Config(error.to_string()))
}

/// Re-render `current` with the `[[host]]` entry named `id` removed, round-tripping every other
/// setting — the pure half of `flux host rm` (C-649). `Ok(None)` when no such entry is declared
/// in this document, so the caller can distinguish "nothing to do here" from a write.
pub fn render_host_removal(current: Option<(&str, &str)>, id: &str) -> Result<Option<String>> {
    let mut cfg = current
        .map(|(source, text)| parse_source(source, text))
        .transpose()?
        .unwrap_or_default();
    let before = cfg.hosts.len();
    cfg.hosts.retain(|h| h.id != id);
    if cfg.hosts.len() == before {
        return Ok(None);
    }
    toml::to_string_pretty(&cfg)
        .map(Some)
        .map_err(|error| Error::Config(error.to_string()))
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
        theme: project.theme.or(user.theme),
        allow_private_net: user.allow_private_net || project.allow_private_net,
        private_net: merge_private_net(user.private_net, project.private_net),
        web: WebConfig {
            allowed_secrets: match (user.web.allowed_secrets, project.web.allowed_secrets) {
                (None, None) => None,
                (Some(entries), None) | (None, Some(entries)) => Some(dedupe(entries)),
                (Some(user), Some(project)) => Some(dedupe([user, project].concat())),
            },
        },
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
        hosts: merge_hosts(user.hosts, project.hosts),
        exchange: ExchangeConfig {
            host: project.exchange.host.or(user.exchange.host),
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
            max_concurrent_tool_calls: project
                .limits
                .max_concurrent_tool_calls
                .or(user.limits.max_concurrent_tool_calls),
            max_live_agents: project
                .limits
                .max_live_agents
                .or(user.limits.max_live_agents),
            tool_call_queue_timeout_ms: project
                .limits
                .tool_call_queue_timeout_ms
                .or(user.limits.tool_call_queue_timeout_ms),
            max_retained_result_bytes: project
                .limits
                .max_retained_result_bytes
                .or(user.limits.max_retained_result_bytes),
            max_evidence_payload_bytes: project
                .limits
                .max_evidence_payload_bytes
                .or(user.limits.max_evidence_payload_bytes),
        },
        server: ServerConfig {
            // Same scalar rule throughout: a project value (including an explicit 0/false)
            // overrides the user's.
            a2a_session_ttl_secs: project
                .server
                .a2a_session_ttl_secs
                .or(user.server.a2a_session_ttl_secs),
            requests_per_minute: project
                .server
                .requests_per_minute
                .or(user.server.requests_per_minute),
            max_inflight_per_principal: project
                .server
                .max_inflight_per_principal
                .or(user.server.max_inflight_per_principal),
            provider_calls_per_day: project
                .server
                .provider_calls_per_day
                .or(user.server.provider_calls_per_day),
            provider_spend_usd_per_day: project
                .server
                .provider_spend_usd_per_day
                .or(user.server.provider_spend_usd_per_day),
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
            model_invoked: user.skills.model_invoked || project.skills.model_invoked,
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
        // Concatenate like the permission lists (order carries no meaning; user first, then
        // project) — a project's disable list refines (adds to) the user's, never silently drops it.
        tools: ToolsConfig {
            disable: dedupe([user.tools.disable, project.tools.disable].concat()),
        },
        // Scalars: a project value overrides the user's, same rule as `model`/`limits`.
        consult: ConsultConfig {
            model: project.consult.model.or(user.consult.model),
            max_calls: project.consult.max_calls.or(user.consult.max_calls),
        },
        // `enabled` is OR'd (a project may turn the op on even if the user's own config didn't;
        // mirrors the security-tightening direction of `enable_shell`-style switches — either side
        // opting in is enough). The bounds are scalars: a project value overrides the user's.
        wakeup: WakeupConfig {
            enabled: user.wakeup.enabled || project.wakeup.enabled,
            max_horizon_secs: project
                .wakeup
                .max_horizon_secs
                .or(user.wakeup.max_horizon_secs),
            max_pending_per_session: project
                .wakeup
                .max_pending_per_session
                .or(user.wakeup.max_pending_per_session),
        },
        // Meaningful only when one side is the managed layer itself (see
        // `from_sources_with_managed`); harmless union otherwise.
        managed: ManagedMeta {
            pins: dedupe([user.managed.pins, project.managed.pins].concat()),
        },
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

/// Merge `[[host]]` declarations: user first, then project — a project entry with the same `id`
/// overrides the user's (so a repo can retarget a named binding), otherwise it is appended.
/// Insertion order is preserved (deterministic display / registry seeding), mirroring
/// [`merge_static_endpoints`].
fn merge_hosts(user: Vec<HostEntry>, project: Vec<HostEntry>) -> Vec<HostEntry> {
    let mut out = user;
    for host in project {
        if let Some(slot) = out.iter_mut().find(|h| h.id == host.id) {
            *slot = host;
        } else {
            out.push(host);
        }
    }
    out
}

/// Load and merge the project config at `<cwd>/.flux/config.toml` with **no** user layer.
#[cfg(test)]
fn load(cwd: &Path) -> Result<Config> {
    load_in(cwd, None)
}

/// [`load`] with an explicitly pinned home (C-332): merges `<home>/.flux/config.toml` (user) then
/// `<cwd>/.flux/config.toml` (project). Mirrors `flux_runtime::metadata::load_config_in`, whose
/// `DiscoveryEnv` this crate (L0) cannot depend on — the idiom is the same value-held home.
#[cfg(test)]
fn load_in(cwd: &Path, home: Option<&Path>) -> Result<Config> {
    let user = match home_config_path(home) {
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

/// The user group manifest under an *explicitly passed* home — see [`home_config_path`] (C-332).
#[cfg(test)]
fn home_groups_path(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| h.join(".flux").join("groups.toml"))
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
    load_groups_in(cwd, None)
}

/// [`load_groups`] with an explicitly pinned home (C-332) — see [`load_in`].
#[cfg(test)]
fn load_groups_in(cwd: &Path, home: Option<&Path>) -> Vec<flux_evidence::ToolGroup> {
    let mut out: Vec<flux_evidence::ToolGroup> = Vec::new();
    let paths = home_groups_path(home)
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
///
/// C-332 left exactly **three** holders, and only for the one thing a value cannot express here:
/// `Config::skill_dir_paths` / `workspace_add_dirs` / `sandbox_writable` expand a leading `~/`
/// against process `HOME` in *production*, so the tests asserting that expansion must repoint it.
/// Every other config test now pins its user layer by value through [`load_in`] and takes no lock.
/// If that production expansion ever grows its own seam (C-392), this lock goes with it — a lock
/// nobody takes is worse than none, because the next test copies the pattern from a test that no
/// longer needs it.
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
        // No user layer at all, pinned by value — so only the project file is read (C-332).
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
    fn web_secret_entries_parse_with_their_scope_and_merge_without_dropping_either_layer() {
        let user = toml::from_str::<Config>(
            r#"
[web]
allowed_secrets = ["GITHUB_TOKEN;to=api.github.com;in=header"]
"#,
        )
        .unwrap();
        let project = toml::from_str::<Config>(
            r#"
[web]
allowed_secrets = ["ISSUE_TOKEN;to=issues.example;by=alice"]
"#,
        )
        .unwrap();

        let merged = merge(user, project);
        assert_eq!(
            merged.web.allowed_secrets,
            Some(vec![
                "GITHUB_TOKEN;to=api.github.com;in=header".into(),
                "ISSUE_TOKEN;to=issues.example;by=alice".into(),
            ])
        );
        assert_eq!(
            toml::from_str::<Config>("[web]\nallowed_secrets = []")
                .unwrap()
                .web
                .allowed_secrets,
            Some(Vec::new()),
            "an explicit empty list must remain distinguishable from the env-fallback default"
        );
    }

    /// C-290: a file-configured host reaches the runtime resource ceilings through `[limits]`,
    /// alongside the token budget that was already there.
    #[test]
    fn runtime_resource_ceilings_parse_from_the_limits_table() {
        let config = toml::from_str::<Config>(
            r#"
[limits]
turn_token_budget = 100000
max_concurrent_tool_calls = 4
max_live_agents = 6
tool_call_queue_timeout_ms = 2500
max_retained_result_bytes = 1048576
max_evidence_payload_bytes = 262144
"#,
        )
        .unwrap();
        assert_eq!(config.limits.turn_token_budget, Some(100_000));
        assert_eq!(config.limits.max_concurrent_tool_calls, Some(4));
        assert_eq!(config.limits.max_live_agents, Some(6));
        assert_eq!(config.limits.tool_call_queue_timeout_ms, Some(2500));
        assert_eq!(config.limits.max_retained_result_bytes, Some(1_048_576));
        assert_eq!(
            config.limits.max_evidence_payload_bytes,
            Some(262_144),
            "C-298: the evidence ceiling is configured in the same [limits] table, not a second place"
        );
    }

    /// The new ceilings follow the same scalar merge rule as `turn_token_budget`: a project value
    /// overrides the user's, and a value the project left unset survives from the user layer.
    #[test]
    fn runtime_resource_ceilings_merge_as_scalars() {
        let mut user = Config::default();
        user.limits.max_concurrent_tool_calls = Some(2);
        user.limits.max_live_agents = Some(3);
        user.limits.max_retained_result_bytes = Some(1024);
        user.limits.tool_call_queue_timeout_ms = Some(9_000);
        user.limits.max_evidence_payload_bytes = Some(4_096);

        let mut project = Config::default();
        project.limits.max_concurrent_tool_calls = Some(8);
        project.limits.max_live_agents = Some(12);
        project.limits.max_evidence_payload_bytes = Some(65_536);

        let merged = merge(user, project);
        assert_eq!(merged.limits.max_concurrent_tool_calls, Some(8));
        assert_eq!(merged.limits.max_live_agents, Some(12));
        assert_eq!(merged.limits.max_evidence_payload_bytes, Some(65_536));
        assert_eq!(
            merged.limits.max_retained_result_bytes,
            Some(1024),
            "project left it unset, so the user's value survives"
        );
        assert_eq!(merged.limits.tool_call_queue_timeout_ms, Some(9_000));
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
    fn tool_disable_matches_exact_names_and_family_globs() {
        // Exact name.
        assert!(tool_disable_matches("bash", "bash"));
        assert!(!tool_disable_matches("bash", "bash2"));
        // `family.*` matches every op dotted under that family.
        assert!(tool_disable_matches("browser.*", "browser.navigate"));
        assert!(tool_disable_matches("browser.*", "browser.click"));
        assert!(!tool_disable_matches("browser.*", "browserish.foo"));
        // A bare family name with no `.*` is an exact match only — it does not implicitly glob.
        assert!(!tool_disable_matches("browser", "browser.navigate"));
        // Different family, no match.
        assert!(!tool_disable_matches("web.*", "browser.navigate"));
        assert!(tool_disable_matches("web.*", "web.fetch"));
    }

    #[test]
    fn tools_disable_layers_user_and_project_with_precedence_and_dedup() {
        let mut user = Config::default();
        user.tools.disable = vec!["browser.*".into(), "bash".into()];
        let mut project = Config::default();
        project.tools.disable = vec!["web.*".into(), "bash".into()];

        let merged = merge(user, project);
        assert_eq!(
            merged.tools.disable,
            vec![
                "browser.*".to_string(),
                "bash".to_string(),
                "web.*".to_string()
            ],
            "user entries first, project entries appended, duplicates collapsed"
        );
    }

    #[test]
    fn tools_disable_parses_from_toml() {
        let cfg: Config = toml::from_str(
            r#"
[tools]
disable = ["browser.*", "web.*"]
"#,
        )
        .unwrap();
        assert_eq!(cfg.tools.disable, vec!["browser.*", "web.*"]);
        assert!(!cfg.tools.is_default());
        assert!(Config::default().tools.is_default());
    }

    /// A-96: `[consult] model` parses, is absent by default, and a project value wins over the
    /// user's on merge — same scalar-override rule as `model`/`limits`.
    #[test]
    fn consult_config_parses_and_project_overrides_user_on_merge() {
        let cfg: Config = toml::from_str(
            r#"
[consult]
model = "openrouter/anthropic/claude-opus-4.6"
max_calls = 3
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.consult.model.as_deref(),
            Some("openrouter/anthropic/claude-opus-4.6")
        );
        assert_eq!(cfg.consult.max_calls, Some(3));
        assert!(!cfg.consult.is_default());
        assert!(Config::default().consult.is_default());

        let mut user = Config::default();
        user.consult.model = Some("user/model".into());
        user.consult.max_calls = Some(1);
        let mut project = Config::default();
        project.consult.model = Some("project/model".into());
        let merged = merge(user, project);
        assert_eq!(merged.consult.model.as_deref(), Some("project/model"));
        assert_eq!(
            merged.consult.max_calls,
            Some(1),
            "project left max_calls unset, so the user's value survives"
        );
    }

    /// A-98: `[wakeup]` parses, is off by default, and merges with `enabled` OR'd (either layer
    /// opting in is enough) while the numeric bounds follow the usual project-overrides-user rule.
    #[test]
    fn wakeup_config_parses_and_merges_with_or_semantics_for_enabled() {
        let cfg: Config = toml::from_str(
            r#"
[wakeup]
enabled = true
max_horizon_secs = 3600
max_pending_per_session = 3
"#,
        )
        .unwrap();
        assert!(cfg.wakeup.enabled);
        assert_eq!(cfg.wakeup.max_horizon_secs, Some(3600));
        assert_eq!(cfg.wakeup.max_pending_per_session, Some(3));
        assert!(!cfg.wakeup.is_default());
        assert!(Config::default().wakeup.is_default());

        // `enabled` is OR'd: the user turned it on, the project says nothing — still on.
        let mut user = Config::default();
        user.wakeup.enabled = true;
        user.wakeup.max_pending_per_session = Some(2);
        let mut project = Config::default();
        project.wakeup.max_horizon_secs = Some(600);
        let merged = merge(user, project);
        assert!(merged.wakeup.enabled, "either layer opting in is enough");
        assert_eq!(merged.wakeup.max_horizon_secs, Some(600));
        assert_eq!(
            merged.wakeup.max_pending_per_session,
            Some(2),
            "project left this unset, so the user's value survives"
        );
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
    fn hosts_parse_from_config() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
[[host]]
id = "build-farm"
backend = "remote"
url = "https://farm.example:8443"
credential_ref = "env/FARM_TOKEN"
ca_cert = "/etc/flux/farm-ca.pem"
labels = { region = "eu" }

[[host]]
id = "here"
backend = "local"
"#,
        );
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.hosts.len(), 2);
        let farm = &cfg.hosts[0];
        assert_eq!(farm.id, "build-farm");
        assert_eq!(farm.backend, HostBackendKind::Remote);
        assert_eq!(farm.url.as_deref(), Some("https://farm.example:8443"));
        assert_eq!(farm.credential_ref.as_deref(), Some("env/FARM_TOKEN"));
        // C-684: the private CA the endpoint chains to is a declarable *location*, so a binding
        // can reach an operator-managed substrate by name. It parses as an ordinary key rather
        // than being refused by `deny_unknown_fields`.
        assert_eq!(farm.ca_cert.as_deref(), Some("/etc/flux/farm-ca.pem"));
        assert_eq!(farm.labels.get("region").map(String::as_str), Some("eu"));
        // Omitted is the public-trust default, not an empty string.
        assert!(cfg.hosts[1].ca_cert.is_none());
        // A minimal declaration: just a name and a backend kind (the local substrate needs no
        // address and no credential).
        assert_eq!(cfg.hosts[1].id, "here");
        assert_eq!(cfg.hosts[1].backend, HostBackendKind::Local);
        assert!(cfg.hosts[1].url.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-677: a `microvm` binding is declarable in `[[host]]` — with the endpoint its guest serves,
    /// or without one yet. Both are legal declarations because flux never provisions the guest: the
    /// endpoint comes to exist through C-480's VM/microVM profile, and until it does the binding is
    /// honestly unwired rather than a config error. The entry's own hard errors are unchanged.
    #[test]
    fn a_microvm_host_binding_parses_with_or_without_a_served_endpoint() {
        let dir = temp_dir();
        write_project(
            &dir,
            r#"
[[host]]
id = "vm-guest"
backend = "microvm"
url = "https://guest.internal:8443"
credential_ref = "env/GUEST_TOKEN"
grant = ["operator"]

[[host]]
id = "vm-planned"
backend = "microvm"
"#,
        );
        let cfg = load(&dir).unwrap();
        assert_eq!(cfg.hosts.len(), 2);
        let served = &cfg.hosts[0];
        assert_eq!(served.backend.as_str(), "microvm");
        assert_eq!(served.url.as_deref(), Some("https://guest.internal:8443"));
        assert_eq!(served.credential_ref.as_deref(), Some("env/GUEST_TOKEN"));
        // Declared before the guest exists: no address, and that is not a parse error.
        assert_eq!(cfg.hosts[1].backend.as_str(), "microvm");
        assert!(cfg.hosts[1].url.is_none());
        std::fs::remove_dir_all(&dir).ok();

        // The unknown-key hard error is unchanged for the new kind — a dropped typo in a substrate
        // binding stays a safety problem, not a formatting one.
        let dir = temp_dir();
        write_project(
            &dir,
            "[[host]]\nid = \"vm\"\nbackend = \"microvm\"\ncredentialref = \"env/X\"\n",
        );
        let err = load(&dir).unwrap_err();
        assert!(err.to_string().contains("credentialref"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_host_backend_kind_is_a_hard_config_error() {
        // C-648: the backend vocabulary is closed. A typo'd or unknown kind must fail the whole
        // config parse — a substrate binding silently skipped is a safety problem.
        let dir = temp_dir();
        write_project(&dir, "[[host]]\nid = \"warp-farm\"\nbackend = \"warp\"\n");
        let err = load(&dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("warp") || msg.contains("unknown variant"),
            "names the bad kind: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_keys_in_host_entries_are_rejected() {
        // Unlike `[[endpoint.static]]`, a `[[host]]` entry refuses unknown keys outright: a
        // dropped `credentialref = …` typo would silently change which substrate a session binds.
        let dir = temp_dir();
        write_project(
            &dir,
            "[[host]]\nid = \"h\"\nbackend = \"local\"\ncredentialref = \"env/X\"\n",
        );
        let err = load(&dir).unwrap_err();
        assert!(err.to_string().contains("credentialref"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge_hosts_project_overrides_user_by_id() {
        let host = |id: &str, url: &str| HostEntry {
            id: id.into(),
            backend: HostBackendKind::Remote,
            url: Some(url.into()),
            credential_ref: None,
            ca_cert: None,
            grant: Vec::new(),
            labels: Default::default(),
            ssh: None,
        };
        let user = vec![
            host("farm", "https://user-farm:8443"),
            host("gpu", "https://gpu:8443"),
        ];
        let project = vec![host("farm", "https://project-farm:8443")];
        let merged = merge_hosts(user, project);
        // `farm` retargeted to the project url in place; `gpu` retained; order preserved.
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, "farm");
        assert_eq!(merged[0].url.as_deref(), Some("https://project-farm:8443"));
        assert_eq!(merged[1].id, "gpu");
    }

    #[test]
    fn render_host_upsert_and_removal_round_trip() {
        let base = "enable_shell = true\n";
        let entry = HostEntry {
            id: "farm".into(),
            backend: HostBackendKind::Remote,
            url: Some("https://farm.example:8443".into()),
            credential_ref: Some("env/FARM_TOKEN".into()),
            ca_cert: None,
            grant: Vec::new(),
            labels: Default::default(),
            ssh: None,
        };
        let body = render_host_upsert(Some(("test", base)), entry.clone()).unwrap();
        assert!(body.contains("enable_shell = true"), "round-trips: {body}");
        assert!(body.contains("[[host]]"), "{body}");

        // A second upsert with the same id retargets in place rather than appending.
        let retargeted = HostEntry {
            url: Some("https://elsewhere.example:8443".into()),
            ..entry
        };
        let body = render_host_upsert(Some(("test", &body)), retargeted).unwrap();
        assert_eq!(body.matches("[[host]]").count(), 1, "{body}");
        assert!(body.contains("https://elsewhere.example:8443"), "{body}");

        // Removal drops the entry, keeps everything else, and reports absence as None.
        let removed = render_host_removal(Some(("test", &body)), "farm")
            .unwrap()
            .expect("declared entry removes");
        assert!(!removed.contains("[[host]]"), "{removed}");
        assert!(removed.contains("enable_shell = true"), "{removed}");
        assert!(render_host_removal(Some(("test", &removed)), "farm")
            .unwrap()
            .is_none());
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

        let cfg = load_in(&project, Some(&home)).unwrap();
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
        write_project(
            &project,
            r#"
[private_net.plugins]
gitlab = ["gitlab.internal"]

[private_net.endpoints]
"gitlab:api.endpoint" = ["api.internal"]
"#,
        );

        let cfg = load_in(&project, Some(&home)).unwrap();
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
        std::fs::write(
            home.join(".flux").join("config.toml"),
            "[server]\na2a_session_ttl_secs = 120\n",
        )
        .unwrap();

        // User-only: the user value beats the default.
        let cfg = load_in(&project, Some(&home)).unwrap();
        assert_eq!(cfg.a2a_session_ttl_secs(), 120);

        // Project sets the disable value 0: it overrides the user's 120 (an explicit 0 is a
        // real setting, not "absent" — `Some(0)` must survive the merge).
        write_project(&project, "[server]\na2a_session_ttl_secs = 0\n");
        let cfg = load_in(&project, Some(&home)).unwrap();
        assert_eq!(cfg.server.a2a_session_ttl_secs, Some(0));
        assert_eq!(cfg.a2a_session_ttl_secs(), 0, "0 = never prune");

        std::fs::remove_dir_all(&project).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn daemon_resource_budgets_merge_project_over_user() {
        let project = temp_dir();
        let home = temp_dir();
        std::fs::write(
            home.join(".flux").join("config.toml"),
            "[server]\nrequests_per_minute = 10\nmax_inflight_per_principal = 2\n\
             provider_calls_per_day = 100\nprovider_spend_usd_per_day = 5.0\n",
        )
        .unwrap();
        write_project(
            &project,
            "[server]\nrequests_per_minute = 20\nprovider_spend_usd_per_day = 1.5\n",
        );

        let cfg = load_in(&project, Some(&home)).unwrap();
        assert_eq!(cfg.server.requests_per_minute, Some(20));
        assert_eq!(cfg.server.max_inflight_per_principal, Some(2));
        assert_eq!(cfg.server.provider_calls_per_day, Some(100));
        assert_eq!(cfg.server.provider_spend_usd_per_day, Some(1.5));

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

        let cfg = load_in(&project, Some(&home)).unwrap();
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

        let cfg = load_in(&project, Some(&home)).unwrap();
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

        let cfg = load_in(&project, Some(&home)).unwrap();
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

    /// C-86: a typo'd `[server]` introspection key (e.g. `introspction_require_account`) must be a
    /// hard parse error, not a silently dropped setting — otherwise the intended auth check stays
    /// *off* (fails open on a security control).
    #[test]
    fn unknown_server_key_is_rejected() {
        let err = toml::from_str::<Config>("[server]\nintrospction_require_account = true\n")
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
        // A malformed `[server]` table fails the whole load.
        let dir = temp_dir();
        write_project(&dir, "[server]\nintrospction_require_account = true\n");
        assert!(load(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// C-86: a typo'd `[limits]` budget key (`turn_tokn_budget`) must be rejected — otherwise the
    /// turn runs *unbounded* (fails open on a spend control).
    #[test]
    fn unknown_limits_key_is_rejected() {
        let err = toml::from_str::<Config>("[limits]\nturn_tokn_budget = 50000\n").unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    /// C-86: the remaining security/scope tables also fail closed on an unknown key.
    #[test]
    fn unknown_keys_in_scope_tables_are_rejected() {
        for body in [
            "[workspace]\nallw_all = true\n",
            "[skills]\ndir = []\n",
            "[endpoint]\ncros_plugin_credentials = []\n",
            "[private_net]\nweb2 = true\n",
            "[permissions]\nalow = []\n",
        ] {
            let err = toml::from_str::<Config>(body).unwrap_err();
            assert!(
                err.to_string().contains("unknown field"),
                "table body `{body}` should reject the unknown key: {err}"
            );
        }
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

    /// C-104: the `theme` field round-trips, merges project-over-user like other scalars, and
    /// `render_theme` sets it while preserving unrelated settings.
    #[test]
    fn theme_field_round_trips_and_renders() {
        let cfg = parse_source("user", "theme = \"light\"\n").unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("light"));

        let merged = from_sources(
            Some(("user", "theme = \"light\"\n")),
            Some(("project", "theme = \"dark\"\n")),
        )
        .unwrap();
        assert_eq!(merged.theme.as_deref(), Some("dark"));

        let rendered = render_theme(
            Some(("user", "model = \"mock\"\ntheme = \"dark\"\n")),
            "light",
        )
        .unwrap();
        let reparsed = parse_source("rendered", &rendered).unwrap();
        assert_eq!(reparsed.theme.as_deref(), Some("light"));
        assert_eq!(reparsed.model.as_deref(), Some("mock"));

        let fresh = render_theme(None, "mono").unwrap();
        assert_eq!(
            parse_source("fresh", &fresh).unwrap().theme.as_deref(),
            Some("mono")
        );
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

    // -----------------------------------------------------------------------
    // C-165: managed config tier — pins vs defaults, precedence, provenance.
    // -----------------------------------------------------------------------

    /// A third layer that sets a value neither user nor project touch is a real layer — not just
    /// an unused stub — asserted against the two pre-existing layers per the C-165 acceptance.
    #[test]
    fn managed_layer_default_wins_when_unset_downstream_but_loses_to_an_explicit_override() {
        let managed = Config {
            model: Some("org-default".into()),
            theme: Some("dark".into()),
            ..Default::default()
        };
        let user = Config::default();
        let project = Config {
            // A plain (non-pinned) managed value is still just a default: downstream may change it.
            theme: Some("light".into()),
            ..Default::default()
        };

        let merged = from_sources_with_managed(
            Some(("managed", &toml::to_string(&managed).unwrap())),
            Some(("user", &toml::to_string(&user).unwrap())),
            Some(("project", &toml::to_string(&project).unwrap())),
        )
        .unwrap();

        assert_eq!(
            merged.model.as_deref(),
            Some("org-default"),
            "the managed layer's value survives when nothing downstream sets it"
        );
        assert_eq!(
            merged.theme.as_deref(),
            Some("light"),
            "an un-pinned managed value is a default, not a pin — project may still change it"
        );
    }

    /// `from_sources` (the existing two-layer entry point) is unaffected by adding the managed
    /// tier — no managed source in, no managed behavior out.
    #[test]
    fn from_sources_without_managed_layer_is_unchanged() {
        let cfg = from_sources(
            Some(("user", "model = \"user-model\"\n")),
            Some(("project", "theme = \"mono\"\n")),
        )
        .unwrap();
        assert_eq!(cfg.model.as_deref(), Some("user-model"));
        assert_eq!(cfg.theme.as_deref(), Some("mono"));
    }

    /// The core C-165 contract: a pinned `private_net.web` floor refuses a downstream layer that
    /// would widen egress, but a downstream layer that leaves it alone or narrows it further is
    /// accepted — both directions of "relaxation is refused in the permissive direction only."
    #[test]
    fn pinned_private_net_web_refuses_widening_but_permits_narrowing_or_silence() {
        let mut managed = Config {
            private_net: PrivateNetConfig {
                web: PrivateNetGrant::Hosts(vec!["reports.internal".into()]),
                ..Default::default()
            },
            ..Default::default()
        };
        managed.managed.pins = vec!["private_net.web".to_string()];
        let managed_text = toml::to_string(&managed).unwrap();

        // Refused: a user config tries to grant unrestricted private-net egress.
        let widened = Config {
            private_net: PrivateNetConfig {
                web: PrivateNetGrant::Enabled(true),
                ..Default::default()
            },
            ..Default::default()
        };
        let err = from_sources_with_managed(
            Some(("managed", &managed_text)),
            Some(("user", &toml::to_string(&widened).unwrap())),
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("private_net.web"),
            "diagnostic names the pinned key: {err}"
        );

        // Permitted: downstream sets no opinion at all — the pinned floor just applies.
        let ok = from_sources_with_managed(Some(("managed", &managed_text)), None, None).unwrap();
        assert_eq!(
            ok.private_net.web,
            PrivateNetGrant::Hosts(vec!["reports.internal".into()])
        );

        // Permitted: downstream is strictly narrower (denies everything) — more restrictive than
        // the pinned floor is always allowed.
        let narrower = Config {
            private_net: PrivateNetConfig {
                web: PrivateNetGrant::Enabled(false),
                ..Default::default()
            },
            ..Default::default()
        };
        let ok = from_sources_with_managed(
            Some(("managed", &managed_text)),
            None,
            Some(("project", &toml::to_string(&narrower).unwrap())),
        )
        .unwrap();
        assert_eq!(
            ok.private_net.web,
            PrivateNetGrant::Hosts(vec!["reports.internal".into()]),
            "an explicit-but-narrower project value doesn't relax the pin, so the floor still wins"
        );
    }

    /// A pinned authorization floor (`policy`) refuses any additional downstream grant — every
    /// grant only ever widens what's allowed, so once pinned the effective policy is closed.
    #[test]
    fn pinned_policy_refuses_any_additional_downstream_grant() {
        use flux_policy::{
            Action, Grant, ResourceKind, ResourceRef, SubjectKind, SubjectRef, TrustLevel,
        };

        let mut managed = Config {
            policy: Some(AuthorizationPolicy {
                grants: vec![Grant {
                    subjects: vec![SubjectRef {
                        kind: SubjectKind::User,
                        id: "*".into(),
                    }],
                    resources: vec![ResourceRef::any(ResourceKind::Workspace)],
                    actions: vec![Action::from("workspace.read")],
                    required_trust: TrustLevel::Untrusted,
                    required_scopes: Vec::new(),
                    requires_approval: false,
                }],
            }),
            ..Default::default()
        };
        managed.managed.pins = vec!["policy".to_string()];
        let managed_text = toml::to_string(&managed).unwrap();

        // Refused: a project adds its own grant on top of the pinned floor.
        let extra = Config {
            policy: Some(AuthorizationPolicy {
                grants: vec![Grant {
                    subjects: vec![SubjectRef {
                        kind: SubjectKind::User,
                        id: "*".into(),
                    }],
                    resources: vec![ResourceRef::any(ResourceKind::Workspace)],
                    actions: vec![Action::from("workspace.write")],
                    required_trust: TrustLevel::Untrusted,
                    required_scopes: Vec::new(),
                    requires_approval: false,
                }],
            }),
            ..Default::default()
        };
        let err = from_sources_with_managed(
            Some(("managed", &managed_text)),
            None,
            Some(("project", &toml::to_string(&extra).unwrap())),
        )
        .unwrap_err();
        assert!(err.to_string().contains("policy"), "{err}");

        // Permitted: nothing downstream adds a grant — the managed floor is the whole policy.
        let ok = from_sources_with_managed(Some(("managed", &managed_text)), None, None).unwrap();
        assert_eq!(ok.policy.unwrap().grants.len(), 1);
    }

    /// `[tools] disable` is pinnable for documentation/audit purposes, but its merge is a union —
    /// downstream can only ever add more disables, never remove a pinned one — so both directions
    /// (adding more, or leaving it alone) are always permitted, never refused.
    #[test]
    fn pinned_tools_disable_permits_additional_downstream_entries_in_both_cases() {
        let mut managed = Config {
            tools: ToolsConfig {
                disable: vec!["browser.*".into()],
            },
            ..Default::default()
        };
        managed.managed.pins = vec!["tools.disable".to_string()];
        let managed_text = toml::to_string(&managed).unwrap();

        let project = Config {
            tools: ToolsConfig {
                disable: vec!["web.*".into()],
            },
            ..Default::default()
        };
        let merged = from_sources_with_managed(
            Some(("managed", &managed_text)),
            None,
            Some(("project", &toml::to_string(&project).unwrap())),
        )
        .unwrap();
        assert!(merged.tools.disable.contains(&"browser.*".to_string()));
        assert!(merged.tools.disable.contains(&"web.*".to_string()));
    }

    /// An unrecognized `[managed] pins` entry is a load-time error, not a silently-ignored typo.
    #[test]
    fn unrecognized_pin_name_is_a_load_time_error() {
        let err = from_sources_with_managed(
            Some(("managed", "[managed]\npins = [\"totally.bogus\"]\n")),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("totally.bogus"), "{err}");
    }

    /// `effective_settings` reports, per pinnable key, which layer supplied the winning value and
    /// whether it's pinned — the API-level "why can't I enable this" answer (C-165).
    #[test]
    fn effective_settings_reports_layer_and_pin_status() {
        let mut managed = Config {
            tools: ToolsConfig {
                disable: vec!["browser.*".into()],
            },
            ..Default::default()
        };
        managed.managed.pins = vec!["tools.disable".to_string()];
        let user = Config::default();
        let project = Config {
            private_net: PrivateNetConfig {
                web: PrivateNetGrant::Hosts(vec!["docs.internal".into()]),
                ..Default::default()
            },
            ..Default::default()
        };

        let settings = effective_settings(&managed, &user, &project);

        let tools_disable = settings
            .iter()
            .find(|s| s.key == PinnableKey::ToolsDisable)
            .unwrap();
        assert_eq!(tools_disable.layer, ConfigLayer::Managed);
        assert!(tools_disable.pinned);

        let private_net_web = settings
            .iter()
            .find(|s| s.key == PinnableKey::PrivateNetWeb)
            .unwrap();
        assert_eq!(private_net_web.layer, ConfigLayer::Project);
        assert!(!private_net_web.pinned);

        let sandbox_enabled = settings
            .iter()
            .find(|s| s.key == PinnableKey::SandboxEnabled)
            .unwrap();
        assert_eq!(sandbox_enabled.layer, ConfigLayer::BuiltIn);
        assert!(!sandbox_enabled.pinned);
    }

    #[test]
    fn pinnable_key_parse_and_as_str_round_trip() {
        for (name, key) in PinnableKey::ALL {
            assert_eq!(PinnableKey::parse(name), Some(*key));
            assert_eq!(key.as_str(), *name);
        }
        assert_eq!(PinnableKey::parse("nope"), None);
    }
}
