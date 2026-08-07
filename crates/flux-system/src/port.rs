//! The guarded-IO **port**: what it means to *be* a `System`, as traits.
//!
//! [`System`] is the native implementor — an OS process, real syscalls, an OS sandbox. This module
//! states the same guarded operations as capability ports so a *non-native* substrate can serve them
//! instead: a WebAssembly embedder that answers through host imports, a remote executor, or a test
//! double. Nothing here relaxes a guarantee; each trait method's contract is the contract of the
//! inherent `System` method it mirrors, and the native impls at the bottom of this file are pure
//! delegation.
//!
//! **This is not a second IO path.** A port implementation gains no new ability to open a file or
//! start a process — those remain whatever the implementor could already do, and for flux's own
//! execution that is still exactly one place: `System::build_command`. The port makes the *caller*
//! substitutable, not the guard.
//!
//! ## Narrow by design
//!
//! There is deliberately **no** god trait. The port is split by guarded resource, and a consumer
//! names only the traits it uses — materializing a `secret:env/KEY` credential reference needs
//! [`GuardedEnv`] and nothing else. Where a consumer genuinely spans families it declares its own
//! bundle (see `flux_plugin::PluginSystem`), which keeps the required surface visible at the consumer
//! rather than hidden behind a catch-all.
//!
//! ## What stays inherent on `System`, and why
//!
//! The port covers the operations a substrate can meaningfully *re-implement*. These stay inherent
//! because they are native-only by construction, and putting them on the port would let an
//! implementation claim a posture it cannot hold:
//!
//! - [`System::rerooted`] returns `Self` — a substrate re-roots by handing out a
//!   differently-configured port, not by cloning this one.
//! - [`System::workspace`] / [`System::sandbox`] expose native path resolution and the OS sandbox
//!   posture. A substrate with no filesystem and no bubblewrap has neither.
//! - `run_with_env_exempt` / `run_with_env_streamed*` are about *exempting a child from the OS
//!   sandbox*, which has no meaning where there is no OS sandbox.
//! - `spawn_interactive` / `spawn_debug_pipe` hand back a tty and a POSIX fd pair.
//!
//! The workspace-confined file surface **is** a port — [`GuardedWorkspaceFiles`] (C-395). C-269
//! deferred it on the stated grounds that every consumer held a concrete `System`, so a trait with
//! no call sites would be indirection without a seam. A second consumer of the substrate is exactly
//! the condition that expired that reasoning. What stays inherent from *that* family is the
//! atomic-replacement pair: `write_file_atomic` and `update_file_reserved` are contracts over
//! filesystem primitives (`O_EXCL` plus a same-directory `rename`) rather than over file contents,
//! and `update_file_reserved` takes a caller closure, which is not dyn-compatible at all.
//!
//! ## These traits are unsealed, and the gate on them stops at this repo
//!
//! Both facts are deliberate, and stated here because neither is obvious from the signatures.
//!
//! **Unsealed.** Any crate depending on published `codewandler-flux-system` can implement
//! `GuardedProcess`, `GuardedHostFiles`, `GuardedWorkspaceFiles` or `GuardedEnv`. That is the point
//! — an out-of-repo Wasm embedder serving these ports is the whole reason they exist. It is also not
//! an escalation: a downstream crate that wanted to run an unguarded process could always just call
//! `Command::new` itself. What these traits are is a *contract*, not a permission — implementing one
//! grants no ability, it only claims to uphold the guarantees documented on each method.
//!
//! **The gate is in-repo only, and it enumerates every port.** `flux-codegate`'s
//! `no_unreviewed_guarded_port_backend_outside_system` reports every production `impl` of
//! [`GuardedProcess`], [`GuardedHostFiles`], [`GuardedWorkspaceFiles`], [`GuardedNetwork`],
//! [`GuardedMetrics`], [`GuardedHttp`] and [`GuardedEnv`] — resolving renamed imports, and
//! excusing only `#[cfg(test)]` — so a second backend for one of those cannot appear inside flux
//! without a reviewed allowance. Its reach ends at this repo's two workspaces: it walks
//! `crates/*/src` and `plugins/*/src` and nothing else. It says nothing about downstream
//! implementors, and (like every AST scanner in that file, including the older `Command` gate it
//! mirrors) it does not see macro-generated impls or sources pulled in from outside `src/` via
//! `#[path]`.
//!
//! C-467 closed the former `GuardedWorkspaceFiles` blind spot, and C-435 adds the network family to
//! the same census in the commit that introduces it, as C-653 does for [`GuardedMetrics`] and C-652
//! for [`GuardedHttp`]. A new `Guarded*` trait fails the census until the reviewer explicitly
//! teaches the gate about its implementors.
//!
//! So: inside flux, "one guarded path starts every OS process" is mechanically enforced. Outside
//! flux, a consumer that implements these traits is taking responsibility for the guarantees itself.
//!
//! That is the *implementor's* question. The adjacent one — what a consumer that merely **links**
//! `flux-system`, native backend and all, does and does not inherit by doing so — is answered once
//! at the crate root, under "Binding `flux-system` without `flux-runtime`". Neither answer is
//! repeated here.
//!
//! ## Fail-closed defaults
//!
//! Optional port operations default to a denial, never to a weaker equivalent. Bringing a substrate
//! up therefore starts from "serves nothing", and each capability is added deliberately — the same
//! deny-by-default posture the plugin host takes with manifest grants.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub use flux_core::{Error, GuardedIoError, GuardedIoFailure, Result};

use crate::metrics::{MetricAnswer, MetricKind};
use crate::net::{
    BindExposure, DatagramEndpoint, DialTarget, InboundLimits, NetworkListener, NetworkStream,
    PrivateNetAllow,
};
use crate::secret_scope::{GuardedSecretTarget, InjectionSite, SecretAllowlist};
use crate::websocket::{GuardedWebSocketSession, WebSocketConnect};
use crate::{ManagedChild, OutputObserver, ProcessOutput, ScopedFileRead, System};
use std::net::SocketAddr;

/// The future a guarded port operation returns.
///
/// Boxed rather than `async fn` in a trait: the port has to be usable as `dyn` (a `SystemSource`
/// hands out one erased substrate and consumers store it in an `Arc`), and `async fn` in trait is not
/// yet dyn-compatible. Hand-rolling the box keeps `flux-system`'s dependency set at
/// `flux-core` + `tokio` + `url`, which matters for a crate the portable core has to compile.
pub type Guarded<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Immutable operator-visible identity of one execution substrate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstrateIdentity {
    /// Stable kind/name reported by the substrate (`native`, `remote`, `container`, ...).
    pub kind: String,
    /// Canonical workspace root as understood by that substrate.
    pub workspace: String,
    /// Human-readable physical confinement posture.
    pub confinement: String,
    /// Whether results are reports from another trust boundary rather than local observations.
    pub remotely_reported: bool,
}

/// Identify where effects land. Identity is immutable for a live execution-system handle.
pub trait ExecutionIdentity: Send + Sync {
    /// Snapshot the identity displayed and recorded for this substrate.
    fn substrate_identity(&self) -> SubstrateIdentity;
}

/// Read the process environment through the guarded boundary.
///
/// The narrowest port there is, and the one most consumers want alone: resolving a `secret:env/KEY`
/// reference is an environment read and nothing more. A substrate with no process environment returns
/// `None` for every key, which fails the credential closed.
pub trait GuardedEnv: Send + Sync {
    /// The value of `key` in the guarded environment, or `None` if unset.
    fn env(&self, key: &str) -> Option<String>;
}

/// Start OS processes through the one guarded path: **argv-only** (never a shell string),
/// workspace-pinned cwd, environment cleared to a minimal non-secret allow-list, captured output
/// byte-capped.
///
/// [`run_with_env`](Self::run_with_env) is the only required operation, and the defaults below reduce
/// to it exactly the way [`System`]'s own convenience methods reduce to one private confinement
/// helper — so a substrate implements one primitive and inherits the same delegation graph the native
/// backend has.
pub trait GuardedProcess: Send + Sync {
    /// Execute `argv` (no shell), additionally setting the caller-chosen `env` entries on top of the
    /// minimal allow-list. `env` is built by Rust callers only — model input never reaches it.
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput>;

    /// Execute `argv` with only the minimal allow-listed environment.
    fn run<'a>(&'a self, argv: &'a [String], timeout: Duration) -> Guarded<'a, ProcessOutput> {
        self.run_with_env(argv, &[], timeout)
    }

