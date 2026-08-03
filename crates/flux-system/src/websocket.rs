//! Guarded, substrate-neutral WebSocket sessions.
//!
//! The native opener performs the RFC 6455 handshake over the exact TCP address admitted by the
//! shared egress guard. Consumers receive only this bounded message session; they never receive the
//! socket or a client from which a second, unguarded connection could be made.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use flux_core::{Error, Result};
use futures_util::{SinkExt as _, StreamExt as _};

use crate::net::{self, DialStream, DialTarget, PrivateNetAllow};
use crate::port::Guarded;

/// Runtime defaults shared by generated connector channels and plugin compatibility callers.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const DEFAULT_QUEUED_MESSAGES: usize = 32;
pub const DEFAULT_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// A complete, already-authorized RFC 6455 handshake declaration.
///
/// `Debug` deliberately omits the URL and header values: both may contain credentials in query or
/// authorization fields. Validation still names structural defects without reproducing secrets.
#[derive(Clone)]
pub struct WebSocketConnect {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub subprotocols: Vec<String>,
    pub max_message_bytes: usize,
    pub queued_messages: usize,
    pub close_timeout: Duration,
}

impl WebSocketConnect {
    /// Construct a connection using the guarded runtime defaults.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
            subprotocols: Vec::new(),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            queued_messages: DEFAULT_QUEUED_MESSAGES,
            close_timeout: DEFAULT_CLOSE_TIMEOUT,
        }
    }
}

impl std::fmt::Debug for WebSocketConnect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketConnect")
            .field("url", &"[REDACTED]")
            .field("header_count", &self.headers.len())
            .field("subprotocol_count", &self.subprotocols.len())
            .field("max_message_bytes", &self.max_message_bytes)
            .field("queued_messages", &self.queued_messages)
            .field("close_timeout", &self.close_timeout)
            .finish()
    }
}

/// One bounded WebSocket message. Control frames are handled inside the guarded session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketEvent {
    Text(String),
    Binary(Vec<u8>),
    Close { code: Option<u16>, reason: String },
}

/// Object-safe lifecycle for a native or remote guarded WebSocket.
pub trait WebSocketHandle: Send {
    fn read<'a>(&'a mut self) -> Guarded<'a, Option<WebSocketEvent>>;
    fn close<'a>(&'a mut self) -> Guarded<'a, ()>;
}

/// Opaque session returned by [`crate::port::GuardedNetwork::open_websocket_scoped`].
pub struct GuardedWebSocketSession {
    inner: Box<dyn WebSocketHandle>,
}

impl GuardedWebSocketSession {
    pub fn from_handle(handle: impl WebSocketHandle + 'static) -> Self {
        Self {
            inner: Box::new(handle),
        }
    }

    /// Read one queued data/close event. `None` means the session ended cleanly.
    pub async fn read(&mut self) -> Result<Option<WebSocketEvent>> {
        self.inner.read().await
    }

    /// Request a graceful close, bounded by the declaration's close timeout.
    pub async fn close(&mut self) -> Result<()> {
        self.inner.close().await
    }
}

enum Command {
    Close(tokio::sync::oneshot::Sender<Result<()>>),
}

struct NativeSession {
    events: tokio::sync::mpsc::Receiver<WebSocketEvent>,
    commands: tokio::sync::mpsc::Sender<Command>,
    terminal_error: Arc<Mutex<Option<String>>>,
    task: tokio::task::AbortHandle,
}

impl WebSocketHandle for NativeSession {
    fn read<'a>(&'a mut self) -> Guarded<'a, Option<WebSocketEvent>> {
        Box::pin(async move {
            match self.events.recv().await {
                Some(event) => Ok(Some(event)),
                None => match self
                    .terminal_error
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .take()
                {
                    Some(error) => Err(Error::Other(error)),
                    None => Ok(None),
                },
            }
        })
    }

    fn close<'a>(&'a mut self) -> Guarded<'a, ()> {
        Box::pin(async move {
            let (reply, answer) = tokio::sync::oneshot::channel();
            if self.commands.send(Command::Close(reply)).await.is_err() {
                return Ok(());
            }
            answer.await.unwrap_or(Ok(()))
        })
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type NativeWebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Native implementation behind the [`crate::port::GuardedNetwork`] port.
pub(crate) async fn open_native(
    connect: &WebSocketConnect,
    allow: &PrivateNetAllow,
) -> Result<GuardedWebSocketSession> {
    open_native_with_resolver(connect, allow, &net::SystemHostResolver, None)
        .await
        .map(|(session, _)| session)
}

