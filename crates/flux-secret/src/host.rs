//! Host references — the named, first-class bindings to execution substrates (Decision 0018).
//!
//! A host names *which substrate* effects land on: its backend kind, its address where one exists,
//! and its credential as a [`Ref`](crate::Ref) (a location), never a value. The reference form here
//! is model-safe by construction — a [`HostRef`] can be listed, inspected and granted without ever
//! holding credential material. Resolution to a live `ExecutionSystem` lives in the runtime, not
//! here, exactly as endpoint resolution does.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Ref;

/// The closed vocabulary of execution-substrate backends a host may bind (Decision 0018 rule 3).
/// Typed — never a free string — so an unknown kind is a hard parse error wherever it appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostBackend {
    /// The native local `System` (the default substrate).
    Local,
    /// OS-level confinement as a peer backend (C-651); until wired, selection fails closed.
    Sandboxed,
    /// The container process backend (C-397).
    Container,
    /// A Kubernetes-served substrate composing the remote protocol (C-655 context).
    Kubernetes,
    /// An ssh-bootstrapped substrate composing the remote protocol (C-683). ssh is the bootstrap,
    /// never the substrate: it starts or verifies `flux system serve` on the far machine and
    /// forwards its endpoint; every effect still rides the delivered protocol.
    Ssh,
    /// The delivered remote-system protocol (`flux system serve`).
    Remote,
}

impl HostBackend {
    /// Every backend kind, in display order.
    pub const ALL: [Self; 6] = [
        Self::Local,
        Self::Sandboxed,
        Self::Container,
        Self::Kubernetes,
        Self::Ssh,
        Self::Remote,
    ];

    /// The lowercase wire/display form (matches the serde encoding).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sandboxed => "sandboxed",
            Self::Container => "container",
            Self::Kubernetes => "kubernetes",
            Self::Ssh => "ssh",
            Self::Remote => "remote",
        }
    }
}

impl fmt::Display for HostBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for HostBackend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| {
                let known: Vec<&str> = Self::ALL.into_iter().map(Self::as_str).collect();
                format!(
                    "unknown host backend `{s}`; known backends: {}",
                    known.join(", ")
                )
            })
    }
}

/// A surface class a host binding may be granted to (Decision 0018 rule 4). Host authority is
/// granted, never ambient: a binding carries the classes that may select it, the default is deny,
/// and the classes are exact — an unattended surface never inherits an `operator` grant, so a
/// serving surface cannot widen a grant silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostGrant {
    /// An attended, operator-driven surface (the interactive CLI/TUI).
    Operator,
    /// An unattended or serving surface (`--yes` runs, `app run --serve`, daemons).
    Unattended,
}

impl HostGrant {
    /// Every grant class, in display order.
    pub const ALL: [Self; 2] = [Self::Operator, Self::Unattended];

    /// The lowercase wire/display form (matches the serde encoding).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Unattended => "unattended",
        }
    }
}

impl fmt::Display for HostGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for HostGrant {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|g| g.as_str() == s)
            .ok_or_else(|| {
                let known: Vec<&str> = Self::ALL.into_iter().map(Self::as_str).collect();
                format!(
                    "unknown host grant `{s}`; known surface classes: {}",
                    known.join(", ")
                )
            })
    }
}

/// Where a host binding came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HostSource {
    /// Declared in configuration (`[[host]]`) or the persisted hosts store.
    #[default]
    Config,
    /// Constructed for this session only (the `--remote <url>` sugar); never persisted.
    Ephemeral,
}

