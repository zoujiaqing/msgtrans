//! Request ids belong to the transport, and the public API must make that
//! impossible to violate.
//!
//! Regression cover for the three defects found in the alpha.4 review:
//!
//! 1. A caller-numbered `Request` could be pushed through a raw send path
//!    (`send_to_session` / `SessionSender::send` / `broadcast`). Its response
//!    then completed a DIFFERENT, tracked waiter that happened to be allocated
//!    the same id.
//! 2. `TransportServer::request_with_options` advertised a `timeout` and then
//!    dropped it, always waiting the fixed 10s default.
//!
//! The third defect from that review — inbound compressed packets forwarded
//! still compressed — is covered by the unit test
//! `inbound_compressed_packet_is_decompressed_before_dispatch`, since no public
//! API can put a pre-compressed packet on the wire for an over-the-socket test
//! to observe.

#![cfg(feature = "tcp")]

use async_trait::async_trait;
use msgtrans::{
    Packet, RequestOptions, Responder, SendOptions, SessionHandler, SessionId, SessionSender,
    TcpClientConfig, TcpServerConfig, TransportClientBuilder, TransportServerBuilder,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Echoes requests back; never answers `biz_type == SILENT` so a timeout can be
/// measured.
const SILENT: u8 = 200;

struct Echo;

#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
    async fn on_request(&self, _s: SessionId, request: Packet, responder: Responder) {
        if request.biz_type() == SILENT {
            // Drop the responder: the request is deliberately never answered.
            return;
        }
        let _ = responder.respond(request.into_payload()).await;
    }
}

async fn start_server(addr: &str) -> (msgtrans::TransportServer, tokio::task::JoinHandle<()>) {
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("server cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server builds");
    let serving = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.serve().await;
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    (server, serving)
}

/// **The type boundary.** No public API accepts a caller-built `Packet`, so a
/// `Request` numbered outside the registry cannot reach the wire at all.
///
/// This is a compile-time assertion by construction: `send_to_session` is
/// crate-internal, `broadcast`/`send_with_options` take payload + options, and
/// `SessionSender` only exposes `send_data*`. If any of them regains a
/// `Packet` parameter, the lines below stop compiling as written and this test
/// must be revisited.
#[tokio::test(flavor = "multi_thread")]
async fn public_api_cannot_send_a_caller_numbered_request() {
    let addr = "127.0.0.1:28971";
    let (server, serving) = start_server(addr).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("client cfg"))
        .build()
        .await
        .expect("client builds");
    // Answer server-initiated requests, except the deliberately SILENT ones.
    let mut events = client.events().await.expect("events");
    let responder_task = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let msgtrans::ClientEvent::Request(req) = event {
                if req.biz_type() == SILENT {
                    continue;
                }
                let payload = req.payload().clone();
                req.respond_detached(payload);
            }
        }
    });
    client.connect().await.expect("connect");
    tokio::time::sleep(Duration::from_millis(150)).await;

    let sessions = server.active_sessions().await;
    let session_id = *sessions.first().expect("one session");

    // The ONLY server-side one-way send takes bytes + options; the id is
    // allocated by the transport and the packet type is fixed to one-way.
    let receipt = server
        .send_with_options(
            session_id,
            bytes::Bytes::from_static(b"one-way"),
            SendOptions::new().biz_type(7),
        )
        .await
        .expect("one-way send");
    assert!(
        receipt.message_id > 0,
        "the transport must assign the message id"
    );

    // Broadcast likewise takes bytes, not a packet.
    let report = server
        .broadcast(
            bytes::Bytes::from_static(b"broadcast"),
            SendOptions::new().biz_type(7),
        )
        .await
        .expect("broadcast prepares");
    assert!(report.is_complete(), "broadcast reached every session");

    // And a real tracked request still round-trips correctly alongside them.
    let response = server
        .request(session_id, b"ping")
        .await
        .expect("tracked request");
    assert_eq!(response.as_ref(), b"ping");

    let _ = client.shutdown().await;
    responder_task.abort();
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