    /// [`run_with_env`](Self::run_with_env) with a live line observer.
    ///
    /// The observer is a *view onto* the captured output, never a second channel, so the default —
    /// run without it — returns a byte-identical result and loses only progress reporting. That is a
    /// safe degradation; a substrate that can stream lines overrides this.
    fn run_with_env_observed<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        let _ = observer;
        self.run_with_env(argv, env, timeout)
    }

    /// [`run`](Self::run) with a live line observer.
    fn run_observed<'a>(
        &'a self,
        argv: &'a [String],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        self.run_with_env_observed(argv, &[], timeout, observer)
    }

    /// Execute `argv`, feeding `stdin` to the child and then closing it.
    ///
    /// Fail-closed default: silently dropping the payload would let a `git apply -` report success
    /// over an empty patch, so a substrate that cannot write a child's stdin must refuse.
    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        let _ = (argv, stdin, timeout);
        Box::pin(async { Err(deny("feed a child process stdin")) })
    }

    /// Start a long-lived child and hand back its opaque live handle, for a process started in one
    /// call and queried or stopped in a later one. The future allows a remote substrate to mint the
    /// handle across a link; the returned [`ManagedChild`] contains no native-handle requirement.
    fn spawn_background<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
    ) -> Guarded<'a, ManagedChild> {
        let _ = (argv, env);
        Box::pin(async { Err(deny("host long-lived child processes")) })
    }
}

/// Open network connections through the one guarded egress policy.
///
/// The native implementation delegates to [`crate::net::dial_scoped`], which owns DNS resolution,
/// private-range checks and address pinning. Implementations must not derive a second hostname or
/// range policy; a non-native substrate enforces the same contract at its own physical boundary.
pub trait GuardedNetwork: Send + Sync {
    /// Resolve, guard, pin, and handshake a WebSocket on this execution substrate.
    ///
    /// The default is a refusal: a substrate must opt in explicitly, and may not silently fall
    /// back to a socket opened by the caller's local process.
    fn open_websocket_scoped<'a>(
        &'a self,
        connect: &'a WebSocketConnect,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, GuardedWebSocketSession> {
        let _ = (connect, allow);
        Box::pin(async { Err(deny("open a guarded WebSocket")) })
    }

    /// Guard and open `target`, returning an opaque connection rather than a native socket.
    fn dial_scoped<'a>(
        &'a self,
        target: &'a DialTarget,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, NetworkStream> {
        let _ = (target, allow);
        Box::pin(async { Err(deny("open a guarded network connection")) })
    }

    /// Bind a bounded stream listener. Non-loopback exposure requires an authenticated posture.
    fn bind_tcp<'a>(
        &'a self,
        addr: SocketAddr,
        exposure: BindExposure,
        limits: InboundLimits,
    ) -> Guarded<'a, NetworkListener> {
        let _ = (addr, exposure, limits);
        Box::pin(async { Err(deny("bind a guarded TCP listener")) })
    }

    /// Bind a bounded datagram endpoint. Sends from it remain subject to `allow` and the shared
    /// destination guard.
    fn bind_udp<'a>(
        &'a self,
        addr: SocketAddr,
        exposure: BindExposure,
        limits: InboundLimits,
        allow: PrivateNetAllow,
    ) -> Guarded<'a, DatagramEndpoint> {
        let _ = (addr, exposure, limits, allow);
        Box::pin(async { Err(deny("bind a guarded UDP endpoint")) })
    }
}

/// The `$secret` grants one guarded HTTP request carries, so the substrate that actually follows the
/// redirect chain can re-authorize them at **every hop** rather than only at the first (C-459).
///
/// This travels with the request instead of staying at the caller because the hop the credential
/// would leak on is the one the caller never named: a `Location` the server chose. Only the
/// substrate that follows it can refuse it. Values never appear here — a scope is names, sites and
/// grants.
#[derive(Debug, Clone, Default)]
pub struct HttpSecretScope {
    /// The operator's allowlist, with each entry's declared destination/principal/site axes.
    pub allowlist: SecretAllowlist,
    /// The allowlisted names this request actually carries, and where each one was placed.
    pub carried: Vec<(String, InjectionSite)>,
    /// The principal the turn runs as, for a grant that declares `by=`. `None` refuses such a grant.
    pub principal: Option<String>,
}

/// One resolved request-header value, carried so that its plaintext **cannot** reach a log, a
/// `Debug` line or a serializer by accident (C-674).
///
/// C-652's review found the gap this closes: [`HttpRequest`] derived `Debug` while `headers` held
/// already-resolved `$secret` values, and nothing structurally tied those values to
/// [`HttpSecretScope::carried`]. Neither leaked while the type stayed inside one process. Both
/// become real the moment the request crosses a frame, so the carriage is fixed here rather than by
/// remembering to be careful at each call site:
///
/// - **The value is private.** [`expose`](Self::expose) is the only way to read it, so every place
///   that puts a header on a wire is a place a reviewer can find by searching for one name.
/// - **`Debug` never prints a value** — not even a literal one. A wrapper that printed the values it
///   believed were plain would leak exactly when a caller mislabelled one, which is the failure this
///   exists to make unrepresentable.
/// - **There is no `Serialize`.** `flux-system` holds no serialization format at all (its dependency
///   set is `flux-core` + `tokio` + `url`), so a transport must encode this deliberately; it cannot
///   fall into a derived one.
/// - **A secret-bearing value names its secret**, which is the structural link `carried` lacked:
///   [`HttpRequest::carried_secrets`] reads the headers themselves, so a header-placed credential is
///   re-authorized at every redirect hop whether or not the caller also listed it.
#[derive(Clone)]
pub struct HeaderValue {
    value: String,
    /// The allowlisted `$secret` name this value materializes, if any. A name, never a value.
    secret: Option<String>,
}

impl HeaderValue {
    /// A header value the caller wrote literally — no `$secret` marker resolved into it.
    pub fn literal(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            secret: None,
        }
    }

    /// A header value that materializes the allowlisted secret `name`.
    ///
    /// Naming the secret is not bookkeeping: it is what makes the credential visible to per-hop
    /// re-authorization without a second list that can drift out of step with this one.
    pub fn secret(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            secret: Some(name.into()),
        }
    }

    /// The plaintext, for the one place that actually puts it on a wire.
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// The `$secret` name this value materializes, or `None` for a literal.
    pub fn secret_name(&self) -> Option<&str> {
        self.secret.as_deref()
    }
}

impl std::fmt::Debug for HeaderValue {
    /// Whether a value is a materialized secret and how long it is — never what it is.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.secret {
            Some(name) => write!(f, "<secret `{name}`, {} bytes>", self.value.len()),
            None => write!(f, "<header value, {} bytes>", self.value.len()),
        }
    }
}

/// One private-destination admission recorded while a guarded HTTP request was served.
///
/// The event exists already — `flux_plugin::EgressAudit`'s `PrivateNetAdmit` — but it is emitted by
/// whichever backend actually followed the hop, and once that backend is on another machine the
/// caller's audit trail would otherwise fall silent about an admission that really happened
/// (C-674). So the substrate *reports* its admits with the response and the caller records them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateAdmit {
    /// The private/internal host the guard admitted.
    pub host: String,
    /// The grant that let it through, as the admitting substrate names it.
    pub grant_source: String,
    /// The substrate that admitted it, when that was **not this process** — the provenance stamp,
    /// set by the hop that crossed a trust boundary, exactly as `remotely_reported` is for a metric
    /// reading. A far side that could set this itself could claim a local admission.
    pub substrate: Option<String>,
}

/// Cap on the response headers a substrate's answer may carry across a hop.
pub const MAX_RESPONSE_HEADERS: usize = 128;
/// Cap on one response header's name or value, in bytes.
pub const MAX_HEADER_TEXT_BYTES: usize = 8 * 1024;
/// Cap on the private-destination admissions one answer may report.
pub const MAX_PRIVATE_ADMITS: usize = 32;
/// Cap on an admit's host or grant-source label, in bytes — a hostname's own maximum.
pub const MAX_ADMIT_LABEL_BYTES: usize = 253;

/// Bound one operator-facing label from a substrate's answer, mirroring
/// [`crate::metrics::bounded_label`]: trimmed, control characters removed, length capped.
///
/// Control characters matter more than length here — an admit's host lands in an audit record an
/// operator reads in a terminal, and a reporter that could embed an escape sequence could rewrite
/// what that terminal shows.
pub fn bounded_admit_label(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_ADMIT_LABEL_BYTES)
        .collect()
}

/// Re-impose every bound a guarded HTTP answer is supposed to honour, on an answer that came from
/// somewhere this process cannot vouch for.
///
/// The caps on [`HttpResponse`] are the *substrate's* promise, and a promise is all they are until
/// the answer is a local observation. A delegate is implementable by anyone, so a caller that
/// re-caps here is not distrusting one transport — it is declining to extend trust to the answer
/// at all. `truncated` becomes true when this side had to cut, and stays true when the substrate
/// says it already did: both mean the caller is not holding the whole body.
pub fn bounded_response(mut response: HttpResponse, max_response_bytes: usize) -> HttpResponse {
    if response.body.len() > max_response_bytes {
        response.body.truncate(max_response_bytes);
        response.truncated = true;
    }
    response.headers.truncate(MAX_RESPONSE_HEADERS);
    for (name, value) in &mut response.headers {
        name.truncate(floor_char_boundary(name, MAX_HEADER_TEXT_BYTES));
        value.truncate(floor_char_boundary(value, MAX_HEADER_TEXT_BYTES));
    }
    response.admits.truncate(MAX_PRIVATE_ADMITS);
    for admit in &mut response.admits {
        admit.host = bounded_admit_label(&admit.host);
        admit.grant_source = bounded_admit_label(&admit.grant_source);
        admit.substrate = admit
            .substrate
            .as_deref()
            .map(bounded_admit_label)
            .filter(|substrate| !substrate.is_empty());
    }
    response
}

