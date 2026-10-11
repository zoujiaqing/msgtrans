/// Protocol abstraction layer module
///
/// Provides abstractions for protocol configuration, adapters, and factories
pub mod adapter;
pub mod client_config;
pub mod server_config;

// Re-export core types
// Re-export configuration types
#[cfg(feature = "quic")]
pub use client_config::QuicClientConfig;
#[cfg(feature = "tcp")]
pub use client_config::TcpClientConfig;
#[cfg(feature = "websocket")]
pub use client_config::WebSocketClientConfig;
#[cfg(feature = "websocket")]
pub use client_config::{ClientTls, WS_SUBPROTOCOL_MSGTRANS};
#[cfg(feature = "quic")]
pub use server_config::QuicServerConfig;
#[cfg(feature = "tcp")]
pub use server_config::TcpServerConfig;
#[cfg(feature = "websocket")]
pub use server_config::WebSocketServerConfig;

// Re-export adapter configuration

// `ProtocolConfig`/`ConfigError` are only referenced by the protocol adapters
// and their configs, all of which are feature-gated; a no-protocol build uses
// neither. (The SPI re-exports them from `adapter` directly.)
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
pub use adapter::ConfigError;
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
pub(crate) use adapter::ProtocolConfig;
