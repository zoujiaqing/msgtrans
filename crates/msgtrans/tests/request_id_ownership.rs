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

/// With the codec available, the same call succeeds, really compresses, and the
/// compressed packet survives the FAN-OUT to a real connected peer.
///
/// A report from a server with zero sessions is `is_complete()` by definition,
/// so it proves only that preparation succeeded. This test connects a client
/// first, so the assertions cover the whole path: prepared once → cloned per
/// session → written → decompressed on receipt.
#[cfg(feature = "zstd")]
#[tokio::test(flavor = "multi_thread")]
async fn broadcast_applies_compression_when_available() {
    // Long and repetitive so compression actually shrinks it; a payload that
    // grew would make the "smaller on the wire" assertion meaningless.
    const PLAINTEXT: &[u8] =
        b"broadcast payload, broadcast payload, broadcast payload, broadcast payload";

    let addr = "127.0.0.1:28976";
    let (server, serving) = start_server(addr).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("client cfg"))
        .build()
        .await
        .expect("client builds");
    client.connect().await.expect("connect");
    let mut events = client.events().await.expect("events");
    // The session must be installed before the fan-out, otherwise this is the
    // empty-report test again.
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.session_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("server registered the session");

    let report = server
        .broadcast(
            bytes::Bytes::from_static(PLAINTEXT),
            SendOptions::new()
                .biz_type(7)
                .compression(msgtrans::CompressionType::Zstd),
        )
        .await
        .expect("zstd is available, so preparation succeeds");
    assert_eq!(report.delivered, 1, "the connected session must be reached");
    assert!(report.is_complete(), "failures: {:?}", report.failed);

    let received = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.next().await {
                Some(msgtrans::ClientEvent::Message(msg)) => return msg,
                Some(_) => continue,
                None => panic!("event stream ended before the broadcast arrived"),
            }
        }
    })
    .await
    .expect("broadcast arrived");

    assert_eq!(received.biz_type(), 7);
    // `ClientMessage` deliberately exposes no compression accessor — the
    // consumer is only ever handed plaintext. The header's flag is checked on
    // the `Packet` the SERVER handler sees, in
    // `server_handler_receives_decompressed_payload`.
    assert_eq!(
        received.payload().as_ref(),
        PLAINTEXT,
        "the peer must see plaintext, so the packet really was compressed \
         once and decompressed on receipt"
    );

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

