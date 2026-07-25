//! Every configuration option must do what it says. These tests exercise the
//! options that #25 wired (they previously existed but were silently ignored):
//! TCP reuse_addr / idle_timeout, WebSocket path validation / subprotocol
//! negotiation / message caps / ping-pong / TLS, QUIC ALPN and transport
//! parameters.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use msgtrans::{
    ClientTls, Packet, QuicServerConfig, Responder, SessionHandler, SessionId, SessionSender,
    TcpServerConfig, TransportClientBuilder, TransportServer, TransportServerBuilder,
    WebSocketClientConfig, WebSocketServerConfig,
};
use std::{sync::Arc, time::Duration};

/// Multiple rustls crypto providers are linked into this test binary (ring
/// via msgtrans, aws-lc via tokio-rustls defaults): pick one explicitly so
/// raw-TLS test peers do not panic on the ambiguous process default.
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

struct Echo;

#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _s: SessionId, _packet: Packet, _sender: SessionSender) {}

    async fn on_request(&self, _s: SessionId, request: Packet, responder: Responder) {
        let _ = responder.respond(request.into_payload()).await;
    }
}

async fn serve(server: &TransportServer) {
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
}

async fn wait_sessions(server: &TransportServer, expected: usize, why: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.session_count().await != expected {
        assert!(tokio::time::Instant::now() < deadline, "timeout: {why}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// reuse_addr(true) must allow an immediate rebind of a port that just served
/// live connections (TIME_WAIT pairs still present).
#[tokio::test(flavor = "multi_thread")]
async fn tcp_reuse_addr_allows_immediate_rebind() {
    let addr = "127.0.0.1:29011";
    let cfg = || {
        TcpServerConfig::new(addr)
            .expect("cfg")
            .reuse_addr(true)
            .nodelay(true)
    };
    let server = TransportServerBuilder::new()
        .protocol(cfg())
        .build(Arc::new(Echo))
        .await
        .expect("server");
    serve(&server).await;
    // Serve a real connection so the port has live traffic history.
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    wait_sessions(&server, 1, "session up").await;
    drop(stream);
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;

    // Immediate rebind on the same port.
    let server2 = TransportServerBuilder::new()
        .protocol(cfg())
        .build(Arc::new(Echo))
        .await
        .expect("rebind with reuse_addr must succeed");
    serve(&server2).await;
    let probe = tokio::net::TcpStream::connect(addr).await;
    assert!(probe.is_ok(), "rebound server must accept connections");
    let _ = server2.shutdown_with_timeout(Duration::from_secs(5)).await;
}

/// idle_timeout must reap a connection with no traffic in either direction.
#[tokio::test(flavor = "multi_thread")]
async fn tcp_idle_timeout_reaps_silent_connection() {
    let addr = "127.0.0.1:29012";
    let server = TransportServerBuilder::new()
        .protocol(
            TcpServerConfig::new(addr)
                .expect("cfg")
                .idle_timeout(Some(Duration::from_millis(400))),
        )
        .build(Arc::new(Echo))
        .await
        .expect("server");
    serve(&server).await;

    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    wait_sessions(&server, 1, "session up").await;
    // Total silence: the server must close it via idle_timeout.
    wait_sessions(&server, 0, "idle connection must be reaped").await;
    drop(stream);
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
}

/// The configured WebSocket path is enforced: wrong path is rejected with 404,
/// the right path (and the offered msgtrans.v1 subprotocol) completes.
#[tokio::test(flavor = "multi_thread")]
async fn ws_path_is_validated_and_subprotocol_is_negotiated() {
    let addr = "127.0.0.1:29013";
    let server = TransportServerBuilder::new()
        .protocol(
            WebSocketServerConfig::new(addr)
                .expect("cfg")
                .path("/msgtrans"),
        )
        .build(Arc::new(Echo))
        .await
        .expect("server");
    serve(&server).await;

    // Wrong path: handshake must be rejected.
    let wrong = tokio_tungstenite::connect_async(format!("ws://{addr}/other")).await;
    assert!(wrong.is_err(), "wrong path must be rejected");

    // Right path with the msgtrans subprotocol offered: negotiated + echoed.
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{addr}/msgtrans")
        .into_client_request()
        .expect("request");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        "msgtrans.v1".parse().expect("header"),
    );
    let (_stream, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("right path must complete the handshake");
    let negotiated = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        negotiated,
        Some("msgtrans.v1"),
        "offered msgtrans.v1 must be echoed"
    );
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
}

/// The server's max_message_size is enforced by the protocol layer: a message
/// over the cap kills the connection instead of being delivered.
#[tokio::test(flavor = "multi_thread")]
async fn ws_message_size_cap_is_enforced() {
    let addr = "127.0.0.1:29014";
    let server = TransportServerBuilder::new()
        .protocol(
            WebSocketServerConfig::new(addr)
                .expect("cfg")
                .max_message_size(1024)
                .max_frame_size(1024),
        )
        .build(Arc::new(Echo))
        .await
        .expect("server");
    serve(&server).await;

    let (mut stream, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
        .await
        .expect("handshake");
    // In-cap message is fine.
    let small = Packet::one_way(1, vec![0u8; 64]).try_encode().unwrap();
    stream
        .send(tokio_tungstenite::tungstenite::Message::Binary(small))
        .await
        .expect("small message accepted");
    wait_sessions(&server, 1, "session up").await;

    // Over-cap message: the server protocol layer must kill the connection.
    let oversized = Packet::one_way(2, vec![0u8; 8 * 1024])
        .try_encode()
        .unwrap();
    let _ = stream
        .send(tokio_tungstenite::tungstenite::Message::Binary(oversized))
        .await;
    wait_sessions(&server, 0, "oversized message must kill the connection").await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
}

/// A peer that never answers pings is detected by pong_timeout: the client
/// must observe the dead connection within the configured window.
#[tokio::test(flavor = "multi_thread")]
async fn ws_pong_timeout_detects_dead_peer() {
    ensure_crypto_provider();
    let addr = "127.0.0.1:29015";
    // Raw WS server: completes the handshake, then never polls the socket
    // again — pings are never answered.
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let _stream = tokio_tungstenite::accept_async(tcp).await.expect("hs");
        std::future::pending::<()>().await;
    });

    let mut client = TransportClientBuilder::new()
        .protocol(
            WebSocketClientConfig::new(&format!("ws://{addr}/"))
                .expect("cfg")
                // The raw test peer does not echo subprotocols; offer none.
                .subprotocols(vec![])
                .ping_interval(Some(Duration::from_millis(100)))
                .pong_timeout(Duration::from_millis(300)),
        )
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");
    assert!(client.is_connected().await);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while client.is_connected().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "unanswered pings must kill the connection within pong_timeout"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = client.shutdown().await;
}

/// wss:// honors ClientTls: a self-signed server is rejected by SystemRoots,
/// accepted with its CA pinned via CustomCa, and accepted by Insecure.
#[tokio::test(flavor = "multi_thread")]
async fn wss_client_tls_modes_are_real() {
    ensure_crypto_provider();
    let addr = "127.0.0.1:29016";
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
    let cert_pem = cert.cert.pem();
    let cert_der = cert.cert.der().clone();
    let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());

    let server_tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())
        .expect("tls config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_tls));

    // Raw TLS WS echo-less server: handshake, then hold.
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                    return;
                };
                // Keep the connection alive (answer pings via read loop).
                while let Some(Ok(_)) = ws.next().await {}
            });
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let url = format!("wss://localhost:{}/", 29016);
    let connect_with = |tls: ClientTls| {
        let url = url.clone();
        async move {
            let mut client = TransportClientBuilder::new()
                .protocol(
                    WebSocketClientConfig::new(&url)
                        .expect("cfg")
                        // The raw TLS test peer does not echo subprotocols.
                        .subprotocols(vec![])
                        .tls(tls)
                        .connect_timeout(Duration::from_secs(5)),
                )
                .build()
                .await
                .expect("client");
            let result = client.connect().await;
            if result.is_ok() {
                let _ = client.shutdown().await;
            }
            result
        }
    };

    assert!(
        connect_with(ClientTls::SystemRoots).await.is_err(),
        "self-signed cert must fail SystemRoots verification"
    );
    assert!(
        connect_with(ClientTls::CustomCa(cert_pem)).await.is_ok(),
        "pinned CA must verify"
    );
    assert!(
        connect_with(ClientTls::Insecure).await.is_ok(),
        "insecure mode must connect"
    );
}

