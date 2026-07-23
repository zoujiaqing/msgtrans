use crate::{command::ConnectionInfo, error::TransportError, packet::Packet, SessionId};
use async_trait::async_trait;

/// Unified connection interface - the single abstraction for all protocols
///
/// In v2.0, adapters implement this trait directly — no wrapper types needed.
#[async_trait]
pub trait Connection: Send + Sync + std::any::Any {
    /// Send packet
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError>;

    /// Enqueue with a WRITE receipt: the returned receiver resolves with the
    /// REAL write result (written to the socket / OS buffer, or the write or
    /// connection failed). Split from awaiting on purpose: callers reach this
    /// method through a connection lock and must await the receipt AFTER
    /// releasing it, or one slow write would serialize every other sender.
    /// The default (for test doubles) performs an enqueue-only `send` and
    /// resolves the receipt immediately; every real adapter overrides it.
    async fn send_with_receipt(
        &mut self,
        packet: Packet,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), TransportError>>, TransportError> {
        self.send(packet).await?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(Ok(()));
        Ok(rx)
    }

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

    /// Take the bounded event pipe: the connection's single event channel
    /// (single consumer, take-once). All queued data events are delivered
    /// first, then exactly one ConnectionClosed.
    #[doc(hidden)]
    fn take_event_pipe(&mut self) -> Option<crate::adapters::events::EventPipeRx>;

    /// Set the frame decode policy for this connection. Adapters that support it
    /// (WebSocket, QUIC) override this; others (e.g. TCP, which is always strict
    /// on a malformed first packet) keep the default no-op.
    fn set_frame_policy(&self, _policy: crate::packet::FramePolicy) {}
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
