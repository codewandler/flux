//! The generic **connector** adapter (`kind = "connector"`, D-216): one arm that reads a channel
//! binding out of a published connector manifest and drives it, instead of one hand-written arm per
//! vendor.
//!
//! # A manifest is untrusted input
//!
//! `~/.flux/connectors/<connector>.connector.toml` is a **published artifact**, and a published
//! artifact can be edited after publication. Every rule the producing repository enforces when it
//! compiles that file is therefore enforced *again* here, against the bytes actually on disk — and
//! all of it happens in [`ConnectorChannel::from_decl`], which the decl-only
//! [`build_channels`](crate::build_channels) calls **before any listener binds**. A rule checked
//! after the bind would already have exposed the endpoint by the time it fired.
//!
//! Two consequences worth stating, because they are what "untrusted" actually buys:
//!
//! - **No field read out of a manifest may influence a filesystem path.** The only path this module
//!   builds comes from the `connector`/`service` *settings*, which are validated by [`validate_name`]
//!   before anything is joined, and the read runs through a [`Workspace`] rooted at the connectors
//!   directory itself — so even a name that slipped the grammar, or a symlink planted inside that
//!   directory, cannot reach outside it.
//! - **Every rule is a positive requirement.** The wire model below is deliberately *not*
//!   `deny_unknown_fields` (a manifest from a newer `connector-cli` must still load), so a key this
//!   module does not model is ignored — and a *misspelled* key is therefore an absent key, which
//!   fails closed against a requirement rather than open against a prohibition.
//!
//! # What this arm can serve today, and what it refuses
//!
//! `transport = "webhook"` only. `poll` needs a `schedule` channel plus a trigger, and `socket` is
//! D-220's — both are load errors here rather than a channel that silently never fires.
//!
//! **A binding whose verification is `hmac` is refused at load**, because flux has no HMAC verifier
//! yet: C-291 (raw-body capture) and C-292 (the parameterized verifier this arm would feed) are both
//! still open. Constructing the channel anyway would bind a public endpoint that ignores the
//! signature the manifest declares — an unauthenticated trigger surface presented as a verified one,
//! which is the exact failure this whole design exists to prevent. The refusal is last in the
//! cascade, *after* every structural rule about the `HmacSpec` has been checked, so a defective spec
//! still reports its own defect rather than hiding behind "not implemented".

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use flux_lang::program::ChannelDecl;
use flux_system::{System, Workspace};

use crate::adapters::webhook::constant_time_eq;
use crate::config::ConnectorSettings;
use crate::{Channel, Deliverer};

/// The manifest filename suffix. Not an extension — `slack.connector.toml` has stem
/// `slack.connector`, which is why this is joined as a suffix rather than set with `set_extension`.
const MANIFEST_SUFFIX: &str = "connector.toml";

/// The longest a connector or service name may be. Generous for every id the producing repository
/// publishes, and short enough that a name can never be a path-length attack.
const MAX_NAME_LEN: usize = 64;

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The manifest wire model
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// The subset of a `.connector.toml` this arm reads.
///
/// It mirrors `connector-cli`'s emitter (`crates/connector-cli/src/seam.rs`, `fn manifest`) field for
/// field *for the fields flux acts on*, and stops there: `generator`, `gid`, `vendor`,
/// `description`, `base_url`, `api_version` and `module` are carried by the file and are not this
/// adapter's business. Modelling only what is validated or used keeps "unread field" and "unenforced
/// rule" the same thing, so a rule cannot quietly go missing behind a field nobody looks at.
///
/// The rule that follows from that, and the reason [`ManifestVerification::verified`] and
/// [`ManifestEvent::when`] are modelled at all: **a field that carries a rule is read even when
/// nothing acts on it.** `verified` is checked against `kind`; `when` is refused. Both are here
/// because leaving a rule-carrying field unread is how an edited manifest loads without comment.
#[derive(Debug, Deserialize)]
struct Manifest {
    /// The connector id, checked against the `connector` setting — the file you opened must be the
    /// connector you asked for.
    connector: String,
    /// The service, elided by the emitter when it is the reserved default.
    #[serde(default)]
    service: Option<String>,
    /// Every operation this manifest publishes. A `reply.operation` outside this list is a dangling
    /// reply.
    #[serde(default)]
    operations: Vec<String>,
    /// The declared events — a **closed set**, which is what lets an undeclared discriminator value
    /// be a logged no-op instead of a label a vendor gets to mint.
    #[serde(default)]
    events: Vec<ManifestEvent>,
    #[serde(default)]
    channels: Vec<ManifestChannel>,
}

#[derive(Debug, Deserialize)]
struct ManifestEvent {
    name: String,
    /// Field equalities that narrow one coarse vendor event into this one — GitHub's single `issues`
    /// event with an `action` field becoming `issues.opened`.
    ///
    /// Modelled **only in order to refuse it** (D-222). Ignoring a narrowing is not a harmless
    /// omission: the discriminator carries the *coarse* vendor value, which is not a member of the
    /// closed event set, so every delivery of a narrowed event would be a silent no-op that looks
    /// exactly like a vendor sending something nobody subscribed to. A load error says so once, at
    /// startup, instead.
    ///
    /// Not a deferral: the producing repository deliberately does not emit `when` into a manifest
    /// (`connector-cli/src/seam.rs` — *"`schema` and `when` are vendor JSON Schemas, and TOML has no
    /// `null`"*), and no shipped manifest carries the key. There is nothing here to match.
    #[serde(default)]
    when: BTreeMap<String, toml::Value>,
}

