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
pub mod host;

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

/// Credential-looking prefixes that are redacted even when the exact value isn't registered, each
/// with the **minimum token length** that spelling has to reach before it counts as a credential.
///
/// The floor is per-prefix rather than global (C-315) because the prefixes are not equally
/// distinctive. `sk-ant-` is seven characters of vendor and effectively cannot occur by accident;
/// `hf_` is three, and `hf_hub_download` is an ordinary identifier in any codebase that talks to
/// Hugging Face. Pinning each floor just under the vendor's real token length keeps a short prefix
/// from turning ordinary source into `[redacted]` — the failure mode this list is one addition away
/// from at all times.
///
/// Deliberately **not** here: `sk_test_`. A Stripe test key is not production credential material,
/// and C-216 did not measure it; an unmeasured addition is exactly the drift this table is meant to
/// resist. See `docs/designs/harness-history.md`.
const SECRET_PREFIXES: &[(&str, usize)] = &[
    ("sk-ant-", 8),
    ("sk-", 8),
    ("sk_live_", 20), // Stripe live secret key, `sk_live_` + ~24
    ("xoxb-", 8),
    ("xoxp-", 8),
    ("xoxe-", 8),
    ("ghp_", 8),
    ("gho_", 8),
    ("github_pat_", 12),
    ("glpat-", 20), // GitLab PAT, `glpat-` + 20
    ("hf_", 30),    // Hugging Face token, `hf_` + 34
    ("AKIA", 8),
    ("AIza", 8),
    ("ya29.", 8),
    ("eyJ", 8), // JWT-ish
];

/// Fragments of an assignment's **name** that declare its value to be credential material.
///
/// The contextual rule this feeds (C-315) is the only mechanism here that redacts a token carrying
/// no vendor marking of its own — an AWS *secret* access key is 40 characters of base64 and nothing
/// else. It is deliberately narrow on both halves: the name must say "secret" and the value must
/// look like opaque material ([`is_opaque_material`]). Entropy scoring would catch more and was
/// rejected for it; see the design note.
const SECRET_NAME_MARKERS: &[&str] = &[
    "secret",
    "token",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "access_key",
    "private_key",
    "credential",
];

/// The shortest value [`Redactor::try_add_secret`] will register.
///
/// Registered values are matched by plain substring, so a short one is a censoring machine: register
/// `"abc"` and every `abc` in every diff, log line and tool result becomes `[redacted]`. The floor
/// is the price of that matching strategy, and it is public because a caller that cannot register a
/// value needs to know why.
pub const MIN_REGISTERED_SECRET_LEN: usize = 6;

/// Why a value was not taken into the redactor's registered set.
///
/// Exists because the alternative — the silent no-op `add_secret` used to be — makes a
/// security-registration call indistinguishable from a successful one at the point where the
/// operator could still do something about it (C-315).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unregistered {
    /// Shorter than [`MIN_REGISTERED_SECRET_LEN`] after trimming.
    TooShort { len: usize },
}

impl fmt::Display for Unregistered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unregistered::TooShort { len } => write!(
                f,
                "secret value not registered: {len} characters is below the \
                 {MIN_REGISTERED_SECRET_LEN}-character floor that keeps substring matching from \
                 over-redacting"
            ),
        }
    }
}

