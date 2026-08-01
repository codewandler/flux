//! The **webhook** adapter (`kind = "webhook" | "http"`): an axum server per channel. A `POST` to its
//! path delivers the JSON body under the channel name and replies with the triggered journeys' results.
//!
//! # The request is authenticated before it is decoded (C-291)
//!
//! The handler takes [`Bytes`], not `Json<Value>`. That is the structural point of C-291 and not a
//! refactor: an extractor consumes and deserializes the body *before the handler body runs*, so with
//! `Json<Value>` there was no point in `handle` where the raw bytes still existed — every check,
//! including the bearer one, ran strictly after the decode, and a signature check could not have been
//! inserted anywhere.
//!
//! **A signature is over bytes.** Anything that re-serializes, reorders keys, drops a duplicate or
//! normalises whitespace before verifying has verified something other than what the sender signed,
//! so the raw buffer is what reaches the verifier and `serde_json::from_slice` sits textually after
//! the comparison, in [`handle`], in one function. The order is:
//!
//! ```text
//! headers → raw bytes → open-channel guard → bearer → signature → content-type → decode → deliver
//! ```
//!
//! Content-type negotiation is *after* the signature deliberately: a `415` an unauthenticated caller
//! can tell apart from a `401` is a probe oracle for how far a forgery got. Every authentication
//! failure — no token, wrong token, missing header, bad digest, stale timestamp — answers with the
//! one fixed [`UNAUTHORIZED_BODY`].
//!
//! # What this module owns, and where C-292 begins
//!
//! This module owns the **seam**: capturing the raw body, carrying and fully validating a declared
//! `verify` record, and refusing at load when a channel declares verification this build cannot
//! perform. It computes no digest. [`SignatureVerifier`] is the trait C-292 implements and
//! [`verifier_for`] is the one place it plugs in — and until it does, that function returns `None`
//! and [`build_verification`] turns the `None` into a **load error**. A missing verifier never
//! degrades to "unverified".

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_lang::program::ChannelDecl;

use crate::adapters::connector::parse_tolerance;
use crate::config::{VerifyDecl, VerifySpec, WebhookSettings};
use crate::{Channel, Deliverer};

/// The **one** body every authentication failure answers with.
///
/// Not "signature mismatch" vs "stale timestamp" vs "missing header": a caller that can tell those
/// apart has a probe for how far its forgery got, and the difference between a wrong digest and a
/// stale timestamp is exactly the feedback a forger needs. Matches the literal this handler has
/// always returned for a bad bearer, so the signature path is indistinguishable from the token path
/// too.
const UNAUTHORIZED_BODY: &str = "unauthorized";

/// The shortest signing secret this channel will load.
///
/// Mirrors `flux_secret::MIN_REGISTERED_SECRET_LEN` (6). `Redactor::try_add_secret` silently
/// registers nothing below that floor, so a shorter secret is registered nowhere and redacted never
/// — a `secret "KEY"` declaration is a promise the value will not surface, and below this length the
/// promise cannot be kept. `flux_app::resolve_secrets` already fails the load for a *reference* that
/// resolves too short (C-315); this is the same rule stated where the value is used, which is the
/// half that also catches a secret written as a literal, since a literal never passes through the
/// redactor at all.
///
/// Restated as a constant rather than imported: flux-channels does not depend on `flux-secret`, and
/// adding the dependency is a manifest change this story is fenced from making.
const MIN_SECRET_LEN: usize = 6;

pub struct WebhookChannel {
    name: String,
    addr: SocketAddr,
    path: String,
    is_async: bool,
    token: Option<String>,
    verify: Verification,
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Verification — the seam
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// A check over the **raw request bytes**, before anything decodes them.
///
/// This is the whole interface C-292 implements. `body` is the buffer exactly as it arrived: not
/// re-serialized, not reordered, not re-encoded. An implementation that wants JSON must parse it
/// itself, *after* deciding, because a verifier that normalises its input has authenticated a
/// document the sender never sent.
pub(crate) trait SignatureVerifier: Send + Sync {
    /// Whether `body` carries, in `headers`, a signature this scheme accepts.
    ///
    /// One `bool` and no error type, deliberately: every rejection reason collapses to the same
    /// answer at the wire ([`UNAUTHORIZED_BODY`]), and a richer return type is an invitation to
    /// report which reason — which is the probe oracle this design refuses to build.
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> bool;
}

/// What a loaded channel establishes about an inbound delivery's authenticity.
///
/// **Tri-state, and the three arms are three different facts.** `Unstated` (the declaration says
/// nothing) and `None` (`verify "none"` — the operator decided) behave identically at request time
/// and must not normalise together: only one of them is admissible on a non-loopback bind, and
/// C-295 needs the difference visible to a flow.
#[derive(Clone)]
pub(crate) enum Verification {
    /// The declaration states nothing. Legal only on a loopback bind.
    Unstated,
    /// `verify "none"` — a deliberate, written statement that this endpoint carries no signature.
    None,
    /// A declared scheme, with a verifier this build can actually perform. Unconstructible while
    /// [`verifier_for`] returns `None`, which is what keeps "declared" and "performed" the same
    /// thing.
    Scheme(Arc<dyn SignatureVerifier>),
}

impl Verification {
    /// Whether something about the *request* is actually checked. The property the bind guard keys
    /// on — see [`is_effectively_open`].
    fn is_verifying(&self) -> bool {
        matches!(self, Self::Scheme(_))
    }

