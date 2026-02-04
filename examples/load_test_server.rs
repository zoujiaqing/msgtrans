//! Load test server - high-performance echo server for benchmarking
//!
//! Usage:
//!   cargo run --example load_test_server --release
//!
//! This is an echo server using the Actor model for maximum throughput.
//! Each connection gets its own dedicated queue and worker task, eliminating
//! global contention and enabling natural backpressure.
//!
//! Architecture:
//!   Connection → mpsc(bounded) → SessionActor → EchoHandler
//!
//! Use with load_test client for benchmarking.

use async_trait::async_trait;
use msgtrans::{
    packet::Packet,
    protocol::QuicServerConfig,
    protocol::TcpServerConfig,
    protocol::WebSocketServerConfig,
    tokio,
    transport::{SessionHandler, SessionSender, TransportServerBuilder},
    SessionId,
};
use std::sync::Arc;

/// Echo handler - implements SessionHandler trait
///
/// This is a minimal echo handler that directly echoes back received messages.
/// No broadcast channel, no global contention, just pure per-connection processing.
struct EchoHandler;

#[async_trait]
impl SessionHandler for EchoHandler {
    async fn on_message(&self, _session_id: SessionId, packet: Packet, sender: SessionSender) {
        // Echo the payload back using the sender
        let _ = sender.send_data(packet.payload).await;
    }

    async fn on_connected(&self, session_id: SessionId) {
        tracing::debug!("[CONNECT] Session {} connected", session_id);
    }

    async fn on_disconnected(&self, session_id: SessionId, _reason: msgtrans::error::CloseReason) {
        tracing::debug!("[DISCONNECT] Session {} disconnected", session_id);
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Minimal logging - only errors
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .init();

    println!("Load Test Server - High-performance Echo Server");
    println!("================================================");
    println!();
    println!("Architecture: Connection -> mpsc(4096) -> SessionActor -> Handler");
    println!();

    let tcp_config = TcpServerConfig::new("127.0.0.1:8001")?;
    let websocket_config = WebSocketServerConfig::new("127.0.0.1:8002")?;
    let quic_config = QuicServerConfig::new("127.0.0.1:8003")?;

    // Create echo handler
    let handler = Arc::new(EchoHandler);

    // Build server with actor mode
    let transport = TransportServerBuilder::new()
        .max_connections(10000)
        .with_handler(handler) // Enable actor mode
        .actor_buffer_size(4096) // Buffer size per connection
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
    println!("Mode: Actor (per-connection mpsc)");
    println!("Server running... Press Ctrl+C to stop.");

    // In actor mode, we don't need to subscribe to events or spawn a dispatcher
    // Each connection has its own actor that calls our handler directly
    transport.serve().await?;

    Ok(())
}
