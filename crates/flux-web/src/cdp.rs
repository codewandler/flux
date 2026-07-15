//! A minimal, hand-rolled Chrome DevTools Protocol (CDP) client.
//!
//! Chrome's `--remote-debugging-pipe` speaks JSON-RPC framed by NUL bytes over a pair of file
//! descriptors — no WebSocket, no network control socket, no debug port to squat. This client is the
//! flux-tradition minimal transport (the hand-rolled SigV4/SCRAM lineage), typed for only the domains
//! the epic uses. It is **transport-agnostic**: [`CdpClient::connect`] takes any
//! `AsyncRead`/`AsyncWrite` pair, so the browser code drives the real Chrome pipe while tests drive an
//! in-memory duplex against a scripted fake — no Chrome needed in CI.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex};

use flux_core::{Error, Result};

/// Hard cap on a single `\0`-framed message the reader will buffer. A hostile page drives the CDP
/// stream (event params, `Runtime.evaluate` return values, AX trees), so a frame with no terminator
/// would otherwise grow the reader's accumulation buffer without bound and OOM the host. Frames past
/// this cap are dropped and the stream resynchronises to the next `\0` — bounding memory to one cap.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Bound on the buffered-event channel. The event stream is drained by the browser pump, but a page
/// can emit events (console/log/lifecycle/fetch) far faster than the pump consumes them; an unbounded
/// channel would let a hostile page OOM the host through the queue. When the channel is full the
/// reader drops the event rather than blocking — blocking here would wedge response correlation (the
/// pump awaits CDP responses that flow through this same reader), so drop-on-full is the safe bound.
const EVENT_CHANNEL_CAP: usize = 4096;

/// A protocol event: a `method` (e.g. `"Page.loadEventFired"`), its `params`, and the `sessionId` it
/// arrived on (empty for browser-level events under CDP's flattened session mode).
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: String,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>;

/// A CDP client over a `\0`-framed JSON transport. Cheap to `Arc`-share; every call is correlated by
/// a monotonically increasing id, and unsolicited events are forwarded to the receiver returned by
/// [`connect`](Self::connect).
pub struct CdpClient {
    next_id: AtomicI64,
    pending: Pending,
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
}

impl CdpClient {
    /// Wire the client to a transport pair, spawning a background reader task that correlates
    /// responses to their calls and forwards events to the returned receiver. The reader task (and
    /// the event stream) end when the transport reaches EOF; any in-flight calls then resolve to an
    /// error rather than hanging.
    pub fn connect<R, W>(reader: R, writer: W) -> (Arc<Self>, mpsc::Receiver<CdpEvent>)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (ev_tx, ev_rx) = mpsc::channel(EVENT_CHANNEL_CAP);
        let client = Arc::new(Self {
            next_id: AtomicI64::new(1),
            pending: pending.clone(),
            writer: Mutex::new(Box::new(writer)),
        });
        tokio::spawn(read_loop(reader, pending, ev_tx));
        (client, ev_rx)
    }

    /// Send a browser-level command and await its result (or the CDP error it returned).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_on(method, params, None).await
    }

    /// Send a command, optionally routed to a page `sessionId` (CDP flattened mode).
    pub async fn call_on(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(sid) = session_id {
            msg["sessionId"] = json!(sid);
        }
        let mut bytes =
            serde_json::to_vec(&msg).map_err(|e| Error::Other(format!("cdp encode: {e}")))?;
        bytes.push(0); // NUL frame terminator

        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(&bytes).await {
                self.pending.lock().await.remove(&id);
                return Err(Error::Other(format!("cdp write: {e}")));
            }
            let _ = w.flush().await;
        }

        match rx.await {
            Ok(result) => result,
            Err(_) => Err(Error::Other(
                "cdp: connection closed before response".into(),
            )),
        }
    }
}

