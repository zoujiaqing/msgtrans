/// Server-side transport layer module
///
/// Provides transport layer API specifically for server-side listening
use crate::{error::TransportError, transport::config::TransportConfig};

// Import new TransportServer
use super::transport_server::TransportServer;

/// Server-side transport builder - focused on server listening related configuration
pub struct TransportServerBuilder {
    transport_config: TransportConfig,
    /// Protocol configuration storage - server supports multi-protocol listening
    protocol_configs:
        std::collections::HashMap<String, Box<dyn crate::protocol::adapter::DynServerConfig>>,
    /// Buffer size for actor channels
    actor_buffer_size: Option<usize>,
    /// Frame decode policy applied to accepted connections.
    frame_policy: crate::packet::FramePolicy,
}

impl TransportServerBuilder {
    pub fn new() -> Self {
        Self {
            transport_config: TransportConfig::default(),
            protocol_configs: std::collections::HashMap::new(),
            actor_buffer_size: None,
            frame_policy: crate::packet::FramePolicy::Lenient,
        }
    }

    /// Set the frame decode policy applied to accepted connections (default Lenient).
    ///
    /// Under `Strict`, WebSocket/QUIC connections that receive an undecodable
    /// frame are closed instead of downgrading it to a raw one-way message.
    pub fn frame_policy(mut self, policy: crate::packet::FramePolicy) -> Self {
        self.frame_policy = policy;
        self
    }

    /// Set transport layer base configuration
    pub fn transport_config(mut self, config: TransportConfig) -> Self {
        self.transport_config = config;
        self
    }

    /// Unified protocol configuration interface - server supports multi-protocol
    pub fn protocol<T: crate::protocol::adapter::DynServerConfig>(mut self, config: T) -> Self {
        let protocol_name = config.protocol_name().to_string();
        self.protocol_configs
            .insert(protocol_name, Box::new(config));
        self
    }

    /// Set buffer size for actor channels (default: 2048)
    pub fn actor_buffer_size(mut self, size: usize) -> Self {
        self.actor_buffer_size = Some(size);
        self
    }

    /// Build the server.
    ///
    /// `handler` is required: each connection gets its own actor that invokes it
    /// for that session's messages and lifecycle. Taking it here rather than via
    /// an optional setter means a server can never be built without a consumer.
    pub async fn build(
        self,
        handler: std::sync::Arc<dyn super::session_actor::SessionHandler>,
    ) -> Result<TransportServer, TransportError> {
        let transport_server = super::transport_server::TransportServer::new(
            self.transport_config.clone(),
            self.protocol_configs,
            handler,
            self.actor_buffer_size,
        )
        .await?;
        let transport_server = transport_server.with_frame_policy(self.frame_policy);

        tracing::info!("[SUCCESS] TransportServer build completed");
        Ok(transport_server)
    }
}

impl Default for TransportServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}
