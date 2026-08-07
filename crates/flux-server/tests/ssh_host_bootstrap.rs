//! C-683's release-boundary proof: the whole ssh chain against a **real** sshd.
//!
//! Bootstrap → forward → handshake → a guarded read that executes on the far side, plus the
//! refusal faces, run against an OpenSSH daemon this test starts itself. The daemon is entirely
//! scoped to a fixture directory — its own host key, its own `authorized_keys`, its own
//! `known_hosts`, a loopback-only high port — so nothing here reads or writes the operator's
//! `~/.ssh`, and `ssh -F none` means no config file of theirs is consulted either.
//!
//! Where no sshd is installed (many CI images ship the client only), every test dispositions
//! loudly and returns rather than passing vacuously: a green run that proved nothing is worse than
//! a skipped one that says so.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use flux_server::ssh::{
    admit_ssh_substrate, connect_ssh_system, SshAdmissionError, SshBootstrap, SshStartPolicy,
};
use flux_server::system::PROTOCOL_VERSION;
use flux_system::net::PrivateNetAllow;
use flux_system::port::{ExecutionIdentity, GuardedWorkspaceFiles};
use flux_system::ssh::{SshPlan, SshRefusal, SshTarget};
use flux_system::{System, Workspace};

const TOKEN: &str = "a-long-random-bearer-token-for-the-loopback-fixture";
const TOKEN_ENV: &str = "FLUX_REMOTE_SYSTEM_TOKEN";
const PLANTED: &str = "the far side read this";

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        // Bare word only: this directory becomes a *far-side* path, and the bootstrap refuses a
        // far-side word the login shell could re-interpret — parentheses from a `ThreadId` debug
        // included. The fixture has to obey the same rule an operator's declaration does.
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "flux-ssh-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .or_else(|| {
            [PathBuf::from("/usr/sbin"), PathBuf::from("/sbin")]
                .into_iter()
                .map(|dir| dir.join(program))
                .find(|candidate| candidate.is_file())
        })
}

/// Everything the chain needs, or an explanation of why it cannot run here.
struct Requirements {
    sshd: PathBuf,
    keygen: PathBuf,
    flux: PathBuf,
}

