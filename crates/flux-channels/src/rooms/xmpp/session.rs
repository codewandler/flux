//! The XMPP-over-WebSocket session: the RFC 7395 handshake, the MUC join, and the socket loop that
//! turns stanzas into [`RoomEvent`]s.
//!
//! The frame sequence is the one the 2026-07-30 spike observed live against a JaaS tenant and the
//! design's Feasibility section records: `<open/>` → SASL → `<open/>` → resource bind → MUC presence →
//! `groupchat`. Nothing here is guessed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

use flux_core::{Error, Result};
use flux_system::net::{guard_url_scoped, PrivateNetAllow};

use super::stanza::{
    parse, stanza, Element, NS_BIND, NS_CLIENT, NS_DELAY, NS_FRAMING, NS_MUC, NS_MUC_USER, NS_PING,
    NS_SASL,
};
use super::{endpoint_for_display, XmppConfig};
use crate::rooms::{
    MessageScope, Occupant, OccupantId, OccupantKind, RoomEvent, RoomEventSender, RoomIdentity,
};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The MUC status code that marks the self-presence — the server telling us "this one is you".
/// XEP-0045 requires it in the join reply, and it is the only signal that survives the service
/// assigning a different nick than the one we asked for.
const STATUS_SELF_PRESENCE: &str = "110";

/// Room occupants that are infrastructure rather than participants. Jitsi's conference focus joins
/// every JaaS room and must never be treated as someone to answer.
const SERVICE_NICKS: &[&str] = &["focus"];

/// One frame to write, plus the channel the writer's real result comes back on. A queued frame is
/// **not** a sent frame: `say` has to be able to fail when the socket has gone, which is precisely
/// the case D-204's driver could not survive.
pub(super) struct Outbound {
    pub frame: String,
    pub ack: oneshot::Sender<Result<()>>,
}

/// A joined session's handles: the write path, and the token that ends it. Cheap to clone, so a
/// caller never holds the room's mutex across an `await`.
#[derive(Clone)]
pub(super) struct Session {
    outbound: mpsc::Sender<Outbound>,
    cancel: CancellationToken,
    /// Our own occupant JID, as the **server** assigned it — where unavailable presence is addressed.
    pub self_jid: String,
}

impl Session {
    /// Write one frame and wait for the socket's real answer.
    pub(super) async fn send(&self, element: &Element) -> Result<()> {
        let (ack, answer) = oneshot::channel();
        self.outbound
            .send(Outbound {
                frame: element.to_xml(),
                ack,
            })
            .await
            .map_err(|_| Error::Other("xmpp: the room transport is closed".into()))?;
        answer
            .await
            .map_err(|_| Error::Other("xmpp: the room transport is closed".into()))?
    }

    /// End the session. The socket loop performs the RFC 7395 `<close/>` on its way out.
    pub(super) fn end(&self) {
        self.cancel.cancel();
    }
}

/// What a completed handshake yields to the room.
pub(super) struct Joined {
    pub session: Session,
    /// The room's bare JID **as the server spelled it**. JaaS lowercases the room name while the
    /// guest token's `room` claim keeps the original case, so a locally-rebuilt address is wrong in
    /// exactly the way that is hard to see.
    pub room_jid: String,
}

/// Connect, authenticate, bind, and join the MUC. Returns once our own presence has come back, so a
/// caller that gets `Ok` is genuinely in the room.
pub(super) async fn connect_and_join(
    config: &XmppConfig,
    identity: &RoomIdentity,
    events: RoomEventSender,
    occupants: Arc<Mutex<Vec<Occupant>>>,
) -> Result<Joined> {
    let endpoint = guarded_endpoint(&config.url, &config.private_net)?;
    // Never the endpoint verbatim in a message: a JaaS guest token rides its query string (D-206).
    let shown = endpoint_for_display(&endpoint).to_string();
    let mut request = endpoint
        .as_str()
        .into_client_request()
        .map_err(|e| Error::Other(format!("xmpp: bad endpoint {shown}: {e}")))?;
    // RFC 7395 §3.1: the WebSocket subprotocol identifier for XMPP.
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("xmpp"));
    let (mut ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| Error::Other(format!("xmpp: cannot reach {shown}: {e}")))?;

    let domain = config.domain_or_host();
    open_stream(&mut ws, &domain, config.handshake_timeout).await?;
    authenticate(&mut ws, config).await?;
    // SASL success restarts the stream: a second `<open/>`, and a second feature set carrying bind.
    open_stream(&mut ws, &domain, config.handshake_timeout).await?;
    bind_resource(&mut ws, &identity.nick, config.handshake_timeout).await?;

    let (room_jid, present) = join_muc(&mut ws, config, identity).await?;
    *occupants.lock().unwrap() = present.clone();

    let (outbound_tx, outbound_rx) = mpsc::channel::<Outbound>(16);
    let cancel = CancellationToken::new();
    let self_jid = present
        .iter()
        .find(|o| o.is_self)
        .map(|o| o.id.to_string())
        .unwrap_or_else(|| format!("{room_jid}/{}", identity.nick));

    tokio::spawn(run_socket(
        ws,
        outbound_rx,
        events,
        occupants,
        present,
        SocketContext {
            room_jid: room_jid.clone(),
            self_nick: identity.nick.clone(),
            self_kind: identity.kind,
            domain,
            keepalive: config.keepalive,
            cancel: cancel.clone(),
        },
    ));

    Ok(Joined {
        session: Session {
            outbound: outbound_tx,
            cancel,
            self_jid,
        },
        room_jid,
    })
}