/// Native guarded opener with injectable DNS and TLS seams for compatibility callers and tests.
/// The returned addresses are the exact set vetted by the shared egress guard.
pub async fn open_native_with_resolver(
    connect: &WebSocketConnect,
    allow: &PrivateNetAllow,
    resolver: &dyn net::HostResolver,
    connector: Option<tokio_tungstenite::Connector>,
) -> Result<(GuardedWebSocketSession, Vec<std::net::SocketAddr>)> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tokio_tungstenite::tungstenite::http::header;

    validate_limits(connect)?;
    let url = url::Url::parse(&connect.url)
        .map_err(|_| Error::Other("websocket URL is invalid".into()))?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(Error::Other(
            "websocket URL scheme must be ws or wss".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Other(
            "websocket URL must not carry userinfo; use a guarded header".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::Other("websocket URL has no host".into()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| Error::Other("websocket URL has no port".into()))?;

    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| Error::Other("websocket handshake request is invalid".into()))?;
    for (name, value) in &connect.headers {
        let parsed_name = header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| Error::Other("websocket header name is invalid".into()))?;
        if matches!(
            parsed_name,
            header::HOST
                | header::CONNECTION
                | header::UPGRADE
                | header::SEC_WEBSOCKET_KEY
                | header::SEC_WEBSOCKET_VERSION
                | header::SEC_WEBSOCKET_EXTENSIONS
                | header::SEC_WEBSOCKET_PROTOCOL
        ) {
            return Err(Error::Other(format!(
                "websocket header `{parsed_name}` is controlled by the guarded handshake"
            )));
        }
        let parsed_value = header::HeaderValue::from_str(value)
            .map_err(|_| Error::Other("websocket header value is invalid".into()))?;
        request.headers_mut().append(parsed_name, parsed_value);
    }
    if !connect.subprotocols.is_empty() {
        for protocol in &connect.subprotocols {
            if protocol.is_empty()
                || protocol.bytes().any(|byte| {
                    !matches!(byte, 0x21..=0x7e) || b"()<>@,;:\\\"/[]?={} \t".contains(&byte)
                })
            {
                return Err(Error::Other("websocket subprotocol is invalid".into()));
            }
        }
        let value = header::HeaderValue::from_str(&connect.subprotocols.join(", "))
            .map_err(|_| Error::Other("websocket subprotocol list is invalid".into()))?;
        request
            .headers_mut()
            .insert(header::SEC_WEBSOCKET_PROTOCOL, value);
    }

    let target = DialTarget::Tcp {
        host: host.to_owned(),
        port,
    };
    let (stream, pinned) = net::dial_scoped_pinned_with_resolver(&target, allow, resolver).await?;
    let tcp = match stream {
        DialStream::Tcp(stream) => stream,
        _ => return Err(Error::Other("guarded WebSocket dial was not TCP".into())),
    };
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(16 * 1024)
        .max_write_buffer_size(connect.max_message_bytes.saturating_mul(2).max(32 * 1024))
        .max_message_size(Some(connect.max_message_bytes))
        .max_frame_size(Some(connect.max_message_bytes));
    let (stream, response) =
        tokio_tungstenite::client_async_tls_with_config(request, tcp, Some(config), connector)
            .await
            .map_err(handshake_error)?;

    if !connect.subprotocols.is_empty() {
        let selected = response
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok());
        if selected.is_some_and(|value| !connect.subprotocols.iter().any(|p| p == value)) {
            return Err(Error::Other(
                "websocket server selected an undeclared subprotocol".into(),
            ));
        }
    }
    Ok((
        manage(stream, connect.queued_messages, connect.close_timeout),
        pinned,
    ))
}

