use msgtrans::{
    event::ClientEvent, event::ServerEvent, protocol::TcpClientConfig, protocol::TcpServerConfig,
    transport::TransportClientBuilder, transport::TransportServerBuilder,
};
/// Specialized program for debugging transport event streams
///
/// Used to diagnose event bridging and server-side event handling for lock-free connections
use std::time::Duration;

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

    let server = TransportServerBuilder::new()
        .with_protocol(tcp_config)
        .build()
        .await?;

    println!("[SUCCESS] Server created: 127.0.0.1:9001");

    // Subscribe to server events
    let mut server_events = server.subscribe_events();
    let server_clone = server.clone();

    // Server event handling
    let server_task = tokio::spawn(async move {
        println!("[START] Server event handling started");
        let mut event_count = 0;

        while let Ok(event) = server_events.recv().await {
            event_count += 1;
            println!("[RECV] Server event #{}: {:?}", event_count, event);

            match event {
                ServerEvent::ConnectionEstablished { session_id, .. } => {
                    println!("[CONNECT] New connection established: {}", session_id);

                    // [ASYNC] Fix: move send operation to separate async task to avoid blocking event loop
                    let server_for_send = server_clone.clone();
                    tokio::spawn(async move {
                        // Send a test message immediately
                        if let Err(e) = server_for_send
                            .send(session_id, b"Hello from server!")
                            .await
                        {
                            println!("[ERROR] Server send failed: {:?}", e);
                        } else {
                            println!("[SUCCESS] Server send successful");
                        }
                    });
                }
                ServerEvent::MessageReceived {
                    session_id,
                    context,
                } => {
                    println!(
                        "[RECV] Message received (session: {}, ID: {}, request: {}): {}",
                        session_id,
                        context.message_id,
                        context.is_request(),
                        context.as_text_lossy()
                    );

                    if context.is_request() {
                        let response = format!("Echo: {}", context.as_text_lossy());
                        println!("[SEND] Responding to request...");
                        context.respond(response.into_bytes());
                        println!("[SUCCESS] Request responded");
                    }
                }
                ServerEvent::MessageSent {
                    session_id,
                    message_id,
                } => {
                    println!(
                        "[SEND] Message send confirmation: session {}, ID {}",
                        session_id, message_id
                    );
                }
                ServerEvent::ConnectionClosed { session_id, reason } => {
                    println!(
                        "[CLOSE] Connection closed: session {}, reason: {:?}",
                        session_id, reason
                    );
                    break;
                }
                _ => {
                    println!("[INFO] Other event: {:?}", event);
                }
            }
        }

        println!(
            "[WARN] Server event handling ended (processed {} events)",
            event_count
        );
    });

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
        .with_protocol(client_config)
        .build()
        .await?;

    let mut client_events = client.subscribe_events();

    // Client event handling
    let client_task = tokio::spawn(async move {
        println!("[START] Client event handling started");
        let mut event_count = 0;

        while let Ok(event) = client_events.recv().await {
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
                        context.respond(b"Client response!".to_vec());
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
    let _ = tokio::join!(server_task, client_task);

    println!("[STOP] Debug program completed");
    Ok(())
}