/// Vet the WebSocket endpoint through **the** egress guard.
///
/// `flux_system::net` speaks `http`/`https`, so the endpoint is guarded in its HTTP form and the
/// dialled URL is rebuilt from the guard's own normalized answer — there is no second URL guard here,
/// which is the point. Note the limitation the guard's own docs name: a URL-returning guard does not
/// pin the connection to the vetted addresses, so this closes SSRF-by-configuration, not DNS
/// rebinding. The endpoint is operator configuration, never model output.
fn guarded_endpoint(raw: &str, allow: &PrivateNetAllow) -> Result<String> {
    let (ws_scheme, http_scheme) = match raw.split_once("://") {
        Some(("wss", _)) => ("wss", "https"),
        Some(("ws", _)) => ("ws", "http"),
        _ => {
            return Err(Error::Other(format!(
                "xmpp: endpoint must be a ws:// or wss:// url, got {}",
                endpoint_for_display(raw)
            )))
        }
    };
    let probe = format!("{http_scheme}{}", &raw[ws_scheme.len()..]);
    let guarded = guard_url_scoped(&probe, allow)?;
    Ok(format!(
        "{ws_scheme}{}",
        &guarded.as_str()[http_scheme.len()..]
    ))
}

/// Send `<open/>` and read the stream features the server answers with.
async fn open_stream(ws: &mut Ws, domain: &str, timeout: Duration) -> Result<Element> {
    let open = Element::new("open", NS_FRAMING)
        .attr("to", domain)
        .attr("version", "1.0");
    write(ws, &open).await?;
    read_until(ws, timeout, "stream features", |e| e.name() == "features").await
}

/// SASL. `PLAIN` when the config carries a user, `ANONYMOUS` otherwise — the only mechanism JaaS
/// offers, where authorization rides the endpoint URL rather than SASL.
async fn authenticate(ws: &mut Ws, config: &XmppConfig) -> Result<()> {
    let auth = match (&config.user, &config.password) {
        (Some(user), password) => {
            let payload = format!("\0{user}\0{}", password.as_deref().unwrap_or_default());
            Element::new("auth", NS_SASL)
                .attr("mechanism", "PLAIN")
                .text(base64::engine::general_purpose::STANDARD.encode(payload))
        }
        (None, _) => Element::new("auth", NS_SASL).attr("mechanism", "ANONYMOUS"),
    };
    write(ws, &auth).await?;

    let answer = read_until(ws, config.handshake_timeout, "a SASL reply", |e| {
        matches!(e.name(), "success" | "failure")
    })
    .await?;
    if answer.name() == "failure" {
        // The condition element names *why* (`not-authorized`, `invalid-mechanism`, …). The
        // credential itself is never part of this message.
        let condition = answer
            .children()
            .next()
            .map(|c| c.name().to_string())
            .unwrap_or_else(|| "unspecified".to_string());
        return Err(Error::Other(format!(
            "xmpp: authentication refused ({condition})"
        )));
    }
    Ok(())
}

