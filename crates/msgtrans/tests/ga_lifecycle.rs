#![cfg(all(feature = "tcp", feature = "websocket"))]

use async_trait::async_trait;
use msgtrans::{
    ClientEvent, ConnectionInfo, Packet, Responder, SessionHandler, SessionId, SessionSender,
    TcpClientConfig, TcpServerConfig, TransportClient, TransportClientBuilder, TransportServer,
    TransportServerBuilder,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, OnceCell};
struct Handler {
    server: OnceCell<TransportServer>,
    ready: mpsc::UnboundedSender<SessionId>,
    reenter: bool,
}
#[async_trait]
impl SessionHandler for Handler {
    async fn on_connected(&self, id: SessionId, _: ConnectionInfo) {
        let _ = self.ready.send(id);
    }
    async fn on_message(&self, _: SessionId, _: Packet, _: SessionSender) {}
    async fn on_request(&self, id: SessionId, request: Packet, responder: Responder) {
        if self.reenter {
            let server = self.server.get().unwrap();
            server.send(id, b"notice").await.unwrap();
            assert_eq!(
                server.request(id, b"reverse").await.unwrap().as_ref(),
                b"answer"
            );
        }
        responder.respond(request.into_payload()).await.unwrap();
    }
}

async fn start(reenter: bool) -> (TransportServer, TransportClient, SessionId) {
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handler = Arc::new(Handler {
        server: OnceCell::new(),
        ready: tx,
        reenter,
    });
    let server = TransportServerBuilder::new()
        .max_connections(1)
        .protocol(TcpServerConfig::new(&addr).unwrap())
        .build(handler.clone())
        .await
        .unwrap();
    if reenter {
        handler.server.set(server.clone()).unwrap();
    }
    let bg = server.clone();
    tokio::spawn(async move {
        bg.serve().await.unwrap();
    });
    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(&addr).unwrap())
        .build()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if client.connect().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let id = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    (server, client, id)
}
#[tokio::test]
async fn removed_session_should_release_capacity_while_peer_remains_open() {
    let (s, c, id) = start(false).await;
    assert_eq!(c.request(b"echo").await.unwrap().as_ref(), b"echo");
    s.remove_session(id).await.unwrap();

    let outcome = tokio::time::timeout(Duration::from_secs(2), async {
        while s.available_permits() != 1 || s.live_session_tasks() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        outcome.is_ok(),
        "remove_session must free capacity without a peer disconnect"
    );
    assert_eq!(s.session_count().await, 0);
    assert_eq!(s.live_session_tasks(), 0);
    c.shutdown().await.unwrap();
    assert!(s.shutdown().await.clean);
}
#[tokio::test]
async fn handler_should_be_able_to_send_to_its_own_session() {
    let (s, c, _) = start(true).await;
    let mut events = c.events().await.unwrap();
    let event_task = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let ClientEvent::Request(request) = event {
                assert_eq!(request.payload().as_ref(), b"reverse");
                request.respond(b"answer".to_vec()).await.unwrap();
                return;
            }
        }
        panic!("client disconnected before the reverse request");
    });
    let outcome = tokio::time::timeout(Duration::from_secs(2), c.request(b"echo")).await;

    assert_eq!(outcome.unwrap().unwrap().as_ref(), b"echo");
    event_task.await.unwrap();
    c.shutdown().await.unwrap();
    assert!(s.shutdown().await.clean);
}
#[tokio::test]
async fn clean_shutdown_closes_unupgraded_websocket() {
    assert_pending_handshake_closed(false).await;
}

#[cfg(feature = "tcp-tls")]
#[tokio::test]
async fn clean_shutdown_closes_unfinished_tls_handshake() {
    assert_pending_handshake_closed(true).await;
}

async fn assert_pending_handshake_closed(tls: bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let (tx, _) = mpsc::unbounded_channel();
    let builder = TransportServerBuilder::new();
    let builder = if tls {
        #[cfg(feature = "tcp-tls")]
        {
            let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            builder.protocol(
                TcpServerConfig::new(&addr.to_string())
                    .unwrap()
                    .cert_pem(cert.cert.pem())
                    .key_pem(cert.signing_key.serialize_pem()),
            )
        }
        #[cfg(not(feature = "tcp-tls"))]
        unreachable!("TLS test requires tcp-tls");
    } else {
        builder.protocol(msgtrans::WebSocketServerConfig::new(&addr.to_string()).unwrap())
    };
    let s = builder
        .build(Arc::new(Handler {
            server: OnceCell::new(),
            ready: tx,
            reenter: false,
        }))
        .await
        .unwrap();
    let bg = s.clone();
    tokio::spawn(async move {
        bg.serve().await.unwrap();
    });
    let mut sock = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(sock) = tokio::net::TcpStream::connect(addr).await {
                break sock;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let partial = if tls {
        &[0x16, 0x03, 0x01][..]
    } else {
        &b"GET / HTTP/1.1\r\n"[..]
    };
    sock.write_all(partial).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let report = s.shutdown().await;
    assert!(report.clean, "{report:?}");
    let mut buf = [0; 1];
    let result = tokio::time::timeout(Duration::from_secs(2), sock.read(&mut buf)).await;

    assert!(
        matches!(result, Ok(Ok(0)) | Ok(Err(_))),
        "pending handshake socket survived clean shutdown"
    );
    std::net::TcpListener::bind(addr).expect("listener released before clean shutdown");
}
