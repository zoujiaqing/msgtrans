//! The backbone's core claim, proven over a real socket: a slow handler
//! backpressures its connection instead of losing messages. Under the old
//! broadcast path this exact shape lost ~91% of messages at saturation
//! (measured in the A/B that motivated the rework).

use async_trait::async_trait;
use msgtrans::{
    packet::Packet,
    protocol::{TcpClientConfig, TcpServerConfig},
    transport::{SessionHandler, SessionSender, TransportClientBuilder, TransportServerBuilder},
    SessionId,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

struct SlowCounter {
    seen: Arc<AtomicU64>,
}

#[async_trait]
impl SessionHandler for SlowCounter {
    async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {
        // Slow enough that the sender vastly outpaces us: queues must fill.
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.seen.fetch_add(1, Ordering::Relaxed);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_handler_loses_nothing() {
    let _capacity = shrink_pipe(8192); // serialize with the saturation test
    const TOTAL: u64 = 300;
    let addr = "127.0.0.1:28876";
    let seen = Arc::new(AtomicU64::new(0));
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(SlowCounter { seen: seen.clone() }))
        .await
        .expect("server");
    let server_bg = server.clone();
    tokio::spawn(async move {
        let _ = server_bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("cfg"))
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");

    // Fire as fast as send() allows — far faster than 1ms/message service.
    for i in 0..TOTAL {
        client
            .send(format!("m{}", i).as_bytes())
            .await
            .expect("send");
    }

    // Every message must eventually be handled: backpressure, not loss.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while seen.load(Ordering::Relaxed) < TOTAL {
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {}/{} messages handled — backbone lost messages",
            seen.load(Ordering::Relaxed),
            TOTAL
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(seen.load(Ordering::Relaxed), TOTAL);
}

use tokio::sync::Notify;

/// The capacity override is process-global, so tests touching it serialize
/// through this lock and restore the default via guard (panic-safe).
static CAPACITY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct CapacityGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
impl Drop for CapacityGuard {
    fn drop(&mut self) {
        msgtrans::adapters::events::set_default_pipe_capacity(8192);
    }
}
fn shrink_pipe(capacity: usize) -> CapacityGuard {
    let guard = CAPACITY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    msgtrans::adapters::events::set_default_pipe_capacity(capacity);
    CapacityGuard(guard)
}

struct GatedCounter {
    seen: Arc<AtomicU64>,
    gate: Arc<Notify>,
    open: Arc<std::sync::atomic::AtomicBool>,
    last: Arc<AtomicU64>,
    ordered: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl SessionHandler for GatedCounter {
    async fn on_message(&self, _s: SessionId, p: Packet, _tx: SessionSender) {
        while !self.open.load(Ordering::SeqCst) {
            self.gate.notified().await;
        }
        // Order check: payload carries a monotonically increasing number.
        let n: u64 = String::from_utf8_lossy(&p.payload).parse().unwrap_or(0);
        let prev = self.last.swap(n, Ordering::SeqCst);
        if n != prev + 1 && !(prev == 0 && n == 1) {
            self.ordered.store(false, Ordering::SeqCst);
        }
        self.seen.fetch_add(1, Ordering::SeqCst);
    }
}

/// Saturation shape: pipe=8 (env), actor mailbox=4, handler gated shut. The
/// 100-message burst must fill mailbox+pipe and park the pump — and when the
/// gate opens, every message must arrive, in order. The earlier version of
/// this test sent 300 messages at default capacities (8192/2048/512) and
/// therefore saturated nothing.
#[tokio::test(flavor = "multi_thread")]
async fn saturated_queues_block_then_deliver_everything_in_order() {
    let _capacity = shrink_pipe(8);
    const TOTAL: u64 = 100;
    let addr = "127.0.0.1:28877";
    let seen = Arc::new(AtomicU64::new(0));
    let gate = Arc::new(Notify::new());
    let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ordered = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let handler = GatedCounter {
        seen: seen.clone(),
        gate: gate.clone(),
        open: open.clone(),
        last: Arc::new(AtomicU64::new(0)),
        ordered: ordered.clone(),
    };
    let server = TransportServerBuilder::new()
        .actor_buffer_size(4)
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(handler))
        .await
        .expect("server");
    let server_bg = server.clone();
    tokio::spawn(async move {
        let _ = server_bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("cfg"))
        .build()
        .await
        .expect("client");
    client.connect().await.expect("connect");

    for i in 1..=TOTAL {
        client
            .send(format!("{}", i).as_bytes())
            .await
            .expect("send");
    }
    // Gate shut: nothing may be processed; queues are saturated, not leaking.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        seen.load(Ordering::SeqCst),
        0,
        "gate is shut; any processed message means the gate logic is broken"
    );

    // Open the gate: everything must drain, in order.
    open.store(true, Ordering::SeqCst);
    gate.notify_waiters();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while seen.load(Ordering::SeqCst) < TOTAL {
        gate.notify_waiters(); // wake any handler parked before open flipped
        assert!(
            tokio::time::Instant::now() < deadline,
            "only {}/{} delivered after gate opened — messages were lost",
            seen.load(Ordering::SeqCst),
            TOTAL
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        ordered.load(Ordering::SeqCst),
        "delivery order was violated"
    );
}

/// The reconnect regression: one TransportClient must survive
/// connect -> disconnect -> connect and still deliver. The forwarding task
/// owns the Transport's take-once event receiver, so aborting it on
/// disconnect (the old behavior) stranded the receiver and broke every later
/// connect with "does not support event streams".
#[tokio::test(flavor = "multi_thread")]
async fn same_client_reconnects_and_still_delivers() {
    let _capacity = shrink_pipe(8192); // serialize with the saturation test
    let addr = "127.0.0.1:28878";
    let seen = Arc::new(AtomicU64::new(0));
    let server = TransportServerBuilder::new()
        .protocol(TcpServerConfig::new(addr).expect("cfg"))
        .build(Arc::new(SlowCounter { seen: seen.clone() }))
        .await
        .expect("server");
    let server_bg = server.clone();
    tokio::spawn(async move {
        let _ = server_bg.serve().await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(addr).expect("cfg"))
        .build()
        .await
        .expect("client");

    client.connect().await.expect("first connect");
    client.send(b"one".as_slice()).await.expect("send 1");
    client.disconnect().await.expect("disconnect");
    tokio::time::sleep(Duration::from_millis(200)).await;

    client.connect().await.expect("second connect must succeed");
    client
        .send(b"two".as_slice())
        .await
        .expect("send after reconnect");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while seen.load(Ordering::Relaxed) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "server saw {}/2 messages across the reconnect",
            seen.load(Ordering::Relaxed)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