/// Bind a resource. The nick is asked for as the resource so a server-side log names the agent.
async fn bind_resource(ws: &mut Ws, nick: &str, timeout: Duration) -> Result<()> {
    let id = "flux-bind";
    let iq = stanza("iq")
        .attr("type", "set")
        .attr("id", id)
        .child(Element::new("bind", NS_BIND).child(Element::new("resource", NS_BIND).text(nick)));
    write(ws, &iq).await?;

    let answer = read_until(ws, timeout, "the bind result", |e| {
        e.name() == "iq" && e.get("id") == Some(id)
    })
    .await?;
    if answer.get("type") == Some("error") {
        return Err(Error::Other("xmpp: resource bind refused".into()));
    }
    Ok(())
}

/// Send MUC presence and read the occupant replay until our own presence comes back.
async fn join_muc(
    ws: &mut Ws,
    config: &XmppConfig,
    identity: &RoomIdentity,
) -> Result<(String, Vec<Occupant>)> {
    let muc = Element::new("x", NS_MUC).maybe_child(
        config
            .muc_password
            .as_ref()
            .map(|p| Element::new("password", NS_MUC).text(p)),
    );
    let presence = stanza("presence")
        .attr("to", format!("{}/{}", config.room, identity.nick))
        .child(muc);
    write(ws, &presence).await?;

    let mut present: Vec<Occupant> = Vec::new();
    loop {
        let element = read_until(
            ws,
            config.handshake_timeout,
            "the MUC presence replay",
            |e| e.name() == "presence",
        )
        .await?;
        if element.get("type") == Some("error") {
            let condition = element
                .child_named("error")
                .and_then(|e| e.children().next().map(|c| c.name().to_string()))
                .unwrap_or_else(|| "unspecified".to_string());
            return Err(Error::Other(format!(
                "xmpp: the room refused the join ({condition})"
            )));
        }
        let Some(occupant) = occupant_from_presence(&element, &identity.nick, identity.kind) else {
            continue;
        };
        let is_self = occupant.is_self;
        present.retain(|o| o.id != occupant.id);
        present.push(occupant);
        // Our own presence terminates the replay: XEP-0045 sends it last, after every occupant that
        // was already in the room.
        if is_self {
            break;
        }
    }

    let room_jid = present
        .iter()
        .find(|o| o.is_self)
        .and_then(|o| bare_jid(o.id.as_str()))
        .ok_or_else(|| Error::Other("xmpp: the room never acknowledged our presence".into()))?;
    Ok((room_jid, present))
}

/// Everything the socket loop needs to interpret a stanza.
struct SocketContext {
    room_jid: String,
    self_nick: String,
    self_kind: OccupantKind,
    domain: String,
    keepalive: Duration,
    cancel: CancellationToken,
}

/// The running session: read stanzas out, write queued frames in, ping on a timer.
async fn run_socket(
    mut ws: Ws,
    mut outbound: mpsc::Receiver<Outbound>,
    events: RoomEventSender,
    occupants: Arc<Mutex<Vec<Occupant>>>,
    initial: Vec<Occupant>,
    ctx: SocketContext,
) {
    // The presence replay reaches the consumer as `Joined`, ours included — the port's contract that a
    // consumer needs no separate priming call. Sent from here (not from `join`) because the event
    // channel is bounded and nobody is reading it until `join` has returned.
    for occupant in initial {
        if events.send(RoomEvent::Joined { occupant }).await.is_err() {
            return;
        }
    }

    let mut ping = tokio::time::interval(ctx.keepalive);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // the first tick is immediate
    let mut pings = 0u64;

    // `true` once we left deliberately (or the consumer went away): our own departure is not news to
    // the consumer, so it ends the stream rather than announcing `Ended`. The transport dying is.
    let left = 'session: loop {
        tokio::select! {
            _ = ctx.cancel.cancelled() => break 'session true,
            queued = outbound.recv() => {
                // `None` means the room itself was dropped without leaving.
                let Some(Outbound { frame, ack }) = queued else { break 'session true };
                let result = ws
                    .send(WsMessage::Text(frame.into()))
                    .await
                    .map_err(|e| Error::Other(format!("xmpp: send failed: {e}")));
                let failed = result.is_err();
                // The writer's real answer, not "it was queued" — `say` has to be able to fail.
                let _ = ack.send(result);
                if failed {
                    break 'session false;
                }
            }
            _ = ping.tick() => {
                // **Never** a whitespace frame: the WebSocket binding closes a `" "` with
                // `1007 Invalid payload start character`. XEP-0199 ping it is.
                pings += 1;
                let iq = stanza("iq")
                    .attr("type", "get")
                    .attr("id", format!("flux-ping-{pings}"))
                    .attr("to", &ctx.domain)
                    .child(Element::new("ping", NS_PING));
                if ws.send(WsMessage::Text(iq.to_xml().into())).await.is_err() {
                    break 'session false;
                }
            }
            frame = ws.next() => {
                let text = match frame {
                    Some(Ok(WsMessage::Text(t))) => t.to_string(),
                    // A close, an error, or the end of the socket is the room ending under us.
                    Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break 'session false,
                    // Ping/pong/binary/raw frames carry no stanza.
                    Some(Ok(_)) => continue,
                };
                // One unparseable frame is a diagnostic, not a reason to walk out of a room with
                // people in it — the same posture the provider codecs take for a bad envelope.
                let Ok(element) = parse(&text) else { continue };
                for event in interpret(&element, &ctx, &occupants) {
                    if events.send(event).await.is_err() {
                        // Nobody is listening any more; there is no room left to be in.
                        break 'session true;
                    }
                }
            }
        }
    };

    if left {
        // RFC 7395's stream close, then the WebSocket close — in that order.
        let close = Element::new("close", NS_FRAMING);
        let _ = ws.send(WsMessage::Text(close.to_xml().into())).await;
    } else {
        let _ = events.send(RoomEvent::Ended).await;
    }
    let _ = ws.close(None).await;
    // Dropping `events` ends the consumer's stream.
}

