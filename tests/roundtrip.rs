//! End-to-end tests of the bindings' Rust surface against real msgtrans
//! servers.
//!
//! Every client call is driven by `futures::executor::block_on` on a plain
//! thread with NO Tokio context, which is exactly the situation of a Kotlin
//! coroutine or Swift task polling an exported future. A test that passed
//! only because an ambient Tokio runtime happened to be present would prove
//! nothing about the bindings.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use msgtrans_ffi::*;

// ---------------------------------------------------------------- harness --

/// Run `fut` on a fresh thread without a Tokio context, failing the test if it
/// does not finish in time.
fn wait<T: Send + 'static>(fut: impl Future<Output = T> + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(futures::executor::block_on(fut));
    });
    rx.recv_timeout(Duration::from_secs(15))
        .expect("operation did not complete within 15s")
}

/// Next event that is not the droppable `MessageSent` diagnostic.
fn next_event(client: &Arc<MsgtransClient>) -> ClientEvent {
    loop {
        let c = client.clone();
        match wait(async move { c.next_event().await }) {
            Some(ClientEvent::MessageSent { .. }) => continue,
            Some(event) => return event,
            None => panic!("event stream ended unexpectedly"),
        }
    }
}

struct Echo {
    connected: tokio::sync::mpsc::UnboundedSender<msgtrans::SessionId>,
}

#[async_trait]
impl msgtrans::SessionHandler for Echo {
    async fn on_message(
        &self,
        _session: msgtrans::SessionId,
        packet: msgtrans::Packet,
        sender: msgtrans::SessionSender,
    ) {
        let _ = sender.send_data(packet.into_payload()).await;
    }

    async fn on_request(
        &self,
        _session: msgtrans::SessionId,
        request: msgtrans::Packet,
        responder: msgtrans::Responder,
    ) {
        let _ = responder.respond(request.into_payload()).await;
    }

    async fn on_connected(&self, session: msgtrans::SessionId, _info: msgtrans::ConnectionInfo) {
        let _ = self.connected.send(session);
    }
}

struct TestServer {
    rt: tokio::runtime::Runtime,
    server: msgtrans::TransportServer,
    sessions: tokio::sync::mpsc::UnboundedReceiver<msgtrans::SessionId>,
}

impl TestServer {
    fn start<C: msgtrans::spi::DynServerConfig + 'static>(config: C) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (tx, sessions) = tokio::sync::mpsc::unbounded_channel();
        let server = rt.block_on(async {
            msgtrans::TransportServerBuilder::new()
                .protocol(config)
                .build(Arc::new(Echo { connected: tx }))
                .await
                .unwrap()
        });
        let serving = server.clone();
        rt.spawn(async move {
            let _ = serving.serve().await;
        });
        // Give the listeners a moment to bind (UDP cannot be probed).
        std::thread::sleep(Duration::from_millis(300));
        Self {
            rt,
            server,
            sessions,
        }
    }

    /// Push a one-way message to the most recently connected client.
    fn push_from_server(&mut self, payload: &'static [u8]) {
        let server = self.server.clone();
        let sessions = &mut self.sessions;
        self.rt.block_on(async move {
            let session = tokio::time::timeout(Duration::from_secs(5), sessions.recv())
                .await
                .expect("no client connected")
                .unwrap();
            server
                .send(session, payload)
                .await
                .expect("server push failed");
        });
    }

    /// Ask the most recently connected client something, from the server.
    fn request_from_server(&mut self, payload: &'static [u8]) -> Vec<u8> {
        let server = self.server.clone();
        let sessions = &mut self.sessions;
        self.rt.block_on(async move {
            let session = tokio::time::timeout(Duration::from_secs(5), sessions.recv())
                .await
                .expect("no client connected")
                .unwrap();
            server
                .request(session, payload)
                .await
                .expect("server -> client request failed")
                .to_vec()
        })
    }
}

fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Self-signed `localhost` certificate: (cert PEM, key PEM, SPKI pin).
fn cert_pair() -> (String, String, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let pin = msgtrans::spki_sha256_base64(cert.der()).unwrap();
    (cert.pem(), key.serialize_pem(), pin)
}

fn config(endpoint: Endpoint) -> ClientConfig {
    ClientConfig {
        endpoint,
        connect_timeout_ms: 5_000,
        limits: None,
    }
}

/// Connect, then exercise the full client surface once.
fn exercise(client: &Arc<MsgtransClient>) {
    let c = client.clone();
    wait(async move { c.connect().await }).expect("connect failed");
    assert!(matches!(next_event(client), ClientEvent::Connected { .. }));

    let c = client.clone();
    assert!(wait(async move { c.is_connected().await }));

    // Write-confirmed send, echoed back as a server message.
    let c = client.clone();
    wait(async move { c.send(b"hello".to_vec()).await }).expect("send failed");
    match next_event(client) {
        ClientEvent::Message { message } => assert_eq!(message.payload, b"hello"),
        other => panic!("expected the echoed message, got {other:?}"),
    }

    // Request / response.
    let c = client.clone();
    let response = wait(async move { c.request(b"ping".to_vec()).await }).expect("request failed");
    assert_eq!(response, b"ping");

    // Options are carried through.
    let c = client.clone();
    let response = wait(async move {
        c.request_with_options(
            b"with-options".to_vec(),
            RequestOptions {
                biz_type: Some(7),
                ext_header: None,
                timeout_ms: Some(3_000),
            },
        )
        .await
    })
    .expect("request_with_options failed");
    assert_eq!(response, b"with-options");
}

