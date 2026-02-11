/// Echo server - simplified API demonstration
/// [TARGET] Byte-only API version, users handle string conversion
use msgtrans::{
    event::ServerEvent, protocol::QuicServerConfig, protocol::TcpServerConfig,
    protocol::WebSocketServerConfig, tokio, transport::TransportServerBuilder,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable verbose logging to observe event flow
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("[TARGET] Echo server - simplified API demonstration (byte-only version)");
    println!("============================================");
    println!("[INFO] Using Pattern A: define event handling first, then start server");
    println!();

    // Create TCP server configuration - use ? operator to handle errors
    let tcp_config = TcpServerConfig::new("127.0.0.1:8001")?;
    let web_socket_server_config = WebSocketServerConfig::new("127.0.0.1:8002")?;
    let quic_server_config = QuicServerConfig::new("127.0.0.1:8003")?;

    let transport = TransportServerBuilder::new()
        .max_connections(10) // Limit connections for testing
        .with_protocol(tcp_config)
        .with_protocol(web_socket_server_config)
        .with_protocol(quic_server_config)
        .build()
        .await?;

    println!("[SUCCESS] TCP server created: 127.0.0.1:8001");

    // [TARGET] Core: create event stream immediately (server not started yet)
    let mut events = transport.subscribe_events();
    println!("[INFO] Event stream created - server not started yet");

    // Clone transport for sending echo in event handling
    let transport_for_echo = transport.clone();
    // Clone another one for starting server at the end
    let transport_for_serve = transport.clone();

    // [TARGET] Pattern A: define complete event handling logic first
    let event_task = tokio::spawn(async move {
        let transport = transport_for_echo; // Move cloned transport into closure
        println!("[START] Starting to listen for events...");
        let mut event_count = 0u64;
        let mut connections = std::collections::HashMap::new();

        while let Ok(event) = events.recv().await {
            event_count += 1;
            println!("[RECV] Event #{}: {:?}", event_count, event);

            match event {
                ServerEvent::ConnectionEstablished { session_id, info } => {
                    event_count += 1;
                    println!("[RECV] Event #{}: New connection established", event_count);
                    println!("   Session ID: {}", session_id);
                    println!("   Address: {} ↔ {}", info.local_addr, info.peer_addr);

                    // [ASYNC] Fix: move send operation to separate async task to avoid blocking event loop
                    let transport_clone = transport.clone();
                    tokio::spawn(async move {
                        // Send welcome message - use unified byte API
                        match transport_clone
                            .send(session_id, "Welcome to Echo Server!".as_bytes())
                            .await
                        {
                            Ok(result) => {
                                println!(
                                    "[SUCCESS] Welcome message sent -> session {} (ID: {})",
                                    session_id, result.message_id
                                );
                            }
                            Err(e) => {
                                println!("[ERROR] Welcome message send failed: {:?}", e);
                            }
                        }

                        // [TARGET] Demonstrate server sending request to client - use simplified byte API
                        // Wait 100ms to ensure client is fully ready
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                        println!("[REQUEST] Server sending request to client...");
                        match transport_clone
                            .request(session_id, "Server asks: What is your status?".as_bytes())
                            .await
                        {
                            Ok(result) => {
                                if let Some(response_data) = &result.data {
                                    let response_text = String::from_utf8_lossy(response_data);
                                    println!(
                                        "[SUCCESS] Received client response (ID: {}): \"{}\"",
                                        result.message_id, response_text
                                    );
                                } else {
                                    println!(
                                        "[WARN] Request result has no data (ID: {})",
                                        result.message_id
                                    );
                                }
                            }
                            Err(e) => {
                                println!("[ERROR] Server request failed: {:?}", e);
                            }
                        }
                    });

                    connections.insert(session_id, info);
                }
                ServerEvent::ConnectionClosed { session_id, reason } => {
                    event_count += 1;
                    println!("[RECV] Event #{}: Connection closed", event_count);
                    println!("   Session ID: {}", session_id);
                    println!("   Reason: {:?}", reason);
                    connections.remove(&session_id);
                }
                ServerEvent::MessageReceived {
                    session_id,
                    context,
                } => {
                    event_count += 1;
                    let msg_text = context.as_text_lossy();
                    let message_id = context.message_id;
                    let is_request = context.is_request();

                    println!("[RECV] Event #{}: Message received", event_count);
                    println!("   Session: {}", session_id);
                    println!("   Message ID: {}", message_id);
                    println!("   Size: {} bytes", context.data.len());
                    println!("   Content: \"{}\"", msg_text);

                    // If it's a request, respond
                    if is_request {
                        let echo_message = format!("Echo: {}", msg_text);
                        println!("[SEND] Responding to client request...");
                        context.respond(echo_message.as_bytes().to_vec());
                        println!("[SUCCESS] Client request responded (ID: {})", message_id);
                    } else {
                        // [ASYNC] Fix: move send operation to separate async task to avoid blocking event loop
                        let transport_clone = transport.clone();
                        let echo_message = format!("Echo: {}", msg_text);
                        tokio::spawn(async move {
                            // Send echo - use unified byte API
                            match transport_clone
                                .send(session_id, echo_message.as_bytes())
                                .await
                            {
                                Ok(result) => {
                                    println!(
                                        "[SUCCESS] Echo sent -> session {} (ID: {})",
                                        session_id, result.message_id
                                    );
                                }
                                Err(e) => {
                                    println!("[ERROR] Echo send failed: {:?}", e);
                                }
                            }
                        });
                    }
                }
                ServerEvent::MessageSent {
                    session_id,
                    message_id,
                } => {
                    println!(
                        "[SEND] Message send confirmation: session {}, message ID {}",
                        session_id, message_id
                    );
                }
                ServerEvent::TransportError { session_id, error } => {
                    println!(
                        "[WARN] Transport error: {:?} (session: {:?})",
                        error, session_id
                    );
                }
                ServerEvent::ServerStarted { address } => {
                    println!("[START] Server start notification: {}", address);
                }
                ServerEvent::ServerStopped => {
                    println!("[STOP] Server stop notification");
                }
            }
        }

        println!(
            "[WARN] Event stream ended (processed {} events total)",
            event_count
        );
        println!("   Final connection count: {}", connections.len());
    });

    println!("[START] Event handling task started");
    println!("[INFO] Now starting server...");
    println!();
    println!("[TARGET] Test methods:");
    println!("   Run in another terminal: cargo run --example echo_client_tcp");
    println!("   Or use: telnet 127.0.0.1 8001");
    println!();

    // [TARGET] Key to Pattern A: only now start the server, but event stream is already listening
    let server_result = transport_for_serve.serve().await;

    println!("[STOP] Server stopped");

    // Wait for event handling to complete
    let _ = event_task.await;

    if let Err(e) = server_result {
        println!("[ERROR] Server error: {:?}", e);
    }

    Ok(())
}