/// Map one inbound stanza onto room events, updating the tracked occupant list as presence moves.
fn interpret(
    element: &Element,
    ctx: &SocketContext,
    occupants: &Arc<Mutex<Vec<Occupant>>>,
) -> Vec<RoomEvent> {
    // Strict on write, lenient on read: flux always qualifies its own stanzas (`jabber:client` is
    // what makes them acceptable), but a server that leans on the stream's default namespace still
    // gets understood. Anything in a *different* namespace is not a client stanza.
    if !matches!(element.ns(), None | Some(NS_CLIENT)) {
        return Vec::new();
    }
    match element.name() {
        "presence" => {
            let Some(from) = element.get("from") else {
                return Vec::new();
            };
            if !in_this_room(from, &ctx.room_jid) {
                return Vec::new();
            }
            if element.get("type") == Some("unavailable") {
                let id = OccupantId::new(from);
                occupants.lock().unwrap().retain(|o| o.id != id);
                return vec![RoomEvent::Left { occupant: id }];
            }
            let Some(occupant) = occupant_from_presence(element, &ctx.self_nick, ctx.self_kind)
            else {
                return Vec::new();
            };
            let mut tracked = occupants.lock().unwrap();
            match tracked.iter_mut().find(|o| o.id == occupant.id) {
                // A presence update for someone already here (a role change, a status) is not a
                // second join.
                Some(existing) => {
                    *existing = occupant;
                    Vec::new()
                }
                None => {
                    tracked.push(occupant.clone());
                    vec![RoomEvent::Joined { occupant }]
                }
            }
        }
        "message" => {
            let Some(from) = element.get("from") else {
                return Vec::new();
            };
            if !in_this_room(from, &ctx.room_jid) {
                return Vec::new();
            }
            // A `<delay/>` marks the MUC's history replay. Answering messages that were said before
            // flux arrived is the same unbounded-cost mistake as answering our own echo.
            if element.has_descendant_in(NS_DELAY) {
                return Vec::new();
            }
            // A subject change or a chat-state notification carries no body and is not a turn.
            let Some(body) = element.child_named("body") else {
                return Vec::new();
            };
            let scope = match element.get("type") {
                Some("groupchat") => MessageScope::Groupchat,
                // XEP-0045 private messages are `type='chat'` addressed to an occupant JID. A message
                // with no type defaults to `normal` and is likewise one-to-one.
                _ => MessageScope::Private,
            };
            vec![RoomEvent::Message {
                from: OccupantId::new(from),
                text: body.body_text().to_string(),
                scope,
            }]
        }
        _ => Vec::new(),
    }
}

/// Whether a stanza's `from` addresses an occupant of *our* room.
fn in_this_room(from: &str, room_jid: &str) -> bool {
    bare_jid(from).is_some_and(|bare| bare.eq_ignore_ascii_case(room_jid))
}