fn requirements(what: &str) -> Option<Requirements> {
    let disposition = |missing: &str| {
        eprintln!(
            "disposition [{what}]: {missing} is not available here, so the loopback-sshd chain \
             cannot be exercised on this machine. The chain is proved wherever OpenSSH's server \
             and a built `flux` binary are both present; the declaration and refusal faces that do \
             not need a daemon are covered in crates/flux-cli/tests/ssh_binding_faces.rs."
        );
        None::<Requirements>
    };
    let (Some(sshd), Some(keygen), Some(_ssh)) = (which("sshd"), which("ssh-keygen"), which("ssh"))
    else {
        return disposition("an OpenSSH server, ssh-keygen or ssh client");
    };
    // The far side runs the flux binary this workspace builds. `cargo test --workspace` builds it
    // beside this test binary; a targeted `-p codewandler-flux-server` run may not have.
    let flux = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent().map(|dir| dir.join("flux")))
        .filter(|path| path.is_file());
    let Some(flux) = flux else {
        return disposition("a built `flux` binary beside this test");
    };
    Some(Requirements { sshd, keygen, flux })
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// A scoped OpenSSH daemon: its own keys, its own `authorized_keys`, loopback only, no PAM, no
/// password face. Killed on drop.
struct Sshd {
    child: Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sshd {
    fn start(req: &Requirements, dir: &Path) -> Option<Self> {
        let keygen = |name: &str| {
            let path = dir.join(name);
            let status = Command::new(&req.keygen)
                .args([
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-C",
                    "flux-c683-fixture",
                    "-f",
                ])
                .arg(&path)
                .status()
                .expect("ssh-keygen runs");
            assert!(status.success(), "ssh-keygen failed for {name}");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            path
        };
        let host_key = keygen("host_key");
        keygen("id_client");

        let authorized = dir.join("authorized_keys");
        std::fs::copy(dir.join("id_client.pub"), &authorized).unwrap();
        std::fs::set_permissions(&authorized, std::fs::Permissions::from_mode(0o600)).unwrap();

        let port = free_port();
        let config = dir.join("sshd_config");
        std::fs::write(
            &config,
            format!(
                "Port {port}\n\
                 ListenAddress 127.0.0.1\n\
                 HostKey {host_key}\n\
                 AuthorizedKeysFile {authorized}\n\
                 PubkeyAuthentication yes\n\
                 PasswordAuthentication no\n\
                 KbdInteractiveAuthentication no\n\
                 UsePAM no\n\
                 StrictModes no\n\
                 AllowTcpForwarding yes\n\
                 PermitTTY no\n\
                 X11Forwarding no\n\
                 AcceptEnv {TOKEN_ENV}\n\
                 PidFile {pid}\n\
                 LogLevel ERROR\n",
                host_key = host_key.display(),
                authorized = authorized.display(),
                pid = dir.join("sshd.pid").display(),
            ),
        )
        .unwrap();

        let child = Command::new(&req.sshd)
            .arg("-D")
            .arg("-f")
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let mut daemon = Self {
            child,
            port,
            dir: dir.to_path_buf(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", daemon.port)).is_ok() {
                return Some(daemon);
            }
            if matches!(daemon.child.try_wait(), Ok(Some(_))) {
                eprintln!(
                    "disposition: this machine's sshd would not start unprivileged in a fixture \
                     directory, so the loopback chain cannot run here"
                );
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("disposition: the fixture sshd never accepted a connection");
        None
    }

    /// A `known_hosts` naming this daemon's real key — the record strict verification checks
    /// against.
    fn known_hosts(&self) -> PathBuf {
        let public = std::fs::read_to_string(self.dir.join("host_key.pub")).unwrap();
        let path = self.dir.join("known_hosts");
        std::fs::write(&path, format!("[127.0.0.1]:{} {public}", self.port)).unwrap();
        path
    }

    /// A `known_hosts` naming a *different* key, so verification must refuse.
    fn wrong_known_hosts(&self, req: &Requirements) -> PathBuf {
        let other = self.dir.join("other_host_key");
        let _ = std::fs::remove_file(&other);
        let _ = std::fs::remove_file(self.dir.join("other_host_key.pub"));
        Command::new(&req.keygen)
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&other)
            .status()
            .unwrap();
        let public = std::fs::read_to_string(self.dir.join("other_host_key.pub")).unwrap();
        let path = self.dir.join("wrong_known_hosts");
        std::fs::write(&path, format!("[127.0.0.1]:{} {public}", self.port)).unwrap();
        path
    }
}

/// A TLS certificate and key on disk whose SAN is `127.0.0.1` — the address the forward lands on.
fn loopback_tls(dir: &Path) -> (PathBuf, PathBuf, Vec<u8>) {
    let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .expect("a self-signed loopback certificate");
    let cert_pem = certified.cert.pem();
    let key_pem = certified.signing_key.serialize_pem();
    let cert = dir.join("tls.crt");
    let key = dir.join("tls.key");
    std::fs::write(&cert, &cert_pem).unwrap();
    std::fs::write(&key, &key_pem).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    (cert, key, cert_pem.into_bytes())
}

fn local_system(dir: &Path) -> System {
    System::new(Workspace::new(dir).expect("a local workspace"))
}

struct Fixture {
    _dir: TempDir,
    _sshd: Sshd,
    bootstrap: SshBootstrap,
    local: System,
    far_workspace: PathBuf,
}

/// The whole scoped world: a daemon, a key pair, a `known_hosts`, TLS material and a far-side
/// workspace with one planted file. `None` means this machine cannot host it, already explained.
fn fixture(tag: &str) -> Option<Fixture> {
    let req = requirements(tag)?;
    let dir = TempDir::new(tag);
    let sshd = Sshd::start(&req, dir.path())?;
    let (cert, key, ca_pem) = loopback_tls(dir.path());

    let far_workspace = dir.path().join("far-workspace");
    std::fs::create_dir_all(&far_workspace).unwrap();
    std::fs::write(far_workspace.join("planted.txt"), PLANTED).unwrap();

    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .expect("a login name for the loopback session");
    let plan = SshPlan {
        target: SshTarget::parse(&format!("ssh://{user}@127.0.0.1:{}", sshd.port))
            .expect("the fixture target parses"),
        key_path: dir.path().join("id_client").display().to_string(),
        binary: req.flux.display().to_string(),
        serve_port: free_port(),
        workspace: Some(far_workspace.display().to_string()),
        cert: Some(cert.display().to_string()),
        key: Some(key.display().to_string()),
        known_hosts: Some(sshd.known_hosts().display().to_string()),
        token_env: Some(TOKEN_ENV.to_string()),
        start_timeout: Duration::from_secs(60),
    };
    let local = local_system(dir.path());
    Some(Fixture {
        bootstrap: SshBootstrap {
            plan,
            server_name: "127.0.0.1".to_string(),
            ca_pem: Some(ca_pem),
        },
        local,
        far_workspace,
        _dir: dir,
        _sshd: sshd,
    })
}

fn loopback_grant() -> PrivateNetAllow {
    PrivateNetAllow::from_hosts(vec!["127.0.0.1".to_string()])
}

// ---------------------------------------------------------------------------
// The chain
// ---------------------------------------------------------------------------

/// Acceptance 4, the whole of it: bootstrap → forward → handshake → a guarded read that runs on the
/// far side. And acceptance 3's provenance, because the identity a caller sees must be the far
/// side's own, reported by it, with the version the two seats actually negotiated.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_ssh_binding_bootstraps_forwards_handshakes_and_serves_a_guarded_read() {
    let Some(fixture) = fixture("chain") else {
        return;
    };
    std::env::set_var(TOKEN_ENV, TOKEN);

    let system = connect_ssh_system(
        &fixture.local,
        &fixture.bootstrap,
        TOKEN.to_string(),
        &loopback_grant(),
    )
    .await
    .unwrap_or_else(|error| {
        panic!("the ssh binding did not bootstrap a served substrate: {error}")
    });

    // The identity is the far side's, reported by it. `remotely_reported` is what tells the
    // executor a selection is in force, which is what withholds `browser.*` / `web.crawl`: a
    // tunnelled substrate must never look native just because the socket is on loopback.
    let identity = system.substrate_identity();
    assert!(
        identity.remotely_reported,
        "the far side's readings are reports, not local observations: {identity:?}"
    );
    assert!(
        system.is_tethered(),
        "the substrate holds the forward it rides on"
    );
    assert_eq!(
        identity.workspace,
        fixture.far_workspace.display().to_string(),
        "the workspace reported is the far side's declared one"
    );

    // The guarded read executes over there: the file exists only in the far-side workspace, and it
    // arrives through the protocol's file port rather than any local path.
    let read = GuardedWorkspaceFiles::read_file(&system, "planted.txt")
        .await
        .expect("a guarded read served by the far side");
    assert_eq!(read, PLANTED);

    // …and the same port refuses an escape at the far side's boundary, not at ours.
    assert!(
        GuardedWorkspaceFiles::read_file(&system, "../outside.txt")
            .await
            .is_err(),
        "the far side enforces its own workspace jail"
    );
}

/// Acceptance 3: the probe's negotiated version is the protocol's own, read from the constant
/// rather than restated. A story that hardcoded `3` would go stale the first time the protocol
/// moves and would still pass — the deployment-artifact drift detectors exist for the same reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_probe_reports_the_negotiated_version_and_never_starts_a_far_side_serve() {
    let Some(fixture) = fixture("probe") else {
        return;
    };