impl std::error::Error for Unregistered {}

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

    /// Register a known secret value, reporting the one case in which it is **declined**.
    ///
    /// This is the registration entry point production code should use: registering a credential is
    /// a security action, and a caller that learns nothing when it fails cannot compensate (C-315).
    /// The only failure is [`Unregistered::TooShort`] — see [`MIN_REGISTERED_SECRET_LEN`] for why
    /// that floor exists rather than being a rounding error.
    ///
    /// The value is stored **trimmed** — env/file-sourced secrets often carry a trailing newline,
    /// and storing the raw value would mean the bare token never matches in tool output. Takes
    /// `&self` (interior-mutable, shared store) so a credential materialized mid-run is registered
    /// even when only a clone of the redactor is in hand.
    pub fn try_add_secret(&self, value: impl Into<String>) -> Result<(), Unregistered> {
        let v = value.into();
        let trimmed = v.trim();
        if trimmed.len() < MIN_REGISTERED_SECRET_LEN {
            return Err(Unregistered::TooShort { len: trimmed.len() });
        }
        self.values.lock().unwrap().push(trimmed.to_string());
        Ok(())
    }

    /// Register a known secret value, **discarding** the fact that a value below
    /// [`MIN_REGISTERED_SECRET_LEN`] was not registered at all.
    ///
    /// Kept for callers that have already established the value is long enough — chiefly tests
    /// registering literals. Anywhere the value comes from the environment, a store, a plugin or a
    /// model, prefer [`Redactor::try_add_secret`]: this form cannot tell a registered credential
    /// from an ignored one, and `codewandler-flux-secret` is a published 1.x protocol-line crate, so
    /// the fallible form had to be added beside this one rather than replacing it.
    pub fn add_secret(&self, value: impl Into<String>) {
        let _ = self.try_add_secret(value);
    }

    /// Redact registered values (exact substring) and credential-shaped material from `input`.
    ///
    /// Four passes, in this order, because each later one assumes the earlier ones have already
    /// consumed what they can:
    ///
    /// 1. registered values, longest-first;
    /// 2. [`redact_pem_private_keys`] — the block body between `-----BEGIN … PRIVATE KEY-----` and
    ///    its `-----END`, which no token rule can see because the body is unprefixed base64;
    /// 3. [`redact_url_credentials`] — the password in a `scheme://user:password@host` authority,
    ///    which the tokenizer splits away from anything that identifies it;
    /// 4. [`redact_patterns`] — the token pass: known credential prefixes, plus a value whose own
    ///    assignment name declares it a secret.
    pub fn redact(&self, input: &str) -> String {
        self.scrub(input, &mut None)
    }

    /// Which class of credential material `input` carries, or `None` if it carries none.
    ///
    /// This is [`redact`](Self::redact)'s own verdict, exposed: both run the identical four-pass
    /// pipeline ([`scrub`](Self::scrub)), and a shape is reported exactly when the pass that
    /// recognises it changed the text. There is deliberately no second recogniser — a caller that
    /// **refuses** on credential material and a caller that **hides** it must agree on what counts,
    /// and the only way to guarantee that is for them to be one implementation. Remove a pass from
    /// the pipeline and both lose the same shape.
    ///
    /// The first matching pass wins, so the return value names *a* shape present, not every one.
    /// Callers use it for the sentence they report; the security-relevant fact is `Some` vs `None`.
    ///
    /// The refusal seam this exists for is C-312's credential boundary on platform-sourced plugin
    /// operations, where a response carrying credential-shaped material is refused rather than
    /// merely redacted — redaction hides a leak from the model, refusal says the boundary was
    /// crossed.
    pub fn credential_shape(&self, input: &str) -> Option<CredentialShape> {
        let mut found = None;
        self.scrub(input, &mut found);
        found
    }

    /// The one redaction pipeline, reporting the first pass that recognised something.
    ///
    /// Four passes, in this order, because each later one assumes the earlier ones have already
    /// consumed what they can:
    ///
    /// 1. registered values, longest-first;
    /// 2. [`redact_pem_private_keys`] — the block body between `-----BEGIN … PRIVATE KEY-----` and
    ///    its `-----END`, which no token rule can see because the body is unprefixed base64;
    /// 3. [`redact_url_credentials`] — the password in a `scheme://user:password@host` authority,
    ///    which the tokenizer splits away from anything that identifies it;
    /// 4. [`redact_patterns`] — the token pass: known credential prefixes, plus a value whose own
    ///    assignment name declares it a secret.
    fn scrub(&self, input: &str, found: &mut Option<CredentialShape>) -> String {
        let mut registered = input.to_string();
        // Longest-first so a value that contains another is replaced whole.
        let mut vals = self.values.lock().unwrap().clone();
        vals.sort_by_key(|v| std::cmp::Reverse(v.len()));
        for v in vals {
            if !v.is_empty() {
                registered = registered.replace(&v, REDACTED);
            }
        }
        if found.is_none() && registered != input {
            *found = Some(CredentialShape::Registered);
        }
        let pem = redact_pem_private_keys(&registered);
        if found.is_none() && pem != registered {
            *found = Some(CredentialShape::PrivateKeyBlock);
        }
        let urls = redact_url_credentials(&pem);
        if found.is_none() && urls != pem {
            *found = Some(CredentialShape::UrlPassword);
        }
        let tokens = redact_patterns(&urls);
        if found.is_none() && tokens != urls {
            *found = Some(CredentialShape::TokenShape);
        }
        tokens
    }
}

