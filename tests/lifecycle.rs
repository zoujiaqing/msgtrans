//! Lifecycle determinism over real sockets: dropping a client releases the
//! server session without an explicit disconnect; requests work across a
//! reconnect and stale generations cannot cancel them; shutdown() is
//! awaitable and reports what it actually did.

use async_trait::async_trait;
use msgtrans::{
    Packet, Responder, SessionHandler, SessionId, SessionSender, TcpClientConfig, TcpServerConfig,
    TransportClient, TransportClientBuilder, TransportServer, TransportServerBuilder,
};
use std::{sync::Arc, time::Duration};

struct Echo;

#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _s: SessionId, _packet: Packet, _sender: SessionSender) {}

    async fn on_request(&self, _s: SessionId, request: Packet, responder: Responder) {
        let _ = responder.respond(request.into_payload()).await;
    }
}

async fn start_server(addr: &str) -> TransportServer {
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    server
}

async fn connect(addr: &str) -> TransportClient {
    let mut c = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("cfg"))
        .build()
        .await
        .expect("client");
    c.connect().await.expect("connect");
    c
}

async fn echo_ok(c: &TransportClient) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(3), c.request(b"ping".as_slice())).await,
        Ok(Ok(response)) if response.as_ref() == b"ping"
    )
}

/// Dropping the client (no disconnect) must release the server session: the
/// forwarding task is aborted in Drop, background tasks hold only Weak, so
/// the socket closes and the server reaps the session.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_client_releases_server_session() {
    let addr = "127.0.0.1:28881";
    let server = start_server(addr).await;
    let client = connect(addr).await;
    assert!(echo_ok(&client).await);
    assert_eq!(server.session_count().await, 1);

    drop(client); // no disconnect on purpose

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.session_count().await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "server session not released after client drop"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Requests must work across a reconnect, and the old generation's teardown
/// must not cancel the new generation's in-flight request.
#[tokio::test(flavor = "multi_thread")]
async fn requests_survive_reconnect() {
    let addr = "127.0.0.1:28882";
    let _server = start_server(addr).await;
    let mut client = connect(addr).await;
    assert!(echo_ok(&client).await, "request on generation 1");

    client.disconnect().await.expect("disconnect");
    client.connect().await.expect("reconnect");
    // Immediately request on the new generation: a stale close from
    // generation 1 arriving late must not cancel this.
    assert!(echo_ok(&client).await, "request on generation 2");
    assert!(echo_ok(&client).await, "second request on generation 2");
}

/// shutdown() closes sessions, waits for the drain, and reports honestly.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_drains_sessions_and_reports() {
    let addr = "127.0.0.1:28883";
    let server = start_server(addr).await;
    let c1 = connect(addr).await;
    let c2 = connect(addr).await;
    assert!(echo_ok(&c1).await && echo_ok(&c2).await);
    assert_eq!(server.session_count().await, 2);

    let report = server.shutdown_with_timeout(Duration::from_secs(10)).await;
    assert!(report.clean, "shutdown not clean: {:?}", report);
    assert_eq!(report.sessions_remaining, 0);
    assert!(
        report.sessions_closed >= 2,
        "closed {}",
        report.sessions_closed
    );
    assert_eq!(server.session_count().await, 0);
}

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

struct GatedEcho {
    gate: Arc<Notify>,
    open: Arc<AtomicBool>,
}

#[async_trait]
impl SessionHandler for GatedEcho {
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {
        while !self.open.load(Ordering::SeqCst) {
            self.gate.notified().await;
        }
    }

    async fn on_request(&self, _s: SessionId, _p: Packet, responder: Responder) {
        while !self.open.load(Ordering::SeqCst) {
            self.gate.notified().await;
        }
        let _ = responder;
    }
}