fn disconnect(client: &Arc<MsgtransClient>) {
    let c = client.clone();
    wait(async move { c.disconnect().await }).expect("disconnect failed");
    assert!(matches!(
        next_event(client),
        ClientEvent::Disconnected { .. }
    ));
}

// ------------------------------------------------------------------ tests --

#[test]
fn tcp_plaintext_roundtrip() {
    let port = free_tcp_port();
    let _server =
        TestServer::start(msgtrans::TcpServerConfig::new(&format!("127.0.0.1:{port}")).unwrap());
    let client = MsgtransClient::new(config(Endpoint::Tcp {
        address: format!("127.0.0.1:{port}"),
        tls: None,
    }))
    .unwrap();
    exercise(&client);
    disconnect(&client);
}

#[test]
fn tcp_tls_with_matching_pin_connects() {
    let (cert, key, pin) = cert_pair();
    let port = free_tcp_port();
    let _server = TestServer::start(
        msgtrans::TcpServerConfig::new(&format!("127.0.0.1:{port}"))
            .unwrap()
            .cert_pem(cert)
            .key_pem(key),
    );
    let client = MsgtransClient::new(config(Endpoint::Tcp {
        address: format!("127.0.0.1:{port}"),
        tls: Some(TcpTls {
            spki_pins: vec![pin],
            server_name: Some("localhost".to_string()),
        }),
    }))
    .unwrap();
    exercise(&client);
    disconnect(&client);
}

#[test]
fn tcp_tls_with_wrong_pin_is_refused() {
    let (cert, key, _pin) = cert_pair();
    let (_, _, other_pin) = cert_pair();
    let port = free_tcp_port();
    let _server = TestServer::start(
        msgtrans::TcpServerConfig::new(&format!("127.0.0.1:{port}"))
            .unwrap()
            .cert_pem(cert)
            .key_pem(key),
    );
    let client = MsgtransClient::new(config(Endpoint::Tcp {
        address: format!("127.0.0.1:{port}"),
        tls: Some(TcpTls {
            spki_pins: vec![other_pin],
            server_name: Some("localhost".to_string()),
        }),
    }))
    .unwrap();
    let c = client.clone();
    let err = wait(async move { c.connect().await }).expect_err("pin mismatch must be refused");
    // Retrying can never succeed, so a reconnect loop must be told to stop.
    assert!(
        matches!(
            err,
            MsgtransError::Connection {
                retryable: false,
                ..
            }
        ),
        "a pin mismatch must be a non-retryable connection error: {err:?}"
    );
}

#[test]
fn tcp_tls_without_pins_is_a_config_error_not_plaintext() {
    let result = MsgtransClient::new(config(Endpoint::Tcp {
        address: "127.0.0.1:1".to_string(),
        tls: Some(TcpTls {
            spki_pins: vec![],
            server_name: None,
        }),
    }));
    assert!(matches!(result, Err(MsgtransError::Configuration { .. })));
}

#[test]
fn websocket_plaintext_roundtrip() {
    let port = free_tcp_port();
    let _server = TestServer::start(
        msgtrans::WebSocketServerConfig::new(&format!("127.0.0.1:{port}")).unwrap(),
    );
    let client = MsgtransClient::new(config(Endpoint::WebSocket {
        url: format!("ws://127.0.0.1:{port}"),
        tls: None,
        headers: HashMap::new(),
    }))
    .unwrap();
    exercise(&client);
    disconnect(&client);
}

#[test]
fn websocket_tls_settings_on_ws_url_are_rejected() {
    let result = MsgtransClient::new(config(Endpoint::WebSocket {
        url: "ws://127.0.0.1:1".to_string(),
        tls: Some(WebSocketTls::InsecureSkipVerification),
        headers: HashMap::new(),
    }));
    assert!(matches!(result, Err(MsgtransError::Configuration { .. })));
}

fn quic_server(port: u16) -> (TestServer, String) {
    let (cert, key, pin) = cert_pair();
    let server = TestServer::start(
        msgtrans::QuicServerConfig::new(&format!("127.0.0.1:{port}"))
            .unwrap()
            .cert_pem(cert)
            .key_pem(key),
    );
    (server, pin)
}

fn quic_client(port: u16, verification: QuicVerification) -> Arc<MsgtransClient> {
    MsgtransClient::new(config(Endpoint::Quic {
        address: format!("127.0.0.1:{port}"),
        server_name: Some("localhost".to_string()),
        verification,
        keep_alive_interval_ms: Some(5_000),
        max_idle_timeout_ms: Some(30_000),
    }))
    .unwrap()
}

