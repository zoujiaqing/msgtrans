#![cfg(all(feature = "tcp", feature = "websocket", feature = "tcp-tls"))]
use async_trait::async_trait;
use msgtrans::*;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, sync::Notify};
struct Probe {
    entered: Arc<Notify>,
    gate: Arc<Notify>,
    written: Arc<Notify>,
    size: Arc<AtomicUsize>,
}
#[async_trait]
impl SessionHandler for Probe {
    async fn on_message(&self, _: SessionId, p: Packet, _: SessionSender) {
        self.size.store(p.payload().len(), Ordering::SeqCst);
        self.entered.notify_one();
    }
    async fn on_request(&self, _: SessionId, p: Packet, r: Responder) {
        self.entered.notify_one();
        self.gate.notified().await;
        r.respond(p.into_payload()).await.unwrap();
        self.written.notify_one();
    }
}
fn probe() -> Arc<Probe> {
    Arc::new(Probe {
        entered: Arc::new(Notify::new()),
        gate: Arc::new(Notify::new()),
        written: Arc::new(Notify::new()),
        size: Arc::new(AtomicUsize::new(0)),
    })
}
async fn start(p: Arc<Probe>, limits: ServerLimits) -> (TransportServer, tokio::net::TcpStream) {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    let s = TransportServerBuilder::new()
        .limits(limits)
        .protocol(TcpServerConfig::new(&addr.to_string()).unwrap())
        .build(p)
        .await
        .unwrap();
    let s2 = s.clone();
    tokio::spawn(async move {
        s2.serve().await.unwrap();
    });
    let peer = loop {
        if let Ok(p) = tokio::net::TcpStream::connect(addr).await {
            break p;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    (s, peer)
}
#[tokio::test]
async fn tcp_saturated_ingress_keeps_response_writer_live() {
    let p = probe();
    let (s, mut peer) = start(
        p.clone(),
        ServerLimits::new()
            .pipe_capacity(1)
            .mailbox_capacity(1)
            .write_deadline(Duration::from_millis(50)),
    )
    .await;
    peer.write_all(&Packet::request(1, b"ping".to_vec()).try_encode().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), p.entered.notified())
        .await
        .unwrap();
    let mut burst = Vec::new();
    for id in 2..102 {
        burst.extend_from_slice(&Packet::one_way(id, vec![1u8; 64]).try_encode().unwrap());
    }
    peer.write_all(&burst).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    p.gate.notify_one();
    let stalled = tokio::time::timeout(Duration::from_millis(500), p.written.notified())
        .await
        .is_err();
    println!("tcp response stalled beyond 50ms write deadline: {stalled}");
    assert!(!stalled, "reader backpressure blocked the writer");
    let report = s.shutdown_with_timeout(Duration::from_secs(2)).await;
    assert!(report.clean, "{report:?}");
}
#[tokio::test]
async fn tcp_connect_timeout_includes_tls_handshake() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut c = TransportClientBuilder::new()
        .retry_strategy(RetryConfig::default().max_retries(0))
        .protocol(
            TcpClientConfig::new(&listener.local_addr().unwrap().to_string())
                .unwrap()
                .spki_pins(vec!["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into()])
                .connect_timeout(Duration::from_millis(30)),
        )
        .build()
        .await
        .unwrap();
    let accept = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    let result = tokio::time::timeout(Duration::from_millis(300), c.connect()).await;
    println!(
        "TLS configured 30ms, still pending at 300ms: {}",
        result.is_err()
    );
    assert!(matches!(result, Ok(Err(_))), "{result:?}");
    accept.abort();
}
#[cfg(feature = "flate2")]
#[tokio::test]
async fn decompression_respects_connection_payload_limit() {
    let p = probe();
    let (s, mut peer) = start(p.clone(), ServerLimits::new().max_payload_size(1024)).await;
    let mut packet = Packet::one_way(1, vec![0u8; 65536]);
    packet.set_compression(CompressionType::Zlib);
    packet.compress_payload().unwrap();
    println!("compressed wire payload={}", packet.payload().len());
    peer.write_all(&packet.try_encode().unwrap()).await.unwrap();
    use tokio::io::AsyncReadExt;
    let mut byte = [0u8; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), peer.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(p.size.load(Ordering::SeqCst), 0);
    s.shutdown().await;
}
#[tokio::test]
async fn websocket_saturated_ingress_keeps_response_writer_live() {
    use futures_util::SinkExt;
    let p = probe();
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    let s = TransportServerBuilder::new()
        .limits(
            ServerLimits::new()
                .pipe_capacity(1)
                .mailbox_capacity(1)
                .write_deadline(Duration::from_millis(50)),
        )
        .protocol(WebSocketServerConfig::new(&addr.to_string()).unwrap())
        .build(p.clone())
        .await
        .unwrap();
    let bg = s.clone();
    tokio::spawn(async move {
        bg.serve().await.unwrap();
    });
    let (mut peer, _) = loop {
        if let Ok(p) = tokio_tungstenite::connect_async(format!("ws://{addr}/")).await {
            break p;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    peer.send(tokio_tungstenite::tungstenite::Message::Binary(
        Packet::request(1, b"ping".to_vec()).try_encode().unwrap(),
    ))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), p.entered.notified())
        .await
        .unwrap();
    for id in 2..102 {
        peer.send(tokio_tungstenite::tungstenite::Message::Binary(
            Packet::one_way(id, vec![1u8; 64]).try_encode().unwrap(),
        ))
        .await
        .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    p.gate.notify_one();
    let stalled = tokio::time::timeout(Duration::from_millis(500), p.written.notified())
        .await
        .is_err();
    println!("WS response stalled beyond 50ms write deadline: {stalled}");
    assert!(!stalled, "reader backpressure blocked the writer");
    assert!(s.shutdown_with_timeout(Duration::from_secs(1)).await.clean);
}
struct CloseProbe {
    entered: Notify,
    gate: Notify,
    count: AtomicUsize,
    closed: AtomicUsize,
}
#[async_trait]
impl SessionHandler for CloseProbe {
    async fn on_message(&self, _: SessionId, _: Packet, _: SessionSender) {
        if self.count.fetch_add(1, Ordering::SeqCst) == 0 {
            self.entered.notify_one();
            self.gate.notified().await;
        }
    }
    async fn on_request(&self, _: SessionId, _: Packet, _: Responder) {}
    async fn on_disconnected(&self, _: SessionId, _: CloseReason) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn full_mailbox_shutdown_finalizes_disconnect_once() {
    let p = Arc::new(CloseProbe {
        entered: Notify::new(),
        gate: Notify::new(),
        count: AtomicUsize::new(0),
        closed: AtomicUsize::new(0),
    });
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = free.local_addr().unwrap();
    drop(free);
    let s = TransportServerBuilder::new()
        .limits(ServerLimits::new().pipe_capacity(1).mailbox_capacity(1))
        .protocol(TcpServerConfig::new(&addr.to_string()).unwrap())
        .build(p.clone())
        .await
        .unwrap();
    let bg = s.clone();
    tokio::spawn(async move {
        bg.serve().await.unwrap();
    });
    let mut peer = loop {
        if let Ok(p) = tokio::net::TcpStream::connect(addr).await {
            break p;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    peer.write_all(&Packet::one_way(1, vec![1u8]).try_encode().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), p.entered.notified())
        .await
        .unwrap();
    peer.write_all(&Packet::one_way(2, vec![2u8]).try_encode().unwrap())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let bg = s.clone();
    let close = tokio::spawn(async move { bg.shutdown_with_timeout(Duration::from_secs(2)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    p.gate.notify_one();
    let report = close.await.unwrap();
    println!(
        "shutdown clean={}, on_disconnected count={}",
        report.clean,
        p.closed.load(Ordering::SeqCst)
    );
    assert!(report.clean);
    assert_eq!(p.closed.load(Ordering::SeqCst), 1);
}

struct FinalizerProbe {
    connected: Notify,
    finalizing: Notify,
    release: Notify,
    calls: AtomicUsize,
}
#[async_trait]
impl SessionHandler for FinalizerProbe {
    async fn on_message(&self, _: SessionId, _: Packet, _: SessionSender) {}
    async fn on_request(&self, _: SessionId, _: Packet, _: Responder) {}
    async fn on_connected(&self, _: SessionId, _: ConnectionInfo) {
        self.connected.notify_one();
    }
    async fn on_disconnected(&self, _: SessionId, _: CloseReason) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.finalizing.notify_one();
        self.release.notified().await;
    }
}

#[tokio::test]
async fn shutdown_cannot_report_clean_before_disconnect_finalization() {
    let handler = Arc::new(FinalizerProbe {
        connected: Notify::new(),
        finalizing: Notify::new(),
        release: Notify::new(),
        calls: AtomicUsize::new(0),
    });
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(&addr.to_string()).unwrap())
        .build(handler.clone())
        .await
        .unwrap();
    let bg = server.clone();
    let serve = tokio::spawn(async move { bg.serve().await });
    let _peer = loop {
        if let Ok(peer) = tokio::net::TcpStream::connect(addr).await {
            break peer;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    handler.connected.notified().await;
    let bg = server.clone();
    let closing =
        tokio::spawn(async move { bg.shutdown_with_timeout(Duration::from_millis(100)).await });
    handler.finalizing.notified().await;
    assert!(!closing.await.unwrap().clean);
    handler.release.notify_one();
    assert!(
        server
            .shutdown_with_timeout(Duration::from_secs(2))
            .await
            .clean
    );
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
    serve.await.unwrap().unwrap();
}
