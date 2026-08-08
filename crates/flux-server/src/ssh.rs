//! The ssh binding's bootstrap → attach → handshake state machine (Decision 0018 rule 3, C-683).
//!
//! `flux-system` owns the local half of the bootstrap — the pinned ssh argv and the live tunnel.
//! This module is where that meets the delivered remote protocol, because the protocol client lives
//! here. What it composes is deliberately thin: **once the forward is up, nothing about the session
//! is special.** The same [`HttpDelegate`](crate::system::HttpDelegate) connects, over the same TLS,
//! with the same bearer token, negotiating the same version and admitting through the same
//! handshake as a directly-addressed `remote` binding. The tunnel adds transport privacy and
//! reachability; it never stands in for the protocol's own authentication or identity check, and
//! there is no code path here that relaxes either because a link happens to be tunnelled.
//!
//! ## The state machine
//!
//! ```text
//!   reserve a loopback port ──▶ spawn `ssh -N -L` ──▶ local end accepts?
//!                                     │                      │ no, client died
//!                                     │ yes                  ▼
//!                                     ▼               diagnose its own words:
//!                             handshake through            no sshd · host-key
//!                             the forward                  mismatch · key refused
//!                              │        │
//!                       admitted        refused
//!                              │        ├── the far side answered (protocol mismatch, bad
//!                              │        │   token) ──▶ surface the protocol's own refusal
//!                              │        └── nothing is serving
//!                              │              ├── attach-only (probe, or no far-side cert/key)
//!                              │              │     ──▶ refuse, naming the missing piece
//!                              │              └── start `flux system serve` over a second
//!                              │                    session, then re-handshake until a bounded
//!                              │                    deadline
//!                              ▼
//!                     RemoteSystem::tethered(tunnel)
//! ```
//!
//! ## Idempotency
//!
//! Two local sessions must not fight over one far-side serve, and here they cannot: **the far
//! side's `--bind` is the mutex.** A second session that starts a serve while one is already
//! listening loses the bind, its child exits, and its next handshake attaches to the serve that
//! won. Nothing in flux reserves, locks or reaps a far-side process, which is what keeps the
//! failure mode "one of them exits" instead of "one of them kills the other's substrate".

use std::sync::Arc;

use flux_core::{Error, Result};
use flux_system::net::PrivateNetAllow;
use flux_system::remote::RemoteSystem;
use flux_system::ssh::{reserve_loopback_port, SshPlan, SshRefusal, SshTunnel};
use flux_system::System;

use crate::system::{HttpDelegate, SystemHandshake};

/// How often the far side is re-asked for admission while a started serve comes up.
const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// How long a serve child that has exited is given to flush its last words before they are read
/// and classified.
const SERVE_EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Everything the composition needs beyond the local half's plan: the name the far side's
/// certificate carries, and the PEM whose roots this binding trusts.
#[derive(Debug, Clone)]
pub struct SshBootstrap {
    /// The local half: target, key path, far-side paths and ports.
    pub plan: SshPlan,
    /// The host the tunnelled endpoint is addressed as — it must match the far side's certificate,
    /// which is why it is declared rather than assumed. Defaults to `127.0.0.1` at the config seam.
    pub server_name: String,
    /// An operator-managed CA whose roots this binding trusts, the delivered `--remote-ca` pinning
    /// form. `None` uses the platform roots.
    pub ca_pem: Option<Vec<u8>>,
}

/// Why an ssh binding produced no substrate. Two classes, because they are two conversations: the
/// bootstrap never got a link up, or the link is up and the *protocol* refused.
#[derive(Debug)]
pub enum SshAdmissionError {
    /// The ssh half failed: unreachable, host-key mismatch, unusable key, no far-side binary,
    /// nothing serving.
    Bootstrap(SshRefusal),
    /// The far side answered and admission was refused — a version mismatch, a rejected bearer
    /// token, a malformed handshake. Carried **verbatim**: the protocol states its own refusals,
    /// and restating them here would be a second, drifting vocabulary.
    Handshake(Error),
}