/// The largest index `<= max` that `String::truncate` will accept — truncating mid-codepoint panics,
/// and a hostile far side gets to choose where the codepoints fall.
fn floor_char_boundary(text: &str, max: usize) -> usize {
    if text.len() <= max {
        return text.len();
    }
    (0..=max)
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0)
}

/// One guarded HTTP request, stated so a **non-native** substrate can serve it.
///
/// The target is a [`GuardedSecretTarget`] rather than a bare string: it can only be minted by
/// `flux-system`'s own egress guard, and it keeps the admitted URL, the addresses that guard vetted,
/// and the destination token secret authorization was measured against as **one correlated value**.
/// A caller therefore cannot hand a substrate a URL it claims was admitted, and the substrate sends
/// to exactly what was vetted — the C-77 pin and the C-459 destination stay the same decision rather
/// than two resolutions that can disagree.
///
/// A substrate in *another* address space treats the pinned addresses as what they are — this
/// process's answer, not its own — and re-admits at its own boundary, where [`Self::secrets`] gives
/// it everything the local guard had.
///
/// `Debug` is safe to derive because no field can print a plaintext credential: header values are
/// [`HeaderValue`]s, the target redacts its query, and a scope is names and grants.
#[derive(Debug)]
pub struct HttpRequest {
    /// The operation whose effect this is (`http.request`, `web.fetch`). Diagnostics and audit
    /// records name it, so a refusal reads the way the op's own refusals read.
    pub operation: String,
    /// The HTTP method, upper-cased. Model input never reaches this un-validated: the caller parses
    /// it before the request is built.
    pub method: String,
    /// The admitted target: URL, vetted addresses, and destination token, correlated by the guard.
    pub target: GuardedSecretTarget,
    /// Request headers, already resolved — a `$secret` marker is a value by the time it is here, so
    /// the value travels in a [`HeaderValue`] that cannot be printed or serialized un-redacted.
    pub headers: Vec<(String, HeaderValue)>,
    /// The request body, if any.
    pub body: Option<Vec<u8>>,
    /// Wall-clock budget for the whole request, redirect chain included.
    pub timeout: Duration,
    /// Cap on the response bytes retained. A substrate stops buffering at the cap rather than
    /// reading a whole body and truncating afterwards.
    pub max_response_bytes: usize,
    /// The secret grants this request carries. Empty for a request that carries none.
    pub secrets: HttpSecretScope,
}

impl HttpRequest {
    /// Every secret this request actually carries, and where each one sits — the union of what the
    /// caller declared in [`HttpSecretScope::carried`] and what the headers themselves say.
    ///
    /// This is the structural link C-652's review found missing. `carried` is a caller-assembled
    /// list; the headers are the request. Deriving the header half from the headers means a
    /// credential that is physically present is re-authorized at every redirect hop even if the
    /// list forgot it — and a query-placed secret, which lives in the URL and appears in no header,
    /// still comes from `carried`, because that is the only place it can.
    pub fn carried_secrets(&self) -> Vec<(String, InjectionSite)> {
        let mut carried = self.secrets.carried.clone();
        for (_, value) in &self.headers {
            let Some(name) = value.secret_name() else {
                continue;
            };
            if !carried
                .iter()
                .any(|(known, site)| known == name && *site == InjectionSite::Header)
            {
                carried.push((name.to_string(), InjectionSite::Header));
            }
        }
        carried
    }
}

/// What a guarded HTTP request answered.
///
/// Deliberately not a `reqwest::Response`: a port type that named one client's response would make
/// that client part of the contract, and a substrate that is not this process has none. A non-2xx
/// status is *data* here, exactly as it is for the ops — the error arm is for a guard's refusal, a
/// broken link or an unserved family, never for a status code.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// The final response status.
    pub status: u16,
    /// Response headers in wire order, duplicates preserved. A value that is not valid header text
    /// is reported as `<binary>` rather than dropped, so the caller still sees the name.
    pub headers: Vec<(String, String)>,
    /// At most [`HttpRequest::max_response_bytes`] of the body, undecoded.
    pub body: Vec<u8>,
    /// Whether the body was cut at the cap.
    pub truncated: bool,
    /// The private/internal destinations the guard admitted while serving this request, in hop
    /// order (C-674).
    ///
    /// Reported rather than only logged, because the substrate that admits is not always the
    /// process whose audit trail an operator reads. A native backend records these on its own sink
    /// *and* reports them here; a caller across a hop records what it is told, stamped with the
    /// substrate it came from. Empty for a request that reached nothing private.
    pub admits: Vec<PrivateAdmit>,
}

/// Make HTTP requests through the one guarded egress policy — the *application*-level counterpart to
/// [`GuardedNetwork`], and the family that lets a web effect follow the operator's selected substrate
/// instead of being pinned to the coordinator's process (Decision 0018 rule 5).
///
/// The implementation owns admission for every hop, the connection pin, the redirect bound, the
/// response-byte cap and the per-hop secret-scope re-authorization. It does **not** own redaction:
/// what a model may see is a turn-level decision the caller makes with its own `Redactor`, over the
/// bytes this returns.
///
/// # Fail-closed, and why this family especially
///
/// The single operation defaults to a refusal, so a substrate serves HTTP only by saying so. That
/// matters more here than anywhere else on the port: the tempting default — send it from the
/// caller's own process — is not a degraded answer but a *wrong* one. It would put the request, the
/// source address the far end sees, and any credential it carries on the coordinator while the
/// operator's provenance says the effect landed on the substrate they selected. A substrate that
/// cannot make HTTP requests says so and the operation refuses.
pub trait GuardedHttp: Send + Sync {
    /// Whether this substrate serves the family at all — deny-by-default, like every capability
    /// declaration on this port.
    ///
    /// It exists so a caller can answer "this substrate makes no HTTP requests" **without sending a
    /// request**, which matters once the substrate is across a link: a request sent to find out
    /// costs a round trip and hands back a failure the caller then has to interpret, where
    /// [`GuardedIoFailure::Unserved`] already means the one thing an operator can act on (implement
    /// it, or stop asking). Same shape, same reason, as
    /// [`GuardedMetrics::served_metric_kinds`].
    ///
    /// Answering `true` promises nothing about any individual request; it is the difference between
    /// a substrate that will try and one that structurally cannot.
    fn serves_http(&self) -> bool {
        false
    }

    /// Admit, pin, send and read back one HTTP request on this execution substrate, following at
    /// most a bounded redirect chain and re-admitting every hop under `allow`.
    fn http_request<'a>(
        &'a self,
        request: &'a HttpRequest,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, HttpResponse> {
        let _ = (request, allow);
        Box::pin(async { Err(deny("perform a guarded HTTP request")) })
    }
}

/// The HTTP backend a **composition site** handed to a native substrate (C-675) — the one field
/// behind [`System::with_http`], and the whole of how a selected native substrate comes to serve
/// [`GuardedHttp`].
///
/// # Why this exists at all
///
/// The workspace's one HTTP client is in `flux-web` (L5) and this crate is L2, so the native
/// implementation of this family cannot live here — C-652 settled that, and a `System` that serves
/// no HTTP is the honest consequence. It is *also* a gap the moment a substrate is selected: a
/// confined peer composing this same `System` then refuses web effects that an unselected run
/// performs, which is a capability the operator lost by asking for confinement rather than a
/// boundary anything needed.
///
/// The seam that closes it without moving the client is a value: the surface that already
/// constructs both halves — `flux-cli` builds the egress client *and* the substrate it installs —
/// joins them here. Dependency arrows still point downward, because what travels is an
/// implementation of **this crate's own trait**, reached through `dyn`. `flux-system` gains no
/// dependency, constructs no client, and holds no ambient default: an unattached value is exactly
/// the fail-closed substrate C-652 shipped.
///
/// # What it is not
///
/// It is not an IO seam. Attaching does not let a substrate open a socket it could not open —
/// whoever built the backend already could, and its guarantees (the shared egress guard, the
/// connection pin, the redirect bound, the per-hop secret re-authorization, the byte cap) are the
/// backend's own, unchanged by being reached through here. And it is not ambient: this is a field
/// on one value, cloned with it, never a process-global registry a later caller could consult or
/// replace.
#[derive(Clone, Default)]
pub(crate) struct AttachedHttp(Option<Arc<dyn GuardedHttp>>);

impl AttachedHttp {
    /// Attach `backend` as the HTTP implementation this substrate serves.
    pub(crate) fn new(backend: Arc<dyn GuardedHttp>) -> Self {
        Self(Some(backend))
    }

    /// The attached backend, or `None` where the composition site attached none.
    pub(crate) fn backend(&self) -> Option<&Arc<dyn GuardedHttp>> {
        self.0.as_ref()
    }
}

