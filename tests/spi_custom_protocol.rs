//! A COMPLETE custom protocol, implemented using only `msgtrans::spi`, driven
//! end-to-end through the public `TransportServerBuilder`/`TransportClientBuilder`.
//!
//! `spi_external.rs` proves a `Connection` compiles against the SPI; this goes
//! further and proves the SPI is *sufficient*: an out-of-crate protocol can be
//! registered on both builders and carry a real request/response round trip,
//! with no built-in protocol feature involved. It is the regression test for
//! the config-trait surface — if the object-safe trait set ever stops being
//! enough to plug in a protocol, this stops compiling.

use async_trait::async_trait;
use msgtrans::spi::{
    event_channel, CloseReason, Connection, ConnectionEvents, ConnectionInfo, ConnectionWriter,
    DynClientConfig, DynProtocolConfig, DynServerConfig, EventSink, Packet, SessionId,
    TransportError, WriteCompletion,
};
use msgtrans::{
    FramePolicy, Responder, SessionHandler, SessionSender, TransportClientBuilder,
    TransportServerBuilder,
};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// The "wire": a process-global pair of in-memory channels the two endpoints
/// rendezvous on. Stands in for a socket so the test needs no networking.
type Wire = (
    mpsc::Sender<Packet>,
    Arc<Mutex<Option<mpsc::Receiver<Packet>>>>,
);

fn client_to_server() -> &'static Wire {
    static W: OnceLock<Wire> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = mpsc::channel(64);
        (tx, Arc::new(Mutex::new(Some(rx))))
    })
}

fn server_to_client() -> &'static Wire {
    static W: OnceLock<Wire> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = mpsc::channel(64);
        (tx, Arc::new(Mutex::new(Some(rx))))
    })
}

// ---------------------------------------------------------------- connection

struct PipeWriter {
    out: mpsc::Sender<Packet>,
}

#[async_trait]
impl ConnectionWriter for PipeWriter {
    async fn send_with_completion(
        &self,
        packet: Packet,
        completion: WriteCompletion,
    ) -> Result<(), TransportError> {
        match self.out.send(packet).await {
            // The write reached the "socket": confirm truthfully.
            Ok(()) => completion.complete(Ok(())),
            // Dropping the completion reports the failure by construction.
            Err(_) => drop(completion),
        }
        Ok(())
    }
}

struct PipeConnection {
    session_id: SessionId,
    out: mpsc::Sender<Packet>,
    events: Option<ConnectionEvents>,
    reader: Option<tokio::task::JoinHandle<()>>,
}

impl PipeConnection {
    /// Build one endpoint: write into `out`, and pump `inbound` into the sink.
    fn new(out: mpsc::Sender<Packet>, mut inbound: mpsc::Receiver<Packet>) -> Self {
        let (sink, events) = event_channel(std::num::NonZeroUsize::new(64).unwrap());
        let reader = tokio::spawn(async move {
            let sink: EventSink = sink;
            while let Some(packet) = inbound.recv().await {
                // Typed data plane: backpressured, never silently dropped.
                if !sink.message(packet).await {
                    break;
                }
            }
            sink.close(CloseReason::Normal);
        });
        Self {
            session_id: SessionId::new(0),
            out,
            events: Some(events),
            reader: Some(reader),
        }
    }
}