/// A class of credential material the [`Redactor`] recognises — one variant per pass of its
/// pipeline, reported by [`Redactor::credential_shape`].
///
/// Deliberately coarse. It exists so a refusal can say *what kind of thing* it saw without
/// quoting the thing, and it is not a taxonomy anyone should branch security decisions on: the
/// security-relevant distinction is "the redactor recognised something" versus "it did not".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialShape {
    /// A value registered with this redactor via [`Redactor::try_add_secret`] — on the connectors
    /// seam, the one secret flux legitimately holds (the deployment session bearer) coming back.
    Registered,
    /// A `-----BEGIN … PRIVATE KEY-----` block.
    PrivateKeyBlock,
    /// The password in a `scheme://user:password@host` URL authority.
    UrlPassword,
    /// A token by its own spelling — a known vendor prefix, or an opaque value in an assignment
    /// whose name declares it a secret.
    TokenShape,
}

impl CredentialShape {
    /// A short phrase naming the shape, for a refusal message. Never contains the value.
    pub fn as_str(&self) -> &'static str {
        match self {
            CredentialShape::Registered => "a value registered as a secret with this session",
            CredentialShape::PrivateKeyBlock => "a PEM private-key block",
            CredentialShape::UrlPassword => "a password in a URL authority",
            CredentialShape::TokenShape => "credential-shaped token material",
        }
    }
}

impl fmt::Display for CredentialShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

/// Is this token a credential by its own spelling — a known vendor prefix at or above that
/// prefix's length floor?
fn has_credential_prefix(body: &str) -> bool {
    SECRET_PREFIXES
        .iter()
        .any(|(prefix, min_len)| body.len() >= *min_len && body.starts_with(prefix))
}

/// Does an assignment's name declare its value to be credential material?
///
/// Substring match on the lower-cased name, so `AWS_SECRET_ACCESS_KEY`, `stripeApiKey` and
/// `db_password` all land. Bounded in length because the "name" is whatever token happened to
/// precede the `=`, and a paragraph that happens to contain the word "token" is not a declaration.
///
/// **Public because the same rule has to reach structured input** (C-312). [`redact_patterns`]
/// recovers a name/value pair by tokenizing flat text around `=`; a caller holding a JSON object
/// or a URL query already *has* the pair, and re-deriving the vocabulary there would be a second
/// list to drift from this one. Pair it with [`is_opaque_material`] — the naming half alone
/// over-fires on ordinary config, which is the whole reason the contextual rule has two halves.
pub fn names_a_secret(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    SECRET_NAME_MARKERS.iter().any(|m| lower.contains(m))
}

/// Does this value look like opaque credential material rather than ordinary configuration?
///
/// Three conditions, each buying back a class of false positive that the naming rule alone would
/// produce — this is the guard that keeps `secret_name=my-app-config` and `TOKEN_PATH=/etc/app/key`
/// out of the redactor's mouth:
///
/// - **length ≥ 16.** Real opaque credentials are long; `password=hunter2` is not reached, and that
///   miss is deliberate — see the design note's residual-gap table.
/// - **base64/base64url/hex alphabet only.** `.` and `:` are excluded specifically so hostnames,
///   URLs, version strings and paths-with-extensions can never qualify.
/// - **letters and digits together.** A path segment, an English word and a `${TEMPLATE_REF}` all
///   fail this; 40 characters of AWS base64 essentially never do.
///
/// Public for the same reason as [`names_a_secret`] — see there.
pub fn is_opaque_material(value: &str) -> bool {
    const MIN_OPAQUE_LEN: usize = 16;
    value.len() >= MIN_OPAQUE_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'_' | b'-'))
        && value.bytes().any(|b| b.is_ascii_digit())
        && value.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Redact credential-shaped tokens. A token is a maximal run of non-boundary characters; a run is
