use crate::error::TransportError;
#[cfg(feature = "quic")]
use crate::protocol::{QuicClientConfig, QuicServerConfig};
#[cfg(feature = "tcp")]
use crate::protocol::{TcpClientConfig, TcpServerConfig};
#[cfg(feature = "websocket")]
use crate::protocol::{WebSocketClientConfig, WebSocketServerConfig};

/// Protocol configuration trait
pub trait ProtocolConfig: Send + Sync + Clone + std::fmt::Debug + 'static {
    /// Validate if configuration is valid
    fn validate(&self) -> Result<(), ConfigError>;

    /// Get default configuration
    fn default_config() -> Self;
}

/// Object-safe protocol configuration trait for unified Builder interface
pub trait DynProtocolConfig: Send + Sync + 'static {
    /// Get protocol name
    fn protocol_name(&self) -> &'static str;

    /// Validate configuration
    fn validate_dyn(&self) -> Result<(), ConfigError>;
}

/// 🔧 Server-specific dynamic configuration
pub trait DynServerConfig: DynProtocolConfig {
    /// Dynamically build server (object-safe) with per-connection limits.
    #[allow(clippy::type_complexity)]
    fn build_server_dyn(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Server>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    >;

    /// Get bind address
    fn get_bind_address(&self) -> std::net::SocketAddr;

    /// Clone as `Box<dyn DynServerConfig>`
    fn clone_server_dyn(&self) -> Box<dyn DynServerConfig>;
}

/// 🔧 Client-specific dynamic configuration  
pub trait DynClientConfig: DynProtocolConfig {
    /// Dynamically build connection (object-safe) with per-connection limits.
    #[allow(clippy::type_complexity)]
    fn build_connection_dyn(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Connection>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    >;

    /// Clone as `Box<dyn DynClientConfig>`
    fn clone_client_dyn(&self) -> Box<dyn DynClientConfig>;
}

/// Protocol configuration error
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Invalid address '{address}': {reason}")]
    InvalidAddress {
        address: String,
        reason: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("Invalid port {port}: {reason}\nSuggestion: Use a port between 1 and 65535")]
    InvalidPort { port: u32, reason: String },

    #[error("Missing required field '{field}'\nSuggestion: {suggestion}")]
    MissingRequiredField { field: String, suggestion: String },

    #[error("Invalid value for '{field}': {value}\nReason: {reason}\nSuggestion: {suggestion}")]
    InvalidValue {
        field: String,
        value: String,
        reason: String,
        suggestion: String,
    },

    #[error("File not found: '{path}'\nSuggestion: {suggestion}")]
    FileNotFound { path: String, suggestion: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(feature = "tcp")]
impl ServerConfig for TcpServerConfig {
    type Server = crate::adapters::factories::TcpServerWrapper;

    async fn build_server(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self::Server, TransportError> {
        use crate::adapters::tcp::TcpServerBuilder;

        let server = TcpServerBuilder::new()
            .bind_address(self.bind_address)
            .config(self.clone())
            .limits(limits)
            .build()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build TCP server: {:?}", e),
                    true,
                )
            })?;

        Ok(crate::adapters::factories::TcpServerWrapper::new(server))
    }
}

#[cfg(feature = "tcp")]
impl ClientConfig for TcpClientConfig {
    type Connection = crate::adapters::tcp::TcpAdapter<TcpClientConfig>;

    async fn build_connection(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self::Connection, TransportError> {
        use crate::adapters::tcp::TcpClientBuilder;

        TcpClientBuilder::new()
            .target_address(self.target_address)
            .config(self.clone())
            .limits(limits)
            .connect()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build TCP connection: {:?}", e),
                    true,
                )
            })
    }
}

#[cfg(feature = "websocket")]
impl ServerConfig for WebSocketServerConfig {
    type Server = crate::adapters::factories::WebSocketServerWrapper;

    async fn build_server(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self::Server, TransportError> {
        use crate::adapters::websocket::WebSocketServerBuilder;

        let server = WebSocketServerBuilder::new()
            .limits(limits)
            .bind_address(self.bind_address)
            .config(self.clone())
            .build()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build WebSocket server: {:?}", e),
                    true,
                )
            })?;

        Ok(crate::adapters::factories::WebSocketServerWrapper::new(
            server,
        ))
    }
}

#[cfg(feature = "websocket")]
impl ClientConfig for WebSocketClientConfig {
    type Connection = crate::adapters::websocket::WebSocketAdapter<WebSocketClientConfig>;

    async fn build_connection(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self::Connection, TransportError> {
        use crate::adapters::websocket::WebSocketClientBuilder;

        WebSocketClientBuilder::new()
            .limits(limits)
            .target_url(&self.target_url)
            .config(self.clone())
            .connect()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build WebSocket connection: {:?}", e),
                    true,
                )
            })
    }
}

#[cfg(feature = "quic")]
impl ServerConfig for QuicServerConfig {
    type Server = crate::adapters::factories::QuicServerWrapper;

    async fn build_server(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self::Server, TransportError> {
        use crate::adapters::quic::QuicServerBuilder;

        let server = QuicServerBuilder::new()
            .limits(limits)
            .bind_address(self.bind_address)
            .config(self.clone())
            .build()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build QUIC server: {:?}", e),
                    true,
                )
            })?;

        Ok(crate::adapters::factories::QuicServerWrapper::new(server))
    }
}

#[cfg(feature = "quic")]
impl ClientConfig for QuicClientConfig {
    type Connection = crate::adapters::quic::QuicAdapter<QuicClientConfig>;

    async fn build_connection(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self::Connection, TransportError> {
        use crate::adapters::quic::QuicClientBuilder;

        QuicClientBuilder::new()
            .limits(limits)
            .target_address(self.target_address)
            .config(self.clone())
            .connect()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build QUIC connection: {:?}", e),
                    true,
                )
            })
    }
}

/// Server configuration trait — the typed builder used internally by each
/// protocol's `DynServerConfig` impl.
///
/// Crate-private on purpose: its `type Server` associated type names the
/// concrete (private) adapter, so exposing this trait would have written the
/// internal adapter types into the frozen public API and made any internal
/// adapter refactor a breaking change. External protocols implement the
/// object-safe [`DynServerConfig`] instead.
pub(crate) trait ServerConfig: Send + Sync + 'static {
    type Server: crate::Server;

    /// Build server instance with the per-connection resource limits.
    fn build_server(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> impl std::future::Future<Output = Result<Self::Server, TransportError>> + Send;
}

/// Client configuration trait — the typed builder used internally by each
/// protocol's `DynClientConfig` impl. Crate-private for the same reason as
/// [`ServerConfig`]: `type Connection` names the private adapter.
pub(crate) trait ClientConfig: Send + Sync + 'static {
    type Connection: crate::Connection;

    /// Build connection instance with the per-connection resource limits.
    fn build_connection(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> impl std::future::Future<Output = Result<Self::Connection, TransportError>> + Send;
}

// ConnectableConfig implementation has been moved to client_config.rs
