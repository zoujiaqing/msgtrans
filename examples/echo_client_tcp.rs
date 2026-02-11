//! TCP Echo client example
//!
//! [TARGET] Demonstrates simplified API: byte-only version, users handle string conversion

use msgtrans::{
    event::ClientEvent, protocol::TcpClientConfig, transport::client::TransportClientBuilder,
};
use std::time::Duration;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging - enable DEBUG level to debug event forwarding
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    info!("[START] Starting TCP Echo client (simplified API - byte-only version)");

    // Create TCP configuration - simplified API
    let tcp_config = TcpClientConfig::new("127.0.0.1:8001")?
        .with_connect_timeout(Duration::from_secs(5))
        .with_nodelay(true);

    // [TARGET] Build client using new TransportClientBuilder
    let mut transport = TransportClientBuilder::new()
        .with_protocol(tcp_config)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .await?;

    // Connect to server
    info!("[CONNECT] Connecting to TCP server...");
    match transport.connect().await {
        Ok(()) => {
            info!("[SUCCESS] Connection successful!");
        }
        Err(e) => {
            error!("[ERROR] Connection failed: {:?}", e);
            return Err(e.into());
        }
    }

    // Get event stream
    let mut event_stream = transport.subscribe_events();

    // [ASYNC] Critical fix: start event handling task to run in parallel with sending
    let event_handle = tokio::spawn(async move {
        // [TARGET] Handle simplified events (no Packet involvement)
        info!("[START] Starting to listen for events...");
        let mut event_count = 0;

        while let Ok(event) = event_stream.recv().await {
            event_count += 1;

            match event {
                ClientEvent::Connected { info } => {
                    info!("[CONNECT] Connection event: {:?}", info);
                }

                ClientEvent::MessageReceived(context) => {
                    // [TARGET] Unified context handles all message types
                    info!(
                        "[RECV] Message received (ID: {}): {}",
                        context.message_id,
                        context.as_text_lossy()
                    );

                    // If it's a request, respond
                    if context.is_request() {
                        let message_id = context.message_id;
                        info!("[SEND] Responding to server request...");
                        context.respond(b"Hello from client response!".to_vec());
                        info!("[SUCCESS] Server request responded (ID: {})", message_id);
                    }
                }

                ClientEvent::MessageSent { message_id } => {
                    info!("[SUCCESS] Message sent successfully (ID: {})", message_id);
                }

                ClientEvent::Disconnected { reason } => {
                    warn!("[CLOSE] Connection disconnected: {:?}", reason);
                    break;
                }

                ClientEvent::Error { error } => {
                    error!("[ERROR] Transport error: {:?}", error);
                    break;
                }
            }

            // Limit event processing count to avoid infinite loop
            if event_count >= 15 {
                info!("[STOP] Processed {} events, ending listening", event_count);
                break;
            }
        }
    });

    // [TARGET] Demonstrate unified send API - returns TransportResult
    info!("[SEND] Sending messages...");
    let result1 = transport.send("Hello from TCP client!".as_bytes()).await?;
    info!(
        "[SUCCESS] Message sent successfully (ID: {})",
        result1.message_id
    );

    let result2 = transport.send(b"Binary data from client").await?;
    info!(
        "[SUCCESS] Message sent successfully (ID: {})",
        result2.message_id
    );

    // Give event processing some time
    tokio::time::sleep(Duration::from_millis(100)).await;

    // [TARGET] Demonstrate unified request API - returns TransportResult
    info!("[REQUEST] Sending request...");
    match transport.request("What time is it?".as_bytes()).await {
        Ok(result) => {
            if let Some(response_data) = &result.data {
                let response_text = String::from_utf8_lossy(response_data);
                info!(
                    "[RECV] Response received (ID: {}): {}",
                    result.message_id, response_text
                );
            } else {
                warn!(
                    "[WARN] Request result has no data (ID: {})",
                    result.message_id
                );
            }
        }
        Err(e) => {
            warn!("[WARN] Request failed: {:?}", e);
        }
    }

    match transport.request(b"Binary request").await {
        Ok(result) => {
            if let Some(response) = &result.data {
                info!(
                    "[RECV] Binary response received (ID: {}): {} bytes",
                    result.message_id,
                    response.len()
                );
                if let Ok(text) = String::from_utf8(response.clone()) {
                    info!("   Content: {}", text);
                }
            } else {
                warn!(
                    "[WARN] Binary request result has no data (ID: {})",
                    result.message_id
                );
            }
        }
        Err(e) => {
            warn!("[WARN] Binary request failed: {:?}", e);
        }
    }

    // Wait for event processing to complete
    info!("[WAIT] Waiting for event processing to complete...");
    let _ = tokio::time::timeout(Duration::from_secs(5), event_handle).await;

    // Gracefully disconnect
    info!("[CLOSE] Disconnecting...");
    transport.disconnect().await?;

    info!("[STOP] TCP Echo client example completed");
    Ok(())
}
