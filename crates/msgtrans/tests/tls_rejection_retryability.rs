//! A server rejected by TLS verification (an SPKI pin that does not match, a
//! certificate that does not verify) is not a transient failure: retrying the
//! same connect can never succeed. It must be reported as non-retryable, or a
//! caller's reconnect loop spins forever on it. Plain network failures stay
//! retryable.
#![cfg(any(feature = "tcp", feature = "quic"))]

use msgtrans::{TransportClientBuilder, TransportError};
use std::time::Duration;

#[cfg(any(feature = "tcp-tls", feature = "quic"))]
use async_trait::async_trait;
#[cfg(any(feature = "tcp-tls", feature = "quic"))]
use msgtrans::{
    Packet, Responder, SessionHandler, SessionId, SessionSender, TransportServerBuilder,
};
#[cfg(any(feature = "tcp-tls", feature = "quic"))]
use std::sync::Arc;

#[cfg(any(feature = "tcp-tls", feature = "quic"))]
struct Echo;

#[cfg(any(feature = "tcp-tls", feature = "quic"))]
#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _id: SessionId, p: Packet, tx: SessionSender) {
        let _ = tx.send_data(p.into_payload()).await;
    }
    async fn on_request(&self, _id: SessionId, req: Packet, responder: Responder) {
        let _ = responder.respond(req.into_payload()).await;
    }
}

#[cfg(any(feature = "tcp-tls", feature = "quic"))]
fn cert_pair() -> (String, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    (cert.pem(), key.serialize_pem())
}

/// A pin that cannot match any real key.
#[cfg(any(feature = "tcp-tls", feature = "quic"))]
const WRONG_PIN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn free_port(udp: bool) -> u16 {
    if udp {
        std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    } else {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
}

async fn connect_err<C: msgtrans::spi::DynClientConfig + 'static>(cfg: C) -> TransportError {
    let mut client = TransportClientBuilder::new()
        .protocol(cfg)
        .build()
        .await
        .unwrap();
    client.connect().await.expect_err("connect must fail")
}

#[cfg(feature = "tcp-tls")]
#[tokio::test(flavor = "multi_thread")]
async fn tcp_tls_pin_mismatch_is_not_retryable() {
    use msgtrans::{TcpClientConfig, TcpServerConfig};
    let (cert, key) = cert_pair();
    let addr = format!("127.0.0.1:{}", free_port(false));
    let server = TransportServerBuilder::new()
        .protocol(
            TcpServerConfig::new(&addr)
                .unwrap()
                .cert_pem(cert)
                .key_pem(key),
        )
        .build(Arc::new(Echo))
        .await
        .unwrap();
    let serving = server.clone();
    tokio::spawn(async move { serving.serve().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let err = connect_err(
        TcpClientConfig::new(&addr)
            .unwrap()
            .spki_pins([WRONG_PIN.to_string()])
            .server_name("localhost")
            .connect_timeout(Duration::from_secs(3)),
    )
    .await;
    assert!(
        !err.is_retryable(),
        "a TLS pin mismatch must not be retryable: {err:?}"
    );
}

#[cfg(feature = "quic")]
#[tokio::test(flavor = "multi_thread")]
async fn quic_pin_mismatch_is_not_retryable() {
    use msgtrans::{QuicClientConfig, QuicServerConfig};
    let (cert, key) = cert_pair();
    let addr = format!("127.0.0.1:{}", free_port(true));
    let server = TransportServerBuilder::new()
        .protocol(
            QuicServerConfig::new(&addr)
                .unwrap()
                .cert_pem(cert)
                .key_pem(key),
        )
        .build(Arc::new(Echo))
        .await
        .unwrap();
    let serving = server.clone();
    tokio::spawn(async move { serving.serve().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let err = connect_err(
        QuicClientConfig::new(&addr)
            .unwrap()
            .server_name("localhost")
            .spki_pins([WRONG_PIN.to_string()])
            .connect_timeout(Duration::from_secs(3)),
    )
    .await;
    assert!(
        !err.is_retryable(),
        "a QUIC pin mismatch must not be retryable: {err:?}"
    );
}

#[cfg(feature = "tcp")]
#[tokio::test(flavor = "multi_thread")]
async fn tcp_nothing_listening_stays_retryable() {
    use msgtrans::TcpClientConfig;
    // Nothing listens here: a network failure, which a retry may well fix.
    let addr = format!("127.0.0.1:{}", free_port(false));
    let err = connect_err(
        TcpClientConfig::new(&addr)
            .unwrap()
            .connect_timeout(Duration::from_secs(3)),
    )
    .await;
    assert!(
        err.is_retryable(),
        "a refused connection must stay retryable: {err:?}"
    );
}