/// Read `\0`-framed messages off the transport, correlate responses to pending calls, forward events.
async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    pending: Pending,
    ev_tx: mpsc::Sender<CdpEvent>,
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    // Leading bytes of `buf` already scanned and known to contain no terminator. Scanning only the
    // freshly-read tail keeps framing O(total bytes) instead of O(n²) rescans as one frame grows —
    // which for a large frame is itself a CPU-DoS on a hostile stream.
    let mut scanned = 0usize;
    // When a frame exceeds `MAX_FRAME_BYTES` we drop it and skip bytes until the next `\0` so the
    // stream re-synchronises on the following frame instead of the reader OOMing on one huge message.
    let mut resyncing = false;
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => break, // EOF — Chrome closed the pipe
            Ok(n) => n,
            Err(_) => break,
        };
        buf.extend_from_slice(&chunk[..n]);
        loop {
            // Search only the not-yet-scanned tail for the next terminator.
            let found = buf[scanned..]
                .iter()
                .position(|&b| b == 0)
                .map(|p| scanned + p);
            match found {
                Some(pos) => {
                    if resyncing {
                        // Drop the abandoned over-cap frame up to and including this terminator.
                        buf.drain(..=pos);
                        resyncing = false;
                        scanned = 0;
                        continue;
                    }
                    let frame: Vec<u8> = buf.drain(..=pos).collect();
                    scanned = 0;
                    let frame = &frame[..frame.len() - 1]; // strip the NUL
                    if frame.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_slice::<Value>(frame) {
                        dispatch(v, &pending, &ev_tx).await;
                    }
                    // Unparseable frames are skipped, not fatal (the stream-resilience discipline).
                }
                None => {
                    // No terminator in what we've read. Everything buffered is now scanned; if the
                    // partial frame already blew the cap it can never be a frame we accept — drop it
                    // and resync to the next terminator so the reader can't be driven to OOM.
                    scanned = buf.len();
                    if buf.len() > MAX_FRAME_BYTES {
                        if !resyncing {
                            eprintln!(
                                "cdp: dropping over-cap frame (> {MAX_FRAME_BYTES} bytes) and resyncing"
                            );
                        }
                        buf.clear();
                        scanned = 0;
                        resyncing = true;
                    }
                    break;
                }
            }
        }
    }
    // Transport gone: fail every in-flight call so no caller hangs.
    pending.lock().await.clear();
}

