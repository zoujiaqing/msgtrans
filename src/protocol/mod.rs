/// Protocol abstraction layer module
///
/// Provides abstractions for protocol configuration, adapters, and factories
pub mod adapter;
pub mod client_config;
pub mod protocol_adapter;
pub mod server_config;

// Re-export core types
pub use adapter::AdapterStats;
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
pub use adapter::{
    ClientConfig, ConfigError, DynClientConfig, DynProtocolConfig, DynServerConfig, ProtocolConfig,
    ServerConfig,
};
