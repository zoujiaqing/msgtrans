use crate::command::ConnectionInfo;
use crate::packet::Packet;
use crate::protocol::{
    QuicClientConfig, QuicServerConfig, TcpClientConfig, TcpServerConfig, WebSocketClientConfig,
    WebSocketServerConfig,
};
use crate::{error::TransportError, SessionId};
use async_trait::async_trait;

/// Adapter statistics information
#[derive(Debug, Clone)]
pub struct AdapterStats {
    /// Number of packets sent
    pub packets_sent: u64,
    /// Number of packets received
    pub packets_received: u64,
    /// Number of bytes sent
    pub bytes_sent: u64,
    /// Number of bytes received
    pub bytes_received: u64,
    /// Error count
    pub errors: u64,
    /// Last activity time
    pub last_activity: std::time::SystemTime,
}

impl Default for AdapterStats {
    fn default() -> Self {
        Self {
            packets_sent: 0,
            packets_received: 0,
            bytes_sent: 0,
            bytes_received: 0,
            errors: 0,
            last_activity: std::time::SystemTime::now(),
        }
    }
}

impl AdapterStats {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn record_packet_sent(&mut self, size: usize) {
        self.packets_sent += 1;
        self.bytes_sent += size as u64;
        self.last_activity = std::time::SystemTime::now();
    }

    pub fn record_packet_received(&mut self, size: usize) {
        self.packets_received += 1;
        self.bytes_received += size as u64;
        self.last_activity = std::time::SystemTime::now();
    }

    pub fn record_error(&mut self) {
        self.errors += 1;
        self.last_activity = std::time::SystemTime::now();
    }
}

/// Protocol adapter trait
///
/// Defines the basic interface that all protocol adapters must implement
/// This is the core abstraction of the event-driven architecture
#[async_trait]
pub trait ProtocolAdapter: Send + 'static {
    type Config: ProtocolConfig;
    type Error: Into<TransportError> + Send + std::fmt::Debug + 'static;

    /// Send packet
    async fn send(&mut self, packet: Packet) -> Result<(), Self::Error>;

    /// Close connection
    async fn close(&mut self) -> Result<(), Self::Error>;

    /// Gracefully close connection
    ///
    /// Send protocol-specific close signal and wait for peer confirmation
    async fn graceful_close(&mut self) -> Result<(), Self::Error> {
        // Default implementation: directly call close()
        self.close().await
    }

    /// Force close connection
    ///
    /// Immediately close connection without waiting for peer confirmation
    async fn force_close(&mut self) -> Result<(), Self::Error> {
        // Default implementation: directly call close()
        self.close().await
    }

    /// Get connection information
    fn connection_info(&self) -> ConnectionInfo;

    /// Check connection status
    fn is_connected(&self) -> bool;

    /// Get adapter statistics information
    fn stats(&self) -> AdapterStats;

    /// Get session ID
    fn session_id(&self) -> SessionId;

    /// Set session ID
    fn set_session_id(&mut self, session_id: SessionId);

    /// Flush send buffer
    async fn flush(&mut self) -> Result<(), Self::Error> {
        // Default implementation: handled by internal event loop in event-driven mode
        Ok(())
    }
}

/// Protocol configuration trait
pub trait ProtocolConfig: Send + Sync + Clone + std::fmt::Debug + 'static {
    /// Validate if configuration is valid
    fn validate(&self) -> Result<(), ConfigError>;

    /// Get default configuration
    fn default_config() -> Self;

    /// Merge configurations
    fn merge(self, other: Self) -> Self;
}

/// Object-safe protocol configuration trait for unified Builder interface
pub trait DynProtocolConfig: Send + Sync + 'static {
    /// Get protocol name
    fn protocol_name(&self) -> &'static str;

    /// Validate configuration
    fn validate_dyn(&self) -> Result<(), ConfigError>;

    /// Convert to Any to support downcasting
    fn as_any(&self) -> &dyn std::any::Any;

    /// Clone as Box<dyn DynProtocolConfig>
    fn clone_dyn(&self) -> Box<dyn DynProtocolConfig>;
}

/// 🔧 Server-specific dynamic configuration
pub trait DynServerConfig: DynProtocolConfig {
    /// Dynamically build server (object-safe)
    fn build_server_dyn(
        &self,
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

    /// Clone as Box<dyn DynServerConfig>
    fn clone_server_dyn(&self) -> Box<dyn DynServerConfig>;
}

/// 🔧 Client-specific dynamic configuration  
pub trait DynClientConfig: DynProtocolConfig {
    /// Dynamically build connection (object-safe)
    fn build_connection_dyn(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Connection>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    >;

    /// Get target information (could be SocketAddr or URL)
    fn get_target_info(&self) -> String;

    /// Clone as Box<dyn DynClientConfig>
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

/// TCP adapter configuration

impl ServerConfig for TcpServerConfig {
    type Server = crate::adapters::factories::TcpServerWrapper;

    fn validate(&self) -> Result<(), TransportError> {
        ProtocolConfig::validate(self).map_err(|e| {
            TransportError::config_error(
                "protocol",
                format!("TCP config validation failed: {:?}", e),
            )
        })
    }

    async fn build_server(&self) -> Result<Self::Server, TransportError> {
        use crate::adapters::tcp::TcpServerBuilder;

        let server = TcpServerBuilder::new()
            .bind_address(self.bind_address)
            .config(self.clone())
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

    fn protocol_name(&self) -> &'static str {
        "tcp"
    }
}

impl ClientConfig for TcpClientConfig {
    type Connection = crate::connection::TcpConnection;

    fn validate(&self) -> Result<(), TransportError> {
        ProtocolConfig::validate(self).map_err(|e| {
            TransportError::config_error(
                "protocol",
                format!("TCP config validation failed: {:?}", e),
            )
        })
    }

    async fn build_connection(&self) -> Result<Self::Connection, TransportError> {
        use crate::adapters::tcp::TcpClientBuilder;

        let adapter = TcpClientBuilder::new()
            .target_address(self.target_address)
            .config(self.clone())
            .connect()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build TCP connection: {:?}", e),
                    true,
                )
            })?;