/// One `[[channels]]` block — a [`ChannelBinding`] as it survives the trip through TOML.
///
/// [`ChannelBinding`]: https://github.com/codewandler/flux-connectors
#[derive(Debug, Deserialize)]
struct ManifestChannel {
    name: String,
    transport: String,
    #[serde(default)]
    events: Vec<String>,
    /// **Tri-state, and the absent arm is the dangerous one.** Absent means the binding states
    /// nothing; on a `webhook` that is a load error, because silence is never a verification answer.
    #[serde(default)]
    verification: Option<ManifestVerification>,
    #[serde(default)]
    discriminator: Option<ManifestSelector>,
    #[serde(default)]
    delivery_id: Option<ManifestSelector>,
    #[serde(default)]
    payload: BTreeMap<String, String>,
    #[serde(default)]
    reply: Option<ManifestReply>,
}

#[derive(Debug, Deserialize)]
struct ManifestVerification {
    /// `hmac` | `none` | `connection` — the producing repository's own stable machine token.
    kind: String,
    /// The **derived** half of the pair `connector-cli` emits (`crates/connector-cli/src/seam.rs`,
    /// `struct ManifestVerification`: *"`kind` and `verified` are the pair `crate::inbound`
    /// documents: a consumer reads one boolean and does not have to learn the vocabulary"*).
    ///
    /// Read here **only to check it against `kind`.** Because the emitter derives it, the two can
    /// never disagree in a published file — so a file where they do disagree has been edited since
    /// publication, which is the entire class of defect this arm exists to catch. `kind = "none"`
    /// with `verified = true` is the dangerous direction: a manifest that says, to anything reading
    /// the boolean alone, that this endpoint is authenticated when it declares nothing that would
    /// authenticate it.
    ///
    /// `Option`, not `bool`: an absent key is "not stated", and this arm refuses an incoherent
    /// *statement* rather than inventing a requirement that the key be present.
    #[serde(default)]
    verified: Option<bool>,
    #[serde(default)]
    hmac: Option<ManifestHmac>,
}

/// The declared HMAC parameters. Every field here is *validated* at load even though no verifier
/// consumes them yet (C-291/C-292): a defect in this table is a defect whether or not flux can act
/// on the table, and reporting it at load is how it reaches whoever can fix it.
#[derive(Debug, Deserialize)]
struct ManifestHmac {
    algorithm: String,
    encoding: String,
    header: String,
    /// The signed-string template over `{body}` and `{timestamp}`.
    signed: String,
    /// The **name** of the credential holding the shared secret — never a value.
    secret: String,
    #[serde(default)]
    tolerance: Option<String>,
    #[serde(default)]
    timestamp_format: Option<String>,
    #[serde(default)]
    timestamp: Option<ManifestSelector>,
}

#[derive(Debug, Deserialize)]
struct ManifestSelector {
    /// `header` | `body`.
    source: String,
    /// The header name, or the dotted body path.
    name: String,
}

#[derive(Debug, Deserialize)]
struct ManifestReply {
    operation: String,
    #[serde(default)]
    bind: BTreeMap<String, String>,
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Names and paths
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Whether `name` may be joined onto the connectors directory as a filename component.
///
/// This is the first thing [`ConnectorChannel::from_decl`] does, and it runs **before** any path is
/// built. `connector = "../../etc"` must be refused, not resolved: the grammar admits one lowercase
/// ASCII segment, so `.`, `/`, `\` and `~` are all unspellable and traversal has nothing to stand on.
///
/// The read is *also* confined to the connectors directory by [`read_manifest`], so this grammar is
/// the outer of two independent guards rather than the only one.
fn validate_name(channel: &str, what: &str, name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("channel `{channel}`: {what} name must not be empty");
    }
    if name.len() > MAX_NAME_LEN {
        anyhow::bail!(
            "channel `{channel}`: {what} name `{name}` is longer than {MAX_NAME_LEN} characters"
        );
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or_default();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        anyhow::bail!(
            "channel `{channel}`: {what} name `{name}` must start with a lowercase ASCII letter or \
             digit — it is joined onto `~/.flux/connectors` as a filename, so it may not address a \
             directory"
        );
    }
    if let Some(bad) =
        chars.find(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && *c != '_' && *c != '-')
    {
        anyhow::bail!(
            "channel `{channel}`: {what} name `{name}` contains {bad:?}; a connector name is \
             lowercase ASCII letters, digits, `_` and `-` only — it is joined onto \
             `~/.flux/connectors` as a filename, never as a path"
        );
    }
    Ok(())
}

/// `~/.flux/connectors` — the home for an installed connector's manifest, beside `~/.flux/flows`,
/// which is already where the same connector's `.flux` module lands. One directory pair per
/// installed connector, not two mechanisms.
fn connectors_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("HOME is unset, so `~/.flux/connectors` cannot be located")
        })?;
    Ok(Path::new(&home).join(".flux").join("connectors"))
}

/// `<connector>.connector.toml`, or `<connector>-<service>.connector.toml` for a named service —
/// the producing repository's own naming rule (`connector-cli`'s `service_manifest_path`), which
/// elides the reserved default service.
fn manifest_file_name(connector: &str, service: Option<&str>) -> String {
    match service {
        Some(service) => format!("{connector}-{service}.{MANIFEST_SUFFIX}"),
        None => format!("{connector}.{MANIFEST_SUFFIX}"),
    }
}

