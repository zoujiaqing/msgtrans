//! The backbone's core claim, proven over a real socket: a slow handler
//! backpressures its connection instead of losing messages. Under the old
//! broadcast path this exact shape lost ~91% of messages at saturation
//! (measured in the A/B that motivated the rework).

use async_trait::async_trait;
use msgtrans::{
    packet::Packet,
    protocol::{TcpClientConfig, TcpServerConfig},
    transport::{SessionHandler, SessionSender, TransportClientBuilder, TransportServerBuilder},
    SessionId,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

struct SlowCounter {
    seen: Arc<AtomicU64>,
}

#[async_trait]
impl SessionHandler for SlowCounter {
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {
        // Slow enough that the sender vastly outpaces us: queues must fill.
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.seen.fetch_add(1, Ordering::Relaxed);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_handler_loses_nothing() {
    const TOTAL: u64 = 300;
    let addr = "127.0.0.1:28876";
    let seen = Arc::new(AtomicU64::new(0));
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(SlowCounter { seen: seen.clone() }))
        .await
        .expect("server");
    let server_bg = server.clone();
    tokio::spawn(async move {
        let _ = server_bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("cfg"))
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");

    // Fire as fast as send() allows — far faster than 1ms/message service.
    for i in 0..TOTAL {
        client
            .send(format!("m{}", i).as_bytes())
            .await
            .expect("send");
    }

    // Every message must eventually be handled: backpressure, not loss.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while seen.load(Ordering::Relaxed) < TOTAL {
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {}/{} messages handled — backbone lost messages",
            seen.load(Ordering::Relaxed),
            TOTAL
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(seen.load(Ordering::Relaxed), TOTAL);
}