/// The QUIC handshake requires the msgtrans ALPN: a foreign QUIC client is
/// rejected at the TLS layer, the msgtrans ALPN completes.
#[tokio::test(flavor = "multi_thread")]
async fn quic_alpn_rejects_foreign_clients() {
    ensure_crypto_provider();
    let addr = "127.0.0.1:29017";
    let server = TransportServerBuilder::new()
        .protocol(QuicServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    serve(&server).await;

    // Raw quinn client, skip-verify, per-ALPN attempt.
    let connect_with_alpn = |alpn: &'static [u8]| async move {
        #[derive(Debug)]
        struct SkipVerify(Arc<rustls::crypto::CryptoProvider>);
        impl rustls::client::danger::ServerCertVerifier for SkipVerify {
            fn verify_server_cert(
                &self,
                _e: &rustls::pki_types::CertificateDer<'_>,
                _i: &[rustls::pki_types::CertificateDer<'_>],
                _s: &rustls::pki_types::ServerName<'_>,
                _o: &[u8],
                _n: rustls::pki_types::UnixTime,
            ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }
            fn verify_tls12_signature(
                &self,
                m: &[u8],
                c: &rustls::pki_types::CertificateDer<'_>,
                d: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                rustls::crypto::verify_tls12_signature(
                    m,
                    c,
                    d,
                    &self.0.signature_verification_algorithms,
                )
            }
            fn verify_tls13_signature(
                &self,
                m: &[u8],
                c: &rustls::pki_types::CertificateDer<'_>,
                d: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                rustls::crypto::verify_tls13_signature(
                    m,
                    c,
                    d,
                    &self.0.signature_verification_algorithms,
                )
            }
            fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
                self.0.signature_verification_algorithms.supported_schemes()
            }
        }

        let mut crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipVerify(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))))
            .with_no_client_auth();
        crypto.alpn_protocols = vec![alpn.to_vec()];
        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).expect("crypto"),
        ));
        let mut endpoint =
            quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).expect("endpoint");
        endpoint.set_default_client_config(client_config);
        let connecting = endpoint
            .connect(addr.parse().unwrap(), "localhost")
            .expect("connect start");
        let result = tokio::time::timeout(Duration::from_secs(5), connecting).await;
        match result {
            Ok(Ok(conn)) => {
                conn.close(0u32.into(), b"done");
                true
            }
            _ => false,
        }
    };

    assert!(
        !connect_with_alpn(b"http/1.1").await,
        "foreign ALPN must be rejected"
    );
    assert!(
        connect_with_alpn(b"msgtrans/1").await,
        "msgtrans ALPN must complete the handshake"
    );
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
}