/// Read a manifest **through `flux_system::System`**, confined to `dir`.
///
/// Rooting the workspace at the connectors directory itself is what makes the confinement total:
/// `resolve_read` admits only paths under that root, chases every symlink in the existing prefix,
/// and re-checks the physical target — so a symlink planted inside `~/.flux/connectors` cannot
/// redirect the read, and neither can a filename that escaped [`validate_name`]. A missing directory
/// is `None` rather than an error, so "this host has not installed that connector" reads as itself.
fn read_manifest(dir: &Path, file: &str) -> anyhow::Result<Option<String>> {
    let workspace = Workspace::new_optional(dir)
        .map_err(|e| anyhow::anyhow!("connectors directory {}: {e}", dir.display()))?;
    let Some(workspace) = workspace else {
        return Ok(None);
    };
    System::new(workspace)
        .read_optional_text(file)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", dir.join(file).display()))
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The channel
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Where one named value is read off an inbound request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Header,
    Body,
}

/// One named value read off an inbound request: the event discriminator, or the delivery id.
#[derive(Debug, Clone)]
struct Selector {
    source: Source,
    name: String,
}

impl Selector {
    fn read(&self, headers: &HeaderMap, body: &Value) -> Option<String> {
        match self.source {
            Source::Header => headers
                .get(&self.name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            Source::Body => match dotted(body, &self.name)? {
                Value::String(s) => Some(s.clone()),
                Value::Null => None,
                other => Some(other.to_string()),
            },
        }
    }
}

/// Resolve a dotted path into a JSON envelope — the one path grammar, the same one
/// `Param::wire` uses for request bodies. Deliberately not JSONPath.
fn dotted<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut here = root;
    for segment in path.split('.') {
        here = here.get(segment)?;
    }
    Some(here)
}

/// A channel binding, loaded and fully validated, over the `webhook` transport.
pub struct ConnectorChannel {
    name: String,
    addr: SocketAddr,
    path: String,
    token: Option<String>,
    /// The **closed** set of event names the binding declares. A discriminator value outside it is a
    /// logged no-op — never a label of its own, and never a fallback to the bare channel name.
    events: BTreeSet<String>,
    discriminator: Option<Selector>,
    delivery_id: Option<Selector>,
    payload: BTreeMap<String, String>,
    /// The operation that answers on this binding. Its *tool* is asserted to exist by
    /// [`crate::serve`], which has the registry the decl-only builder does not.
    reply_operation: Option<String>,
}

