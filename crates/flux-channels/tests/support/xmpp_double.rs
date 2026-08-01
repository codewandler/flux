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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse as WsErrorResponse, Request as WsRequest, Response as WsResponse,
};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::WebSocketStream;

/// The MUC JID as a program declares it — mixed case, the way a JaaS token's `room` claim keeps it.
pub const ROOM_JID_CONFIGURED: &str = "StandUp@conference.example.org";
/// The MUC JID as the server spells it — lowercased, the way the conference-request response does.
pub const ROOM_JID_SERVER: &str = "standup@conference.example.org";

/// How long a `wait_for` waits before declaring the client never sent what was expected.
const WAIT: Duration = Duration::from_secs(5);

/// A running double: one WebSocket listener serving **concurrent** connections.
///
/// Concurrent rather than one-at-a-time on purpose: a JaaS session that crosses its guest token's
/// expiry re-mints and *re-joins*, and it opens the new connection before closing the old one
/// (D-206) — a double that accepts serially would deadlock exactly the path under test. Every
/// connection's request URI is recorded, which is how a test proves the token rode the URL and that
/// the re-join carried a fresh one.
pub struct XmppDouble {
    addr: SocketAddr,
    sent: Arc<Mutex<Vec<String>>>,
    connections: Arc<Mutex<Vec<String>>>,
    hold: Arc<AtomicBool>,
    push: broadcast::Sender<String>,
    /// Kept so [`XmppDouble::push`] never fails for want of a subscriber. Never drained: a new
    /// connection subscribes at the tail, so it sees only what is pushed after it arrives.
    _retain: broadcast::Receiver<String>,
    _accept: tokio::task::JoinHandle<()>,
}

