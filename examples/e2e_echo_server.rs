//! E2E echo server for the @msgtrans/client TypeScript test suite.
//!
//! Differences from `echo_server`:
//! - WebSocket only (no TCP / QUIC), reduces port conflicts.
//! - Configurable bind port via `MSGTRANS_E2E_PORT` env (default 18080).
//! - Prints exactly one stable ready line: `MSGTRANS_E2E_READY ws://127.0.0.1:<port>`
//! - Pure passive echo — no welcome message, no server-initiated request.
//! - For incoming Requests: responds with the **exact same** payload bytes.
//! - For incoming OneWay: sends back a OneWay with the same payload + biz_type
//!   so the TS side can verify round-trip semantics.
//! - `biz_type == 250` (`BIZ_DROP`) is silently dropped to support timeout tests.
//!
//! Run: `cargo run --example e2e_echo_server`
//!
//! Stop: SIGINT / SIGTERM.

use async_trait::async_trait;
use msgtrans::{
    Packet, Responder, SessionHandler, SessionId, SessionSender, TransportServerBuilder,
    WebSocketServerConfig,
};
use std::{env, sync::Arc};

const BIZ_DROP: u8 = 250;
const DEFAULT_PORT: u16 = 18080;

struct EchoHandler;

#[async_trait]
impl SessionHandler for EchoHandler {
    async fn on_message(&self, _session_id: SessionId, packet: Packet, sender: SessionSender) {
        let biz_type = packet.biz_type();

        if biz_type == BIZ_DROP {
            // Silent drop — used by TS timeout / close-pending tests.
            return;
        }

        // Mirror OneWay back with same biz_type and payload. The transport
        // allocates the id — handlers cannot number packets themselves.
        let _ = sender
            .send_data_with_options(
                packet.into_payload(),
                msgtrans::SendOptions::new().biz_type(biz_type),
            )
            .await;
    }

    async fn on_request(&self, _session_id: SessionId, request: Packet, responder: Responder) {
        if request.biz_type() == BIZ_DROP {
            // Silent drop: the untouched responder leaves the request to the
            // lifecycle machinery (timeout), as the TS tests expect.
            return;
        }
        let _ = responder.respond(request.into_payload()).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = env::var("MSGTRANS_E2E_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let bind = format!("127.0.0.1:{}", port);
    let ws_config = WebSocketServerConfig::new(&bind)?;

    let transport = TransportServerBuilder::new()
        .max_connections(64)
        .protocol(ws_config)
        .build(Arc::new(EchoHandler))
        .await?;

    // Print ready line BEFORE awaiting serve(). The TS test waits for this line
    // and then retries connect with a small backoff to cover the brief window
    // until the WS listener binds.
    println!("MSGTRANS_E2E_READY ws://{}", bind);

    transport.serve().await?;
    Ok(())
}
