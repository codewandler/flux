//! `flux-secret` — secret addressing, material, sensitivity, and redaction (pure, no IO).
//!
//! Secrets are referred to by an addressable [`Ref`] (`env/KEY`, `plugin/slack/main/bot_token`,
//! `kubernetes/ns/name/key`) — never by raw value in logs or prompts. [`Material`] holds the
//! resolved value behind a non-leaking `Debug`. The [`Redactor`] scrubs known secret values and
//! common credential shapes from any captured text before it is logged or shown to a model.
//! Resolution (env/store lookups) lives in the runtime, not here.

use std::fmt;

use serde::{Deserialize, Serialize};

pub mod endpoint;

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
///
/// The registered-value set lives behind a shared `Arc<Mutex<…>>`, so **cloning a `Redactor` shares
/// its value store**: a secret registered through one handle (e.g. a runtime-materialized credential
/// the host injects on the `credential` capability path) is immediately redacted by every clone,
/// including the one the executor uses to scrub tool output. Registration is therefore `&self`.
#[derive(Default, Clone)]
pub struct Redactor {
    values: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a known secret value (no-op for trivially short values to avoid over-redaction).
    /// The value is stored **trimmed** — env/file-sourced secrets often carry a trailing newline,
    /// and storing the raw value would mean the bare token never matches in tool output. Takes
    /// `&self` (interior-mutable, shared store) so a credential materialized mid-run is registered
    /// even when only a clone of the redactor is in hand.
    pub fn add_secret(&self, value: impl Into<String>) {
        let v = value.into();
        let trimmed = v.trim();
        if trimmed.len() >= 6 {
            self.values.lock().unwrap().push(trimmed.to_string());
        }
    }

    /// Redact registered values (exact substring) and credential-shaped tokens from `input`.
    pub fn redact(&self, input: &str) -> String {
        let mut out = input.to_string();
        // Longest-first so a value that contains another is replaced whole.
        let mut vals = self.values.lock().unwrap().clone();
        vals.sort_by_key(|v| std::cmp::Reverse(v.len()));
        for v in vals {
            if !v.is_empty() {
                out = out.replace(&v, REDACTED);
            }
        }
        redact_patterns(&out)
    }
}

/// Line markers that may be glued to the FRONT of a token on a rendered line: the `+`/`-` of a
/// unified diff, a `*` bullet, a `#` comment or heading. They are stripped before the prefix match
/// and re-emitted verbatim (C-185).
///
/// They deliberately are **not** boundary characters. `-` occurs *inside* nearly every credential
/// shape we match (`sk-ant-…`, `xoxb-…`), so splitting on it would break a key into fragments that
/// no longer start with a prefix — the token would render in the clear, which is the opposite of
/// the fix. Leading-only stripping catches `+sk-ant-…` without widening what counts as a token.
const LINE_MARKERS: &[char] = &['+', '-', '*', '#'];

