//! A permanently blocked socket write must not block respond callers forever:
//! the per-write deadline fails the write, resolves the completion (so the
//! respond returns Err), and tears the connection down.

use async_trait::async_trait;
use msgtrans::{
    packet::Packet,
    protocol::TcpServerConfig,
    transport::{Responder, SessionHandler, SessionSender, TransportServerBuilder},
    SessionId,
};
use std::{sync::Arc, time::Duration};
use tokio::io::AsyncWriteExt;

/// Responds to any request with a payload far larger than the loopback socket
/// buffers, and reports the respond's actual outcome to the test.
struct HugeResponder {
    outcome: tokio::sync::mpsc::Sender<Result<msgtrans::RespondOutcome, msgtrans::TransportError>>,
}

#[async_trait]
impl SessionHandler for HugeResponder {
    async fn on_message(&self, _s: SessionId, _packet: Packet, _sender: SessionSender) {}

    async fn on_request(&self, _s: SessionId, _request: Packet, responder: Responder) {
        // 64 MiB: no loopback socket buffer absorbs this, so the write
        // blocks once the peer stops reading.
        let huge = vec![0u8; 64 * 1024 * 1024];
        let result = responder.respond(huge).await;
        let _ = self.outcome.send(result).await;
    }
}

/// The peer sends a request and then never reads. The server's respond must
/// fail within the write deadline instead of hanging, and the stalled
/// connection must be reaped.
#[tokio::test(flavor = "multi_thread")]
async fn stalled_write_fails_the_respond_within_the_deadline() {
    // Per-connection write deadline via ServerLimits — no process-global hook.
    let addr = "127.0.0.1:28971";
    let (outcome_tx, mut outcome_rx) = tokio::sync::mpsc::channel(1);
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .limits(msgtrans::ServerLimits::new().write_deadline(Duration::from_millis(300)))
        .build(Arc::new(HugeResponder {
            outcome: outcome_tx,
        }))
        .await
        .expect("server");
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Raw client: speaks just enough wire format to send one request, then
    // stops draining its receive buffer entirely.
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let request = Packet::request(1, b"gimme".to_vec());
    stream
        .write_all(&request.try_encode().unwrap())
        .await
        .expect("send request");
    stream.flush().await.expect("flush");
    // Never read from `stream` again; keep it alive so the stall is real.

    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(10), outcome_rx.recv())
        .await
        .expect("respond must resolve — a stalled write may not hang the handler")
        .expect("handler alive");
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "respond against a stalled peer must fail, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "deadline (300ms) should fail the write promptly, took {elapsed:?}"
    );

    // The stalled connection is torn down, releasing the session.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.session_count().await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "stalled connection must be reaped after the write deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    drop(stream);
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
}
