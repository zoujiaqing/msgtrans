//! UniFFI bindings for the [msgtrans](https://github.com/zoujiaqing/msgtrans)
//! client, for Kotlin (Android, JVM) and Swift (iOS, macOS).
//!
//! The surface is the client only: connect over TCP (optionally TLS with SPKI
//! pinning), WebSocket or QUIC, send messages and requests, receive events,
//! and answer requests the server sends.

mod client;
mod config;
mod error;
mod events;
mod runtime;

pub use client::{MsgtransClient, RequestOptions, SendOptions, SendReceipt};
pub use config::{ClientConfig, ClientLimits, Endpoint, QuicVerification, TcpTls, WebSocketTls};
pub use error::MsgtransError;
pub use events::{
    ClientEvent, CloseReason, ConnectionInfo, InboundMessage, IncomingRequest, RespondOutcome,
};

/// Version of these bindings.
#[uniffi::export]
pub fn msgtrans_ffi_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

uniffi::setup_scaffolding!();
