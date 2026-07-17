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

use msgtrans::{
    event::ServerEvent, packet::Packet, protocol::WebSocketServerConfig, tokio,
    transport::TransportServerBuilder,
};
use std::env;

const BIZ_DROP: u8 = 250;
const DEFAULT_PORT: u16 = 18080;

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
        .with_protocol(ws_config)
        .build()
        .await?;

    let mut events = transport.subscribe_events();
    let transport_for_echo = transport.clone();
    let transport_for_serve = transport.clone();

    let event_task = tokio::spawn(async move {
        let transport = transport_for_echo;
        while let Ok(event) = events.recv().await {
            if let ServerEvent::MessageReceived {
                session_id,
                context,
            } = event
            {
                let biz_type = context.biz_type;
                let payload = context.data.clone();

                if biz_type == BIZ_DROP {
                    // Silent drop — used by TS timeout / close-pending tests.
                    continue;
                }

                if context.is_request() {
                    context.respond(payload);
                } else {
                    // Mirror OneWay back with same biz_type and payload.
                    let transport_clone = transport.clone();
                    tokio::spawn(async move {
                        let mut packet = Packet::one_way(0, payload);
                        packet.set_biz_type(biz_type);
                        let _ = transport_clone.send_to_session(session_id, packet).await;
                    });
                }
            }
        }
    });

    // Print ready line BEFORE awaiting serve(). The TS test waits for this line
    // and then retries connect with a small backoff to cover the brief window
    // until the WS listener binds.
    println!("MSGTRANS_E2E_READY ws://{}", bind);

    let serve_result = transport_for_serve.serve().await;
    let _ = event_task.await;
    serve_result?;
    Ok(())
}