/// Custom QUIC transport parameters (windows, streams, RTT, keepalive) are
/// applied without breaking the connection path: end-to-end echo works.
#[tokio::test(flavor = "multi_thread")]
async fn quic_custom_transport_parameters_smoke() {
    ensure_crypto_provider();
    let addr = "127.0.0.1:29018";
    let server = TransportServerBuilder::new()
        .protocol(
            QuicServerConfig::new(addr)
                .expect("cfg")
                .receive_window(512 * 1024)
                .send_window(512 * 1024)
                .max_concurrent_streams(16)
                .initial_rtt(Duration::from_millis(50))
                .keep_alive_interval(Some(Duration::from_secs(1))),
        )
        .build(Arc::new(Echo))
        .await
        .expect("server");
    serve(&server).await;

    let mut client = TransportClientBuilder::new()
        .protocol(
            msgtrans::QuicClientConfig::new(addr)
                .expect("cfg")
                .verify_certificate(false),
        )
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");
    let reply = tokio::time::timeout(Duration::from_secs(5), client.request(b"ping".as_slice()))
        .await
        .expect("no timeout")
        .expect("echo works with custom transport parameters");
    assert_eq!(reply.data.as_deref(), Some(b"ping".as_slice()));
    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
}

/// A healthy connection pings at ping_interval, NOT at pong_timeout: with
/// interval 300ms / pong_timeout 100ms over ~1.05s a correct client sends
/// ~3 pings; the collapsed-cadence bug sent ~10.
#[tokio::test(flavor = "multi_thread")]
async fn ws_ping_cadence_follows_interval_not_pong_timeout() {
    let addr = "127.0.0.1:29019";
    let ping_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = ping_count.clone();
    // Raw WS peer that polls (so tungstenite auto-pongs) and counts pings.
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let mut ws = tokio_tungstenite::accept_async(tcp).await.expect("hs");
        while let Some(Ok(msg)) = ws.next().await {
            if matches!(msg, tokio_tungstenite::tungstenite::Message::Ping(_)) {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    });

    let mut client = TransportClientBuilder::new()
        .protocol(
            WebSocketClientConfig::new(&format!("ws://{addr}/"))
                .expect("cfg")
                .subprotocols(vec![])
                .ping_interval(Some(Duration::from_millis(300)))
                .pong_timeout(Duration::from_millis(100)),
        )
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");
    tokio::time::sleep(Duration::from_millis(1050)).await;
    let pings = ping_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        (2..=5).contains(&pings),
        "expected ~3 interval-paced pings in 1.05s, got {pings}"
    );
    let _ = client.shutdown().await;
}

