//! C-480 — the shipped container deployment profile, exercised end to end.
//!
//! The daemon has always been able to run in a container; what was missing is a *checked* image
//! contract. This suite builds the committed `deploy/container/Dockerfile`, runs it the way the
//! published profile says to run it (workspace volume, TLS volume, token through the environment,
//! non-root identity, nothing secret in a layer), drives it with the same client entry point
//! `flux tui --remote` uses, restarts the container, and proves the canonical workspace *and* the
//! bounded delivery ledger survived.
//!
//! **Disposition.** Docker is not available in ordinary workspace CI, so this suite is opt-in and
//! skips loudly when `FLUX_TEST_CONTAINER` is unset — the `sandbox_backend.rs` rule: the capability
//! a test needs is declared, never inferred from whatever the host happens to have. Unset, it skips
//! and `cargo test --workspace` stays green. Set, it is unforgiving: a missing Docker daemon or a
//! missing Linux binary fails rather than degrading into a vacuous pass.
//!
//! ```sh
//! cargo build --target x86_64-unknown-linux-musl --bin flux
//! FLUX_TEST_CONTAINER=1 cargo test -p codewandler-flux-server --test remote_system_container
//! ```
//!
//! The image needs a Linux binary that runs on the base image. In a release the image carries the
//! published `flux-cli-x86_64-unknown-linux-gnu.tar.xz`; here it carries a locally built
//! statically linked musl binary, which runs on the same committed base image without depending on
//! the development host's glibc. `FLUX_TEST_CONTAINER_BINARY` overrides the path.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use flux_system::net::PrivateNetAllow;
use flux_system::port::GuardedWorkspaceFiles;

/// Declared opt-in. `FLUX_TEST_*` is the reserved test-only prefix, excluded from the public
/// configuration surface by naming convention.
const OPT_IN: &str = "FLUX_TEST_CONTAINER";
/// Overrides the Linux `flux` binary baked into the image under test.
const BINARY_VAR: &str = "FLUX_TEST_CONTAINER_BINARY";
/// Where a `cargo build --target x86_64-unknown-linux-musl --bin flux` lands.
const DEFAULT_BINARY: &str = "target/x86_64-unknown-linux-musl/debug/flux";

/// The ledger the container profile exists to keep. Mirrors `DELIVERY_LEDGER_PATH`, which is
/// private to `flux_server::system`.
const LEDGER: &str = ".flux/remote-system-delivery.json";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh, collision-free scratch directory, mirroring `tests/support/mod.rs`.
fn scratch_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("flux-server-it-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// Whether this run was promised a usable container runtime. Printed rather than silent, so a
/// skipped profile is visible in `cargo test -- --nocapture` and in the CI log.
fn container_runtime_promised(test: &str) -> bool {
    if std::env::var(OPT_IN).as_deref() == Ok("1") {
        return true;
    }
    println!(
        "skipping {test}: set {OPT_IN}=1 on a host with a working Docker daemon (the container \
         deployment profile is not exercised in ordinary workspace CI)"
    );
    false
}

/// A promised runtime must be real: past this point every failure is a real failure.
fn require_docker() {
    let probe = Command::new("docker").arg("info").output();
    match probe {
        Ok(output) if output.status.success() => {}
        Ok(output) => panic!(
            "{OPT_IN}=1 promised a container runtime, but `docker info` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(error) => {
            panic!("{OPT_IN}=1 promised a container runtime, but `docker` is not runnable: {error}")
        }
    }
}

/// The Linux `flux` binary the image under test carries.
fn require_linux_binary() -> PathBuf {
    let path = match std::env::var(BINARY_VAR) {
        Ok(value) => PathBuf::from(value),
        Err(_) => repo_root().join(DEFAULT_BINARY),
    };
    assert!(
        path.is_file(),
        "no Linux `flux` binary at {} — build one with `cargo build --target \
         x86_64-unknown-linux-musl --bin flux`, or point {BINARY_VAR} at a Linux binary that runs \
         on the profile's base image",
        path.display()
    );
    path
}

/// A port the daemon can publish on. Bound and released, which is the ordinary race every test
/// harness accepts; the container claims it immediately afterwards.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("reserve a loopback port")
        .local_addr()
        .expect("reserved port")
        .port()
}

/// Serializes image builds across this binary's test threads.
///
/// Both tests build from an identical context, and BuildKit resolves two concurrent builds of the
/// same layers against one shared cache. One run failed here under load while three consecutive
/// runs passed, which is exactly the shape of a cache race — so the builds take turns. The second
/// build is a cache hit, so this costs nothing but the ordering.
static IMAGE_BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Build the profile's image under that lock.
fn build_image(root: &std::path::Path, binary: &std::path::Path, tag: &str) {
    let _serialized = IMAGE_BUILD
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    run(Command::new(root.join("deploy/container/build-image.sh"))
        .current_dir(root)
        .arg("--binary")
        .arg(binary)
        .args(["--tag", tag]));
}