impl std::fmt::Debug for AttachedHttp {
    /// Whether a backend is attached, never what it is: a `dyn` backend has no useful `Debug`, and
    /// the security-relevant fact is the presence of one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AttachedHttp")
            .field(&self.0.is_some())
            .finish()
    }
}

/// Read files that legitimately live **outside** the workspace jail, admitted by an explicit path
/// scope rather than by workspace confinement.
///
/// This is the seam behind the plugin `fs.read` capability: a `~/.kube/config` is not
/// workspace-relative, so the guard is a declared exact / `/*` / `/**` scope instead. Both the scope
/// anchor and the requested path are reduced to physical identities before matching, so an in-scope
/// symlink spelling cannot name an out-of-scope target. Both operations are required: a substrate that
/// cannot reduce a path to an identity cannot be trusted to match a scope at all, and a fail-open
/// default here would be a security hole rather than a missing feature.
pub trait GuardedHostFiles: Send + Sync {
    /// Reduce a host path to its physical identity — every existing symlink followed, a
    /// not-yet-existing tail preserved. Neither confines to nor widens the workspace; matching the
    /// result against a grant is the caller's job.
    fn host_path_identity(&self, path: &str) -> Result<String>;

    /// Read a host file through an explicit `scope`, capped at `max_bytes`.
    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Guarded<'a, ScopedFileRead>;
}

/// Read and write files **inside the workspace jail** — the counterpart to [`GuardedHostFiles`],
/// which is for the files that legitimately live outside it.
///
/// Every path is resolved against the implementor's workspace before any open, and both escape
/// shapes are refused: a lexical `..` that normalizes out of the matched root, and a symlink whose
/// target canonicalizes outside it (including a *dangling* one, which a plain parent-canonicalize
/// misses on write).
///
/// # The asymmetry is part of the contract
///
/// Reads and writes are **not** confined to the same set. A read resolves against the primary root,
/// any `@named` root, **and** any configured read-only root; a write resolves against the primary
/// and `@named` roots only. That is the whole point of a read root, and it is why the two primitives
/// below are separate required methods rather than one `access: PathAccess` parameter with a
/// caller-chosen argument: an implementor cannot serve this port while collapsing the two, and a
/// consumer holding only the trait cannot ask for a write "as a read".
///
/// Both directions are required for the same reason both of [`GuardedHostFiles`]'s operations are:
/// a substrate that answers reads but silently inherits a generic denial for writes is
/// indistinguishable from one whose write guard was never wired, and the difference matters.
/// A genuinely read-only substrate says so in its own words, in its own `write_file_bytes`.
pub trait GuardedWorkspaceFiles: Send + Sync {
    /// The raw bytes of a workspace file — no UTF-8 decode, so a caller can sniff binary content
    /// and report byte sizes before a lossy text decode. Resolved on the **read** path, so a
    /// configured read-only root is reachable.
    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<u8>>;

    /// Write raw bytes to a workspace file, creating parent directories (also confined). Resolved
    /// on the **write** path, so a read-only root is *not* reachable.
    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Guarded<'a, ()>;

    /// A workspace file decoded as UTF-8.
    ///
    /// Reduces to [`read_file_bytes`](Self::read_file_bytes) exactly the way the native backend's
    /// own `read_file` does — same guard, same decode, same error — so the default is the native
    /// behaviour rather than an approximation of it.
    fn read_file<'a>(&'a self, path: &'a str) -> Guarded<'a, String> {
        Box::pin(async move {
            let bytes = self.read_file_bytes(path).await?;
            String::from_utf8(bytes).map_err(|_| Error::Other(format!("{path}: not valid UTF-8")))
        })
    }

    /// Write UTF-8 text to a workspace file, creating parent directories (also confined).
    ///
    /// Reduces to [`write_file_bytes`](Self::write_file_bytes): the native backend's `write_file`
    /// and `write_file_bytes` differ only in the type of the payload they hand to the same guarded
    /// write, so the reduction is byte-identical rather than merely equivalent.
    fn write_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        self.write_file_bytes(path, contents.as_bytes())
    }

    /// Append text to a workspace file, creating it (and parent directories) if absent.
    ///
    /// Fail-closed default. Read-then-write is *not* an append: it races every other writer and
    /// turns a crash mid-write into a truncation of content the caller never intended to touch. A
    /// substrate that cannot open a file for append refuses rather than emulating one.
    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        let _ = (path, contents);
        Box::pin(async { Err(deny("append to a workspace file")) })
    }

    /// At most `max` bytes of a **regular** workspace file, as `(bytes, truncated)`.
    ///
    /// Fail-closed default. Reducing this to [`read_file_bytes`](Self::read_file_bytes) and
    /// truncating afterwards would lose both guarantees that make it worth having — the memory bound
    /// on an attacker-influenced size, and the refusal of a FIFO or device that would otherwise
    /// stream forever — so the degradation is not safe and the operation denies instead.
    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Guarded<'a, (Vec<u8>, bool)> {
        let _ = (path, max);
        Box::pin(async { Err(deny("read a workspace file under a byte cap")) })
    }

    /// The byte size of a workspace file, as a metadata call.
    ///
    /// Fail-closed default: the point of asking is to skip an oversized file *without* reading it,
    /// so answering by reading it would invert the operation's reason to exist.
    fn file_size<'a>(&'a self, path: &'a str) -> Guarded<'a, u64> {
        let _ = path;
        Box::pin(async { Err(deny("stat a workspace file's size")) })
    }

    /// Whether a path exists inside the workspace or its read roots.
    ///
    /// Fail-closed default. `Ok(false)` would be the dangerous answer, not the neutral one: a
    /// caller asks this to decide whether it is creating or overwriting, and a substrate that
    /// cannot tell must not report "nothing there".
    fn path_exists<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        let _ = path;
        Box::pin(async { Err(deny("test whether a workspace path exists")) })
    }

    /// Whether a workspace path is a directory. Fail-closed for the same reason as
    /// [`path_exists`](Self::path_exists): a guessed `false` sends the caller down the file path.
    fn is_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        let _ = path;
        Box::pin(async { Err(deny("test whether a workspace path is a directory")) })
    }

    /// The last-modification time of a workspace file.
    ///
    /// Fail-closed default: this is what a read-before-write guard compares to detect a file that
    /// changed under the caller, so a fabricated timestamp would defeat the guard rather than
    /// degrade it.
    fn file_mtime<'a>(&'a self, path: &'a str) -> Guarded<'a, std::time::SystemTime> {
        let _ = path;
        Box::pin(async { Err(deny("read a workspace file's modification time")) })
    }

    /// The entry names of a workspace directory, sorted.
    ///
    /// Fail-closed default: an empty `Vec` is a *wrong answer* ("the directory is empty"), not a
    /// missing feature, and callers act on it.
    fn list_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<String>> {
        let _ = path;
        Box::pin(async { Err(deny("list a workspace directory")) })
    }

    /// Files under a workspace directory, recursively, capped at `max`. Symlinks are never
    /// followed. Fail-closed for the same reason as [`list_dir`](Self::list_dir).
    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Guarded<'a, Vec<String>> {
        let _ = (base, max);
        Box::pin(async { Err(deny("walk a workspace directory")) })
    }
}

/// Read bounded, typed measurements about the substrate **itself** — CPU, memory, disk, load,
/// uptime, temperature and fans — as opposed to about the work flowing through it.
///
/// The vocabulary is closed and lives in [`crate::metrics`]; read that module for why there is no
/// free-form string metric and what each unit is. What belongs *here* is the port's own posture,
/// and it turns on there being **two distinct negatives**, never collapsed into one:
///
/// - `Err(Unserved)` — this substrate does not serve metrics at all. Every operation below defaults
///   to it, so bringing a substrate up starts from "measures nothing" exactly as the rest of the
///   port does.
/// - `Ok(`[`MetricAnswer::Unavailable`]`)` — it serves the family and this machine has no such
///   instrument: no hwmon fan tachometer, no swap area, no mounted filesystem.
///
/// Neither is ever a zero reading. A fabricated zero is the failure mode this trait exists to make
/// unrepresentable: a monitoring projection cannot tell "0 °C" from "no thermometer", and an
/// operator acting on the first when the truth is the second is acting on a measurement that was
/// never taken.
pub trait GuardedMetrics: Send + Sync {
    /// The kinds this substrate can attempt at all.
    ///
    /// Deny-by-default: a substrate that has not opted in claims nothing, which is also what makes
    /// [`read_metrics`](Self::read_metrics) fail closed rather than hand back an empty snapshot.
    fn served_metric_kinds(&self) -> Vec<MetricKind> {
        Vec::new()
    }