/// The SERVER-initiated direction: `ClientRequest::respond_with_options`.
///
/// The server-side `Responder` had an end-to-end test; the client side had only
/// a unit test proving the options reach the write path. That left the two
/// directions asymmetrically covered right before a freeze — a client whose
/// response was compressed but never decompressed on the server would have
/// passed everything.
#[cfg(feature = "zstd")]
#[tokio::test(flavor = "multi_thread")]
async fn compressed_client_response_reaches_the_server_as_plaintext() {
    const ANSWER: &[u8] =
        b"client answer, client answer, client answer, client answer, client answer";

    let addr = "127.0.0.1:28979";
    let (server, serving) = start_server(addr).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("client cfg"))
        .build()
        .await
        .expect("client builds");
    client.connect().await.expect("connect");
    let mut events = client.events().await.expect("events");

    // The client answers whatever the server asks, with a compressed body.
    let responder = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let msgtrans::ClientEvent::Request(request) = event {
                let outcome = request
                    .respond_with_options(
                        bytes::Bytes::from_static(ANSWER),
                        SendOptions::new().compression(msgtrans::CompressionType::Zstd),
                    )
                    .await
                    .expect("compressed respond");
                assert_eq!(outcome, msgtrans::RespondOutcome::Written);
                return;
            }
        }
        panic!("the client never saw the server's request");
    });

    let session = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(id) = server.active_sessions().await.first().copied() {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("server registered the session");

    let response = server
        .request_with_options(
            session,
            bytes::Bytes::from_static(b"ask the client"),
            RequestOptions::new().timeout(Duration::from_secs(5)),
        )
        .await
        .expect("client answered");
    assert_eq!(
        response.as_ref(),
        ANSWER,
        "a compressed client response must be decompressed before the server sees it"
    );

    responder.await.expect("responder task");
    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

/// The companion falsifier for the test below.
///
/// On its own, "the requester got plaintext" cannot tell a response that was
/// compressed and then decompressed from one whose options were dropped on the
/// floor. Here the build has no Zstd codec, so honouring the options is
/// OBSERVABLE: `respond_with_options` must fail. An implementation that ignored
/// them would happily report `Written`.
#[cfg(not(feature = "zstd"))]
#[tokio::test(flavor = "multi_thread")]
async fn respond_with_options_fails_when_the_codec_is_missing() {
    use std::sync::Mutex;

    type RespondResult = Arc<Mutex<Option<Result<msgtrans::RespondOutcome, String>>>>;

    struct Compressing(RespondResult);
    #[async_trait]
    impl SessionHandler for Compressing {
        async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
        async fn on_request(&self, _s: SessionId, _p: Packet, responder: Responder) {
            let outcome = responder
                .respond_with_options(
                    bytes::Bytes::from_static(b"answer"),
                    SendOptions::new().compression(msgtrans::CompressionType::Zstd),
                )
                .await
                .map_err(|e| e.to_string());
            *self.0.lock().unwrap() = Some(outcome);
        }
    }

    let addr = "127.0.0.1:28978";
    let seen: RespondResult = Arc::new(Mutex::new(None));
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("server cfg"))
        .build(Arc::new(Compressing(seen.clone())))
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

    // The respond fails, so the request is never answered and this times out —
    // which is itself the honest outcome: no half-compressed frame was sent.
    let _ = client
        .request_with_options(
            bytes::Bytes::from_static(b"ask"),
            RequestOptions::new().timeout(Duration::from_millis(800)),
        )
        .await;

    let outcome = seen.lock().unwrap().clone().expect("handler ran");
    assert!(
        outcome.is_err(),
        "asking for a codec this build lacks must fail the respond, \
         not silently answer uncompressed: {outcome:?}"
    );

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}

/// End-to-end cover for `Responder::respond_with_options`: a compressed
/// RESPONSE must reach the requester as plaintext.
///
/// The unit tests either check that the options reach the write path or that
/// `apply_over` compresses; neither observes the whole chain, so a response
/// that was compressed but never decompressed (the fourth review's inbound
/// defect, in the response direction) would still pass them.
#[cfg(feature = "zstd")]
#[tokio::test(flavor = "multi_thread")]
async fn compressed_response_reaches_the_requester_as_plaintext() {
    const ANSWER: &[u8] = b"response payload, response payload, response payload, response payload";

    /// Answers every request with a Zstd-compressed body.
    struct Compressing;
    #[async_trait]
    impl SessionHandler for Compressing {
        async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
        async fn on_request(&self, _s: SessionId, _p: Packet, responder: Responder) {
            let outcome = responder
                .respond_with_options(
                    bytes::Bytes::from_static(ANSWER),
                    SendOptions::new().compression(msgtrans::CompressionType::Zstd),
                )
                .await
                .expect("compressed respond");
            assert_eq!(outcome, msgtrans::RespondOutcome::Written);
        }
    }

    let addr = "127.0.0.1:28977";
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("server cfg"))
        .build(Arc::new(Compressing))
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

    let response = client
        .request_with_options(
            bytes::Bytes::from_static(b"ask"),
            RequestOptions::new().timeout(Duration::from_secs(5)),
        )
        .await
        .expect("request answered");
    assert_eq!(
        response.as_ref(),
        ANSWER,
        "a compressed response must be decompressed before the requester sees it"
    );

    let _ = client.shutdown().await;
    let _ = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    serving.abort();
}
