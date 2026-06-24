//! `flux-secret` — secret addressing, material, sensitivity, and redaction (pure, no IO).
//!
//! Secrets are referred to by an addressable [`Ref`] (`env/KEY`, `plugin/slack/main/bot_token`,
//! `kubernetes/ns/name/key`) — never by raw value in logs or prompts. [`Material`] holds the
//! resolved value behind a non-leaking `Debug`. The [`Redactor`] scrubs known secret values and
//! common credential shapes from any captured text before it is logged or shown to a model.
//! Resolution (env/store lookups) lives in the runtime, not here.

use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Reference
// ---------------------------------------------------------------------------

/// The addressing scheme of a secret reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    Env,
    Plugin,
    Kubernetes,
}

/// An addressable secret reference. `env/KEY` uses only `slot`; `plugin`/`kubernetes` use all
/// three of `plugin`/`instance`/`slot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    pub scheme: Scheme,
    #[serde(default)]
    pub plugin: String,
    #[serde(default)]
    pub instance: String,
    pub slot: String,
}

impl Ref {
    pub fn env(key: impl Into<String>) -> Self {
        Self {
            scheme: Scheme::Env,
            plugin: String::new(),
            instance: String::new(),
            slot: key.into(),
        }
    }

    pub fn plugin(
        plugin: impl Into<String>,
        instance: impl Into<String>,
        slot: impl Into<String>,
    ) -> Self {
        Self {
            scheme: Scheme::Plugin,
            plugin: plugin.into(),
            instance: instance.into(),
            slot: slot.into(),
        }
    }

    pub fn kubernetes(
        namespace: impl Into<String>,
        name: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            scheme: Scheme::Kubernetes,
            plugin: namespace.into(),
            instance: name.into(),
            slot: key.into(),
        }
    }

    /// Parse a `scheme/...` reference string.
    pub fn parse(s: &str) -> Result<Ref, String> {
        let parts: Vec<&str> = s.split('/').collect();
        match parts.first().copied() {
            Some("env") if parts.len() == 2 => Ok(Ref::env(parts[1])),
            Some("plugin") if parts.len() == 4 => Ok(Ref::plugin(parts[1], parts[2], parts[3])),
            Some("kubernetes") if parts.len() == 4 => {
                Ok(Ref::kubernetes(parts[1], parts[2], parts[3]))
            }
            _ => Err(format!("invalid secret ref: {s:?}")),
        }
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.scheme {
            Scheme::Env => write!(f, "env/{}", self.slot),
            Scheme::Plugin => write!(f, "plugin/{}/{}/{}", self.plugin, self.instance, self.slot),
            Scheme::Kubernetes => {
                write!(
                    f,
                    "kubernetes/{}/{}/{}",
                    self.plugin, self.instance, self.slot
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Material / sensitivity
// ---------------------------------------------------------------------------

/// The kind of credential a secret holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    ApiKey,
    BearerToken,
    Oauth2Token,
    Basic,
    Pki,
}

/// How sensitive a value is, gating where it may be exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Public,
    Internal,
    Restricted,
    Confidential,
    Secret,
}

/// Resolved secret material. `Debug` never prints the value.
#[derive(Clone)]
pub struct Material {
    pub reference: Ref,
    pub kind: Kind,
    pub value: String,
    pub media_type: Option<String>,
}

impl fmt::Debug for Material {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Material")
            .field("reference", &self.reference)
            .field("kind", &self.kind)
            .field("value", &"[redacted]")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

const REDACTED: &str = "[redacted]";

/// Credential-looking prefixes that are redacted even when the exact value isn't registered.
const SECRET_PREFIXES: &[&str] = &[
    "sk-ant-",
    "sk-",
    "xoxb-",
    "xoxp-",
    "xoxe-",
    "ghp_",
    "gho_",
    "github_pat_",
    "AKIA",
    "AIza",
    "ya29.",
    "eyJ", // JWT-ish
];

/// Scrubs registered secret values and common credential shapes from text before it is logged
/// or shown to the model.
#[derive(Default, Clone)]
pub struct Redactor {
    values: Vec<String>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a known secret value (no-op for trivially short values to avoid over-redaction).
    pub fn add_secret(&mut self, value: impl Into<String>) {
        let v = value.into();
        if v.trim().len() >= 6 {
            self.values.push(v);
        }
    }

    /// Redact registered values (exact substring) and credential-shaped tokens from `input`.
    pub fn redact(&self, input: &str) -> String {
        let mut out = input.to_string();
        // Longest-first so a value that contains another is replaced whole.
        let mut vals = self.values.clone();
        vals.sort_by_key(|v| std::cmp::Reverse(v.len()));
        for v in vals {
            if !v.is_empty() {
                out = out.replace(&v, REDACTED);
            }
        }
        redact_patterns(&out)
    }
}

/// Redact whitespace/quote-delimited tokens that look like credentials.
fn redact_patterns(input: &str) -> String {
    input
        .split_inclusive(|c: char| c.is_whitespace())
        .map(|chunk| {
            // Separate the trailing whitespace so it is preserved.
            let trimmed = chunk.trim_end();
            let ws = &chunk[trimmed.len()..];
            // Strip surrounding quotes/brackets for the prefix check.
            let core =
                trimmed.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '(' | ')' | ',' | ';'));
            if core.len() >= 8 && SECRET_PREFIXES.iter().any(|p| core.starts_with(p)) {
                format!("{REDACTED}{ws}")
            } else {
                chunk.to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_roundtrips() {
        for s in [
            "env/MY_KEY",
            "plugin/slack/main/bot_token",
            "kubernetes/ns/name/key",
        ] {
            let r = Ref::parse(s).unwrap();
            assert_eq!(r.to_string(), s);
        }
        assert!(Ref::parse("bogus").is_err());
        assert!(Ref::parse("env/A/B").is_err());
    }

    #[test]
    fn redacts_registered_values() {
        let mut r = Redactor::new();
        r.add_secret("supersecretvalue");
        assert_eq!(
            r.redact("token=supersecretvalue here"),
            "token=[redacted] here"
        );
        // too short → not registered
        r.add_secret("ab");
        assert_eq!(r.redact("x ab y"), "x ab y");
    }

    #[test]
    fn redacts_credential_shapes() {
        let r = Redactor::new();
        let out = r.redact("using key sk-ant-abc123def456 and ghp_0123456789abcdef now");
        assert!(!out.contains("sk-ant-abc123def456"));
        assert!(!out.contains("ghp_0123456789abcdef"));
        assert!(out.contains("[redacted]"));
        assert!(out.contains("using key"));
        assert!(out.contains("now"));
    }

    #[test]
    fn material_debug_does_not_leak() {
        let m = Material {
            reference: Ref::env("K"),
            kind: Kind::ApiKey,
            value: "supersecret".into(),
            media_type: None,
        };
        let dbg = format!("{m:?}");
        assert!(!dbg.contains("supersecret"));
        assert!(dbg.contains("[redacted]"));
    }
}
