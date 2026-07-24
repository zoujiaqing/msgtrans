use async_trait::async_trait;
use msgtrans::{
    command::ConnectionInfo,
    event::ClientEvent,
    packet::Packet,
    protocol::{TcpClientConfig, TcpServerConfig},
    transport::{
        SessionHandler, SessionSender, TransportClientBuilder, TransportServer,
        TransportServerBuilder,
    },
    SessionId,
};
/// Specialized program for debugging transport event streams
///
/// Used to diagnose event bridging and server-side session handling
use std::sync::{Arc, OnceLock};
use std::time::Duration;

struct DebugHandler {
    /// Filled in after the server is built, so the handler can push a message
    /// as soon as a session connects.
    server: OnceLock<TransportServer>,
}

#[async_trait]
impl SessionHandler for DebugHandler {
    async fn on_request(
        &self,
        session_id: SessionId,
        request: Packet,
        responder: msgtrans::transport::Responder,
    ) {
        println!(
            "[REQUEST] session={} id={} ({} bytes)",
            session_id,
            request.message_id(),
            request.payload().len()
        );
        let reply = format!("Echo: {}", String::from_utf8_lossy(request.payload()));
        let _ = responder.respond(reply.into_bytes()).await;
    }

    async fn on_connected(&self, session_id: SessionId, _info: ConnectionInfo) {
        println!("[CONNECT] New connection established: {}", session_id);

        let Some(server) = self.server.get().cloned() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(e) = server.send(session_id, b"Hello from server!").await {
                println!("[ERROR] Server send failed: {:?}", e);
            } else {
                println!("[SUCCESS] Server send successful");
            }
        });
    }

    async fn on_message(&self, session_id: SessionId, packet: Packet, _sender: SessionSender) {
        let text = String::from_utf8_lossy(packet.payload()).to_string();
        println!(
            "[RECV] Message received (session: {}, ID: {}): {}",
            session_id,
            packet.message_id(),
            text
        );
    }

    async fn on_disconnected(&self, session_id: SessionId, reason: msgtrans::CloseReason) {
        println!(
            "[CLOSE] Connection closed: session {}, reason: {:?}",
            session_id, reason
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable most verbose logging to observe all event flows
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE) // [DEBUG] Most verbose log level
        .init();

    println!("[DEBUG] Transport event stream debug program");
    println!("====================");
    println!("Observe event bridging and server-side event handling for lock-free connections");
    println!();

    // Start server - simplified API
    let tcp_config = TcpServerConfig::new("127.0.0.1:9001")?;

    let handler = Arc::new(DebugHandler {
        server: OnceLock::new(),
    });

    let server = TransportServerBuilder::new()
        .protocol(tcp_config)
        .build(handler.clone())
        .await?;

    println!("[SUCCESS] Server created: 127.0.0.1:9001");

    let _ = handler.server.set(server.clone());

    // Start server
    let server_handle = tokio::spawn(async move {
        if let Err(e) = server.serve().await {
            println!("[ERROR] Server error: {:?}", e);
        }
    });

    // Wait for server startup
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("[START] Server started, now starting client");

    // Start client - simplified API
    let client_config = TcpClientConfig::new("127.0.0.1:9001")?;
    let mut client = TransportClientBuilder::new()
        .protocol(client_config)
        .build()
        .await?;

    let mut client_events = client.events().await?;

    // Client event handling
    let client_task = tokio::spawn(async move {
        println!("[START] Client event handling started");
        let mut event_count = 0;

        while let Some(event) = client_events.next().await {
            event_count += 1;
            println!("[RECV] Client event #{}: {:?}", event_count, event);

            match event {
                ClientEvent::Connected { .. } => {
                    println!("[CONNECT] Client connected successfully");
                }
                ClientEvent::MessageReceived(context) => {
                    println!(
                        "[RECV] Client received message (ID: {}): {}",
                        context.message_id,
                        context.as_text_lossy()
                    );

                    if context.is_request() {
                        println!("[SEND] Responding to server request...");
                        context.respond_detached(b"Client response!".to_vec());
                        println!("[SUCCESS] Server request responded");
                    }
                }
                ClientEvent::MessageSent { message_id } => {
                    println!("[SEND] Client message send confirmation: ID {}", message_id);
                }
                ClientEvent::Disconnected { .. } => {
                    println!("[CLOSE] Client disconnected");
                    break;
                }
                _ => {
                    println!("[INFO] Client other event: {:?}", event);
                }
            }
        }

        println!(
            "[WARN] Client event handling ended (processed {} events)",
            event_count
        );
    });

    // Connect to server
    println!("[CONNECT] Client connecting...");
    client.connect().await?;
    println!("[SUCCESS] Client connected");

    // Wait for connection to stabilize
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send one-way message
    println!("[SEND] Sending one-way message...");
    let result = client.send(b"Hello Server!").await?;
    println!("[SUCCESS] One-way message sent (ID: {})", result.message_id);

    // Wait a moment
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send request
    println!("[REQUEST] Sending request...");
    let response = client.request(b"What time is it?").await?;
    if let Some(data) = response.data {
        println!(
            "[SUCCESS] Received response: {}",
            String::from_utf8_lossy(&data)
        );
    } else {
        println!("[WARN] Request timeout or no response");
    }

    // Wait for event processing
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Disconnect
    println!("[CLOSE] Disconnecting...");
    client.disconnect().await?;

    // Wait for tasks to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Cleanup
    server_handle.abort();
    let _ = client_task.await;

    println!("[STOP] Debug program completed");
    Ok(())
}