impl ConnectorChannel {
    /// Load the named binding out of the named connector's manifest, refusing everything refusable.
    ///
    /// The order of the cascade is load-bearing and not incidental: the name grammar runs before any
    /// path is built, every structural rule about a declared `HmacSpec` runs before the blanket
    /// "flux has no verifier yet" refusal, and the transport bind settings are parsed last — so each
    /// defect reports itself rather than being masked by a coarser one downstream.
    pub fn from_decl(decl: &ChannelDecl) -> anyhow::Result<Self> {
        let name = decl.name.as_str();
        let s: ConnectorSettings = serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{name}` settings: {e}"))?;

        // 1 ─ Names first. Nothing is joined onto a directory before this returns.
        validate_name(name, "connector", &s.connector)?;
        if let Some(service) = &s.service {
            validate_name(name, "service", service)?;
        }

        // 2 ─ Resolve the manifest. An explicit `manifest` override is operator input (the operator
        //     can already point flux at any file); the confinement below still applies to it, rooted
        //     at the file's own directory, so the read cannot walk out of wherever it points.
        let (dir, file) = match &s.manifest {
            Some(override_path) => {
                let path = Path::new(override_path);
                let dir = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."))
                    .to_path_buf();
                let file = path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "channel `{name}`: `manifest` {override_path:?} names no file"
                        )
                    })?
                    .to_string();
                (dir, file)
            }
            None => (
                connectors_dir()?,
                manifest_file_name(&s.connector, s.service.as_deref()),
            ),
        };
        let Some(text) = read_manifest(&dir, &file)? else {
            anyhow::bail!(
                "channel `{name}`: no manifest for connector `{}` at {} — install the connector, or \
                 set `manifest` to its path",
                s.connector,
                dir.join(&file).display()
            );
        };
        let manifest: Manifest = toml::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "channel `{name}`: {} is not a readable connector manifest: {e}",
                dir.join(&file).display()
            )
        })?;

        // 3 ─ The file you opened is the connector you asked for.
        if manifest.connector != s.connector {
            anyhow::bail!(
                "channel `{name}`: {} declares connector `{}`, but this channel asked for `{}`",
                dir.join(&file).display(),
                manifest.connector,
                s.connector
            );
        }
        if manifest.service.as_deref() != s.service.as_deref() {
            anyhow::bail!(
                "channel `{name}`: {} declares service {:?}, but this channel asked for {:?}",
                dir.join(&file).display(),
                manifest.service.as_deref().unwrap_or("<default>"),
                s.service.as_deref().unwrap_or("<default>")
            );
        }

        // 4 ─ The binding.
        let binding = manifest
            .channels
            .iter()
            .find(|c| c.name == s.binding)
            .ok_or_else(|| {
                let known: Vec<&str> = manifest.channels.iter().map(|c| c.name.as_str()).collect();
                anyhow::anyhow!(
                    "channel `{name}`: connector `{}` declares no binding `{}` — it declares {}",
                    s.connector,
                    s.binding,
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                )
            })?;

        // 5 ─ A transport this arm can serve.
        match binding.transport.as_str() {
            "webhook" => {}
            "poll" => anyhow::bail!(
                "channel `{name}`: binding `{}` is `transport = \"poll\"`, which this kind cannot \
                 serve — a poll is a `schedule` channel plus a trigger that calls the binding's \
                 cursor operation, not an inbound listener",
                s.binding
            ),
            "socket" => anyhow::bail!(
                "channel `{name}`: binding `{}` is `transport = \"socket\"`, which this kind cannot \
                 serve yet (D-220 ports the outbound socket loop)",
                s.binding
            ),
            other => anyhow::bail!(
                "channel `{name}`: binding `{}` declares unknown transport `{other}`",
                s.binding
            ),
        }

        // 6 ─ Verification. The tri-state, reproduced against the file.
        let verification = binding.verification.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{name}`: binding `{}` is a webhook and states no verification — silence \
                 is never a verification answer, and a published manifest always states one, so \
                 this file has been edited since it was published",
                s.binding
            )
        })?;
        // `kind` and `verified` are one value emitted twice, so a file where they disagree is a file
        // someone edited. Checked before the `kind` match so an *unknown* kind still reports itself
        // (there is no coherent boolean to hold an unknown token to).
        let derived = match verification.kind.as_str() {
            "none" => Some(false),
            "hmac" | "connection" => Some(true),
            _ => None,
        };
        if let (Some(derived), Some(stated)) = (derived, verification.verified) {
            if derived != stated {
                anyhow::bail!(
                    "channel `{name}`: binding `{}` states `verification.kind = \"{}\"` with \
                     `verified = {stated}`, but a published manifest derives one from the other — \
                     so this pair cannot have been emitted, and the file has been edited since it \
                     was published",
                    s.binding,
                    verification.kind
                );
            }
        }
        match verification.kind.as_str() {
            // Explicitly unverifiable: the vendor publishes no signature. Servable, and said loudly.
            "none" => {}
            "hmac" => {
                let hmac = verification.hmac.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "channel `{name}`: binding `{}` states `verification.kind = \"hmac\"` with \
                         no `[channels.verification.hmac]` table",
                        s.binding
                    )
                })?;
                validate_hmac(name, &s.binding, hmac)?;
                require_credential(name, &s, &hmac.secret)?;
                // Last, and only once the spec itself is known to be well-formed: flux has no
                // verifier to feed these parameters to yet. Binding the endpoint anyway would serve
                // an unauthenticated trigger surface while the manifest says it is signed.
                anyhow::bail!(
                    "channel `{name}`: binding `{}` requires HMAC verification, which this build \
                     cannot perform — the raw-body capture (C-291) and the signature verifier \
                     (C-292) are not implemented, so binding this endpoint would accept unsigned \
                     deliveries on a surface the manifest declares as verified",
                    s.binding
                );
            }
            "connection" => anyhow::bail!(
                "channel `{name}`: binding `{}` states `verification.kind = \"connection\"` on a \
                 webhook — a connection-authenticated verification belongs to a socket or a poll, \
                 so nothing proves who called this endpoint",
                s.binding
            ),
            other => anyhow::bail!(
                "channel `{name}`: binding `{}` states unknown `verification.kind = {other:?}`",
                s.binding
            ),
        }

        // 7 ─ The payload map: one grammar for symbols, one for paths.
        for (symbol, path) in &binding.payload {
            validate_symbol(name, &s.binding, symbol)?;
            validate_path(name, &s.binding, symbol, path)?;
        }

        // 8 ─ The reply, if any: it must name an operation this connector publishes, and it may only
        //     bind symbols the payload map declares.
        let reply_operation = match &binding.reply {
            None => None,
            Some(reply) => {
                if !manifest.operations.contains(&reply.operation) {
                    anyhow::bail!(
                        "channel `{name}`: binding `{}` replies with operation `{}`, which connector \
                         `{}` does not publish",
                        s.binding,
                        reply.operation,
                        s.connector
                    );
                }
                for (parameter, symbol) in &reply.bind {
                    if !binding.payload.contains_key(symbol) {
                        anyhow::bail!(
                            "channel `{name}`: binding `{}` binds reply parameter `{parameter}` to \
                             payload symbol `{symbol}`, which its `[channels.payload]` map does not \
                             declare",
                            s.binding
                        );
                    }
                }
                Some(reply.operation.clone())
            }
        };

        // 9 ─ The closed event set is the **binding's**, not the connector's: a binding carries the
        //     subset it actually receives, and a name in it that the manifest declares nowhere is a
        //     label this host could fire for an event that does not exist.
        let declared: BTreeMap<String, ManifestEvent> = manifest
            .events
            .into_iter()
            .map(|e| (e.name.clone(), e))
            .collect();
        for event in &binding.events {
            let Some(declaration) = declared.get(event) else {
                anyhow::bail!(
                    "channel `{name}`: binding `{}` carries event `{event}`, which connector `{}` \
                     declares nowhere",
                    s.binding,
                    s.connector
                );
            };
            // A narrowing this build cannot match is refused rather than ignored — see
            // [`ManifestEvent::when`] for why ignoring it is the worse failure.
            if !declaration.when.is_empty() {
                anyhow::bail!(
                    "channel `{name}`: binding `{}` carries event `{event}`, which connector `{}` \
                     narrows with a `when` condition this build cannot match — the discriminator \
                     would carry the coarse vendor event instead, and every `{event}` delivery \
                     would be a silent no-op",
                    s.binding,
                    s.connector
                );
            }
        }
        let events: BTreeSet<String> = binding.events.iter().cloned().collect();
        if binding.discriminator.is_some() && events.is_empty() {
            anyhow::bail!(
                "channel `{name}`: binding `{}` selects a discriminator but declares no events, so \
                 every delivery would be a no-op",
                s.binding
            );
        }

        // 10 ─ Selectors.
        let discriminator = binding
            .discriminator
            .as_ref()
            .map(|sel| selector(name, &s.binding, "discriminator", sel))
            .transpose()?;
        let delivery_id = binding
            .delivery_id
            .as_ref()
            .map(|sel| selector(name, &s.binding, "delivery_id", sel))
            .transpose()?;
        if delivery_id.is_some() && binding.payload.contains_key(DELIVERY_ID_SYMBOL) {
            anyhow::bail!(
                "channel `{name}`: binding `{}` declares both a `delivery_id` selector and a \
                 payload symbol named `{DELIVERY_ID_SYMBOL}`; one would silently overwrite the other",
                s.binding
            );
        }

        // 11 ─ The transport's own settings, last: nothing about *where* flux listens can excuse a
        //      defect in *what* it would serve.
        let addr = s.addr.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{name}`: binding `{}` is a webhook, so this channel needs an `addr` to \
                 listen on",
                s.binding
            )
        })?;
        let addr = SocketAddr::from_str(addr)
            .map_err(|e| anyhow::anyhow!("channel `{name}`: bad addr `{addr}`: {e}"))?;
        // **An empty token is not a token, and it is worse than none.**
        //
        // `token ""` — or `token secret "K"` where `K` is exported empty, because
        // `flux_app::resolve_secrets` resolves through `std::env::var`, which does not filter an
        // empty value — would otherwise arrive here as `Some("")`. The bearer check compares the
        // *presented* token, which is `""` when the request carries no `Authorization` header at
        // all, against the expected one; two empty byte strings are equal, so every anonymous
        // request would authenticate. On a host that auto-approves tools that is an open
        // remote-trigger surface presented as an authenticated one.
        //
        // [`authorized`] refuses an empty expected token as well, so neither half depends on the
        // other being right. This one is the half that runs **before a port is bound**, which is
        // the only half that can prevent the exposure rather than survive it.
        let token = match s.token.as_deref() {
            Some(token) if token.trim().is_empty() => anyhow::bail!(
                "channel `{name}`: `token` is set but empty, which would authenticate every \
                 request — including one carrying no `Authorization` header at all. Give it a \
                 value, or remove it (a loopback bind needs none). A `secret \"KEY\"` reference \
                 resolves to an empty string when `KEY` is exported empty."
            ),
            other => other.map(str::to_string),
        };
        // The host auto-approves tools, so an open non-loopback listener is a remote-trigger
        // surface. The binding states it cannot be verified, so a bearer token is the only thing
        // left that can attribute a delivery — mirroring `WebhookChannel`'s rule exactly.
        if !addr.ip().is_loopback() && token.is_none() {
            anyhow::bail!(
                "channel `{name}`: refusing to bind non-loopback {addr} for binding `{}`, whose \
                 verification is `none`, without a `token` (set `token secret \"KEY\"`)",
                s.binding
            );
        }
        let path = if s.path.starts_with('/') {
            s.path.clone()
        } else {
            format!("/{}", s.path)
        };

        Ok(Self {
            name: decl.name.clone(),
            addr,
            path,
            token,
            events,
            discriminator,
            delivery_id,
            payload: binding.payload.clone(),
            reply_operation,
        })
    }

    /// Build the axum router for this channel over `d` (exposed for hermetic tests).
    pub fn router(&self, d: Arc<dyn Deliverer>) -> Router {
        let state = Arc::new(BindingState {
            name: self.name.clone(),
            deliverer: d,
            token: self.token.clone(),
            events: self.events.clone(),
            discriminator: self.discriminator.clone(),
            delivery_id: self.delivery_id.clone(),
            payload: self.payload.clone(),
        });
        Router::new()
            .route(&self.path, post(handle))
            .with_state(state)
    }
}

