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
//! Ready line written to stdout after `serve()` is invoked:
//! `MSGTRANS_E2E_READY ws://127.0.0.1:<ws_port>`
use async_trait::async_trait;
use msgtrans::{
    command::ConnectionInfo,
    packet::Packet,
    protocol::{QuicServerConfig, TcpServerConfig, WebSocketServerConfig},
    transport::{SessionHandler, SessionSender, TransportServer, TransportServerBuilder},
    SessionId,
};
use std::{env, sync::Arc, sync::OnceLock};

const BIZ_DROP: u8 = 250;

struct EchoHandler {
    passive: bool,
    /// Set once the server exists, so demo mode can initiate a request back to a
    /// client. Handlers are built before the server, hence the deferred fill-in.
    server: OnceLock<TransportServer>,
}

#[async_trait]
impl SessionHandler for EchoHandler {
    async fn on_connected(&self, session_id: SessionId, info: ConnectionInfo) {
        if self.passive {
            return;
        }
        println!("[RECV] New connection established");
        println!("   Session ID: {}", session_id);
        println!("   Address: {} ↔ {}", info.local_addr, info.peer_addr);

        let Some(server) = self.server.get().cloned() else {
            return;
        };
        tokio::spawn(async move {
            match server
                .send(session_id, "Welcome to Echo Server!".as_bytes())
                .await
            {
                Ok(result) => println!(
                    "[SUCCESS] Welcome message sent -> session {} (ID: {})",
                    session_id, result.message_id
                ),
                Err(e) => println!("[ERROR] Welcome message send failed: {:?}", e),
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            println!("[REQUEST] Server sending request to client...");
            match server
                .request(session_id, "Server asks: What is your status?".as_bytes())
                .await
            {
                Ok(result) => match &result.data {
                    Some(data) => println!(
                        "[SUCCESS] Received client response (ID: {}): \"{}\"",
                        result.message_id,
                        String::from_utf8_lossy(data)
                    ),
                    None => println!(
                        "[WARN] Request result has no data (ID: {})",
                        result.message_id
                    ),
                },
                Err(e) => println!("[ERROR] Server request failed: {:?}", e),
            }
        });
    }

    async fn on_message(&self, session_id: SessionId, packet: Packet, sender: SessionSender) {
        let biz_type = packet.biz_type();

        if self.passive {
            // Byte-for-byte echo with biz_type preserved.
            // biz_type=250 is silently dropped (timeout/close tests).
            if biz_type == BIZ_DROP {
                return;
            }
            let mut echo = Packet::one_way(0, packet.into_payload());
            echo.set_biz_type(biz_type);
            let _ = sender.send(echo).await;
            return;
        }

        // Demo: "Echo: " prefix.
        let msg_text = String::from_utf8_lossy(packet.payload()).to_string();
        println!("[RECV] Message received");
        println!("   Session: {}", session_id);
        println!("   Content: \"{}\"", msg_text);

        let echo_message = format!("Echo: {}", msg_text);
        match sender.send_data(echo_message.into_bytes()).await {
            Ok(()) => println!("[SUCCESS] Echo sent -> session {}", session_id),
            Err(e) => println!("[ERROR] Echo send failed: {:?}", e),
        }
    }

    async fn on_request(
        &self,
        session_id: SessionId,
        request: Packet,
        responder: msgtrans::transport::Responder,
    ) {
        let biz_type = request.biz_type();
        if self.passive {
            if biz_type == BIZ_DROP {
                return; // dropped responder -> request left to the lifecycle machinery
            }
            let _ = responder.respond(request.into_payload()).await;
            return;
        }
        let msg_text = String::from_utf8_lossy(request.payload()).to_string();
        println!("[SEND] Responding to client request (session {session_id})...");
        let echo_message = format!("Echo: {}", msg_text);
        let _ = responder.respond(echo_message.into_bytes()).await;
    }

    async fn on_disconnected(&self, session_id: SessionId, reason: msgtrans::CloseReason) {
        if !self.passive {
            println!("[RECV] Connection closed: {} ({:?})", session_id, reason);
        }
    }

    async fn on_error(&self, session_id: SessionId, error: msgtrans::TransportError) {
        if !self.passive {
            println!("[WARN] Transport error on {}: {:?}", session_id, error);
        }
    }
}

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
        println!();
    }

    let tcp_config = TcpServerConfig::new(format!("127.0.0.1:{}", tcp_port).as_str())?;
    let ws_config = WebSocketServerConfig::new(format!("127.0.0.1:{}", ws_port).as_str())?;
    let quic_config = QuicServerConfig::new(format!("127.0.0.1:{}", quic_port).as_str())?;

    let handler = Arc::new(EchoHandler {
        passive,
        server: OnceLock::new(),
    });

    let transport = TransportServerBuilder::new()
        .max_connections(if passive { 64 } else { 10 })
        .protocol(tcp_config)
        .protocol(ws_config)
        .protocol(quic_config)
        .build(handler.clone())
        .await?;

    // Let the handler talk back to the server it is attached to.
    let _ = handler.server.set(transport.clone());

    if !passive {
        println!("[SUCCESS] TCP server created: 127.0.0.1:{}", tcp_port);
        println!("[TARGET] Test methods:");
        println!("   Run in another terminal: cargo run --example echo_client_tcp");
        println!("   Or use: telnet 127.0.0.1 {}", tcp_port);
        println!();
    } else {
        // Single, stable ready line for cross-language test runners.
        println!("MSGTRANS_E2E_READY ws://127.0.0.1:{}", ws_port);
    }

    let server_result = transport.serve().await;

    if !passive {
        println!("[STOP] Server stopped");
    }

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