fn validate_limits(connect: &WebSocketConnect) -> Result<()> {
    if connect.max_message_bytes == 0
        || connect.queued_messages == 0
        || connect.close_timeout.is_zero()
    {
        return Err(Error::Other("websocket limits must be non-zero".into()));
    }
    Ok(())
}

fn handshake_error(error: tokio_tungstenite::tungstenite::Error) -> Error {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => Error::Api {
            status: response.status().as_u16(),
            message: "websocket handshake was refused".into(),
        },
        _ => Error::Http("websocket handshake failed".into()),
    }
}

fn manage(
    mut stream: NativeWebSocket,
    queue: usize,
    close_timeout: Duration,
) -> GuardedWebSocketSession {
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(queue);
    let (command_tx, mut command_rx) = tokio::sync::mpsc::channel(1);
    let terminal_error = Arc::new(Mutex::new(None));
    let task_error = Arc::clone(&terminal_error);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    if let Some(Command::Close(reply)) = command {
                        let result = close_bounded(&mut stream, close_timeout).await;
                        let _ = reply.send(result);
                    }
                    break;
                }
                incoming = stream.next() => {
                    let event = match incoming {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            Some(WebSocketEvent::Text(text.to_string()))
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes))) => {
                            Some(WebSocketEvent::Binary(bytes.to_vec()))
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => {
                            let (code, reason) = frame.map_or((None, String::new()), |frame| {
                                (Some(u16::from(frame.code)), frame.reason.to_string())
                            });
                            let _ = event_tx.try_send(WebSocketEvent::Close { code, reason });
                            break;
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(_))) => {
                            match tokio::time::timeout(close_timeout, stream.flush()).await {
                                Ok(Ok(())) => {}
                                Ok(Err(_)) => {
                                    set_error(&task_error, "websocket pong flush failed");
                                    break;
                                }
                                Err(_) => {
                                    set_error(&task_error, "websocket pong flush timed out");
                                    break;
                                }
                            }
                            None
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(_)))
                        | Some(Ok(tokio_tungstenite::tungstenite::Message::Frame(_))) => None,
                        Some(Err(_)) => {
                            set_error(&task_error, "websocket read failed");
                            break;
                        }
                        None => break,
                    };
                    if let Some(event) = event {
                        if event_tx.try_send(event).is_err() {
                            set_error(&task_error, "websocket message queue overflow");
                            let _ = close_bounded(&mut stream, close_timeout).await;
                            break;
                        }
                    }
                }
            }
        }
    });
    GuardedWebSocketSession::from_handle(NativeSession {
        events: event_rx,
        commands: command_tx,
        terminal_error,
        task: task.abort_handle(),
    })
}

fn set_error(error: &Mutex<Option<String>>, message: &str) {
    *error.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(message.to_owned());
}

