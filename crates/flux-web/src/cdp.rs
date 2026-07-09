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
    pub fn connect<R, W>(reader: R, writer: W) -> (Arc<Self>, mpsc::UnboundedReceiver<CdpEvent>)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
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
    ev_tx: mpsc::UnboundedSender<CdpEvent>,
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) => break, // EOF — Chrome closed the pipe
            Ok(n) => n,
            Err(_) => break,
        };
        buf.extend_from_slice(&chunk[..n]);
        while let Some(pos) = buf.iter().position(|&b| b == 0) {
            let frame: Vec<u8> = buf.drain(..=pos).collect();
            let frame = &frame[..frame.len() - 1]; // strip the NUL
            if frame.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_slice::<Value>(frame) {
                dispatch(v, &pending, &ev_tx).await;
            }
            // Unparseable frames are skipped, not fatal (the stream-resilience discipline).
        }
    }
    // Transport gone: fail every in-flight call so no caller hangs.
    pending.lock().await.clear();
}

async fn dispatch(v: Value, pending: &Pending, ev_tx: &mpsc::UnboundedSender<CdpEvent>) {
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
        let _ = ev_tx.send(CdpEvent {
            method: method.to_string(),
            params: v.get("params").cloned().unwrap_or(Value::Null),
            session_id: v
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
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
        mpsc::UnboundedReceiver<CdpEvent>,
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
    async fn transport_close_fails_pending_calls_instead_of_hanging() {
        let (client, _events, chrome_r, chrome_w) = wired();
        // Drop the fake-Chrome ends → the client transport hits EOF.
        drop(chrome_r);
        drop(chrome_w);
        let err = client.call("Target.getTargets", json!({})).await;
        assert!(err.is_err(), "a call after disconnect must error, not hang");
    }
}
