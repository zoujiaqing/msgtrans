//! Load testing tool for msgtrans
//!
//! Usage:
//!   cargo run --example load_test -- --protocol tcp --connections 100 --duration 30
//!
//! Options:
//!   --protocol, -p    Protocol to test: tcp, websocket, quic (default: tcp)
//!   --connections, -c Number of concurrent connections (default: 10)
//!   --duration, -d    Test duration in seconds (default: 10)
//!   --message-size, -s Message size in bytes (default: 64)
//!   --interval, -i    Interval between messages in milliseconds (default: 10)
//!   --host, -h        Server host (default: 127.0.0.1)
//!   --port            Server port (default: 8001 for tcp, 8002 for websocket, 8003 for quic)

use msgtrans::{
    event::ClientEvent,
    protocol::{QuicClientConfig, TcpClientConfig, WebSocketClientConfig},
    transport::client::TransportClientBuilder,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Barrier;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Protocol {
    Tcp,
    WebSocket,
    Quic,
}

impl Protocol {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "tcp" => Some(Protocol::Tcp),
            "websocket" | "ws" => Some(Protocol::WebSocket),
            "quic" => Some(Protocol::Quic),
            _ => None,
        }
    }

    fn default_port(&self) -> u16 {
        match self {
            Protocol::Tcp => 8001,
            Protocol::WebSocket => 8002,
            Protocol::Quic => 8003,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::WebSocket => "WebSocket",
            Protocol::Quic => "QUIC",
        }
    }
}

struct Config {
    protocol: Protocol,
    connections: usize,
    duration_secs: u64,
    message_size: usize,
    interval_ms: u64,
    host: String,
    port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            protocol: Protocol::Tcp,
            connections: 10,
            duration_secs: 10,
            message_size: 64,
            interval_ms: 10,
            host: "127.0.0.1".to_string(),
            port: 8001,
        }
    }
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let mut config = Config::default();
    let mut port_specified = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--protocol" | "-p" => {
                if i + 1 < args.len() {
                    if let Some(p) = Protocol::from_str(&args[i + 1]) {
                        config.protocol = p;
                    } else {
                        eprintln!(
                            "Invalid protocol: {}. Use tcp, websocket, or quic.",
                            args[i + 1]
                        );
                        std::process::exit(1);
                    }
                    i += 1;
                }
            }
            "--connections" | "-c" => {
                if i + 1 < args.len() {
                    config.connections = args[i + 1].parse().unwrap_or(10);
                    i += 1;
                }
            }
            "--duration" | "-d" => {
                if i + 1 < args.len() {
                    config.duration_secs = args[i + 1].parse().unwrap_or(10);
                    i += 1;
                }
            }
            "--message-size" | "-s" => {
                if i + 1 < args.len() {
                    config.message_size = args[i + 1].parse().unwrap_or(64);
                    i += 1;
                }
            }
            "--interval" | "-i" => {
                if i + 1 < args.len() {
                    config.interval_ms = args[i + 1].parse().unwrap_or(10);
                    i += 1;
                }
            }
            "--host" => {
                if i + 1 < args.len() {
                    config.host = args[i + 1].clone();
                    i += 1;
                }
            }
            "--port" => {
                if i + 1 < args.len() {
                    config.port = args[i + 1].parse().unwrap_or(8001);
                    port_specified = true;
                    i += 1;
                }
            }
            "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }

    // Use protocol-specific default port if not specified
    if !port_specified {
        config.port = config.protocol.default_port();
    }

    config
}

fn print_help() {
    println!(
        r#"
msgtrans Load Testing Tool

Usage:
  cargo run --example load_test -- [OPTIONS]

Options:
  -p, --protocol <PROTOCOL>      Protocol to test: tcp, websocket, quic [default: tcp]
  -c, --connections <NUM>        Number of concurrent connections [default: 10]
  -d, --duration <SECONDS>       Test duration in seconds [default: 10]
  -s, --message-size <BYTES>     Message size in bytes [default: 64]
  -i, --interval <MS>            Interval between messages in ms [default: 10]
      --host <HOST>              Server host [default: 127.0.0.1]
      --port <PORT>              Server port [default: 8001/8002/8003 based on protocol]
      --help                     Print this help message

Examples:
  # Test TCP with 100 connections for 30 seconds
  cargo run --example load_test -- -p tcp -c 100 -d 30

  # Test WebSocket with 50 connections
  cargo run --example load_test -- -p websocket -c 50 -d 20

  # Test QUIC with custom message size
  cargo run --example load_test -- -p quic -c 100 -d 30 -s 1024

  # Test with slower send rate (100ms interval)
  cargo run --example load_test -- -p tcp -c 10 -d 10 -i 100

Note: Make sure echo_server is running before starting the load test.
"#
    );
}

// Statistics collector
struct Stats {
    messages_sent: AtomicU64,
    messages_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    errors: AtomicU64,
    connections_established: AtomicU64,
    connection_failures: AtomicU64,
    latency_sum_us: AtomicU64,
    latency_count: AtomicU64,
}