    /// Whether the declaration made a verification decision at all. `verify "none"` did; silence
    /// did not.
    fn is_stated(&self) -> bool {
        !matches!(self, Self::Unstated)
    }

    /// Whether this delivery may proceed to be decoded. `true` when nothing is checked — the
    /// *authorisation* to serve an unchecked endpoint is decided at load, not here.
    fn admits(&self, headers: &HeaderMap, body: &[u8]) -> bool {
        match self {
            Self::Unstated | Self::None => true,
            Self::Scheme(verifier) => verifier.verify(headers, body),
        }
    }
}

/// The verifier for a validated scheme, or `None` when this build has none.
///
/// **This is the single plug-in point for C-292.** It returns a verifier for `scheme = "hmac"` once
/// the parameterized HMAC lands; until then it returns `None` for everything, and
/// [`build_verification`] turns that into a load error rather than a channel that serves. The
/// distinction is the entire safety property: a build that cannot perform a declared verification
/// must refuse to bind, never bind and skip it.
fn verifier_for(spec: &VerifySpec) -> Option<Arc<dyn SignatureVerifier>> {
    let _ = spec;
    None
}

/// Whether **nothing at all** authenticates an inbound request to a channel in this shape.
///
/// Keyed on the *property*, not on which declaration variant produced it — C-321's lesson, learned
/// on `flux-server`'s bind guard, where a guard that read `matches!(auth, Open)` let a
/// `SharedSecret` holding the empty string bind `0.0.0.0` because an empty secret authenticates
/// every anonymous request without being that variant. So an empty or whitespace-only token counts
/// as no token here regardless of what refuses it elsewhere, and a future verification arm that is
/// open in effect is caught without anyone remembering to extend a `matches!`.
///
/// Note what is *not* required: a bearer token. A signature-verifying channel is authenticated by
/// the signature, which is the point of the story — a vendor that signs its payloads and cannot send
/// a custom `Authorization` header now has an authenticated route in.
fn is_effectively_open(token: Option<&str>, verify: &Verification) -> bool {
    let token_authenticates = token.is_some_and(|t| !t.trim().is_empty());
    !token_authenticates && !verify.is_verifying()
}

/// Whether a bind at `addr` may serve requests nothing authenticates. Loopback only — the host
/// auto-approves tools, so an open non-loopback listener is a remote-trigger surface.
fn unauthenticated_bind_allowed(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Translate a `verify` declaration into the verification this channel will perform, refusing
/// everything refusable **before a port is bound**.
///
/// The cascade order is load-bearing: every structural rule about the declared scheme is checked
/// *before* the blanket "this build has no verifier" refusal, so a defective declaration reports its
/// own defect rather than hiding behind "not implemented" — the same ordering the connector arm uses
/// for the same reason (D-216).
fn build_verification(channel: &str, decl: Option<&VerifyDecl>) -> anyhow::Result<Verification> {
    let spec = match decl {
        None => return Ok(Verification::Unstated),
        Some(VerifyDecl::Word(word)) if word == "none" => return Ok(Verification::None),
        Some(VerifyDecl::Word(word)) => anyhow::bail!(
            "channel `{channel}`: `verify {word:?}` is not a verification answer — write \
             `verify \"none\"` to state deliberately that this endpoint carries no signature, or a \
             `verify {{ scheme: \"hmac\", … }}` record naming a scheme"
        ),
        Some(VerifyDecl::Scheme(spec)) => spec.as_ref(),
    };
    validate_scheme(channel, spec)?;
    let scheme = spec.scheme.as_deref().unwrap_or_default();
    match verifier_for(spec) {
        Some(verifier) => Ok(Verification::Scheme(verifier)),
        // Last, and only once the declaration is known to be well-formed.
        None => anyhow::bail!(
            "channel `{channel}`: `verify` declares scheme `{scheme}`, which this build cannot \
             perform — the signature verifier (C-292) is not implemented, so binding this endpoint \
             would accept unsigned deliveries on a surface the declaration presents as verified"
        ),
    }
}

/// Every structural rule about a declared scheme, each its own refusal so a defect reports itself.
///
/// Deliberately a mirror of the connector arm's `validate_hmac`: an operator writing a `verify`
/// record and a connector publishing an `HmacSpec` are describing the same four axes, and two
/// vocabularies for one thing is how one of them drifts.
fn validate_scheme(channel: &str, spec: &VerifySpec) -> anyhow::Result<()> {
    let scheme = required(channel, "scheme", spec.scheme.as_deref())?;
    if scheme != "hmac" {
        anyhow::bail!(
            "channel `{channel}`: `verify` declares unknown scheme `{scheme}` — a scheme nobody can \
             perform would have to fail open or fail confusingly, and neither is acceptable on an \
             authentication path"
        );
    }
    let algorithm = required(channel, "algorithm", spec.algorithm.as_deref())?;
    if !matches!(algorithm, "sha1" | "sha256") {
        anyhow::bail!("channel `{channel}`: `verify` signs with unknown algorithm `{algorithm}`");
    }
    let encoding = required(channel, "encoding", spec.encoding.as_deref())?;
    if !matches!(encoding, "hex" | "base64") {
        anyhow::bail!(
            "channel `{channel}`: `verify` spells its digest with unknown encoding `{encoding}`"
        );
    }
    let header = required(channel, "header", spec.header.as_deref())?;
    validate_header(channel, "the signature `header`", header)?;
    if let Some(prefix) = &spec.prefix {
        if prefix.is_empty() {
            anyhow::bail!(
                "channel `{channel}`: `verify` declares an empty `prefix`; a prefix nobody wrote is \
                 an absent `prefix`, not a prefix matching everything"
            );
        }
    }

    // The secret. Never echoed — not the value, not its length, not a fragment.
    let secret = required(channel, "secret", spec.secret.as_deref())?;
    if secret.trim().is_empty() {
        anyhow::bail!(
            "channel `{channel}`: `verify` declares an empty `secret`, which signs nothing. A \
             `secret \"KEY\"` reference resolves to an empty string when `KEY` is exported empty."
        );
    }
    if secret.trim().len() < MIN_SECRET_LEN {
        anyhow::bail!(
            "channel `{channel}`: `verify` declares a signing secret shorter than \
             {MIN_SECRET_LEN} characters, which the redactor will not register — so it would be \
             scrubbed from no log, no diff and no tool result. No vendor issues a signing key that \
             short, and one that short signs nothing worth verifying."
        );
    }

    // The signed template. `{body}` is mandatory: a template that omits it signs a string the
    // payload never enters, so one captured signature verifies every forged payload — and every
    // other thing about such a declaration reads as correct.
    let signed = required(channel, "signed", spec.signed.as_deref())?;
    let mut rest = signed;
    let (mut has_body, mut has_timestamp) = (false, false);
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after.find('}').ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{channel}`: `verify`'s signed template `{signed}` has an unclosed \
                 placeholder"
            )
        })?;
        match &after[..close] {
            "body" => has_body = true,
            "timestamp" => has_timestamp = true,
            other => anyhow::bail!(
                "channel `{channel}`: `verify`'s signed template interpolates unknown placeholder \
                 `{{{other}}}` — a host that cannot fill it would fail open or fail confusingly"
            ),
        }
        rest = &after[close + 1..];
    }
    if !has_body {
        anyhow::bail!(
            "channel `{channel}`: `verify`'s signed template `{signed}` never interpolates \
             `{{body}}`, so one captured signature would verify every forged payload"
        );
    }

    if has_timestamp {
        // A timestamped scheme with no window is a signature that replays forever, which is worse
        // than not timestamping at all because it reads as though replay were handled.
        let tolerance = spec.tolerance.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{channel}`: `verify`'s signed template interpolates `{{timestamp}}` with \
                 no `tolerance` — a replay window nobody states is a signature that replays forever"
            )
        })?;
        parse_tolerance(tolerance).ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{channel}`: `verify` declares `tolerance = {tolerance:?}`, which is not a \
                 duration — a window nobody can apply reads as though replay were handled just as \
                 convincingly as one that is"
            )
        })?;
        let timestamp = spec.timestamp.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "channel `{channel}`: `verify`'s signed template interpolates `{{timestamp}}` but \
                 selects no `timestamp` — a host left to guess falls back to its own clock, which \
                 verifies nothing"
            )
        })?;
        // Body-sourced is spellable and **unimplementable by construction**, not merely
        // unimplemented: honouring it would parse the very bytes the signature authenticates,
        // before they are authenticated, which inverts the whole ordering this module exists to
        // establish and hands an anonymous caller a parser.
        match timestamp.source.as_deref() {
            Some("header") => {}
            Some("body") => anyhow::bail!(
                "channel `{channel}`: `verify` reads its signed timestamp from the body — that \
                 would parse the very bytes the signature is meant to authenticate, before they are \
                 authenticated"
            ),
            Some(other) => anyhow::bail!(
                "channel `{channel}`: `verify`'s timestamp reads from unknown source `{other}`"
            ),
            None => anyhow::bail!(
                "channel `{channel}`: `verify`'s `timestamp` names no `source` — it is read from a \
                 `header`, always"
            ),
        }
        let name = required(channel, "timestamp.name", timestamp.name.as_deref())?;
        validate_header(channel, "the `timestamp` selector", name)?;
    } else if spec.timestamp.is_some() || spec.tolerance.is_some() {
        // A selector or a window the template never interpolates describes a value nothing reads —
        // and reads, to whoever wrote it, as replay protection that is in force.
        anyhow::bail!(
            "channel `{channel}`: `verify` declares a `timestamp` or a `tolerance` its signed \
             template `{signed}` never interpolates, so neither is ever applied"
        );
    }
    Ok(())
}

/// A field a scheme cannot be performed without. Absent is refused by name, never defaulted: every
/// default here would be a guess about how a vendor signs.
fn required<'a>(channel: &str, field: &str, value: Option<&'a str>) -> anyhow::Result<&'a str> {
    value.ok_or_else(|| {
        anyhow::anyhow!("channel `{channel}`: `verify` states no `{field}`, which it cannot omit")
    })
}

/// A header name this host can actually look up. An unparseable one is not cosmetic: `HeaderMap::get`
/// resolves it to nothing on every delivery, so a signature header nobody can read fails **open**.
fn validate_header(channel: &str, what: &str, header: &str) -> anyhow::Result<()> {
    if header.trim().is_empty() {
        anyhow::bail!("channel `{channel}`: {what} names no header");
    }
    if HeaderName::from_bytes(header.as_bytes()).is_err() {
        anyhow::bail!(
            "channel `{channel}`: {what} names {header:?}, which is not a valid HTTP header name — \
             nothing would ever resolve it"
        );
    }
    Ok(())
}

impl WebhookChannel {
    pub fn from_decl(decl: &ChannelDecl) -> anyhow::Result<Self> {
        let s: WebhookSettings = serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{}` settings: {e}", decl.name))?;
        let addr = SocketAddr::from_str(&s.addr)
            .map_err(|e| anyhow::anyhow!("channel `{}`: bad addr `{}`: {e}", decl.name, s.addr))?;
        // Secrets are already host-resolved before these settings deserialize, so `token` is a plain value.
        //
        // **An empty token is not a token, and it is worse than none.**
        //
        // `token ""` — or `token secret "K"` where `K` is exported empty, because
        // `flux_app::resolve_secrets` resolves through `std::env::var`, which does not filter an empty
        // value — would otherwise arrive here as `Some("")`. It then satisfies the `is_none()` guard
        // below, so the public bind is permitted, and the bearer check compares the *presented* token
        // (which is `""` when the request carries no `Authorization` header at all) against the
        // expected one; two empty byte strings are equal, so every anonymous request authenticates. On
        // a host that auto-approves tools that is an open remote-trigger surface presented as an
        // authenticated one.
        //
        // Refused rather than normalised to `None`, and refused on **loopback** too: normalising is
        // silent, and would ship an operator a channel they believe is authenticated, one `addr` edit
        // away from being public. [`authorized`] refuses an empty expected token as well, so neither
        // half depends on the other being right — but this is the half that runs before a port is
        // bound, which is the only half that can prevent the exposure rather than survive it.
        let token = match s.token.as_deref() {
            Some(token) if token.trim().is_empty() => anyhow::bail!(
                "channel `{}`: `token` is set but empty, which would authenticate every request — \
                 including one carrying no `Authorization` header at all. Give it a value, or remove \
                 it (a loopback bind needs none). A `secret \"KEY\"` reference resolves to an empty \
                 string when `KEY` is exported empty.",
                decl.name
            ),
            other => other.map(str::to_string),
        };
        // The declared verification, fully validated — before any of the bind rules below, because
        // whether the channel *may* be public depends on what it verifies.
        let verify = build_verification(&decl.name, s.verify.as_ref())?;

        // The host auto-approves tools (no interactive approver), so an open non-loopback listener is
        // a remote-trigger surface. Keyed on the property `is_effectively_open` rather than on
        // `token.is_none()` (C-321): a signature-verifying channel is authenticated by its signature
        // and needs no bearer, and an empty token is not a token however it was spelled. [`handle`]
        // refuses the same property before it looks at a request, so neither half depends on the
        // other being right — but this is the half that runs before a port is bound, which is the
        // only half that can prevent the exposure rather than survive it.
        if !unauthenticated_bind_allowed(addr) && is_effectively_open(token.as_deref(), &verify) {
            anyhow::bail!(
                "channel `{}`: refusing to bind non-loopback {addr} with nothing to authenticate a \
                 delivery — set a `token secret \"KEY\"`, or declare a `verify {{ … }}` scheme that \
                 checks the sender's signature",
                decl.name
            );
        }
        // Silence is not a verification answer on a public endpoint. A channel that states nothing
        // and a channel that states `verify "none"` are different facts, and normalising them
        // together is how an operator ships an unsigned public webhook without ever deciding to.
        if !unauthenticated_bind_allowed(addr) && !verify.is_stated() {
            anyhow::bail!(
                "channel `{}`: refusing to bind non-loopback {addr} without a stated verification \
                 decision — add `verify \"none\"` to say deliberately that this vendor sends no \
                 signature, or a `verify {{ scheme: \"hmac\", … }}` record saying how one is checked",
                decl.name
            );
        }
        // axum route paths must start with `/`; normalize a bare path so a typo isn't a runtime panic.
        let path = if s.path.starts_with('/') {
            s.path
        } else {
            format!("/{}", s.path)
        };
        Ok(Self {
            name: decl.name.clone(),
            addr,
            path,
            is_async: s.is_async,
            token,
            verify,
        })
    }

    /// Build the axum router for this channel over `d` (exposed for hermetic tests).
    pub fn router(&self, d: Arc<dyn Deliverer>) -> Router {
        let state = Arc::new(HookState {
            name: self.name.clone(),
            deliverer: d,
            is_async: self.is_async,
            token: self.token.clone(),
            verify: self.verify.clone(),
            unauthenticated_allowed: unauthenticated_bind_allowed(self.addr),
        });
        Router::new()
            .route(&self.path, post(handle))
            .with_state(state)
    }
}