/// A weak host reference: model-safe by construction — it names a substrate binding and carries no
/// secret, only a `credential_ref` pointing at where the credential lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRef {
    /// Stable binding name (a bare name, e.g. `build-farm`).
    pub id: String,
    /// Which substrate backend this binding selects.
    pub backend: HostBackend,
    /// `scheme://host[:port]` for backends that have an address (`remote`, `kubernetes`) — never
    /// with embedded credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether this binding was declared in config or constructed ephemerally for the session.
    #[serde(default)]
    pub source: HostSource,
    /// Where the credential lives — a *reference*, never a value. `None` for unauthenticated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<Ref>,
    /// The surface classes granted to select this binding (Decision 0018 rule 4). Empty means
    /// deny: the binding is listable and probeable but selects for nobody.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grant: Vec<HostGrant>,
    /// Free-form non-secret labels (region, cluster, tags) for display/filtering.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// The far-side bootstrap contract for an `ssh` binding (C-683). `None` for every other
    /// backend, and for an `ssh` binding that takes the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<HostSsh>,
}

/// What an `ssh` binding declares about the far machine (C-683).
///
/// Every field is a *declaration about the far side*, not a secret: paths, a port and a name. The
/// two credentials an ssh binding needs stay references — the private key is the binding's own
/// `credential_ref`, and the serving endpoint's bearer token is [`token_ref`](Self::token_ref).
/// Installing the flux binary on that machine remains the operator's step (the C-480 boundary);
/// what this declares is where to find it and how to reach what it serves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSsh {
    /// The far-side flux binary. Absent means `flux`, resolved on the far side's `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// The far-side loopback port `flux system serve` binds and the tunnel forwards to. Absent
    /// means the delivered default, 8790.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serve_port: Option<u16>,
    /// The far-side workspace root a started serve is given. Absent leaves it to the far side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The far-side TLS certificate and key `flux system serve` is started with. Both absent means
    /// this binding may only *attach* to an already-serving far side, never start one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    /// The far-side TLS key; see [`cert`](Self::cert).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// A **local** PEM whose roots the client trusts for this binding — the delivered `--remote-ca`
    /// pinning form, not a bypass. Absent uses the platform roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
    /// A **local** `known_hosts` file scoping strict host-key verification to this binding. Absent
    /// uses ssh's own default. Verification is strict either way; this only says which record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_hosts: Option<String>,
    /// The name the far side's certificate carries, used as the tunnelled endpoint's host. Absent
    /// means `127.0.0.1` — the address the forward actually lands on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    /// Where the serving endpoint's bearer token lives. Absent means `env/FLUX_REMOTE_SYSTEM_TOKEN`,
    /// the delivered default on both seats. A location, never a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_ref: Option<Ref>,
}

impl HostRef {
    /// A named, config-declared binding.
    pub fn declared(id: impl Into<String>, backend: HostBackend) -> Self {
        Self {
            id: id.into(),
            backend,
            url: None,
            source: HostSource::Config,
            credential_ref: None,
            grant: Vec::new(),
            labels: BTreeMap::new(),
            ssh: None,
        }
    }

    /// A session-only binding (the anonymous `--remote <url>` sugar records as one of these).
    pub fn ephemeral(id: impl Into<String>, backend: HostBackend) -> Self {
        Self {
            source: HostSource::Ephemeral,
            ..Self::declared(id, backend)
        }
    }

    /// The display address: the bound URL where one exists, `-` otherwise. Never credential-bearing
    /// — construction paths refuse a userinfo URL before a `HostRef` exists.
    pub fn display_address(&self) -> &str {
        self.url.as_deref().unwrap_or("-")
    }
}

/// A stored host binding. Persisted forms never contain credential material — only the
/// `credential_ref` location, re-resolved live each session (the same weak-ref rule as
/// endpoint records).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRecord {
    #[serde(flatten)]
    pub host: HostRef,
    /// Who declared this binding: `"config"` for config-declared, `"session"` for ephemeral.
    pub owner: String,
}

impl HostRecord {
    /// A config-declared record.
    pub fn config(host: HostRef) -> Self {
        Self {
            host,
            owner: "config".to_string(),
        }
    }

