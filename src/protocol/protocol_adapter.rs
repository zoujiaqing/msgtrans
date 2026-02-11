use super::adapter::{AdapterStats, ProtocolAdapter, ProtocolConfig};
use crate::Connection;
use crate::{command::ConnectionInfo, error::TransportError, packet::Packet, SessionId};
/// Protocol connection adapter
///
/// Wraps the new Connection trait as ProtocolAdapter trait for compatibility with existing Actor system
use async_trait::async_trait;

/// Protocol connection adapter
///
/// This adapter wraps the new Connection trait as ProtocolAdapter trait
pub struct ProtocolConnectionAdapter {
    connection: Box<dyn Connection>,
    stats: AdapterStats,
}

impl ProtocolConnectionAdapter {
    /// Create new protocol connection adapter
    pub fn new(connection: Box<dyn Connection>) -> Self {
        Self {
            connection,
            stats: AdapterStats::new(),
        }
    }
}

#[async_trait]
impl ProtocolAdapter for ProtocolConnectionAdapter {
    type Config = EmptyConfig;
    type Error = TransportError;

    async fn send(&mut self, packet: Packet) -> Result<(), Self::Error> {
        let packet_size = packet.payload.len();
        let result = self.connection.send(packet).await;

        if result.is_ok() {
            self.stats.record_packet_sent(packet_size);
        } else {
            self.stats.record_error();
        }

        result
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.connection.close().await
    }

    fn connection_info(&self) -> ConnectionInfo {
        self.connection.connection_info()
    }

    fn is_connected(&self) -> bool {
        self.connection.is_connected()
    }

    fn stats(&self) -> AdapterStats {
        self.stats.clone()
    }

    fn session_id(&self) -> SessionId {
        self.connection.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        self.connection.set_session_id(session_id);
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // Most protocol connections don't need explicit flush
        Ok(())
    }
}

impl ProtocolConnectionAdapter {
    /// Get event stream - core of event-driven architecture
    pub fn subscribe_events(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        self.connection.event_stream()
    }
}

/// Empty configuration type for protocol connection adapter
#[derive(Debug, Clone)]
pub struct EmptyConfig;

impl ProtocolConfig for EmptyConfig {
    fn validate(&self) -> Result<(), super::adapter::ConfigError> {
        Ok(())
    }

    fn default_config() -> Self {
        Self
    }

    fn merge(self, _other: Self) -> Self {
        self
    }
}
