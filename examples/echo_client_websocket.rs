use msgtrans::{
    event::ClientEvent, protocol::WebSocketClientConfig, transport::client::TransportClientBuilder,
};
/// WebSocket Echo client - simplified API demonstration
/// [TARGET] Demonstrates simplified byte API, hiding all Packet complexity
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable verbose logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!("[START] Starting WebSocket Echo client (simplified API - byte-only version)");

    // [TARGET] Configure WebSocket client - simplified API
    let websocket_config = WebSocketClientConfig::new("ws://127.0.0.1:8002")?
        .with_connect_timeout(Duration::from_secs(10))
        .with_ping_interval(Some(Duration::from_secs(30)))
        .with_pong_timeout(Duration::from_secs(10))
        .with_max_frame_size(8192)
        .with_max_message_size(65536)
        .with_verify_tls(false); // Test environment

    // [TARGET] Build TransportClient
    let mut transport = TransportClientBuilder::new()
        .with_protocol(websocket_config)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .await?;

    tracing::info!("[CONNECT] Connecting to WebSocket server...");
    transport.connect().await?;
    tracing::info!("[SUCCESS] Connection successful!");

    tracing::info!("[SEND] Sending messages...");
    // [TARGET] Use simplified byte API
    let result = transport.send(b"Hello from WebSocket client!").await?;
    tracing::info!(
        "[SUCCESS] Message sent successfully (ID: {})",
        result.message_id
    );

    let result = transport.send(b"Binary data from client").await?;
    tracing::info!(
        "[SUCCESS] Message sent successfully (ID: {})",
        result.message_id
    );

    tracing::info!("[START] Starting to listen for events...");
    let mut events = transport.subscribe_events();

    // [TARGET] Process events in parallel to avoid blocking
    let event_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                ClientEvent::Connected { info } => {
                    tracing::info!(
                        "[CONNECT] Connection established: {} ↔ {}",
                        info.local_addr,
                        info.peer_addr
                    );
                }
                ClientEvent::Disconnected { reason } => {
                    tracing::info!("[CLOSE] Connection closed: {:?}", reason);
                    break;
                }
                ClientEvent::MessageReceived(context) => {
                    let content = String::from_utf8_lossy(&context.data);
                    tracing::info!(
                        "[RECV] Message received (ID: {}): {}",
                        context.message_id,
                        content
                    );

                    // If it's a request, respond
                    if context.is_request() {
                        let message_id = context.message_id;
                        tracing::info!("[SEND] Responding to server request...");
                        context.respond(b"Hello from WebSocket client response!".to_vec());
                        tracing::info!("[SUCCESS] Server request responded (ID: {})", message_id);
                    }
                }
                ClientEvent::MessageSent { message_id } => {
                    tracing::info!("[SUCCESS] Message sent successfully (ID: {})", message_id);
                }
                ClientEvent::Error { error } => {
                    tracing::error!("[ERROR] Transport error: {:?}", error);
                    break;
                }
            }
        }
    });

    // Wait 100ms for connection to stabilize
    tokio::time::sleep(Duration::from_millis(100)).await;

    tracing::info!("[REQUEST] Sending request...");
    // [TARGET] Simplified request API
    match transport.request(b"What time is it?").await {
        Ok(result) => {
            if let Some(response_data) = result.data {
                let content = String::from_utf8_lossy(&response_data);
                tracing::info!(
                    "[RECV] Response received (ID: {}): {}",
                    result.message_id,
                    content
                );
            } else {
                tracing::warn!(
                    "[WARN] Request response data is empty (ID: {})",
                    result.message_id
                );
            }
        }
        Err(e) => {
            tracing::error!("[ERROR] Request failed: {:?}", e);
        }
    }

    match transport.request(b"Binary request").await {
        Ok(result) => {
            if let Some(response_data) = result.data {
                tracing::info!(
                    "[RECV] Binary response received (ID: {}): {} bytes",
                    result.message_id,
                    response_data.len()
                );
                let content = String::from_utf8_lossy(&response_data);
                tracing::info!("   Content: {}", content);
            } else {
                tracing::warn!(
                    "[WARN] Binary request response data is empty (ID: {})",
                    result.message_id
                );
            }
        }
        Err(e) => {
            tracing::error!("[ERROR] Binary request failed: {:?}", e);
        }
    }

    tracing::info!("[WAIT] Waiting for event processing to complete...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    tracing::info!("[CLOSE] Disconnecting...");
    transport.disconnect().await?;

    // Wait for event task to end
    let _ = tokio::time::timeout(Duration::from_secs(1), event_task).await;

    tracing::info!("[STOP] WebSocket Echo client example completed");
    Ok(())
}