impl Stats {
    fn new() -> Self {
        Self {
            messages_sent: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            connections_established: AtomicU64::new(0),
            connection_failures: AtomicU64::new(0),
            latency_sum_us: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
        }
    }

    fn record_send(&self, bytes: u64) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    fn record_receive(&self, bytes: u64) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
    }

    fn record_latency(&self, latency_us: u64) {
        self.latency_sum_us.fetch_add(latency_us, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
    }

    fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    fn record_connection(&self) {
        self.connections_established.fetch_add(1, Ordering::Relaxed);
    }

    fn record_connection_failure(&self) {
        self.connection_failures.fetch_add(1, Ordering::Relaxed);
    }

    fn print_report(&self, duration: Duration) {
        let secs = duration.as_secs_f64();
        let sent = self.messages_sent.load(Ordering::Relaxed);
        let received = self.messages_received.load(Ordering::Relaxed);
        let bytes_sent = self.bytes_sent.load(Ordering::Relaxed);
        let bytes_received = self.bytes_received.load(Ordering::Relaxed);
        let errors = self.errors.load(Ordering::Relaxed);
        let connections = self.connections_established.load(Ordering::Relaxed);
        let conn_failures = self.connection_failures.load(Ordering::Relaxed);
        let latency_sum = self.latency_sum_us.load(Ordering::Relaxed);
        let latency_count = self.latency_count.load(Ordering::Relaxed);

        let avg_latency_us = if latency_count > 0 {
            latency_sum / latency_count
        } else {
            0
        };

        println!();
        println!("============================================================");
        println!("                    LOAD TEST RESULTS                       ");
        println!("============================================================");
        println!();
        println!("Duration:              {:.2} seconds", secs);
        println!();
        println!("Connections:");
        println!("  Established:         {}", connections);
        println!("  Failed:              {}", conn_failures);
        println!();
        println!("Messages:");
        println!("  Sent:                {}", sent);
        println!("  Received:            {}", received);
        println!("  Errors:              {}", errors);
        println!();
        println!("Throughput:");
        println!("  Messages/sec (TX):   {:.2}", sent as f64 / secs);
        println!("  Messages/sec (RX):   {:.2}", received as f64 / secs);
        println!(
            "  Bytes/sec (TX):      {:.2} KB/s",
            bytes_sent as f64 / secs / 1024.0
        );
        println!(
            "  Bytes/sec (RX):      {:.2} KB/s",
            bytes_received as f64 / secs / 1024.0
        );
        println!();
        println!("Data Transfer:");
        println!(
            "  Total Sent:          {:.2} KB",
            bytes_sent as f64 / 1024.0
        );
        println!(
            "  Total Received:      {:.2} KB",
            bytes_received as f64 / 1024.0
        );
        println!();
        println!("Latency:");
        println!(
            "  Average:             {:.2} ms",
            avg_latency_us as f64 / 1000.0
        );
        println!();
        println!("============================================================");
    }
}

