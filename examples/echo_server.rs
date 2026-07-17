//! Echo server — simplified API demonstration.
//!
//! Two modes:
//!
//! - **Demo mode** (default): verbose `tracing` logs, welcome message on connect,
//!   server-initiated request, `"Echo: "` prefix on echoed payloads. This is the
//!   user-facing demonstration program.
//!
//! - **Passive mode** (`MSGTRANS_ECHO_MODE=passive`): silent stdout except for
//!   one stable ready line, **byte-for-byte** echo (no prefix), no welcome /
//!   server-initiated request, and `biz_type=250` is silently dropped (used by
//!   timeout / close-pending E2E tests).
//!
//!   In passive mode, all three protocol listeners (TCP / WebSocket / QUIC) still
//!   bind, so this serves as a real-world compatibility target for cross-language
//!   clients (the `@msgtrans/client` TypeScript test suite uses it).
//!
//!   Env vars (passive mode only):
//!     - `MSGTRANS_ECHO_TCP_PORT`  (default 18091)
//!     - `MSGTRANS_ECHO_WS_PORT`   (default 18092)
//!     - `MSGTRANS_ECHO_QUIC_PORT` (default 18093)
//!
//!   Ready line written to stdout after `serve()` is invoked:
//!     `MSGTRANS_E2E_READY ws://127.0.0.1:<ws_port>`
use msgtrans::{
    event::ServerEvent, packet::Packet, protocol::QuicServerConfig, protocol::TcpServerConfig,
    protocol::WebSocketServerConfig, tokio, transport::TransportServerBuilder,
};
use std::env;

