//! Load test server - high-performance echo server for benchmarking
//!
//! Usage:
//!   cargo run --example load_test_server --release
//!
//! This is a minimal echo server optimized for maximum throughput.
//! It removes all println! statements and unnecessary allocations.
//! Use with load_test client for benchmarking.

use msgtrans::{
    event::ServerEvent, protocol::QuicServerConfig, protocol::TcpServerConfig,
    protocol::WebSocketServerConfig, tokio, transport::TransportServerBuilder, SessionId,
};
use std::sync::Arc;
use tokio::sync::mpsc;

const NUM_WORKERS: usize = 8;
const WORKER_QUEUE_SIZE: usize = 10000;

// Task for worker to process
struct EchoTask {
    session_id: SessionId,
    data: Vec<u8>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Minimal logging - only errors
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .init();

    println!("Load Test Server - High-performance Echo Server");
    println!("================================================");
    println!("Workers: {}", NUM_WORKERS);

    let tcp_config = TcpServerConfig::new("127.0.0.1:8001")?;
    let websocket_config = WebSocketServerConfig::new("127.0.0.1:8002")?;
    let quic_config = QuicServerConfig::new("127.0.0.1:8003")?;

    let transport = TransportServerBuilder::new()
        .max_connections(10000)
        .with_protocol(tcp_config)
        .with_protocol(websocket_config)
        .with_protocol(quic_config)
        .build()
        .await?;

    println!("Listening on:");
    println!("  TCP:       127.0.0.1:8001");
    println!("  WebSocket: 127.0.0.1:8002");
    println!("  QUIC:      127.0.0.1:8003");
    println!();

    let transport = Arc::new(transport);
    let transport_for_serve = transport.clone();

    // Create work distribution channels - one sender, multiple receivers via mpsc
    // Using mpsc with multiple worker tasks that share a single receiver
    let (task_tx, task_rx) = mpsc::channel::<EchoTask>(WORKER_QUEUE_SIZE);
    let task_rx = Arc::new(tokio::sync::Mutex::new(task_rx));

    // Spawn worker tasks - each worker competes for tasks from the shared queue
    let mut worker_handles = Vec::new();
    for _ in 0..NUM_WORKERS {
        let transport_clone = transport.clone();
        let rx = task_rx.clone();

        let handle = tokio::spawn(async move {
            loop {
                let task = {
                    let mut rx_guard = rx.lock().await;
                    rx_guard.recv().await
                };

                match task {
                    Some(task) => {
                        let _ = transport_clone.send(task.session_id, &task.data).await;
                    }
                    None => break, // Channel closed
                }
            }
        });
        worker_handles.push(handle);
    }

    // Single event dispatcher - distributes work to workers
    let mut events = transport.subscribe_events();
    let dispatcher_handle = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                ServerEvent::MessageReceived {
                    session_id,
                    context,
                } => {
                    let data = context.data.clone();
                    if context.is_request() {
                        // Direct respond for requests
                        context.respond(data);
                    } else {
                        // Send to worker queue - workers compete for tasks
                        let _ = task_tx.send(EchoTask { session_id, data }).await;
                    }
                }
                _ => {}
            }
        }
    });

    println!(
        "Server running with {} workers... Press Ctrl+C to stop.",
        NUM_WORKERS
    );

    let server_result = transport_for_serve.serve().await;

    // Cleanup
    dispatcher_handle.abort();
    for handle in worker_handles {
        handle.abort();
    }

    if let Err(e) = server_result {
        eprintln!("Server error: {:?}", e);
    }

    Ok(())
}