/// The zombie-session regression: shutdown's deadline fires while a close is
/// already past the Closing state transition (the actor is blocked, its tiny
/// mailbox full, so the close parks feeding it the close event). Cancelling
/// that close future would strand the session as a permanent zombie — both
/// close paths gate on "already closing" and would return Ok forever. With
/// owned close tasks, the deadline only abandons the JOIN: the close keeps
/// running, finishes once unblocked, and a second shutdown finds nothing left.
#[tokio::test(flavor = "multi_thread")]
async fn deadline_during_close_leaves_no_zombie_session() {
    let addr = "127.0.0.1:28884";
    let gate = Arc::new(Notify::new());
    let open = Arc::new(AtomicBool::new(false));
    let server = TransportServerBuilder::new()
        .actor_buffer_size(4)
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(GatedEcho {
            gate: gate.clone(),
            open: open.clone(),
        }))
        .await
        .expect("server");
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let client = connect(addr).await;
    // Block the actor and fill its mailbox so the close event cannot enter.
    for i in 0..12u32 {
        client
            .send(format!("{}", i).as_bytes())
            .await
            .expect("send");
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.session_count().await, 1);

    // Tiny budget: the deadline WILL fire mid-close.
    let started = std::time::Instant::now();
    let report = server
        .shutdown_with_timeout(Duration::from_millis(300))
        .await;
    let elapsed = started.elapsed();
    assert!(!report.clean, "blocked actor must force clean=false");
    assert_eq!(report.sessions_remaining, 1);
    assert!(
        elapsed < Duration::from_secs(2),
        "small timeout must bound real elapsed, took {:?}",
        elapsed
    );

    // Unblock: the detached close task must finish the job on its own.
    open.store(true, Ordering::SeqCst);
    gate.notify_waiters();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.session_count().await != 0 {
        gate.notify_waiters();
        assert!(
            tokio::time::Instant::now() < deadline,
            "zombie session: close never completed after unblocking"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A second shutdown must be clean and instant — no stuck Closing state.
    let report2 = server.shutdown_with_timeout(Duration::from_secs(2)).await;
    assert!(
        report2.clean,
        "second shutdown hit zombie state: {:?}",
        report2
    );
    assert_eq!(report2.sessions_remaining, 0);
    drop(client);
}

use msgtrans::{QuicServerConfig, WebSocketServerConfig};

/// Full acceptance: after clean shutdown the serve() handle has completed and
/// all three protocol endpoints (TCP/WS/QUIC) can be re-bound immediately by
/// a brand-new server, which then actually serves.
#[tokio::test(flavor = "multi_thread")]
async fn clean_shutdown_completes_serve_and_frees_all_ports() {
    let (tcp, ws, quic) = ("127.0.0.1:28885", "127.0.0.1:28886", "127.0.0.1:28887");
    let build = || async {
        TransportServerBuilder::new()
            .protocol(TcpServerConfig::new(tcp).expect("tcp"))
            .protocol(WebSocketServerConfig::new(ws).expect("ws"))
            .protocol(QuicServerConfig::new(quic).expect("quic"))
            .build(Arc::new(Echo))
            .await
            .expect("server")
    };
    let server = build().await;
    let serve_handle = {
        let bg = server.clone();
        tokio::spawn(async move { bg.serve().await })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    let client = connect(tcp).await;
    assert!(echo_ok(&client).await);
    drop(client);

    let report = server.shutdown_with_timeout(Duration::from_secs(10)).await;
    assert!(report.clean, "not clean: {:?}", report);
    assert!(report.infra_stopped);
    assert_eq!(
        server.live_session_tasks(),
        0,
        "clean=true must mean the task tracker is empty"
    );

    // Ownership semantics: infra_stopped proves the OWNED infra actually
    // joined (Stopped is published by the owning task after tracker.wait()).
    // serve() is a pure observer now, so its future completing is a separate
    // scheduler wakeup — assert the REAL invariant at the instant (ports
    // free, below) and give the observer a short bound to be scheduled.
    tokio::time::timeout(Duration::from_secs(1), serve_handle)
        .await
        .expect("serve() observer must complete promptly after Stopped")
        .expect("join")
        .ok();

    // All three endpoints are free: a new server binds the SAME addresses and
    // actually serves.
    let server2 = build().await;
    let bg2 = server2.clone();
    tokio::spawn(async move {
        let _ = bg2.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    let client2 = connect(tcp).await;
    assert!(echo_ok(&client2).await, "rebound server must serve");
    drop(client2);
    let report2 = server2.shutdown_with_timeout(Duration::from_secs(10)).await;
    assert!(report2.clean, "second server shutdown: {:?}", report2);
}

/// Shutdown/accept interleave: connections hammer the server while shutdown
/// runs. After the report returns and racers settle, no session may remain —
/// gate refusals and post-insert self-cancels both end in zero.
#[tokio::test(flavor = "multi_thread")]
async fn no_sessions_survive_shutdown_accept_interleave() {
    let addr = "127.0.0.1:28888";
    let server = start_server(addr).await;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hammer = {
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::SeqCst) {
                if let Ok(cfg) = TcpClientConfig::new(addr) {
                    if let Ok(mut c) = TransportClientBuilder::new().protocol(cfg).build().await {
                        let _ = c.connect().await;
                        // keep briefly, then drop
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let report = server.shutdown_with_timeout(Duration::from_secs(10)).await;
    stop.store(true, Ordering::SeqCst);
    let _ = hammer.await;
    // Post-report racers self-cancel; settle then verify emptiness sticks.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        server.session_count().await,
        0,
        "sessions survived the interleave (report: {:?})",
        report
    );
    let report2 = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    assert!(report2.clean, "post-interleave shutdown: {:?}", report2);
}

/// With a cap configured, clean shutdown restores every permit.
#[tokio::test(flavor = "multi_thread")]
async fn permits_fully_restored_after_shutdown() {
    let addr = "127.0.0.1:28889";
    const CAP: usize = 2;
    let server = TransportServerBuilder::new()
        .max_connections(CAP)
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let c1 = connect(addr).await;
    let c2 = connect(addr).await;
    assert!(echo_ok(&c1).await && echo_ok(&c2).await);
    assert_eq!(server.available_permits(), 0);

    let report = server.shutdown_with_timeout(Duration::from_secs(10)).await;
    assert!(report.clean, "{:?}", report);
    assert!(report.permits_restored);
    assert_eq!(server.available_permits(), CAP);
    drop((c1, c2));
}

/// Concurrent and repeated shutdowns are idempotent: both callers return, the
/// server ends drained, and a follow-up shutdown is instantly clean.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_and_repeated_shutdowns_are_idempotent() {
    let addr = "127.0.0.1:28890";
    let server = start_server(addr).await;
    let client = connect(addr).await;
    assert!(echo_ok(&client).await);
    drop(client);

    let (a, b) = tokio::join!(
        server.shutdown_with_timeout(Duration::from_secs(10)),
        server.shutdown_with_timeout(Duration::from_secs(10)),
    );
    assert!(
        a.clean || b.clean,
        "at least one owner must finish clean: {:?} / {:?}",
        a,
        b
    );
    let again = server.shutdown_with_timeout(Duration::from_secs(2)).await;
    assert!(again.clean, "repeat shutdown must be clean: {:?}", again);
    assert!(
        again.elapsed < Duration::from_millis(500),
        "repeat must be instant"
    );
}

/// Client shutdown() joins the forwarding task and is idempotent.
#[tokio::test(flavor = "multi_thread")]
async fn client_shutdown_joins_forwarding_and_is_idempotent() {
    let addr = "127.0.0.1:28891";
    let server = start_server(addr).await;
    let mut client = connect(addr).await;
    assert!(echo_ok(&client).await);

    client.shutdown().await.expect("shutdown");
    client.shutdown().await.expect("shutdown twice is fine");
    drop(client);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while server.session_count().await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "session not released"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let report = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    assert!(report.clean);
}

/// serve() is one-shot: shutdown-before-serve and a concurrent second serve
/// are both rejected before touching shared state, and the running instance
/// is unaffected.
#[tokio::test(flavor = "multi_thread")]
async fn serve_is_one_shot_and_shutdown_before_serve_finalizes() {
    // shutdown before serve: clean (never served), then serve() refuses.
    let s1 = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new("127.0.0.1:28892").expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let report = s1.shutdown_with_timeout(Duration::from_secs(2)).await;
    assert!(report.clean, "{:?}", report);
    assert!(
        s1.serve().await.is_err(),
        "serve after shutdown must be rejected"
    );

    // concurrent second serve rejected; first keeps serving.
    let addr = "127.0.0.1:28893";
    let s2 = start_server(addr).await; // first serve running inside
    assert!(
        s2.serve().await.is_err(),
        "second serve must be rejected while the first runs"
    );
    let client = connect(addr).await;
    assert!(echo_ok(&client).await, "first instance must be unaffected");
    drop(client);
    let report = s2.shutdown_with_timeout(Duration::from_secs(10)).await;
    assert!(report.clean, "{:?}", report);
}

/// Partial start failure releases every already-bound endpoint before serve()
/// returns: occupy the TCP port so serve fails, then prove the WS port is
/// immediately free.
#[tokio::test(flavor = "multi_thread")]
async fn partial_start_failure_frees_already_bound_ports() {
    let (tcp, ws) = ("127.0.0.1:28894", "127.0.0.1:28895");
    // Occupy TCP so the msgtrans TCP listener cannot bind.
    let _blocker = tokio::net::TcpListener::bind(tcp).await.expect("blocker");

    let server = TransportServerBuilder::new()
        .protocol(WebSocketServerConfig::new(ws).expect("ws"))
        .protocol(TcpServerConfig::new(tcp).expect("tcp"))
        .build(Arc::new(Echo))
        .await
        .expect("build");
    let result = server.serve().await;
    assert!(
        result.is_err(),
        "serve must fail with the TCP port occupied"
    );

    // Two-phase start: the WS endpoint bound in phase A was dropped before
    // any accept loop ran, so it is free and no session ever existed.
    tokio::net::TcpListener::bind(ws)
        .await
        .expect("WS port must be free immediately after serve() failed");
    assert_eq!(server.session_count().await, 0);
    assert_eq!(server.live_session_tasks(), 0);
}

/// Fast connect/drop churn cannot leave stale supervisor entries: the
/// registration barrier orders start after registration, and the reaper is
/// the only remover. Afterwards a shutdown is clean.
#[tokio::test(flavor = "multi_thread")]
async fn rapid_connect_drop_churn_leaves_no_stale_entries() {
    let addr = "127.0.0.1:28896";
    let server = start_server(addr).await;
    for _ in 0..20 {
        let client = connect(addr).await;
        drop(client); // immediate drop: session may complete extremely fast
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.session_count().await != 0 {
        assert!(tokio::time::Instant::now() < deadline, "sessions leaked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let report = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    assert!(report.clean, "stale supervisor entries: {:?}", report);
}

/// After client shutdown(), connect() is rejected before any network work —
/// the server must never see a session from a stopped client.
#[tokio::test(flavor = "multi_thread")]
async fn connect_after_client_shutdown_is_rejected() {
    let addr = "127.0.0.1:28897";
    let server = start_server(addr).await;
    let mut client = connect(addr).await;
    assert!(echo_ok(&client).await);
    client.shutdown().await.expect("shutdown");

    let before = server.session_count().await; // old session may still drain
    assert!(
        client.connect().await.is_err(),
        "connect after shutdown must be rejected"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        server.session_count().await <= before,
        "a stopped client created a server session"
    );
    drop(client);
}

/// Cancelling the server shutdown FUTURE loses nothing: the session tasks are
/// owned by the tracker, wait() is observation-only, so a second shutdown
/// after unblocking finishes clean.
#[tokio::test(flavor = "multi_thread")]
async fn outer_cancelled_server_shutdown_then_second_is_clean() {
    let addr = "127.0.0.1:28898";
    let gate = Arc::new(Notify::new());
    let open = Arc::new(AtomicBool::new(false));
    let server = TransportServerBuilder::new()
        .actor_buffer_size(4)
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(GatedEcho {
            gate: gate.clone(),
            open: open.clone(),
        }))
        .await
        .expect("server");
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let client = connect(addr).await;
    for i in 0..12u32 {
        client
            .send(format!("{}", i).as_bytes())
            .await
            .expect("send");
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Outer timeout CANCELS the shutdown future while the blocked session is
    // still draining — the owned tracker must not lose the task.
    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        server.shutdown_with_timeout(Duration::from_secs(30)),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "outer timeout should cancel the shutdown"
    );

    open.store(true, Ordering::SeqCst);
    gate.notify_waiters();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.live_session_tasks() != 0 {
        gate.notify_waiters();
        assert!(
            tokio::time::Instant::now() < deadline,
            "session task leaked"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let report = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    assert!(
        report.clean,
        "second shutdown after outer cancel: {:?}",
        report
    );
    drop(client);
}

/// Cancelling the client shutdown FUTURE loses nothing either: the teardown
/// task is owned in a field; a second shutdown observes its completion and
/// performs the formal join.
#[tokio::test] // current_thread ON PURPOSE: the spawned teardown cannot run
               // until we yield, so the first poll of shutdown() is DETERMINISTICALLY
               // pending at its observation await — dropping it there is a guaranteed
               // mid-flight cancellation, not a race.
async fn outer_cancelled_client_shutdown_then_second_joins() {
    let addr = "127.0.0.1:28899";
    let server = start_server(addr).await;
    let mut client = connect(addr).await;
    assert!(echo_ok(&client).await);

    // Poll the shutdown future exactly once, assert it parked at its
    // observation await (deterministic on current_thread: the spawned
    // teardown cannot have run yet), then DROP it — a proven mid-flight
    // cancellation, not a race.
    {
        let fut = client.shutdown();
        tokio::pin!(fut);
        let first = futures::poll!(fut.as_mut());
        assert!(
            first.is_pending(),
            "first poll must park at the completion watch"
        );
    } // dropped here = cancelled
      // Second call must resume/observe the owned teardown and fully join.
    client.shutdown().await.expect("resumed shutdown");
    assert!(
        client.connect().await.is_err(),
        "stopped client must refuse connect"
    );
    drop(client);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while server.session_count().await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "session not released"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// serve() racing shutdown() cannot resurrect the server: with the flag gone,
/// listeners are cancellation-driven, and Running is only published from
/// Starting. Whatever the interleave, the end state is stopped + refusing.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_serve_and_shutdown_cannot_resurrect() {
    let addr = "127.0.0.1:28900";
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let serve_handle = {
        let s = server.clone();
        tokio::spawn(async move { s.serve().await })
    };
    // Race shutdown against serve's startup.
    let report = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    // serve() must terminate — no listener may keep running past shutdown.
    let _ = tokio::time::timeout(Duration::from_secs(3), serve_handle)
        .await
        .expect("serve() must terminate after concurrent shutdown");
    assert!(report.infra_stopped || server.live_session_tasks() == 0);
    // And nothing accepts anymore.
    let cfg = TcpClientConfig::new(addr).expect("cfg");
    let mut c = TransportClientBuilder::new()
        .protocol(cfg)
        .build()
        .await
        .expect("client");
    assert!(
        c.connect().await.is_err(),
        "no listener may survive the shutdown race"
    );
}

/// PERMANENT regression (previously failed for real): aborting serve() at
/// runtime detaches nothing — the infra is owned elsewhere — so a subsequent
/// shutdown is honest and the port is immediately rebindable.
#[tokio::test(flavor = "multi_thread")]
async fn aborting_serve_at_runtime_frees_ports_via_shutdown() {
    let addr = "127.0.0.1:28901";
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let serve_handle = {
        let s = server.clone();
        tokio::spawn(async move { s.serve().await })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    let client = connect(addr).await;
    assert!(echo_ok(&client).await);
    drop(client);

    serve_handle.abort();
    let _ = serve_handle.await;

    let report = server.shutdown_with_timeout(Duration::from_secs(10)).await;
    assert!(report.clean, "shutdown after serve abort: {:?}", report);
    tokio::net::TcpListener::bind(addr)
        .await
        .expect("port must be free after clean shutdown");
}

/// PERMANENT regression (previously failed for real): shutdown during
/// STARTUP cannot wedge the phase — the owned startup task (or its guard)
/// always drives Stopped, so shutdown reports infra_stopped and the port is
/// free even when serve() is cancelled/raced at its earliest moments.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_racing_startup_cannot_wedge_the_phase() {
    let addr = "127.0.0.1:28902";
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let serve_handle = {
        let s = server.clone();
        tokio::spawn(async move { s.serve().await })
    };
    // No sleep: race shutdown directly against startup (Phase A/B window).
    let report = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    assert!(
        report.infra_stopped,
        "startup must never wedge the phase: {:?}",
        report
    );
    let _ = tokio::time::timeout(Duration::from_secs(3), serve_handle)
        .await
        .expect("serve() must terminate");
    tokio::net::TcpListener::bind(addr)
        .await
        .expect("port must be free after the startup race");
}

/// PERMANENT regression (previously failed for real): a panicking handler
/// must not pin its session — the supervisor cancels the sibling and the
/// permit returns, all WITHOUT any shutdown.
struct PanicOnce;

#[async_trait]
impl SessionHandler for PanicOnce {
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {
        panic!("handler exploded on purpose");
    }

    async fn on_request(&self, _s: SessionId, _p: Packet, _r: Responder) {
        panic!("handler exploded on purpose");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn handler_panic_releases_session_and_permit() {
    let addr = "127.0.0.1:28903";
    const CAP: usize = 1;
    let server = TransportServerBuilder::new()
        .max_connections(CAP)
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(PanicOnce))
        .await
        .expect("server");
    let bg = server.clone();
    tokio::spawn(async move {
        let _ = bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let client = connect(addr).await;
    client.send(b"boom".as_slice()).await.expect("send");

    // The panic must cascade: session gone, permit back — no shutdown needed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.session_count().await != 0
        || server.available_permits() != CAP
        || server.live_session_tasks() != 0
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "panic pinned the session: sessions={} permits={} tasks={}",
            server.session_count().await,
            server.available_permits(),
            server.live_session_tasks()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(client);
    // The freed permit is genuinely usable: a new client gets served... or in
    // this fixture, at least admitted (the handler panics per message).
    let c2 = connect(addr).await;
    drop(c2);
}

use msgtrans::spi::{DynProtocolConfig, DynServerConfig};

/// Test-only protocol config: build_server_dyn signals it has ENTERED the
/// build, then pends forever — the deterministic Phase A window.
#[derive(Clone)]
struct GatedBuildConfig {
    entered: Arc<Notify>,
    entered_flag: Arc<AtomicBool>,
}

impl DynProtocolConfig for GatedBuildConfig {
    fn protocol_name(&self) -> &'static str {
        "gated-test"
    }
    fn validate_dyn(&self) -> Result<(), msgtrans::spi::ConfigError> {
        Ok(())
    }
}

impl DynServerConfig for GatedBuildConfig {
    fn build_server_dyn(
        &self,
        _limits: msgtrans::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn msgtrans::Server>, msgtrans::TransportError>,
                > + Send
                + '_,
        >,
    > {
        let entered = self.entered.clone();
        let flag = self.entered_flag.clone();
        Box::pin(async move {
            flag.store(true, Ordering::SeqCst);
            entered.notify_waiters();
            std::future::pending::<()>().await;
            unreachable!()
        })
    }
    fn get_bind_address(&self) -> std::net::SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }
    fn clone_server_dyn(&self) -> Box<dyn DynServerConfig> {
        Box::new(self.clone())
    }
}

/// DETERMINISTIC Phase A coverage (the shape that reproduced the original
/// bug): the build signals entry then pends; only after that signal do we
/// abort the serve() observer and shut down. The owned startup must be
/// interrupted by the cancellation and drive the phase to Stopped.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_interrupts_a_stuck_phase_a_build() {
    let entered = Arc::new(Notify::new());
    let entered_flag = Arc::new(AtomicBool::new(false));
    let server = TransportServerBuilder::new()
        .protocol(GatedBuildConfig {
            entered: entered.clone(),
            entered_flag: entered_flag.clone(),
        })
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let serve_handle = {
        let s = server.clone();
        tokio::spawn(async move { s.serve().await })
    };
    // Wait until we are PROVABLY inside Phase A.
    if !entered_flag.load(Ordering::SeqCst) {
        entered.notified().await;
    }
    // Cancel the observer mid-Phase-A: must detach nothing.
    serve_handle.abort();
    let _ = serve_handle.await;

    let report = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    assert!(
        report.infra_stopped,
        "a stuck Phase A build must be interrupted and finalized: {:?}",
        report
    );
    assert!(report.clean, "{:?}", report);
}

/// Test-only protocol config whose build PANICS: serve() must report the
/// failure, not Ok(()).
#[derive(Clone)]
struct PanicBuildConfig;

impl DynProtocolConfig for PanicBuildConfig {
    fn protocol_name(&self) -> &'static str {
        "panic-test"
    }
    fn validate_dyn(&self) -> Result<(), msgtrans::spi::ConfigError> {
        Ok(())
    }
}

impl DynServerConfig for PanicBuildConfig {
    fn build_server_dyn(
        &self,
        _limits: msgtrans::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn msgtrans::Server>, msgtrans::TransportError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move { panic!("build exploded on purpose") })
    }
    fn get_bind_address(&self) -> std::net::SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }
    fn clone_server_dyn(&self) -> Box<dyn DynServerConfig> {
        Box::new(self.clone())
    }
}

/// A panicking startup must surface as a serve() ERROR (previously it was
/// reported as Ok), and the phase must still reach Stopped.
#[tokio::test(flavor = "multi_thread")]
async fn startup_panic_is_reported_as_serve_error() {
    let server = TransportServerBuilder::new()
        .protocol(PanicBuildConfig)
        .build(Arc::new(Echo))
        .await
        .expect("server");
    let result = server.serve().await;
    assert!(result.is_err(), "a panicking startup must not report Ok");
    let report = server.shutdown_with_timeout(Duration::from_secs(5)).await;
    assert!(report.infra_stopped, "{:?}", report);
    assert!(report.clean, "{:?}", report);
}