/// Redact credential-shaped tokens. A token is a maximal run of non-boundary characters; any run
/// that begins with a known secret prefix — after any leading [`LINE_MARKERS`] are set aside — is
/// replaced. Boundaries include whitespace AND common delimiters (`= : " ' ` ( ) [ ] { } , ;`), so
/// punctuation-glued forms like `api_key=sk-ant-…` and `"sk-ant-…"` are caught, not just
/// whitespace-separated tokens.
fn redact_patterns(input: &str) -> String {
    fn is_boundary(c: char) -> bool {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\''
                    | '`'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | ','
                    | ';'
                    | '='
                    | ':'
                    | '<'
                    | '>'
            )
    }
    fn flush(token: &mut String, out: &mut String) {
        // `body` is the token minus any leading diff/list marker; the markers are ASCII, so the
        // byte split is always on a char boundary.
        let body = token.trim_start_matches(LINE_MARKERS);
        if body.len() >= 8 && SECRET_PREFIXES.iter().any(|p| body.starts_with(p)) {
            out.push_str(&token[..token.len() - body.len()]);
            out.push_str(REDACTED);
        } else {
            out.push_str(token);
        }
        token.clear();
    }

    let mut out = String::with_capacity(input.len());
    let mut token = String::new();
    for c in input.chars() {
        if is_boundary(c) {
            flush(&mut token, &mut out);
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
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
        let r = Redactor::new();
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
    fn redacts_glued_and_trimmed_secrets() {
        let r = Redactor::new();
        // A file-sourced value with a trailing newline must still redact the bare token in output.
        r.add_secret("topsecretvalue\n");
        assert_eq!(
            r.redact("the value is topsecretvalue!"),
            "the value is [redacted]!"
        );
        // A punctuation-glued, unregistered credential shape is still caught.
        let out = r.redact("api_key=sk-ant-abc123def456;next");
        assert!(!out.contains("sk-ant-abc123def456"), "leaked: {out}");
        assert!(out.contains("api_key="));
        assert!(out.contains("next"));
    }

    /// C-185 failing-first: a unified-diff or list marker glued to the front of a credential used
    /// to hide it — `+`/`-`/`*`/`#` are not boundary characters, so `+sk-ant-…` tokenized with the
    /// marker attached and never matched a prefix. Every surface that renders a diff (the approval
    /// sheet's hunk preview, tool-card detail, the HTML export) reads through this one function.
    #[test]
    fn a_line_marker_does_not_hide_a_credential() {
        let r = Redactor::new();
        for marker in ["+", "-", "*", "#", "--", "##"] {
            let line = format!("{marker}sk-ant-abc123def456");
            let out = r.redact(&line);
            assert!(!out.contains("sk-ant-abc123def456"), "leaked: {out}");
            // The marker itself is structure, not secret — a diff must still read as a diff.
            assert_eq!(out, format!("{marker}[redacted]"), "marker lost: {out}");
        }
        // The shape holds mid-line too, e.g. inside a rendered hunk.
        let hunk = "@@ -0,0 +1 @@\n+api_key = sk-ant-abc123def456\n-ghp_0123456789abcdef\n";
        let out = r.redact(hunk);
        assert!(!out.contains("sk-ant-abc123def456"), "leaked: {out}");
        assert!(!out.contains("ghp_0123456789abcdef"), "leaked: {out}");
        assert!(out.contains("@@ -0,0 +1 @@"), "hunk header mangled: {out}");
    }

    /// The other direction (C-185): the markers may only be stripped from the FRONT of a token.
    /// `-` occurs *inside* every `sk-ant-…`/`xoxb-…` key, so promoting it to a boundary character
    /// would split a credential into fragments that no longer start with a prefix and would render
    /// in the clear. Pin that a hyphenated credential still redacts as exactly one unit.
    #[test]
    fn a_hyphenated_credential_redacts_as_one_unit() {
        let r = Redactor::new();
        for secret in [
            "sk-ant-api03-abc123def456",
            "xoxb-1234-5678-abcdefghijkl",
            "sk-proj-abc123def456",
        ] {
            let out = r.redact(&format!("key: {secret} done"));
            assert_eq!(out, "key: [redacted] done", "split into fragments: {out}");
        }
        // A marker-led token that is NOT credential-shaped is untouched — no over-redaction.
        assert_eq!(r.redact("- a bullet"), "- a bullet");
        assert_eq!(r.redact("--- a/note.txt"), "--- a/note.txt");
        assert_eq!(r.redact("+++ b/note.txt"), "+++ b/note.txt");
        assert_eq!(r.redact("# heading-with-hyphens"), "# heading-with-hyphens");
    }

    #[test]
    fn cloned_redactor_shares_value_store() {
        // A secret registered through one handle is redacted by a clone — the property the
        // `credential` capability relies on to scrub a credential materialized mid-run.
        let a = Redactor::new();
        let b = a.clone();
        a.add_secret("clonedsharedsecret");
        assert_eq!(b.redact("x clonedsharedsecret y"), "x [redacted] y");
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