        Ok(crate::connection::TcpConnection::new(adapter))
    }

    fn protocol_name(&self) -> &'static str {
        "tcp"
    }
}

impl ServerConfig for WebSocketServerConfig {
    type Server = crate::adapters::factories::WebSocketServerWrapper;

    fn validate(&self) -> Result<(), TransportError> {
        ProtocolConfig::validate(self).map_err(|e| {
            TransportError::config_error(
                "protocol",
                format!("WebSocket config validation failed: {:?}", e),
            )
        })
    }

    async fn build_server(&self) -> Result<Self::Server, TransportError> {
        use crate::adapters::websocket::WebSocketServerBuilder;

        let server = WebSocketServerBuilder::new()
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

    fn protocol_name(&self) -> &'static str {
        "websocket"
    }
}

impl ClientConfig for WebSocketClientConfig {
    type Connection = crate::adapters::factories::WebSocketConnection;

    fn validate(&self) -> Result<(), TransportError> {
        ProtocolConfig::validate(self).map_err(|e| {
            TransportError::config_error(
                "protocol",
                format!("WebSocket config validation failed: {:?}", e),
            )
        })
    }

    async fn build_connection(&self) -> Result<Self::Connection, TransportError> {
        use crate::adapters::websocket::WebSocketClientBuilder;

        let adapter = WebSocketClientBuilder::new()
            .target_url(&self.target_url)
            .config(self.clone())
            .connect()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build WebSocket connection: {:?}", e),
                    true,
                )
            })?;

        Ok(crate::adapters::factories::WebSocketConnection::new(
            adapter,
        ))
    }

    fn protocol_name(&self) -> &'static str {
        "websocket"
    }
}

impl ServerConfig for QuicServerConfig {
    type Server = crate::adapters::factories::QuicServerWrapper;

    fn validate(&self) -> Result<(), TransportError> {
        ProtocolConfig::validate(self).map_err(|e| {
            TransportError::config_error(
                "protocol",
                format!("QUIC config validation failed: {:?}", e),
            )
        })
    }

    async fn build_server(&self) -> Result<Self::Server, TransportError> {
        use crate::adapters::quic::QuicServerBuilder;

        let server = QuicServerBuilder::new()
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

    fn protocol_name(&self) -> &'static str {
        "quic"
    }
}

impl ClientConfig for QuicClientConfig {
    type Connection = crate::adapters::factories::QuicConnection;

    fn validate(&self) -> Result<(), TransportError> {
        ProtocolConfig::validate(self).map_err(|e| {
            TransportError::config_error(
                "protocol",
                format!("QUIC config validation failed: {:?}", e),
            )
        })
    }

    async fn build_connection(&self) -> Result<Self::Connection, TransportError> {
        use crate::adapters::quic::QuicClientBuilder;

        let adapter = QuicClientBuilder::new()
            .target_address(self.target_address)
            .config(self.clone())
            .connect()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("Failed to build QUIC connection: {:?}", e),
                    true,
                )
            })?;

        Ok(crate::adapters::factories::QuicConnection::new(adapter))
    }

    fn protocol_name(&self) -> &'static str {
        "quic"
    }
}

/// Server configuration trait - for type-safe server startup
pub trait ServerConfig: Send + Sync + 'static {
    type Server: crate::Server;

    /// Validate configuration correctness
    fn validate(&self) -> Result<(), TransportError>;

    /// Build server instance
    fn build_server(
        &self,
    ) -> impl std::future::Future<Output = Result<Self::Server, TransportError>> + Send;

    /// Get protocol name
    fn protocol_name(&self) -> &'static str;
}

/// Client configuration trait - for type-safe client connections
pub trait ClientConfig: Send + Sync + 'static {
    type Connection: crate::Connection;

    /// Validate configuration correctness
    fn validate(&self) -> Result<(), TransportError>;

    /// Build connection instance
    fn build_connection(
        &self,
    ) -> impl std::future::Future<Output = Result<Self::Connection, TransportError>> + Send;

    /// Get protocol name
    fn protocol_name(&self) -> &'static str;
}

// ConnectableConfig implementation has been moved to client_config.rs