/// The reserved payload symbol carrying the vendor's redelivery id, when the binding selects one.
const DELIVERY_ID_SYMBOL: &str = "delivery_id";

/// Translate a manifest selector, refusing a source this host cannot read.
fn selector(
    channel: &str,
    binding: &str,
    what: &str,
    sel: &ManifestSelector,
) -> anyhow::Result<Selector> {
    if sel.name.is_empty() {
        anyhow::bail!("channel `{channel}`: binding `{binding}`'s {what} names nothing");
    }
    let source = match sel.source.as_str() {
        "header" => {
            validate_header(channel, binding, what, &sel.name)?;
            Source::Header
        }
        "body" => {
            validate_path(channel, binding, what, &sel.name)?;
            Source::Body
        }
        other => anyhow::bail!(
            "channel `{channel}`: binding `{binding}`'s {what} reads from unknown source \
             `{other}` — a selector reads a `header` or a `body` path"
        ),
    };
    Ok(Selector {
        source,
        name: sel.name.clone(),
    })
}

/// Every structural rule the producing repository's loader makes about an [`ManifestHmac`], made
/// again against the file. Each one is its own refusal so a defect reports itself.
fn validate_hmac(channel: &str, binding: &str, hmac: &ManifestHmac) -> anyhow::Result<()> {
    if !matches!(hmac.algorithm.as_str(), "sha1" | "sha256") {
        anyhow::bail!(
            "channel `{channel}`: binding `{binding}` signs with unknown algorithm `{}`",
            hmac.algorithm
        );
    }
    if !matches!(hmac.encoding.as_str(), "hex" | "base64") {
        anyhow::bail!(
            "channel `{channel}`: binding `{binding}` spells its digest with unknown encoding `{}`",
            hmac.encoding
        );
    }
    validate_header(channel, binding, "signature header", &hmac.header)?;
    if hmac.secret.trim().is_empty() {
        anyhow::bail!("channel `{channel}`: binding `{binding}` names no signing credential");
    }

    // The template. `{body}` is mandatory: a template that omits it signs a string the payload never
    // enters, so one captured signature verifies every forged payload — and every other thing about
    // such a declaration reads as correct.
    let mut rest = hmac.signed.as_str();
    let mut has_body = false;
    let mut has_timestamp = false;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after.find('}').ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{channel}`: binding `{binding}`'s signed template `{}` has an unclosed \
                 placeholder",
                hmac.signed
            )
        })?;
        match &after[..close] {
            "body" => has_body = true,
            "timestamp" => has_timestamp = true,
            other => anyhow::bail!(
                "channel `{channel}`: binding `{binding}`'s signed template interpolates unknown \
                 placeholder `{{{other}}}` — a host that cannot fill it would fail open or fail \
                 confusingly, and neither is acceptable on an authentication path"
            ),
        }
        rest = &after[close + 1..];
    }
    if !has_body {
        anyhow::bail!(
            "channel `{channel}`: binding `{binding}`'s signed template `{}` never interpolates \
             `{{body}}`, so one captured signature would verify every forged payload",
            hmac.signed
        );
    }

    if has_timestamp {
        // A timestamped scheme with no window is a signature that replays forever, which is worse
        // than not timestamping at all because it reads as though replay were handled.
        let tolerance = hmac.tolerance.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{channel}`: binding `{binding}`'s signed template interpolates \
                 `{{timestamp}}` with no `tolerance` — a replay window nobody states is a signature \
                 that replays forever"
            )
        })?;
        parse_tolerance(tolerance).ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{channel}`: binding `{binding}` declares `tolerance = {tolerance:?}`, \
                 which is not a duration — a window nobody can apply reads as though replay were \
                 handled just as convincingly as one that is"
            )
        })?;
        let timestamp = hmac.timestamp.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{channel}`: binding `{binding}`'s signed template interpolates \
                 `{{timestamp}}` but selects no timestamp — a host left to guess falls back to its \
                 own clock, which verifies nothing"
            )
        })?;
        // Body-sourced is spellable and incoherent: a timestamp read from the body has to be parsed
        // *before* the bytes carrying it are verified, which inverts the order that makes
        // verification mean anything and exposes a parser to any anonymous caller.
        if timestamp.source == "body" {
            anyhow::bail!(
                "channel `{channel}`: binding `{binding}` reads its signed timestamp from the \
                 body — that would parse the very bytes the signature is meant to authenticate, \
                 before they are authenticated"
            );
        }
        if timestamp.source != "header" {
            anyhow::bail!(
                "channel `{channel}`: binding `{binding}`'s timestamp reads from unknown source \
                 `{}`",
                timestamp.source
            );
        }
        if let Some(format) = &hmac.timestamp_format {
            if !matches!(format.as_str(), "unix_seconds" | "rfc3339") {
                anyhow::bail!(
                    "channel `{channel}`: binding `{binding}` declares unknown \
                     `timestamp_format = {format:?}`"
                );
            }
        }
    } else {
        // An unused selector or spelling describes a value nothing reads — the same ground the
        // producing repository refuses it on.
        if hmac.timestamp.is_some() || hmac.timestamp_format.is_some() {
            anyhow::bail!(
                "channel `{channel}`: binding `{binding}` selects a timestamp its signed template \
                 `{}` never interpolates",
                hmac.signed
            );
        }
    }
    Ok(())
}

