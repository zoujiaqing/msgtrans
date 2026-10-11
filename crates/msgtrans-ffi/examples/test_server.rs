//! A msgtrans server for exercising the generated bindings from Kotlin or
//! Swift.
//!
//!   cargo run --example test_server
//!
//! Prints `READY tcp_tls=<port> quic=<port> pin=<spki pin>` once listening.
//! Both endpoints use one self-signed `localhost` certificate, so clients
//! must pin it. Messages and requests are echoed; every client is also sent
//! one request, `server-asks`, right after it connects, and the answer is
//! printed as `SERVER_GOT <answer>`.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;

static SERVER: OnceLock<msgtrans::TransportServer> = OnceLock::new();

struct Echo;

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

    async fn on_connected(&self, session: msgtrans::SessionId, info: msgtrans::ConnectionInfo) {
        println!("CONNECTED {} {}", info.protocol, info.peer_addr);
        tokio::spawn(async move {
            let server = SERVER.get().expect("server is set before serving");
            let options = msgtrans::RequestOptions::new().timeout(Duration::from_secs(10));
            match server
                .request_with_options(session, bytes::Bytes::from_static(b"server-asks"), options)
                .await
            {
                Ok(answer) => println!("SERVER_GOT {}", String::from_utf8_lossy(&answer)),
                Err(error) => println!("SERVER_REQUEST_FAILED {error}"),
            }
        });
    }
}

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

#[tokio::main]
async fn main() {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let pin = msgtrans::spki_sha256_base64(cert.der()).unwrap();
    let (cert_pem, key_pem) = (cert.pem(), key.serialize_pem());

    let (tcp_port, quic_port) = (free_port(false), free_port(true));
    let server = msgtrans::TransportServerBuilder::new()
        .protocol(
            msgtrans::TcpServerConfig::new(&format!("127.0.0.1:{tcp_port}"))
                .unwrap()
                .cert_pem(cert_pem.clone())
                .key_pem(key_pem.clone()),
        )
        .protocol(
            msgtrans::QuicServerConfig::new(&format!("127.0.0.1:{quic_port}"))
                .unwrap()
                .cert_pem(cert_pem)
                .key_pem(key_pem),
        )
        .build(Arc::new(Echo))
        .await
        .unwrap();
    SERVER.set(server.clone()).ok().unwrap();

    let serving = server.clone();
    tokio::spawn(async move { serving.serve().await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!("READY tcp_tls={tcp_port} quic={quic_port} pin={pin}");

    tokio::signal::ctrl_c().await.ok();
}