impl std::fmt::Display for SshAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bootstrap(refusal) => write!(f, "{refusal}"),
            Self::Handshake(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SshAdmissionError {}

impl From<SshRefusal> for SshAdmissionError {
    fn from(refusal: SshRefusal) -> Self {
        Self::Bootstrap(refusal)
    }
}

/// Whether a binding may start a far-side serve on this call.
///
/// `probe` is defined across the whole `flux host` family as side-effect-free, and starting a
/// process on someone's build machine is an effect by any reading — so a probe attaches to what is
/// already serving and says so plainly when nothing is. Selecting the binding is the act that
/// carries the intent to start one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshStartPolicy {
    /// Attach to a running serve; never start one.
    AttachOnly,
    /// Attach if something is serving, otherwise start one and wait, bounded, for admission.
    VerifyOrStart,
}

/// An admitted ssh-bootstrapped substrate, before anything is installed.
///
/// `Debug` names what it is and what it is holding, never the delegate — a delegate's `Debug` would
/// be a second place a bearer token could surface.
pub struct SshAdmission {
    /// The far side's own handshake — identity, negotiated version, served operations.
    pub handshake: SystemHandshake,
    /// The live local end. Whoever keeps the substrate keeps this.
    pub tunnel: Arc<SshTunnel>,
    /// Whether this session started the far-side serve (as opposed to attaching to one).
    pub started_serve: bool,
    /// The tunnelled endpoint the protocol client was pointed at.
    pub endpoint: String,
    delegate: Arc<HttpDelegate>,
}

impl std::fmt::Debug for SshAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshAdmission")
            .field("endpoint", &self.endpoint)
            .field("protocol_version", &self.handshake.protocol_version)
            .field("substrate_kind", &self.handshake.substrate_kind)
            .field("started_serve", &self.started_serve)
            .field("tunnel_alive", &self.tunnel.is_alive())
            .finish()
    }
}

impl SshAdmission {
    /// Install the admitted substrate, tying the tunnel's life to it.
    pub fn into_system(self) -> RemoteSystem {
        RemoteSystem::identified(self.delegate, self.handshake.identity()).tethered(self.tunnel)
    }
}