    // Nothing is serving yet, and a probe is side-effect-free: it must say so rather than start
    // one. This is the distinction between verifying a binding and selecting it.
    let refused = admit_ssh_substrate(
        &fixture.local,
        &fixture.bootstrap,
        TOKEN.to_string(),
        &loopback_grant(),
        SshStartPolicy::AttachOnly,
    )
    .await
    .expect_err("a probe against a far side with nothing serving must refuse");
    match refused {
        SshAdmissionError::Bootstrap(SshRefusal::NotServing { detail, .. }) => assert!(
            detail.contains("never starts"),
            "the refusal explains the side-effect-free promise: {detail}"
        ),
        other => panic!("expected a `not serving` refusal, got {other}"),
    }

    // Now select the binding, which is the act that carries the intent to start one…
    let started = connect_ssh_system(
        &fixture.local,
        &fixture.bootstrap,
        TOKEN.to_string(),
        &loopback_grant(),
    )
    .await
    .unwrap_or_else(|error| panic!("selection did not start a far-side serve: {error}"));

    // …and now the same side-effect-free probe attaches and reports the negotiated version.
    let admitted = admit_ssh_substrate(
        &fixture.local,
        &fixture.bootstrap,
        TOKEN.to_string(),
        &loopback_grant(),
        SshStartPolicy::AttachOnly,
    )
    .await
    .unwrap_or_else(|error| panic!("a probe could not attach to the running serve: {error}"));
    assert!(!admitted.started_serve, "an attach starts nothing");
    assert_eq!(admitted.handshake.protocol_version, PROTOCOL_VERSION);
    assert!(admitted.handshake.identity().remotely_reported);
    drop(started);
}

