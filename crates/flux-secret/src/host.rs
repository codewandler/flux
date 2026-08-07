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
    /// A VM/microVM guest serving the remote protocol (C-677). Composed, not invented: the guest
    /// runs the delivered `flux system serve` and the binding consumes the endpoint it already
    /// serves. Flux never provisions the guest — that is a deployment concern (C-480's profile)
    /// and, for a lifecycle verb, a future generic isolation-provisioner contract.
    Microvm,
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
        Self::Microvm,
        Self::Remote,
    ];

    /// The lowercase wire/display form (matches the serde encoding).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sandboxed => "sandboxed",
            Self::Container => "container",
            Self::Kubernetes => "kubernetes",
            Self::Microvm => "microvm",
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
    /// `scheme://host[:port]` for backends that have an address (`remote`, `kubernetes`,
    /// `microvm`) — never with embedded credentials.
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

    /// C-677: `microvm` joins the closed vocabulary as a *word*, not a wire. Decision 0018 rule 3
    /// composes rather than invents — the guest runs the delivered remote protocol — so what is
    /// added here is a backend kind an operator can declare, list, grant and probe, pointing at a
    /// served endpoint that something else (C-480's guest profile) brought into existence.
    #[test]
    fn microvm_is_a_declarable_backend_kind() {
        let backend: HostBackend = "microvm"
            .parse()
            .expect("`microvm` is a declarable host backend kind");
        assert_eq!(backend.as_str(), "microvm");
        assert!(
            HostBackend::ALL.contains(&backend),
            "a kind absent from ALL is unlistable and unparseable: {:?}",
            HostBackend::ALL
        );
        // Serde and `FromStr` are one vocabulary, or a `[[host]]` table and a `--backend` flag
        // would disagree about what exists.
        assert_eq!(
            serde_json::from_str::<HostBackend>("\"microvm\"").unwrap(),
            backend
        );
        assert_eq!(serde_json::to_string(&backend).unwrap(), "\"microvm\"");
        // Still closed: an unknown kind stays a hard error, and it now names `microvm` among the
        // known ones. A *hypervisor* is not a backend kind — flux never provisions one.
        let err = "firecracker".parse::<HostBackend>().unwrap_err();
        assert!(
            err.contains("microvm") && err.contains("firecracker"),
            "the refusal must list the real vocabulary: {err}"
        );

        // A microvm binding is address-bearing and credential-referencing like any remote-shaped
        // one, and its persisted form still carries the credential *location* only.
        let reference = HostRef {
            url: Some("https://guest.internal:8443".into()),
            credential_ref: Some(Ref::env("GUEST_TOKEN")),
            ..HostRef::declared("vm-guest", backend)
        };
        let json = serde_json::to_string(&reference).unwrap();
        assert!(json.contains("\"microvm\"") && json.contains("GUEST_TOKEN"), "{json}");
        assert_eq!(
            serde_json::from_str::<HostRef>(&json).unwrap(),
            reference,
            "the binding round-trips"
        );
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
