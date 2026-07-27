//! Every configurable parameter must mean the same thing everywhere.
//!
//! Three defects motivated this suite, all of the same shape — a number that
//! was documented or configured in one place and quietly different in another:
//!
//! 1. TCP hardcoded a 1 MiB payload cap (and no way to change it) while
//!    WebSocket and QUIC allowed 16 MiB, so an application that worked over
//!    WebSocket lost its connection over TCP on the same message.
//! 2. `ServerLimits::default().mailbox_capacity` was 2048 while the builder's
//!    own fallback was 512, so a server built without explicit limits did not
//!    get the documented default.
//! 3. The client's `ClientEvent` queue was an independent, untunable 8192 that
//!    ignored `ClientLimits::pipe_capacity` in both directions.

use msgtrans::{ClientLimits, ServerLimits};

/// The caps a connection is BUILT with are the caps it reports. If these
/// diverge, an adapter enforcing `decode_limits()` enforces something the
/// caller never asked for.
#[test]
fn configured_frame_caps_are_the_reported_frame_caps() {
    let limits = ServerLimits::new()
        .max_payload_size(4 * 1024 * 1024)
        .max_ext_header_size(1024);
    let decode = limits.connection_limits().decode_limits();

    assert_eq!(decode.max_payload_size, 4 * 1024 * 1024);
    assert_eq!(decode.max_ext_header_size, 1024);
    // Derived, never independent: a frame cap that disagreed with its parts
    // would reject frames whose payload and ext header were each legal.
    assert_eq!(decode.max_frame_size, 16 + 1024 + 4 * 1024 * 1024);
}

/// Out-of-range values are clamped into a representable range rather than
/// producing limits no frame can satisfy (or that exceed what decompression
/// would accept anyway).
#[test]
fn frame_caps_are_clamped_to_something_representable() {
    let huge = ServerLimits::new()
        .max_payload_size(usize::MAX)
        .max_ext_header_size(usize::MAX);
    let decode = huge.connection_limits().decode_limits();
    assert_eq!(decode.max_payload_size, msgtrans::DEFAULT_MAX_PAYLOAD_SIZE);
    assert_eq!(
        decode.max_ext_header_size,
        u16::MAX as usize,
        "the wire field is a u16; a larger cap could never be reached"
    );

    let tiny = ServerLimits::new().max_payload_size(0);
    assert!(
        tiny.connection_limits().decode_limits().max_payload_size >= 64,
        "a zero payload cap would reject every frame"
    );
}

/// All three protocols must start from the SAME caps. This is the assertion
/// that fails if a future adapter reintroduces its own constants.
#[test]
fn every_protocol_starts_from_the_same_caps() {
    let server = ServerLimits::new().connection_limits().decode_limits();
    let client = ClientLimits::new().connection_limits().decode_limits();
    assert_eq!(
        server, client,
        "a client and server built from defaults must agree on what fits"
    );
    assert_eq!(server.max_payload_size, msgtrans::DEFAULT_MAX_PAYLOAD_SIZE);
}

/// The documented default and the builder's fallback are the same value.
#[test]
fn the_documented_mailbox_default_is_the_real_default() {
    assert_eq!(
        ServerLimits::new().mailbox(),
        msgtrans::DEFAULT_MAILBOX_CAPACITY
    );
}

/// The mailbox is a fast-draining hop in FRONT of the outbound queue, so it
/// must stay the smaller of the two: making it larger moves the backpressure
/// point off the queue that was sized for it. This is knowable at compile
/// time, so it fails the build rather than a test run.
const _: () = assert!(msgtrans::DEFAULT_MAILBOX_CAPACITY < msgtrans::DEFAULT_OUTBOUND_CAPACITY);

/// The defect, over a real socket: a 2 MiB message.
///
/// TCP's hardcoded 1 MiB cap made `is_valid_header_at` reject the header, so
/// the frame was treated as corruption and the connection died — while the
/// identical message went through on WebSocket and QUIC. Nothing the caller
/// could configure changed that.
#[cfg(feature = "tcp")]
#[tokio::test(flavor = "multi_thread")]
async fn a_payload_over_the_old_tcp_cap_survives_a_real_tcp_connection() {
    use async_trait::async_trait;
    use msgtrans::{
        Packet, Responder, SessionHandler, SessionId, SessionSender, TcpClientConfig,
        TcpServerConfig, TransportClientBuilder, TransportServerBuilder,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::Notify;

    const SIZE: usize = 2 * 1024 * 1024; // > the old 1 MiB TCP cap

    struct Capture {
        seen: Arc<Mutex<Option<usize>>>,
        ready: Arc<Notify>,
    }
    #[async_trait]
    impl SessionHandler for Capture {
        async fn on_message(&self, _s: SessionId, packet: Packet, _tx: SessionSender) {
            *self.seen.lock().unwrap() = Some(packet.payload().len());
            self.ready.notify_waiters();
        }
        async fn on_request(&self, _s: SessionId, _p: Packet, _r: Responder) {}
    }

    let addr = "127.0.0.1:28981";
    let seen = Arc::new(Mutex::new(None));
    let ready = Arc::new(Notify::new());
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("server cfg"))
        .build(Arc::new(Capture {
            seen: seen.clone(),
            ready: ready.clone(),
        }))
        .await
        .expect("server builds");
    let serving = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.serve().await;
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("client cfg"))
        .build()
        .await
        .expect("client builds");
    client.connect().await.expect("connect");

    let notified = ready.notified();
    client
        .send(&vec![b'x'; SIZE][..])
        .await
        .expect("a 2 MiB one-way send must be written");
    tokio::time::timeout(Duration::from_secs(10), notified)
        .await
        .expect("the server must receive it instead of closing the connection");

    assert_eq!(*seen.lock().unwrap(), Some(SIZE));

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}