struct HookState {
    name: String,
    deliverer: Arc<dyn Deliverer>,
    is_async: bool,
    token: Option<String>,
    verify: Verification,
    /// Whether this channel's bind may serve requests nothing authenticates — loopback only.
    /// Carried rather than re-derived so [`handle`] can restate the bind rule at the point a request
    /// is answered; see the guard at the top of it.
    unauthenticated_allowed: bool,
}

/// The one answer every authentication failure gets.
fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, UNAUTHORIZED_BODY).into_response()
}

/// What `Json<Value>` negotiated implicitly, made explicit so it can be sequenced **after**
/// verification: `application/json`, or any `application/…+json`, ignoring parameters like
/// `; charset=utf-8`.
fn json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let mime = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let Some(subtype) = mime.strip_prefix("application/") else {
        return false;
    };
    subtype == "json" || subtype.ends_with("+json")
}

/// The request path, in the one order that makes a signature mean anything.
///
/// `body: Bytes` rather than `Json<Value>` is the structural change C-291 is: the raw buffer still
/// exists here, so every check below runs over the bytes the sender actually signed and
/// `serde_json::from_slice` sits textually after all of them, in this function, where the ordering
/// can be read rather than inferred from an extractor list.
async fn handle(State(state): State<Arc<HookState>>, headers: HeaderMap, body: Bytes) -> Response {
    // 1 ─ The bind rule, restated where a request is answered. `from_decl` already refuses to
    //     construct an effectively-open non-loopback channel, and that is the half that prevents
    //     the exposure; this is the half that survives a future path reaching the handler without
    //     that constructor. Keyed on the same property, so the two cannot drift apart.
    if !state.unauthenticated_allowed && is_effectively_open(state.token.as_deref(), &state.verify)
    {
        return unauthorized();
    }
    // 2 ─ The bearer, if one is declared.
    if !authorized(state.token.as_deref(), &headers) {
        return unauthorized();
    }
    // 3 ─ The signature, over the raw bytes, unmodified. `token` and `verify` compose: control
    //     reaches here only if the bearer already passed, so declaring both means both must pass.
    if !state.verify.admits(&headers, &body) {
        return unauthorized();
    }

    // 4 ─ Only now does anything look at what the body *is*. A content-type rejection emitted before
    //     the signature check is a probe oracle — a caller that can tell `415` from `401` learns its
    //     forgery got past authentication.
    if !json_content_type(&headers) {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported media type").into_response();
    }
    // 5 ─ The decode, last. A fixed body: a parse error names offsets in bytes the caller controls,
    //     and by here it has authenticated, so there is nothing to gain from echoing its input.
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "bad request").into_response();
    };

    if state.is_async {
        let d = state.deliverer.clone();
        let label = state.name.clone();
        tokio::spawn(async move {
            if let Err(e) = d.deliver(&label, body).await {
                eprintln!("webhook `{label}`: async delivery failed: {e}");
            }
        });
        return StatusCode::ACCEPTED.into_response();
    }

    match state.deliverer.deliver(&state.name, body).await {
        Ok(runs) => {
            let out: Vec<Value> = runs
                .into_iter()
                .map(|r| json!({ "journey": r.journey, "result": r.result, "steps": r.steps }))
                .collect();
            Json(json!({ "runs": out })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Whether a request may be delivered, given the channel's expected bearer token.
///
/// Two rules, and the second is the one that is easy to get wrong:
///
/// - **No expected token** → nothing to check. Only reachable on a loopback bind; a non-loopback one
///   without a token is refused at load.
/// - **An empty expected token authenticates nothing.** A request with no `Authorization` header
///   presents `""`, and a constant-time compare of two empty byte strings is `true` — so an empty
///   expected token would admit every anonymous caller while the channel reads, everywhere it is
///   printed or logged, as "token-protected". [`WebhookChannel::from_decl`] already refuses one
///   before a port is bound; this is the same rule stated where the comparison happens, so a future
///   path that reaches the handler without that constructor cannot reopen the hole.
///
/// Extracted rather than inlined precisely so the two halves are independently testable: once the
/// constructor makes `Some("")` unreachable, a test routed through it can only ever cover one of them.
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

/// Length-aware constant-time comparison (mirrors flux-server; avoids leaking the token via timing).
/// Shared with the `connector` adapter, which authenticates the same bearer the same way.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[async_trait]
impl Channel for WebhookChannel {
    fn name(&self) -> &str {
        &self.name
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

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use axum::body::Body;
    use axum::http::Request;
    use flux_app::JourneyRun;
    use tower::ServiceExt; // for `oneshot`

    struct Nothing;

    #[async_trait]
    impl Deliverer for Nothing {
        async fn deliver(&self, _label: &str, _payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
            Ok(Vec::new())
        }
    }

    /// Counts deliveries. A status assertion passes against a handler that rejects *and* delivers,
    /// which is the defect worth testing for — so every negative test here asserts the **count**.
    #[derive(Default)]
    struct Counting(AtomicUsize);

    #[async_trait]
    impl Deliverer for Counting {
        async fn deliver(&self, _label: &str, _payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }

    /// A verifier that accepts one exact byte string and records everything it was handed.
    ///
    /// The recording is the point: it is the only way to assert that what reached the verifier is
    /// what arrived on the wire, rather than a re-serialization that merely *means* the same thing.
    struct ExactBytes {
        expected: Vec<u8>,
        seen: Mutex<Vec<Vec<u8>>>,
    }

    impl ExactBytes {
        fn new(expected: &str) -> Arc<Self> {
            Arc::new(Self {
                expected: expected.as_bytes().to_vec(),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    impl SignatureVerifier for ExactBytes {
        fn verify(&self, _headers: &HeaderMap, body: &[u8]) -> bool {
            self.seen.lock().expect("no poison").push(body.to_vec());
            body == self.expected.as_slice()
        }
    }

    /// A channel built **without** [`WebhookChannel::from_decl`], so the request path can be exercised
    /// with a token the constructor refuses, and with a [`Verification::Scheme`] no shipped
    /// [`verifier_for`] can yet produce. That is the whole point of testing the two halves apart:
    /// once the constructor makes `Some("")` unreachable, a channel routed through `from_decl` can
    /// never reach the comparison with one, and the comparison would go untested forever.
    fn channel(token: Option<&str>) -> WebhookChannel {
        channel_at("127.0.0.1:0", token, Verification::Unstated)
    }

    fn channel_at(addr: &str, token: Option<&str>, verify: Verification) -> WebhookChannel {
        WebhookChannel {
            name: "hook".to_string(),
            addr: SocketAddr::from_str(addr).expect("an addr"),
            path: "/hook".to_string(),
            is_async: false,
            token: token.map(str::to_string),
            verify,
        }
    }

    async fn post(token: Option<&str>, bearer: Option<&str>) -> StatusCode {
        let mut req = Request::post("/hook").header("content-type", "application/json");
        if let Some(bearer) = bearer {
            req = req.header(axum::http::header::AUTHORIZATION, bearer);
        }
        channel(token)
            .router(Arc::new(Nothing))
            .oneshot(req.body(Body::from("{}")).expect("a request"))
            .await
            .expect("the router answers")
            .status()
    }

    /// POST `raw` to `channel` over `deliverer`, returning the response.
    async fn post_raw(
        channel: WebhookChannel,
        deliverer: Arc<dyn Deliverer>,
        raw: &str,
        bearer: Option<&str>,
    ) -> Response {
        let mut req = Request::post("/hook").header("content-type", "application/json");
        if let Some(bearer) = bearer {
            req = req.header(axum::http::header::AUTHORIZATION, bearer);
        }
        channel
            .router(deliverer)
            .oneshot(req.body(Body::from(raw.to_string())).expect("a request"))
            .await
            .expect("the router answers")
    }

    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("a bounded body");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// **A request carrying no `Authorization` header is rejected by a channel whose token is empty.**
    ///
    /// The failure this pins is an identity, not a typo: the presented token is `""` when the header
    /// is absent, so a bare `constant_time_eq(b"", b"")` is `true` — equal lengths, an empty loop.
    /// An empty expected token would admit every anonymous caller while the channel reads, everywhere
    /// it is printed or logged, as "token-protected", on a host that auto-approves tools.
    ///
    /// [`WebhookChannel::from_decl`] refuses an empty token before a port is bound, and that is the
    /// half that prevents the exposure. This is the same rule stated where the comparison happens, so
    /// a future path reaching the handler without that constructor cannot reopen the hole.
    #[tokio::test]
    async fn a_request_with_no_authorization_header_is_rejected_by_an_empty_token_channel() {
        assert_eq!(
            post(Some(""), None).await,
            StatusCode::UNAUTHORIZED,
            "an empty expected token must never authorize an anonymous request"
        );
        assert_eq!(
            post(Some(""), Some("Bearer ")).await,
            StatusCode::UNAUTHORIZED,
            "nor an empty bearer spelled out"
        );

        // The rules either side of it, so the fix cannot have been "refuse everything".
        assert_eq!(
            post(None, None).await,
            StatusCode::OK,
            "no expected token is a loopback channel with nothing to check"
        );
        assert_eq!(post(Some("t0ken"), None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            post(Some("t0ken"), Some("Bearer nope")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(Some("t0ken"), Some("Bearer t0ken")).await,
            StatusCode::OK
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────────────────────
    // C-291 — the raw body reaches the verifier, and nothing is delivered when it does not verify
    // ─────────────────────────────────────────────────────────────────────────────────────────────

    /// A body whose bytes a JSON round trip would change, in three ways at once: keys out of sorted
    /// order, a **duplicate** key (a parse keeps only the last), and whitespace no serializer emits.
    const UNROUNDTRIPPABLE: &str = "{\"b\": 1,\n  \"a\": 2,   \"a\": 3 }";

    /// **What the verifier is handed is the bytes that arrived, not a re-encoding of their meaning.**
    ///
    /// A signature is over bytes. A verifier fed `serde_json::to_vec(&parsed)` would be checking a
    /// document the sender never sent — one whose keys have been sorted, whose duplicate key has been
    /// dropped, and whose whitespace has been rewritten — so it would reject every genuine delivery
    /// from a vendor whose serializer disagrees with ours, and, far worse, it would accept a forgery
    /// that merely *parses* to the same value.
    ///
    /// A test built on canonical JSON proves none of this: canonical JSON survives the round trip, so
    /// a normalize-then-verify bypass passes it. The premise below is asserted, not assumed.
    #[tokio::test]
    async fn verify_uses_raw_body_not_reserialized() {
        let parsed: Value = serde_json::from_str(UNROUNDTRIPPABLE).expect("valid JSON");
        let reserialized = serde_json::to_string(&parsed).expect("re-serializes");
        assert_ne!(
            reserialized, UNROUNDTRIPPABLE,
            "the premise of the whole test: this body must not survive a round trip"
        );

        let verifier = ExactBytes::new(UNROUNDTRIPPABLE);
        let delivered = Arc::new(Counting::default());
        let response = post_raw(
            channel_at("127.0.0.1:0", None, Verification::Scheme(verifier.clone())),
            delivered.clone(),
            UNROUNDTRIPPABLE,
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(delivered.0.load(Ordering::SeqCst), 1);

        let seen = verifier.seen.lock().expect("no poison");
        assert_eq!(seen.len(), 1, "the verifier ran exactly once");
        assert_eq!(
            String::from_utf8_lossy(&seen[0]),
            UNROUNDTRIPPABLE,
            "the verifier must see the bytes exactly as they arrived"
        );
        assert_ne!(
            String::from_utf8_lossy(&seen[0]),
            reserialized,
            "and specifically not the re-serialization, which is a different document"
        );
    }

    /// **A rejected signature delivers nothing.** The count, not the status: a handler that answers
    /// `401` *and* delivers passes a status assertion, and that is the defect worth testing for.
    ///
    /// The accepting case is asserted alongside so the guard cannot have been "refuse everything".
    #[tokio::test]
    async fn bad_signature_delivers_nothing() {
        let delivered = Arc::new(Counting::default());
        let response = post_raw(
            channel_at(
                "127.0.0.1:0",
                None,
                Verification::Scheme(ExactBytes::new("{\"signed\":true}")),
            ),
            delivered.clone(),
            "{\"signed\":false}",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            delivered.0.load(Ordering::SeqCst),
            0,
            "an unverified body must reach no journey at all"
        );

        let delivered = Arc::new(Counting::default());
        let response = post_raw(
            channel_at(
                "127.0.0.1:0",
                None,
                Verification::Scheme(ExactBytes::new("{\"signed\":true}")),
            ),
            delivered.clone(),
            "{\"signed\":true}",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(delivered.0.load(Ordering::SeqCst), 1);
    }

    /// **Verification precedes the `async` branch, not the delivery inside it.**
    ///
    /// `async` replies `202 Accepted` and spawns. A failure discovered after that 202 can neither
    /// report itself nor stop the delivery it has already scheduled — so a check placed inside the
    /// spawned task would leave a forged payload running a journey while the forger reads a success.
    #[tokio::test]
    async fn bad_signature_delivers_nothing_in_async_mode() {
        let mut channel = channel_at(
            "127.0.0.1:0",
            None,
            Verification::Scheme(ExactBytes::new("{\"signed\":true}")),
        );
        channel.is_async = true;

        let delivered = Arc::new(Counting::default());
        let response = post_raw(channel, delivered.clone(), "{\"signed\":false}", None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // A spawned delivery would land after the response. Give it every chance to.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            delivered.0.load(Ordering::SeqCst),
            0,
            "nothing may be scheduled before the signature is checked"
        );

        let mut channel = channel_at(
            "127.0.0.1:0",
            None,
            Verification::Scheme(ExactBytes::new("{\"signed\":true}")),
        );
        channel.is_async = true;
        let delivered = Arc::new(Counting::default());
        let response = post_raw(channel, delivered.clone(), "{\"signed\":true}", None).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(delivered.0.load(Ordering::SeqCst), 1, "and a good one does");
    }

    /// **`token` and `verify` compose: declaring both means both must pass.** Neither is a way past
    /// the other, in either direction.
    #[tokio::test]
    async fn a_token_and_a_verify_scheme_must_both_pass() {
        const GOOD: &str = "{\"signed\":true}";
        let cases = [
            (Some("Bearer t0ken"), GOOD, StatusCode::OK, 1),
            (
                Some("Bearer t0ken"),
                "{\"signed\":false}",
                StatusCode::UNAUTHORIZED,
                0,
            ),
            (Some("Bearer nope"), GOOD, StatusCode::UNAUTHORIZED, 0),
            (None, GOOD, StatusCode::UNAUTHORIZED, 0),
        ];
        for (bearer, body, expected, deliveries) in cases {
            let delivered = Arc::new(Counting::default());
            let response = post_raw(
                channel_at(
                    "127.0.0.1:0",
                    Some("t0ken"),
                    Verification::Scheme(ExactBytes::new(GOOD)),
                ),
                delivered.clone(),
                body,
                bearer,
            )
            .await;
            assert_eq!(response.status(), expected, "{bearer:?} + {body}");
            assert_eq!(
                delivered.0.load(Ordering::SeqCst),
                deliveries,
                "{bearer:?} + {body}"
            );
        }
    }

    /// **Every failure mode answers with one fixed body, and nothing about the request's *content*
    /// is answered before it is authenticated.**
    ///
    /// A caller that can tell "wrong token" from "bad signature" from "unsupported media type" from
    /// "malformed JSON" has a probe for how far its forgery got. All four are `401 unauthorized`
    /// here; only a request that has already authenticated learns anything about its body.
    #[tokio::test]
    async fn every_authentication_failure_returns_one_fixed_body() {
        const GOOD: &str = "{\"signed\":true}";
        let probes: [(Option<&str>, &str); 4] = [
            (Some("Bearer nope"), GOOD),         // wrong bearer
            (Some("Bearer t0ken"), "{\"x\":1}"), // bad signature
            (Some("Bearer nope"), "{ not json"), // malformed, unauthenticated
            (None, GOOD),                        // no bearer at all
        ];
        for (bearer, body) in probes {
            let response = post_raw(
                channel_at(
                    "127.0.0.1:0",
                    Some("t0ken"),
                    Verification::Scheme(ExactBytes::new(GOOD)),
                ),
                Arc::new(Nothing),
                body,
                bearer,
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{bearer:?}");
            assert_eq!(body_text(response).await, UNAUTHORIZED_BODY, "{bearer:?}");
        }

        // Past authentication, the body's own defects report themselves again — the negotiation
        // `Json<Value>` used to do implicitly, reproduced explicitly and *after* the signature.
        let malformed = post_raw(
            channel_at("127.0.0.1:0", None, Verification::None),
            Arc::new(Nothing),
            "{ not json",
            None,
        )
        .await;
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

        let untyped = channel_at("127.0.0.1:0", None, Verification::None)
            .router(Arc::new(Nothing))
            .oneshot(
                Request::post("/hook")
                    .body(Body::from("{}"))
                    .expect("a request"),
            )
            .await
            .expect("the router answers");
        assert_eq!(untyped.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// **The second refusal site: a channel nothing authenticates serves nothing off loopback.**
    ///
    /// [`WebhookChannel::from_decl`] refuses to construct one, and that is the half that prevents the
    /// exposure. This is the half that survives a future path reaching the handler without that
    /// constructor — which is exactly the path this test takes, since `from_decl` cannot produce the
    /// channels below. Neither half depends on the other being right.
    ///
    /// Note the shape of the property: `verify "none"` is a *statement*, not authentication, and an
    /// empty token is not a token however it was spelled.
    #[tokio::test]
    async fn an_effectively_open_non_loopback_channel_refuses_every_request() {
        for token in [None, Some(""), Some("   ")] {
            for verify in [Verification::Unstated, Verification::None] {
                let delivered = Arc::new(Counting::default());
                let response = post_raw(
                    channel_at("0.0.0.0:8790", token, verify),
                    delivered.clone(),
                    "{}",
                    None,
                )
                .await;
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{token:?}");
                assert_eq!(delivered.0.load(Ordering::SeqCst), 0, "{token:?}");
            }
        }

        // The rules either side of it, so the guard cannot have been "refuse everything public".
        let delivered = Arc::new(Counting::default());
        let response = post_raw(
            channel_at("0.0.0.0:8790", Some("t0ken"), Verification::None),
            delivered.clone(),
            "{}",
            Some("Bearer t0ken"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "a token authenticates");
        assert_eq!(delivered.0.load(Ordering::SeqCst), 1);

        let delivered = Arc::new(Counting::default());
        let response = post_raw(
            channel_at(
                "0.0.0.0:8790",
                None,
                Verification::Scheme(ExactBytes::new("{}")),
            ),
            delivered.clone(),
            "{}",
            None,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "and so does a signature — a vendor that cannot send an `Authorization` header is the \
             whole point of this story"
        );
        assert_eq!(delivered.0.load(Ordering::SeqCst), 1);
    }

    /// The property both bind guards key on, stated directly — C-321's lesson: a guard keyed on a
    /// *variant* drifts from the property it means, and that drift was an RCE-class hole.
    #[test]
    fn effectively_open_is_a_property_not_a_variant() {
        for token in [None, Some(""), Some("  \t")] {
            assert!(
                is_effectively_open(token, &Verification::Unstated),
                "{token:?} authenticates nothing"
            );
            assert!(
                is_effectively_open(token, &Verification::None),
                "a stated `verify \"none\"` is a decision, not authentication"
            );
            assert!(
                !is_effectively_open(token, &Verification::Scheme(ExactBytes::new("{}"))),
                "a verifying scheme authenticates without a bearer"
            );
        }
        assert!(!is_effectively_open(Some("t0ken"), &Verification::Unstated));
    }

    /// `Unstated` and `None` behave identically at request time and are **not** the same fact. The
    /// declaration-level distinction is what the non-loopback bind rule keys on, and what C-295 needs
    /// visible to a flow.
    #[test]
    fn an_absent_verification_and_a_stated_none_are_different_facts() {
        assert!(!Verification::Unstated.is_stated());
        assert!(Verification::None.is_stated());
        assert!(!Verification::Unstated.is_verifying());
        assert!(!Verification::None.is_verifying());
    }

    /// **The secret never reaches a formatter.** `WebhookSettings` used to derive `Debug` while
    /// holding the resolved plaintext `token`; a `verify` record placed there would have inherited
    /// that derive and printed the HMAC key the first time anyone added a trace line.
    #[test]
    fn the_settings_debug_prints_neither_the_token_nor_the_signing_secret() {
        let settings: WebhookSettings = serde_json::from_value(json!({
            "addr": "127.0.0.1:0",
            "token": "t0ken-never-in-a-log",
            "verify": {
                "scheme": "hmac",
                "algorithm": "sha256",
                "encoding": "hex",
                "header": "X-Hub-Signature-256",
                "prefix": "sha256=",
                "signed": "{body}",
                "secret": "sup3r-signing-key",
            },
        }))
        .expect("the settings deserialize");

        let text = format!("{settings:?}");
        assert!(!text.contains("t0ken-never-in-a-log"), "{text}");
        assert!(!text.contains("sup3r-signing-key"), "{text}");
        assert!(text.contains("<redacted>"), "{text}");
        // Present-but-redacted stays observable, and the declaration stays readable — a Debug that
        // hid the whole record would just be un-debuggable rather than safe.
        assert!(text.contains("X-Hub-Signature-256"), "{text}");
        assert!(text.contains("sha256="), "{text}");
    }

    /// The content-type negotiation `Json<Value>` did implicitly, reproduced.
    #[test]
    fn json_content_type_accepts_what_the_extractor_accepted() {
        let typed = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                value.parse().expect("a header value"),
            );
            json_content_type(&headers)
        };
        assert!(typed("application/json"));
        assert!(typed("application/json; charset=utf-8"));
        assert!(typed("APPLICATION/JSON"));
        assert!(typed("application/cloudevents+json"));
        assert!(!typed("text/plain"));
        assert!(!typed("application/x-www-form-urlencoded"));
        assert!(!json_content_type(&HeaderMap::new()));
    }
}
