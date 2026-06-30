//! Endpoint references — the host-managed weak references a plugin operation deals in.
//!
//! A plugin op never holds a URL with credentials or a secret value. It holds an [`EndpointRef`]:
//! *where + how to connect*, with the credential as a [`Ref`](crate::Ref) (a location), never a
//! value. Discovery yields [`EndpointCandidate`]s; the host stores them as [`EndpointRecord`]s. Only
//! the host turns a reference into a [`ResolvedEndpoint`] (absolute URL + injected auth) at the
//! moment of an IO call — that runtime form has **no `Serialize`** and a non-leaking `Debug`, so it
//! can never reach the model or the logs. Resolution itself lives in the runtime, not here.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Ref;

/// Canonical prefix for a discovered endpoint id (`@endpoint/<id>`).
pub const ENDPOINT_REF_PREFIX: &str = "@endpoint/";

/// Where an endpoint reference came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A named endpoint resolved from host config (or a plugin manifest default binding).
    Config,
    /// Discovered at runtime by a provider plugin (the provider is the record's `owner`).
    Discovered,
}

/// A weak endpoint reference: model-safe by construction — it carries no secret, only a
/// `credential_ref` pointing at where the secret lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRef {
    /// Stable id. Discovered endpoints use the `@endpoint/<id>` form; named ones use the bare name.
    pub id: String,
    /// `scheme://host[:port][/base]` — never with embedded credentials.
    pub url: String,
    /// Product class this endpoint serves (`postgres`, `prometheus`, …); empty if generic.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub product: String,
    /// Wire protocol hint (`http`, `postgres`, `ami`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Whether this came from config or discovery.
    pub source: SourceKind,
    /// Where the credential lives — a *reference*, never a value. `None` for unauthenticated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<Ref>,
    /// Free-form non-secret labels (region, namespace, tags) for display/filtering.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl EndpointRef {
    /// A discovered endpoint with an `@endpoint/<id>` id.
    pub fn discovered(
        id: impl AsRef<str>,
        url: impl Into<String>,
        product: impl Into<String>,
    ) -> Self {
        let id = id.as_ref();
        let id = if id.starts_with(ENDPOINT_REF_PREFIX) {
            id.to_string()
        } else {
            format!("{ENDPOINT_REF_PREFIX}{id}")
        };
        Self {
            id,
            url: url.into(),
            product: product.into(),
            protocol: None,
            source: SourceKind::Discovered,
            credential_ref: None,
            labels: BTreeMap::new(),
        }
    }

    /// A named, config-bound endpoint (e.g. `"sql.endpoint"`).
    pub fn named(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: name.into(),
            url: url.into(),
            product: String::new(),
            protocol: None,
            source: SourceKind::Config,
            credential_ref: None,
            labels: BTreeMap::new(),
        }
    }

    /// True if `id` looks like a discovered endpoint reference (`@endpoint/…`).
    pub fn is_discovered_ref(id: &str) -> bool {
        id.starts_with(ENDPOINT_REF_PREFIX)
    }
}

/// A discovery candidate: a weak [`EndpointRef`] plus ranking/explanation metadata. Still
/// model-safe — carries no secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointCandidate {
    #[serde(flatten)]
    pub endpoint: EndpointRef,
    /// Higher = better match for the discovery query.
    #[serde(default)]
    pub score: f64,
    /// Human-readable reasons this candidate matched (for audit/explanation).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// A stored endpoint with lifecycle metadata. Persisted to `~/.flux/endpoints.toml` on import — and
/// because an [`EndpointRef`] carries no secret, the persisted form never contains credential
/// material (only the `credential_ref` location, re-resolved live each session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRecord {
    #[serde(flatten)]
    pub endpoint: EndpointRef,
    /// The plugin that discovered this endpoint, or `"config"` for a config-bound one.
    pub owner: String,
    /// Time-to-live in seconds (discovery freshness); `None` = no expiry (e.g. config-bound).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    /// Unix seconds when discovered/last-refreshed (stamped by the registry, not this pure type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovered_at_secs: Option<u64>,
    /// Last health probe result, if any (display only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
}

impl EndpointRecord {
    /// A config-bound record (owner = `"config"`, no expiry).
    pub fn config(endpoint: EndpointRef) -> Self {
        Self {
            endpoint,
            owner: "config".to_string(),
            ttl_secs: None,
            discovered_at_secs: None,
            health: None,
        }
    }
}

/// The runtime-ready resolution of an endpoint reference: an absolute URL plus any injected auth
/// headers (HTTP). **Host-only.** It deliberately has **no `Serialize`** and a non-leaking `Debug`,
/// so it cannot be projected into model-visible output or logs (mirroring [`Material`](crate::Material)).
#[derive(Clone)]
pub struct ResolvedEndpoint {
    /// The reference this resolved from (the id/name), for audit.
    pub reference: String,
    /// The absolute URL to use — may contain no credentials (those are in `injected_headers`).
    pub url: String,
    /// Header name/value pairs to inject for HTTP. **Values may be secret — host-only.**
    pub injected_headers: Vec<(String, String)>,
}

impl ResolvedEndpoint {
    pub fn new(reference: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            url: url.into(),
            injected_headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.injected_headers.push((name.into(), value.into()));
        self
    }
}

impl fmt::Debug for ResolvedEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print header *values* — they may be credentials.
        let header_names: Vec<&str> = self
            .injected_headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        f.debug_struct("ResolvedEndpoint")
            .field("reference", &self.reference)
            .field("url", &self.url)
            .field("injected_headers", &header_names)
            .field("header_values", &"[redacted]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_ref_round_trips_and_carries_no_secret() {
        let r = EndpointRef {
            credential_ref: Some(Ref::kubernetes("monitoring", "pg-creds", "password")),
            protocol: Some("postgres".into()),
            ..EndpointRef::discovered(
                "pg-prod-1",
                "postgres://pg.monitoring.svc:5432/app",
                "postgres",
            )
        };
        assert!(r.id.starts_with(ENDPOINT_REF_PREFIX));
        let json = serde_json::to_string(&r).unwrap();
        // The credential is only a *reference* (a location), never a value.
        assert!(
            json.contains("kubernetes/monitoring/pg-creds/password")
                || json.contains("\"slot\":\"password\"")
        );
        let back: EndpointRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn record_toml_round_trips() {
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            endpoint: Vec<EndpointRecord>,
        }
        let rec = EndpointRecord {
            owner: "kubernetes".into(),
            ttl_secs: Some(900),
            discovered_at_secs: Some(1_700_000_000),
            ..EndpointRecord::config(EndpointRef::discovered(
                "pg-1",
                "postgres://db:5432/app",
                "postgres",
            ))
        };
        let body = toml::to_string(&Wrap {
            endpoint: vec![rec.clone()],
        })
        .unwrap();
        let back: Wrap = toml::from_str(&body).unwrap();
        assert_eq!(back.endpoint, vec![rec]);
    }

    #[test]
    fn resolved_endpoint_debug_does_not_leak_header_values() {
        // `ResolvedEndpoint` has no `Serialize` (compile-time guarantee it can't reach the model);
        // its `Debug` must also never print a header value.
        let resolved = ResolvedEndpoint::new("@endpoint/pg-1", "https://api.internal/v1")
            .with_header("Authorization", "Bearer sk-super-secret-token");
        let dbg = format!("{resolved:?}");
        assert!(!dbg.contains("sk-super-secret-token"), "leaked: {dbg}");
        assert!(dbg.contains("Authorization")); // header *name* is fine
        assert!(dbg.contains("[redacted]"));
    }
}