async fn dispatch(v: Value, pending: &Pending, ev_tx: &mpsc::Sender<CdpEvent>) {
    if let Some(id) = v.get("id").and_then(Value::as_i64) {
        if let Some(tx) = pending.lock().await.remove(&id) {
            let result = if let Some(err) = v.get("error") {
                let msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                Err(Error::Other(format!("cdp error: {msg}")))
            } else {
                Ok(v.get("result").cloned().unwrap_or(Value::Null))
            };
            let _ = tx.send(result);
        }
    } else if let Some(method) = v.get("method").and_then(Value::as_str) {
        // Non-blocking send: blocking here would wedge response correlation, since the pump that
        // drains this channel awaits CDP responses that flow through this same reader. A full or
        // closed channel drops the event (the bound that keeps a chatty/hostile page from OOMing us).
        let ev = CdpEvent {
            method: method.to_string(),
            params: v.get("params").cloned().unwrap_or(Value::Null),
            session_id: v
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = ev_tx.try_send(ev) {
            eprintln!("cdp: event channel full ({EVENT_CHANNEL_CAP}) — dropping {method}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};

    /// Read one `\0`-framed message off a transport half.
    async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Value {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = r.read(&mut byte).await.unwrap();
            if n == 0 {
                panic!("eof before frame");
            }
            if byte[0] == 0 {
                break;
            }
            buf.push(byte[0]);
        }
        serde_json::from_slice(&buf).unwrap()
    }

    async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, v: &Value) {
        let mut bytes = serde_json::to_vec(v).unwrap();
        bytes.push(0);
        w.write_all(&bytes).await.unwrap();
        w.flush().await.unwrap();
    }

    /// Connect a client to one end of an in-memory duplex, returning the client, its event stream,
    /// and the "fake Chrome" read/write halves.
    fn wired() -> (
        Arc<CdpClient>,
        mpsc::Receiver<CdpEvent>,
        ReadHalf<tokio::io::DuplexStream>,
        WriteHalf<tokio::io::DuplexStream>,
    ) {
        let (client_side, chrome_side) = tokio::io::duplex(64 * 1024);
        let (cr, cw) = tokio::io::split(client_side);
        let (chrome_r, chrome_w) = tokio::io::split(chrome_side);
        let (client, events) = CdpClient::connect(cr, cw);
        (client, events, chrome_r, chrome_w)
    }

    #[tokio::test]
    async fn call_correlates_response_and_forwards_events() {
        let (client, mut events, mut chrome_r, mut chrome_w) = wired();
        tokio::spawn(async move {
            let req = read_frame(&mut chrome_r).await;
            assert_eq!(req["method"], "Target.getTargets");
            let id = req["id"].as_i64().unwrap();
            write_frame(
                &mut chrome_w,
                &json!({ "id": id, "result": { "targetInfos": [] } }),
            )
            .await;
            // Then an unsolicited event.
            write_frame(
                &mut chrome_w,
                &json!({ "method": "Target.targetCreated", "params": { "targetInfo": { "targetId": "t1" } } }),
            )
            .await;
            // Keep the transport open so the reader task doesn't hit EOF mid-test.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let result = client.call("Target.getTargets", json!({})).await.unwrap();
        assert_eq!(result["targetInfos"], json!([]));

        let ev = events.recv().await.unwrap();
        assert_eq!(ev.method, "Target.targetCreated");
        assert_eq!(ev.params["targetInfo"]["targetId"], "t1");
    }

    #[tokio::test]
    async fn cdp_error_response_is_surfaced_as_err() {
        let (client, _events, mut chrome_r, mut chrome_w) = wired();
        tokio::spawn(async move {
            let req = read_frame(&mut chrome_r).await;
            let id = req["id"].as_i64().unwrap();
            write_frame(
                &mut chrome_w,
                &json!({ "id": id, "error": { "code": -32000, "message": "no such target" } }),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });
        let err = client
            .call("Target.attachToTarget", json!({ "targetId": "bogus" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no such target"),
            "surfaces CDP message: {err}"
        );
    }

    #[tokio::test]
    async fn session_routing_stamps_session_id() {
        let (client, _events, mut chrome_r, mut chrome_w) = wired();
        tokio::spawn(async move {
            let req = read_frame(&mut chrome_r).await;
            assert_eq!(req["sessionId"], "S1", "session id routed on the wire");
            let id = req["id"].as_i64().unwrap();
            write_frame(&mut chrome_w, &json!({ "id": id, "result": {} })).await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });
        client
            .call_on("Page.enable", json!({}), Some("S1"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn over_cap_frame_is_dropped_and_stream_resyncs() {
        // A hostile page can drive an unterminated (or absurdly large) frame at the reader. The cap
        // must drop it and resync to the next `\0` so a following well-formed frame is still handled
        // — the reader never buffers the whole over-cap frame (bounding memory to one cap).
        let (client, _events, mut chrome_r, mut chrome_w) = wired();
        tokio::spawn(async move {
            // An over-cap run of bytes with NO terminator — the reader must abandon it at the cap
            // rather than buffer it whole. Then a terminator closes the dropped frame out.
            let junk = vec![b'x'; MAX_FRAME_BYTES + 64 * 1024];
            chrome_w.write_all(&junk).await.unwrap();
            chrome_w.write_all(&[0u8]).await.unwrap();
            chrome_w.flush().await.unwrap();
            // Then a valid response to the pending call — must survive the drop + resync.
            let req = read_frame(&mut chrome_r).await;
            let id = req["id"].as_i64().unwrap();
            write_frame(
                &mut chrome_w,
                &json!({ "id": id, "result": { "ok": true } }),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.call("Target.getTargets", json!({})),
        )
        .await
        .expect("call must not hang after an over-cap frame")
        .unwrap();
        assert_eq!(
            result["ok"],
            json!(true),
            "the frame after the dropped over-cap frame is still delivered"
        );
    }

    #[tokio::test]
    async fn saturated_event_channel_does_not_wedge_response_correlation() {
        // The event channel is bounded. The reader must DROP events when it is full, never block —
        // blocking would wedge response correlation (the pump awaits CDP responses that flow through
        // this same reader). We never drain `events`, flood past the cap, then require a call to
        // still resolve. A naive back-pressure (blocking `send().await`) bound would deadlock here.
        let (client, _events, mut chrome_r, mut chrome_w) = wired();
        tokio::spawn(async move {
            for i in 0..(EVENT_CHANNEL_CAP + 500) {
                write_frame(
                    &mut chrome_w,
                    &json!({ "method": "Runtime.consoleAPICalled", "params": { "i": i } }),
                )
                .await;
            }
            let req = read_frame(&mut chrome_r).await;
            let id = req["id"].as_i64().unwrap();
            write_frame(
                &mut chrome_w,
                &json!({ "id": id, "result": { "ok": true } }),
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.call("Target.getTargets", json!({})),
        )
        .await
        .expect("a full event channel must not block response correlation")
        .unwrap();
        assert_eq!(result["ok"], json!(true));
    }

    #[tokio::test]
    async fn transport_close_fails_pending_calls_instead_of_hanging() {
        let (client, _events, chrome_r, chrome_w) = wired();
        // Drop the fake-Chrome ends → the client transport hits EOF.
        drop(chrome_r);
        drop(chrome_w);
        let err = client.call("Target.getTargets", json!({})).await;
        assert!(err.is_err(), "a call after disconnect must error, not hang");
    }
}