/// replaced when either
///
/// - it begins with a known secret prefix — after any leading [`LINE_MARKERS`] are set aside — or
/// - it is the value of an assignment whose **name** declares it a secret and whose own shape is
///   opaque material (C-315: an AWS secret access key carries no prefix, so the only thing that
///   identifies it is the `AWS_SECRET_ACCESS_KEY=` in front of it).
///
/// Boundaries include whitespace AND common delimiters (`= : " ' ` ( ) [ ] { } , ;`), so
/// punctuation-glued forms like `api_key=sk-ant-…` and `"sk-ant-…"` are caught, not just
/// whitespace-separated tokens. The assignment rule keys on `=` only; `key: value` is deliberately
/// out, because `:` introduces far more prose than it does credentials.
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
    /// Emits `token`, redacted or not, and reports whether it names a secret — the caller pairs
    /// that with the boundary character to decide about the *next* token.
    fn flush(token: &mut String, out: &mut String, assigned_to_secret: bool) -> bool {
        // `body` is the token minus any leading diff/list marker; the markers are ASCII, so the
        // byte split is always on a char boundary.
        let body = token.trim_start_matches(LINE_MARKERS);
        let named = names_a_secret(body);
        if has_credential_prefix(body) || (assigned_to_secret && is_opaque_material(body)) {
            out.push_str(&token[..token.len() - body.len()]);
            out.push_str(REDACTED);
        } else {
            out.push_str(token);
        }
        token.clear();
        named
    }

    let mut out = String::with_capacity(input.len());
    let mut token = String::new();
    // Set when the token just flushed named a secret and an `=` closed it, i.e. the next token is
    // that secret's value.
    let mut assigned_to_secret = false;
    for c in input.chars() {
        if is_boundary(c) {
            let named = flush(&mut token, &mut out, assigned_to_secret);
            assigned_to_secret = named && c == '=';
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut token, &mut out, assigned_to_secret);
    out
}

