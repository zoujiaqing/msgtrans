use async_trait::async_trait;
use std::{sync::Arc, time::Duration};

macro_rules! echo {
    ($name:ident, $m:ident) => {
        struct $name;
        #[async_trait]
        impl $m::SessionHandler for $name {
            async fn on_message(&self, _: $m::SessionId, _: $m::Packet, _: $m::SessionSender) {}
            async fn on_request(&self, _: $m::SessionId, req: $m::Packet, tx: $m::Responder) {
                tx.respond(req.into_payload()).await.unwrap();
            }
        }
    };
}
echo!(GaEcho, msgtrans);
echo!(BetaEcho, beta);

fn free_port() -> String {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string()
}
async fn wait_for_bind(addr: &str) {
    while std::net::UdpSocket::bind(addr).is_ok() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::main]
async fn main() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let id = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = id.cert.pem();
        let key = id.signing_key.serialize_pem();
        let addr = free_port();
        let ga = msgtrans::TransportServerBuilder::new()
            .protocol(
                msgtrans::QuicServerConfig::new(&addr)
                    .unwrap()
                    .cert_pem(cert.clone())
                    .key_pem(key.clone()),
            )
            .build(Arc::new(GaEcho))
            .await
            .unwrap();
        let bg = ga.clone();
        let serve = tokio::spawn(async move { bg.serve().await.unwrap() });
        wait_for_bind(&addr).await;
        let mut client = beta::TransportClientBuilder::new()
            .protocol(
                beta::QuicClientConfig::new(&addr)
                    .unwrap()
                    .server_name("localhost")
                    .ca_cert_pem(cert.clone()),
            )
            .build()
            .await
            .unwrap();
        client.connect().await.unwrap();
        assert_eq!(
            client
                .request(b"beta client / GA server")
                .await
                .unwrap()
                .as_ref(),
            b"beta client / GA server"
        );
        client.shutdown().await.unwrap();
        assert!(ga.shutdown().await.clean);
        serve.await.unwrap();
        println!("PASS registry 2.0.0-beta.3 client -> 2.0.0 packaged server");

        let addr = free_port();
        let beta_server = beta::TransportServerBuilder::new()
            .protocol(
                beta::QuicServerConfig::new(&addr)
                    .unwrap()
                    .cert_pem(cert.clone())
                    .key_pem(key),
            )
            .build(Arc::new(BetaEcho))
            .await
            .unwrap();
        let bg = beta_server.clone();
        let serve = tokio::spawn(async move { bg.serve().await.unwrap() });
        wait_for_bind(&addr).await;
        let mut client = msgtrans::TransportClientBuilder::new()
            .protocol(
                msgtrans::QuicClientConfig::new(&addr)
                    .unwrap()
                    .server_name("localhost")
                    .ca_cert_pem(cert),
            )
            .build()
            .await
            .unwrap();
        client.connect().await.unwrap();
        assert_eq!(
            client
                .request(b"GA client / beta server")
                .await
                .unwrap()
                .as_ref(),
            b"GA client / beta server"
        );
        client.shutdown().await.unwrap();
        assert!(beta_server.shutdown().await.clean);
        serve.await.unwrap();
        println!("PASS 2.0.0 packaged client -> registry 2.0.0-beta.3 server");
    })
    .await
    .unwrap();
}