    /// One metric kind, as a typed answer.
    ///
    /// Fail-closed default. There is no safe degradation: the whole point of the seam is that a
    /// consumer can trust the number, so a substrate that cannot measure says so.
    fn read_metric(&self, kind: MetricKind) -> Guarded<'_, MetricAnswer> {
        let _ = kind;
        Box::pin(async { Err(deny("read a host metric")) })
    }

    /// Every kind this substrate serves, once each, in [`MetricKind::ALL`] order.
    ///
    /// Bounded by construction rather than by a cap the caller has to remember: the result is a
    /// subset of a closed vocabulary, so it can never be longer than `MetricKind::ALL`. The
    /// reduction to [`read_metric`](Self::read_metric) is the same shape the file port's text
    /// operations use — serving one primitive earns the snapshot.
    fn read_metrics(&self) -> Guarded<'_, Vec<MetricAnswer>> {
        Box::pin(async move {
            let served = self.served_metric_kinds();
            if served.is_empty() {
                // Not an empty `Vec`: that is the wrong answer ("this machine measured nothing"),
                // and a caller would render it as a healthy host with no instruments.
                return Err(deny("read host metrics"));
            }
            let mut answers = Vec::with_capacity(served.len().min(MetricKind::ALL.len()));
            for kind in MetricKind::ALL {
                if served.contains(&kind) {
                    answers.push(self.read_metric(kind).await?);
                }
            }
            Ok(answers)
        })
    }
}

/// The complete execution-facing guarded substrate available to a tool turn.
///
/// This bundle is declared at the consumer boundary, following the same pattern as
/// `flux_plugin::PluginSystem`: the resource traits remain narrow and independently useful, while
/// a runtime that intentionally spans all current families can store one object-safe handle. It is
/// deliberately free of native-only workspace and sandbox accessors; those belong to [`System`]
/// and cannot be truthfully implemented by every remote or Wasm substrate.
pub trait ExecutionSystem:
    GuardedProcess
    + GuardedHostFiles
    + GuardedEnv
    + GuardedWorkspaceFiles
    + GuardedNetwork
    + GuardedMetrics
    + GuardedHttp
    + ExecutionIdentity
{
}

impl<T> ExecutionSystem for T where
    T: GuardedProcess
        + GuardedHostFiles
        + GuardedEnv
        + GuardedWorkspaceFiles
        + GuardedNetwork
        + GuardedMetrics
        + GuardedHttp
        + ExecutionIdentity
        + ?Sized
{
}

/// The one spelling of "the substrate does not serve this", as a prefix.
///
/// Public so diagnostics can quote the canonical spelling without copying it. Classification is
/// structural through [`GuardedIoFailure::Unserved`], never inferred from this text.
pub const UNSERVED: &str = GuardedIoFailure::Unserved.prefix();

/// The refusal an unserved optional port operation returns. One spelling, so a consumer can tell a
/// capability the substrate does not offer from a guard that rejected its path.
fn deny(operation: &str) -> Error {
    Error::GuardedIo(GuardedIoError::new(GuardedIoFailure::Unserved, operation))
}

// ---------------------------------------------------------------------------
// The native implementor
// ---------------------------------------------------------------------------
//
// Pure delegation to the inherent methods. Inherent methods win method resolution, so every existing
// `system.run(..)` call site on a concrete `System` still calls the inherent `async fn` and is
// unaffected by these impls.

impl GuardedEnv for System {
    fn env(&self, key: &str) -> Option<String> {
        System::env(self, key)
    }
}

impl ExecutionIdentity for System {
    fn substrate_identity(&self) -> SubstrateIdentity {
        SubstrateIdentity {
            kind: "native".into(),
            workspace: self.workspace().root().display().to_string(),
            confinement: self.sandbox().describe(),
            remotely_reported: false,
        }
    }
}

impl GuardedProcess for System {
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_env(self, argv, env, timeout))
    }

    fn run<'a>(&'a self, argv: &'a [String], timeout: Duration) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run(self, argv, timeout))
    }

    fn run_with_env_observed<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_env_observed(
            self, argv, env, timeout, observer,
        ))
    }

    fn run_observed<'a>(
        &'a self,
        argv: &'a [String],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_observed(self, argv, timeout, observer))
    }

    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_stdin(self, argv, stdin, timeout))
    }

    fn spawn_background<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
    ) -> Guarded<'a, ManagedChild> {
        Box::pin(async move { System::spawn_background(self, argv, env) })
    }
}

impl GuardedNetwork for System {
    fn open_websocket_scoped<'a>(
        &'a self,
        connect: &'a WebSocketConnect,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, GuardedWebSocketSession> {
        let _ = self;
        Box::pin(crate::websocket::open_native(connect, allow))
    }

    fn dial_scoped<'a>(
        &'a self,
        target: &'a DialTarget,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, NetworkStream> {
        let _ = self;
        Box::pin(async move {
            crate::net::dial_scoped(target, allow)
                .await
                .map(NetworkStream::from_handle)
        })
    }

    fn bind_tcp<'a>(
        &'a self,
        addr: SocketAddr,
        exposure: BindExposure,
        limits: InboundLimits,
    ) -> Guarded<'a, NetworkListener> {
        let _ = self;
        Box::pin(crate::net::bind_tcp(addr, exposure, limits))
    }

    fn bind_udp<'a>(
        &'a self,
        addr: SocketAddr,
        exposure: BindExposure,
        limits: InboundLimits,
        allow: PrivateNetAllow,
    ) -> Guarded<'a, DatagramEndpoint> {
        let _ = self;
        Box::pin(crate::net::bind_udp(addr, exposure, limits, allow))
    }
}

/// The native backend reads the local machine. Which kernel interfaces it reads is a value on the
/// `System` ([`crate::metrics::MetricsRoots`]), and the parsing lives in [`crate::metrics`] — this
/// file states the port, not the format of `/proc/meminfo`.
impl GuardedMetrics for System {
    fn served_metric_kinds(&self) -> Vec<MetricKind> {
        crate::metrics::native_served_kinds()
    }

    fn read_metric(&self, kind: MetricKind) -> Guarded<'_, MetricAnswer> {
        Box::pin(crate::metrics::read_native(self.metrics_roots(), kind))
    }
}

/// The native backend serves **no HTTP of its own**, and that is a decision rather than a gap. What
/// it serves is whatever backend a composition site attached to it ([`System::with_http`], C-675).
///
/// `flux-system` is a crate the portable core has to compile, and its dependency set is deliberately
/// `flux-core` + `tokio` + `url`. An HTTP client here would be the workspace's *second* one — the
/// first lives in `flux-web`, where it is already the reviewed broker that owns redirect handling,
/// connection pinning and the response-byte cap — and `flux-codegate`'s `Http` census exists
/// precisely to keep that count at one. So the native HTTP implementation is `flux_web::NativeHttp`,
/// a layer up, wrapping that same client.
///
/// Nothing about that changed; only *who may hold it* did. This method makes exactly one call, on a
/// backend it was handed, and constructs nothing — so the count of clients is still the census's,
/// and the guarantees on the wire are still the backend's own.
///
/// **Unattached is the default and stays fail-closed.** A `System` nobody attached a backend to
/// answers the port's own refusal, so a caller holding only a `System` gets "this substrate serves
/// no HTTP" rather than a request sent through some other path — the C-652 posture, unchanged for
/// every consumer that does not deliberately compose one.
impl GuardedHttp for System {
    /// Served exactly when a composition site attached a backend — the honest declaration, and the
    /// one a serving process announces in its protocol handshake (C-674).
    fn serves_http(&self) -> bool {
        self.attached_http().backend().is_some()
    }

    fn http_request<'a>(
        &'a self,
        request: &'a HttpRequest,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, HttpResponse> {
        match self.attached_http().backend() {
            Some(backend) => backend.http_request(request, allow),
            None => Box::pin(async { Err(deny("perform a guarded HTTP request")) }),
        }
    }
}

impl GuardedHostFiles for System {
    fn host_path_identity(&self, path: &str) -> Result<String> {
        System::host_path_identity(self, path)
    }

    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Guarded<'a, ScopedFileRead> {
        Box::pin(System::read_file_scoped(self, path, scope, max_bytes))
    }
}

