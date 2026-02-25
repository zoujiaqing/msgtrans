use crate::{command::ConnectionInfo, error::TransportError, packet::Packet, SessionId};
use async_trait::async_trait;

/// Unified connection interface - the single abstraction for all protocols
///
/// In v2.0, adapters implement this trait directly — no wrapper types needed.
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

    /// Get event stream
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