/// The idempotency contract, exercised rather than asserted about: two local sessions bootstrap the
/// same binding at the same time. The far side's `--bind` is the mutex, so one of them loses the
/// bind and attaches to the other's serve — neither fails, and neither kills the other's substrate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_local_sessions_do_not_fight_over_one_far_side_serve() {
    let Some(fixture) = fixture("idempotent") else {
        return;
    };

    let grant = loopback_grant();
    let (first, second) = tokio::join!(
        connect_ssh_system(
            &fixture.local,
            &fixture.bootstrap,
            TOKEN.to_string(),
            &grant
        ),
        connect_ssh_system(
            &fixture.local,
            &fixture.bootstrap,
            TOKEN.to_string(),
            &grant
        ),
    );
    let first = first.unwrap_or_else(|e| panic!("the first session failed: {e}"));
    let second = second.unwrap_or_else(|e| panic!("the second session failed: {e}"));

    // Both hold a working substrate against the same far side, through their own tunnels.
    for (which, system) in [("first", &first), ("second", &second)] {
        let read = GuardedWorkspaceFiles::read_file(system, "planted.txt")
            .await
            .unwrap_or_else(|e| panic!("the {which} session could not read the far side: {e}"));
        assert_eq!(read, PLANTED);
    }

    // Dropping one session releases only its own tunnel; the other keeps working.
    drop(first);
    let read = GuardedWorkspaceFiles::read_file(&second, "planted.txt")
        .await
        .expect("the surviving session is unaffected by the other's release");
    assert_eq!(read, PLANTED);
}

/// Acceptance 4's refusal faces. A mismatched host key **refuses**; it does not prompt, because
/// there is nobody to prompt and a bootstrap that waited would hang an unattended surface forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_key_mismatch_refuses_rather_than_prompting() {
    let Some(mut fixture) = fixture("hostkey") else {
        return;
    };
    let req = requirements("hostkey").expect("already checked");
    fixture.bootstrap.plan.known_hosts =
        Some(fixture._sshd.wrong_known_hosts(&req).display().to_string());

    let started = Instant::now();
    let refused = connect_ssh_system(
        &fixture.local,
        &fixture.bootstrap,
        TOKEN.to_string(),
        &loopback_grant(),
    )
    .await
    .expect_err("a host key that is not the one on record must refuse");
    match &refused {
        SshAdmissionError::Bootstrap(SshRefusal::HostKeyMismatch { .. }) => {}
        other => panic!("expected a host-key refusal, got {other}"),
    }
    assert!(
        refused.to_string().contains("never prompted"),
        "the refusal says why there was no question to answer: {refused}"
    );
    // A prompt would have blocked until a timeout; a refusal is immediate.
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the bootstrap refused promptly rather than waiting on an answer nobody can give"
    );
}

/// The no-sshd face, against a port that is closed rather than filtered so the failure is a refusal
/// and not a hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_sshd_reachable_is_a_named_refusal_and_never_a_local_fallback() {
    let Some(mut fixture) = fixture("no-sshd") else {
        return;
    };
    fixture.bootstrap.plan.target = SshTarget::parse(&format!(
        "ssh://{}@127.0.0.1:{}",
        fixture.bootstrap.plan.target.user,
        free_port()
    ))
    .unwrap();

    let refused = connect_ssh_system(
        &fixture.local,
        &fixture.bootstrap,
        TOKEN.to_string(),
        &loopback_grant(),
    )
    .await
    .expect_err("no sshd means no substrate");
    match &refused {
        SshAdmissionError::Bootstrap(SshRefusal::Unreachable { .. }) => {}
        other => panic!("expected an unreachable refusal, got {other}"),
    }
}

