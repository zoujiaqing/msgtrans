use async_trait::async_trait;
use msgtrans::{
    Packet, QuicClientConfig, QuicServerConfig, Responder, SessionHandler, SessionId,
    SessionSender, TcpClientConfig, TcpServerConfig, TransportClient, TransportClientBuilder,
    TransportServerBuilder, WebSocketClientConfig, WebSocketServerConfig,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    task::JoinSet,
    time::{sleep, timeout, Instant},
};

struct Echo;
#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _: SessionId, _: Packet, _: SessionSender) {}
    async fn on_request(&self, _: SessionId, packet: Packet, reply: Responder) {
        reply.respond(packet.into_payload()).await.unwrap();
    }
}
async fn dial(lane: usize, spec: &[String; 4]) -> TransportClient {
    let builder = TransportClientBuilder::new();
    let builder = match lane % 3 {
        0 => builder.protocol(TcpClientConfig::new(&spec[0]).unwrap()),
        1 => builder.protocol(WebSocketClientConfig::new(&spec[1]).unwrap()),
        _ => builder.protocol(
            QuicClientConfig::new(&spec[2])
                .unwrap()
                .server_name("localhost")
                .ca_cert_pem(spec[3].clone()),
        ),
    };
    let mut client = builder.build().await.unwrap();
    client.connect().await.unwrap();
    client
}
fn tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
#[tokio::main]
async fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .map(|v| v.parse().unwrap())
        .unwrap_or(3600);
    let tcp = format!("127.0.0.1:{}", tcp_port());
    let ws = format!("127.0.0.1:{}", tcp_port());
    let quic = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let identity = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = identity.cert.pem();
    let spec = Arc::new([
        tcp.clone(),
        format!("ws://{ws}"),
        quic.clone(),
        cert.clone(),
    ]);
    const CAP: usize = 24;
    let server = TransportServerBuilder::new()
        .max_connections(CAP)
        .protocol(TcpServerConfig::new(&tcp).unwrap())
        .protocol(WebSocketServerConfig::new(&ws).unwrap())
        .protocol(
            QuicServerConfig::new(&quic)
                .unwrap()
                .cert_pem(cert)
                .key_pem(identity.signing_key.serialize_pem()),
        )
        .build(Arc::new(Echo))
        .await
        .unwrap();
    let bg = server.clone();
    let mut serving = tokio::spawn(async move { bg.serve().await.unwrap() });
    timeout(Duration::from_secs(5), async {
        while tokio::net::TcpStream::connect(&tcp).await.is_err() {
            sleep(Duration::from_millis(10)).await;
        }
        while server.live_session_tasks() != 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    sleep(Duration::from_millis(100)).await;
    let start = Instant::now();
    let mut waves = 0usize;
    let mut max_ms = 0u128;
    let mut total_requests = 0usize;
    while start.elapsed().as_secs() < seconds {
        timeout(Duration::from_secs(15),async {
            let wave=Instant::now();
            let mut clients=JoinSet::new();
            for i in 0..CAP {
                let spec=spec.clone();
                clients.spawn(async move {
                    let c=dial(i,&spec).await;
                    for id in 0..4 {
                        let sent=format!("{waves}:{i}:{id}");
                        assert_eq!(c.request(sent.as_bytes()).await.unwrap(), sent.as_bytes());
                    }
                    c
                });
            }
            let mut live=Vec::with_capacity(CAP);
            while let Some(c)=clients.join_next().await {live.push(c.unwrap());}
            assert_eq!(server.session_count().await,CAP);
            assert_eq!(server.available_permits(),0);
            if waves % 2 == 0 {
                for id in server.active_sessions().await {server.remove_session(id).await.unwrap();}
                while server.available_permits()!=CAP || server.live_session_tasks()!=0 {
                    sleep(Duration::from_millis(5)).await;
                }
            }
            for c in live {c.shutdown().await.unwrap();}
            while server.available_permits()!=CAP || server.live_session_tasks()!=0 {
                sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(server.session_count().await,0);
            assert!(!serving.is_finished());
            max_ms=max_ms.max(wave.elapsed().as_millis());
            total_requests+=CAP*4;
            waves+=1;
            println!("elapsed_ms={} wave={waves} requests={total_requests} permits={} tasks={} max_wave_ms={max_ms}",start.elapsed().as_millis(),server.available_permits(),server.live_session_tasks());
        }).await.unwrap();
        sleep(Duration::from_secs(2)).await;
    }
    assert!(server.shutdown().await.clean);
    timeout(Duration::from_secs(3), &mut serving)
        .await
        .unwrap()
        .unwrap();
    std::net::UdpSocket::bind(&quic).unwrap();
    println!(
        "PASS elapsed_ms={} requests={total_requests} waves={waves} max_wave_ms={max_ms}",
        start.elapsed().as_millis()
    );
}
