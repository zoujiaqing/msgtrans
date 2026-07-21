//! max_connections enforcement: a connection accepted at capacity is closed
//! before any per-session resources are allocated, existing sessions are
//! unaffected, and capacity is released when a session ends — including a
//! peer-initiated disconnect (which historically never reaped the session
//! from the transports map).

use async_trait::async_trait;
use msgtrans::{
    packet::{Packet, PacketType},
    protocol::{TcpClientConfig, TcpServerConfig, WebSocketClientConfig, WebSocketServerConfig},
    transport::{
        SessionHandler, SessionSender, TransportClient, TransportClientBuilder,
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

async fn connect_client(addr: &str) -> TransportClient {
    let cfg = TcpClientConfig::new(addr)
        .expect("tcp config")
        .connect_timeout(Duration::from_secs(5));
    let mut client = TransportClientBuilder::new()
        .protocol(cfg)
        .build()
        .await
        .expect("build client");
    client.connect().await.expect("tcp connect");
    client
}

async fn echo_works(client: &TransportClient) -> bool {
    match tokio::time::timeout(Duration::from_secs(3), client.request(b"ping".as_slice())).await {
        Ok(Ok(result)) => result.data.as_deref() == Some(b"ping"),
        _ => false,
    }
}

async fn connect_ws_client(url: &str) -> TransportClient {
    let cfg = WebSocketClientConfig::new(url)
        .expect("ws config")
        .connect_timeout(Duration::from_secs(5));
    let mut client = TransportClientBuilder::new()
        .protocol(cfg)
        .build()
        .await
        .expect("build ws client");
    client.connect().await.expect("ws connect");
    client
}

/// The original race was cross-protocol: TCP, WS and QUIC each run their own
/// accept loop, and a per-loop len() check let simultaneous accepts on
/// different protocols all pass at N-1. Contend TCP and WebSocket bursts for
/// one shared cap and assert it still holds.
#[tokio::test(flavor = "multi_thread")]
async fn cap_is_shared_across_protocols() {
    let tcp_addr = "127.0.0.1:28873";
    let ws_addr = "127.0.0.1:28874";
    const CAP: usize = 4;
    let server = TransportServerBuilder::new()
        .max_connections(CAP)
        .protocol(TcpServerConfig::new(tcp_addr).expect("tcp server config"))
        .protocol(WebSocketServerConfig::new(ws_addr).expect("ws server config"))
        .build(Arc::new(Echo))
        .await
        .expect("build server");

    let server_bg = server.clone();
    tokio::spawn(async move {
        let _ = server_bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 8 TCP + 8 WS clients connect simultaneously against a cap of 4.
    let ws_url = format!("ws://{}", ws_addr);
    let mut tasks = Vec::new();
    for i in 0..16 {
        let ws_url = ws_url.clone();
        tasks.push(tokio::spawn(async move {
            let client = if i % 2 == 0 {
                connect_client(tcp_addr).await
            } else {
                connect_ws_client(&ws_url).await
            };
            let served = echo_works(&client).await;
            (client, served)
        }));
    }
    let mut clients = Vec::new();
    let mut served = 0usize;
    for t in tasks {
        let (client, ok) = t.await.expect("join");
        if ok {
            served += 1;
        }
        clients.push(client); // keep alive so slots stay occupied
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    let sessions = server.session_count().await;
    assert!(
        sessions <= CAP,
        "shared cap must hold across protocols: {} sessions > cap {}",
        sessions,
        CAP
    );
    assert_eq!(
        served, CAP,
        "exactly cap clients should be served across both protocols (got {})",
        served
    );
}

/// The cap must hold under a concurrent connect burst: the permit acquisition
/// is atomic across accept loops, unlike the len() check it replaced, which
/// could let several simultaneous accepts all observe N-1 and pass.
#[tokio::test(flavor = "multi_thread")]
async fn cap_holds_under_concurrent_burst() {
    let addr = "127.0.0.1:28872";
    const CAP: usize = 5;
    let server = TransportServerBuilder::new()
        .max_connections(CAP)
        .protocol(TcpServerConfig::new(addr).expect("tcp server config"))
        .build(Arc::new(Echo))
        .await
        .expect("build server");

    let server_bg = server.clone();
    tokio::spawn(async move {
        let _ = server_bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 20 clients connect simultaneously; hold them all open.
    let mut tasks = Vec::new();
    for _ in 0..20 {
        tasks.push(tokio::spawn(async move {
            let client = connect_client(addr).await;
            let served = echo_works(&client).await;
            (client, served)
        }));
    }
    let mut clients = Vec::new();
    let mut served = 0usize;
    for t in tasks {
        let (client, ok) = t.await.expect("join");
        if ok {
            served += 1;
        }
        clients.push(client); // keep alive so slots stay occupied
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    let sessions = server.session_count().await;
    assert!(
        sessions <= CAP,
        "cap must never be exceeded: {} sessions > cap {}",
        sessions,
        CAP
    );
    assert_eq!(
        served, CAP,
        "exactly cap clients should be served (got {})",
        served
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cap_rejects_then_releases() {
    let addr = "127.0.0.1:28871";
    let server = TransportServerBuilder::new()
        .max_connections(1)
        .protocol(TcpServerConfig::new(addr).expect("tcp server config"))
        .build(Arc::new(Echo))
        .await
        .expect("build server");

    let server_bg = server.clone();
    tokio::spawn(async move {
        let _ = server_bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 1. First client fills the single slot and works.
    let client1 = connect_client(addr).await;
    assert!(echo_works(&client1).await, "client1 echo should succeed");
    assert_eq!(server.session_count().await, 1);

    // 2. Second client is accepted at the TCP level but closed by the cap;
    //    it must never become a session, and client1 keeps working.
    let client2 = connect_client(addr).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.session_count().await,
        1,
        "over-capacity connection must not become a session"
    );
    assert!(
        !echo_works(&client2).await,
        "rejected client must not get echo service"
    );
    assert!(
        echo_works(&client1).await,
        "existing session must be unaffected by the rejected connection"
    );

    // 3. Peer-initiated disconnect frees the slot: a new client gets in.
    client1.disconnect().await.expect("disconnect client1");
    // Wait for the server to reap the session (event pump teardown).
    let mut freed = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if server.session_count().await == 0 {
            freed = true;
            break;
        }
    }
    assert!(freed, "peer disconnect must release the session slot");

    let client3 = connect_client(addr).await;
    assert!(
        echo_works(&client3).await,
        "capacity released after disconnect: client3 should be served"
    );
    assert_eq!(server.session_count().await, 1);
}