async fn close_bounded(stream: &mut NativeWebSocket, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, stream.close(None))
        .await
        .map_err(|_| Error::Other("websocket close handshake timed out".into()))?
        .map_err(|_| Error::Other("websocket close handshake failed".into()))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct FixedResolver {
        calls: AtomicUsize,
    }

    impl net::HostResolver for FixedResolver {
        fn resolve(&self, _: &str, _: u16) -> std::io::Result<Vec<IpAddr>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
        }
    }

    #[test]
    fn debug_never_contains_the_url_or_header_values() {
        let mut connect = WebSocketConnect::new("wss://secret.example/events?token=value");
        connect
            .headers
            .push(("Authorization".into(), "Basic secret".into()));
        let rendered = format!("{connect:?}");
        assert!(!rendered.contains("secret.example"));
        assert!(!rendered.contains("Basic secret"));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn defaults_are_the_declared_runtime_bounds() {
        let connect = WebSocketConnect::new("wss://events.example.test/socket");
        assert_eq!(connect.max_message_bytes, 1024 * 1024);
        assert_eq!(connect.queued_messages, 32);
        assert_eq!(connect.close_timeout, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_private_resolution_is_refused_before_any_socket_is_opened() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("private test listener");
        let port = listener.local_addr().expect("private test address").port();
        let resolver = FixedResolver {
            calls: AtomicUsize::new(0),
        };
        let mut connect = WebSocketConnect::new(format!(
            "ws://events-private.test:{port}/events?token=credential"
        ));
        connect
            .headers
            .push(("Authorization".into(), "Bearer credential".into()));

        let refusal = open_native_with_resolver(
            &connect,
            &PrivateNetAllow::from_hosts(Vec::<String>::new()),
            &resolver,
            None,
        )
        .await
        .err()
        .expect("private destinations need an exact host admission");
        assert!(refusal.to_string().contains("private"), "{refusal}");
        assert_eq!(resolver.calls.load(Ordering::Relaxed), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err(),
            "a refused private destination reached the TCP listener"
        );
        assert!(!refusal.to_string().contains("credential"), "{refusal}");
    }

    #[tokio::test]
    async fn a_caller_cannot_replace_the_guarded_handshakes_authority_header() {
        let mut connect = WebSocketConnect::new("ws://127.0.0.1:9/events");
        connect
            .headers
            .push(("Host".into(), "attacker.example".into()));
        let refusal = open_native_with_resolver(
            &connect,
            &PrivateNetAllow::from_hosts(["127.0.0.1".into()]),
            &net::SystemHostResolver,
            None,
        )
        .await
        .err()
        .expect("the guarded handshake owns Host");
        assert!(refusal.to_string().contains("controlled"), "{refusal}");
        assert!(!refusal.to_string().contains("attacker"), "{refusal}");

        let authority = WebSocketConnect::new("ws://declared@127.0.0.1:9/events");
        let refusal = open_native_with_resolver(
            &authority,
            &PrivateNetAllow::from_hosts(["127.0.0.1".into()]),
            &net::SystemHostResolver,
            None,
        )
        .await
        .err()
        .expect("userinfo cannot reshape the declared authority");
        assert!(refusal.to_string().contains("userinfo"), "{refusal}");
    }

    async fn one_text_frame(system: &dyn crate::port::GuardedNetwork) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock vendor");
        let address = listener.local_addr().expect("mock address");
        let vendor = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let mut websocket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("accept WebSocket");
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    r#"{"type":"ChannelCreated"}"#.into(),
                ))
                .await
                .expect("send event");
            websocket.close(None).await.expect("close vendor socket");
        });
        let connect = WebSocketConnect::new(format!("ws://{address}/ari/events"));
        let allow = PrivateNetAllow::from_hosts(["127.0.0.1".to_string()]);
        let mut session = system
            .open_websocket_scoped(&connect, &allow)
            .await
            .expect("guarded handshake");
        assert_eq!(
            session.read().await.expect("read event"),
            Some(WebSocketEvent::Text(r#"{"type":"ChannelCreated"}"#.into()))
        );
        vendor.await.expect("vendor task");
    }

    #[tokio::test]
    async fn native_and_selected_remote_systems_host_the_websocket() {
        let workspace = crate::Workspace::new(std::env::current_dir().expect("current directory"))
            .expect("workspace");
        let native = Arc::new(crate::System::new(workspace));
        one_text_frame(native.as_ref()).await;

        let remote = crate::remote::RemoteSystem::loopback(native);
        one_text_frame(&remote).await;
    }

    #[tokio::test]
    async fn an_unserved_selected_remote_never_falls_back_to_a_local_socket() {
        struct NoSocketDelegate;
        impl crate::remote::Delegate for NoSocketDelegate {}

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local fallback detector");
        let address = listener.local_addr().expect("local fallback address");
        let remote = crate::remote::RemoteSystem::new(Arc::new(NoSocketDelegate));
        let refusal = crate::port::GuardedNetwork::open_websocket_scoped(
            &remote,
            &WebSocketConnect::new(format!("ws://{address}/events")),
            &PrivateNetAllow::from_hosts(["127.0.0.1".into()]),
        )
        .await
        .err()
        .expect("the selected remote serves no WebSocket operation");
        assert_eq!(
            crate::remote::failure_mode(&refusal),
            Some(flux_core::GuardedIoFailure::Unserved)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err(),
            "a remote refusal silently opened the coordinator's local socket"
        );
    }

    #[tokio::test]
    async fn message_and_queue_limits_fail_closed() {
        async fn vendor(messages: Vec<String>) -> (u16, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bounded test listener");
            let port = listener.local_addr().expect("bounded test address").port();
            let task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.expect("bounded test client");
                let mut socket = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("bounded WebSocket handshake");
                for message in messages {
                    if socket
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            message.into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                let _ = socket.close(None).await;
            });
            (port, task)
        }

        let allow = PrivateNetAllow::from_hosts(["127.0.0.1".to_owned()]);
        let (port, server) = vendor(vec!["first".into(), "second".into()]).await;
        let mut connect = WebSocketConnect::new(format!("ws://127.0.0.1:{port}/events"));
        connect.queued_messages = 1;
        let (mut session, _) =
            open_native_with_resolver(&connect, &allow, &net::SystemHostResolver, None)
                .await
                .expect("bounded queue handshake");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            session.read().await.expect("first queued event"),
            Some(WebSocketEvent::Text("first".into()))
        );
        let overflow = session
            .read()
            .await
            .expect_err("the second event exceeds the queue");
        assert!(
            overflow.to_string().contains("queue overflow"),
            "{overflow}"
        );
        server.await.expect("queue-limit vendor");

        let (port, server) = vendor(vec!["x".repeat(65)]).await;
        let mut connect = WebSocketConnect::new(format!("ws://127.0.0.1:{port}/events"));
        connect.max_message_bytes = 64;
        let (mut session, _) =
            open_native_with_resolver(&connect, &allow, &net::SystemHostResolver, None)
                .await
                .expect("bounded message handshake");
        let oversized = session
            .read()
            .await
            .expect_err("an oversized frame never reaches the consumer");
        assert!(oversized.to_string().contains("read failed"), "{oversized}");
        server.await.expect("message-limit vendor");
    }

    #[tokio::test]
    async fn wss_keeps_the_declared_hostname_for_sni_over_one_pinned_resolution() {
        use rustls::pki_types::PrivatePkcs8KeyDer;

        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        }
        let certified = rcgen::generate_simple_self_signed(vec!["events-sni.test".into()])
            .expect("certificate");
        let certificate = certified.cert.der().clone();
        let private_key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], private_key.into())
            .expect("server TLS config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate).expect("test root");
        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_tungstenite::Connector::Rustls(Arc::new(client_config));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS address").port();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("TLS client");
            let tls = acceptor.accept(stream).await.expect("TLS handshake");
            let server_name = tls
                .get_ref()
                .1
                .server_name()
                .expect("client supplied SNI")
                .to_owned();
            let _ = seen_tx.send(server_name);
            let mut socket = tokio_tungstenite::accept_async(tls)
                .await
                .expect("WebSocket handshake");
            socket.close(None).await.expect("server close");
        });

        let resolver = FixedResolver {
            calls: AtomicUsize::new(0),
        };
        let connect =
            WebSocketConnect::new(format!("wss://events-sni.test:{port}/ari/events?app=flux"));
        let allow = PrivateNetAllow::from_hosts(["events-sni.test".to_owned()]);
        let (mut session, pinned) =
            open_native_with_resolver(&connect, &allow, &resolver, Some(connector))
                .await
                .expect("guarded WSS handshake");
        assert_eq!(seen_rx.await.expect("observed SNI"), "events-sni.test");
        assert_eq!(resolver.calls.load(Ordering::Relaxed), 1);
        assert_eq!(pinned, [std::net::SocketAddr::from(([127, 0, 0, 1], port))]);
        let _ = session.read().await.expect("clean server close");
        server.await.expect("TLS server task");
    }
}
