//! C-503's release-boundary proof through the real CLI, provider loop, executor and event store.
//!
//! The local Exchange fixture deliberately models the security-relevant host path rather than a
//! response-only mock: it holds a vendor credential in an Exchange-side store, resolves that value
//! only while constructing the vendor request, records the vendor wire that consumed it, and sends
//! a credential-free result back to Flux. That proves the sentinel was actually exercised before
//! checking every Flux-visible and persisted surface for its absence.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use flux_events::EventStore;
use serde_json::json;

const SERVICE_TOKEN: &str = "flux-service-account-token-123";
const EXCHANGE_HELD_SECRET: &str = "vendor-credential-never-sent-to-flux";
const CREDENTIAL_ADDRESS: &str = "tenants/acme/com.vendor.api/prod/token";

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

#[derive(Default)]
struct FaithfulExchangeHost {
    credentials: Mutex<BTreeMap<String, String>>,
    flux_wire: Mutex<Vec<String>>,
    vendor_wire: Mutex<Vec<String>>,
    logs: Mutex<Vec<String>>,
}

struct ExchangeServer {
    base: String,
    host: Arc<FaithfulExchangeHost>,
    running: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}

impl ExchangeServer {
    fn start() -> Self {
        let host = Arc::new(FaithfulExchangeHost::default());
        host.credentials
            .lock()
            .unwrap()
            .insert(CREDENTIAL_ADDRESS.into(), EXCHANGE_HELD_SECRET.into());

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let running = Arc::new(AtomicBool::new(true));
        let thread_host = host.clone();
        let thread_running = running.clone();
        let task = thread::spawn(move || {
            while thread_running.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => serve(&mut stream, &thread_host),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("Exchange fixture accept failed: {error}"),
                }
            }
        });
        Self {
            base,
            host,
            running,
            task: Some(task),
        }
    }
}

impl Drop for ExchangeServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        let Some(headers_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..headers_end]);
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if bytes.len() >= headers_end + 4 + length {
            break;
        }
    }
    String::from_utf8(bytes).unwrap()
}

fn serve(stream: &mut TcpStream, host: &FaithfulExchangeHost) {
    let request = read_request(stream);
    host.flux_wire.lock().unwrap().push(request.clone());
    let line = request.lines().next().unwrap_or_default();
    let authorized = request.contains(&format!("Bearer {SERVICE_TOKEN}"));
    let (status, body) = if !authorized {
        (
            "401 Unauthorized",
            json!({"error": "service account authentication failed"}).to_string(),
        )
    } else if line.contains("/api/catalogue/effective") {
        (
            "200 OK",
            json!({
                "generation": format!("sha256:{}", "1".repeat(64)),
                "operations": [{
                    "id": "vendor.ok",
                    "description": "Read through an Exchange-held credential",
                    "input_schema": {
                        "type": "object",
                        "properties": {"message": {"type": "string"}},
                        "additionalProperties": false
                    },
                    "effects": ["read", "network"],
                    "risk": "low",
                    "idempotency": "idempotent",
                    "admitted": true,
                    "connection": "prod"
                }]
            })
            .to_string(),
        )
    } else if line.contains("/api/operations/vendor.ok/invoke?connection=prod") {
        let credential = host
            .credentials
            .lock()
            .unwrap()
            .get(CREDENTIAL_ADDRESS)
            .cloned()
            .expect("the Exchange host owns the vendor credential");
        host.vendor_wire
            .lock()
            .unwrap()
            .push(format!("Authorization: Bearer {credential}"));
        host.logs
            .lock()
            .unwrap()
            .push("service-account invoked vendor.ok through connection prod".into());
        (
            "200 OK",
            json!({
                "operation": "vendor.ok",
                "content": "vendor-result",
                "view": null,
                "is_error": false
            })
            .to_string(),
        )
    } else {
        (
            "404 Not Found",
            json!({"error": "unknown route"}).to_string(),
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
}

fn assert_excludes_secret(surface: &str, value: impl AsRef<[u8]>) {
    let bytes = value.as_ref();
    assert_excludes_exchange_credential(surface, bytes);
    assert!(
        !bytes
            .windows(SERVICE_TOKEN.len())
            .any(|part| part == SERVICE_TOKEN.as_bytes()),
        "{surface} contains the Exchange Service Account token"
    );
}

fn assert_excludes_exchange_credential(surface: &str, value: impl AsRef<[u8]>) {
    let bytes = value.as_ref();
    assert!(
        !bytes
            .windows(EXCHANGE_HELD_SECRET.len())
            .any(|part| part == EXCHANGE_HELD_SECRET.as_bytes()),
        "{surface} contains the Exchange-held credential"
    );
}

#[test]
fn exchange_held_credential_stays_out_of_flux_output_logs_events_and_session_state() {
    let temp = TempDir::new("exchange-binding");
    let work = temp.path().join("work");
    let home = temp.path().join("home");
    let store = temp.path().join("store");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    let exchange = ExchangeServer::start();

    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--yes", "-m", "mock", "read through Exchange"])
        .current_dir(&work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .env("FLUX_STORE_DIR", &store)
        .env("FLUX_EXCHANGE_URL", &exchange.base)
        .env("FLUX_EXCHANGE_SERVICE_ACCOUNT_TOKEN", SERVICE_TOKEN)
        .env("FLUX_MOCK_TOOL", "vendor.ok")
        .env("FLUX_MOCK_TOOL_INPUT", r#"{"message":"hello"}"#)
        .stdin(Stdio::null())
        .output()
        .expect("spawn real Flux CLI");

    assert!(
        output.status.success(),
        "Flux run failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_excludes_secret("stdout", &output.stdout);
    assert_excludes_secret("stderr/logs", &output.stderr);

    let credentials = exchange.host.credentials.lock().unwrap();
    assert_eq!(
        credentials.get(CREDENTIAL_ADDRESS).map(String::as_str),
        Some(EXCHANGE_HELD_SECRET),
        "the sentinel must be held in the Exchange-side credential store"
    );
    drop(credentials);
    assert!(
        exchange
            .host
            .vendor_wire
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains(EXCHANGE_HELD_SECRET)),
        "the Exchange host must consume the held credential on its vendor wire"
    );
    let flux_wire = exchange.host.flux_wire.lock().unwrap().join("\n");
    assert!(flux_wire.contains(&format!("Bearer {SERVICE_TOKEN}")));
    assert_excludes_exchange_credential("Flux/Exchange HTTP wire", flux_wire);
    assert_excludes_secret(
        "Exchange host logs",
        exchange.host.logs.lock().unwrap().join("\n"),
    );

    let events = EventStore::open(store.join("events.db")).expect("open persisted event store");
    let streams = events.all_streams().expect("enumerate persisted sessions");
    assert!(
        !streams.is_empty(),
        "the real CLI run must persist a session"
    );
    for stream in streams {
        let raw_events = format!("{:?}", events.load_stream(&stream, None).unwrap());
        assert_excludes_secret("persisted events", raw_events);
        let evidence = serde_json::to_vec(&events.observations(&stream).unwrap()).unwrap();
        assert_excludes_secret("persisted evidence", evidence);
        let conversation = serde_json::to_vec(&events.conversation(&stream).unwrap()).unwrap();
        assert_excludes_secret("persisted session conversation", conversation);
        let trace = serde_json::to_vec(&events.run_trace(&stream).unwrap()).unwrap();
        assert_excludes_secret("persisted run trace", trace);
    }
    for entry in std::fs::read_dir(&store).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            assert_excludes_secret("event-store bytes", std::fs::read(path).unwrap());
        }
    }
}