#[async_trait]
impl Connection for PipeConnection {
    fn writer(&self) -> Arc<dyn ConnectionWriter> {
        Arc::new(PipeWriter {
            out: self.out.clone(),
        })
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        Ok(())
    }
    fn session_id(&self) -> SessionId {
        self.session_id
    }
    fn set_session_id(&mut self, session_id: SessionId) {
        self.session_id = session_id;
    }
    fn connection_info(&self) -> ConnectionInfo {
        let mut info = ConnectionInfo::default();
        info.protocol = "pipe".to_string();
        info.session_id = self.session_id;
        info.peer_addr = "127.0.0.1:9".parse().unwrap();
        info.local_addr = "127.0.0.1:9".parse().unwrap();
        info
    }
    fn is_connected(&self) -> bool {
        !self.out.is_closed()
    }
    async fn flush(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
    fn take_event_pipe(&mut self) -> Option<ConnectionEvents> {
        self.events.take()
    }
    fn set_frame_policy(&self, _policy: FramePolicy) {
        // This protocol carries whole packets, so there is no framing to relax.
    }
}

// -------------------------------------------------------------------- server

struct PipeServer {
    handed_out: bool,
}

#[async_trait]
impl msgtrans::spi::Server for PipeServer {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError> {
        if self.handed_out {
            // Exactly one connection exists on this in-memory wire; park so the
            // accept loop simply idles instead of spinning.
            std::future::pending::<()>().await;
        }
        self.handed_out = true;
        let rx = client_to_server()
            .1
            .lock()
            .unwrap()
            .take()
            .expect("server takes the inbound half once");
        Ok(Box::new(PipeConnection::new(
            server_to_client().0.clone(),
            rx,
        )))
    }
    async fn shutdown(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        Ok("127.0.0.1:9".parse().unwrap())
    }
}

// -------------------------------------------------------------------- config

#[derive(Clone)]
struct PipeConfig;

impl DynProtocolConfig for PipeConfig {
    fn protocol_name(&self) -> &'static str {
        "pipe"
    }
    fn validate_dyn(&self) -> Result<(), msgtrans::spi::ConfigError> {
        Ok(())
    }
}

impl DynServerConfig for PipeConfig {
    fn build_server_dyn(
        &self,
        _limits: msgtrans::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Box<dyn msgtrans::Server>, TransportError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            Ok(Box::new(PipeServer { handed_out: false }) as Box<dyn msgtrans::Server>)
        })
    }
    fn get_bind_address(&self) -> std::net::SocketAddr {
        "127.0.0.1:9".parse().unwrap()
    }
    fn clone_server_dyn(&self) -> Box<dyn DynServerConfig> {
        Box::new(self.clone())
    }
}

impl DynClientConfig for PipeConfig {
    fn build_connection_dyn(
        &self,
        _limits: msgtrans::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Box<dyn Connection>, TransportError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let rx = server_to_client()
                .1
                .lock()
                .unwrap()
                .take()
                .expect("client takes the inbound half once");
            Ok(
                Box::new(PipeConnection::new(client_to_server().0.clone(), rx))
                    as Box<dyn Connection>,
            )
        })
    }
    fn clone_client_dyn(&self) -> Box<dyn DynClientConfig> {
        Box::new(self.clone())
    }
}

// --------------------------------------------------------------------- test

struct Echo;

#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
    async fn on_request(&self, _s: SessionId, request: Packet, responder: Responder) {
        let mut body = b"echo:".to_vec();
        body.extend_from_slice(request.payload());
        let _ = responder.respond(body).await;
    }
}

/// A protocol defined entirely outside the crate plugs into BOTH top-level
/// builders and carries a real request/response round trip.
#[tokio::test(flavor = "multi_thread")]
async fn custom_protocol_round_trips_through_the_public_builders() {
    let server = TransportServerBuilder::new()
        .protocol(PipeConfig)
        .build(Arc::new(Echo))
        .await
        .expect("server builds from a custom DynServerConfig");
    let serving = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.serve().await;
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client = TransportClientBuilder::new()
        .protocol(PipeConfig)
        .build()
        .await
        .expect("client builds from a custom DynClientConfig");
    client
        .connect()
        .await
        .expect("connects over the custom protocol");

    let response = tokio::time::timeout(Duration::from_secs(5), client.request(b"ping"))
        .await
        .expect("no timeout")
        .expect("request succeeds over the custom protocol");
    assert_eq!(
        response.as_ref(),
        b"echo:ping",
        "the custom protocol must carry the response payload intact"
    );

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}