/// The `5m` / `300s` duration grammar a `tolerance` is written in. `None` is "not a duration".
///
/// Shared with the `webhook` adapter, whose `verify { … }` record states the same window in the same
/// grammar (C-291) — an operator writing a tolerance and a connector publishing one must not be
/// parsing two dialects.
pub(crate) fn parse_tolerance(text: &str) -> Option<u64> {
    let (digits, scale) = match text.strip_suffix('s') {
        Some(d) => (d, 1),
        None => match text.strip_suffix('m') {
            Some(d) => (d, 60),
            None => match text.strip_suffix('h') {
                Some(d) => (d, 3600),
                None => (text, 1),
            },
        },
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(scale)
}

/// Every credential the binding **names** must be mapped by this deployment. Without the mapping a
/// signature check fails open, or the reply 401s on its first delivery — both of which are runtime
/// surprises for a defect that is visible at load.
fn require_credential(
    channel: &str,
    s: &ConnectorSettings,
    credential: &str,
) -> anyhow::Result<()> {
    if !s.credentials.contains_key(credential) {
        anyhow::bail!(
            "channel `{channel}`: binding `{}` names credential `{credential}`, which this \
             channel's `credentials` record does not map — add \
             `credentials {{ \"{credential}\": secret \"KEY\" }}`",
            s.binding
        );
    }
    Ok(())
}

/// A payload key becomes a symbol a journey reads, so it has to be spellable as one. Snake case
/// rather than flux's full name grammar: `$a-b` reads as a subtraction.
fn validate_symbol(channel: &str, binding: &str, symbol: &str) -> anyhow::Result<()> {
    let mut chars = symbol.chars();
    let first = chars.next().unwrap_or_default();
    if !first.is_ascii_lowercase()
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        anyhow::bail!(
            "channel `{channel}`: binding `{binding}` declares payload symbol `{symbol}`, which is \
             not a Flux symbol — lowercase ASCII letters, digits and `_`, starting with a letter"
        );
    }
    Ok(())
}

/// A header name this host can actually look up.
///
/// An unparseable one is not a cosmetic defect: `HeaderMap::get` would resolve it to nothing on every
/// delivery, so a signature header nobody can read fails *open* and a discriminator nobody can read
/// makes every delivery a no-op. Both read as working. A load error is the only honest answer.
fn validate_header(channel: &str, binding: &str, what: &str, header: &str) -> anyhow::Result<()> {
    if header.trim().is_empty() {
        anyhow::bail!("channel `{channel}`: binding `{binding}`'s {what} names no header");
    }
    if HeaderName::from_bytes(header.as_bytes()).is_err() {
        anyhow::bail!(
            "channel `{channel}`: binding `{binding}`'s {what} names {header:?}, which is not a \
             valid HTTP header name — nothing would ever resolve it"
        );
    }
    Ok(())
}