#[test]
fn quic_pinned_roundtrip_and_server_initiated_request() {
    let port = free_udp_port();
    let (mut server, pin) = quic_server(port);
    let client = quic_client(port, QuicVerification::SpkiPins { pins: vec![pin] });
    exercise(&client);

    // The server asks the client something; the client answers through
    // IncomingRequest, exactly once.
    let answering = client.clone();
    let answered = std::thread::spawn(move || match next_event(&answering) {
        ClientEvent::Request { request } => {
            assert_eq!(request.payload(), b"server-asks");
            let r = request.clone();
            let outcome = wait(async move { r.respond(b"client-answers".to_vec()).await })
                .expect("respond failed");
            assert_eq!(outcome, RespondOutcome::Written);
            let r = request.clone();
            let again = wait(async move { r.respond(b"twice".to_vec()).await });
            assert!(matches!(again, Err(MsgtransError::InvalidState { .. })));
        }
        other => panic!("expected a server request, got {other:?}"),
    });
    assert_eq!(
        server.request_from_server(b"server-asks"),
        b"client-answers"
    );
    answered.join().unwrap();

    disconnect(&client);
}

#[test]
fn quic_with_wrong_pin_is_refused() {
    let port = free_udp_port();
    let (_server, _pin) = quic_server(port);
    let (_, _, other_pin) = cert_pair();
    let client = quic_client(
        port,
        QuicVerification::SpkiPins {
            pins: vec![other_pin],
        },
    );
    let c = client.clone();
    let err = wait(async move { c.connect().await }).expect_err("pin mismatch must be refused");
    assert!(
        matches!(
            err,
            MsgtransError::Connection {
                retryable: false,
                ..
            }
        ),
        "a QUIC pin mismatch must be a non-retryable connection error: {err:?}"
    );
}

#[test]
fn quic_empty_pin_list_is_a_config_error_not_webpki_fallback() {
    let result = MsgtransClient::new(config(Endpoint::Quic {
        address: "127.0.0.1:1".to_string(),
        server_name: None,
        verification: QuicVerification::SpkiPins { pins: vec![] },
        keep_alive_interval_ms: None,
        max_idle_timeout_ms: None,
    }));
    assert!(matches!(result, Err(MsgtransError::Configuration { .. })));
}

#[test]
fn cancelled_next_event_does_not_lose_the_event() {
    let port = free_tcp_port();
    let mut server =
        TestServer::start(msgtrans::TcpServerConfig::new(&format!("127.0.0.1:{port}")).unwrap());
    let client = MsgtransClient::new(config(Endpoint::Tcp {
        address: format!("127.0.0.1:{port}"),
        tls: None,
    }))
    .unwrap();
    let c = client.clone();
    wait(async move { c.connect().await }).unwrap();
    assert!(matches!(next_event(&client), ClientEvent::Connected { .. }));

    // Start waiting for an event, then cancel the wait while it is pending —
    // what a Kotlin `withTimeout` or a cancelled scope does.
    {
        let mut pending = Box::pin(client.next_event());
        let waker = futures::task::noop_waker();
        assert!(matches!(
            pending.as_mut().poll(&mut Context::from_waker(&waker)),
            Poll::Pending
        ));
    } // dropped here

    // A server push, not a client send: a client send would first emit a
    // MessageSent diagnostic, and a lossy implementation could swallow that
    // instead of the message and still pass.
    server.push_from_server(b"after-cancel");
    match next_event(&client) {
        ClientEvent::Message { message } => assert_eq!(message.payload, b"after-cancel"),
        other => panic!("the event after a cancelled wait was lost: got {other:?}"),
    }
}

#[test]
fn shutdown_ends_a_pending_event_wait() {
    let port = free_tcp_port();
    let _server =
        TestServer::start(msgtrans::TcpServerConfig::new(&format!("127.0.0.1:{port}")).unwrap());
    let client = MsgtransClient::new(config(Endpoint::Tcp {
        address: format!("127.0.0.1:{port}"),
        tls: None,
    }))
    .unwrap();
    let c = client.clone();
    wait(async move { c.connect().await }).unwrap();
    assert!(matches!(next_event(&client), ClientEvent::Connected { .. }));

    // An event loop parked in next_event() — the state a Swift task is in when
    // the app wants to stop it. Swift task cancellation cannot interrupt it
    // (UniFFI's Swift bridge does not propagate cancellation), so shutdown()
    // is the way out and must actually end the wait.
    let (tx, rx) = std::sync::mpsc::channel();
    let looping = client.clone();
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        while let Some(event) = futures::executor::block_on(looping.next_event()) {
            seen.push(format!("{event:?}"));
        }
        let _ = tx.send(seen);
    });
    std::thread::sleep(Duration::from_millis(200));

    let c = client.clone();
    wait(async move { c.shutdown().await }).expect("shutdown failed");
    let drained = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("next_event() still pending 10s after shutdown()");
    assert!(
        drained.iter().all(|e| !e.starts_with("Message")),
        "unexpected events while shutting down: {drained:?}"
    );
}

#[test]
fn reports_its_version() {
    assert_eq!(msgtrans_ffi_version(), env!("CARGO_PKG_VERSION"));
}