fn run(command: &mut Command) -> String {
    let rendered = format!("{command:?}");
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run {rendered}: {error}"));
    assert!(
        output.status.success(),
        "{rendered} failed ({})\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Removes the container even when an assertion unwinds, so a failed run does not strand a
/// listener on the developer's machine.
struct Container {
    name: String,
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.name])
            .output();
    }
}

impl Container {
    fn logs(&self) -> String {
        let output = Command::new("docker")
            .args(["logs", &self.name])
            .output()
            .expect("docker logs");
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    /// The documented readiness probe: a TCP connect proves the TLS listener accepts connections.
    fn await_tcp_readiness(&self, port: u16) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!(
            "the container never accepted a TCP connection on 127.0.0.1:{port}\nlogs:\n{}",
            self.logs()
        );
    }
}

/// Operation ids recorded in the committed delivery ledger.
fn ledger_operation_ids(encoded: &str) -> Vec<String> {
    let records: serde_json::Value =
        serde_json::from_str(encoded).expect("the delivery ledger is JSON");
    records
        .as_array()
        .expect("the delivery ledger is a JSON array")
        .iter()
        .map(|record| {
            record
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                .expect("every delivery record carries an operation id")
                .to_string()
        })
        .collect()
}

/// C-480: the whole container profile in one pass. Split into separate `#[test]`s this would build
/// and boot the image several times over for no additional evidence; the acceptance is a single
/// sequence — start, use, restart, prove what survived.
#[tokio::test]
async fn the_shipped_image_serves_a_mounted_workspace_across_a_restart() {
    if !container_runtime_promised("the_shipped_image_serves_a_mounted_workspace_across_a_restart")
    {
        return;
    }
    require_docker();
    let binary = require_linux_binary();
    let root = repo_root();

    let staging = scratch_dir("c480-container");
    let workspace = staging.join("workspace");
    let tls = staging.join("tls");
    let secrets = staging.join("secrets");
    for directory in [&workspace, &tls, &secrets] {
        std::fs::create_dir_all(directory).expect("profile directory");
    }
    // The image runs as uid 10001, which does not own a bind mount created by the test user. A
    // cluster solves this with `fsGroup`; a bind mount has no such control, so the test grants the
    // container write access to its own workspace explicitly.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o777))
            .expect("workspace is writable by the container's non-root identity");
    }

    // A certificate whose SAN matches the client URL, exactly as the profile requires.
    let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .expect("self-signed certificate for the daemon");
    let certificate_pem = certified.cert.pem();
    std::fs::write(tls.join("tls.crt"), &certificate_pem).expect("write certificate");
    std::fs::write(tls.join("tls.key"), certified.signing_key.serialize_pem())
        .expect("write private key");

    // The token reaches the daemon through the environment and never through argv: `--env-file`
    // keeps it off the command line, which is the rule the published profile states.
    let token = "c480-container-profile-token-not-a-real-secret";
    let env_file = secrets.join("remote-system.env");
    std::fs::write(&env_file, format!("FLUX_REMOTE_SYSTEM_TOKEN={token}\n"))
        .expect("write token environment file");

    let tag = format!("flux-system:c480-test-{}", std::process::id());
    build_image(&root, &binary, &tag);

    // Nothing secret and no workspace content may sit in a layer.
    let baked_env = run(Command::new("docker").args([
        "image",
        "inspect",
        "--format",
        "{{json .Config.Env}}",
        &tag,
    ]));
    assert!(
        !baked_env.contains("FLUX_REMOTE_SYSTEM_TOKEN"),
        "the image bakes a bearer token into its environment: {baked_env}"
    );
    let baked_user = run(Command::new("docker").args([
        "image",
        "inspect",
        "--format",
        "{{.Config.User}}",
        &tag,
    ]));
    assert_eq!(
        baked_user, "10001:10001",
        "the image must run under a non-root identity"
    );
    let baked_entrypoint = run(Command::new("docker").args([
        "image",
        "inspect",
        "--format",
        "{{json .Config.Entrypoint}}",
        &tag,
    ]));
    assert_eq!(
        baked_entrypoint, r#"["/usr/local/bin/flux","system","serve"]"#,
        "the image must run only `flux system serve`"
    );

    let port = free_port();
    let name = format!("flux-system-c480-{}", std::process::id());
    let container = Container { name: name.clone() };
    run(Command::new("docker")
        .args(["run", "--detach", "--name", &name])
        .args(["--read-only", "--tmpfs", "/tmp"])
        .args(["--publish", &format!("127.0.0.1:{port}:8790")])
        .arg("--volume")
        .arg(format!("{}:/srv/flux/workspace", workspace.display()))
        .arg("--volume")
        .arg(format!("{}:/run/flux-tls:ro", tls.display()))
        .arg("--env-file")
        .arg(&env_file)
        .arg(&tag));
    container.await_tcp_readiness(port);

    let endpoint = format!("https://127.0.0.1:{port}");
    // The same entry point `flux tui --remote` reaches through `execution.rs`; a released client
    // and this call are the same code path.
    let system = flux_server::system::connect_remote_system_with_ca_pem(
        &endpoint,
        token.to_string(),
        &PrivateNetAllow::Any,
        certificate_pem.as_bytes(),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "connect to the containerized daemon: {error}\nlogs:\n{}",
            container.logs()
        )
    });

    system
        .write_file("notes.txt", "written through the remote system\n")
        .await
        .expect("write through the containerized daemon");
    let read_back = system
        .read_file("notes.txt")
        .await
        .expect("read through the containerized daemon");
    assert_eq!(read_back, "written through the remote system\n");

    // The mount is the canonical workspace, not a copy inside the container.
    let on_host = std::fs::read_to_string(workspace.join("notes.txt"))
        .expect("the mounted volume holds the canonical workspace");
    assert_eq!(on_host, "written through the remote system\n");

    let ledger_before = system
        .read_file(LEDGER)
        .await
        .expect("the delivery ledger is written beneath the workspace");
    let ids_before = ledger_operation_ids(&ledger_before);
    assert!(
        !ids_before.is_empty(),
        "the daemon recorded no delivery for an executed operation"
    );

    // A mismatched peer is refused rather than served. The client refuses at connect; a raw
    // request proves the daemon's own side of the same contract.
    let mismatched = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(certificate_pem.as_bytes()).expect("root certificate"),
        )
        .build()
        .expect("mismatched-protocol client")
        .post(format!("{endpoint}/system/v1/execute"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "protocol_version": 999,
            "operation_id": "c480-protocol-mismatch",
            "fingerprint": "c480",
            "operation": "workspace.read",
            "arguments": {"path": "notes.txt"},
        }))
        .send()
        .await
        .expect("send a mismatched-protocol request");
    assert_eq!(
        mismatched.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "a mismatched protocol version must be refused"
    );
    let refusal = mismatched.text().await.expect("refusal body");
    assert!(
        refusal.contains("unsupported remote-system protocol version 999"),
        "the refusal must name the unsupported version: {refusal}"
    );

    // Restart, which is what an upgrade, a node drain and a crash all look like from here.
    run(Command::new("docker").args(["restart", &name]));
    container.await_tcp_readiness(port);

    let system = flux_server::system::connect_remote_system_with_ca_pem(
        &endpoint,
        token.to_string(),
        &PrivateNetAllow::Any,
        certificate_pem.as_bytes(),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "reconnect after restart: {error}\nlogs:\n{}",
            container.logs()
        )
    });

    let after_restart = system
        .read_file("notes.txt")
        .await
        .expect("the canonical workspace survives a restart");
    assert_eq!(
        after_restart, "written through the remote system\n",
        "the workspace volume did not survive the restart"
    );

    let ledger_after = system
        .read_file(LEDGER)
        .await
        .expect("the delivery ledger survives a restart");
    let ids_after = ledger_operation_ids(&ledger_after);
    for id in &ids_before {
        assert!(
            ids_after.contains(id),
            "delivery record `{id}` did not survive the restart — an operation id could execute \
             twice.\nbefore: {ledger_before}\nafter: {ledger_after}"
        );
    }

    // Only on the way out: a failed run keeps its evidence.
    drop(container);
    let _ = Command::new("docker")
        .args(["image", "rm", "--force", &tag])
        .output();
    let _ = std::fs::remove_dir_all(&staging);
}

/// The image the profile builds must not be able to smuggle a workspace in a layer: an operator who
/// forgets the volume gets an empty canonical workspace, not someone else's files.
#[test]
fn the_shipped_image_carries_no_workspace_content() {
    if !container_runtime_promised("the_shipped_image_carries_no_workspace_content") {
        return;
    }
    require_docker();
    let binary = require_linux_binary();
    let root = repo_root();

    let tag = format!("flux-system:c480-layers-{}", std::process::id());
    build_image(&root, &binary, &tag);

    let listing = run(Command::new("docker").args([
        "run",
        "--rm",
        "--entrypoint",
        "/bin/ls",
        &tag,
        "-A",
        "/srv/flux/workspace",
    ]));
    assert!(
        listing.is_empty(),
        "the image ships a non-empty canonical workspace: {listing}"
    );

    let _ = Command::new("docker")
        .args(["image", "rm", "--force", &tag])
        .output();
}
