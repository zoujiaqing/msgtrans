use crate::{command::ConnectionInfo, error::TransportError, packet::Packet, SessionId};
use async_trait::async_trait;

/// Unified connection interface - Connection abstraction for all protocols
///
/// This is the only connection interface in msgtrans, all protocol adapters should implement this interface
#[async_trait]
pub trait Connection: Send + Sync + std::any::Any {
    /// Send packet
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError>;

    /// Close connection
    async fn close(&mut self) -> Result<(), TransportError>;

    /// Get session ID
    fn session_id(&self) -> SessionId;

    /// Set session ID
    fn set_session_id(&mut self, session_id: SessionId);

    /// Get connection information
    fn connection_info(&self) -> ConnectionInfo;

    /// Check if connection is active
    fn is_connected(&self) -> bool;

    /// Flush send buffer
    async fn flush(&mut self) -> Result<(), TransportError>;

    /// Get event stream - Core of event-driven architecture
    ///
    /// All connections should support event streams, this is the foundation of event-driven architecture
    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>>;
}

/// Unified server interface - Accept new connections
#[async_trait]
pub trait Server: Send + Sync {
    /// Accept new connection
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError>;

    /// Get server bind address
    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError>;

    /// Shutdown server
    async fn shutdown(&mut self) -> Result<(), TransportError>;
}

/// Connection factory - Create client connections
#[async_trait]
pub trait ConnectionFactory: Send + Sync {
    /// Establish connection
    async fn connect(&self) -> Result<Box<dyn Connection>, TransportError>;
}

/// TCP connection wrapper
pub struct TcpConnection {
    adapter: crate::adapters::tcp::TcpAdapter<crate::protocol::TcpClientConfig>,
    cached_info: ConnectionInfo,
}

impl TcpConnection {
    pub fn new(
        adapter: crate::adapters::tcp::TcpAdapter<crate::protocol::TcpClientConfig>,
    ) -> Self {
        use crate::protocol::ProtocolAdapter;
        let cached_info = adapter.connection_info();
        Self {
            adapter,
            cached_info,
        }
    }

    /// Get event stream receiver
    ///
    /// This allows clients to subscribe to events sent by the TCP adapter's internal event loop
    pub fn subscribe_events(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::event::TransportEvent> {
        self.adapter.subscribe_events()
    }
}

#[async_trait]
impl Connection for TcpConnection {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.adapter.send(packet).await.map_err(Into::into)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.adapter.close().await.map_err(Into::into)
    }

    fn session_id(&self) -> SessionId {
        use crate::protocol::ProtocolAdapter;
        self.adapter.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        use crate::protocol::ProtocolAdapter;
        self.adapter.set_session_id(session_id);
    }

    fn connection_info(&self) -> ConnectionInfo {
        self.cached_info.clone()
    }

    fn is_connected(&self) -> bool {
        use crate::protocol::ProtocolAdapter;
        self.adapter.is_connected()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.adapter.flush().await.map_err(Into::into)
    }

    /// Get event stream - TCP connection specific implementation
    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        Some(self.adapter.subscribe_events())
    }
}

/// TCP server wrapper
pub struct TcpServer {
    inner: crate::adapters::tcp::TcpServer,
}

impl TcpServer {
    pub fn new(inner: crate::adapters::tcp::TcpServer) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Server for TcpServer {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError> {
        let adapter = self.inner.accept().await.map_err(|e| {
            TransportError::protocol_error("generic", format!("TCP accept failed: {:?}", e))
        })?;

        // Note: Need to convert server-side adapter to client connection
        // TODO: Need to implement appropriate conversion logic
        Err(TransportError::config_error(
            "tcp",
            "TCP server accept not fully implemented yet",
        ))
    }

    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        self.inner.local_addr().map_err(|e| {
            TransportError::protocol_error(
                "generic",
                format!("Failed to get local address: {:?}", e),
            )
        })
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        // TCP server has no explicit shutdown method, this is just a marker
        Ok(())
    }
}

/// TCP connection factory
pub struct TcpConnectionFactory {
    target_addr: std::net::SocketAddr,
    config: crate::protocol::TcpClientConfig,
}

impl TcpConnectionFactory {
    pub fn new(
        target_addr: std::net::SocketAddr,
        config: crate::protocol::TcpClientConfig,
    ) -> Self {
        Self {
            target_addr,
            config,
        }
    }
}

#[async_trait]
impl ConnectionFactory for TcpConnectionFactory {
    async fn connect(&self) -> Result<Box<dyn Connection>, TransportError> {
        let adapter =
            crate::adapters::tcp::TcpAdapter::connect(self.target_addr, self.config.clone())
                .await
                .map_err(|e| {
                    TransportError::protocol_error(
                        "generic",
                        format!("TCP connect failed: {:?}", e),
                    )
                })?;

        Ok(Box::new(TcpConnection::new(adapter)))
    }
}
