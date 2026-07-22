//! Lifecycle determinism over real sockets: dropping a client releases the
//! server session without an explicit disconnect; requests work across a
//! reconnect and stale generations cannot cancel them; shutdown() is
//! awaitable and reports what it actually did.

use async_trait::async_trait;
use msgtrans::{
    packet::{Packet, PacketType},
    protocol::{TcpClientConfig, TcpServerConfig},
    transport::{
        SessionHandler, SessionSender, TransportClient, TransportClientBuilder, TransportServer,
        TransportServerBuilder,
    },
    SessionId,
};
use std::{sync::Arc, time::Duration};

struct Echo;

#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _s: SessionId, packet: Packet, sender: SessionSender) {
        if packet.header.packet_type == PacketType::Request {
            let _ = sender
                .respond(
                    packet.header.message_id,
                    packet.header.biz_type,
                    packet.payload,
                )
                .await;
        }
    }
}

async fn start_server(addr: &str) -> TransportServer {
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    server
}

async fn connect(addr: &str) -> TransportClient {
    let mut c = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("cfg"))
        .build()
        .await
        .expect("client");
    c.connect().await.expect("connect");
    c
}

async fn echo_ok(c: &TransportClient) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(3), c.request(b"ping".as_slice())).await,
        Ok(Ok(r)) if r.data.as_deref() == Some(b"ping")
    )
}

/// Dropping the client (no disconnect) must release the server session: the
/// forwarding task is aborted in Drop, background tasks hold only Weak, so
/// the socket closes and the server reaps the session.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_client_releases_server_session() {
    let addr = "127.0.0.1:28881";
    let server = start_server(addr).await;
    let client = connect(addr).await;
    assert!(echo_ok(&client).await);
    assert_eq!(server.session_count().await, 1);

    drop(client); // no disconnect on purpose

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.session_count().await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "server session not released after client drop"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Requests must work across a reconnect, and the old generation's teardown
/// must not cancel the new generation's in-flight request.
#[tokio::test(flavor = "multi_thread")]
async fn requests_survive_reconnect() {
    let addr = "127.0.0.1:28882";
    let _server = start_server(addr).await;
    let mut client = connect(addr).await;
    assert!(echo_ok(&client).await, "request on generation 1");

    client.disconnect().await.expect("disconnect");
    client.connect().await.expect("reconnect");
    // Immediately request on the new generation: a stale close from
    // generation 1 arriving late must not cancel this.
    assert!(echo_ok(&client).await, "request on generation 2");
    assert!(echo_ok(&client).await, "second request on generation 2");
}

/// shutdown() closes sessions, waits for the drain, and reports honestly.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_drains_sessions_and_reports() {
    let addr = "127.0.0.1:28883";
    let server = start_server(addr).await;
    let c1 = connect(addr).await;
    let c2 = connect(addr).await;
    assert!(echo_ok(&c1).await && echo_ok(&c2).await);
    assert_eq!(server.session_count().await, 2);

    let report = server.shutdown_with_timeout(Duration::from_secs(10)).await;
    assert!(report.clean, "shutdown not clean: {:?}", report);
    assert_eq!(report.sessions_remaining, 0);
    assert!(
        report.sessions_closed >= 2,
        "closed {}",
        report.sessions_closed
    );
    assert_eq!(server.session_count().await, 0);
}