    /// A session-only record (never persisted by any production path).
    pub fn session(host: HostRef) -> Self {
        Self {
            host,
            owner: "session".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_ref_round_trips_and_carries_no_secret() {
        let r = HostRef {
            url: Some("https://build-farm.internal:8443".into()),
            credential_ref: Some(Ref::env("BUILD_FARM_TOKEN")),
            ..HostRef::declared("build-farm", HostBackend::Remote)
        };
        let json = serde_json::to_string(&r).unwrap();
        // The credential is only a *reference* (a location), never a value.
        assert!(
            json.contains("BUILD_FARM_TOKEN") && json.contains("\"env\""),
            "location, not value: {json}"
        );
        let back: HostRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn record_toml_round_trips() {
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            host: Vec<HostRecord>,
        }
        let rec = HostRecord::config(HostRef {
            url: Some("https://farm.example:8443".into()),
            credential_ref: Some(Ref::kubernetes("infra", "farm-creds", "token")),
            ..HostRef::declared("farm", HostBackend::Kubernetes)
        });
        let body = toml::to_string(&Wrap {
            host: vec![rec.clone()],
        })
        .unwrap();
        let back: Wrap = toml::from_str(&body).unwrap();
        assert_eq!(back.host, vec![rec]);
    }

    #[test]
    fn unknown_backend_kind_is_a_hard_parse_error() {
        // The backend vocabulary is closed: deserializing an unknown kind fails outright rather
        // than defaulting or skipping (C-648's hard-config-error contract rides this).
        let err = serde_json::from_str::<HostBackend>("\"warp\"").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("warp") || msg.contains("unknown variant"),
            "{msg}"
        );
        let parse_err = "warp".parse::<HostBackend>().unwrap_err();
        assert!(
            parse_err.contains("local") && parse_err.contains("remote"),
            "names the known kinds: {parse_err}"
        );
    }

    #[test]
    fn ssh_joins_the_closed_vocabulary_carrying_only_references() {
        // C-683: the kind parses, round-trips and is listed among the known kinds a refusal names.
        assert_eq!("ssh".parse::<HostBackend>().unwrap(), HostBackend::Ssh);
        assert!(HostBackend::ALL.contains(&HostBackend::Ssh));
        assert!(
            "warp".parse::<HostBackend>().unwrap_err().contains("ssh"),
            "an unknown kind's refusal lists ssh"
        );

        let binding = HostRef {
            url: Some("ssh://build@devbox.internal:2222".into()),
            // The *key* is a reference: what resolves is the path openssh opens, and flux never
            // reads the material itself.
            credential_ref: Some(Ref::env("FLUX_DEVBOX_KEY")),
            ssh: Some(HostSsh {
                binary: Some("/usr/local/bin/flux".into()),
                serve_port: Some(8790),
                token_ref: Some(Ref::env("FLUX_REMOTE_SYSTEM_TOKEN")),
                ..HostSsh::default()
            }),
            ..HostRef::declared("devbox", HostBackend::Ssh)
        };
        let json = serde_json::to_string(&binding).unwrap();
        let back: HostRef = serde_json::from_str(&json).unwrap();
        assert_eq!(binding, back);
        assert_eq!(back.display_address(), "ssh://build@devbox.internal:2222");
        // Both credentials are locations. There is no value anywhere in the persisted form.
        assert!(json.contains("FLUX_DEVBOX_KEY") && json.contains("FLUX_REMOTE_SYSTEM_TOKEN"));
    }

    #[test]
    fn ephemeral_refs_are_marked_and_default_source_is_config() {
        let eph = HostRef::ephemeral("remote-cli", HostBackend::Remote);
        assert_eq!(eph.source, HostSource::Ephemeral);
        // A serialized form without `source` deserializes as config-declared (store compat).
        let bare: HostRef = serde_json::from_str(r#"{"id":"h","backend":"local"}"#).unwrap();
        assert_eq!(bare.source, HostSource::Config);
    }
}
