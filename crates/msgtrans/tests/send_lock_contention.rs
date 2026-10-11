//! Lifecycle operations must NOT queue behind senders parked on a full
//! outbound queue.
//!
//! The send path used to hold the transport's connection lock across the
//! enqueue's backpressure wait (up to `SEND_QUEUE_WAIT`, 100ms). With many
//! concurrent senders on a saturated queue, `disconnect()`/`shutdown()` — which
//! need the same lock — could be parked behind all of them, so a "graceful"
//! shutdown deadline was decided by sender contention rather than by the
//! deadline itself. 2.0 clones a `ConnectionWriter` under the lock and releases
//! it before awaiting, so lifecycle work stays responsive under saturation.

#![cfg(feature = "tcp")]

use async_trait::async_trait;
use msgtrans::{
    ClientLimits, Packet, SessionHandler, SessionId, SessionSender, TcpClientConfig,
    TcpServerConfig, TransportClientBuilder, TransportServerBuilder,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A server that never reads fast enough to matter here; the client-side
/// outbound queue is what we are saturating.
struct Idle;

#[async_trait]
impl SessionHandler for Idle {
    async fn on_request(&self, _s: SessionId, _p: Packet, _r: msgtrans::Responder) {}
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {
        // Deliberately slow: the client's queue backs up behind it.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_is_not_parked_behind_saturated_senders() {
    let addr = "127.0.0.1:28921";
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("server cfg"))
        .build(Arc::new(Idle))
        .await
        .expect("server builds");
    let serving = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.serve().await;
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A tiny outbound queue so a handful of senders saturate it immediately.
    let mut client = TransportClientBuilder::new()
        .protocol(
            TcpClientConfig::new(addr)
                .expect("client cfg")
                .connect_timeout(Duration::from_secs(5)),
        )
        .limits(ClientLimits::new().outbound_queue_capacity(1))
        .build()
        .await
        .expect("client builds");
    client.connect().await.expect("connect");

    // Saturate: many concurrent senders, each big enough that the writer task
    // cannot drain them promptly. These are expected to succeed OR fail with a
    // queue-full resource error — either is fine; the point is that they are
    // parked in the enqueue while we try a lifecycle operation.
    let payload = vec![0u8; 64 * 1024];
    let mut senders = Vec::new();
    for _ in 0..64 {
        let client = &client;
        let payload = payload.clone();
        senders.push(async move {
            let _ = client.send_detached(&payload).await;
        });
    }
    let saturating = futures::future::join_all(senders);
    tokio::pin!(saturating);

    // Give the senders a moment to actually be in flight/parked.
    tokio::select! {
        _ = &mut saturating => {}
        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
    }

    // NOW take a lifecycle action (`disconnect` needs the same connection
    // lock the senders go through). It must acquire that lock promptly rather
    // than queueing behind every parked sender: with the old
    // lock-held-across-await shape the wait scaled with
    // senders × SEND_QUEUE_WAIT.
    let started = Instant::now();
    let disconnected = tokio::time::timeout(Duration::from_secs(10), client.disconnect()).await;
    let elapsed = started.elapsed();

    assert!(
        disconnected.is_ok(),
        "disconnect must not hang while senders are parked on a full queue"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "disconnect queued behind saturated senders: took {:?}",
        elapsed
    );

    // Let the saturating senders finish/fail before tearing down.
    let _ = tokio::time::timeout(Duration::from_secs(10), saturating).await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}
