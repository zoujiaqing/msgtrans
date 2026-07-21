//! max_connections enforcement: a connection accepted at capacity is closed
//! before any per-session resources are allocated, existing sessions are
//! unaffected, and capacity is released when a session ends — including a
//! peer-initiated disconnect (which historically never reaped the session
//! from the transports map).

use async_trait::async_trait;
use msgtrans::{
    packet::{Packet, PacketType},
    protocol::{TcpClientConfig, TcpServerConfig},
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
