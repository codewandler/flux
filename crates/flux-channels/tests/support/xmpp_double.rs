//! An **in-process XMPP-over-WebSocket double** — the server side of RFC 7395, enough of it to drive
//! `XmppMucRoom` through a real socket with no browser, no vendor SDK and no network.
//!
//! It speaks the exact sequence the 2026-07-30 spike observed live and the design's Feasibility
//! section records: `<open/>` → SASL → `<open/>` → resource bind → MUC presence → `groupchat`. It is
//! deliberately dumb about *parsing* (it dispatches on the frame's opening element) and precise about
//! *recording*: every text frame the client sent is kept verbatim, because the two traps this suite
//! regresses — an unqualified stanza, a whitespace keepalive — are properties of the raw bytes.
//!
//! It also reproduces the JaaS case asymmetry on purpose: the client is configured with
//! [`ROOM_JID_CONFIGURED`] and the double answers presence from [`ROOM_JID_SERVER`].

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse as WsErrorResponse, Request as WsRequest, Response as WsResponse,
};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// The MUC JID as a program declares it — mixed case, the way a JaaS token's `room` claim keeps it.
pub const ROOM_JID_CONFIGURED: &str = "StandUp@conference.example.org";
/// The MUC JID as the server spells it — lowercased, the way the conference-request response does.
pub const ROOM_JID_SERVER: &str = "standup@conference.example.org";

/// How long a `wait_for` waits before declaring the client never sent what was expected.
const WAIT: Duration = Duration::from_secs(5);

/// A running double: one WebSocket listener, one connection at a time.
pub struct XmppDouble {
    addr: SocketAddr,
    sent: Arc<Mutex<Vec<String>>>,
    push: mpsc::UnboundedSender<String>,
    _accept: tokio::task::JoinHandle<()>,
}

impl XmppDouble {
    /// Bind on loopback and start serving. The returned double is ready to be connected to.
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (push, mut push_rx) = mpsc::unbounded_channel::<String>();

        let recorded = sent.clone();
        let accept = tokio::spawn(async move {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(sock, negotiate).await else {
                return;
            };

            let mut opens = 0usize;
            loop {
                tokio::select! {
                    outbound = push_rx.recv() => match outbound {
                        Some(frame) => {
                            if ws.send(WsMessage::Text(frame.into())).await.is_err() {
                                return;
                            }
                        }
                        None => return,
                    },
                    inbound = ws.next() => {
                        let frame = match inbound {
                            Some(Ok(WsMessage::Text(t))) => t.to_string(),
                            // A close, an error, or a non-text frame ends the session. (A binary or
                            // whitespace frame would be a protocol error on a real server; the
                            // keepalive regression test asserts the client never sends one.)
                            Some(Ok(WsMessage::Close(_))) | None => return,
                            Some(Err(_)) => return,
                            Some(Ok(_)) => continue,
                        };
                        recorded.lock().unwrap().push(frame.clone());
                        for reply in replies_to(&frame, &mut opens) {
                            if ws.send(WsMessage::Text(reply.into())).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });

        Self {
            addr,
            sent,
            push,
            _accept: accept,
        }
    }

    /// The `ws://` endpoint to configure a room with.
    pub fn url(&self) -> String {
        format!("ws://{}/xmpp-websocket", self.addr)
    }

    /// Every text frame the client has sent, in order and verbatim.
    pub fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    /// Deliver one server→client frame.
    pub async fn push(&self, frame: impl Into<String>) {
        self.push.send(frame.into()).expect("the double is running");
    }

    /// Block until the client has sent a frame matching `pred`, and return it. Panics on timeout —
    /// "the client never sent it" is the failure these tests are looking for.
    pub async fn wait_for(&self, pred: impl Fn(&str) -> bool) -> String {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(found) = self.sent().into_iter().find(|f| pred(f)) {
                return found;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for a frame; sent were {:?}", self.sent());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// RFC 7395 §3.1: the WebSocket subprotocol is `xmpp`, and a compliant server echoes it back — which
/// is how this double proves the client asked for it.
// The `Err` type is tungstenite's `ErrorResponse` and is not ours to shrink.
#[allow(clippy::result_large_err)]
fn negotiate(req: &WsRequest, mut resp: WsResponse) -> Result<WsResponse, WsErrorResponse> {
    if req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|p| p.trim() == "xmpp"))
    {
        resp.headers_mut()
            .insert("sec-websocket-protocol", HeaderValue::from_static("xmpp"));
    }
    Ok(resp)
}

/// The server's answer to one client frame. `opens` counts stream opens, because RFC 7395's SASL
/// restart replays `<open/>` and the two get different feature sets.
fn replies_to(frame: &str, opens: &mut usize) -> Vec<String> {
    const FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
    const STREAMS: &str = "http://etherx.jabber.org/streams";

    if frame.starts_with("<open") {
        *opens += 1;
        let open = format!(
            "<open xmlns='{FRAMING}' from='example.org' id='s1' version='1.0' xml:lang='en'/>"
        );
        let features = if *opens == 1 {
            format!(
                "<stream:features xmlns:stream='{STREAMS}'>\
                 <mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>\
                 <mechanism>ANONYMOUS</mechanism><mechanism>PLAIN</mechanism>\
                 </mechanisms></stream:features>"
            )
        } else {
            format!(
                "<stream:features xmlns:stream='{STREAMS}'>\
                 <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/>\
                 </stream:features>"
            )
        };
        return vec![open, features];
    }

    if frame.starts_with("<auth") {
        return vec!["<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>".to_string()];
    }

    if frame.starts_with("<iq") && frame.contains("urn:ietf:params:xml:ns:xmpp-bind") {
        let id = attr(frame, "id").unwrap_or_default();
        return vec![format!(
            "<iq xmlns='jabber:client' type='result' id='{id}'>\
             <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
             <jid>guest-9f3c@example.org/flux</jid></bind></iq>"
        )];
    }

    if frame.starts_with("<presence") && frame.contains("http://jabber.org/protocol/muc") {
        // The MUC join. Existing occupants first, our own self-presence (status 110) last — the order
        // a real service produces, and the marker a backend must key `is_self` on.
        let nick = attr(frame, "to")
            .and_then(|to| to.rsplit_once('/').map(|(_, n)| n.to_string()))
            .unwrap_or_else(|| "flux".to_string());
        return vec![
            format!(
                "<presence xmlns='jabber:client' from='{ROOM_JID_SERVER}/timo'>\
                 <x xmlns='http://jabber.org/protocol/muc#user'>\
                 <item affiliation='owner' role='moderator'/></x></presence>"
            ),
            format!(
                "<presence xmlns='jabber:client' from='{ROOM_JID_SERVER}/{nick}'>\
                 <x xmlns='http://jabber.org/protocol/muc#user'>\
                 <item affiliation='none' role='participant'/>\
                 <status code='110'/></x></presence>"
            ),
        ];
    }

    Vec::new()
}

/// The value of a single-quoted attribute on the frame's opening tag.
fn attr(frame: &str, name: &str) -> Option<String> {
    let needle = format!("{name}='");
    let start = frame.find(&needle)? + needle.len();
    let rest = &frame[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}