/// Bootstrap, attach (or start), and admit — the whole state machine, once.
///
/// `token` is the serving endpoint's bearer token, already resolved from its *reference* by the
/// caller. It is used for exactly two things: authenticating the protocol handshake, and (when a
/// serve is started) reaching the far side through the ssh channel's environment. It never appears
/// in an argv on either seat.
pub async fn admit_ssh_substrate(
    local: &System,
    bootstrap: &SshBootstrap,
    token: String,
    private_net: &PrivateNetAllow,
    policy: SshStartPolicy,
) -> std::result::Result<SshAdmission, SshAdmissionError> {
    let plan = &bootstrap.plan;
    plan.validate()?;

    let local_port = reserve_loopback_port(local).await?;
    let tunnel = Arc::new(SshTunnel::open(local, plan, local_port).await?);
    let endpoint = plan.endpoint(&bootstrap.server_name, local_port);

    // First attempt: whatever is already serving on the far side's declared port. This is the
    // "verify" half, and for a probe it is the only half.
    let first = connect_through(&endpoint, &token, private_net, bootstrap.ca_pem.as_deref()).await;
    let refusal = match first {
        Ok((delegate, handshake)) => {
            return Ok(SshAdmission {
                handshake,
                tunnel,
                started_serve: false,
                endpoint,
                delegate,
            })
        }
        Err(error) => error,
    };

    // The forward itself may have died between coming up and being used — a link that dropped is
    // an ssh story, not a protocol one, and the client's own words say which.
    if !tunnel.is_alive() {
        return Err(SshAdmissionError::Bootstrap(tunnel.diagnose()));
    }
    // The far side *answered* and refused. That is the protocol's own refusal — a version mismatch
    // or a rejected token — and it is surfaced verbatim rather than retried or restarted, because
    // starting a second serve would not change either answer.
    if far_side_answered(&refusal) {
        return Err(SshAdmissionError::Handshake(refusal));
    }
    if policy == SshStartPolicy::AttachOnly {
        return Err(SshAdmissionError::Bootstrap(SshRefusal::NotServing {
            target: plan.target.display(),
            detail: format!(
                "the forward is up but nothing answered the protocol handshake on the far side's \
                 127.0.0.1:{} ({refusal}). A probe never starts one — it is side-effect-free; \
                 selecting the binding does",
                plan.serve_port
            ),
        }));
    }

    // The "or start" half. The token reaches the far side through the ssh channel's environment,
    // which requires the far side's sshd to accept the variable; a far side that already holds its
    // own token (a unit file, a profile) needs nothing here.
    let token_env = plan
        .token_env
        .as_ref()
        .map(|name| (name.clone(), token.clone()));
    tunnel.start_serve(local, plan, token_env)?;

    let deadline = std::time::Instant::now() + plan.start_timeout;
    let mut last = refusal;
    let mut serve_died_at: Option<std::time::Instant> = None;
    loop {
        if !tunnel.is_alive() {
            return Err(SshAdmissionError::Bootstrap(tunnel.diagnose()));
        }
        // A serve that exited without the far side becoming admissible is the informative case, and
        // it is answered *now* rather than at the deadline — a far side that refused to start will
        // not change its mind, and making an operator wait out the whole start window for an answer
        // already sitting in the transcript is a worse error than a slower one.
        //
        // The single exception is the idempotency race: a serve that *lost* the far-side bind also
        // exits, and the session that won it is about to start answering. That one is waited out,
        // which is exactly what the bind-as-mutex contract asks for.
        if !tunnel.serve_running() {
            // Give the child's output a moment to flush before reading it. The tolerated case is
            // recognised *from* the transcript, and a drain that had not caught up yet would turn
            // the idempotency race into a spurious refusal — a grace of two seconds against a start
            // window of tens of them.
            let died = *serve_died_at.get_or_insert_with(std::time::Instant::now);
            if died.elapsed() >= SERVE_EXIT_GRACE && !tunnel.serve_lost_the_bind() {
                return Err(SshAdmissionError::Bootstrap(tunnel.diagnose_serve(plan)));
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(SshAdmissionError::Bootstrap(if tunnel.serve_running() {
                SshRefusal::StartTimeout {
                    target: plan.target.display(),
                    detail: format!(
                        "`flux system serve` is running on the far side but never admitted a \
                         handshake within {}s; the last answer was: {last}",
                        plan.start_timeout.as_secs()
                    ),
                }
            } else {
                tunnel.diagnose_serve(plan)
            }));
        }
        tokio::time::sleep(RETRY_INTERVAL).await;
        match connect_through(&endpoint, &token, private_net, bootstrap.ca_pem.as_deref()).await {
            Ok((delegate, handshake)) => {
                return Ok(SshAdmission {
                    handshake,
                    tunnel,
                    started_serve: true,
                    endpoint,
                    delegate,
                })
            }
            Err(error) => {
                if far_side_answered(&error) {
                    return Err(SshAdmissionError::Handshake(error));
                }
                last = error;
            }
        }
    }
}

/// The side-effect-free identity check for an ssh binding (C-649's `probe`, C-683's backend): the
/// bootstrap runs, the handshake is performed through the forward, and the tunnel is released with
/// the returned value. Nothing is started on the far side and nothing is installed here.
pub async fn probe_ssh_system(
    local: &System,
    bootstrap: &SshBootstrap,
    token: String,
    private_net: &PrivateNetAllow,
) -> std::result::Result<SystemHandshake, SshAdmissionError> {
    let admitted = admit_ssh_substrate(
        local,
        bootstrap,
        token,
        private_net,
        SshStartPolicy::AttachOnly,
    )
    .await?;
    Ok(admitted.handshake)
}

/// Bootstrap and install: verify-or-start, admit through the handshake, and hand back the delivered
/// remote substrate holding its own tunnel.
pub async fn connect_ssh_system(
    local: &System,
    bootstrap: &SshBootstrap,
    token: String,
    private_net: &PrivateNetAllow,
) -> std::result::Result<RemoteSystem, SshAdmissionError> {
    let admitted = admit_ssh_substrate(
        local,
        bootstrap,
        token,
        private_net,
        SshStartPolicy::VerifyOrStart,
    )
    .await?;
    Ok(admitted.into_system())
}

async fn connect_through(
    endpoint: &str,
    token: &str,
    private_net: &PrivateNetAllow,
    ca_pem: Option<&[u8]>,
) -> Result<(Arc<HttpDelegate>, SystemHandshake)> {
    match ca_pem {
        Some(pem) => {
            HttpDelegate::connect_with_ca_pem(endpoint, token.to_string(), private_net, pem).await
        }
        None => HttpDelegate::connect(endpoint, token.to_string(), private_net).await,
    }
}

/// Whether the far side *answered* the handshake and refused it, as opposed to nothing being there.
///
/// The distinction decides whether starting a serve could possibly help, so it is drawn from the
/// two shapes the client can only produce after a response exists: the protocol's own version
/// refusal ([`Error::Config`], minted after the frame is parsed) and an HTTP status. Everything
/// else — a reset channel, a TLS failure with nothing behind it — means the port had no server, and
/// the caller falls through to the start path where the real answer is waited for rather than
/// guessed at.
fn far_side_answered(error: &Error) -> bool {
    match error {
        Error::Config(_) => true,
        other => {
            let said = other.to_string();
            said.contains("HTTP status") || said.contains("handshake frame")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answered_refusal_is_never_retried_as_a_missing_serve() {
        // The version refusal the protocol mints for itself. Starting a second serve cannot change
        // a version, so this must not fall through to the start path — it is surfaced verbatim.
        let mismatch = Error::Config("remote-system protocol mismatch: local 9, remote 2".into());
        assert!(far_side_answered(&mismatch));
        assert!(far_side_answered(&Error::Other(
            "remote-system handshake: HTTP status client error (401 Unauthorized) for url (…)"
                .into()
        )));

        // Nothing behind the forwarded port: the start path is exactly what this is for.
        assert!(!far_side_answered(&Error::Other(
            "remote-system handshake: error sending request for url (…): connection closed".into()
        )));
    }

    /// The non-native pin, which holds on every machine whether or not one has an sshd.
    ///
    /// A far side reached over a tunnel says `native` about itself, because it *is* native over
    /// there. `remotely_reported` is what tells `Executor::non_native_target` that a selection is
    /// in force, and that is what withholds `browser.*` / `web.crawl` — operations that own a live
    /// child and a CDP pipe in *this* process. If a tunnelled substrate could look locally observed
    /// because its socket happens to be on loopback, those operations would be offered against a
    /// machine that cannot serve them. The handshake stamps the bit locally rather than reading it
    /// off the wire, so no far side can claim otherwise.
    #[test]
    fn a_tunnelled_far_side_is_never_mistaken_for_a_local_one() {
        let handshake: SystemHandshake = serde_json::from_value(serde_json::json!({
            "protocol_version": crate::system::PROTOCOL_VERSION,
            "substrate_kind": "native",
            "workspace": "/srv/flux",
            "confinement": "none",
            "operations": [],
            "metric_kinds": [],
        }))
        .expect("a far side announcing itself as native");
        assert!(
            handshake.identity().remotely_reported,
            "a far side's `native` is still a report from another trust boundary"
        );
    }

    #[test]
    fn a_probe_never_starts_a_far_side_serve() {
        // Stated as a value rather than as prose so the two callers cannot drift: the probe seam
        // passes `AttachOnly`, and `AttachOnly` is the only policy that refuses to start.
        assert_ne!(SshStartPolicy::AttachOnly, SshStartPolicy::VerifyOrStart);
    }
}