async fn run_client(
    client_id: usize,
    config: &Config,
    stats: Arc<Stats>,
    running: Arc<AtomicBool>,
    barrier: Arc<Barrier>,
    message_payload: Arc<Vec<u8>>,
) {
    let addr = format!("{}:{}", config.host, config.port);

    // Build transport based on protocol
    let transport_result = match config.protocol {
        Protocol::Tcp => {
            let tcp_config = match TcpClientConfig::new(&addr) {
                Ok(c) => c
                    .with_connect_timeout(Duration::from_secs(5))
                    .with_nodelay(true),
                Err(e) => {
                    tracing::error!(
                        "[Client {}] Failed to create TCP config: {:?}",
                        client_id,
                        e
                    );
                    stats.record_connection_failure();
                    return;
                }
            };
            TransportClientBuilder::new()
                .with_protocol(tcp_config)
                .connect_timeout(Duration::from_secs(10))
                .build()
                .await
        }
        Protocol::WebSocket => {
            let ws_addr = format!("ws://{}", addr);
            let ws_config = match WebSocketClientConfig::new(&ws_addr) {
                Ok(c) => c.with_connect_timeout(Duration::from_secs(5)),
                Err(e) => {
                    tracing::error!(
                        "[Client {}] Failed to create WebSocket config: {:?}",
                        client_id,
                        e
                    );
                    stats.record_connection_failure();
                    return;
                }
            };
            TransportClientBuilder::new()
                .with_protocol(ws_config)
                .connect_timeout(Duration::from_secs(10))
                .build()
                .await
        }
        Protocol::Quic => {
            let quic_config = match QuicClientConfig::new(&addr) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(
                        "[Client {}] Failed to create QUIC config: {:?}",
                        client_id,
                        e
                    );
                    stats.record_connection_failure();
                    return;
                }
            };
            TransportClientBuilder::new()
                .with_protocol(quic_config)
                .connect_timeout(Duration::from_secs(10))
                .build()
                .await
        }
    };

    let mut transport = match transport_result {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("[Client {}] Failed to build transport: {:?}", client_id, e);
            stats.record_connection_failure();
            return;
        }
    };

    // Connect
    if let Err(e) = transport.connect().await {
        tracing::debug!("[Client {}] Connection failed: {:?}", client_id, e);
        stats.record_connection_failure();
        return;
    }

    stats.record_connection();
    tracing::debug!("[Client {}] Connected", client_id);

    // Subscribe to events
    let mut events = transport.subscribe_events();
    let stats_clone = stats.clone();
    let running_clone = running.clone();

    // Event handler task - high priority receiver
    let event_task = tokio::spawn(async move {
        loop {
            if !running_clone.load(Ordering::Relaxed) {
                break;
            }

            // Use select! to handle events without timeout blocking
            match events.recv().await {
                Ok(event) => match event {
                    ClientEvent::MessageReceived(context) => {
                        stats_clone.record_receive(context.data.len() as u64);
                    }
                    ClientEvent::Disconnected { .. } => break,
                    ClientEvent::Error { .. } => {
                        stats_clone.record_error();
                        break;
                    }
                    _ => {}
                },
                Err(_) => break,
            }
        }
    });

    // Wait for all clients to be ready
    barrier.wait().await;

    // Send messages continuously until stopped
    let payload = message_payload.as_ref();
    let interval = Duration::from_millis(config.interval_ms);

    while running.load(Ordering::Relaxed) {
        let start = Instant::now();
        match transport.send(payload).await {
            Ok(_) => {
                stats.record_send(payload.len() as u64);
                let latency = start.elapsed().as_micros() as u64;
                stats.record_latency(latency);
            }
            Err(_) => {
                stats.record_error();
                break;
            }
        }

        // Wait for interval before sending next message
        // Use sleep even for 0ms to allow other tasks to run
        if config.interval_ms > 0 {
            tokio::time::sleep(interval).await;
        } else {
            // Yield to allow event_task to process received messages
            tokio::time::sleep(Duration::from_micros(1)).await;
        }
    }

    // Cleanup
    let _ = transport.disconnect().await;
    event_task.abort();
    tracing::debug!("[Client {}] Disconnected", client_id);
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging - use ERROR level to reduce noise during load testing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_target(false)
        .init();

    let config = parse_args();

    println!();
    println!("============================================================");
    println!("              msgtrans Load Testing Tool                    ");
    println!("============================================================");
    println!();
    println!("Configuration:");
    println!("  Protocol:            {}", config.protocol.name());
    println!("  Server:              {}:{}", config.host, config.port);
    println!("  Concurrent clients:  {}", config.connections);
    println!("  Duration:            {} seconds", config.duration_secs);
    println!("  Message size:        {} bytes", config.message_size);
    println!("  Send interval:       {} ms", config.interval_ms);
    println!();

    // Create shared state
    let stats = Arc::new(Stats::new());
    let running = Arc::new(AtomicBool::new(true));
    let barrier = Arc::new(Barrier::new(config.connections));

    // Generate message payload
    let message_payload = Arc::new(vec![b'X'; config.message_size]);

    println!("Starting {} clients...", config.connections);
    println!();

    let start_time = Instant::now();

    // Spawn client tasks
    let mut handles = Vec::new();
    for i in 0..config.connections {
        let stats_clone = stats.clone();
        let running_clone = running.clone();
        let barrier_clone = barrier.clone();
        let payload_clone = message_payload.clone();

        // Clone config values needed in the task
        let protocol = config.protocol;
        let host = config.host.clone();
        let port = config.port;
        let message_size = config.message_size;
        let interval_ms = config.interval_ms;

        let handle = tokio::spawn(async move {
            let task_config = Config {
                protocol,
                connections: 1,
                duration_secs: 0,
                message_size,
                interval_ms,
                host,
                port,
            };
            run_client(
                i,
                &task_config,
                stats_clone,
                running_clone,
                barrier_clone,
                payload_clone,
            )
            .await;
        });
        handles.push(handle);
    }

    // Progress reporting task
    let stats_progress = stats.clone();
    let running_progress = running.clone();
    let progress_task = tokio::spawn(async move {
        let mut last_sent = 0u64;
        let mut last_received = 0u64;

        while running_progress.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(1)).await;

            let current_sent = stats_progress.messages_sent.load(Ordering::Relaxed);
            let current_received = stats_progress.messages_received.load(Ordering::Relaxed);
            let connections = stats_progress
                .connections_established
                .load(Ordering::Relaxed);
            let errors = stats_progress.errors.load(Ordering::Relaxed);

            let sent_rate = current_sent - last_sent;
            let recv_rate = current_received - last_received;

            println!(
                "[Progress] Connections: {} | TX: {} msg/s | RX: {} msg/s | Errors: {}",
                connections, sent_rate, recv_rate, errors
            );

            last_sent = current_sent;
            last_received = current_received;
        }
    });

    // Wait for test duration
    tokio::time::sleep(Duration::from_secs(config.duration_secs)).await;

    // Signal stop
    println!();
    println!("Stopping test...");
    running.store(false, Ordering::Relaxed);

    // Wait for clients to finish
    for handle in handles {
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    progress_task.abort();

    let total_duration = start_time.elapsed();

    // Print results
    stats.print_report(total_duration);

    Ok(())
}