/// The dotted-path grammar, the same one the producing repository's loader enforces: dot-separated
/// segments, as in `event.thread_ts`. Deliberately not JSONPath.
fn validate_path(channel: &str, binding: &str, what: &str, path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        anyhow::bail!("channel `{channel}`: binding `{binding}`'s `{what}` path is empty");
    }
    if path.starts_with('.') || path.ends_with('.') || path.contains("..") {
        anyhow::bail!(
            "channel `{channel}`: binding `{binding}`'s `{what}` path {path:?} has an empty \
             segment; a source path reads as `event.thread_ts`, never with a leading, trailing or \
             doubled `.`"
        );
    }
    if let Some(bad) = path.chars().find(|c| c.is_whitespace()) {
        anyhow::bail!(
            "channel `{channel}`: binding `{binding}`'s `{what}` path {path:?} contains whitespace \
             ({bad:?})"
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The request path
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Whether a request may be delivered, given the channel's expected bearer token.
///
/// Two rules, and the second is the one that is easy to get wrong:
///
/// - **No expected token** → nothing to check. Only reachable on a loopback bind; a non-loopback one
///   without a token is refused at load.
/// - **An empty expected token authenticates nothing.** A request with no `Authorization` header
///   presents `""`, and a constant-time compare of two empty byte strings is `true` — so an empty
///   expected token would admit every anonymous caller while reading, everywhere it is printed or
///   logged, as "this channel is token-protected". `from_decl` already refuses one before a port is
///   bound; this is the same rule stated where the comparison happens, so a future path that reaches
///   the handler without that constructor cannot reopen the hole.
fn authorized(expected: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if expected.is_empty() {
        return false;
    }
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    constant_time_eq(presented.as_bytes(), expected.as_bytes())
}

/// A bounded, char-boundary-safe, escaped rendering of a **vendor-controlled** string for a log line.
///
/// Three things at once, all of them required because the value comes off the wire: bounded, so a
/// megabyte header cannot become a megabyte of log; clipped on a `char` boundary, never a byte
/// offset, so a multi-byte value cannot panic the formatter; and rendered through `{:?}`, so an
/// embedded newline or terminal escape cannot forge a second log line.
fn clip(value: &str) -> String {
    const MAX: usize = 64;
    let mut clipped: String = value.chars().take(MAX).collect();
    if clipped.chars().count() < value.chars().count() {
        clipped.push('…');
    }
    format!("{clipped:?}")
}

/// The line a dropped delivery logs. Rendered by a free function so the claim "a logged no-op" is
/// itself testable, without a test having to capture stderr.
fn drop_note(channel: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!(
            "connector channel `{channel}`: ignoring a delivery whose event {} is not one this \
             binding declares — no trigger fired",
            clip(value)
        ),
        None => format!(
            "connector channel `{channel}`: ignoring a delivery that carries no event \
             discriminator — no trigger fired"
        ),
    }
}

struct BindingState {
    name: String,
    deliverer: Arc<dyn Deliverer>,
    token: Option<String>,
    events: BTreeSet<String>,
    discriminator: Option<Selector>,
    delivery_id: Option<Selector>,
    payload: BTreeMap<String, String>,
}

impl BindingState {
    /// The bus label a delivery fires under, or the log line saying why there is none.
    ///
    /// `"<channel>.<event>"` when the discriminator resolves to a **declared** event, `"<channel>"`
    /// when the binding declares no discriminator. A value outside the closed event set is an `Err`
    /// — a *logged* no-op, never a label of its own and never a fallback to the bare channel name.
    /// Without that narrowing a vendor would get to name this host's trigger labels, and sanitising
    /// the characters does not stop that.
    ///
    /// The reason comes back as the rendered line rather than as a unit, so the drop is observable:
    /// a silently dropped delivery and a delivery nobody sent are the same thing from outside, and
    /// an operator debugging "my trigger never fires" has to be able to tell them apart.
    fn label(&self, headers: &HeaderMap, body: &Value) -> Result<String, String> {
        let Some(discriminator) = &self.discriminator else {
            return Ok(self.name.clone());
        };
        let Some(value) = discriminator.read(headers, body) else {
            return Err(drop_note(&self.name, None));
        };
        if self.events.contains(&value) {
            Ok(format!("{}.{value}", self.name))
        } else {
            Err(drop_note(&self.name, Some(&value)))
        }
    }

    /// The delivery payload: the binding's declared symbols, resolved against the vendor envelope.
    /// A path that does not resolve contributes no symbol rather than an empty string, so a journey
    /// can tell "absent" from "empty".
    fn payload(&self, headers: &HeaderMap, body: &Value) -> Value {
        let mut out = Map::new();
        for (symbol, path) in &self.payload {
            if let Some(value) = dotted(body, path) {
                if !value.is_null() {
                    out.insert(symbol.clone(), value.clone());
                }
            }
        }
        if let Some(selector) = &self.delivery_id {
            if let Some(id) = selector.read(headers, body) {
                out.insert(DELIVERY_ID_SYMBOL.to_string(), Value::String(id));
            }
        }
        Value::Object(out)
    }
}

async fn handle(
    State(state): State<Arc<BindingState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(state.token.as_deref(), &headers) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    let label = match state.label(&headers, &body) {
        Ok(label) => label,
        Err(note) => {
            // An event nobody declared, or one nobody subscribed to. Vendors send event types nobody
            // asked for, and a 500 teaches them to retry forever — so it is a 204. It is still
            // *logged*, because from the outside a dropped delivery and a delivery nobody sent look
            // identical, and that is the difference between debugging a binding and guessing at one.
            eprintln!("{note}");
            return StatusCode::NO_CONTENT.into_response();
        }
    };
    let payload = state.payload(&headers, &body);

    // Spawn rather than await: the reply is an operation call (D-217), never this HTTP response, so
    // holding the vendor's connection open buys nothing — and a channel adapter must not block its
    // own protocol loop on a delivery. Deliveries run concurrently and are bounded by the App's
    // admission limit, so this adds no queue of its own.
    let deliverer = state.deliverer.clone();
    tokio::spawn(async move {
        if let Err(e) = deliverer.deliver(&label, payload).await {
            eprintln!("connector channel `{label}`: delivery failed: {e}");
        }
    });
    StatusCode::ACCEPTED.into_response()
}

#[async_trait]
impl Channel for ConnectorChannel {
    fn name(&self) -> &str {
        &self.name
    }

    fn required_tool(&self) -> Option<&str> {
        self.reply_operation.as_deref()
    }

    async fn start(&self, d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(self.addr)
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: bind {}: {e}", self.name, self.addr))?;
        axum::serve(listener, self.router(d))
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: serve: {e}", self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An empty expected token authenticates nothing — least of all a request with no header.**
    ///
    /// The failure this pins is not a typo, it is an identity: the presented token is `""` when the
    /// `Authorization` header is absent, so a naive `constant_time_eq(b"", b"")` returns `true`,
    /// equal lengths and an empty loop. A channel that reads as token-protected everywhere it is
    /// printed would then admit every anonymous caller on a host that auto-approves tools.
    #[test]
    fn an_empty_expected_token_authenticates_nothing() {
        let mut bearer = HeaderMap::new();
        bearer.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer ".parse().expect("a header value"),
        );

        for headers in [&HeaderMap::new(), &bearer] {
            assert!(
                !authorized(Some(""), headers),
                "an empty expected token must never authorize"
            );
        }

        // The rules either side of it, so the fix cannot have been "refuse everything".
        assert!(
            authorized(None, &HeaderMap::new()),
            "no expected token is a loopback channel with nothing to check"
        );
        assert!(!authorized(Some("t0ken"), &HeaderMap::new()));
        let mut good = HeaderMap::new();
        good.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer t0ken".parse().expect("a header value"),
        );
        assert!(authorized(Some("t0ken"), &good));
    }

    /// The drop really is *logged*, and the line it logs is safe to log: a vendor controls the value,
    /// so it is bounded, clipped on a `char` boundary, and escaped so it cannot forge a second line.
    #[test]
    fn a_dropped_delivery_logs_a_bounded_escaped_note() {
        let note = drop_note("support", Some("not_an_event"));
        assert!(note.contains("support"), "{note}");
        assert!(note.contains("not_an_event"), "{note}");
        assert!(note.contains("no trigger fired"), "{note}");

        assert!(drop_note("support", None).contains("no event discriminator"));

        // Multi-byte, over-long, and carrying a newline: clipped on a char boundary, and the newline
        // escaped rather than emitted.
        let hostile = format!("{}\nfake log line", "é".repeat(500));
        let note = drop_note("support", Some(&hostile));
        assert!(
            note.len() < 300,
            "the note is bounded: {} bytes",
            note.len()
        );
        assert!(!note.contains('\n'), "no forged second line: {note}");
        assert!(note.contains('…'), "the clip is marked: {note}");
    }

    #[test]
    fn clip_never_splits_a_char_and_never_marks_a_short_value() {
        assert_eq!(clip("short"), "\"short\"");
        let long = "é".repeat(100);
        let clipped = clip(&long);
        assert!(clipped.ends_with("…\""), "{clipped}");
        assert!(clipped.chars().filter(|c| *c == 'é').count() == 64);
    }

    #[test]
    fn a_name_may_not_address_a_directory() {
        for bad in ["../../etc", "..", "a/b", "/etc/passwd", "~", "a.b", ""] {
            assert!(
                validate_name("c", "connector", bad).is_err(),
                "must refuse {bad:?}"
            );
        }
        for good in ["slack", "microsoft_graph", "acme-2"] {
            assert!(
                validate_name("c", "connector", good).is_ok(),
                "must admit {good:?}"
            );
        }
    }

    #[test]
    fn the_manifest_file_name_elides_the_default_service() {
        assert_eq!(manifest_file_name("slack", None), "slack.connector.toml");
        assert_eq!(
            manifest_file_name("microsoft_graph", Some("files")),
            "microsoft_graph-files.connector.toml"
        );
    }

    #[test]
    fn the_connectors_dir_is_the_flux_home_beside_flows() {
        let dir = connectors_dir().expect("HOME is set in a test environment");
        assert!(
            dir.ends_with(".flux/connectors"),
            "the connector home sits beside ~/.flux/flows: {}",
            dir.display()
        );
    }

    #[test]
    fn tolerance_is_a_duration_or_it_is_nothing() {
        assert_eq!(parse_tolerance("5m"), Some(300));
        assert_eq!(parse_tolerance("300s"), Some(300));
        assert_eq!(parse_tolerance("1h"), Some(3600));
        assert_eq!(parse_tolerance("300"), Some(300));
        assert_eq!(parse_tolerance("banana"), None);
        assert_eq!(parse_tolerance("m"), None);
        assert_eq!(parse_tolerance(""), None);
    }

    #[test]
    fn a_dotted_path_addresses_nested_json() {
        let body = serde_json::json!({ "event": { "type": "app_mention", "n": 1 } });
        assert_eq!(dotted(&body, "event.type").unwrap(), "app_mention");
        assert_eq!(dotted(&body, "event.n").unwrap(), 1);
        assert!(dotted(&body, "event.missing").is_none());
        assert!(dotted(&body, "nope.type").is_none());
    }
}