/// One occupant, read out of a presence stanza. `None` when the stanza names no occupant JID.
///
/// **`is_self` is decided here and never defaulted.** `Occupant::new` leaves it `false`, so a backend
/// that forgets makes the agent answer its own echoed messages forever. Two independent signals settle
/// it: XEP-0045's `<status code='110'/>` (authoritative, and survives the service assigning us a
/// different nick), and the nick we asked to join under (which covers a server that omits the code).
fn occupant_from_presence(
    element: &Element,
    self_nick: &str,
    self_kind: OccupantKind,
) -> Option<Occupant> {
    let from = element.get("from")?;
    let nick = resource(from)?.to_string();
    let statuses: Vec<&str> = element
        .child_in(NS_MUC_USER)
        .map(|x| {
            x.children()
                .filter(|c| c.name() == "status")
                .filter_map(|c| c.get("code"))
                .collect()
        })
        .unwrap_or_default();
    let is_self = statuses.contains(&STATUS_SELF_PRESENCE) || nick == self_nick;

    let kind = if is_self {
        self_kind
    } else if SERVICE_NICKS.contains(&nick.as_str()) {
        OccupantKind::Service
    } else {
        // XMPP presence carries no "human or bot" signal, and inventing one would be worse than
        // admitting we cannot tell. D-207 reads this and treats `Unknown` as untrusted.
        OccupantKind::Unknown
    };

    let occupant = Occupant::new(from, nick, kind);
    Some(if is_self {
        occupant.as_self()
    } else {
        occupant
    })
}

/// The bare JID (`room@service`) of a full JID.
fn bare_jid(jid: &str) -> Option<String> {
    Some(match jid.split_once('/') {
        Some((bare, _)) => bare.to_string(),
        None => jid.to_string(),
    })
}

/// The resource part (`/nick`) of a full JID.
fn resource(jid: &str) -> Option<&str> {
    jid.split_once('/').map(|(_, resource)| resource)
}

/// Write one element as a single frame.
async fn write(ws: &mut Ws, element: &Element) -> Result<()> {
    ws.send(WsMessage::Text(element.to_xml().into()))
        .await
        .map_err(|e| Error::Other(format!("xmpp: send failed: {e}")))
}