/// Plain ws:// must not consult the TLS configuration at all: a garbage
/// CustomCa neither blocks the connection nor is parsed.
#[tokio::test(flavor = "multi_thread")]
async fn plain_ws_ignores_tls_configuration() {
    let addr = "127.0.0.1:29020";
    let server = TransportServerBuilder::new()
        .protocol(WebSocketServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    serve(&server).await;

    let mut client = TransportClientBuilder::new()
        .protocol(
            WebSocketClientConfig::new(&format!("ws://{addr}/"))
                .expect("cfg")
                .tls(ClientTls::CustomCa("not a pem at all".to_string())),
        )
        .build()
        .await
        .expect("client");
    client
        .connect()
        .await
        .expect("ws:// must ignore TLS configuration entirely");
    assert!(client.is_connected().await);
    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
}

/// The config-driven builder is the SINGLE construction path and validates
/// every protocol config before building: an invalid config is rejected
/// instead of producing a server that cannot carry data (the legacy factory
/// SPI that duplicated this path is gone).
#[tokio::test(flavor = "multi_thread")]
async fn builder_validates_configs_before_building() {
    // Zero-stream QUIC: rejected at build, never a running data-less server.
    let bad = TransportServerBuilder::new()
        .protocol(
            QuicServerConfig::new("127.0.0.1:29022")
                .expect("cfg")
                .max_concurrent_streams(0),
        )
        .build(Arc::new(Echo))
        .await;
    assert!(
        bad.is_err(),
        "zero-stream QUIC config must be rejected at build"
    );

    // A valid config still builds and serves.
    let ok = TransportServerBuilder::new()
        .protocol(QuicServerConfig::new("127.0.0.1:29023").expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("valid config builds");
    let _ = ok.shutdown_with_timeout(Duration::from_secs(5)).await;
}
