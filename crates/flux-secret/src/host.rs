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
    /// The delivered remote-system protocol (`flux system serve`).
    Remote,
}

impl HostBackend {
    /// Every backend kind, in display order.
    pub const ALL: [Self; 5] = [
        Self::Local,
        Self::Sandboxed,
        Self::Container,
        Self::Kubernetes,
        Self::Remote,
    ];

    /// The lowercase wire/display form (matches the serde encoding).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sandboxed => "sandboxed",
            Self::Container => "container",
            Self::Kubernetes => "kubernetes",
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
            credential_ref: Some(Ref::env("FLUX_BUILD_FARM_TOKEN")),
            ..HostRef::declared("build-farm", HostBackend::Remote)
        };
        let json = serde_json::to_string(&r).unwrap();
        // The credential is only a *reference* (a location), never a value.
        assert!(
            json.contains("FLUX_BUILD_FARM_TOKEN") && json.contains("\"env\""),
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
    fn ephemeral_refs_are_marked_and_default_source_is_config() {
        let eph = HostRef::ephemeral("remote-cli", HostBackend::Remote);
        assert_eq!(eph.source, HostSource::Ephemeral);
        // A serialized form without `source` deserializes as config-declared (store compat).
        let bare: HostRef = serde_json::from_str(r#"{"id":"h","backend":"local"}"#).unwrap();
        assert_eq!(bare.source, HostSource::Config);
    }
}