/// `request_with_options` must honor the caller's timeout.
///
/// Previously `options.timeout` was parsed and then ignored, so a 30ms deadline
/// still blocked for the fixed 10s default.
#[tokio::test(flavor = "multi_thread")]
async fn server_request_honors_the_caller_timeout() {
    let addr = "127.0.0.1:28972";
    let (server, serving) = start_server(addr).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("client cfg"))
        .build()
        .await
        .expect("client builds");
    client.connect().await.expect("connect");
    tokio::time::sleep(Duration::from_millis(150)).await;

    let sessions = server.active_sessions().await;
    let session_id = *sessions.first().expect("one session");

    // The client never answers this biz_type, so only the timeout can end it.
    let started = Instant::now();
    let result = server
        .request_with_options(
            session_id,
            bytes::Bytes::from_static(b"no answer"),
            RequestOptions::new()
                .biz_type(SILENT)
                .timeout(Duration::from_millis(30)),
        )
        .await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "timeout was ignored: waited {elapsed:?} for a 30ms deadline"
    );
    // Assert the exact error, not just "some error fast": the wait honored the
    // caller's deadline while the error object still reported a hardcoded 10s,
    // which made telemetry and retry decisions wrong.
    let err = result.expect_err("an unanswered request must time out");
    assert!(
        matches!(
            &err,
            msgtrans::TransportError::Timeout { duration, .. }
                if *duration == Duration::from_millis(30)
        ),
        "expected Timeout{{duration: 30ms}}, got {err:?}"
    );

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

/// The SERVER must receive plaintext too — this is the defect the fourth review
/// reproduced over real TCP: only the client normalized, so a compressing peer
/// left `SessionHandler` holding compressed bytes and a stale `compression`
/// flag.
#[cfg(feature = "zstd")]
#[tokio::test(flavor = "multi_thread")]
async fn server_handler_receives_decompressed_payload() {
    use std::sync::Mutex;
    use tokio::sync::Notify;

    const PLAINTEXT: &[u8] =
        b"the quick brown fox jumps over the lazy dog, repeatedly and compressibly";

    /// What the handler observed: the payload bytes and the packet's declared
    /// compression. (Named so the strict `clippy::type_complexity` gate in the
    /// all-features CI job stays clean without an `allow`.)
    type Observed = Arc<Mutex<Option<(Vec<u8>, msgtrans::CompressionType)>>>;

    struct Capture {
        seen: Observed,
        ready: Arc<Notify>,
    }
    #[async_trait]
    impl SessionHandler for Capture {
        async fn on_message(&self, _s: SessionId, packet: Packet, _tx: SessionSender) {
            *self.seen.lock().unwrap() = Some((packet.payload().to_vec(), packet.compression()));
            self.ready.notify_waiters();
        }
        async fn on_request(&self, _s: SessionId, _p: Packet, _r: Responder) {}
    }

    let addr = "127.0.0.1:28974";
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
        .send_with_options(
            PLAINTEXT,
            SendOptions::new()
                .biz_type(9)
                .compression(msgtrans::CompressionType::Zstd),
        )
        .await
        .expect("compressed send");
    tokio::time::timeout(Duration::from_secs(5), notified)
        .await
        .expect("handler saw the message");

    let (payload, compression) = seen.lock().unwrap().clone().expect("captured");
    assert_eq!(
        payload, PLAINTEXT,
        "SessionHandler must receive plaintext, not compressed bytes"
    );
    assert_eq!(
        compression,
        msgtrans::CompressionType::None,
        "the header must not still claim a compression that was undone"
    );

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

/// `broadcast` must not silently ignore a requested compression.
///
/// It used to copy `biz_type`/`ext_header` by hand and never call
/// `SendOptions::apply`, so asking for Zstd on a build without the codec
/// reported a successful delivery and put PLAINTEXT on the wire.
#[cfg(not(feature = "zstd"))]
#[tokio::test(flavor = "multi_thread")]
async fn broadcast_reports_unavailable_compression() {
    let addr = "127.0.0.1:28975";
    let (server, serving) = start_server(addr).await;

    let result = server
        .broadcast(
            bytes::Bytes::from_static(b"payload"),
            SendOptions::new()
                .biz_type(7)
                .compression(msgtrans::CompressionType::Zstd),
        )
        .await;
    assert!(
        result.is_err(),
        "requesting a compression the build cannot perform must fail, \
         not silently broadcast plaintext"
    );

    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

/// With the codec available, the same call succeeds and really compresses.
#[cfg(feature = "zstd")]
#[tokio::test(flavor = "multi_thread")]
async fn broadcast_applies_compression_when_available() {
    let addr = "127.0.0.1:28976";
    let (server, serving) = start_server(addr).await;

    let report = server
        .broadcast(
            bytes::Bytes::from_static(b"payload"),
            SendOptions::new()
                .biz_type(7)
                .compression(msgtrans::CompressionType::Zstd),
        )
        .await
        .expect("zstd is available, so preparation succeeds");
    assert!(report.is_complete());

    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}