/// A far side that speaks a different protocol version surfaces **the protocol's own refusal**,
/// unrestated. The far side here is a stand-in that announces a version this build cannot pair
/// with — the real daemon always agrees with itself, so the mismatch has to be staged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_far_side_version_mismatch_surfaces_the_protocols_own_refusal() {
    let Some(fixture) = fixture("version") else {
        return;
    };
    // The stand-in serves TLS itself, so it makes the same provider choice the product does.
    flux_server::system::ensure_crypto_provider();
    let announced = PROTOCOL_VERSION + 1;
    let peer = fixture.bootstrap.plan.serve_port;
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        std::fs::read(fixture.bootstrap.plan.cert.as_ref().unwrap()).unwrap(),
        std::fs::read(fixture.bootstrap.plan.key.as_ref().unwrap()).unwrap(),
    )
    .await
    .expect("the fixture certificate loads");
    let router = axum::Router::new().route(
        "/system/v1/handshake",
        axum::routing::get(move || async move {
            axum::Json(serde_json::json!({
                "protocol_version": announced,
                "substrate_kind": "native",
                "workspace": "/srv/flux",
                "confinement": "none",
                "operations": [],
                "metric_kinds": [],
            }))
        }),
    );
    let addr: SocketAddr = format!("127.0.0.1:{peer}").parse().unwrap();
    let peer_task = tokio::spawn(async move {
        let _ = axum_server::bind_rustls(addr, tls)
            .serve(router.into_make_service())
            .await;
    });
    // The stand-in is bound before the bootstrap looks, exactly as an already-serving far side is.
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(addr).is_err() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let refused = admit_ssh_substrate(
        &fixture.local,
        &fixture.bootstrap,
        TOKEN.to_string(),
        &loopback_grant(),
        SshStartPolicy::AttachOnly,
    )
    .await
    .expect_err("a mixed version pair must refuse to pair at all");
    match &refused {
        SshAdmissionError::Handshake(error) => {
            let said = error.to_string();
            // Verbatim: the protocol states its own refusal, naming both seats' versions. The
            // tunnel neither softens it nor restates it in a second vocabulary.
            assert!(
                said.contains("protocol mismatch")
                    && said.contains(&PROTOCOL_VERSION.to_string())
                    && said.contains(&announced.to_string()),
                "{said}"
            );
        }
        other => panic!("expected the protocol's own refusal, got {other}"),
    }
    peer_task.abort();
}

/// The bearer token still authenticates over the tunnel. A tunnelled link is not a trusted one:
/// the forward proves nothing about who is asking, and the protocol's own auth is what admits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tunnel_never_substitutes_for_the_protocols_bearer_auth() {
    let Some(fixture) = fixture("bearer") else {
        return;
    };
    let started = connect_ssh_system(
        &fixture.local,
        &fixture.bootstrap,
        TOKEN.to_string(),
        &loopback_grant(),
    )
    .await
    .unwrap_or_else(|error| panic!("the fixture serve did not come up: {error}"));

    // Same tunnel shape, same far side, a token the far side never issued.
    let refused = admit_ssh_substrate(
        &fixture.local,
        &fixture.bootstrap,
        "not-the-token-the-far-side-was-started-with".to_string(),
        &loopback_grant(),
        SshStartPolicy::AttachOnly,
    )
    .await
    .expect_err("the far side must refuse a bearer token it did not issue");
    match &refused {
        SshAdmissionError::Handshake(error) => assert!(
            error.to_string().contains("401") || error.to_string().contains("HTTP status"),
            "the far side's own auth refusal is what surfaces: {error}"
        ),
        other => panic!("expected an authentication refusal from the protocol, got {other}"),
    }
    drop(started);
}

/// Belt and braces for the fixture itself: the daemon really is scoped, so a green run above is not
/// quietly leaning on the operator's own ssh configuration.
#[test]
fn the_fixture_never_reads_or_writes_the_operators_ssh_directory() {
    let plan = SshPlan {
        target: SshTarget::parse("ssh://build@127.0.0.1:2222").unwrap(),
        key_path: "/fixture/id_client".to_string(),
        binary: "flux".to_string(),
        serve_port: 8790,
        workspace: None,
        cert: None,
        key: None,
        known_hosts: Some("/fixture/known_hosts".to_string()),
        token_env: None,
        start_timeout: Duration::from_secs(1),
    };
    let argv = plan.forward_argv(1234).join(" ");
    assert!(
        argv.contains("-F none"),
        "no ssh config file of the operator's is consulted: {argv}"
    );
    assert!(
        argv.contains("UserKnownHostsFile=/fixture/known_hosts"),
        "verification checks the fixture's record: {argv}"
    );
    assert!(!argv.contains(".ssh/"), "{argv}");
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "fixture argv: {argv}");
}

/// Keeps `Arc` in use for the shared-fixture shape above without an unused-import warning when the
/// disposition path is taken.
#[allow(dead_code)]
fn _shared(system: System) -> Arc<System> {
    Arc::new(system)
}