const BIZ_DROP: u8 = 250;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let passive = env::var("MSGTRANS_ECHO_MODE").ok().as_deref() == Some("passive");

    if !passive {
        // Demo mode: verbose tracing observation.
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .init();
    }

    let tcp_port = read_port("MSGTRANS_ECHO_TCP_PORT", if passive { 18091 } else { 8001 });
    let ws_port = read_port("MSGTRANS_ECHO_WS_PORT", if passive { 18092 } else { 8002 });
    let quic_port = read_port(
        "MSGTRANS_ECHO_QUIC_PORT",
        if passive { 18093 } else { 8003 },
    );

    if !passive {
        println!("[TARGET] Echo server - simplified API demonstration (byte-only version)");
        println!("============================================");
        println!("[INFO] Using Pattern A: define event handling first, then start server");
        println!();
    }

    let tcp_config = TcpServerConfig::new(format!("127.0.0.1:{}", tcp_port).as_str())?;
    let ws_config = WebSocketServerConfig::new(format!("127.0.0.1:{}", ws_port).as_str())?;
    let quic_config = QuicServerConfig::new(format!("127.0.0.1:{}", quic_port).as_str())?;

    let transport = TransportServerBuilder::new()
        .max_connections(if passive { 64 } else { 10 })
        .with_protocol(tcp_config)
        .with_protocol(ws_config)
        .with_protocol(quic_config)
        .build()
        .await?;

    if !passive {
        println!("[SUCCESS] TCP server created: 127.0.0.1:{}", tcp_port);
    }

    let mut events = transport.subscribe_events();
    if !passive {
        println!("[INFO] Event stream created - server not started yet");
    }

    let transport_for_echo = transport.clone();
    let transport_for_serve = transport.clone();

    let event_task = tokio::spawn(async move {
        let transport = transport_for_echo;
        if !passive {
            println!("[START] Starting to listen for events...");
        }
        let mut event_count = 0u64;
        let mut connections = std::collections::HashMap::new();

        while let Ok(event) = events.recv().await {
            event_count += 1;
            if !passive {
                println!("[RECV] Event #{}: {:?}", event_count, event);
            }

            match event {
                ServerEvent::ConnectionEstablished { session_id, info } => {
                    if !passive {
                        event_count += 1;
                        println!("[RECV] Event #{}: New connection established", event_count);
                        println!("   Session ID: {}", session_id);
                        println!("   Address: {} ↔ {}", info.local_addr, info.peer_addr);

                        // Welcome message + server-initiated request (demo only)
                        let transport_clone = transport.clone();
                        tokio::spawn(async move {
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
                    }

                    connections.insert(session_id, info);
                }
                ServerEvent::ConnectionClosed { session_id, reason } => {
                    if !passive {
                        event_count += 1;
                        println!("[RECV] Event #{}: Connection closed", event_count);
                        println!("   Session ID: {}", session_id);
                        println!("   Reason: {:?}", reason);
                    }
                    connections.remove(&session_id);
                }
                ServerEvent::MessageReceived {
                    session_id,
                    context,
                } => {
                    if passive {
                        // Byte-for-byte echo with biz_type preserved.
                        // biz_type=250 is silently dropped (timeout/close tests).
                        let biz_type = context.biz_type;
                        let payload = context.data.clone();
                        let is_request = context.is_request();

                        if biz_type == BIZ_DROP {
                            continue;
                        }

                        if is_request {
                            context.respond(payload);
                        } else {
                            let transport_clone = transport.clone();
                            tokio::spawn(async move {
                                let mut packet = Packet::one_way(0, payload);
                                packet.set_biz_type(biz_type);
                                let _ = transport_clone.send_to_session(session_id, packet).await;
                            });
                        }
                    } else {
                        // Demo: "Echo: " prefix.
                        event_count += 1;
                        let msg_text = context.as_text_lossy();
                        let message_id = context.message_id;
                        let is_request = context.is_request();

                        println!("[RECV] Event #{}: Message received", event_count);
                        println!("   Session: {}", session_id);
                        println!("   Message ID: {}", message_id);
                        println!("   Size: {} bytes", context.data.len());
                        println!("   Content: \"{}\"", msg_text);

                        if is_request {
                            let echo_message = format!("Echo: {}", msg_text);
                            println!("[SEND] Responding to client request...");
                            context.respond(echo_message.as_bytes().to_vec());
                            println!("[SUCCESS] Client request responded (ID: {})", message_id);
                        } else {
                            let transport_clone = transport.clone();
                            let echo_message = format!("Echo: {}", msg_text);
                            tokio::spawn(async move {
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
                }
                ServerEvent::MessageSent {
                    session_id,
                    message_id,
                } => {
                    if !passive {
                        println!(
                            "[SEND] Message send confirmation: session {}, message ID {}",
                            session_id, message_id
                        );
                    }
                }
                ServerEvent::TransportError { session_id, error } => {
                    if !passive {
                        println!(
                            "[WARN] Transport error: {:?} (session: {:?})",
                            error, session_id
                        );
                    }
                }
                ServerEvent::ServerStarted { address } => {
                    if !passive {
                        println!("[START] Server start notification: {}", address);
                    }
                }
                ServerEvent::ServerStopped => {
                    if !passive {
                        println!("[STOP] Server stop notification");
                    }
                }
            }
        }

        if !passive {
            println!(
                "[WARN] Event stream ended (processed {} events total)",
                event_count
            );
            println!("   Final connection count: {}", connections.len());
        }
    });

    if !passive {
        println!("[START] Event handling task started");
        println!("[INFO] Now starting server...");
        println!();
        println!("[TARGET] Test methods:");
        println!("   Run in another terminal: cargo run --example echo_client_tcp");
        println!("   Or use: telnet 127.0.0.1 {}", tcp_port);
        println!();
    } else {
        // Single, stable ready line for cross-language test runners.
        println!("MSGTRANS_E2E_READY ws://127.0.0.1:{}", ws_port);
    }

    let server_result = transport_for_serve.serve().await;

    if !passive {
        println!("[STOP] Server stopped");
    }

    let _ = event_task.await;

    if let Err(e) = server_result {
        if !passive {
            println!("[ERROR] Server error: {:?}", e);
        }
        return Err(e.into());
    }

    Ok(())
}

fn read_port(name: &str, default: u16) -> u16 {
    env::var(name)
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(default)
}