/// Read frames until one satisfies `want`, or the handshake budget runs out. `what` names the
/// expectation so a stalled handshake says which step it stalled on.
async fn read_until(
    ws: &mut Ws,
    budget: Duration,
    what: &str,
    want: impl Fn(&Element) -> bool,
) -> Result<Element> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_| Error::Other(format!("xmpp: timed out waiting for {what}")))?;
        let text = match frame {
            Some(Ok(WsMessage::Text(t))) => t.to_string(),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(Error::Other(format!("xmpp: transport failed: {e}"))),
            None => {
                return Err(Error::Other(format!(
                    "xmpp: the stream closed while waiting for {what}"
                )))
            }
        };
        let element = parse(&text)?;
        if element.name() == "close" {
            return Err(Error::Other(format!(
                "xmpp: the server closed the stream while waiting for {what}"
            )));
        }
        if want(&element) {
            return Ok(element);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SocketContext {
        SocketContext {
            room_jid: "standup@conference.example.org".into(),
            self_nick: "flux".into(),
            self_kind: OccupantKind::Agent,
            domain: "example.org".into(),
            keepalive: Duration::from_secs(30),
            cancel: CancellationToken::new(),
        }
    }

    fn events(xml: &str, tracked: &Arc<Mutex<Vec<Occupant>>>) -> Vec<RoomEvent> {
        interpret(&parse(xml).unwrap(), &ctx(), tracked)
    }

    #[test]
    fn the_endpoint_is_guarded_and_loopback_needs_a_grant() {
        // The one egress guard, reached through its http form. A loopback endpoint is refused unless
        // the caller holds a scoped grant — no second URL guard, no exception carved for XMPP.
        let refused =
            guarded_endpoint("ws://127.0.0.1:5280/xmpp-websocket", &PrivateNetAllow::None);
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(
            guarded_endpoint("ws://127.0.0.1:5280/xmpp-websocket", &PrivateNetAllow::Any).unwrap(),
            "ws://127.0.0.1:5280/xmpp-websocket"
        );
        assert!(guarded_endpoint("https://example.org/x", &PrivateNetAllow::Any).is_err());
        assert!(guarded_endpoint("standup@conference.example.org", &PrivateNetAllow::Any).is_err());
    }

    #[test]
    fn the_self_presence_is_recognized_by_status_110() {
        let me = occupant_from_presence(
            &parse(
                "<presence xmlns='jabber:client' from='standup@conference.example.org/agent-7'>\
                 <x xmlns='http://jabber.org/protocol/muc#user'><status code='110'/></x></presence>",
            )
            .unwrap(),
            "flux",
            OccupantKind::Agent,
        )
        .unwrap();
        // The service handed us a different nick than we asked for; status 110 still names us.
        assert!(me.is_self, "status 110 is authoritative over the nick");
        assert_eq!(me.nick, "agent-7");
        assert_eq!(me.kind, OccupantKind::Agent);
    }

    #[test]
    fn the_joined_nick_recognizes_us_when_the_server_omits_status_110() {
        let me = occupant_from_presence(
            &parse("<presence xmlns='jabber:client' from='standup@conference.example.org/flux'/>")
                .unwrap(),
            "flux",
            OccupantKind::Agent,
        )
        .unwrap();
        assert!(me.is_self, "the second, independent self signal");
    }

    #[test]
    fn another_occupant_is_never_marked_as_us_and_focus_is_service() {
        let timo = occupant_from_presence(
            &parse("<presence xmlns='jabber:client' from='standup@conference.example.org/timo'/>")
                .unwrap(),
            "flux",
            OccupantKind::Agent,
        )
        .unwrap();
        assert!(!timo.is_self);
        assert_eq!(timo.kind, OccupantKind::Unknown, "XMPP cannot tell us more");

        let focus = occupant_from_presence(
            &parse("<presence xmlns='jabber:client' from='standup@conference.example.org/focus'/>")
                .unwrap(),
            "flux",
            OccupantKind::Agent,
        )
        .unwrap();
        assert_eq!(focus.kind, OccupantKind::Service);
    }

    #[test]
    fn groupchat_and_private_messages_keep_their_scope() {
        let tracked = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            events(
                "<message xmlns='jabber:client' type='groupchat' \
                 from='standup@conference.example.org/timo'><body>hi</body></message>",
                &tracked
            ),
            vec![RoomEvent::Message {
                from: OccupantId::new("standup@conference.example.org/timo"),
                text: "hi".into(),
                scope: MessageScope::Groupchat,
            }]
        );
        assert_eq!(
            events(
                "<message xmlns='jabber:client' type='chat' \
                 from='standup@conference.example.org/timo'><body>psst</body></message>",
                &tracked
            ),
            vec![RoomEvent::Message {
                from: OccupantId::new("standup@conference.example.org/timo"),
                text: "psst".into(),
                scope: MessageScope::Private,
            }]
        );
    }

    #[test]
    fn history_subjects_and_foreign_rooms_are_not_turns() {
        let tracked = Arc::new(Mutex::new(Vec::new()));
        assert!(
            events(
                "<message xmlns='jabber:client' type='groupchat' \
                 from='standup@conference.example.org/timo'><body>old</body>\
                 <delay xmlns='urn:xmpp:delay' stamp='2026-07-30T09:00:00Z'/></message>",
                &tracked
            )
            .is_empty(),
            "history replay is not a turn"
        );
        assert!(
            events(
                "<message xmlns='jabber:client' type='groupchat' \
                 from='standup@conference.example.org/timo'><subject>standup</subject></message>",
                &tracked
            )
            .is_empty(),
            "a subject change carries no body"
        );
        assert!(
            events(
                "<message xmlns='jabber:client' type='groupchat' \
                 from='other@conference.example.org/timo'><body>hi</body></message>",
                &tracked
            )
            .is_empty(),
            "a stanza from another room is not ours"
        );
    }

    #[test]
    fn presence_tracks_arrivals_updates_and_departures() {
        let tracked = Arc::new(Mutex::new(Vec::new()));
        let join = "<presence xmlns='jabber:client' from='standup@conference.example.org/timo'/>";
        assert!(matches!(
            events(join, &tracked).as_slice(),
            [RoomEvent::Joined { .. }]
        ));
        assert_eq!(tracked.lock().unwrap().len(), 1);
        assert!(
            events(join, &tracked).is_empty(),
            "a presence update for someone already here is not a second join"
        );
        assert_eq!(tracked.lock().unwrap().len(), 1);

        assert_eq!(
            events(
                "<presence xmlns='jabber:client' type='unavailable' \
                 from='standup@conference.example.org/timo'/>",
                &tracked
            ),
            vec![RoomEvent::Left {
                occupant: OccupantId::new("standup@conference.example.org/timo")
            }]
        );
        assert!(tracked.lock().unwrap().is_empty());
    }
}
