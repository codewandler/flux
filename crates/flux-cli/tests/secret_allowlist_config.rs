//! C-464 — the documented `$secret` allowlist reaches the real `http.request` tool.
//!
//! This is a binary-level test because a config parser test and a flux-web test can both pass while
//! `execution.rs` still hard-codes `allowed_secrets: None` between them — the exact wiring gap this
//! story found. A deterministic Flux program makes the real tool call; a test-owned loopback server
//! proves the configured secret reached the wire.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "flux-c464-{}-{:?}",
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

#[test]
fn configured_secret_allowlist_reaches_the_real_http_tool() {
    const NAME: &str = "FLUX_TEST_C464_TOKEN";
    const VALUE: &str = "c464-configured-secret-42";

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = vec![0_u8; 8192];
                    let read = stream.read(&mut request).unwrap();
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
                        )
                        .unwrap();
                    return Some(String::from_utf8_lossy(&request[..read]).into_owned());
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("test server accept failed: {error}"),
            }
        }
    });

    let tmp = TempDir::new();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(tmp.path().join(".flux")).unwrap();
    std::fs::write(
        tmp.path().join(".flux/config.toml"),
        format!(
            "[private_net]\nweb = true\n\n[web]\nallowed_secrets = [\"{NAME};to=127.0.0.1;in=header\"]\n"
        ),
    )
    .unwrap();

    std::fs::write(
        tmp.path().join("probe.flux"),
        "flow probe(url: String, headers: Any) -> Number\n  $resp = http.request({url: $url, headers: $headers})\n  return $resp.status\n",
    )
    .unwrap();

    let inputs = serde_json::json!({
        "url": format!("http://{addr}/probe"),
        "headers": {"authorization": {"$secret": NAME}}
    });
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args([
            "run",
            "probe.flux",
            "--entry",
            "probe",
            "--inputs",
            &inputs.to_string(),
            "--yes",
            "-m",
            "mock",
        ])
        .current_dir(tmp.path())
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .env(NAME, VALUE)
        .env_remove("FLUX_WEB_SECRET_ALLOW")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let request = server.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "configured run failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    let request = request.expect("the configured secret never reached the test server");
    assert!(
        request.contains(&format!("authorization: {VALUE}")),
        "the configured allowlist did not authorize the secret: {request}"
    );
    assert!(!stdout.contains(VALUE) && !stderr.contains(VALUE));
}