// Every operation, including the two the trait would otherwise default, so the native backend
// answers each one through its own guarded method rather than through a reduction — the port and the
// struct are then the same code path, not merely the same outcome.
impl GuardedWorkspaceFiles for System {
    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<u8>> {
        Box::pin(System::read_file_bytes(self, path))
    }

    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Guarded<'a, ()> {
        Box::pin(System::write_file_bytes(self, path, contents))
    }

    fn read_file<'a>(&'a self, path: &'a str) -> Guarded<'a, String> {
        Box::pin(System::read_file(self, path))
    }

    fn write_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        Box::pin(System::write_file(self, path, contents))
    }

    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        Box::pin(System::append_file(self, path, contents))
    }

    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Guarded<'a, (Vec<u8>, bool)> {
        Box::pin(System::read_file_bytes_capped(self, path, max))
    }

    fn file_size<'a>(&'a self, path: &'a str) -> Guarded<'a, u64> {
        Box::pin(System::file_size(self, path))
    }

    fn path_exists<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        Box::pin(System::path_exists(self, path))
    }

    fn is_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        Box::pin(System::is_dir(self, path))
    }

    fn file_mtime<'a>(&'a self, path: &'a str) -> Guarded<'a, std::time::SystemTime> {
        Box::pin(System::file_mtime(self, path))
    }

    fn list_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<String>> {
        Box::pin(System::list_dir(self, path))
    }

    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Guarded<'a, Vec<String>> {
        Box::pin(System::walk_files(self, base, max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::DuplexStream;
    use crate::{sandbox, ChildStatus, ManagedProcess, Workspace};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn a_non_native_process_port_can_own_a_managed_child_lifecycle() {
        #[derive(Default)]
        struct State {
            reads: usize,
            stopped: bool,
        }

        struct Handle(Arc<std::sync::Mutex<State>>);
        impl ManagedProcess for Handle {
            fn read_output(&mut self) -> (String, String) {
                let mut state = self.0.lock().unwrap();
                state.reads += 1;
                ("remote output".into(), String::new())
            }

            fn status(&mut self) -> ChildStatus {
                ChildStatus {
                    running: !self.0.lock().unwrap().stopped,
                    exit_code: None,
                }
            }

            fn kill(&mut self) {
                self.0.lock().unwrap().stopped = true;
            }
        }

        struct NonNative(Arc<std::sync::Mutex<State>>);
        impl GuardedProcess for NonNative {
            fn run_with_env<'a>(
                &'a self,
                _argv: &'a [String],
                _env: &'a [(String, String)],
                _timeout: Duration,
            ) -> Guarded<'a, ProcessOutput> {
                Box::pin(async { Err(deny("run a foreground process")) })
            }

            fn spawn_background<'a>(
                &'a self,
                _argv: &'a [String],
                _env: &'a [(String, String)],
            ) -> Guarded<'a, ManagedChild> {
                let state = self.0.clone();
                Box::pin(async move { Ok(ManagedChild::from_handle(Handle(state))) })
            }
        }

        let state = Arc::new(std::sync::Mutex::new(State::default()));
        let port: &dyn GuardedProcess = &NonNative(state.clone());
        let mut child = port
            .spawn_background(&["remote-program".into()], &[])
            .await
            .unwrap();

        assert!(child.status().running);
        assert_eq!(child.read_output().0, "remote output");
        child.kill();
        assert!(!child.status().running);
        assert_eq!(state.lock().unwrap().reads, 1);
        drop(child);
        assert!(state.lock().unwrap().stopped);
    }

    #[tokio::test]
    async fn guarded_network_calls_are_substitutable_without_native_sockets() {
        struct MemoryReadHalf;
        impl crate::net::DuplexReadHalf for MemoryReadHalf {
            fn read<'a>(&'a mut self, max: usize) -> Guarded<'a, Vec<u8>> {
                Box::pin(async move { Ok(b"reply"[..max.min(5)].to_vec()) })
            }
        }

        struct MemoryWriteHalf(Arc<std::sync::Mutex<Vec<u8>>>);
        impl crate::net::DuplexWriteHalf for MemoryWriteHalf {
            fn write_all<'a>(&'a mut self, data: &'a [u8]) -> Guarded<'a, ()> {
                let written = self.0.clone();
                Box::pin(async move {
                    written.lock().unwrap().extend_from_slice(data);
                    Ok(())
                })
            }

            fn shutdown<'a>(&'a mut self) -> Guarded<'a, ()> {
                Box::pin(async { Ok(()) })
            }
        }

        struct MemoryStream(Arc<std::sync::Mutex<Vec<u8>>>);
        impl DuplexStream for MemoryStream {
            fn read<'a>(&'a mut self, max: usize) -> Guarded<'a, Vec<u8>> {
                Box::pin(async move { Ok(b"reply"[..max.min(5)].to_vec()) })
            }

            fn write_all<'a>(&'a mut self, data: &'a [u8]) -> Guarded<'a, ()> {
                let written = self.0.clone();
                Box::pin(async move {
                    written.lock().unwrap().extend_from_slice(data);
                    Ok(())
                })
            }

            fn shutdown<'a>(&'a mut self) -> Guarded<'a, ()> {
                Box::pin(async { Ok(()) })
            }

            fn split(
                self: Box<Self>,
            ) -> (
                Box<dyn crate::net::DuplexReadHalf>,
                Box<dyn crate::net::DuplexWriteHalf>,
            ) {
                (Box::new(MemoryReadHalf), Box::new(MemoryWriteHalf(self.0)))
            }
        }

        struct MemoryNetwork(Arc<std::sync::Mutex<Vec<u8>>>);
        impl GuardedNetwork for MemoryNetwork {
            fn dial_scoped<'a>(
                &'a self,
                _target: &'a DialTarget,
                _allow: &'a PrivateNetAllow,
            ) -> Guarded<'a, NetworkStream> {
                let written = self.0.clone();
                Box::pin(async move { Ok(NetworkStream::from_handle(MemoryStream(written))) })
            }
        }

        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let port: &dyn GuardedNetwork = &MemoryNetwork(written.clone());
        let mut stream = port
            .dial_scoped(
                &DialTarget::Tcp {
                    host: "example.test".into(),
                    port: 443,
                },
                &PrivateNetAllow::None,
            )
            .await
            .unwrap();
        stream.write_all(b"request").await.unwrap();

        assert_eq!(stream.read(5).await.unwrap(), b"reply");
        assert_eq!(&*written.lock().unwrap(), b"request");
    }

    /// The native backend reaches the guarded exec path *through the port*, not only through its
    /// inherent methods — so an erased `dyn GuardedProcess` is a usable substitute for `&System`.
    #[tokio::test]
    async fn the_native_system_serves_the_process_port_as_a_trait_object() {
        let system = System::new(Workspace::new(std::env::temp_dir()).unwrap());
        let port: &dyn GuardedProcess = &system;
        let argv = vec!["echo".to_string(), "port".to_string()];

        let out = port.run(&argv, Duration::from_secs(30)).await.unwrap();

        assert_eq!(out.exit_code, 0);
        assert_eq!(out.stdout.trim(), "port");
    }

    /// A substrate that implements only the required primitive still answers every derived operation,
    /// and the ones it cannot serve deny instead of degrading.
    #[tokio::test]
    async fn a_minimal_substrate_inherits_the_delegations_and_denies_the_rest() {
        struct Minimal;

        impl GuardedProcess for Minimal {
            fn run_with_env<'a>(
                &'a self,
                argv: &'a [String],
                env: &'a [(String, String)],
                _timeout: Duration,
            ) -> Guarded<'a, ProcessOutput> {
                let stdout = format!("{}|{}", argv.join(" "), env.len());
                Box::pin(async move {
                    Ok(ProcessOutput {
                        stdout,
                        stderr: String::new(),
                        exit_code: 0,
                    })
                })
            }
        }

        let argv = vec!["true".to_string()];
        let port: &dyn GuardedProcess = &Minimal;

        assert_eq!(
            port.run(&argv, Duration::from_secs(1))
                .await
                .unwrap()
                .stdout,
            "true|0",
            "`run` must reduce to the required `run_with_env` with an empty env"
        );

        let observer = std::sync::Arc::new(|_line: &str| {}) as OutputObserver;
        assert_eq!(
            port.run_observed(&argv, Duration::from_secs(1), observer)
                .await
                .unwrap()
                .stdout,
            "true|0",
            "the observer default must not change the captured result"
        );

        let stdin_error = port
            .run_with_stdin(&argv, b"patch", Duration::from_secs(1))
            .await
            .expect_err("stdin must fail closed, never silently drop the payload");
        assert!(stdin_error
            .to_string()
            .contains("cannot feed a child process"));

        // `ManagedChild` is not `Debug` (it owns a live child), so match rather than `expect_err`.
        let spawn_error = match port.spawn_background(&argv, &[]).await {
            Err(error) => error,
            Ok(_) => panic!("a long-lived native child cannot be fabricated"),
        };
        assert!(spawn_error
            .to_string()
            .contains("cannot host long-lived child processes"));
    }

    /// A workspace root, plus a sibling directory *outside* it holding `secret.txt`, plus a symlink
    /// inside the root pointing at that sibling. Returns the root, the outside directory, and the
    /// relative spelling that lexically escapes the root.
    fn escape_fixture(prefix: &str) -> (PathBuf, PathBuf, String, System) {
        let root = sandbox::fixture_dir(prefix);
        let outside = sandbox::fixture_dir(&format!("{prefix}-outside"));
        std::fs::write(outside.join("secret.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        // Both fixture roots are siblings under the temp dir, so `..` reaches one from the other.
        let lexical = format!(
            "../{}/secret.txt",
            outside.file_name().unwrap().to_string_lossy()
        );
        let system = System::new(Workspace::new(&root).unwrap());
        (root, outside, lexical, system)
    }

    /// C-395 — the workspace jail travels with the *port*, not only with the struct.
    ///
    /// A consumer holding nothing but `&dyn GuardedWorkspaceFiles` is refused both escape shapes
    /// that a concrete-`System` consumer is refused: a lexical `..` out of the root, and a symlink
    /// whose target canonicalizes outside it. Each refusal is checked twice — once through the
    /// trait and once through the struct — so the test fails if the port's confinement ever drifts
    /// from the inherent method's, in either direction. And a refusal is only a refusal if nothing
    /// happened: the outside directory is inspected afterwards.
    #[tokio::test]
    async fn the_file_port_refuses_the_escapes_the_concrete_system_refuses() {
        let (root, outside, lexical, system) = escape_fixture("port-file-escape");
        let port: &dyn GuardedWorkspaceFiles = &system;
        let through_link = "link/secret.txt";

        for path in [lexical.as_str(), through_link] {
            assert!(
                port.read_file(path).await.is_err(),
                "the port must refuse a read of {path:?}"
            );
            assert!(
                System::read_file(&system, path).await.is_err(),
                "the struct must refuse a read of {path:?} — the port is not stricter than it"
            );
            assert!(
                port.read_file_bytes(path).await.is_err(),
                "the port must refuse a byte read of {path:?}"
            );
            assert!(
                port.write_file(path, "owned").await.is_err(),
                "the port must refuse a write of {path:?}"
            );
            assert!(
                System::write_file(&system, path, "owned").await.is_err(),
                "the struct must refuse a write of {path:?}"
            );
            assert!(
                port.write_file_bytes(path, b"owned").await.is_err(),
                "the port must refuse a byte write of {path:?}"
            );
        }

        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "outside",
            "a refused write through the port still reached the outside file"
        );
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            1,
            "a refused write through the port created a file outside the workspace"
        );

        // The port is confined, not broken: an in-root path round-trips through it.
        port.write_file("inside.txt", "kept").await.unwrap();
        assert_eq!(port.read_file("inside.txt").await.unwrap(), "kept");

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// C-395 — the native backend answers *every* file operation through the port, including the
    /// optional ones. Without this the delegation could quietly lose an override and inherit the
    /// trait's denial, which would look like a working port right up until a consumer asked.
    #[tokio::test]
    async fn the_native_system_serves_every_file_operation_through_the_port() {
        let root = sandbox::fixture_dir("port-file-native");
        let system = System::new(Workspace::new(&root).unwrap());
        let port: &dyn GuardedWorkspaceFiles = &system;

        port.write_file("dir/a.txt", "hello").await.unwrap();
        port.append_file("dir/a.txt", " again").await.unwrap();

        assert_eq!(port.read_file("dir/a.txt").await.unwrap(), "hello again");
        assert_eq!(port.file_size("dir/a.txt").await.unwrap(), 11);
        assert_eq!(
            port.read_file_bytes_capped("dir/a.txt", 5).await.unwrap(),
            (b"hello".to_vec(), true)
        );
        assert!(port.path_exists("dir/a.txt").await.unwrap());
        assert!(!port.path_exists("dir/missing.txt").await.unwrap());
        assert!(port.is_dir("dir").await.unwrap());
        assert!(!port.is_dir("dir/a.txt").await.unwrap());
        port.file_mtime("dir/a.txt").await.unwrap();
        assert_eq!(port.list_dir("dir").await.unwrap(), vec!["a.txt"]);
        assert_eq!(port.walk_files(".", 100).await.unwrap(), vec!["dir/a.txt"]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-395 — the read/write asymmetry survives the port.
    ///
    /// A `read_root` (C-21) widens *reads* only. Through the trait, exactly as through the struct, a
    /// file under one is readable and the same directory is not writable — so a consumer that swaps
    /// a concrete `System` for the port cannot quietly gain a write it did not have.
    #[tokio::test]
    async fn read_roots_stay_readable_and_unwritable_through_the_file_port() {
        let root = sandbox::fixture_dir("port-file-readroot");
        let read_root = sandbox::fixture_dir("port-file-readroot-extra");
        std::fs::write(read_root.join("notes.md"), "readable").unwrap();

        let mut workspace = Workspace::new(&root).unwrap();
        workspace.add_read_root(&read_root).unwrap();
        let system = System::new(workspace);
        let port: &dyn GuardedWorkspaceFiles = &system;

        let readable = read_root.join("notes.md").to_string_lossy().into_owned();
        assert_eq!(
            port.read_file(&readable).await.unwrap(),
            "readable",
            "a read root must be readable through the port"
        );

        let unwritable = read_root.join("planted.md").to_string_lossy().into_owned();
        assert!(
            port.write_file(&unwritable, "planted").await.is_err(),
            "a read root must NOT be writable through the port"
        );
        assert!(
            System::write_file(&system, &unwritable, "planted")
                .await
                .is_err(),
            "a read root must NOT be writable through the struct either"
        );
        assert!(
            port.write_file(&readable, "overwritten").await.is_err(),
            "an existing read-root file must not be overwritable through the port"
        );
        assert_eq!(
            std::fs::read_to_string(read_root.join("notes.md")).unwrap(),
            "readable",
            "a refused overwrite through the port still modified the read-root file"
        );
        assert!(
            !read_root.join("planted.md").exists(),
            "a refused write through the port still created a file in the read root"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&read_root).ok();
    }

    /// One guarded HTTP request aimed at a closed loopback port, admitted under an `Any` grant.
    ///
    /// A literal address pins without a DNS lookup, so the fixture is hermetic and the tests below
    /// are about the *port's* answer rather than about a network.
    fn http_fixture(operation: &str) -> HttpRequest {
        let target = crate::net::guard_url_scoped_for_secret(
            "http://127.0.0.1:9/probe",
            &PrivateNetAllow::Any,
        )
        .expect("a loopback literal is admitted under an `Any` grant");
        HttpRequest {
            operation: operation.to_string(),
            method: "GET".into(),
            target,
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(1),
            max_response_bytes: 1024,
            secrets: HttpSecretScope::default(),
        }
    }

    /// C-674, acceptance 2 — no plaintext credential is `Debug`-printable, from any angle.
    ///
    /// C-652's review found the exposure while it was still harmless: the request derived `Debug`
    /// with resolved header values in it, and the target's `Debug` printed a URL that an `in=query`
    /// secret lives inside. Both stayed inside one process. This story makes the type cross a frame,
    /// so the test asserts what "by construction" has to mean — the value is absent from every
    /// rendering, whether a caller formats the whole request, one header, or the target.
    #[test]
    fn no_resolved_secret_survives_any_debug_rendering_of_a_request() {
        const TOKEN: &str = "ghp-super-secret-token-value";
        const QUERY_TOKEN: &str = "query-placed-secret-value";

        let target = crate::net::guard_url_scoped_for_secret(
            &format!("http://127.0.0.1:9/probe?api_key={QUERY_TOKEN}"),
            &PrivateNetAllow::Any,
        )
        .expect("a loopback literal is admitted under an `Any` grant");
        let request = HttpRequest {
            operation: "http.request".into(),
            method: "GET".into(),
            target,
            headers: vec![
                (
                    "authorization".into(),
                    HeaderValue::secret("GITHUB_TOKEN", TOKEN),
                ),
                (
                    "content-type".into(),
                    HeaderValue::literal("application/json"),
                ),
            ],
            body: None,
            timeout: Duration::from_secs(1),
            max_response_bytes: 1024,
            secrets: HttpSecretScope::default(),
        };

        for rendering in [
            format!("{request:?}"),
            format!("{:?}", request.headers),
            format!("{:?}", request.headers[0].1),
            format!("{:?}", request.target),
        ] {
            assert!(
                !rendering.contains(TOKEN),
                "a resolved header secret reached a Debug rendering: {rendering}"
            );
            assert!(
                !rendering.contains(QUERY_TOKEN),
                "a query-placed secret reached a Debug rendering: {rendering}"
            );
        }

        // The name is not a secret and stays visible — an operator has to be able to tell *which*
        // credential a refusal is about, and a rendering that hid that would be unreadable.
        assert!(format!("{request:?}").contains("GITHUB_TOKEN"));
        // A literal is redacted too. A wrapper that printed the values it believed were plain would
        // leak exactly when a caller mislabelled one, which is the failure this shape forecloses.
        assert!(!format!("{:?}", request.headers[1].1).contains("application/json"));
        // And the value is still reachable, at exactly one named door.
        assert_eq!(request.headers[0].1.expose(), TOKEN);
    }

    /// C-674, acceptance 2 — a header that carries a secret is re-authorized per hop because the
    /// header says so, not because a separate list remembered to.
    #[test]
    fn a_header_placed_secret_is_carried_whether_or_not_the_caller_listed_it() {
        let target =
            crate::net::guard_url_scoped_for_secret("http://127.0.0.1:9/p", &PrivateNetAllow::Any)
                .unwrap();
        let mut request = HttpRequest {
            operation: "http.request".into(),
            method: "GET".into(),
            target,
            headers: vec![(
                "authorization".into(),
                HeaderValue::secret("GITHUB_TOKEN", "value"),
            )],
            body: None,
            timeout: Duration::from_secs(1),
            max_response_bytes: 1024,
            // The list a caller assembles — here, empty, which is the drift the structural link
            // closes.
            secrets: HttpSecretScope::default(),
        };
        assert_eq!(
            request.carried_secrets(),
            vec![("GITHUB_TOKEN".to_string(), InjectionSite::Header)]
        );

        // A query-placed secret has no header to be read from, so it still comes from the list —
        // and declaring the header one as well does not duplicate it.
        request.secrets.carried = vec![
            ("QUERY_KEY".to_string(), InjectionSite::Query),
            ("GITHUB_TOKEN".to_string(), InjectionSite::Header),
        ];
        assert_eq!(
            request.carried_secrets(),
            vec![
                ("QUERY_KEY".to_string(), InjectionSite::Query),
                ("GITHUB_TOKEN".to_string(), InjectionSite::Header),
            ]
        );
    }

    /// C-652 — HTTP joins the port fail-closed.
    ///
    /// A substrate that says nothing about HTTP serves nothing. That is the whole posture: bringing
    /// a substrate up starts from "serves nothing", and an HTTP request is the operation where a
    /// well-meaning default would be actively dangerous — sending it from the *caller's* process
    /// while the operator selected another substrate puts the effect, its source address and its
    /// credentials somewhere the operator did not choose.
    #[tokio::test]
    async fn the_http_port_denies_for_a_substrate_that_serves_no_http() {
        struct Silent;
        impl GuardedHttp for Silent {}

        let port: &dyn GuardedHttp = &Silent;
        let error = port
            .http_request(&http_fixture("http.request"), &PrivateNetAllow::Any)
            .await
            .expect_err("HTTP must fail closed, never fall back to the caller's own client");

        assert!(
            matches!(&error, Error::GuardedIo(failure) if failure.kind() == GuardedIoFailure::Unserved),
            "the refusal must classify structurally as unserved: {error}"
        );
        assert!(
            error.to_string().starts_with(UNSERVED),
            "the refusal must use the port's one spelling: {error}"
        );
    }

    /// C-652 — the native `System` is such a substrate, deliberately.
    ///
    /// `flux-system`'s dependency set is `flux-core` + `tokio` + `url` by design, so the one reviewed
    /// HTTP client in the workspace lives a layer up in `flux-web`. The native backend therefore
    /// denies HTTP in the port's own words instead of growing a second client — and this test is what
    /// stops a later edit from "fixing" that by adding one here.
    ///
    /// C-675 attaches a backend built elsewhere ([`System::with_http`]) rather than building one, so
    /// this is still the answer for every `System` nobody composed one onto — which is every
    /// `System` this crate constructs.
    #[tokio::test]
    async fn the_native_system_denies_http_rather_than_growing_a_second_client() {
        let system = System::new(Workspace::new(std::env::temp_dir()).unwrap());
        let port: &dyn GuardedHttp = &system;

        let error = port
            .http_request(&http_fixture("web.fetch"), &PrivateNetAllow::Any)
            .await
            .expect_err("the native System serves no HTTP of its own");
        assert!(
            matches!(&error, Error::GuardedIo(failure) if failure.kind() == GuardedIoFailure::Unserved),
            "the native refusal must classify as unserved: {error}"
        );
    }

    /// One HTTP backend, of the kind a composition site attaches: it answers, and it records the
    /// operation it was asked for so a test can prove the call reached *it*.
    #[derive(Default)]
    struct AttachedBackend(std::sync::Mutex<Vec<String>>);

    impl GuardedHttp for AttachedBackend {
        fn http_request<'a>(
            &'a self,
            request: &'a HttpRequest,
            _allow: &'a PrivateNetAllow,
        ) -> Guarded<'a, HttpResponse> {
            self.0.lock().unwrap().push(request.operation.clone());
            Box::pin(async {
                Ok(HttpResponse {
                    status: 204,
                    headers: Vec::new(),
                    body: Vec::new(),
                    truncated: false,
                    admits: Vec::new(),
                })
            })
        }
    }

    /// C-675 — a `System` serves the HTTP backend a composition site attached to it, and only that.
    ///
    /// The whole seam is here: the surface that holds the workspace's one client hands it down as an
    /// implementation of this crate's own trait, and the native substrate serves that. Nothing is
    /// constructed here, nothing is ambient — the *same* system without the attachment is still the
    /// fail-closed one the test above pins, which is what makes this a composition rather than a
    /// default.
    #[tokio::test]
    async fn the_native_system_serves_the_http_backend_a_composition_site_attached() {
        let backend = Arc::new(AttachedBackend::default());
        let bare = System::new(Workspace::new(std::env::temp_dir()).unwrap());
        let composed = bare.clone().with_http(backend.clone());

        let served = GuardedHttp::http_request(
            &composed,
            &http_fixture("http.request"),
            &PrivateNetAllow::Any,
        )
        .await
        .expect("an attached backend is what this substrate serves");
        assert_eq!(served.status, 204);
        assert_eq!(
            backend.0.lock().unwrap().as_slice(),
            ["http.request".to_string()],
            "the call must reach the attached backend, not a client built here"
        );

        // The attachment travels with the value (a re-rooted system is the entered worktree, and the
        // backend is not workspace-relative), and never leaks onto a system nobody composed.
        let rerooted = composed.rerooted(std::env::temp_dir()).unwrap();
        assert!(
            GuardedHttp::http_request(&rerooted, &http_fixture("web.fetch"), &PrivateNetAllow::Any)
                .await
                .is_ok(),
            "a re-rooted system must still serve the backend it was composed with"
        );
        assert!(
            GuardedHttp::http_request(&bare, &http_fixture("web.fetch"), &PrivateNetAllow::Any)
                .await
                .is_err(),
            "attaching to one value must not reach another — this is a field, not an ambient default"
        );
    }

    /// C-652 — HTTP is *in* the bundle, so a consumer holding one erased substrate reaches it
    /// alongside the process, file, env and network families rather than through a side channel.
    #[tokio::test]
    async fn the_execution_system_bundle_spans_the_http_family() {
        let system = System::new(Workspace::new(std::env::temp_dir()).unwrap());
        let substrate: &dyn ExecutionSystem = &system;

        assert!(
            substrate
                .http_request(&http_fixture("http.request"), &PrivateNetAllow::Any)
                .await
                .is_err(),
            "`ExecutionSystem` must span `GuardedHttp` — a web effect cannot follow the selected \
             substrate if the bundle cannot express it"
        );
    }

    /// A substrate implementing only the two required primitives still answers the text operations,
    /// and every optional one denies rather than degrading to a weaker equivalent.
    #[tokio::test]
    async fn a_minimal_file_substrate_inherits_the_text_reductions_and_denies_the_rest() {
        #[derive(Default)]
        struct Memory(std::sync::Mutex<Vec<(String, Vec<u8>)>>);

        impl GuardedWorkspaceFiles for Memory {
            fn read_file_bytes<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<u8>> {
                Box::pin(async move {
                    self.0
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|(name, _)| name == path)
                        .map(|(_, bytes)| bytes.clone())
                        .ok_or_else(|| Error::Other(format!("{path}: not found")))
                })
            }

            fn write_file_bytes<'a>(
                &'a self,
                path: &'a str,
                contents: &'a [u8],
            ) -> Guarded<'a, ()> {
                Box::pin(async move {
                    self.0
                        .lock()
                        .unwrap()
                        .push((path.to_string(), contents.to_vec()));
                    Ok(())
                })
            }
        }

        let memory = Memory::default();
        let port: &dyn GuardedWorkspaceFiles = &memory;

        port.write_file("note.md", "text").await.unwrap();
        assert_eq!(
            port.read_file_bytes("note.md").await.unwrap(),
            b"text".to_vec(),
            "`write_file` must reduce to the required `write_file_bytes` byte-for-byte"
        );
        assert_eq!(
            port.read_file("note.md").await.unwrap(),
            "text",
            "`read_file` must reduce to the required `read_file_bytes`"
        );

        port.write_file_bytes("raw.bin", &[0xff, 0xfe])
            .await
            .unwrap();
        let decode_error = port
            .read_file("raw.bin")
            .await
            .expect_err("the text reduction must not decode non-UTF-8 lossily");
        assert!(decode_error.to_string().contains("not valid UTF-8"));

        // Every optional operation denies. `path_exists`/`is_dir` are the ones a well-meaning
        // default would answer `false` to, which is why they are asserted alongside the rest.
        for (label, result) in [
            ("append", port.append_file("note.md", "more").await.err()),
            (
                "capped read",
                port.read_file_bytes_capped("note.md", 2).await.err(),
            ),
            ("size", port.file_size("note.md").await.err()),
            ("exists", port.path_exists("note.md").await.err()),
            ("is_dir", port.is_dir("note.md").await.err()),
            ("mtime", port.file_mtime("note.md").await.err()),
            ("list", port.list_dir(".").await.err()),
            ("walk", port.walk_files(".", 10).await.err()),
        ] {
            let error = result.unwrap_or_else(|| {
                panic!("the optional `{label}` operation must fail closed, not degrade")
            });
            assert!(
                error
                    .to_string()
                    .starts_with("this guarded substrate cannot"),
                "`{label}` denied with an off-contract message: {error}"
            );
        }
    }
}
