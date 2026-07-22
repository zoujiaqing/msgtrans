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

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

struct GatedEcho {
    gate: Arc<Notify>,
    open: Arc<AtomicBool>,
}

#[async_trait]
impl SessionHandler for GatedEcho {
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {
        while !self.open.load(Ordering::SeqCst) {
            self.gate.notified().await;
        }
    }
}

/// The zombie-session regression: shutdown's deadline fires while a close is
/// already past the Closing state transition (the actor is blocked, its tiny
/// mailbox full, so the close parks feeding it the close event). Cancelling
/// that close future would strand the session as a permanent zombie — both
/// close paths gate on "already closing" and would return Ok forever. With
/// owned close tasks, the deadline only abandons the JOIN: the close keeps
/// running, finishes once unblocked, and a second shutdown finds nothing left.
#[tokio::test(flavor = "multi_thread")]
async fn deadline_during_close_leaves_no_zombie_session() {
    let addr = "127.0.0.1:28884";
    let gate = Arc::new(Notify::new());
    let open = Arc::new(AtomicBool::new(false));
    let server = TransportServerBuilder::new()
        .actor_buffer_size(4)
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(GatedEcho {
            gate: gate.clone(),
            open: open.clone(),
        }))
        .await
        .expect("server");
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let client = connect(addr).await;
    // Block the actor and fill its mailbox so the close event cannot enter.
    for i in 0..12u32 {
        client
            .send(format!("{}", i).as_bytes())
            .await
            .expect("send");
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.session_count().await, 1);

    // Tiny budget: the deadline WILL fire mid-close.
    let started = std::time::Instant::now();
    let report = server
        .shutdown_with_timeout(Duration::from_millis(300))
        .await;
    let elapsed = started.elapsed();
    assert!(!report.clean, "blocked actor must force clean=false");
    assert_eq!(report.sessions_remaining, 1);
    assert!(
        elapsed < Duration::from_secs(2),
        "small timeout must bound real elapsed, took {:?}",
        elapsed
    );

    // Unblock: the detached close task must finish the job on its own.
    open.store(true, Ordering::SeqCst);
    gate.notify_waiters();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.session_count().await != 0 {
        gate.notify_waiters();
        assert!(
            tokio::time::Instant::now() < deadline,
            "zombie session: close never completed after unblocking"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A second shutdown must be clean and instant — no stuck Closing state.
    let report2 = server.shutdown_with_timeout(Duration::from_secs(2)).await;
    assert!(
        report2.clean,
        "second shutdown hit zombie state: {:?}",
        report2
    );
    assert_eq!(report2.sessions_remaining, 0);
    drop(client);
}
