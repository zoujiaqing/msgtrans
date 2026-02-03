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
    protocol::WebSocketServerConfig, tokio, transport::TransportServerBuilder,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Minimal logging - only errors
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .init();

    println!("Load Test Server - High-performance Echo Server");
    println!("================================================");

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

    // Subscribe to events BEFORE serve() to ensure we don't miss any
    let mut events = transport.subscribe_events();

    // Single dispatcher - handles echo directly without intermediate queue
    // This avoids the overhead of task spawning and channel communication
    let transport_clone = transport.clone();
    let dispatcher_handle = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                ServerEvent::MessageReceived {
                    session_id,
                    context,
                } => {
                    // Clone data first before consuming context
                    let data = context.data.clone();
                    if context.is_request() {
                        // Direct respond for requests - this is synchronous and fast
                        context.respond(data);
                    } else {
                        // For one-way messages, spawn a task to avoid blocking the event loop
                        let transport = transport_clone.clone();
                        tokio::spawn(async move {
                            let _ = transport.send(session_id, &data).await;
                        });
                    }
                }
                _ => {}
            }
        }
    });

    println!("Server running... Press Ctrl+C to stop.");

    let server_result = transport_for_serve.serve().await;

    // Cleanup
    dispatcher_handle.abort();

    if let Err(e) = server_result {
        eprintln!("Server error: {:?}", e);
    }

    Ok(())
}