impl XmppDouble {
    /// Bind on loopback and start serving. The returned double is ready to be connected to.
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(Vec::new()));
        let (push, _retain) = broadcast::channel::<String>(64);

        let recorded = sent.clone();
        let dialled = connections.clone();
        let pushes = push.clone();
        let occupancy: Occupancy = Arc::new(Mutex::new(HashMap::new()));
        let hold = Arc::new(AtomicBool::new(false));
        let held = hold.clone();
        let accept = tokio::spawn(async move {
            let mut next_id = 0usize;
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                next_id += 1;
                let conn = Conn {
                    id: next_id,
                    occupancy: occupancy.clone(),
                    opens: 0,
                    holds: None,
                };
                let seen = dialled.clone();
                // The `Err` type is tungstenite's `ErrorResponse` and is not ours to shrink.
                #[allow(clippy::result_large_err)]
                let hook = move |req: &WsRequest, resp: WsResponse| {
                    seen.lock().unwrap().push(req.uri().to_string());
                    negotiate(req, resp)
                };
                let recorded = recorded.clone();
                let outbound = pushes.subscribe();
                let held = held.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(sock, hook).await else {
                        return;
                    };
                    // Parked *after* the WebSocket handshake, so the client is connected and waiting
                    // for stream features — the middle of `connect_and_join`. That is the window a
                    // test needs in order to make something else happen during a join.
                    while held.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    serve(&mut ws, outbound, &recorded, conn).await;
                });
            }
        });

        Self {
            addr,
            sent,
            connections,
            hold,
            push,
            _retain,
            _accept: accept,
        }
    }

    /// Park every connection accepted from now on just after its WebSocket handshake — the client
    /// sits inside `connect_and_join` waiting for stream features until [`Self::release`].
    pub fn hold_new_connections(&self) {
        self.hold.store(true, Ordering::SeqCst);
    }

    /// Let held connections proceed.
    pub fn release(&self) {
        self.hold.store(false, Ordering::SeqCst);
    }

    /// The `ws://` endpoint to configure a room with.
    pub fn url(&self) -> String {
        format!("ws://{}/xmpp-websocket", self.addr)
    }

    /// The scheme-and-authority the double is reachable at — what a backend that builds its own path
    /// (the JaaS one appends `/<tenant>/xmpp-websocket`) is configured with.
    pub fn ws_base(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Every connection's request URI, in the order they were accepted.
    pub fn connections(&self) -> Vec<String> {
        self.connections.lock().unwrap().clone()
    }

    /// Block until `n` connections have been accepted. Panics on timeout — "the client never
    /// reconnected" is exactly the failure the refresh test is looking for.
    pub async fn wait_for_connections(&self, n: usize) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if self.connections().len() >= n {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {n} connections; saw {:?}",
                    self.connections()
                );
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
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

/// Serve one accepted connection until it ends.
async fn serve(
    ws: &mut WebSocketStream<TcpStream>,
    mut outbound: broadcast::Receiver<String>,
    recorded: &Arc<Mutex<Vec<String>>>,
    mut conn: Conn,
) {
    loop {
        tokio::select! {
            pushed = outbound.recv() => match pushed {
                Ok(frame) => {
                    if ws.send(WsMessage::Text(frame.into())).await.is_err() {
                        return;
                    }
                }
                // Lagged: this connection missed pushes it was never the target of. Closed: the
                // double is gone.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => { conn.release(); return }
            },
            inbound = ws.next() => {
                let frame = match inbound {
                    Some(Ok(WsMessage::Text(t))) => t.to_string(),
                    // A close, an error, or the end of the socket ends this session — and frees the
                    // nick, as it would on a real service. (A binary or whitespace frame would be a
                    // protocol error on a real server; the keepalive regression test asserts the
                    // client never sends one.)
                    Some(Ok(WsMessage::Close(_))) | None => { conn.release(); return }
                    Some(Err(_)) => { conn.release(); return }
                    Some(Ok(_)) => continue,
                };
                recorded.lock().unwrap().push(frame.clone());
                for reply in replies_to(&frame, &mut conn) {
                    if ws.send(WsMessage::Text(reply.into())).await.is_err() {
                        conn.release();
                        return;
                    }
                }
            }
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

/// Who currently holds which nick in the room, shared across connections.
///
/// **This is the double's occupancy model, and it exists because without one the double is more
/// permissive than the vendor.** Under SASL `ANONYMOUS` every connection is a *different* anonymous
/// JID, so two overlapping sessions asking for the same nick is XEP-0045 §7.2.9's nickname-conflict
/// case — a real MUC answers `<error type='cancel'><conflict/></error>` and refuses the join. A
/// backend that overlaps its sessions therefore passes against a double with no occupancy and fails
/// against the vendor, which is the "guards tested against their own assumptions" trap.
type Occupancy = Arc<Mutex<HashMap<String, usize>>>;

/// One connection's view of the double: its id (so the occupancy map can tell whose nick is whose)
/// and the shared room.
struct Conn {
    id: usize,
    occupancy: Occupancy,
    /// Stream opens on *this* connection: RFC 7395's SASL restart replays `<open/>` and the two get
    /// different feature sets.
    opens: usize,
    /// The nick this connection successfully claimed, released when it goes away.
    holds: Option<String>,
}

impl Conn {
    /// Claim `nick` for this connection. `false` when someone else already holds it.
    fn claim(&mut self, nick: &str) -> bool {
        let mut occupancy = self.occupancy.lock().unwrap();
        match occupancy.get(nick) {
            Some(&owner) if owner != self.id => false,
            _ => {
                occupancy.insert(nick.to_string(), self.id);
                self.holds = Some(nick.to_string());
                true
            }
        }
    }

    /// Release whatever this connection holds — on `<presence type='unavailable'/>`, and on the way
    /// out, because a dropped socket frees a nick on a real service too.
    fn release(&mut self) {
        if let Some(nick) = self.holds.take() {
            let mut occupancy = self.occupancy.lock().unwrap();
            if occupancy.get(&nick) == Some(&self.id) {
                occupancy.remove(&nick);
            }
        }
    }
}

/// The server's answer to one client frame.
fn replies_to(frame: &str, conn: &mut Conn) -> Vec<String> {
    const FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
    const STREAMS: &str = "http://etherx.jabber.org/streams";

    if frame.starts_with("<open") {
        conn.opens += 1;
        let open = format!(
            "<open xmlns='{FRAMING}' from='example.org' id='s1' version='1.0' xml:lang='en'/>"
        );
        let features = if conn.opens == 1 {
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
        // A *distinct* anonymous JID per connection — which is exactly why two overlapping sessions
        // asking for one nick is a conflict rather than a reconnect.
        return vec![format!(
            "<iq xmlns='jabber:client' type='result' id='{id}'>\
             <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
             <jid>guest-{}@example.org/flux</jid></bind></iq>",
            conn.id
        )];
    }

    // Leaving the room frees the nick, so the next session may take it.
    if frame.starts_with("<presence") && frame.contains("type='unavailable'") {
        conn.release();
        return Vec::new();
    }

    if frame.starts_with("<presence") && frame.contains("http://jabber.org/protocol/muc") {
        let nick = attr(frame, "to")
            .and_then(|to| to.rsplit_once('/').map(|(_, n)| n.to_string()))
            .unwrap_or_else(|| "flux".to_string());

        // XEP-0045 §7.2.9: the nick is taken by someone who is not us. This is what a real MUC does
        // to a backend that opens its replacement session before closing the one it is replacing.
        if !conn.claim(&nick) {
            return vec![format!(
                "<presence xmlns='jabber:client' type='error' from='{ROOM_JID_SERVER}/{nick}'>\
                 <error type='cancel'><conflict xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
                 </error></presence>"
            )];
        }

        // The MUC join. Existing occupants first, our own self-presence (status 110) last — the order
        // a real service produces, and the marker a backend must key `is_self` on.
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