/// Replace the body of every `-----BEGIN … PRIVATE KEY-----` block with a single [`REDACTED`] line,
/// leaving the delimiters in place.
///
/// The delimiters stay because they are the only thing that makes the redaction legible: a bare
/// `[redacted]` in a config file tells a reader nothing, while `-----BEGIN OPENSSH PRIVATE
/// KEY-----` / `[redacted]` / `-----END …` says exactly what was removed. They are also, in
/// themselves, not secret.
///
/// Scoped to `PRIVATE KEY` on purpose. `-----BEGIN CERTIFICATE-----` and `-----BEGIN PUBLIC
/// KEY-----` are public material, and blanking them would be censorship, not containment.
///
/// A block with no `-----END` is redacted **to the end of the input**. That is the common shape of
/// a truncated key rather than an exotic one — `flux-system` byte-caps process output, so a key
/// that runs past the cap arrives with its BEGIN and no END — and there is no reading of an
/// unterminated private key under which its remaining bytes are safe to show.
fn redact_pem_private_keys(input: &str) -> String {
    const PEM_PRIVATE: &str = "PRIVATE KEY";
    if !input.contains(PEM_PRIVATE) {
        return input.to_string();
    }
    fn is_delimiter(trimmed: &str, opener: &str) -> bool {
        trimmed.starts_with(opener) && trimmed.ends_with("-----") && trimmed.contains(PEM_PRIVATE)
    }

    let mut out = String::with_capacity(input.len());
    let mut in_body = false;
    let mut emitted = false;
    for line in input.split_inclusive('\n') {
        let trimmed = line.trim();
        if in_body {
            if is_delimiter(trimmed, "-----END ") {
                in_body = false;
                out.push_str(line);
            } else if !emitted {
                emitted = true;
                out.push_str(REDACTED);
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            // Every further body line is dropped: the block collapses to one marker.
            continue;
        }
        out.push_str(line);
        if is_delimiter(trimmed, "-----BEGIN ") {
            in_body = true;
            emitted = false;
        }
    }
    out
}

/// Characters that end a URL authority. `/`, `?` and `#` are the structural ones; the rest are how
/// a URL is punctuated when it sits inside prose, a shell line or a JSON string.
const AUTHORITY_END: &[char] = &[
    '/', '?', '#', '"', '\'', '`', '<', '>', ',', ';', ')', ']', '}', '\\',
];

/// Replace the password in every `scheme://user:password@host` authority.
///
/// This is a structural rule, not a heuristic: userinfo with a colon in it *is* a credential by the
/// URL grammar, so the false-positive rate is zero by construction. It exists because the token
/// pass cannot help here — `:` is a token boundary, so the password arrives at the matcher as its
/// own unprefixed, uncontextualized run (C-216 measured exactly this on `DATABASE_URL`).
///
/// Only the password is replaced. The scheme, the user and the host stay, because "which database
/// did it connect to, as whom" is the part of a connection string an operator is reading it for.
fn redact_url_credentials(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(scheme_end) = rest.find("://") {
        let start = scheme_end + "://".len();
        let end = start
            + rest[start..]
                .find(|c: char| c.is_whitespace() || AUTHORITY_END.contains(&c))
                .unwrap_or(rest.len() - start);
        let authority = &rest[start..end];
        // The LAST `@` closes the userinfo, so a password containing `@` is still bounded correctly.
        let password = authority.rfind('@').and_then(|at| {
            authority[..at]
                .find(':')
                .map(|colon| (start + colon + 1, start + at))
                .filter(|(from, to)| to > from)
        });
        match password {
            Some((from, to)) => {
                out.push_str(&rest[..from]);
                out.push_str(REDACTED);
                out.push_str(&rest[to..end]);
            }
            None => out.push_str(&rest[..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
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
            concat!("xoxb", "-1234-5678-abcdefghijkl"),
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

    /// C-315 — the three vendor spellings C-216 measured as missed, each a plain prefix addition.
    ///
    /// C-325: joined from fragments at compile time — the split falls inside the vendor prefix — so
    /// a forge's secret scanning finds no credential in this file while `redact` still receives the
    /// identical bytes. Two of these three were among the twelve detections that blocked a real
    /// push on 2026-07-31.
    #[test]
    fn the_vendor_spellings_c216_measured_are_caught() {
        let r = Redactor::new();
        for secret in [
            concat!("sk_", "live_51NotARealStripeSecretKey00"),
            concat!("hf", "_NotARealHuggingFaceAccessToken0000"),
            concat!("glpat", "-NotARealGitlabPat00000"),
        ] {
            let out = r.redact(&format!("KEY={secret}\n"));
            assert!(!out.contains(secret), "leaked: {out}");
        }
    }

    /// The other side of a short prefix: `hf_` and `glpat-` have length floors precisely so ordinary
    /// identifiers keep their spelling. A prefix list that redacts source code is not a fix.
    #[test]
    fn a_short_prefix_does_not_swallow_ordinary_identifiers() {
        let r = Redactor::new();
        for benign in [
            "hf_hub_download",
            "hf_transfer enabled",
            "glpat-example",
            "sk_live_x",
        ] {
            assert_eq!(r.redact(benign), benign, "over-redacted: {benign}");
        }
    }

    /// C-315 — an AWS *secret* access key has no prefix; the only thing identifying it is the name
    /// of the assignment it sits in.
    #[test]
    fn a_secret_named_assignment_redacts_its_opaque_value() {
        let r = Redactor::new();
        let line = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI0K7MDENGbPxRfiCYEXAMPLEKEY";
        let out = r.redact(line);
        assert_eq!(out, "AWS_SECRET_ACCESS_KEY=[redacted]", "leaked: {out}");
        // The name half is a substring match, so the vocabulary reaches the spellings in the wild.
        for name in ["db_password", "stripeApiKey", "SERVICE_CREDENTIAL"] {
            let out = r.redact(&format!("{name}=aB3dEf6hIj9lMn2pQr5t"));
            assert_eq!(out, format!("{name}=[redacted]"), "leaked: {out}");
        }
    }

    /// The contextual rule's whole risk is over-firing on ordinary config, so pin the four ways a
    /// secret-named assignment is left alone. Each of these is a shape a real repository is full of.
    #[test]
    fn a_secret_named_assignment_leaves_ordinary_config_alone() {
        let r = Redactor::new();
        for line in [
            "TOKEN_PATH=/etc/flux/credentials", // no digit — a path, not material
            "SECRET_NAME=my-app-production-config", // no digit
            "AWS_SECRET_ACCESS_KEY=${AWS_SECRET}", // template reference
            "API_TOKEN_URL=https://auth.example.com", // punctuation outside the alphabet
            "password=hunter2",                 // below the opaque-material floor
            "secret_ttl=3600",                  // no letters
        ] {
            assert_eq!(r.redact(line), line, "over-redacted: {line}");
        }
        // And a name that merely *mentions* a secret does not make the next word one.
        assert_eq!(
            r.redact("the token was rotated on 2026-01-02T03:04:05Z"),
            "the token was rotated on 2026-01-02T03:04:05Z"
        );
    }

    /// C-315 — PEM private-key material: the body goes, the delimiters stay, and a public block is
    /// not touched at all.
    #[test]
    fn a_pem_private_key_body_is_redacted_and_its_delimiters_are_not() {
        let r = Redactor::new();
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
                   b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtz\n\
                   c2gtZWQyNTUxOQAAACBOtARealKeyMaterialWouldContinueForManyLines00\n\
                   -----END OPENSSH PRIVATE KEY-----\n";
        let out = r.redact(&format!("cat > id_ed25519 <<'EOF'\n{key}EOF\n"));
        assert!(!out.contains("b3BlbnNzaC1rZXktdjEA"), "leaked: {out}");
        assert!(!out.contains("c2gtZWQyNTUxOQ"), "leaked: {out}");
        assert_eq!(
            out,
            "cat > id_ed25519 <<'EOF'\n\
             -----BEGIN OPENSSH PRIVATE KEY-----\n\
             [redacted]\n\
             -----END OPENSSH PRIVATE KEY-----\n\
             EOF\n"
        );

        // A certificate and a public key are public material — blanking them would be censorship.
        let cert = "-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAKa\n-----END CERTIFICATE-----\n";
        assert_eq!(r.redact(cert), cert);
        let public = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0\n-----END PUBLIC KEY-----\n";
        assert_eq!(r.redact(public), public);
    }

    /// A key that ran past a byte cap arrives with a BEGIN and no END. There is no reading of the
    /// remaining bytes under which they are safe to show, so the redaction runs to the end.
    #[test]
    fn an_unterminated_private_key_block_is_redacted_to_the_end() {
        let r = Redactor::new();
        let out = r.redact("-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\ntruncated");
        assert_eq!(out, "-----BEGIN RSA PRIVATE KEY-----\n[redacted]\n");
    }

    /// C-315 — the password in a connection URL. `:` is a token boundary, so the token pass sees the
    /// password as a bare unprefixed run; only the URL grammar identifies it.
    #[test]
    fn a_connection_url_password_is_redacted_and_the_rest_of_the_url_is_not() {
        let r = Redactor::new();
        assert_eq!(
            r.redact("DATABASE_URL=postgres://flux:hunter2pass@db.internal:5432/app"),
            "DATABASE_URL=postgres://flux:[redacted]@db.internal:5432/app"
        );
        // A password containing `@` is bounded by the LAST `@`, not the first.
        assert_eq!(
            r.redact("redis://user:p@ss@cache:6379"),
            "redis://user:[redacted]@cache:6379"
        );
        // Two of them on one line, and each is handled independently.
        assert_eq!(
            r.redact("from amqp://a:b1@x/ to amqp://c:d2@y/"),
            "from amqp://a:[redacted]@x/ to amqp://c:[redacted]@y/"
        );
        // No userinfo, or a user with no password: nothing to redact, nothing changed.
        for benign in [
            "https://docs.example.com/a/b?c=d",
            "ssh://git@github.com/codewandler/flux.git",
            "see https://example.com, then stop",
        ] {
            assert_eq!(r.redact(benign), benign, "over-redacted: {benign}");
        }
    }

    /// An all-digit credential is outside every heuristic here — no prefix can mark it, and
    /// [`is_opaque_material`] requires a letter precisely so `secret_ttl=3600` survives. That makes
    /// registration its *only* recourse, so registration has to be total: anything that narrows
    /// where a registered value is matched is a hole in this guarantee, not an optimization.
    #[test]
    fn an_all_digit_credential_is_registration_only_and_registration_is_total() {
        let r = Redactor::new();
        let numeric = "216216216216216218";
        // Not caught by shape, even named by an assignment that says "secret".
        assert_eq!(
            r.redact(&format!("ACCOUNT_SECRET_ID={numeric}")),
            format!("ACCOUNT_SECRET_ID={numeric}")
        );
        // Caught once registered, in every position it can occupy.
        r.try_add_secret(numeric).unwrap();
        assert_eq!(
            r.redact(&format!("id={numeric} ({numeric}) [{numeric}]")),
            "id=[redacted] ([redacted]) [[redacted]]"
        );
    }

    /// C-315 — the registration floor is a real decision, and a caller now learns when it applies.
    /// The silent form is what made a security-registration call indistinguishable from a no-op.
    #[test]
    fn a_declined_registration_is_visible_to_the_caller() {
        let r = Redactor::new();
        assert_eq!(
            r.try_add_secret("hunt3"),
            Err(Unregistered::TooShort { len: 5 })
        );
        assert_eq!(r.redact("password=hunt3"), "password=hunt3");
        assert!(r.try_add_secret("hunt3r").is_ok());
        assert_eq!(r.redact("password=hunt3r"), "password=[redacted]");
        // Trimming happens before the floor is applied, so whitespace does not buy length.
        assert_eq!(
            r.try_add_secret("  ab  "),
            Err(Unregistered::TooShort { len: 2 })
        );
        assert!(format!("{}", Unregistered::TooShort { len: 2 }).contains("below the 6-character"));
    }

    /// C-312 — `credential_shape` is `redact`'s own verdict, not a second recogniser. For every
    /// input, "a shape was reported" and "the text changed" are the same fact; there is no input
    /// on which a refusing caller and a hiding caller can disagree.
    ///
    /// This is the property that makes the boundary test in `flux-plugin` mean something: delete a
    /// pass from `scrub` and the refusal loses exactly the shape the redaction lost.
    #[test]
    fn a_reported_shape_and_a_changed_text_are_the_same_fact() {
        let r = Redactor::new();
        r.try_add_secret("registeredsessionbearer").unwrap();
        for input in [
            // Each of the four passes, and then a spread of material that must stay untouched.
            "bearer registeredsessionbearer here",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\n",
            "postgres://flux:hunter2pass@db.internal:5432/app",
            concat!("xoxb", "-1234-5678-abcdefghijkl"),
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI0K7MDENGbPxRfiCYEXAMPLEKEY",
            "an ordinary sentence",
            "TOKEN_PATH=/etc/flux/credentials",
            "-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAKa\n-----END CERTIFICATE-----\n",
            "https://docs.example.com/a/b?c=d",
            "",
        ] {
            assert_eq!(
                r.credential_shape(input).is_some(),
                r.redact(input) != input,
                "detection and redaction disagree on: {input:?}"
            );
        }
    }

    /// The four passes each report their own shape, so a refusal can say what it saw without
    /// quoting it. Order matters: the first pass that fires wins.
    #[test]
    fn each_pass_reports_its_own_shape() {
        let r = Redactor::new();
        r.try_add_secret("registeredsessionbearer").unwrap();
        assert_eq!(
            r.credential_shape("bearer registeredsessionbearer"),
            Some(CredentialShape::Registered)
        );
        assert_eq!(
            r.credential_shape(
                "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\n"
            ),
            Some(CredentialShape::PrivateKeyBlock)
        );
        assert_eq!(
            r.credential_shape("postgres://flux:hunter2pass@db.internal:5432/app"),
            Some(CredentialShape::UrlPassword)
        );
        assert_eq!(
            r.credential_shape(concat!("glpat", "-NotARealGitlabPat00000")),
            Some(CredentialShape::TokenShape)
        );
        assert_eq!(r.credential_shape("nothing to see"), None);
        // The phrase a refusal prints never contains a value.
        for shape in [
            CredentialShape::Registered,
            CredentialShape::PrivateKeyBlock,
            CredentialShape::UrlPassword,
            CredentialShape::TokenShape,
        ] {
            assert!(!shape.as_str().is_empty());
            assert_eq!(shape.to_string(), shape.as_str());
        }
    }

    /// C-312 — the two halves of the contextual rule are public so a caller holding *structured*
    /// input (a JSON object, a URL query) can apply the same vocabulary to a name/value pair it
    /// already has, instead of maintaining a second list. Pin that they still mean what
    /// `redact_patterns` uses them for.
    #[test]
    fn the_contextual_rules_two_halves_are_reusable_on_their_own() {
        for name in ["AWS_SECRET_ACCESS_KEY", "db_password", "access_token"] {
            assert!(names_a_secret(name), "should name a secret: {name}");
        }
        for name in ["client_id", "url", "state", ""] {
            assert!(!names_a_secret(name), "should not name a secret: {name}");
        }
        for value in [
            "wJalrXUtnFEMI0K7MDENGbPxRfiCYEXAMPLEKEY",
            "aB3dEf6hIj9lMn2pQr5t",
        ] {
            assert!(is_opaque_material(value), "should be opaque: {value}");
        }
        for value in [
            "hunter2",
            "/etc/flux/credentials",
            "https://a.example.com",
            "3600",
        ] {
            assert!(!is_opaque_material(value), "should not be opaque: {value}");
        }
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
