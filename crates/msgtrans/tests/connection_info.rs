//! `ConnectionInfo` must contain REAL data for every protocol.
//!
//! Before 2.0 only TCP filled in the addresses: WebSocket left them at the
//! `0.0.0.0:0` default and QUIC rebuilt a `ConnectionInfo::default()` in its
//! `connection_info()`, discarding the addresses it had already resolved. A
//! handler therefore could not tell a real peer address from a placeholder.
//! These tests assert, over real sockets, that the handler sees a real session
//! id, a real peer address, a real local address and the right protocol name.

// The whole file exercises real sockets, so it only has content when at least
// one protocol adapter is compiled in.
#![cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]

use async_trait::async_trait;
use msgtrans::{
    ConnectionInfo, Packet, SessionHandler, SessionId, SessionSender, TransportClientBuilder,
    TransportServerBuilder,
};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

struct Capture {
    info: Arc<Mutex<Option<ConnectionInfo>>>,
    seen: Arc<Notify>,
}

#[async_trait]
impl SessionHandler for Capture {
    async fn on_request(&self, _s: SessionId, _p: Packet, _r: msgtrans::Responder) {}
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
    async fn on_connected(&self, _session_id: SessionId, info: ConnectionInfo) {
        *self.info.lock().unwrap() = Some(info);
        self.seen.notify_waiters();
    }
}

fn capture() -> (
    Arc<Capture>,
    Arc<Mutex<Option<ConnectionInfo>>>,
    Arc<Notify>,
) {
    let info = Arc::new(Mutex::new(None));
    let seen = Arc::new(Notify::new());
    (
        Arc::new(Capture {
            info: info.clone(),
            seen: seen.clone(),
        }),
        info,
        seen,
    )
}

/// Every field the handler is given must be real, not a placeholder.
#[cfg(any(feature = "tcp", feature = "websocket"))]
fn assert_real(info: &ConnectionInfo, expected_protocol: &str, bind: SocketAddr) {
    assert_eq!(
        info.protocol, expected_protocol,
        "protocol name must identify the transport"
    );
    assert_ne!(
        info.session_id,
        SessionId::new(0),
        "{expected_protocol}: handler must receive the assigned session id, not the 0 default"
    );
    assert!(
        !info.peer_addr.ip().is_unspecified() && info.peer_addr.port() != 0,
        "{expected_protocol}: peer_addr is a placeholder ({})",
        info.peer_addr
    );
    assert_eq!(
        info.local_addr.port(),
        bind.port(),
        "{expected_protocol}: local_addr must be the accepting socket ({})",
        info.local_addr
    );
}

#[cfg(feature = "tcp")]
#[tokio::test(flavor = "multi_thread")]
async fn tcp_connection_info_is_real() {
    use msgtrans::{TcpClientConfig, TcpServerConfig};
    let bind: SocketAddr = "127.0.0.1:28941".parse().unwrap();
    let (handler, info, seen) = capture();
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(&bind.to_string()).expect("server cfg"))
        .build(handler)
        .await
        .expect("server");
    let serving = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.serve().await;
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    let notified = seen.notified();
    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(&bind.to_string()).expect("client cfg"))
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");
    tokio::time::timeout(Duration::from_secs(5), notified)
        .await
        .expect("on_connected fired");

    let captured = info.lock().unwrap().clone().expect("info captured");
    assert_real(&captured, "tcp", bind);

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

#[cfg(feature = "websocket")]
#[tokio::test(flavor = "multi_thread")]
async fn websocket_connection_info_is_real() {
    use msgtrans::{WebSocketClientConfig, WebSocketServerConfig};
    let bind: SocketAddr = "127.0.0.1:28942".parse().unwrap();
    let (handler, info, seen) = capture();
    let server = TransportServerBuilder::new()
        .protocol(WebSocketServerConfig::new(&bind.to_string()).expect("server cfg"))
        .build(handler)
        .await
        .expect("server");
    let serving = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.serve().await;
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    let notified = seen.notified();
    let mut client = TransportClientBuilder::new()
        .protocol(WebSocketClientConfig::new(&format!("ws://{bind}")).expect("client cfg"))
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");
    tokio::time::timeout(Duration::from_secs(5), notified)
        .await
        .expect("on_connected fired");

    let captured = info.lock().unwrap().clone().expect("info captured");
    assert_real(&captured, "websocket", bind);

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

#[cfg(feature = "quic")]
#[tokio::test(flavor = "multi_thread")]
async fn quic_connection_info_is_real() {
    use msgtrans::{QuicClientConfig, QuicServerConfig};
    let bind: SocketAddr = "127.0.0.1:28943".parse().unwrap();
    let (handler, info, seen) = capture();
    let server = TransportServerBuilder::new()
        .protocol(QuicServerConfig::new(&bind.to_string()).expect("server cfg"))
        .build(handler)
        .await
        .expect("server");
    let serving = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.serve().await;
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    let notified = seen.notified();
    let mut client = TransportClientBuilder::new()
        .protocol(
            QuicClientConfig::new(&bind.to_string())
                .expect("client cfg")
                .danger_skip_verification(),
        )
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");
    tokio::time::timeout(Duration::from_secs(5), notified)
        .await
        .expect("on_connected fired");

    let captured = info.lock().unwrap().clone().expect("info captured");
    // QUIC multiplexes one endpoint across connections, so quinn exposes only
    // the local IP (port 0). Assert the peer address and protocol are real and
    // the local IP is not a placeholder.
    assert_eq!(captured.protocol, "quic");
    assert_ne!(captured.session_id, SessionId::new(0));
    assert!(
        !captured.peer_addr.ip().is_unspecified() && captured.peer_addr.port() != 0,
        "quic: peer_addr is a placeholder ({})",
        captured.peer_addr
    );
    assert!(
        !captured.local_addr.ip().is_unspecified(),
        "quic: local_addr ip is a placeholder ({})",
        captured.local_addr
    );

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}
