/// Protocol abstraction layer module
///
/// Provides abstractions for protocol configuration, adapters, and factories
pub mod adapter;
pub mod client_config;
pub mod protocol;
pub mod protocol_adapter;
pub mod server_config;

// Re-export core types
pub use adapter::{AdapterStats, ProtocolAdapter};
pub use protocol::{
    BoxFuture, PluginManager, ProtocolFactory, ProtocolRegistry, ProtocolSet, StandardProtocols,
};

// Re-export configuration types
pub use client_config::{QuicClientConfig, RetryConfig, TcpClientConfig, WebSocketClientConfig};
pub use server_config::{QuicServerConfig, TcpServerConfig, WebSocketServerConfig};

// Re-export adapter configuration
pub use adapter::{
    ClientConfig, ConfigError, DynClientConfig, DynProtocolConfig, DynServerConfig, ProtocolConfig,
    ServerConfig,
};
