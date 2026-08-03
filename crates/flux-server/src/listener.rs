//! Axum adaptation for the substrate-neutral guarded inbound network port.

use std::io;
use std::net::SocketAddr;

use flux_system::net::{BindExposure, InboundLimits, NetworkListener};
use flux_system::port::GuardedNetwork;

use crate::ServerLimits;

/// Connection-level limits at the HTTP boundary. Complete request bodies, response production,
/// request rate and live work are bounded independently by the router.
pub fn http_inbound_limits(limits: ServerLimits) -> InboundLimits {
    InboundLimits {
        max_connections: limits.max_resource_keys.clamp(1, 256),
        max_frame_bytes: 64 * 1024,
        io_timeout: InboundLimits::default().io_timeout,
    }
}

/// An axum listener whose physical accept/read/write lifecycle stays behind `GuardedNetwork`.
pub struct GuardedHttpListener {
    inner: NetworkListener,
    frame_bytes: usize,
}

impl GuardedHttpListener {
    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        self.inner
            .local_addr()
            .map_err(|error| anyhow::anyhow!("{error}"))
    }
}

impl axum::serve::Listener for GuardedHttpListener {
    type Io = tokio::io::DuplexStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.inner.accept().await {
                Ok((stream, peer)) => return (stream.into_async_io(self.frame_bytes), peer),
                Err(error) => {
                    if !error.to_string().contains("accept timed out") {
                        eprintln!("guarded HTTP listener accept failed: {error}");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner
            .local_addr()
            .map_err(|error| io::Error::other(error.to_string()))
    }
}

/// Bind one HTTP listener through the selected execution substrate.
pub async fn bind_http_listener<N: GuardedNetwork + ?Sized>(
    network: &N,
    addr: SocketAddr,
    exposure: BindExposure,
    limits: ServerLimits,
) -> anyhow::Result<GuardedHttpListener> {
    let inbound = http_inbound_limits(limits);
    let listener = network
        .bind_tcp(addr, exposure, inbound)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(GuardedHttpListener {
        inner: listener,
        frame_bytes: inbound.max_frame_bytes,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use flux_core::{Error, Result};
    use flux_system::net::{NetworkStream, StreamListener};
    use flux_system::port::Guarded;

    use super::*;

    struct RefusingListener {
        accepts: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
    }

    impl StreamListener for RefusingListener {
        fn local_addr(&self) -> Result<SocketAddr> {
            Ok(SocketAddr::from(([127, 0, 0, 1], 1)))
        }

        fn accept<'a>(&'a mut self) -> Guarded<'a, (NetworkStream, SocketAddr)> {
            self.accepts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(Error::Other("accept timed out".into())) })
        }

        fn close(&mut self) {
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn failed_accepts_are_backed_off_and_listener_drop_closes_once() {
        let accepts = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        {
            let mut listener = GuardedHttpListener {
                inner: NetworkListener::from_handle(RefusingListener {
                    accepts: accepts.clone(),
                    closes: closes.clone(),
                }),
                frame_bytes: 64,
            };
            let result = tokio::time::timeout(
                Duration::from_millis(250),
                axum::serve::Listener::accept(&mut listener),
            )
            .await;
            assert!(result.is_err(), "a refusing listener never accepts");
            assert!(
                (2..=4).contains(&accepts.load(Ordering::SeqCst)),
                "accept failures must not hot-loop: {} attempts",
                accepts.load(Ordering::SeqCst)
            );
        }
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }
}
